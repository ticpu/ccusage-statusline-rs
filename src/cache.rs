use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tempfile::NamedTempFile;

use crate::warn;

/// Semaphore cache for fast statusline rendering.
/// `date` and `transcript_path` are omitted — write-only fields dropped; old cache files
/// with those extra fields still parse correctly (serde ignores unknown fields).
#[derive(Debug, Serialize, Deserialize)]
struct Semaphore {
    last_output: String,
    last_update_time: u64,
    transcript_mtime: u64,
}

/// Create and return the cache directory `config_dir` owns under `runtime_dir`.
///
/// Two config dirs must never share a cache: their credentials, plans and transcripts
/// differ, so a shared usage cache reports one account's numbers under the other.
pub fn cache_dir_for(runtime_dir: &Path, config_dir: &Path) -> Result<PathBuf> {
    let config_name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".claude")
        .trim_start_matches('.');
    let dir = runtime_dir
        .join("ccusage-statusline-rs")
        .join(config_name);

    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create cache dir {}", dir.display()))?;

    // The XDG_RUNTIME_DIR fallback lands in a world-writable /tmp, where the cache
    // holds the rendered statusline (working-directory path) and usage percentages.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("Failed to restrict cache dir {}", dir.display()))?;
    }

    Ok(dir)
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(unix)]
            {
                let candidate =
                    PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()));
                if candidate.is_dir() {
                    candidate
                } else {
                    std::env::temp_dir()
                }
            }
            #[cfg(not(unix))]
            {
                std::env::temp_dir()
            }
        })
}

/// Get cache directory from XDG_RUNTIME_DIR, scoped per config dir.
/// Computed once per process; env lookups and create_dir_all happen only once.
pub fn get_cache_dir() -> Result<PathBuf> {
    static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

    if let Some(dir) = CACHE_DIR.get() {
        return Ok(dir.clone());
    }

    let dir = cache_dir_for(&runtime_dir(), &crate::paths::claude_config_dir()?)?;

    // Losing the race is expected: every caller computes the same path.
    Ok(CACHE_DIR
        .get_or_init(|| dir)
        .clone())
}

/// Atomic write: serialize `value` to a temp file then rename into place.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_string(value)?;
    write_atomic(path, json.as_bytes())
}

/// Publish `bytes` at `path` via a uniquely-named temp file in the same directory.
/// A temp name derived from `path` alone is shared by every concurrent writer, so
/// two of them interleave into one file and rename the spliced result into place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .unwrap_or(Path::new("."));
    let mut temp = NamedTempFile::new_in(dir)
        .with_context(|| format!("Failed to create temp file in {}", dir.display()))?;
    temp.write_all(bytes)
        .with_context(|| format!("Failed to write temp file for {}", path.display()))?;
    temp.as_file()
        .sync_data()
        .with_context(|| format!("Failed to flush temp file for {}", path.display()))?;
    temp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("Failed to publish {}", path.display()))?;
    Ok(())
}

/// Open a cache file read-write, creating it private to the user.
///
/// The XDG_RUNTIME_DIR fallback lands in a world-readable /tmp, and these files hold
/// the rendered statusline and usage percentages. The mode applies only on creation.
pub fn open_private_rw(path: &Path) -> Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true)
        .write(true)
        .create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .with_context(|| format!("Failed to open {}", path.display()))
}

/// Overwrite `file` through the descriptor whose exclusive lock the caller holds.
///
/// Readers take a shared lock on these files, so publishing by rename would leave them
/// holding a lock on the unlinked inode and reading pre-rename content.
pub fn write_locked_in_place(file: &mut File, bytes: &[u8]) -> Result<()> {
    file.set_len(0)
        .context("Failed to truncate locked cache file")?;
    file.rewind()
        .context("Failed to rewind locked cache file")?;
    file.write_all(bytes)
        .context("Failed to write locked cache file")?;
    file.sync_data()
        .context("Failed to flush locked cache file")
}

/// Read and deserialize a JSON file. Returns `None` on NotFound, `Err` on other failures.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("Failed to parse JSON from {}", path.display()))
            .map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

/// Read and deserialize a JSON cache file, reporting an unusable one as a miss.
pub fn read_json_warn<T: DeserializeOwned>(path: &Path) -> Option<T> {
    match read_json(path) {
        Ok(v) => v,
        Err(e) => {
            warn!("cache read error: {:#}", e);
            None
        }
    }
}

/// Returns the file modification time as Unix epoch seconds.
pub fn path_mtime_secs(path: impl AsRef<Path>) -> Result<u64> {
    let path = path.as_ref();
    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    metadata
        .modified()
        .context("File modification time unavailable")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("File mtime is before UNIX epoch")
        .map(|d| d.as_secs())
}

/// Try to get cached output if valid
pub fn try_get_cached(
    cache_path: &Path,
    transcript_path: &str,
    ttl_secs: u64,
) -> Result<Option<String>> {
    if !cache_path.exists() {
        return Ok(None);
    }

    let mut file = match File::open(cache_path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            warn!(
                "Output cache open failed ({}): {:#}",
                cache_path.display(),
                e
            );
            return Ok(None);
        }
    };

    // Try to acquire shared lock (non-blocking)
    match file.try_lock_shared() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => {
            warn!(
                "Output cache lock failed ({}): {:#}",
                cache_path.display(),
                e
            );
            return Ok(None);
        }
    }

    let mut contents = String::new();
    if let Err(e) = file.read_to_string(&mut contents) {
        warn!(
            "Output cache read failed ({}): {:#}",
            cache_path.display(),
            e
        );
        return Ok(None);
    }

    let semaphore: Semaphore = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "Output cache parse failed ({}): {:#}",
                cache_path.display(),
                e
            );
            return Ok(None);
        }
    };

    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(e) => {
            warn!("Output cache: system clock is before UNIX epoch: {:#}", e);
            return Ok(None);
        }
    };

    // Saturating: a future timestamp (clock step) is treated as stale
    let is_expired = now.saturating_sub(semaphore.last_update_time) >= ttl_secs;

    // A transcript that vanished (deleted, rotated) is a cache miss, not a render
    // failure: propagating here would print an empty statusline.
    let current_mtime = match path_mtime_secs(transcript_path) {
        Ok(m) => m,
        Err(e) => {
            warn!("Output cache transcript stat failed: {:#}", e);
            return Ok(None);
        }
    };
    let is_file_modified = current_mtime != semaphore.transcript_mtime;

    if is_expired || is_file_modified {
        return Ok(None);
    }

    Ok(Some(semaphore.last_output))
}

/// Update cache with new output
pub fn update_cache(cache_path: &Path, transcript_path: &str, output: &str) -> Result<()> {
    // Before opening: a missing transcript aborts the write, and truncating first
    // would leave a 0-byte cache file that every later read has to reject.
    let mtime = path_mtime_secs(transcript_path)?;

    let mut file = open_private_rw(cache_path)?;

    file.lock()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let semaphore = Semaphore {
        last_output: output.to_string(),
        last_update_time: now,
        transcript_mtime: mtime,
    };

    let json = serde_json::to_string(&semaphore)?;
    let result = write_locked_in_place(&mut file, json.as_bytes());

    file.unlock()?;
    result
}

/// Remove .lock files whose mtime exceeds `ttl_secs`. Runs at most once
/// per `ttl_secs`, gated by the mtime of a marker file.
pub fn cleanup_stale_locks(cache_dir: &Path, ttl_secs: u64) {
    let marker = cache_dir.join("last-cleanup");
    if let Ok(mtime) = fs::metadata(&marker).and_then(|m| m.modified())
        && let Ok(age) = mtime.elapsed()
        && age.as_secs() < ttl_secs
    {
        return;
    }

    // Touch the marker first so concurrent invocations skip cleanup. A marker that
    // never lands (read-only cache dir) silently re-runs the scan on every render.
    if let Err(e) = open_private_rw(&marker).and_then(|f| {
        f.set_len(0)
            .context("truncate")
    }) {
        warn!(
            "Cache cleanup marker {} not writable, cleanup will rescan every run: {:#}",
            marker.display(),
            e
        );
    }

    let entries = match fs::read_dir(cache_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(
                "Cache cleanup skipped, cannot list {}: {:#}",
                cache_dir.display(),
                e
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|e| e.to_str())
            != Some("lock")
        {
            continue;
        }
        let mtime = match fs::metadata(&path).and_then(|m| m.modified()) {
            Ok(m) => m,
            Err(e) => {
                warn!("Cache cleanup cannot stat {}: {:#}", path.display(), e);
                continue;
            }
        };
        if let Ok(age) = mtime.elapsed()
            && age.as_secs() > ttl_secs
            && let Err(e) = fs::remove_file(&path)
        {
            warn!("Cache cleanup cannot remove {}: {:#}", path.display(), e);
        }
    }
}

use crate::types::Semaphore;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Get cache directory from XDG_RUNTIME_DIR, scoped per config dir.
/// Computed once per process; env lookups and create_dir_all happen only once.
pub fn get_cache_dir() -> Result<PathBuf> {
    static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

    if let Some(dir) = CACHE_DIR.get() {
        return Ok(dir.clone());
    }

    let dir = compute_cache_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create cache dir {}", dir.display()))?;
    let _ = CACHE_DIR.set(dir.clone());
    Ok(CACHE_DIR
        .get()
        .unwrap_or(&dir)
        .clone())
}

fn compute_cache_dir() -> Result<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
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
        });
    let config_dir = crate::paths::claude_config_dir()?;
    let config_name = config_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".claude")
        .trim_start_matches('.');
    Ok(runtime_dir
        .join("ccusage-statusline-rs")
        .join(config_name))
}

/// Atomic write: serialize `value` to a temp file then rename into place.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let temp = path.with_extension("tmp");
    let json = serde_json::to_string(value)?;
    fs::write(&temp, &json).with_context(|| format!("Failed to write {}", temp.display()))?;
    fs::rename(&temp, path)
        .with_context(|| format!("Failed to rename {} to {}", temp.display(), path.display()))
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
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "Output cache open failed ({}): {:#}",
                    cache_path.display(),
                    e
                );
            }
            return Ok(None);
        }
    };

    // Try to acquire shared lock (non-blocking)
    match file.try_lock_shared() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(e)) => {
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "Output cache lock failed ({}): {:#}",
                    cache_path.display(),
                    e
                );
            }
            return Ok(None);
        }
    }

    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let semaphore: Semaphore = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "Output cache parse failed ({}): {:#}",
                    cache_path.display(),
                    e
                );
            }
            return Ok(None);
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    // Saturating: a future timestamp (clock step) is treated as stale
    let is_expired = now.saturating_sub(semaphore.last_update_time) >= ttl_secs;

    // Check if transcript file was modified
    let current_mtime = path_mtime_secs(transcript_path)?;
    let is_file_modified = current_mtime != semaphore.transcript_mtime;

    if is_expired || is_file_modified {
        return Ok(None);
    }

    Ok(Some(semaphore.last_output))
}

/// Update cache with new output
pub fn update_cache(cache_path: &Path, transcript_path: &str, output: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(cache_path)
        .with_context(|| format!("Failed to open cache file {}", cache_path.display()))?;

    file.lock()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let mtime = path_mtime_secs(transcript_path)?;

    let semaphore = Semaphore {
        date: Utc::now().to_rfc3339(),
        last_output: output.to_string(),
        last_update_time: now,
        transcript_path: transcript_path.to_string(),
        transcript_mtime: mtime,
    };

    let json = serde_json::to_string(&semaphore)?;
    file.write_all(json.as_bytes())?;

    file.unlock()?;
    Ok(())
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

    // Touch the marker first so concurrent invocations skip cleanup
    let _ = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&marker);

    let entries = match fs::read_dir(cache_dir) {
        Ok(e) => e,
        Err(_) => return,
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
            Err(_) => continue,
        };
        if let Ok(age) = mtime.elapsed()
            && age.as_secs() > ttl_secs
        {
            let _ = fs::remove_file(&path);
        }
    }
}

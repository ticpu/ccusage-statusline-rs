use crate::types::Semaphore;
use anyhow::Result;
use chrono::Utc;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Get cache directory from XDG_RUNTIME_DIR, scoped per config dir.
/// Fallback on Unix is per-user `/run/user/<uid>` (mode 0700, tmpfs); on
/// non-Unix targets it is `std::env::temp_dir()`.
pub fn get_cache_dir() -> Result<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(unix)]
            {
                PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
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
        Err(_) => return Ok(None),
    };

    // Try to acquire shared lock (non-blocking)
    if FileExt::try_lock_shared(&file).is_err() {
        return Ok(None);
    }

    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let semaphore: Semaphore = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    // Check if cache is still valid
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let is_expired = now - semaphore.last_update_time >= ttl_secs;

    // Check if transcript file was modified
    let current_mtime = get_file_mtime(transcript_path)?;
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
        .open(cache_path)?;

    // Acquire exclusive lock
    file.lock_exclusive()?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();

    let mtime = get_file_mtime(transcript_path)?;

    let semaphore = Semaphore {
        date: Utc::now().to_rfc3339(),
        last_output: output.to_string(),
        last_update_time: now,
        transcript_path: transcript_path.to_string(),
        transcript_mtime: mtime,
    };

    let json = serde_json::to_string(&semaphore)?;
    file.write_all(json.as_bytes())?;

    FileExt::unlock(&file)?;
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

/// Get file modification time in seconds
pub fn get_file_mtime(path: &str) -> Result<u64> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok(mtime)
}

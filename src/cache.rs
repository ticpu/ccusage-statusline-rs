use crate::types::Semaphore;
use anyhow::Result;
use chrono::Utc;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Get cache directory from XDG_RUNTIME_DIR
pub fn get_cache_dir() -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    Ok(PathBuf::from(runtime_dir).join("ccusage-statusline-rs"))
}

/// Try to get cached output if valid
pub fn try_get_cached(cache_path: &Path, transcript_path: &str) -> Result<Option<String>> {
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

    // Cache expires after 30 seconds (default refresh interval)
    let is_expired = now - semaphore.last_update_time >= 30;

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

/// Get file modification time in seconds
pub fn get_file_mtime(path: &str) -> Result<u64> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok(mtime)
}

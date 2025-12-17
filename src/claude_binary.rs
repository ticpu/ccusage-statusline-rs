use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;

use crate::cache::get_cache_dir;

const VERSION_CACHE_FILE: &str = "claude-version-cache.json";

#[derive(Debug, Serialize, Deserialize)]
struct VersionCache {
    version: String,
    binary_mtime: u64,
}

/// Get Claude binary path from PATH
fn get_claude_binary_path() -> Option<PathBuf> {
    let output = Command::new("which").arg("claude").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let path_str = String::from_utf8(output.stdout).ok()?;
    Some(PathBuf::from(path_str.trim()))
}

/// Get binary modification time as unix timestamp
fn get_binary_mtime(path: &PathBuf) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;
    mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Get cached version if still valid (binary hasn't changed)
fn get_cached_version() -> Option<String> {
    let cache_dir = get_cache_dir().ok()?;
    let cache_path = cache_dir.join(VERSION_CACHE_FILE);

    let contents = fs::read_to_string(&cache_path).ok()?;
    let cache: VersionCache = serde_json::from_str(&contents).ok()?;

    let binary_path = get_claude_binary_path()?;
    let current_mtime = get_binary_mtime(&binary_path)?;

    if cache.binary_mtime == current_mtime {
        Some(cache.version)
    } else {
        None
    }
}

/// Save version to cache
fn save_version_cache(version: &str, mtime: u64) {
    if let Ok(cache_dir) = get_cache_dir() {
        let _ = fs::create_dir_all(&cache_dir);
        let cache_path = cache_dir.join(VERSION_CACHE_FILE);
        let cache = VersionCache {
            version: version.to_string(),
            binary_mtime: mtime,
        };
        if let Ok(contents) = serde_json::to_string(&cache) {
            let _ = fs::write(cache_path, contents);
        }
    }
}

/// Fetch version from `claude --version`
fn fetch_claude_version() -> Option<String> {
    let output = Command::new("claude").arg("--version").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let version_output = String::from_utf8(output.stdout).ok()?;
    version_output.split_whitespace().next().map(String::from)
}

/// Get Claude Code version (cached based on binary mtime)
pub fn get_version() -> Option<String> {
    // Try cache first
    if let Some(version) = get_cached_version() {
        return Some(version);
    }

    // Fetch fresh version
    let version = fetch_claude_version()?;

    // Cache it with binary mtime
    if let Some(binary_path) = get_claude_binary_path()
        && let Some(mtime) = get_binary_mtime(&binary_path)
    {
        save_version_cache(&version, mtime);
    }

    Some(version)
}

/// Get User-Agent string for API requests
pub fn get_user_agent() -> String {
    match get_version() {
        Some(version) => format!("claude-code/{}", version),
        None => "claude-code/unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_cache_serialization() {
        let cache = VersionCache {
            version: "2.0.71".to_string(),
            binary_mtime: 1234567890,
        };
        let json = serde_json::to_string(&cache).unwrap();
        let parsed: VersionCache = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, "2.0.71");
        assert_eq!(parsed.binary_mtime, 1234567890);
    }

    #[test]
    fn test_user_agent_format() {
        // Test with known version
        let ua = format!("claude-code/{}", "2.0.71");
        assert!(ua.starts_with("claude-code/"));
        assert!(ua.contains('.'));
    }
}

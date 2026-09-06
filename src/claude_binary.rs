use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

use crate::paths::Env;
use crate::warn;

const VERSION_CACHE_FILE: &str = "claude-version-cache.json";

#[derive(Debug, Serialize, Deserialize)]
struct VersionCache {
    version: String,
    binary_mtime: u64,
}

/// Get Claude binary path from PATH
fn get_claude_binary_path() -> Option<PathBuf> {
    which::which("claude").ok()
}

/// Get binary modification time as unix timestamp
fn get_binary_mtime(path: &PathBuf) -> Option<u64> {
    crate::cache::path_mtime_secs(path).ok()
}

/// Get cached version if still valid (binary hasn't changed)
fn get_cached_version(env: &Env) -> Option<String> {
    let cache: VersionCache = crate::cache::read_json_warn(&env.cache_file(VERSION_CACHE_FILE))?;

    let binary_path = get_claude_binary_path()?;
    (cache.binary_mtime == get_binary_mtime(&binary_path)?).then_some(cache.version)
}

/// Save version to cache
fn save_version_cache(env: &Env, version: &str, mtime: u64) {
    let cache_path = env.cache_file(VERSION_CACHE_FILE);
    let cache = VersionCache {
        version: version.to_string(),
        binary_mtime: mtime,
    };
    if let Err(e) = crate::cache::write_json_atomic(&cache_path, &cache) {
        warn!(
            "version cache write failed (claude --version will run each invocation): {:#}",
            e
        );
    }
}

/// How long `claude --version` may take before it is killed. It is the only
/// unbounded wait in a render: a hung binary would otherwise hang the statusline.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Fetch version from `claude --version`
fn fetch_claude_version() -> Option<String> {
    let mut child = match Command::new("claude")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // Not installed is the ordinary case and says nothing worth reporting.
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!("claude --version could not start: {e}");
            }
            return None;
        }
    };

    let status = match child.wait_timeout(VERSION_TIMEOUT) {
        Ok(Some(status)) => status,
        Ok(None) => {
            warn!("claude --version timed out, skipping update check");
            if let Err(e) = child.kill() {
                warn!("claude --version could not be killed, leaving it running: {e}");
            } else if let Err(e) = child.wait() {
                warn!("claude --version killed but not reaped: {e}");
            }
            return None;
        }
        Err(e) => {
            warn!("claude --version failed: {e}");
            return None;
        }
    };

    if !status.success() {
        return None;
    }

    let mut stdout = child
        .stdout
        .take()?;
    let mut buf = String::new();
    stdout
        .read_to_string(&mut buf)
        .ok()?;
    buf.split_whitespace()
        .next()
        .map(String::from)
}

/// Get Claude Code version (cached based on binary mtime)
pub fn get_version(env: &Env) -> Option<String> {
    // Try cache first
    if let Some(version) = get_cached_version(env) {
        return Some(version);
    }

    // Fetch fresh version
    let version = fetch_claude_version()?;

    // Cache it with binary mtime
    if let Some(binary_path) = get_claude_binary_path()
        && let Some(mtime) = get_binary_mtime(&binary_path)
    {
        save_version_cache(env, &version, mtime);
    }

    Some(version)
}

/// Get User-Agent string for API requests
pub fn get_user_agent(env: &Env) -> String {
    match get_version(env) {
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
        let ua = format!("claude-code/{}", "2.0.71");
        assert!(ua.starts_with("claude-code/"));
        assert!(ua.contains('.'));
    }
}

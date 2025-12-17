use crate::config::{StatuslineConfig, VersionChannel};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code";
const GCS_STABLE_URL: &str = "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/stable";
const UPDATE_CHECK_CACHE_TTL: Duration = Duration::from_secs(1800); // 30 minutes
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NpmRegistryResponse {
    #[serde(rename = "dist-tags")]
    dist_tags: DistTags,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DistTags {
    latest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCache {
    latest_version: Option<String>,
    checked_at: DateTime<Utc>,
    #[serde(default)]
    channel: VersionChannel,
}

fn get_cache_path() -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| -> Result<String, std::env::VarError> {
            let uid = unsafe { libc::getuid() };
            Ok(format!("/run/user/{}", uid))
        })
        .context("Failed to determine XDG_RUNTIME_DIR")?;

    Ok(PathBuf::from(runtime_dir).join("ccusage-claude-update-cache.json"))
}

fn read_cache() -> Option<UpdateCache> {
    let cache_path = get_cache_path().ok()?;
    let contents = fs::read_to_string(cache_path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn write_cache(cache: &UpdateCache) -> Result<()> {
    let cache_path = get_cache_path()?;
    let contents = serde_json::to_string(cache)?;
    fs::write(cache_path, contents)?;
    Ok(())
}

fn is_cache_fresh(cache: &UpdateCache) -> bool {
    let elapsed = Utc::now() - cache.checked_at;
    elapsed
        .to_std()
        .map(|d| d < UPDATE_CHECK_CACHE_TTL)
        .unwrap_or(false)
}

fn fetch_latest_version(channel: VersionChannel) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    match channel {
        VersionChannel::Stable => {
            let response = client
                .get(GCS_STABLE_URL)
                .send()
                .context("Failed to fetch GCS stable version")?;

            if !response.status().is_success() {
                anyhow::bail!("GCS returned status: {}", response.status());
            }

            let version = response.text().context("Failed to read version")?;
            Ok(version.trim().to_string())
        }
        VersionChannel::Latest => {
            let response = client
                .get(NPM_REGISTRY_URL)
                .send()
                .context("Failed to fetch npm registry")?;

            if !response.status().is_success() {
                anyhow::bail!("npm registry returned status: {}", response.status());
            }

            let data: NpmRegistryResponse = response
                .json()
                .context("Failed to parse npm registry response")?;

            Ok(data.dist_tags.latest)
        }
    }
}

fn get_installed_claude_version() -> Option<String> {
    let output = Command::new("claude").arg("--version").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let version_output = String::from_utf8(output.stdout).ok()?;
    version_output.split_whitespace().next().map(String::from)
}

fn compare_versions(current: &str, latest: &str) -> bool {
    use semver::Version;

    let Ok(current_v) = Version::parse(current) else {
        return false;
    };
    let Ok(latest_v) = Version::parse(latest) else {
        return false;
    };

    latest_v > current_v
}

/// Check if a Claude Code update is available.
/// Returns Some(version) if an update is available, None otherwise.
/// Caches results for 30 minutes.
pub fn check_update_available() -> Option<String> {
    let current = get_installed_claude_version()?;
    let channel = StatuslineConfig::load()
        .map(|c| c.version_channel)
        .unwrap_or_default();

    // Try to read cache first
    if let Some(cache) = read_cache()
        && is_cache_fresh(&cache)
        && cache.channel == channel
    {
        // Use cached result (same channel)
        if let Some(ref latest) = cache.latest_version
            && compare_versions(&current, latest)
        {
            return Some(latest.clone());
        }
        return None;
    }

    // Cache miss, stale, or channel changed - fetch new data
    let latest_version = match fetch_latest_version(channel) {
        Ok(version) => Some(version),
        Err(_) => {
            // Fail silently like Claude Code does
            // Use old cache if available and same channel
            if let Some(cache) = read_cache()
                && cache.channel == channel
            {
                cache.latest_version
            } else {
                None
            }
        }
    };

    // Update cache
    let new_cache = UpdateCache {
        latest_version: latest_version.clone(),
        checked_at: Utc::now(),
        channel,
    };
    let _ = write_cache(&new_cache); // Ignore write errors

    // Check if update available
    if let Some(ref latest) = latest_version
        && compare_versions(&current, latest)
    {
        return Some(latest.clone());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_versions() {
        assert!(compare_versions("1.0.0", "1.0.1"));
        assert!(compare_versions("1.0.0", "1.1.0"));
        assert!(compare_versions("1.0.0", "2.0.0"));
        assert!(!compare_versions("1.0.1", "1.0.0"));
        assert!(!compare_versions("1.0.0", "1.0.0"));
    }
}

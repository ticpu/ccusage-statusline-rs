use crate::config::{StatusElement, StatuslineConfig};
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VersionChannel {
    #[default]
    Stable,
    Latest,
}

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
}

fn get_cache_path(channel: VersionChannel) -> Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| -> Result<String, std::env::VarError> {
            let uid = unsafe { libc::getuid() };
            Ok(format!("/run/user/{}", uid))
        })
        .context("Failed to determine XDG_RUNTIME_DIR")?;

    let filename = match channel {
        VersionChannel::Stable => "ccusage-update-stable.json",
        VersionChannel::Latest => "ccusage-update-latest.json",
    };

    Ok(PathBuf::from(runtime_dir).join(filename))
}

fn read_cache(channel: VersionChannel) -> Option<UpdateCache> {
    let cache_path = get_cache_path(channel).ok()?;
    let contents = fs::read_to_string(cache_path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn write_cache(channel: VersionChannel, cache: &UpdateCache) -> Result<()> {
    let cache_path = get_cache_path(channel)?;
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

/// Determine which version channel to use based on enabled elements
fn get_version_channel() -> Option<VersionChannel> {
    let config = StatuslineConfig::load().ok()?;

    // Check which update element is enabled (prefer stable if both somehow enabled)
    if config
        .enabled_elements
        .contains(&StatusElement::UpdateStable)
    {
        Some(VersionChannel::Stable)
    } else if config
        .enabled_elements
        .contains(&StatusElement::UpdateLatest)
    {
        Some(VersionChannel::Latest)
    } else {
        None
    }
}

/// Check if a Claude Code update is available.
/// Returns Some(version) if an update is available, None otherwise.
/// Caches results for 30 minutes per channel.
pub fn check_update_available() -> Option<String> {
    let channel = get_version_channel()?;
    let current = get_installed_claude_version()?;

    // Try to read cache first
    if let Some(cache) = read_cache(channel)
        && is_cache_fresh(&cache)
    {
        if let Some(ref latest) = cache.latest_version
            && compare_versions(&current, latest)
        {
            return Some(latest.clone());
        }
        return None;
    }

    // Cache miss or stale - fetch new data
    let latest_version = match fetch_latest_version(channel) {
        Ok(version) => Some(version),
        Err(_) => {
            // Fail silently, use old cache if available
            read_cache(channel).and_then(|c| c.latest_version)
        }
    };

    // Update cache
    let new_cache = UpdateCache {
        latest_version: latest_version.clone(),
        checked_at: Utc::now(),
    };
    let _ = write_cache(channel, &new_cache);

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

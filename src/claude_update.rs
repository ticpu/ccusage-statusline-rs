use crate::cache::get_cache_dir;
use crate::claude_binary;
use crate::config::{StatusElement, StatuslineConfig};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code";
const GCS_STABLE_URL: &str = "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/stable";
const UPDATE_CHECK_CACHE_TTL: Duration = Duration::from_secs(1800); // 30 minutes

#[derive(Debug, Clone, Copy)]
enum VersionChannel {
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
    let cache_dir = get_cache_dir()?;

    let filename = match channel {
        VersionChannel::Stable => "update-stable.json",
        VersionChannel::Latest => "update-latest.json",
    };

    Ok(cache_dir.join(filename))
}

fn read_cache(channel: VersionChannel) -> Option<UpdateCache> {
    let cache_path = get_cache_path(channel).ok()?;
    match crate::cache::read_json::<UpdateCache>(&cache_path) {
        Ok(v) => v,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!("update cache read error: {:#}", e);
            }
            None
        }
    }
}

fn write_cache(channel: VersionChannel, cache: &UpdateCache) -> Result<()> {
    let cache_path = get_cache_path(channel)?;
    crate::cache::write_json_atomic(&cache_path, cache)
}

fn is_cache_fresh(cache: &UpdateCache) -> bool {
    let elapsed = Utc::now() - cache.checked_at;
    elapsed
        .to_std()
        .map(|d| d < UPDATE_CHECK_CACHE_TTL)
        .unwrap_or(false)
}

fn fetch_latest_version(channel: VersionChannel) -> Result<String> {
    let client = crate::http::http_client()?;

    match channel {
        VersionChannel::Stable => {
            let response = client
                .get(GCS_STABLE_URL)
                .send()
                .context("Failed to fetch GCS stable version")?;

            if !response
                .status()
                .is_success()
            {
                anyhow::bail!("GCS returned status: {}", response.status());
            }

            let version = response
                .text()
                .context("Failed to read version")?;
            Ok(version
                .trim()
                .to_string())
        }
        VersionChannel::Latest => {
            let response = client
                .get(NPM_REGISTRY_URL)
                .send()
                .context("Failed to fetch npm registry")?;

            if !response
                .status()
                .is_success()
            {
                anyhow::bail!("npm registry returned status: {}", response.status());
            }

            let data: NpmRegistryResponse = response
                .json()
                .context("Failed to parse npm registry response")?;

            Ok(data
                .dist_tags
                .latest)
        }
    }
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
    let current = claude_binary::get_version()?;

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
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!("update check failed, using cached version: {:#}", e);
            }
            read_cache(channel).and_then(|c| c.latest_version)
        }
    };

    // Update cache
    let new_cache = UpdateCache {
        latest_version: latest_version.clone(),
        checked_at: Utc::now(),
    };
    if let Err(e) = write_cache(channel, &new_cache)
        && std::io::stderr().is_terminal()
    {
        eprintln!("update cache write failed: {:#}", e);
    }

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

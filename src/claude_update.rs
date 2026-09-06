use crate::claude_binary;
use crate::config::{StatusElement, StatuslineConfig};
use crate::paths::Env;
use crate::warn;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code";
const GCS_STABLE_URL: &str = "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/stable";
const UPDATE_CHECK_CACHE_TTL: Duration = Duration::from_secs(1800);

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

fn get_cache_path(env: &Env, channel: VersionChannel) -> PathBuf {
    env.cache_file(match channel {
        VersionChannel::Stable => "update-stable.json",
        VersionChannel::Latest => "update-latest.json",
    })
}

fn read_cache(env: &Env, channel: VersionChannel) -> Option<UpdateCache> {
    crate::cache::read_json_warn(&get_cache_path(env, channel))
}

fn write_cache(env: &Env, channel: VersionChannel, cache: &UpdateCache) -> Result<()> {
    crate::cache::write_json_atomic(&get_cache_path(env, channel), cache)
}

fn is_cache_fresh(cache: &UpdateCache) -> bool {
    let elapsed = Utc::now() - cache.checked_at;
    elapsed
        .to_std()
        .map(|d| d < UPDATE_CHECK_CACHE_TTL)
        .unwrap_or(false)
}

fn fetch_body(url: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let response = crate::http::http_client()?
        .get(url)
        .send()
        .with_context(|| format!("Failed to fetch {url}"))?;

    if !response
        .status()
        .is_success()
    {
        anyhow::bail!("{url} returned status: {}", response.status());
    }

    crate::http::read_body_limited(response, max_bytes)
}

fn fetch_latest_version(channel: VersionChannel) -> Result<String> {
    match channel {
        VersionChannel::Stable => {
            let body = fetch_body(GCS_STABLE_URL, MAX_VERSION_BYTES)?;
            let version = String::from_utf8(body).context("Version response is not UTF-8")?;
            Ok(version
                .trim()
                .to_string())
        }
        VersionChannel::Latest => {
            let body = fetch_body(NPM_REGISTRY_URL, MAX_REGISTRY_BYTES)?;
            let data: NpmRegistryResponse =
                serde_json::from_slice(&body).context("Failed to parse npm registry response")?;
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
fn get_version_channel(config: &StatuslineConfig) -> Option<VersionChannel> {
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

/// A bare version string; the registry document is small but not fixed.
const MAX_VERSION_BYTES: u64 = 4 * 1024;
const MAX_REGISTRY_BYTES: u64 = 8 * 1024 * 1024;

/// Check if a Claude Code update is available.
/// Returns Some(version) if an update is available, None otherwise.
/// Caches results for 30 minutes per channel.
pub fn check_update_available(env: &Env, config: &StatuslineConfig) -> Option<String> {
    let channel = get_version_channel(config)?;
    let current = claude_binary::get_version(env)?;

    let cached = read_cache(env, channel);
    let still_fresh = cached
        .as_ref()
        .filter(|cache| is_cache_fresh(cache))
        .map(|cache| {
            cache
                .latest_version
                .clone()
        });

    let latest_version = match still_fresh {
        Some(version) => version,
        None => {
            let fetched = match fetch_latest_version(channel) {
                Ok(version) => Some(version),
                Err(e) => {
                    warn!("update check failed, using cached version: {:#}", e);
                    cached.and_then(|c| c.latest_version)
                }
            };
            // Written on failure too: without it every render retries the fetch.
            let new_cache = UpdateCache {
                latest_version: fetched.clone(),
                checked_at: Utc::now(),
            };
            if let Err(e) = write_cache(env, channel, &new_cache) {
                warn!("update cache write failed: {:#}", e);
            }
            fetched
        }
    };

    latest_version.filter(|latest| compare_versions(&current, latest))
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

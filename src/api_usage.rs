use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::time::Duration;

use crate::cache::get_cache_dir;
use crate::types::ApiUsageData;

#[derive(Debug, Serialize, Deserialize)]
struct UsageLimit {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiResponse {
    five_hour: UsageLimit,
    seven_day: UsageLimit,
    seven_day_sonnet: Option<UsageLimit>,
}

#[derive(Debug, Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthCredentials>,
}

#[derive(Debug, Deserialize)]
struct OAuthCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
}

const CACHE_TTL_SECS: u64 = 30;

/// Get API cache file path
fn get_api_cache_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
    Ok(cache_dir.join("api-usage-cache.json"))
}

/// Read OAuth credentials from Claude Code's credentials file
fn read_oauth_credentials() -> Result<String> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let creds_path = PathBuf::from(&home).join(".claude/.credentials.json");

    let content = fs::read_to_string(&creds_path).context(
        "Failed to read ~/.claude/.credentials.json - ensure you're logged in with Claude Code",
    )?;

    let creds: ClaudeCredentials =
        serde_json::from_str(&content).context("Failed to parse credentials file")?;

    creds
        .claude_ai_oauth
        .map(|oauth| oauth.access_token)
        .context("No OAuth credentials found - run 'claude' to login")
}

/// Fetch usage data from Anthropic API with filesystem-based caching and advisory locks
pub fn fetch_usage() -> Option<ApiUsageData> {
    match fetch_usage_with_lock() {
        Ok(data) => Some(data),
        Err(e) => {
            eprintln!("Failed to fetch API usage: {}", e);
            None
        }
    }
}

fn fetch_usage_with_lock() -> Result<ApiUsageData> {
    let cache_path = get_api_cache_path()?;

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&cache_path)?;

    // Try to acquire exclusive lock (non-blocking)
    match file.try_lock_exclusive() {
        Ok(()) => {
            // We have exclusive access - check cache and fetch if stale
            let result = fetch_or_use_cache(&mut file, &cache_path);
            file.unlock()?;
            result
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
            // Another process is fetching - wait with shared lock and read cache
            file.lock_shared()?;
            let result = read_cache_from_file(&mut file);
            file.unlock()?;
            result.context("Cache unavailable while another process is fetching")
        }
        Err(e) => Err(e.into()),
    }
}

fn fetch_or_use_cache(file: &mut File, cache_path: &PathBuf) -> Result<ApiUsageData> {
    // Check if cache is fresh using mtime
    let metadata = file.metadata()?;
    let mtime = metadata.modified()?;
    let age = mtime
        .elapsed()
        .unwrap_or(Duration::from_secs(CACHE_TTL_SECS + 1));

    if age < Duration::from_secs(CACHE_TTL_SECS) {
        // Cache is fresh - read it
        if let Ok(data) = read_cache_from_file(file) {
            return Ok(data);
        }
    }

    // Cache is stale or invalid - fetch fresh data
    let api_response = fetch_api_response()?;

    // Atomic write: temp file + rename (so shared lock readers always see valid data)
    let temp_path = cache_path.with_extension("tmp");
    let json = serde_json::to_string(&api_response)?;
    fs::write(&temp_path, json)?;
    fs::rename(&temp_path, cache_path)?;

    Ok(parse_api_response(api_response))
}

fn read_cache_from_file(file: &mut File) -> Result<ApiUsageData> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    if contents.is_empty() {
        anyhow::bail!("Cache file is empty");
    }

    let api_response: ApiResponse = serde_json::from_str(&contents)?;
    Ok(parse_api_response(api_response))
}

fn parse_api_response(api_response: ApiResponse) -> ApiUsageData {
    let five_hour_resets_at = api_response
        .five_hour
        .resets_at
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    let seven_day_resets_at = api_response
        .seven_day
        .resets_at
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    let seven_day_sonnet_percent = api_response
        .seven_day_sonnet
        .map(|l| l.utilization)
        .unwrap_or(0.0);

    ApiUsageData {
        five_hour_percent: api_response.five_hour.utilization,
        five_hour_resets_at,
        seven_day_percent: api_response.seven_day.utilization,
        seven_day_resets_at,
        seven_day_sonnet_percent,
    }
}

fn fetch_api_response() -> Result<ApiResponse> {
    let access_token = read_oauth_credentials()?;
    let user_agent = crate::claude_binary::get_user_agent();

    let url = "https://api.anthropic.com/api/oauth/usage";

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", user_agent)
        .send()
        .context("Failed to send request to Anthropic API")?;

    if !response.status().is_success() {
        anyhow::bail!("API returned status: {}", response.status());
    }

    response
        .json()
        .context("Failed to parse API response as JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_atomic_write_preserves_valid_data() {
        let cache_dir = std::env::temp_dir().join("ccusage-test-atomic");
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("test-cache.json");

        // Write initial valid data
        let initial_response = ApiResponse {
            five_hour: UsageLimit {
                utilization: 50.0,
                resets_at: Some("2025-11-01T12:00:00Z".to_string()),
            },
            seven_day: UsageLimit {
                utilization: 25.0,
                resets_at: Some("2025-11-02T12:00:00Z".to_string()),
            },
            seven_day_sonnet: Some(UsageLimit {
                utilization: 10.0,
                resets_at: Some("2025-11-02T12:00:00Z".to_string()),
            }),
        };
        fs::write(
            &cache_path,
            serde_json::to_string(&initial_response).unwrap(),
        )
        .unwrap();

        // Open file and simulate concurrent read during write
        let cache_path_clone = cache_path.clone();
        let reader = thread::spawn(move || {
            // Try to read while writer might be working
            for _ in 0..10 {
                if let Ok(mut file) = File::open(&cache_path_clone) {
                    let mut contents = String::new();
                    if file.read_to_string(&mut contents).is_ok() && !contents.is_empty() {
                        // Should always parse successfully - never see partial data
                        assert!(serde_json::from_str::<ApiResponse>(&contents).is_ok());
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
        });

        // Simulate atomic write
        let new_response = ApiResponse {
            five_hour: UsageLimit {
                utilization: 75.0,
                resets_at: Some("2025-11-01T13:00:00Z".to_string()),
            },
            seven_day: UsageLimit {
                utilization: 30.0,
                resets_at: Some("2025-11-02T13:00:00Z".to_string()),
            },
            seven_day_sonnet: Some(UsageLimit {
                utilization: 15.0,
                resets_at: Some("2025-11-02T13:00:00Z".to_string()),
            }),
        };

        let temp_path = cache_path.with_extension("tmp");
        fs::write(&temp_path, serde_json::to_string(&new_response).unwrap()).unwrap();
        fs::rename(&temp_path, &cache_path).unwrap();

        reader.join().unwrap();
        fs::remove_dir_all(&cache_dir).unwrap();
    }

    #[test]
    fn test_shared_lock_readers_wait_for_valid_data() {
        let cache_dir = std::env::temp_dir().join("ccusage-test-shared");
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("test-cache.json");

        // Write initial cache
        let response = ApiResponse {
            five_hour: UsageLimit {
                utilization: 50.0,
                resets_at: Some("2025-11-01T12:00:00Z".to_string()),
            },
            seven_day: UsageLimit {
                utilization: 25.0,
                resets_at: Some("2025-11-02T12:00:00Z".to_string()),
            },
            seven_day_sonnet: None,
        };
        fs::write(&cache_path, serde_json::to_string(&response).unwrap()).unwrap();

        let cache_path_shared = Arc::new(cache_path.clone());

        // Writer thread: holds exclusive lock
        let cache_path_writer = cache_path_shared.clone();
        let writer = thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&*cache_path_writer)
                .unwrap();
            file.lock_exclusive().unwrap();
            thread::sleep(Duration::from_millis(100));
            file.unlock().unwrap();
        });

        // Reader thread: waits with shared lock
        let cache_path_reader = cache_path_shared.clone();
        thread::sleep(Duration::from_millis(10)); // Let writer acquire lock first
        let reader = thread::spawn(move || {
            let mut file = File::open(&*cache_path_reader).unwrap();
            file.lock_shared().unwrap(); // Should block until writer releases
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();
            assert!(!contents.is_empty());
            assert!(serde_json::from_str::<ApiResponse>(&contents).is_ok());
            file.unlock().unwrap();
        });

        writer.join().unwrap();
        reader.join().unwrap();
        fs::remove_dir_all(&cache_dir).unwrap();
    }

    #[test]
    fn test_exclusive_lock_prevents_concurrent_writes() {
        let cache_dir = std::env::temp_dir().join("ccusage-test-exclusive");
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("test-cache.json");

        // Create empty cache file
        File::create(&cache_path).unwrap();

        let cache_path_shared = Arc::new(cache_path.clone());
        let acquired_count = Arc::new(std::sync::Mutex::new(0));

        let mut handles = vec![];
        for _ in 0..5 {
            let cache_path_thread = cache_path_shared.clone();
            let count = acquired_count.clone();
            let handle = thread::spawn(move || {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&*cache_path_thread)
                    .unwrap();
                if file.try_lock_exclusive().is_ok() {
                    *count.lock().unwrap() += 1;
                    thread::sleep(Duration::from_millis(50));
                    file.unlock().unwrap();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Only one thread should have acquired exclusive lock
        assert_eq!(*acquired_count.lock().unwrap(), 1);
        fs::remove_dir_all(&cache_dir).unwrap();
    }
}

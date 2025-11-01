use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use curl::easy::Easy;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::time::Duration;

use crate::cache::get_cache_dir;
use crate::firefox;
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
}

const CACHE_TTL_SECS: u64 = 30;

/// Get API cache file path
fn get_api_cache_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
    Ok(cache_dir.join("api-usage-cache.json"))
}

/// Fetch usage data from claude.ai API with filesystem-based caching and advisory locks
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

    ApiUsageData {
        five_hour_percent: api_response.five_hour.utilization,
        five_hour_resets_at,
        seven_day_percent: api_response.seven_day.utilization,
    }
}

fn fetch_api_response() -> Result<ApiResponse> {
    // Get cookies from Firefox
    let cookies = firefox::get_claude_cookies().context("Failed to extract Firefox cookies")?;

    // Build API URL
    let url = format!(
        "https://claude.ai/api/organizations/{}/usage",
        cookies.org_id
    );

    // Build cookie header (minimal: only sessionKey and lastActiveOrg)
    let cookie_header = format!(
        "sessionKey={}; lastActiveOrg={}",
        cookies.session_key, cookies.org_id
    );

    // Use libcurl (bypasses Cloudflare while reqwest gets 403)
    let mut easy = Easy::new();
    easy.url(&url)?;
    easy.timeout(Duration::from_secs(5))?;

    let mut headers = curl::easy::List::new();
    headers.append(&format!("User-Agent: {}", cookies.user_agent))?;
    headers.append(&format!("Cookie: {}", cookie_header))?;

    // Add anthropic headers (required by API as of late 2025)
    if let Some(anon_id) = &cookies.anonymous_id {
        headers.append(&format!("anthropic-anonymous-id: claudeai.v1.{}", anon_id))?;
    }
    if let Some(dev_id) = &cookies.device_id {
        headers.append(&format!("anthropic-device-id: {}", dev_id))?;
    }

    easy.http_headers(headers)?;

    let mut response_data = Vec::new();
    {
        let mut transfer = easy.transfer();
        transfer.write_function(|data| {
            response_data.extend_from_slice(data);
            Ok(data.len())
        })?;
        transfer.perform()?;
    }

    let response_code = easy.response_code()?;
    if response_code != 200 {
        anyhow::bail!("API returned status: {}", response_code);
    }

    let response_text =
        String::from_utf8(response_data).context("API response is not valid UTF-8")?;

    serde_json::from_str(&response_text).context("Failed to parse API response")
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

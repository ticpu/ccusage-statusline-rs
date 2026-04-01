use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, IsTerminal, Read};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::cache::get_cache_dir;
use crate::paths::claude_config_dir;
use crate::types::{ApiUsageData, PlanType};

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

#[derive(Debug, Serialize, Deserialize)]
struct CacheEnvelope {
    #[serde(default)]
    fetched_at: u64,
    #[serde(default)]
    consecutive_errors: u32,
    response: Option<ApiResponse>,
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
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

use crate::config::CacheSettings;

/// Result of API usage fetch
#[derive(Debug)]
pub enum ApiUsageResult {
    /// Valid, fresh data
    Ok(ApiUsageData),
    /// Cache is too old and fetch failed - show error to user
    StaleCache,
    /// API returned 429 - show rate limited indicator
    RateLimited,
    /// API not configured (no OAuth credentials) - show nothing
    Unavailable,
}

impl ApiUsageResult {
    /// Convert to Option<ApiUsageData> for backward compatibility
    pub fn data(&self) -> Option<&ApiUsageData> {
        match self {
            ApiUsageResult::Ok(data) => Some(data),
            _ => None,
        }
    }

    pub fn error_label(&self) -> Option<&'static str> {
        match self {
            ApiUsageResult::StaleCache => Some("api error"),
            ApiUsageResult::RateLimited => Some("rate limited"),
            _ => None,
        }
    }
}

/// Get API cache file path
fn get_api_cache_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
    Ok(cache_dir.join("api-usage-cache.json"))
}

fn read_credentials() -> Result<ClaudeCredentials> {
    let creds_path = claude_config_dir()?.join(".credentials.json");

    let content = fs::read_to_string(&creds_path)
        .context("Failed to read credentials - ensure you're logged in with Claude Code")?;

    serde_json::from_str(&content).context("Failed to parse credentials file")
}

fn read_oauth_credentials() -> Result<String> {
    let creds = read_credentials()?;
    creds
        .claude_ai_oauth
        .map(|oauth| oauth.access_token)
        .context("No OAuth credentials found - run 'claude' to login")
}

pub fn get_plan_type() -> PlanType {
    match read_credentials() {
        Ok(creds) => match creds.claude_ai_oauth {
            Some(oauth)
                if oauth
                    .subscription_type
                    .is_some() =>
            {
                PlanType::Subscription
            }
            _ => PlanType::Api,
        },
        Err(_) => PlanType::Api,
    }
}

/// Fetch usage data from Anthropic API with filesystem-based caching and advisory locks
pub fn fetch_usage(cache_settings: &CacheSettings) -> ApiUsageResult {
    // Check credentials first - if missing, skip network calls entirely
    if read_oauth_credentials().is_err() {
        return ApiUsageResult::Unavailable;
    }

    match fetch_usage_with_lock(cache_settings) {
        Ok((data, _fetched_at)) => ApiUsageResult::Ok(data),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("rate_limited") {
                ApiUsageResult::RateLimited
            } else {
                if std::io::stderr().is_terminal() {
                    eprintln!("Failed to fetch API usage: {}", e);
                }
                ApiUsageResult::StaleCache
            }
        }
    }
}

fn fetch_usage_with_lock(cache_settings: &CacheSettings) -> Result<(ApiUsageData, u64)> {
    let cache_path = get_api_cache_path()?;

    // Only open existing file — don't create an empty one
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&cache_path)
    {
        Ok(mut file) => match file.try_lock_exclusive() {
            Ok(()) => {
                let result = fetch_or_use_cache(&mut file, &cache_path, cache_settings);
                FileExt::unlock(&file)?;
                result
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                FileExt::lock_shared(&file)?;
                let result = read_envelope_from_file(&mut file);
                FileExt::unlock(&file)?;
                let envelope =
                    result.context("Cache unavailable while another process is fetching")?;
                let response = envelope
                    .response
                    .context("Cache has no response data yet")?;
                Ok((parse_api_response(response), envelope.fetched_at))
            }
            Err(e) => Err(e.into()),
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // No cache file — first run, fetch directly
            fetch_and_write_cache(&cache_path)
        }
        Err(e) => Err(e.into()),
    }
}

fn fetch_or_use_cache(
    file: &mut File,
    cache_path: &PathBuf,
    cache_settings: &CacheSettings,
) -> Result<(ApiUsageData, u64)> {
    let metadata = file.metadata()?;
    let mtime = metadata.modified()?;
    let mtime_age = mtime
        .elapsed()
        .unwrap_or(Duration::from_secs(cache_settings.api_refresh_secs + 1));

    let existing = if metadata.len() > 0 {
        read_envelope_from_file(file).ok()
    } else {
        None
    };

    // Exponential backoff: min(refresh * 2^errors, max_backoff)
    // 0 errors → 5m, 1 → 10m, 2 → 20m, 3+ → 30m (capped)
    let errors = existing
        .as_ref()
        .map_or(0, |e| e.consecutive_errors);
    let uncapped = cache_settings
        .api_refresh_secs
        .saturating_mul(1u64 << errors.min(6));
    let effective_fresh = uncapped.min(cache_settings.api_max_backoff_secs);

    // Return cached data if within backoff window and we have response data
    if mtime_age < Duration::from_secs(effective_fresh)
        && let Some(env) = existing
    {
        if let Some(response) = env.response {
            return Ok((parse_api_response(response), env.fetched_at));
        }
        // Envelope exists but no response data — still in backoff from prior failure
        anyhow::bail!("rate_limited");
    }

    match fetch_api_response() {
        Ok(api_response) => {
            let now = now_epoch();
            let envelope = CacheEnvelope {
                fetched_at: now,
                consecutive_errors: 0,
                response: Some(api_response),
            };
            write_envelope(&envelope, cache_path)?;
            Ok((
                parse_api_response(
                    envelope
                        .response
                        .unwrap(),
                ),
                now,
            ))
        }
        Err(fetch_err) => {
            let mut env = existing.unwrap_or(CacheEnvelope {
                fetched_at: now_epoch(),
                consecutive_errors: 0,
                response: None,
            });
            env.consecutive_errors = env
                .consecutive_errors
                .saturating_add(1);
            let next_backoff = cache_settings
                .api_refresh_secs
                .saturating_mul(
                    1u64 << env
                        .consecutive_errors
                        .min(6),
                )
                .min(cache_settings.api_max_backoff_secs);
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "API usage: fetch failed (attempt {}), next retry in {}s: {}",
                    env.consecutive_errors, next_backoff, fetch_err
                );
            }
            write_envelope(&env, cache_path)?;
            if let Some(response) = env.response {
                Ok((parse_api_response(response), env.fetched_at))
            } else {
                Err(fetch_err)
            }
        }
    }
}

fn fetch_and_write_cache(cache_path: &PathBuf) -> Result<(ApiUsageData, u64)> {
    match fetch_api_response() {
        Ok(api_response) => {
            let now = now_epoch();
            let envelope = CacheEnvelope {
                fetched_at: now,
                consecutive_errors: 0,
                response: Some(api_response),
            };
            write_envelope(&envelope, cache_path)?;
            Ok((
                parse_api_response(
                    envelope
                        .response
                        .unwrap(),
                ),
                now,
            ))
        }
        Err(fetch_err) => {
            let envelope = CacheEnvelope {
                fetched_at: now_epoch(),
                consecutive_errors: 1,
                response: None,
            };
            write_envelope(&envelope, cache_path)?;
            Err(fetch_err)
        }
    }
}

fn write_envelope(envelope: &CacheEnvelope, cache_path: &PathBuf) -> Result<()> {
    let temp_path = cache_path.with_extension("tmp");
    let json = serde_json::to_string(envelope)?;
    fs::write(&temp_path, json)?;
    fs::rename(&temp_path, cache_path)?;
    Ok(())
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_envelope_from_file(file: &mut File) -> Result<CacheEnvelope> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    if contents.is_empty() {
        anyhow::bail!("Cache file is empty");
    }

    let envelope: CacheEnvelope = serde_json::from_str(&contents)?;
    Ok(envelope)
}

fn parse_api_response(api_response: ApiResponse) -> ApiUsageData {
    let five_hour_resets_at = api_response
        .five_hour
        .resets_at
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .ok()
        });

    let seven_day_resets_at = api_response
        .seven_day
        .resets_at
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .ok()
        });

    let seven_day_sonnet_percent = api_response
        .seven_day_sonnet
        .map(|l| l.utilization)
        .unwrap_or(0.0);

    ApiUsageData {
        five_hour_percent: api_response
            .five_hour
            .utilization,
        five_hour_resets_at,
        seven_day_percent: api_response
            .seven_day
            .utilization,
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

    let status = response.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            if std::io::stderr().is_terminal() {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| {
                        v.to_str()
                            .ok()
                    });
                eprintln!(
                    "API 429: Retry-After={:?}, headers={:?}",
                    retry_after,
                    response.headers()
                );
            }
            anyhow::bail!("rate_limited");
        }
        anyhow::bail!("API returned status: {}", status);
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

    fn test_envelope(utilization_5h: f64, utilization_7d: f64) -> CacheEnvelope {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        CacheEnvelope {
            fetched_at: now,
            consecutive_errors: 0,
            response: Some(ApiResponse {
                five_hour: UsageLimit {
                    utilization: utilization_5h,
                    resets_at: Some("2025-11-01T12:00:00Z".to_string()),
                },
                seven_day: UsageLimit {
                    utilization: utilization_7d,
                    resets_at: Some("2025-11-02T12:00:00Z".to_string()),
                },
                seven_day_sonnet: None,
            }),
        }
    }

    #[test]
    fn test_atomic_write_preserves_valid_data() {
        let cache_dir = std::env::temp_dir().join("ccusage-test-atomic");
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("test-cache.json");

        let initial = test_envelope(50.0, 25.0);
        fs::write(&cache_path, serde_json::to_string(&initial).unwrap()).unwrap();

        // Open file and simulate concurrent read during write
        let cache_path_clone = cache_path.clone();
        let reader = thread::spawn(move || {
            // Try to read while writer might be working
            for _ in 0..10 {
                if let Ok(mut file) = File::open(&cache_path_clone) {
                    let mut contents = String::new();
                    if file
                        .read_to_string(&mut contents)
                        .is_ok()
                        && !contents.is_empty()
                    {
                        // Should always parse successfully - never see partial data
                        assert!(serde_json::from_str::<CacheEnvelope>(&contents).is_ok());
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
        });

        // Simulate atomic write
        let new = test_envelope(75.0, 30.0);
        let temp_path = cache_path.with_extension("tmp");
        fs::write(&temp_path, serde_json::to_string(&new).unwrap()).unwrap();
        fs::rename(&temp_path, &cache_path).unwrap();

        reader
            .join()
            .unwrap();
        fs::remove_dir_all(&cache_dir).unwrap();
    }

    #[test]
    fn test_shared_lock_readers_wait_for_valid_data() {
        let cache_dir = std::env::temp_dir().join("ccusage-test-shared");
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("test-cache.json");

        let envelope = test_envelope(50.0, 25.0);
        fs::write(&cache_path, serde_json::to_string(&envelope).unwrap()).unwrap();

        let cache_path_shared = Arc::new(cache_path.clone());

        // Writer thread: holds exclusive lock
        let cache_path_writer = cache_path_shared.clone();
        let writer = thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&*cache_path_writer)
                .unwrap();
            FileExt::lock_exclusive(&file).unwrap();
            thread::sleep(Duration::from_millis(100));
            FileExt::unlock(&file).unwrap();
        });

        // Reader thread: waits with shared lock
        let cache_path_reader = cache_path_shared.clone();
        thread::sleep(Duration::from_millis(10)); // Let writer acquire lock first
        let reader = thread::spawn(move || {
            let mut file = File::open(&*cache_path_reader).unwrap();
            FileExt::lock_shared(&file).unwrap(); // Should block until writer releases
            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .unwrap();
            assert!(!contents.is_empty());
            assert!(serde_json::from_str::<CacheEnvelope>(&contents).is_ok());
            FileExt::unlock(&file).unwrap();
        });

        writer
            .join()
            .unwrap();
        reader
            .join()
            .unwrap();
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
                if FileExt::try_lock_exclusive(&file).is_ok() {
                    *count
                        .lock()
                        .unwrap() += 1;
                    thread::sleep(Duration::from_millis(50));
                    FileExt::unlock(&file).unwrap();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle
                .join()
                .unwrap();
        }

        // Only one thread should have acquired exclusive lock
        assert_eq!(
            *acquired_count
                .lock()
                .unwrap(),
            1
        );
        fs::remove_dir_all(&cache_dir).unwrap();
    }

    #[test]
    fn test_api_usage_result_data() {
        let data = ApiUsageData {
            five_hour_percent: 25.0,
            five_hour_resets_at: None,
            seven_day_percent: 10.0,
            seven_day_resets_at: None,
            seven_day_sonnet_percent: 5.0,
        };
        let result = ApiUsageResult::Ok(data.clone());
        assert!(
            result
                .data()
                .is_some()
        );
        assert_eq!(
            result
                .data()
                .unwrap()
                .five_hour_percent,
            25.0
        );
        assert!(
            result
                .error_label()
                .is_none()
        );

        let stale = ApiUsageResult::StaleCache;
        assert!(
            stale
                .data()
                .is_none()
        );
        assert_eq!(stale.error_label(), Some("api error"));

        let rate_limited = ApiUsageResult::RateLimited;
        assert!(
            rate_limited
                .data()
                .is_none()
        );
        assert_eq!(rate_limited.error_label(), Some("rate limited"));

        let unavailable = ApiUsageResult::Unavailable;
        assert!(
            unavailable
                .data()
                .is_none()
        );
        assert!(
            unavailable
                .error_label()
                .is_none()
        );
    }
}

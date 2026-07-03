use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cache::get_cache_dir;
use crate::config::CacheSettings;
use crate::paths::claude_config_dir;
use crate::types::{ApiUsageData, PlanType, UsageWindow};

/// Typed marker for HTTP 429 rate-limit responses; survives anyhow context wrapping.
#[derive(Debug)]
struct RateLimited;

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("API rate limited (429)")
    }
}

impl std::error::Error for RateLimited {}

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
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "get_plan_type: credentials unreadable, defaulting to Api: {:#}",
                    e
                );
            }
            PlanType::Api
        }
    }
}

/// Fetch usage data from Anthropic API with filesystem-based caching and advisory locks
pub fn fetch_usage(cache_settings: &CacheSettings) -> ApiUsageResult {
    // Check credentials first - if missing, skip network calls entirely
    if read_oauth_credentials().is_err() {
        return ApiUsageResult::Unavailable;
    }

    let cache_path = match get_api_cache_path() {
        Ok(p) => p,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!("Failed to get API cache path: {:#}", e);
            }
            return ApiUsageResult::StaleCache;
        }
    };

    match fetch_usage_with_lock(&cache_path, cache_settings) {
        Ok(data) => ApiUsageResult::Ok(data),
        Err(e) => {
            if e.chain()
                .any(|cause| cause.is::<RateLimited>())
            {
                ApiUsageResult::RateLimited
            } else {
                if std::io::stderr().is_terminal() {
                    eprintln!("Failed to fetch API usage: {:#}", e);
                }
                ApiUsageResult::StaleCache
            }
        }
    }
}

fn fetch_usage_with_lock(
    cache_path: &Path,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    // Only open existing file — don't create an empty one
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache_path)
    {
        Ok(mut file) => match file.try_lock() {
            Ok(()) => {
                let result = fetch_or_use_cache(&mut file, cache_path, cache_settings);
                file.unlock()?;
                result
            }
            Err(TryLockError::WouldBlock) => {
                file.lock_shared()?;
                let result = read_envelope_from_file(&mut file);
                file.unlock()?;
                let envelope =
                    result.context("Cache unavailable while another process is fetching")?;
                let response = envelope
                    .response
                    .context("Cache has no response data yet")?;
                Ok(parse_api_response(&response))
            }
            Err(TryLockError::Error(e)) => Err(e.into()),
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // No cache file — first run; share the same core as the exclusive-lock path
            core_fetch_or_use_cache(None, Duration::MAX, cache_path, cache_settings)
        }
        Err(e) => Err(e.into()),
    }
}

fn fetch_or_use_cache(
    file: &mut File,
    cache_path: &Path,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    let metadata = file.metadata()?;
    let mtime_age = metadata
        .modified()?
        .elapsed()
        .unwrap_or(Duration::from_secs(cache_settings.api_refresh_secs + 1));

    let existing = if metadata.len() > 0 {
        match read_envelope_from_file(file) {
            Ok(envelope) => Some(envelope),
            Err(e) => {
                if std::io::stderr().is_terminal() {
                    eprintln!("API cache parse error (treating as absent): {:#}", e);
                }
                None
            }
        }
    } else {
        None
    };

    core_fetch_or_use_cache(existing, mtime_age, cache_path, cache_settings)
}

fn core_fetch_or_use_cache(
    existing: Option<CacheEnvelope>,
    mtime_age: Duration,
    cache_path: &Path,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    // Exponential backoff: min(refresh * 2^errors, max_backoff)
    // 0 errors → 5m, 1 → 10m, 2 → 20m, 3+ → 30m (capped)
    let errors = existing
        .as_ref()
        .map_or(0, |e| e.consecutive_errors);
    let uncapped = cache_settings
        .api_refresh_secs
        .saturating_mul(1u64 << errors.min(6));
    let effective_fresh = uncapped.min(cache_settings.api_max_backoff_secs);

    if mtime_age < Duration::from_secs(effective_fresh) {
        // Within backoff window — return cached data without a network call
        if let Some(response) = existing
            .as_ref()
            .and_then(|e| {
                e.response
                    .as_ref()
            })
        {
            return Ok(parse_api_response(response));
        }
        if existing.is_some() {
            // Envelope exists but has no response: in backoff after a prior non-429 failure.
            // Do not report this as "rate limited" — the original failure was something else.
            anyhow::bail!("no API data: in backoff after prior fetch failure");
        }
        // existing is None AND within backoff: cannot occur in production
        // (the NotFound path always passes mtime_age = Duration::MAX).
        // Fall through to fetch as a safe default.
    }

    match fetch_api_response() {
        Ok(api_response) => {
            let data = parse_api_response(&api_response);
            let envelope = CacheEnvelope {
                fetched_at: now_epoch(),
                consecutive_errors: 0,
                response: Some(api_response),
            };
            write_envelope(&envelope, cache_path)?;
            Ok(data)
        }
        Err(fetch_err) => {
            // Preserve any previously-cached response as a stale-but-valid fallback
            let stale = existing
                .as_ref()
                .and_then(|e| {
                    e.response
                        .as_ref()
                })
                .map(parse_api_response);

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
                    "API usage: fetch failed (attempt {}), next retry in {}s: {:#}",
                    env.consecutive_errors, next_backoff, fetch_err
                );
            }
            write_envelope(&env, cache_path)?;
            if let Some(data) = stale {
                Ok(data)
            } else {
                Err(fetch_err)
            }
        }
    }
}

fn write_envelope(envelope: &CacheEnvelope, cache_path: &Path) -> Result<()> {
    crate::cache::write_json_atomic(cache_path, envelope)
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

fn parse_window(limit: &UsageLimit) -> UsageWindow {
    let resets_at = limit
        .resets_at
        .as_deref()
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .ok()
        });
    UsageWindow {
        percent: limit.utilization,
        resets_at,
    }
}

fn parse_api_response(api_response: &ApiResponse) -> ApiUsageData {
    ApiUsageData {
        five_hour: Some(parse_window(&api_response.five_hour)),
        seven_day: Some(parse_window(&api_response.seven_day)),
        seven_day_sonnet: api_response
            .seven_day_sonnet
            .as_ref()
            .map(|l| l.utilization),
    }
}

fn fetch_api_response() -> Result<ApiResponse> {
    let access_token = read_oauth_credentials()?;
    let user_agent = crate::claude_binary::get_user_agent();

    let url = "https://api.anthropic.com/api/oauth/usage";

    let client = crate::http::http_client()?;

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
            return Err(anyhow::Error::new(RateLimited));
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

    fn make_test_envelope(utilization_5h: f64, utilization_7d: f64, errors: u32) -> CacheEnvelope {
        CacheEnvelope {
            fetched_at: now_epoch(),
            consecutive_errors: errors,
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

    fn make_error_envelope(errors: u32) -> CacheEnvelope {
        CacheEnvelope {
            fetched_at: now_epoch(),
            consecutive_errors: errors,
            response: None,
        }
    }

    /// write_envelope uses atomic rename; concurrent readers must never see partial data.
    #[test]
    fn test_atomic_write_preserves_valid_data() {
        let cache_dir =
            std::env::temp_dir().join(format!("ccusage-test-atomic-{}", std::process::id()));
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("api-usage-cache.json");

        let initial = make_test_envelope(50.0, 25.0, 0);
        write_envelope(&initial, &cache_path).unwrap();

        let path_clone = cache_path.clone();
        let reader = thread::spawn(move || {
            for _ in 0..10 {
                if let Ok(mut file) = File::open(&path_clone)
                    && let Ok(env) = read_envelope_from_file(&mut file)
                {
                    assert!(
                        env.response
                            .is_some(),
                        "response must be present"
                    );
                }
                thread::sleep(Duration::from_millis(1));
            }
        });

        let updated = make_test_envelope(75.0, 30.0, 0);
        write_envelope(&updated, &cache_path).unwrap();

        reader
            .join()
            .unwrap();
        fs::remove_dir_all(&cache_dir).unwrap();
    }

    /// When a writer holds the exclusive lock, fetch_usage_with_lock falls back to shared
    /// lock, blocks until the writer releases, then returns the cached response.
    #[test]
    fn test_shared_lock_readers_wait_for_valid_data() {
        let cache_dir =
            std::env::temp_dir().join(format!("ccusage-test-shared-{}", std::process::id()));
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = Arc::new(cache_dir.join("api-usage-cache.json"));
        let settings = CacheSettings::default();

        let envelope = make_test_envelope(50.0, 25.0, 0);
        write_envelope(&envelope, &cache_path).unwrap();

        let cache_path_writer = cache_path.clone();
        let writer = thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&*cache_path_writer)
                .unwrap();
            // Holding an exclusive lock forces concurrent readers onto the shared-lock path
            file.lock()
                .unwrap();
            thread::sleep(Duration::from_millis(100));
            file.unlock()
                .unwrap();
        });

        thread::sleep(Duration::from_millis(10));

        let result = fetch_usage_with_lock(&cache_path, &settings);
        writer
            .join()
            .unwrap();

        let data = result.expect("should return cached data via shared lock fallback");
        assert!(
            (data
                .five_hour
                .unwrap()
                .percent
                - 50.0)
                .abs()
                < 0.001
        );

        fs::remove_dir_all(&cache_dir).unwrap();
    }

    /// Concurrent callers with fresh cached data all get valid results — no network needed.
    #[test]
    fn test_concurrent_fetch_all_return_cached_data() {
        let cache_dir =
            std::env::temp_dir().join(format!("ccusage-test-concurrent-{}", std::process::id()));
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = Arc::new(cache_dir.join("api-usage-cache.json"));
        let settings = CacheSettings::default();

        let envelope = make_test_envelope(42.0, 20.0, 0);
        write_envelope(&envelope, &cache_path).unwrap();

        let mut handles = vec![];
        for _ in 0..5 {
            let path = cache_path.clone();
            let s = settings.clone();
            handles.push(thread::spawn(move || fetch_usage_with_lock(&path, &s)));
        }

        for handle in handles {
            let result = handle
                .join()
                .unwrap();
            let data = result.expect("all threads should get cached data without network");
            assert!(
                (data
                    .five_hour
                    .unwrap()
                    .percent
                    - 42.0)
                    .abs()
                    < 0.001
            );
        }

        fs::remove_dir_all(&cache_dir).unwrap();
    }

    /// A backoff envelope with no response (prior non-429 failure) must not be
    /// detected as RateLimited — it becomes StaleCache in the caller.
    #[test]
    fn test_backoff_no_response_error_is_not_rate_limited() {
        let cache_dir =
            std::env::temp_dir().join(format!("ccusage-test-backoff-{}", std::process::id()));
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_path = cache_dir.join("api-usage-cache.json");
        let settings = CacheSettings::default();

        let envelope = make_error_envelope(1);
        write_envelope(&envelope, &cache_path).unwrap();

        // mtime_age near-zero → within the 600s backoff window for 1 prior error
        let result = core_fetch_or_use_cache(
            Some(envelope),
            Duration::from_millis(1),
            &cache_path,
            &settings,
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            !err.chain()
                .any(|e| e.is::<RateLimited>()),
            "backoff-after-network-failure must not be classified as rate limited"
        );

        fs::remove_dir_all(&cache_dir).unwrap();
    }

    /// RateLimited must survive anyhow context wrapping for the chain().any() detection
    /// in fetch_usage to work correctly.
    #[test]
    fn test_rate_limited_error_survives_context_wrap() {
        let err = anyhow::Error::new(RateLimited).context("outer context");
        assert!(
            err.chain()
                .any(|e| e.is::<RateLimited>()),
            "RateLimited must be detectable through context wrappers"
        );
    }

    #[test]
    fn test_api_usage_result_data() {
        use crate::types::UsageWindow;
        let data = ApiUsageData {
            five_hour: Some(UsageWindow {
                percent: 25.0,
                resets_at: None,
            }),
            seven_day: Some(UsageWindow {
                percent: 10.0,
                resets_at: None,
            }),
            seven_day_sonnet: Some(5.0),
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
                .five_hour
                .as_ref()
                .unwrap()
                .percent,
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

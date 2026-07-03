use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, IsTerminal, Read};
use std::path::PathBuf;
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
    cache_path: &PathBuf,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    // Only open existing file — don't create an empty one
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache_path)
    {
        Ok(mut file) => match file.try_lock_exclusive() {
            Ok(()) => {
                let result = fetch_or_use_cache(&mut file, cache_path, cache_settings);
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
                Ok(parse_api_response(&response))
            }
            Err(e) => Err(e.into()),
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
    cache_path: &PathBuf,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    let metadata = file.metadata()?;
    let mtime_age = metadata
        .modified()?
        .elapsed()
        .unwrap_or(Duration::from_secs(cache_settings.api_refresh_secs + 1));

    let existing = if metadata.len() > 0 {
        read_envelope_from_file(file).ok()
    } else {
        None
    };

    core_fetch_or_use_cache(existing, mtime_age, cache_path, cache_settings)
}

fn core_fetch_or_use_cache(
    existing: Option<CacheEnvelope>,
    mtime_age: Duration,
    cache_path: &PathBuf,
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

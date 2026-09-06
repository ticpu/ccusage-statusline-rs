use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, TryLockError};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::config::CacheSettings;
use crate::paths::Env;
use crate::types::{ApiUsageData, PlanType, ScopedUsageWindow, UsageWindow};
use crate::warn;

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
    /// Per-model weekly windows; the only place the server reports them now.
    #[serde(default)]
    limits: Vec<LimitEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LimitEntry {
    kind: String,
    percent: Option<f64>,
    scope: Option<LimitScope>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LimitScope {
    model: Option<LimitModel>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LimitModel {
    display_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEnvelope {
    #[serde(default)]
    consecutive_errors: u32,
    response: Option<ApiResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeCredentials {
    claude_ai_oauth: Option<OAuthCredentials>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthCredentials {
    access_token: String,
    subscription_type: Option<String>,
}

/// Result of API usage fetch
#[derive(Debug)]
pub enum ApiUsageResult {
    /// Valid, fresh data
    Ok(ApiUsageData),
    /// Fetch failed and no usable cached response exists.
    Failed,
    /// API returned 429 - show rate limited indicator
    RateLimited,
    /// API not configured (no OAuth credentials) - show nothing
    Unavailable,
}

impl ApiUsageResult {
    pub fn data(&self) -> Option<&ApiUsageData> {
        match self {
            ApiUsageResult::Ok(data) => Some(data),
            _ => None,
        }
    }

    pub fn error_label(&self) -> Option<&'static str> {
        match self {
            ApiUsageResult::Failed => Some("api error"),
            ApiUsageResult::RateLimited => Some("rate limited"),
            _ => None,
        }
    }
}

/// The usage document is a handful of numbers.
const MAX_USAGE_BYTES: u64 = 1024 * 1024;

/// Label for the window the response reports outside limits[]
const SONNET_BUCKET: &str = "Sonnet";

fn read_credentials(env: &Env) -> Result<ClaudeCredentials> {
    let creds_path = env
        .config_dir
        .join(".credentials.json");

    let content = fs::read_to_string(&creds_path)
        .context("Failed to read credentials - ensure you're logged in with Claude Code")?;

    serde_json::from_str(&content).context("Failed to parse credentials file")
}

fn read_oauth_credentials(env: &Env) -> Result<String> {
    let creds = read_credentials(env)?;
    creds
        .claude_ai_oauth
        .map(|oauth| oauth.access_token)
        .context("No OAuth credentials found - run 'claude' to login")
}

pub fn get_plan_type(env: &Env) -> PlanType {
    match read_credentials(env) {
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
            warn!(
                "get_plan_type: credentials unreadable, defaulting to Api: {:#}",
                e
            );
            PlanType::Api
        }
    }
}

/// Fetch usage data from Anthropic API with filesystem-based caching and advisory locks
pub fn fetch_usage(env: &Env, cache_settings: &CacheSettings) -> ApiUsageResult {
    // Check credentials first - if missing, skip network calls entirely
    if read_oauth_credentials(env).is_err() {
        return ApiUsageResult::Unavailable;
    }

    let cache_path = env.cache_file("api-usage-cache.json");

    match fetch_usage_with_lock(env, &cache_path, cache_settings) {
        Ok(data) => ApiUsageResult::Ok(data),
        Err(e) => {
            if e.chain()
                .any(|cause| cause.is::<RateLimited>())
            {
                ApiUsageResult::RateLimited
            } else {
                warn!("Failed to fetch API usage: {:#}", e);
                ApiUsageResult::Failed
            }
        }
    }
}

fn fetch_usage_with_lock(
    env: &Env,
    cache_path: &Path,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    // Creating the file here is what lets every caller reach the lock below. Taking the
    // cold-start fetch unlocked let N concurrent statuslines each call the API, which is
    // the surest way to earn the 429 the backoff logic exists to handle.
    let mut file = crate::cache::open_private_rw(cache_path)?;

    match file.try_lock() {
        Ok(()) => {
            let result = fetch_or_use_cache(env, &mut file, cache_settings);
            file.unlock()?;
            result
        }
        Err(TryLockError::WouldBlock) => {
            file.lock_shared()?;
            let result = read_envelope_from_file(&mut file);
            file.unlock()?;
            let envelope = result.context("Cache unavailable while another process is fetching")?;
            let response = envelope
                .response
                .context("Cache has no response data yet")?;
            Ok(parse_api_response(&response))
        }
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

fn fetch_or_use_cache(
    env: &Env,
    file: &mut File,
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
                warn!("API cache parse error (treating as absent): {:#}", e);
                None
            }
        }
    } else {
        None
    };

    core_fetch_or_use_cache(env, existing, mtime_age, file, cache_settings)
}

/// How long a cache file stays fresh after `errors` consecutive failed fetches:
/// the refresh interval doubled per failure, capped at the configured maximum.
fn backoff_secs(errors: u32, cache_settings: &CacheSettings) -> u64 {
    cache_settings
        .api_refresh_secs
        .saturating_mul(1u64 << errors.min(6))
        .min(cache_settings.api_max_backoff_secs)
}

fn core_fetch_or_use_cache(
    env: &Env,
    existing: Option<CacheEnvelope>,
    mtime_age: Duration,
    file: &mut File,
    cache_settings: &CacheSettings,
) -> Result<ApiUsageData> {
    let errors = existing
        .as_ref()
        .map_or(0, |e| e.consecutive_errors);

    if mtime_age < Duration::from_secs(backoff_secs(errors, cache_settings)) {
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
        // No envelope yet (cold start just created the file): fall through and fetch.
    }

    match fetch_api_response(env) {
        Ok(api_response) => {
            let data = parse_api_response(&api_response);
            let envelope = CacheEnvelope {
                consecutive_errors: 0,
                response: Some(api_response),
            };
            write_envelope_locked(file, &envelope)?;
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

            let mut envelope = existing.unwrap_or(CacheEnvelope {
                consecutive_errors: 0,
                response: None,
            });
            envelope.consecutive_errors = envelope
                .consecutive_errors
                .saturating_add(1);
            let next_backoff = backoff_secs(envelope.consecutive_errors, cache_settings);
            warn!(
                "API usage: fetch failed (attempt {}), next retry in {}s: {:#}",
                envelope.consecutive_errors, next_backoff, fetch_err
            );
            write_envelope_locked(file, &envelope)?;
            if let Some(data) = stale {
                Ok(data)
            } else {
                Err(fetch_err)
            }
        }
    }
}

fn write_envelope_locked(file: &mut File, envelope: &CacheEnvelope) -> Result<()> {
    let json = serde_json::to_string(envelope)?;
    crate::cache::write_locked_in_place(file, json.as_bytes())
}

/// Open, lock and write in place. Seeds cache files for tests, which do not hold a lock.
#[cfg(test)]
fn write_envelope(envelope: &CacheEnvelope, cache_path: &Path) -> Result<()> {
    let mut file = crate::cache::open_private_rw(cache_path)?;
    file.lock()?;
    let result = write_envelope_locked(&mut file, envelope);
    file.unlock()?;
    result
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

fn parse_model_scoped(api_response: &ApiResponse) -> Vec<ScopedUsageWindow> {
    let mut scoped: Vec<ScopedUsageWindow> = api_response
        .limits
        .iter()
        .filter(|l| l.kind == "weekly_scoped")
        .filter_map(|l| {
            Some(ScopedUsageWindow {
                display_name: l
                    .scope
                    .as_ref()?
                    .model
                    .as_ref()?
                    .display_name
                    .clone(),
                percent: l.percent?,
            })
        })
        .collect();

    // Sonnet's weekly window has its own field rather than a limits[] entry, on the plans
    // that get one at all.
    if let Some(sonnet) = api_response
        .seven_day_sonnet
        .as_ref()
        && !scoped
            .iter()
            .any(|w| w.display_name == SONNET_BUCKET)
    {
        scoped.push(ScopedUsageWindow {
            display_name: SONNET_BUCKET.to_string(),
            percent: sonnet.utilization,
        });
    }

    scoped.sort_by(|a, b| {
        a.display_name
            .cmp(&b.display_name)
    });
    scoped
}

fn parse_api_response(api_response: &ApiResponse) -> ApiUsageData {
    ApiUsageData {
        five_hour: Some(parse_window(&api_response.five_hour)),
        seven_day: Some(parse_window(&api_response.seven_day)),
        model_scoped: parse_model_scoped(api_response),
    }
}

fn fetch_api_response(env: &Env) -> Result<ApiResponse> {
    let access_token = read_oauth_credentials(env)?;
    let user_agent = crate::claude_binary::get_user_agent(env);

    let url = "https://api.anthropic.com/api/oauth/usage";

    let client = crate::http::http_client()?;

    let response = client
        .get(url)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("User-Agent", user_agent)
        .send()
        .context("Failed to send request to Anthropic API")?;

    let status = response.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            warn!(
                "API 429: Retry-After={:?}, headers={:?}",
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| {
                        v.to_str()
                            .ok()
                    }),
                response.headers()
            );
            return Err(anyhow::Error::new(RateLimited));
        }
        anyhow::bail!("API returned status: {}", status);
    }

    let body = crate::http::read_body_limited(response, MAX_USAGE_BYTES)?;
    serde_json::from_slice(&body).context("Failed to parse API response as JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::sync::Arc;
    use std::thread;

    fn make_test_envelope(utilization_5h: f64, utilization_7d: f64, errors: u32) -> CacheEnvelope {
        CacheEnvelope {
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
                limits: Vec::new(),
            }),
        }
    }

    fn make_error_envelope(errors: u32) -> CacheEnvelope {
        CacheEnvelope {
            consecutive_errors: errors,
            response: None,
        }
    }

    /// Concurrent readers must never observe a half-written envelope.
    #[test]
    fn test_atomic_write_preserves_valid_data() {
        let cache_dir = crate::paths::test_scratch_dir("api-usage-atomic");
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
    }

    /// When a writer holds the exclusive lock, fetch_usage_with_lock falls back to shared
    /// lock, blocks until the writer releases, then returns the cached response.
    #[test]
    fn test_shared_lock_readers_wait_for_valid_data() {
        let cache_dir = crate::paths::test_scratch_dir("api-usage-shared");
        let env = Env::under(&cache_dir).unwrap();
        let cache_path = Arc::new(env.cache_file("api-usage-cache.json"));
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

        let result = fetch_usage_with_lock(&env, &cache_path, &settings);
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
    }

    /// Concurrent callers with fresh cached data all get valid results — no network needed.
    #[test]
    fn test_concurrent_fetch_all_return_cached_data() {
        let cache_dir = crate::paths::test_scratch_dir("api-usage-concurrent");
        let env = Arc::new(Env::under(&cache_dir).unwrap());
        let cache_path = Arc::new(env.cache_file("api-usage-cache.json"));
        let settings = CacheSettings::default();

        let envelope = make_test_envelope(42.0, 20.0, 0);
        write_envelope(&envelope, &cache_path).unwrap();

        let mut handles = vec![];
        for _ in 0..5 {
            let env = env.clone();
            let path = cache_path.clone();
            let s = settings.clone();
            handles.push(thread::spawn(move || {
                fetch_usage_with_lock(&env, &path, &s)
            }));
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
    }

    /// A backoff envelope with no response (prior non-429 failure) must not be
    /// detected as RateLimited — it becomes Failed in the caller.
    #[test]
    fn test_backoff_no_response_error_is_not_rate_limited() {
        let cache_dir = crate::paths::test_scratch_dir("api-usage-backoff");
        let env = Env::under(&cache_dir).unwrap();
        let cache_path = env.cache_file("api-usage-cache.json");
        let settings = CacheSettings::default();

        let envelope = make_error_envelope(1);
        write_envelope(&envelope, &cache_path).unwrap();

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&cache_path)
            .unwrap();

        // mtime_age near-zero → within the 600s backoff window for 1 prior error
        let result = core_fetch_or_use_cache(
            &env,
            Some(envelope),
            Duration::from_millis(1),
            &mut file,
            &settings,
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            !err.chain()
                .any(|e| e.is::<RateLimited>()),
            "backoff-after-network-failure must not be classified as rate limited"
        );
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

    /// Per-model weekly windows arrive only in limits[], mixed in with the session and
    /// all-models entries that duplicate five_hour/seven_day.
    #[test]
    fn test_parse_model_scoped_from_limits() {
        let json = r#"{
            "five_hour": {"utilization": 17, "resets_at": "2026-08-13T23:59:59Z"},
            "seven_day": {"utilization": 45, "resets_at": "2026-08-17T01:59:59Z"},
            "seven_day_sonnet": null,
            "limits": [
                {"kind": "session", "percent": 17, "scope": null},
                {"kind": "weekly_all", "percent": 45, "scope": null},
                {"kind": "weekly_scoped", "percent": 26,
                 "scope": {"model": {"id": null, "display_name": "Fable"}}},
                {"kind": "weekly_scoped", "percent": 8,
                 "scope": {"model": {"id": null, "display_name": "Aria"}}},
                {"kind": "weekly_scoped", "percent": null,
                 "scope": {"model": {"id": null, "display_name": "Nameless"}}}
            ]
        }"#;
        let response: ApiResponse = serde_json::from_str(json).unwrap();
        let data = parse_api_response(&response);
        let scoped: Vec<(&str, f64)> = data
            .model_scoped
            .iter()
            .map(|w| {
                (
                    w.display_name
                        .as_str(),
                    w.percent,
                )
            })
            .collect();
        assert_eq!(scoped, vec![("Aria", 8.0), ("Fable", 26.0)]);
    }

    /// The plans that have a Sonnet weekly window get it in its own field, never in limits[].
    #[test]
    fn test_sonnet_field_joins_model_scoped() {
        let json = r#"{
            "five_hour": {"utilization": 17, "resets_at": "2026-08-13T23:59:59Z"},
            "seven_day": {"utilization": 45, "resets_at": "2026-08-17T01:59:59Z"},
            "seven_day_sonnet": {"utilization": 12, "resets_at": "2026-08-17T01:59:59Z"},
            "limits": [
                {"kind": "weekly_scoped", "percent": 26,
                 "scope": {"model": {"id": null, "display_name": "Fable"}}}
            ]
        }"#;
        let response: ApiResponse = serde_json::from_str(json).unwrap();
        let data = parse_api_response(&response);
        let scoped: Vec<(&str, f64)> = data
            .model_scoped
            .iter()
            .map(|w| {
                (
                    w.display_name
                        .as_str(),
                    w.percent,
                )
            })
            .collect();
        assert_eq!(scoped, vec![("Fable", 26.0), ("Sonnet", 12.0)]);
    }

    /// Caches written before limits[] was parsed must still load.
    #[test]
    fn test_parse_response_without_limits() {
        let json = r#"{
            "five_hour": {"utilization": 17, "resets_at": "2026-08-13T23:59:59Z"},
            "seven_day": {"utilization": 45, "resets_at": "2026-08-17T01:59:59Z"},
            "seven_day_sonnet": null
        }"#;
        let response: ApiResponse = serde_json::from_str(json).unwrap();
        assert!(
            parse_api_response(&response)
                .model_scoped
                .is_empty()
        );
    }

    #[test]
    fn test_api_usage_result_error_label() {
        assert_eq!(ApiUsageResult::Failed.error_label(), Some("api error"));
        assert_eq!(
            ApiUsageResult::RateLimited.error_label(),
            Some("rate limited")
        );
    }
}

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::firefox;
use crate::types::ApiUsageData;

#[derive(Debug, Deserialize)]
struct UsageLimit {
    utilization: u32,
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    five_hour: UsageLimit,
    seven_day: UsageLimit,
}

struct CachedResponse {
    data: ApiUsageData,
    timestamp: SystemTime,
}

static CACHE: Mutex<Option<CachedResponse>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Fetch usage data from claude.ai API with caching
pub fn fetch_usage() -> Option<ApiUsageData> {
    // Check cache first
    if let Ok(cache) = CACHE.lock()
        && let Some(cached) = cache.as_ref()
        && cached.timestamp.elapsed().unwrap_or(CACHE_TTL) < CACHE_TTL
    {
        return Some(cached.data.clone());
    }

    // Fetch fresh data
    match fetch_usage_internal() {
        Ok(data) => {
            // Update cache
            if let Ok(mut cache) = CACHE.lock() {
                *cache = Some(CachedResponse {
                    data: data.clone(),
                    timestamp: SystemTime::now(),
                });
            }
            Some(data)
        }
        Err(e) => {
            eprintln!("Failed to fetch usage from API: {}", e);
            None
        }
    }
}

fn fetch_usage_internal() -> Result<ApiUsageData> {
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

    // Use curl directly since it bypasses Cloudflare while reqwest gets 403
    let output = Command::new("curl")
        .arg("-s")
        .arg("-m")
        .arg("5")
        .arg(&url)
        .arg("-H")
        .arg(format!("User-Agent: {}", cookies.user_agent))
        .arg("-H")
        .arg(format!("Cookie: {}", cookie_header))
        .output()
        .context("Failed to execute curl")?;

    if !output.status.success() {
        anyhow::bail!("curl failed with status: {}", output.status);
    }

    let response_text =
        String::from_utf8(output.stdout).context("curl output is not valid UTF-8")?;

    let api_response: ApiResponse =
        serde_json::from_str(&response_text).context("Failed to parse API response")?;

    let five_hour_resets_at = api_response
        .five_hour
        .resets_at
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    Ok(ApiUsageData {
        five_hour_percent: api_response.five_hour.utilization,
        five_hour_resets_at,
        seven_day_percent: api_response.seven_day.utilization,
    })
}

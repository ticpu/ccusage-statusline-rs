use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct FirefoxCookies {
    pub session_key: String,
    pub org_id: String,
    pub user_agent: String,
    pub anonymous_id: Option<String>,
    pub device_id: Option<String>,
}

/// Find Firefox profile directory matching Claude userID
pub fn find_firefox_profile() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;

    let claude_json_path = PathBuf::from(&home).join(".claude/claude.json");
    let user_id = if claude_json_path.exists() {
        let content =
            fs::read_to_string(&claude_json_path).context("Failed to read .claude/claude.json")?;
        let json: serde_json::Value =
            serde_json::from_str(&content).context("Failed to parse .claude/claude.json")?;
        json.get("userID")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    let firefox_dir = PathBuf::from(&home).join(".mozilla/firefox");
    if !firefox_dir.exists() {
        anyhow::bail!("Firefox directory not found");
    }

    let mut profiles = Vec::new();
    for entry in fs::read_dir(&firefox_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let cookies_path = path.join("cookies.sqlite");
            if cookies_path.exists()
                && let Ok(metadata) = fs::metadata(&cookies_path)
            {
                profiles.push((path, metadata.modified().ok()));
            }
        }
    }

    if profiles.is_empty() {
        anyhow::bail!("No Firefox profiles with cookies found");
    }

    if let Some(uid) = user_id {
        for (profile_path, _) in &profiles {
            if let Ok(match_found) = profile_matches_user_id(profile_path, &uid)
                && match_found
            {
                return Ok(profile_path.clone());
            }
        }
    }

    profiles.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(profiles[0].0.clone())
}

fn profile_matches_user_id(profile_path: &Path, user_id: &str) -> Result<bool> {
    let cookies_db = profile_path.join("cookies.sqlite");
    let db_uri = format!("file:{}?immutable=1", cookies_db.display());
    let conn = Connection::open(db_uri)?;

    let has_session: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM moz_cookies WHERE host LIKE '%claude.ai%' AND name = 'sessionKey'",
            [],
            |row| {
                let count: i64 = row.get(0)?;
                Ok(count > 0)
            },
        )
        .unwrap_or(false);

    if !has_session {
        return Ok(false);
    }

    let cookie_value: Option<String> = conn
        .query_row(
            "SELECT value FROM moz_cookies WHERE host LIKE '%claude.ai%' AND (name = 'ajs_user_id' OR value LIKE ?1)",
            [format!("%{}%", user_id)],
            |row| row.get(0),
        )
        .ok();

    Ok(cookie_value.is_some())
}

/// Extract cookies from Firefox profile
pub fn extract_cookies(profile_path: &Path) -> Result<FirefoxCookies> {
    let cookies_db = profile_path.join("cookies.sqlite");

    // Use immutable=1 to read locked database
    let db_uri = format!("file:{}?immutable=1", cookies_db.display());
    let conn = Connection::open(db_uri).context("Failed to open Firefox cookies database")?;

    // Extract sessionKey
    let session_key: String = conn
        .query_row(
            "SELECT value FROM moz_cookies WHERE host LIKE '%claude.ai%' AND name = 'sessionKey'",
            [],
            |row| row.get(0),
        )
        .context(
            "sessionKey cookie not found - visit https://claude.ai/settings/usage in Firefox",
        )?;

    // Extract lastActiveOrg
    let org_id: String = conn
        .query_row(
            "SELECT value FROM moz_cookies WHERE host LIKE '%claude.ai%' AND name = 'lastActiveOrg'",
            [],
            |row| row.get(0),
        )
        .context("Failed to find lastActiveOrg cookie")?;

    // Extract ajs_anonymous_id (for anthropic-anonymous-id header)
    let anonymous_id: Option<String> = conn
        .query_row(
            "SELECT value FROM moz_cookies WHERE host LIKE '%claude.ai%' AND name = 'ajs_anonymous_id'",
            [],
            |row| row.get(0),
        )
        .ok();

    // Extract anthropic-device-id
    let device_id: Option<String> = conn
        .query_row(
            "SELECT value FROM moz_cookies WHERE host LIKE '%claude.ai%' AND name = 'anthropic-device-id'",
            [],
            |row| row.get(0),
        )
        .ok();

    let user_agent = get_firefox_user_agent();

    Ok(FirefoxCookies {
        session_key,
        org_id,
        user_agent,
        anonymous_id,
        device_id,
    })
}

/// Get Firefox user agent string
fn get_firefox_user_agent() -> String {
    let version = get_firefox_version().unwrap_or_else(|| "144.0".to_string());
    format!(
        "Mozilla/5.0 (X11; Linux x86_64; rv:{}) Gecko/20100101 Firefox/{}",
        version, version
    )
}

/// Extract Firefox version from binary
fn get_firefox_version() -> Option<String> {
    let firefox_paths = [
        "/usr/lib/firefox/firefox",
        "/usr/bin/firefox",
        "/opt/firefox/firefox",
    ];

    for path in &firefox_paths {
        if let Ok(content) = fs::read(path) {
            let content_str = String::from_utf8_lossy(&content);
            if let Some(pos) = content_str.find("version=") {
                let version_str = &content_str[pos + 8..];
                if let Some(end) = version_str.find('&') {
                    return Some(version_str[..end].to_string());
                }
            }
        }
    }
    None
}

/// Get Firefox cookies for Claude.ai
pub fn get_claude_cookies() -> Result<FirefoxCookies> {
    let profile = find_firefox_profile()?;
    extract_cookies(&profile)
}

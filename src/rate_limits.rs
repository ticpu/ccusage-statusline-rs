use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, IsTerminal, Read};
use std::path::{Path, PathBuf};

use crate::cache::get_cache_dir;
use crate::types::{ApiUsageData, RateLimits, UsageWindow};

/// Stored rate limit reading with DateTime
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRateLimitWindow {
    pub used_percentage: f64,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RateLimitsStore {
    five_hour: Option<StoredRateLimitWindow>,
    seven_day: Option<StoredRateLimitWindow>,
}

fn get_store_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    Ok(cache_dir.join("rate-limits-latest.json"))
}

/// Supersede rule: A supersedes B iff A.resets_at > B.resets_at OR (A.resets_at == B.resets_at AND A.used_percentage >= B.used_percentage)
fn supersedes(a: &StoredRateLimitWindow, b: &StoredRateLimitWindow) -> bool {
    a.resets_at > b.resets_at
        || (a.resets_at == b.resets_at && a.used_percentage >= b.used_percentage)
}

/// Convert stdin epoch seconds to DateTime<Utc>, discarding expired windows
fn parse_stdin_window(resets_at: i64) -> Option<DateTime<Utc>> {
    let dt = DateTime::from_timestamp(resets_at, 0)?;
    if dt <= Utc::now() { None } else { Some(dt) }
}

/// Update slot with new_window if new_window supersedes the current value; returns true if updated
fn merge_window(slot: &mut Option<StoredRateLimitWindow>, new: StoredRateLimitWindow) -> bool {
    match slot {
        Some(existing) if !supersedes(&new, existing) => false,
        _ => {
            *slot = Some(new);
            true
        }
    }
}

/// Merge stdin reading into the store, update if it supersedes
fn merge_and_update_store_at(stdin_limits: &RateLimits, store_path: &Path) -> Result<()> {
    #[allow(clippy::suspicious_open_options)]
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(store_path)?;

    file.lock()?;

    let mut store: RateLimitsStore = if file
        .metadata()?
        .len()
        > 0
    {
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        serde_json::from_str(&contents).unwrap_or_default()
    } else {
        RateLimitsStore::default()
    };

    let mut updated = false;

    if let Some(five_hour) = &stdin_limits.five_hour
        && let Some(resets_at) = parse_stdin_window(five_hour.resets_at)
    {
        updated |= merge_window(
            &mut store.five_hour,
            StoredRateLimitWindow {
                used_percentage: five_hour.used_percentage,
                resets_at,
            },
        );
    }

    if let Some(seven_day) = &stdin_limits.seven_day
        && let Some(resets_at) = parse_stdin_window(seven_day.resets_at)
    {
        updated |= merge_window(
            &mut store.seven_day,
            StoredRateLimitWindow {
                used_percentage: seven_day.used_percentage,
                resets_at,
            },
        );
    }

    if updated {
        crate::cache::write_json_atomic(store_path, &store)?;
    }

    file.unlock()?;
    Ok(())
}

/// Read the freshest stored readings, discarding expired windows
fn read_store_at(store_path: &Path) -> Result<RateLimitsStore> {
    let mut file = match File::open(store_path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Ok(RateLimitsStore::default());
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("Failed to open rate-limit store {}", store_path.display())
            });
        }
    };

    file.lock_shared()?;

    let mut contents = String::new();
    file.read_to_string(&mut contents)?;

    let mut store: RateLimitsStore = match serde_json::from_str(&contents) {
        Ok(s) => s,
        Err(e) => {
            if std::io::stderr().is_terminal() {
                eprintln!(
                    "Rate-limit store parse error ({}): {}",
                    store_path.display(),
                    e
                );
            }
            RateLimitsStore::default()
        }
    };

    file.unlock()?;

    let now = Utc::now();

    if let Some(five_hour) = &store.five_hour
        && five_hour.resets_at <= now
    {
        store.five_hour = None;
    }

    if let Some(seven_day) = &store.seven_day
        && seven_day.resets_at <= now
    {
        store.seven_day = None;
    }

    Ok(store)
}

/// Merge stdin rate_limits into the cross-session store, then merge with API usage data
pub fn merge_and_get_effective_usage(
    stdin_limits: Option<&RateLimits>,
    api_usage: Option<ApiUsageData>,
) -> Result<Option<ApiUsageData>> {
    let store_path = get_store_path()?;
    if let Some(limits) = stdin_limits {
        merge_and_update_store_at(limits, &store_path)?;
    }
    let store = read_store_at(&store_path)?;
    Ok(merge_store_with_api_usage(&store, api_usage))
}

/// Merge a stored window with an API window, preferring whichever supersedes; None if neither has valid data
fn effective_window(
    store_win: Option<&StoredRateLimitWindow>,
    api_win: Option<UsageWindow>,
    now: DateTime<Utc>,
) -> Option<UsageWindow> {
    let valid_store = store_win.filter(|w| w.resets_at > now);

    match (valid_store, api_win) {
        (Some(s), Some(a)) => match a.resets_at {
            Some(ar) if ar > now => {
                let api_stored = StoredRateLimitWindow {
                    used_percentage: a.percent,
                    resets_at: ar,
                };
                if supersedes(&api_stored, s) {
                    Some(a)
                } else {
                    Some(UsageWindow {
                        percent: s.used_percentage,
                        resets_at: Some(s.resets_at),
                    })
                }
            }
            _ => Some(UsageWindow {
                percent: s.used_percentage,
                resets_at: Some(s.resets_at),
            }),
        },
        (Some(s), None) => Some(UsageWindow {
            percent: s.used_percentage,
            resets_at: Some(s.resets_at),
        }),
        (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

fn merge_store_with_api_usage(
    store: &RateLimitsStore,
    api_usage: Option<ApiUsageData>,
) -> Option<ApiUsageData> {
    let now = Utc::now();

    let five_hour = effective_window(
        store
            .five_hour
            .as_ref(),
        api_usage
            .as_ref()
            .and_then(|a| {
                a.five_hour
                    .clone()
            }),
        now,
    );

    let seven_day = effective_window(
        store
            .seven_day
            .as_ref(),
        api_usage
            .as_ref()
            .and_then(|a| {
                a.seven_day
                    .clone()
            }),
        now,
    );

    let seven_day_sonnet = api_usage
        .as_ref()
        .and_then(|a| a.seven_day_sonnet);

    if five_hour.is_none() && seven_day.is_none() && seven_day_sonnet.is_none() {
        return None;
    }

    Some(ApiUsageData {
        five_hour,
        seven_day,
        seven_day_sonnet,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn test_supersede_rule_newer_resets_at_wins() {
        let older = StoredRateLimitWindow {
            used_percentage: 80.0,
            resets_at: Utc::now() + Duration::from_secs(3600),
        };
        let newer = StoredRateLimitWindow {
            used_percentage: 50.0,
            resets_at: Utc::now() + Duration::from_secs(7200),
        };
        assert!(supersedes(&newer, &older));
        assert!(!supersedes(&older, &newer));
    }

    #[test]
    fn test_supersede_rule_equal_resets_higher_percentage_wins() {
        let reset_time = Utc::now() + Duration::from_secs(3600);
        let lower = StoredRateLimitWindow {
            used_percentage: 50.0,
            resets_at: reset_time,
        };
        let higher = StoredRateLimitWindow {
            used_percentage: 75.0,
            resets_at: reset_time,
        };
        assert!(supersedes(&higher, &lower));
        assert!(!supersedes(&lower, &higher));
    }

    #[test]
    fn test_supersede_rule_equal_values() {
        let reset_time = Utc::now() + Duration::from_secs(3600);
        let a = StoredRateLimitWindow {
            used_percentage: 50.0,
            resets_at: reset_time,
        };
        let b = StoredRateLimitWindow {
            used_percentage: 50.0,
            resets_at: reset_time,
        };
        assert!(supersedes(&a, &b));
        assert!(supersedes(&b, &a));
    }

    #[test]
    fn test_parse_stdin_window_discards_expired() {
        let past_epoch = (Utc::now() - Duration::from_secs(3600)).timestamp();
        assert!(parse_stdin_window(past_epoch).is_none());
    }

    #[test]
    fn test_parse_stdin_window_accepts_future() {
        let future_epoch = (Utc::now() + Duration::from_secs(3600)).timestamp();
        let dt = parse_stdin_window(future_epoch);
        assert!(dt.is_some());
        assert!(dt.unwrap() > Utc::now());
    }

    #[test]
    fn test_merge_no_data_returns_none() {
        let result = merge_store_with_api_usage(&RateLimitsStore::default(), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_merge_all_none_api_returns_none() {
        let api = ApiUsageData {
            five_hour: None,
            seven_day: None,
            seven_day_sonnet: None,
        };
        let result = merge_store_with_api_usage(&RateLimitsStore::default(), Some(api));
        assert!(result.is_none());
    }

    #[test]
    fn test_merge_store_wins_when_no_api() {
        let store = RateLimitsStore {
            five_hour: Some(StoredRateLimitWindow {
                used_percentage: 42.0,
                resets_at: Utc::now() + Duration::from_secs(3600),
            }),
            seven_day: None,
        };
        let result = merge_store_with_api_usage(&store, None).unwrap();
        assert_eq!(
            result
                .five_hour
                .as_ref()
                .unwrap()
                .percent,
            42.0
        );
        assert!(
            result
                .seven_day
                .is_none()
        );
    }

    #[test]
    fn test_merge_order_independent() {
        let temp_dir = crate::paths::test_scratch_dir("rate-limits-merge-order");

        let reading_a = RateLimits {
            five_hour: Some(crate::types::RateLimitWindow {
                used_percentage: 50.0,
                resets_at: (Utc::now() + Duration::from_secs(3600)).timestamp(),
            }),
            seven_day: None,
        };

        let reading_b = RateLimits {
            five_hour: Some(crate::types::RateLimitWindow {
                used_percentage: 75.0,
                resets_at: (Utc::now() + Duration::from_secs(3600)).timestamp(),
            }),
            seven_day: None,
        };

        let store_path_1 = temp_dir.join("test-merge-1.json");
        let store_path_2 = temp_dir.join("test-merge-2.json");

        merge_and_update_store_at(&reading_a, &store_path_1).unwrap();
        merge_and_update_store_at(&reading_b, &store_path_1).unwrap();
        let result_1 = read_store_at(&store_path_1).unwrap();

        merge_and_update_store_at(&reading_b, &store_path_2).unwrap();
        merge_and_update_store_at(&reading_a, &store_path_2).unwrap();
        let result_2 = read_store_at(&store_path_2).unwrap();

        assert_eq!(
            result_1
                .five_hour
                .as_ref()
                .map(|w| w.used_percentage),
            result_2
                .five_hour
                .as_ref()
                .map(|w| w.used_percentage)
        );
        assert_eq!(
            result_1
                .five_hour
                .as_ref()
                .unwrap()
                .used_percentage,
            75.0
        );

        fs::remove_dir_all(&temp_dir).unwrap();
    }
}

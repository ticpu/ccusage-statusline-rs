use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, IsTerminal, Read};
use std::path::PathBuf;

use crate::cache::get_cache_dir;
use crate::types::{ApiUsageData, RateLimits};

/// Stored rate limit reading with DateTime
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRateLimitWindow {
    pub used_percentage: f64,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RateLimitsStore {
    five_hour: Option<StoredRateLimitWindow>,
    seven_day: Option<StoredRateLimitWindow>,
}

fn get_store_path() -> Result<PathBuf> {
    #[cfg(test)]
    if let Ok(test_path) = std::env::var("CCUSAGE_TEST_STORE_PATH") {
        let path = PathBuf::from(test_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        return Ok(path);
    }

    let cache_dir = get_cache_dir()?;
    fs::create_dir_all(&cache_dir)?;
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

/// Merge stdin reading into the store, update if it supersedes
fn merge_and_update_store(stdin_limits: &RateLimits) -> Result<()> {
    let store_path = get_store_path()?;

    #[allow(clippy::suspicious_open_options)]
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&store_path)?;

    file.lock_exclusive()?;

    let mut store: RateLimitsStore = if file
        .metadata()?
        .len()
        > 0
    {
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        serde_json::from_str(&contents).unwrap_or(RateLimitsStore {
            five_hour: None,
            seven_day: None,
        })
    } else {
        RateLimitsStore {
            five_hour: None,
            seven_day: None,
        }
    };

    let mut updated = false;

    if let Some(five_hour) = &stdin_limits.five_hour
        && let Some(resets_at) = parse_stdin_window(five_hour.resets_at)
    {
        let new_window = StoredRateLimitWindow {
            used_percentage: five_hour.used_percentage,
            resets_at,
        };

        if let Some(existing) = &store.five_hour {
            if supersedes(&new_window, existing) {
                store.five_hour = Some(new_window);
                updated = true;
            }
        } else {
            store.five_hour = Some(new_window);
            updated = true;
        }
    }

    if let Some(seven_day) = &stdin_limits.seven_day
        && let Some(resets_at) = parse_stdin_window(seven_day.resets_at)
    {
        let new_window = StoredRateLimitWindow {
            used_percentage: seven_day.used_percentage,
            resets_at,
        };

        if let Some(existing) = &store.seven_day {
            if supersedes(&new_window, existing) {
                store.seven_day = Some(new_window);
                updated = true;
            }
        } else {
            store.seven_day = Some(new_window);
            updated = true;
        }
    }

    if updated {
        let temp_path = store_path.with_extension("tmp");
        let json = serde_json::to_string(&store)?;
        fs::write(&temp_path, json)?;
        fs::rename(&temp_path, &store_path)?;
    }

    FileExt::unlock(&file)?;
    Ok(())
}

/// Read the freshest stored readings, discarding expired windows
fn read_store() -> Result<RateLimitsStore> {
    let store_path = get_store_path()?;

    let mut file = match File::open(&store_path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Ok(RateLimitsStore {
                five_hour: None,
                seven_day: None,
            });
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
            RateLimitsStore {
                five_hour: None,
                seven_day: None,
            }
        }
    };

    FileExt::unlock(&file)?;

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
    if let Some(limits) = stdin_limits {
        merge_and_update_store(limits)?;
    }
    let store = read_store()?;
    Ok(merge_store_with_api_usage(&store, api_usage))
}

fn merge_store_with_api_usage(
    store: &RateLimitsStore,
    api_usage: Option<ApiUsageData>,
) -> Option<ApiUsageData> {
    let mut result = api_usage
        .clone()
        .unwrap_or(ApiUsageData {
            five_hour_percent: 0.0,
            five_hour_resets_at: None,
            seven_day_percent: 0.0,
            seven_day_resets_at: None,
            seven_day_sonnet_percent: 0.0,
        });

    let now = Utc::now();

    if let Some(store_5h) = &store.five_hour
        && store_5h.resets_at > now
    {
        let store_reading = StoredRateLimitWindow {
            used_percentage: store_5h.used_percentage,
            resets_at: store_5h.resets_at,
        };

        let api_reading = api_usage
            .as_ref()
            .and_then(|api| {
                api.five_hour_resets_at
                    .map(|resets_at| StoredRateLimitWindow {
                        used_percentage: api.five_hour_percent,
                        resets_at,
                    })
            });

        match api_reading {
            Some(api_win) if api_win.resets_at > now && supersedes(&api_win, &store_reading) => {
                result.five_hour_percent = api_win.used_percentage;
                result.five_hour_resets_at = Some(api_win.resets_at);
            }
            _ => {
                result.five_hour_percent = store_reading.used_percentage;
                result.five_hour_resets_at = Some(store_reading.resets_at);
            }
        }
    }

    if let Some(store_7d) = &store.seven_day
        && store_7d.resets_at > now
    {
        let store_reading = StoredRateLimitWindow {
            used_percentage: store_7d.used_percentage,
            resets_at: store_7d.resets_at,
        };

        let api_reading = api_usage
            .as_ref()
            .and_then(|api| {
                api.seven_day_resets_at
                    .map(|resets_at| StoredRateLimitWindow {
                        used_percentage: api.seven_day_percent,
                        resets_at,
                    })
            });

        match api_reading {
            Some(api_win) if api_win.resets_at > now && supersedes(&api_win, &store_reading) => {
                result.seven_day_percent = api_win.used_percentage;
                result.seven_day_resets_at = Some(api_win.resets_at);
            }
            _ => {
                result.seven_day_percent = store_reading.used_percentage;
                result.seven_day_resets_at = Some(store_reading.resets_at);
            }
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_merge_order_independent() {
        let temp_dir = std::env::temp_dir().join("ccusage-test-merge-order");
        fs::create_dir_all(&temp_dir).unwrap();

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

        {
            unsafe {
                std::env::set_var(
                    "CCUSAGE_TEST_STORE_PATH",
                    store_path_1
                        .to_str()
                        .unwrap(),
                );
            }
            merge_and_update_store(&reading_a).unwrap();
            merge_and_update_store(&reading_b).unwrap();
            let result_1 = read_store().unwrap();

            unsafe {
                std::env::set_var(
                    "CCUSAGE_TEST_STORE_PATH",
                    store_path_2
                        .to_str()
                        .unwrap(),
                );
            }
            merge_and_update_store(&reading_b).unwrap();
            merge_and_update_store(&reading_a).unwrap();
            let result_2 = read_store().unwrap();

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
        }

        unsafe {
            std::env::remove_var("CCUSAGE_TEST_STORE_PATH");
        }
        fs::remove_dir_all(&temp_dir).unwrap();
    }
}

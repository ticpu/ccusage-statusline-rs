//! Burn rate of the billing block and of the endpoint's usage windows.
//!
//! The two numbers a window gets answer different questions. The ratio behind 🔥% compares
//! the window's mean rate with the pace that would land exactly on 100% at reset: will
//! this window run out before it resets. The ETA behind ⏱ is when it runs out, at the
//! window's recent pace (see usage_pace.rs). The mean serves the ETA only when no recent
//! pace is known, since idle time dilutes it: on a 7-day window it put the limit four
//! times further away than it was.

use crate::config::Thresholds;
use crate::types::{ActiveBlock, ApiUsageData, BurnRate, LimitType, UsageWindow};
use chrono::{DateTime, Duration, Utc};

/// Spans of the API usage windows the endpoint reports. Not the billing block: a block
/// is anchored by transcript activity, these by the server's own reset clock.
const FIVE_HOUR_WINDOW_HOURS: f64 = 5.0;
const SEVEN_DAY_WINDOW_HOURS: f64 = 168.0;

pub fn calculate_burn_rate(
    block: Option<&ActiveBlock>,
    api_usage: Option<&ApiUsageData>,
    thresholds: &Thresholds,
    now: DateTime<Utc>,
) -> BurnRate {
    let block = match block {
        Some(b) => b,
        None => return BurnRate::default(),
    };

    let elapsed = (now - block.start_time).num_minutes() as f64;

    if elapsed <= 0.0 {
        return BurnRate::default();
    }

    let cost_per_hour = (block.cost_usd / elapsed) * 60.0;

    let api_usage = match api_usage {
        Some(api) => api,
        None => {
            return BurnRate {
                cost_per_hour,
                ..Default::default()
            };
        }
    };

    let five_hour_ratio = api_usage
        .five_hour
        .as_ref()
        .map_or(0.0, |w| {
            calculate_limit_ratio(w.percent, w.resets_at, FIVE_HOUR_WINDOW_HOURS, now)
        });
    let seven_day_ratio = api_usage
        .seven_day
        .as_ref()
        .map_or(0.0, |w| {
            calculate_limit_ratio(w.percent, w.resets_at, SEVEN_DAY_WINDOW_HOURS, now)
        });

    let show_ratio = thresholds.burn_rate_show_ratio();
    let five_hour = api_usage
        .five_hour
        .as_ref();
    let seven_day = api_usage
        .seven_day
        .as_ref();
    let (critical_limit, ratio, window) = if five_hour_ratio >= show_ratio {
        (LimitType::FiveHour, five_hour_ratio, five_hour)
    } else if seven_day_ratio >= show_ratio {
        (LimitType::SevenDay, seven_day_ratio, seven_day)
    } else if five_hour_ratio > 0.0 {
        (LimitType::FiveHour, five_hour_ratio, five_hour)
    } else if seven_day_ratio > 0.0 {
        (LimitType::SevenDay, seven_day_ratio, seven_day)
    } else {
        (LimitType::None, 0.0, None)
    };

    let is_at_limit = api_usage
        .five_hour
        .as_ref()
        .is_some_and(|w| w.percent >= 100.0)
        || api_usage
            .seven_day
            .as_ref()
            .is_some_and(|w| w.percent >= 100.0);
    let reset_in = window
        .and_then(|w| w.resets_at)
        .map(|reset| reset - now);
    let eta = window.and_then(|w| time_to_limit(w, ratio, now));
    let seven_day_eta = seven_day.and_then(|w| time_to_limit(w, seven_day_ratio, now));

    BurnRate {
        cost_per_hour,
        ratio,
        seven_day_ratio,
        critical_limit,
        is_at_limit,
        reset_in,
        eta,
        seven_day_eta,
    }
}

/// Time until `window` reaches 100%: at its recent pace when one is known, else at its
/// mean rate, which is the time to reset scaled down by the burn ratio.
fn time_to_limit(window: &UsageWindow, ratio: f64, now: DateTime<Utc>) -> Option<Duration> {
    let reset_in = window.resets_at? - now;
    match window.pace_per_hour {
        Some(pace) if pace > 0.0 => {
            let hours = (100.0 - window.percent).max(0.0) / pace;
            Duration::try_seconds((hours * 3600.0) as i64)
        }
        _ => Some(scaled_eta(reset_in, ratio)),
    }
}

/// Time to reset scaled down by `ratio`: when the window runs out at its mean rate.
pub(crate) fn scaled_eta(reset_in: Duration, ratio: f64) -> Duration {
    // A zero or NaN ratio divides to infinity, which saturates to i64::MAX seconds;
    // chrono's Duration::seconds panics on that rather than saturating.
    if ratio.is_nan() || ratio <= 0.0 {
        return reset_in;
    }
    Duration::try_seconds((reset_in.num_seconds() as f64 / ratio) as i64).unwrap_or(reset_in)
}

/// Below this much elapsed window, the rate is not yet meaningful.
const MIN_ELAPSED_HOURS: f64 = 0.25;

fn calculate_limit_ratio(
    current_percent: f64,
    resets_at: Option<DateTime<Utc>>,
    block_duration_hours: f64,
    now: DateTime<Utc>,
) -> f64 {
    if current_percent <= 0.0 || current_percent >= 100.0 {
        return 0.0;
    }

    let reset_time = match resets_at {
        Some(t) => t,
        None => return 0.0,
    };

    let hours_until_reset = (reset_time - now).num_seconds() as f64 / 3600.0;

    if hours_until_reset <= 0.0 {
        return 0.0;
    }

    // Just after a reset the elapsed slice approaches zero and any usage in it divides
    // out to a nonsense rate — 2% spent with 4.99h left is not a 1000% burn rate.
    let api_elapsed_hours = block_duration_hours - hours_until_reset;
    if api_elapsed_hours < MIN_ELAPSED_HOURS {
        return 0.0;
    }

    let current_rate = current_percent / api_elapsed_hours;
    let remaining_percent = 100.0 - current_percent;
    let safe_rate = remaining_percent / hours_until_reset;

    if safe_rate > 0.0 {
        current_rate / safe_rate
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::burn_rate::{BurnRateDisplay, format_burn_rate_component};
    use crate::types::PlanType;
    use crate::usage_pace::PaceHistory;
    use chrono::TimeDelta;

    #[test]
    fn test_scaled_eta_non_positive_ratio_does_not_panic() {
        let reset_in = Duration::hours(3);
        assert_eq!(scaled_eta(reset_in, 0.0), reset_in);
        assert_eq!(scaled_eta(reset_in, -1.0), reset_in);
        assert_eq!(scaled_eta(reset_in, f64::NAN), reset_in);
        assert_eq!(scaled_eta(reset_in, f64::MIN_POSITIVE), reset_in);
    }

    #[test]
    fn test_scaled_eta_scales_by_ratio() {
        assert_eq!(scaled_eta(Duration::hours(4), 2.0), Duration::hours(2));
    }

    fn window(percent: f64, resets_at: DateTime<Utc>) -> UsageWindow {
        UsageWindow {
            percent,
            resets_at: Some(resets_at),
            pace_per_hour: None,
        }
    }

    /// The reported case: 7d at 99% with 63h to reset after 105h, burning 4%/h for the
    /// last hour. The window mean put the limit 63 minutes out; it came in 15.
    fn rendered_reported_case(with_history: bool) -> String {
        let now: DateTime<Utc> = "2026-11-20T12:00:00Z"
            .parse()
            .unwrap();
        let reset_7d = now + TimeDelta::hours(63);
        let usage_at = |percent| ApiUsageData {
            five_hour: Some(window(2.0, now + TimeDelta::hours(3))),
            seven_day: Some(window(percent, reset_7d)),
            model_scoped: Vec::new(),
        };

        let mut pace = PaceHistory::default();
        if with_history {
            for (ago, percent) in [(50, 95.0), (45, 96.0), (30, 97.0), (15, 98.0), (0, 99.0)] {
                pace.record(&usage_at(percent), now - TimeDelta::minutes(ago));
            }
        }
        let mut usage = usage_at(99.0);
        pace.apply(&mut usage, now);

        let block = ActiveBlock {
            start_time: now - TimeDelta::hours(1),
            cost_usd: 4.16,
            hours_remaining: 4.0,
        };
        let thresholds = Thresholds::default();
        let burn = calculate_burn_rate(Some(&block), Some(&usage), &thresholds, now);
        format_burn_rate_component(
            &burn,
            PlanType::Subscription,
            BurnRateDisplay::RateWithEta,
            &thresholds,
        )
        .unwrap()
    }

    #[test]
    fn test_eta_follows_the_last_hour() {
        let line = rendered_reported_case(true);
        assert!(line.contains("[⏱15m]"), "{line}");
    }

    #[test]
    fn test_eta_without_history_is_the_window_mean() {
        let line = rendered_reported_case(false);
        assert!(line.contains("[⏱63m]"), "{line}");
    }
}

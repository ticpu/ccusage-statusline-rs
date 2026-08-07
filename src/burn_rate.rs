use crate::types::{ActiveBlock, ApiUsageData, BurnRate, LimitType};
use anyhow::Result;
use chrono::{DateTime, Utc};

pub fn calculate_burn_rate(
    block: Option<&ActiveBlock>,
    api_usage: Option<&ApiUsageData>,
    burn_rate_show_ratio: f64,
) -> Result<BurnRate> {
    let block = match block {
        Some(b) => b,
        None => return Ok(BurnRate::default()),
    };

    let now = Utc::now();
    let elapsed = (now - block.start_time).num_minutes() as f64;

    if elapsed <= 0.0 {
        return Ok(BurnRate::default());
    }

    let cost_per_hour = (block.cost_usd / elapsed) * 60.0;

    let api_usage = match api_usage {
        Some(api) => api,
        None => {
            return Ok(BurnRate {
                cost_per_hour,
                ..Default::default()
            });
        }
    };

    let five_hour_resets_at = api_usage
        .five_hour
        .as_ref()
        .and_then(|w| w.resets_at);
    let seven_day_resets_at = api_usage
        .seven_day
        .as_ref()
        .and_then(|w| w.resets_at);

    let five_hour_ratio = api_usage
        .five_hour
        .as_ref()
        .map_or(0.0, |w| calculate_limit_ratio(w.percent, w.resets_at, 5.0));
    let seven_day_ratio = api_usage
        .seven_day
        .as_ref()
        .map_or(0.0, |w| {
            calculate_limit_ratio(w.percent, w.resets_at, 168.0)
        });

    let (critical_limit, ratio, reset_at) = if five_hour_ratio >= burn_rate_show_ratio {
        (LimitType::FiveHour, five_hour_ratio, five_hour_resets_at)
    } else if seven_day_ratio >= burn_rate_show_ratio {
        (LimitType::SevenDay, seven_day_ratio, seven_day_resets_at)
    } else if five_hour_ratio > 0.0 {
        (LimitType::FiveHour, five_hour_ratio, five_hour_resets_at)
    } else if seven_day_ratio > 0.0 {
        (LimitType::SevenDay, seven_day_ratio, seven_day_resets_at)
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
    let reset_in = reset_at.map(|reset| reset - now);
    let seven_day_reset_in = seven_day_resets_at.map(|reset| reset - now);

    Ok(BurnRate {
        cost_per_hour,
        ratio,
        seven_day_ratio,
        critical_limit,
        is_at_limit,
        reset_in,
        seven_day_reset_in,
    })
}

/// Below this much elapsed window, the rate is not yet meaningful.
const MIN_ELAPSED_HOURS: f64 = 0.25;

fn calculate_limit_ratio(
    current_percent: f64,
    resets_at: Option<DateTime<Utc>>,
    block_duration_hours: f64,
) -> f64 {
    if current_percent <= 0.0 || current_percent >= 100.0 {
        return 0.0;
    }

    let reset_time = match resets_at {
        Some(t) => t,
        None => return 0.0,
    };

    let now = Utc::now();
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

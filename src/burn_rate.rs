use crate::types::{ApiUsageData, Block, BurnRate, LimitType};
use anyhow::Result;
use chrono::{DateTime, Utc};

pub fn calculate_burn_rate(block: &Block, api_usage: Option<&ApiUsageData>) -> Result<BurnRate> {
    if !block.is_active {
        return Ok(BurnRate::default());
    }

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

    let five_hour_ratio = calculate_limit_ratio(
        api_usage.five_hour_percent,
        api_usage.five_hour_resets_at,
        5.0,
    );

    let seven_day_ratio = calculate_limit_ratio(
        api_usage.seven_day_percent,
        api_usage.seven_day_resets_at,
        168.0,
    );

    let (critical_limit, ratio, reset_at) = if five_hour_ratio >= 0.8 {
        (
            LimitType::FiveHour,
            five_hour_ratio,
            api_usage.five_hour_resets_at,
        )
    } else if seven_day_ratio >= 0.8 {
        (
            LimitType::SevenDay,
            seven_day_ratio,
            api_usage.seven_day_resets_at,
        )
    } else if five_hour_ratio > 0.0 {
        (
            LimitType::FiveHour,
            five_hour_ratio,
            api_usage.five_hour_resets_at,
        )
    } else if seven_day_ratio > 0.0 {
        (
            LimitType::SevenDay,
            seven_day_ratio,
            api_usage.seven_day_resets_at,
        )
    } else {
        (LimitType::None, 0.0, None)
    };

    let is_at_limit = api_usage.five_hour_percent >= 100.0 || api_usage.seven_day_percent >= 100.0;
    let reset_in = reset_at.map(|reset| reset - now);
    let seven_day_reset_in = api_usage.seven_day_resets_at.map(|reset| reset - now);

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

    let api_elapsed_hours = block_duration_hours - hours_until_reset;
    if api_elapsed_hours <= 0.0 {
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

use crate::types::{ApiUsageData, Block, BurnRate, ContextInfo, LimitType, PlanType};
use chrono::{Duration, Utc};
use num_format::{Locale, ToFormattedString};
use owo_colors::OwoColorize;

/// Format block cost
pub fn format_block_info(block: &Block) -> String {
    if !block.is_active {
        return "No block".to_string();
    }

    format_currency(block.cost_usd)
}

/// Pick clock emoji based on hours remaining
fn get_clock_emoji(remaining_hours: f64) -> &'static str {
    let remaining_minutes = remaining_hours * 60.0;

    if remaining_minutes < 15.0 {
        "🕛" // 12 o'clock
    } else if remaining_hours <= 1.0 {
        "🕐" // 1 o'clock
    } else if remaining_hours <= 2.0 {
        "🕑" // 2 o'clock
    } else if remaining_hours <= 3.0 {
        "🕒" // 3 o'clock
    } else if remaining_hours <= 4.0 {
        "🕓" // 4 o'clock
    } else {
        "🕔" // 5 o'clock
    }
}

/// Format 5-hour time remaining
pub fn format_time_remaining_5h(block: &Block, api_usage: &Option<ApiUsageData>) -> Option<String> {
    if !block.is_active {
        return None;
    }

    let now = Utc::now();
    let remaining_hours = if let Some(api) = api_usage {
        if let Some(reset_time) = api.five_hour_resets_at {
            (reset_time - now).num_seconds() as f64 / 3600.0
        } else {
            block.hours_remaining.unwrap_or(0.0)
        }
    } else {
        block.hours_remaining.unwrap_or(0.0)
    };

    Some(format_hours_remaining(remaining_hours))
}

/// Format 7-day time remaining
pub fn format_time_remaining_7d(api_usage: &Option<ApiUsageData>) -> Option<String> {
    let now = Utc::now();

    if let Some(api) = api_usage
        && let Some(reset_time) = api.seven_day_resets_at
    {
        let remaining_hours = (reset_time - now).num_seconds() as f64 / 3600.0;
        Some(format_days_remaining(remaining_hours))
    } else {
        None
    }
}

/// Format hours remaining with clock emoji
fn format_hours_remaining(remaining_hours: f64) -> String {
    if remaining_hours <= 0.0 {
        return format!("{}0h", get_clock_emoji(0.0));
    }

    let hours = remaining_hours.floor() as i64;
    let mins = ((remaining_hours - hours as f64) * 60.0).round() as i64;
    let clock = get_clock_emoji(remaining_hours);

    if hours > 0 && mins > 0 {
        format!("{}{}h{}m", clock, hours, mins)
    } else if hours > 0 {
        format!("{}{}h", clock, hours)
    } else {
        format!("{}{}m", clock, mins)
    }
}

/// Format days remaining for weekly reset
fn format_days_remaining(remaining_hours: f64) -> String {
    if remaining_hours <= 0.0 {
        return "📅0d".to_string();
    }

    let days = (remaining_hours / 24.0).floor() as i64;
    let hours = (remaining_hours % 24.0).floor() as i64;

    if days > 0 && hours > 0 {
        format!("📅{}d{}h", days, hours)
    } else if days > 0 {
        format!("📅{}d", days)
    } else {
        format!("📅{}h", hours)
    }
}

/// Format duration in human readable form
fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.num_seconds();
    if total_seconds < 0 {
        return "0m".to_string();
    }

    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;

    if hours > 0 {
        format!("{}h{}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

/// Format burn rate with color indicator
pub fn format_burn_rate(burn_rate: &BurnRate, plan_type: PlanType) -> String {
    if burn_rate.is_at_limit {
        if let Some(reset) = burn_rate.reset_in {
            return format!("🔥limit {}", format_duration(reset));
        }
        return "🔥limit".to_string();
    }

    let limit = match burn_rate.critical_limit {
        LimitType::FiveHour => " 5h",
        LimitType::SevenDay => " 7d",
        LimitType::None => "",
    };

    let rate_str = match plan_type {
        PlanType::Api => format!("{}/h", format_currency(burn_rate.cost_per_hour)),
        PlanType::Subscription => format!("{:.1}x", burn_rate.ratio),
    };

    let colored_rate = if burn_rate.ratio >= 1.0 {
        rate_str.red().to_string()
    } else if burn_rate.ratio >= 0.8 {
        rate_str.yellow().to_string()
    } else {
        rate_str.green().to_string()
    };

    format!("🔥\u{200B}{}{}", colored_rate, limit)
}

/// Format context information
pub fn format_context(context: &Option<ContextInfo>) -> String {
    match context {
        Some(info) => {
            let color = if info.percentage < 50 {
                info.percentage.to_string().green().to_string()
            } else if info.percentage < 70 {
                info.percentage.to_string().yellow().to_string()
            } else {
                info.percentage.to_string().red().to_string()
            };

            format!("{}({}%)", format_number(info.tokens), color)
        }
        None => "N/A".to_string(),
    }
}

/// Format number with locale-based thousand separators
pub fn format_number(n: u64) -> String {
    n.to_formatted_string(&Locale::en)
}

/// Format currency with locale-based formatting
pub fn format_currency(amount: f64) -> String {
    format!("${:.2}", amount)
}

/// Map decimal portion (0.0-0.9) to Unicode block character (vertical fill)
fn decimal_to_block(value: f64) -> char {
    let decimal = value.fract();
    match (decimal * 10.0) as u32 {
        0 => ' ',
        1 => '▁',
        2 => '▂',
        3 => '▃',
        4 => '▄',
        5 => '▅',
        6 => '▆',
        7 => '▇',
        _ => '█',
    }
}

/// Format 5h API usage
pub fn format_api_usage_5h(api_usage: &Option<ApiUsageData>) -> Option<String> {
    api_usage.as_ref().map(|api| {
        let five_hour_int = api.five_hour_percent as u32;
        let five_hour_block = decimal_to_block(api.five_hour_percent);
        if five_hour_block == ' ' {
            format!("5h:{}%", five_hour_int)
        } else {
            format!("5h:{}%{}", five_hour_int, five_hour_block)
        }
    })
}

/// Format 7d API usage
pub fn format_api_usage_7d(api_usage: &Option<ApiUsageData>) -> Option<String> {
    api_usage
        .as_ref()
        .map(|api| format!("7d:{}%", api.seven_day_percent as u32))
}

/// Format Sonnet 7d API usage
pub fn format_api_usage_sonnet(api_usage: &Option<ApiUsageData>) -> Option<String> {
    api_usage
        .as_ref()
        .map(|api| format!("S7d:{}%", api.seven_day_sonnet_percent as u32))
}

/// Format directory path with home replacement and color
pub fn format_directory(path: &str) -> String {
    use std::env;

    let formatted = if let Ok(home) = env::var("HOME") {
        if path.starts_with(&home) {
            path.replacen(&home, "~", 1)
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    formatted.green().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decimal_to_block_zero() {
        assert_eq!(decimal_to_block(0.0), ' ');
        assert_eq!(decimal_to_block(50.0), ' ');
    }

    #[test]
    fn test_decimal_to_block_fractions() {
        assert_eq!(decimal_to_block(0.1), '▁');
        assert_eq!(decimal_to_block(0.5), '▅');
        assert_eq!(decimal_to_block(0.9), '█');
    }

    #[test]
    fn test_format_api_usage_5h_no_trailing_space() {
        let data = ApiUsageData {
            five_hour_percent: 37.0,
            five_hour_resets_at: None,
            seven_day_percent: 10.0,
            seven_day_resets_at: None,
            seven_day_sonnet_percent: 0.0,
        };
        let result = format_api_usage_5h(&Some(data)).unwrap();
        assert_eq!(result, "5h:37%");
        assert!(!result.ends_with(' '));
    }

    #[test]
    fn test_format_api_usage_5h_with_block() {
        let data = ApiUsageData {
            five_hour_percent: 37.5,
            five_hour_resets_at: None,
            seven_day_percent: 10.0,
            seven_day_resets_at: None,
            seven_day_sonnet_percent: 0.0,
        };
        let result = format_api_usage_5h(&Some(data)).unwrap();
        assert_eq!(result, "5h:37%▅");
    }

    #[test]
    fn test_format_currency() {
        assert_eq!(format_currency(12.345), "$12.35");
        assert_eq!(format_currency(0.0), "$0.00");
    }

    #[test]
    fn test_format_burn_rate() {
        let safe_burn = BurnRate {
            cost_per_hour: 1.5,
            ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
        };
        assert!(format_burn_rate(&safe_burn, PlanType::Api).contains("$1.50/h"));
        assert!(format_burn_rate(&safe_burn, PlanType::Subscription).contains("0.5x"));

        let warning_burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 0.9,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
        };
        assert!(format_burn_rate(&warning_burn, PlanType::Api).contains("$10.00/h"));
        assert!(format_burn_rate(&warning_burn, PlanType::Api).contains("5h"));

        let danger_burn = BurnRate {
            cost_per_hour: 15.0,
            ratio: 1.4,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
        };
        assert!(format_burn_rate(&danger_burn, PlanType::Subscription).contains("1.4x"));
        assert!(format_burn_rate(&danger_burn, PlanType::Subscription).contains("5h"));
    }
}

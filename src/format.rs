use crate::types::{ApiUsageData, Block, BurnRate, ContextInfo};
use chrono::Utc;
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

/// Format burn rate with emoji indicator
pub fn format_burn_rate(burn_rate: &BurnRate) -> String {
    let emoji = if burn_rate.tokens_per_minute < 2000 {
        "🟢".green().to_string()
    } else if burn_rate.tokens_per_minute < 5000 {
        "⚠️".yellow().to_string()
    } else {
        "🚨".red().to_string()
    };

    format!("{}/h {}", format_currency(burn_rate.cost_per_hour), emoji)
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
        format!("5h:{}%{}", five_hour_int, five_hour_block)
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

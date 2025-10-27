use crate::types::{ApiUsageData, Block, BurnRate, ContextInfo};
use chrono::Utc;
use owo_colors::OwoColorize;

/// Format block information (use API timing if available, more accurate)
pub fn format_block_info(block: &Block, api_usage: &Option<ApiUsageData>) -> String {
    if !block.is_active {
        return "No active block".to_string();
    }

    let now = Utc::now();

    // Use API reset time if available (includes web usage, more accurate)
    let remaining = if let Some(api) = api_usage {
        if let Some(reset_time) = api.five_hour_resets_at {
            (reset_time - now).num_minutes()
        } else {
            (block.end_time - now).num_minutes()
        }
    } else {
        (block.end_time - now).num_minutes()
    };

    let hours = remaining / 60;
    let mins = remaining % 60;

    format!("${:.2} block ({}h {}m left)", block.cost_usd, hours, mins)
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

    format!("${:.2}/hr {}", burn_rate.cost_per_hour, emoji)
}

/// Format context information
pub fn format_context(context: &Option<ContextInfo>) -> String {
    match context {
        Some(info) => {
            let color = if info.percentage < 70 {
                info.percentage.to_string().green().to_string()
            } else if info.percentage < 90 {
                info.percentage.to_string().yellow().to_string()
            } else {
                info.percentage.to_string().red().to_string()
            };

            format!("{} ({}%)", format_number(info.tokens), color)
        }
        None => "N/A".to_string(),
    }
}

/// Format number with thousand separators
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Format API usage data
pub fn format_api_usage(api_usage: &Option<ApiUsageData>) -> Option<String> {
    api_usage.as_ref().map(|api| {
        format!(
            "5h:{}% 7d:{}%",
            api.five_hour_percent, api.seven_day_percent
        )
    })
}

use crate::config::Thresholds;
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
    const CLOCKS: [&str; 6] = ["🕛", "🕐", "🕑", "🕒", "🕓", "🕔"];

    if remaining_hours * 60.0 < 15.0 {
        return CLOCKS[0];
    }
    let idx = (remaining_hours.ceil() as usize).clamp(1, 5);
    CLOCKS[idx]
}

/// Format 5-hour time remaining (subscription only)
pub fn format_time_remaining_5h(
    block: &Block,
    api_usage: Option<&ApiUsageData>,
    plan_type: PlanType,
) -> Option<String> {
    if matches!(plan_type, PlanType::Api) || !block.is_active {
        return None;
    }

    let now = Utc::now();
    let remaining_hours = if let Some(api) = api_usage {
        if let Some(reset_time) = api.five_hour_resets_at {
            (reset_time - now).num_seconds() as f64 / 3600.0
        } else {
            block
                .hours_remaining
                .unwrap_or(0.0)
        }
    } else {
        block
            .hours_remaining
            .unwrap_or(0.0)
    };

    Some(format_hours_remaining(remaining_hours))
}

/// Format 7-day time remaining (subscription only)
pub fn format_time_remaining_7d(
    api_usage: Option<&ApiUsageData>,
    plan_type: PlanType,
) -> Option<String> {
    if matches!(plan_type, PlanType::Api) {
        return None;
    }

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

/// Format ETA duration compactly: `87m`, `14h`, `3d1h`
fn format_eta(duration: Duration) -> String {
    let total_minutes = duration.num_minutes();
    if total_minutes < 0 {
        return "0m".to_string();
    }

    let total_hours = total_minutes as f64 / 60.0;

    if total_hours < 2.0 {
        format!("{}m", total_minutes)
    } else if total_hours < 24.0 {
        format!("{}h", total_hours.round() as i64)
    } else {
        let days = (total_hours / 24.0).floor() as i64;
        let hours = (total_hours - days as f64 * 24.0).round() as i64;
        if hours > 0 {
            format!("{}d{}h", days, hours)
        } else {
            format!("{}d", days)
        }
    }
}

/// Unified entry point for all burn rate display modes
pub fn format_burn_rate_component(
    burn_rate: &BurnRate,
    plan_type: PlanType,
    show_rate: bool,
    show_eta: bool,
    thresholds: &Thresholds,
) -> Option<String> {
    if !show_rate && !show_eta {
        return None;
    }

    let eta = show_eta && matches!(plan_type, PlanType::Subscription);

    if show_rate {
        Some(format_rate_display(burn_rate, plan_type, eta, thresholds))
    } else if eta {
        format_eta_only(burn_rate, thresholds)
    } else {
        None
    }
}

/// Format burn rate percentage/cost with optional inline ETA
fn format_rate_display(
    burn_rate: &BurnRate,
    plan_type: PlanType,
    show_eta: bool,
    thresholds: &Thresholds,
) -> String {
    if burn_rate.is_at_limit {
        return "🔥limit".to_string();
    }

    let rate_str = match plan_type {
        PlanType::Api => format!("{}/h", format_currency(burn_rate.cost_per_hour)),
        PlanType::Subscription => format!("{}%", (burn_rate.ratio * 100.0).round() as i32),
    };

    let colored_rate = if burn_rate.ratio >= thresholds.burn_rate_danger_ratio() {
        rate_str
            .red()
            .to_string()
    } else if burn_rate.ratio >= thresholds.burn_rate_warning_ratio() {
        rate_str
            .yellow()
            .to_string()
    } else {
        rate_str
            .green()
            .to_string()
    };

    let primary_eta = if show_eta && burn_rate.ratio >= thresholds.burn_rate_danger_ratio() {
        if let Some(reset_in) = burn_rate.reset_in {
            let eta_seconds = reset_in.num_seconds() as f64 / burn_rate.ratio;
            let eta_duration = Duration::seconds(eta_seconds as i64);
            format!("[⏱{}]", format_eta(eta_duration))
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let limit_str = match burn_rate.critical_limit {
        LimitType::FiveHour => " 5h",
        LimitType::SevenDay => " 7d",
        LimitType::None => "",
    };

    let seven_day_suffix = if burn_rate.seven_day_ratio >= thresholds.burn_rate_danger_ratio()
        && burn_rate.critical_limit != LimitType::SevenDay
    {
        let pct = (burn_rate.seven_day_ratio * 100.0).round() as i32;
        let seven_day_eta = if show_eta {
            if let Some(reset_in) = burn_rate.seven_day_reset_in {
                let eta_seconds = reset_in.num_seconds() as f64 / burn_rate.seven_day_ratio;
                let eta_duration = Duration::seconds(eta_seconds as i64);
                format!("[⏱{}]", format_eta(eta_duration))
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        format!(" {}{} 7d", format!("{}%", pct).red(), seven_day_eta)
    } else {
        String::new()
    };

    format!(
        "🔥\u{200B}{}{}{}{}",
        colored_rate, primary_eta, limit_str, seven_day_suffix
    )
}

/// Format ETA-only mode: time remaining before hitting limit
fn format_eta_only(burn_rate: &BurnRate, thresholds: &Thresholds) -> Option<String> {
    if burn_rate.is_at_limit {
        return Some("⏱\u{200B}limit".to_string());
    }

    let primary = if burn_rate.ratio >= thresholds.burn_rate_danger_ratio() {
        burn_rate
            .reset_in
            .map(|reset_in| {
                let eta_seconds = reset_in.num_seconds() as f64 / burn_rate.ratio;
                let eta_duration = Duration::seconds(eta_seconds as i64);
                format_eta(eta_duration)
                    .red()
                    .to_string()
            })
    } else if burn_rate.ratio >= thresholds.burn_rate_warning_ratio() {
        burn_rate
            .reset_in
            .map(|reset_in| {
                format_eta(reset_in)
                    .yellow()
                    .to_string()
            })
    } else {
        None
    };

    let limit_str = match burn_rate.critical_limit {
        LimitType::FiveHour => " 5h",
        LimitType::SevenDay => " 7d",
        LimitType::None => "",
    };

    let secondary = if burn_rate.seven_day_ratio >= thresholds.burn_rate_danger_ratio()
        && burn_rate.critical_limit != LimitType::SevenDay
    {
        burn_rate
            .seven_day_reset_in
            .map(|reset_in| {
                let eta_seconds = reset_in.num_seconds() as f64 / burn_rate.seven_day_ratio;
                let eta_duration = Duration::seconds(eta_seconds as i64);
                format!(" {} 7d", format_eta(eta_duration).red())
            })
    } else {
        None
    };

    if primary.is_none() && secondary.is_none() {
        return None;
    }

    Some(format!(
        "⏱\u{200B}{}{}{}",
        primary.unwrap_or_default(),
        limit_str,
        secondary.unwrap_or_default()
    ))
}

/// Format context information
pub fn format_context(context: Option<&ContextInfo>, thresholds: &Thresholds) -> String {
    match context {
        Some(info) => {
            let color = if info.percentage < thresholds.context_warning {
                info.percentage
                    .to_string()
                    .green()
                    .to_string()
            } else if info.percentage < thresholds.context_danger {
                info.percentage
                    .to_string()
                    .yellow()
                    .to_string()
            } else {
                info.percentage
                    .to_string()
                    .red()
                    .to_string()
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
    const BLOCKS: [char; 10] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█', '█'];
    let idx = ((value.fract() * 10.0) as usize).min(9);
    BLOCKS[idx]
}

/// Format 5h API usage
pub fn format_api_usage_5h(api_usage: Option<&ApiUsageData>) -> Option<String> {
    api_usage.map(|api| {
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
pub fn format_api_usage_7d(api_usage: Option<&ApiUsageData>) -> Option<String> {
    api_usage.map(|api| format!("7d:{}%", api.seven_day_percent as u32))
}

/// Format Sonnet 7d API usage
pub fn format_api_usage_sonnet(api_usage: Option<&ApiUsageData>) -> Option<String> {
    api_usage.map(|api| format!("S7d:{}%", api.seven_day_sonnet_percent as u32))
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

    formatted
        .green()
        .to_string()
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
        let result = format_api_usage_5h(Some(&data)).unwrap();
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
        let result = format_api_usage_5h(Some(&data)).unwrap();
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
            seven_day_ratio: 0.0,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        };
        let t = default_thresholds();
        let rate_api =
            format_burn_rate_component(&safe_burn, PlanType::Api, true, false, &t).unwrap();
        assert!(rate_api.contains("$1.50/h"));
        let rate_sub =
            format_burn_rate_component(&safe_burn, PlanType::Subscription, true, false, &t)
                .unwrap();
        assert!(rate_sub.contains("50%"));

        let warning_burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 0.9,
            seven_day_ratio: 0.0,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        };
        let warn =
            format_burn_rate_component(&warning_burn, PlanType::Api, true, false, &t).unwrap();
        assert!(warn.contains("$10.00/h"));
        assert!(warn.contains("5h"));

        let danger_burn = BurnRate {
            cost_per_hour: 15.0,
            ratio: 1.4,
            seven_day_ratio: 0.0,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        };
        let danger =
            format_burn_rate_component(&danger_burn, PlanType::Subscription, true, false, &t)
                .unwrap();
        assert!(danger.contains("140%"));
        assert!(danger.contains("5h"));
    }

    #[test]
    fn test_format_burn_rate_with_critical_7d() {
        let burn_with_7d = BurnRate {
            cost_per_hour: 5.0,
            ratio: 0.5,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        };
        let t = default_thresholds();
        let result =
            format_burn_rate_component(&burn_with_7d, PlanType::Subscription, true, false, &t)
                .unwrap();
        assert!(result.contains("50%"));
        assert!(result.contains("5h"));
        assert!(result.contains("110%"));
        assert!(result.contains("7d"));

        let burn_7d_critical = BurnRate {
            cost_per_hour: 5.0,
            ratio: 1.1,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::SevenDay,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        };
        let result =
            format_burn_rate_component(&burn_7d_critical, PlanType::Subscription, true, false, &t)
                .unwrap();
        assert!(result.contains("110%"));
        assert!(result.contains(" 7d"));
        assert_eq!(
            result
                .matches("7d")
                .count(),
            1
        );
    }

    #[test]
    fn test_format_burn_rate_both_over_100_percent() {
        let burn = BurnRate {
            cost_per_hour: 15.0,
            ratio: 1.4,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: None,
            seven_day_reset_in: None,
        };
        let t = default_thresholds();
        let result =
            format_burn_rate_component(&burn, PlanType::Subscription, true, false, &t).unwrap();
        assert_eq!(
            result
                .matches('%')
                .count(),
            2
        );
        assert!(result.contains(" 7d"));
        let stripped = strip_ansi_codes(&result);
        assert!(
            stripped.contains("110% 7d"),
            "expected '110% 7d' in '{}'",
            stripped
        );
        assert!(
            stripped.contains("140% 5h"),
            "expected '140% 5h' in '{}'",
            stripped
        );
    }

    #[test]
    fn test_format_burn_rate_at_limit() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 0.0,
            seven_day_ratio: 0.0,
            critical_limit: LimitType::FiveHour,
            is_at_limit: true,
            reset_in: Some(Duration::hours(2) + Duration::minutes(15)),
            seven_day_reset_in: None,
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        assert_eq!(result, "🔥limit");
    }

    #[test]
    fn test_format_burn_rate_eta_over_100_5h() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        let stripped = strip_ansi_codes(&result);
        // 3h / 1.4 = 2.14h → rounds to 2h
        assert!(
            stripped.contains("[⏱2h]"),
            "expected '[⏱2h]' in '{}'",
            stripped
        );
    }

    #[test]
    fn test_format_burn_rate_eta_over_100_7d() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.57,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::SevenDay,
            is_at_limit: false,
            reset_in: Some(Duration::hours(73)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        let stripped = strip_ansi_codes(&result);
        assert!(
            stripped.contains("[⏱1d22h]"),
            "expected '[⏱1d22h]' in '{}'",
            stripped
        );
    }

    #[test]
    fn test_format_burn_rate_eta_both_over_100() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.4,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        let stripped = strip_ansi_codes(&result);
        // 3h / 1.4 = 2.14h → rounds to 2h
        assert!(
            stripped.contains("[⏱2h]"),
            "expected primary ETA '[⏱2h]' in '{}'",
            stripped
        );
        // 100h / 1.1 = 90.9h = 3d19h
        assert!(
            stripped.contains("[⏱3d19h]"),
            "expected 7d ETA '[⏱3d19h]' in '{}'",
            stripped
        );
    }

    #[test]
    fn test_format_burn_rate_eta_minutes() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.5,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            // 178m / 1.5 = 118.67m → 118m (< 2h, shows minutes)
            reset_in: Some(Duration::minutes(178)),
            seven_day_reset_in: None,
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        let stripped = strip_ansi_codes(&result);
        assert!(
            stripped.contains("[⏱118m]"),
            "expected '[⏱118m]' in '{}'",
            stripped
        );
    }

    #[test]
    fn test_format_burn_rate_eta_under_100_no_show() {
        let burn = BurnRate {
            cost_per_hour: 5.0,
            ratio: 0.8,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        assert!(
            !result.contains("⏱"),
            "should not contain ETA when ratio < 1.0"
        );
    }

    #[test]
    fn test_format_burn_rate_eta_disabled() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, true, false);
        assert!(
            !result.contains("⏱"),
            "should not contain ETA when show_eta=false"
        );
    }

    // --- ETA-only mode tests ---

    #[test]
    fn test_eta_only_at_limit() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 0.0,
            seven_day_ratio: 0.0,
            critical_limit: LimitType::FiveHour,
            is_at_limit: true,
            reset_in: Some(Duration::hours(2)),
            seven_day_reset_in: None,
        };
        let result = verbose(&burn, PlanType::Subscription, false, true);
        assert!(result.contains("limit"), "expected 'limit' in '{}'", result);
        assert!(
            !result.contains("🔥"),
            "eta-only should not contain fire emoji"
        );
    }

    #[test]
    fn test_eta_only_over_100_5h() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, false, true);
        let stripped = strip_ansi_codes(&result);
        // 3h / 1.4 = 2.14h → rounds to 2h
        assert!(stripped.contains("2h"), "expected '2h' in '{}'", stripped);
        assert!(
            stripped.contains("5h"),
            "expected '5h' limit in '{}'",
            stripped
        );
        assert!(
            !result.contains("🔥"),
            "eta-only should not contain fire emoji"
        );
    }

    #[test]
    fn test_eta_only_warning_zone() {
        let burn = BurnRate {
            cost_per_hour: 5.0,
            ratio: 0.85,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(2) + Duration::minutes(30)),
            seven_day_reset_in: None,
        };
        let result = verbose(&burn, PlanType::Subscription, false, true);
        let stripped = strip_ansi_codes(&result);
        // Warning zone: ETA = reset_in = 2h30m → format_eta rounds to 3h
        assert!(
            stripped.contains("3h"),
            "expected '3h' (reset_in) in '{}'",
            stripped
        );
    }

    #[test]
    fn test_eta_only_under_80_no_show() {
        let burn = BurnRate {
            cost_per_hour: 5.0,
            ratio: 0.5,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = format_burn_rate_component(
            &burn,
            PlanType::Subscription,
            false,
            true,
            &default_thresholds(),
        );
        assert!(
            result.is_none(),
            "eta-only should return None when ratio < 0.8"
        );
    }

    #[test]
    fn test_eta_only_both_over_100() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.4,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
        };
        let result = verbose(&burn, PlanType::Subscription, false, true);
        let stripped = strip_ansi_codes(&result);
        assert!(
            stripped.contains("5h"),
            "expected '5h' limit in '{}'",
            stripped
        );
        assert!(
            stripped.contains("7d"),
            "expected '7d' secondary in '{}'",
            stripped
        );
    }

    #[test]
    fn test_both_false_returns_none() {
        let burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            is_at_limit: false,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: None,
        };
        assert!(
            format_burn_rate_component(
                &burn,
                PlanType::Subscription,
                false,
                false,
                &default_thresholds()
            )
            .is_none()
        );
    }

    // --- format_eta unit tests ---

    #[test]
    fn test_format_eta_minutes_only() {
        assert_eq!(format_eta(Duration::minutes(87)), "87m");
        assert_eq!(format_eta(Duration::minutes(119)), "119m");
    }

    #[test]
    fn test_format_eta_days_hours() {
        assert_eq!(format_eta(Duration::hours(25)), "1d1h");
        assert_eq!(format_eta(Duration::hours(73)), "3d1h");
        assert_eq!(format_eta(Duration::hours(48)), "2d");
    }

    #[test]
    fn test_format_eta_hours_only() {
        assert_eq!(format_eta(Duration::hours(2)), "2h");
        assert_eq!(format_eta(Duration::hours(14)), "14h");
        assert_eq!(format_eta(Duration::hours(23)), "23h");
    }

    fn default_thresholds() -> Thresholds {
        Thresholds::default()
    }

    fn verbose(
        burn_rate: &BurnRate,
        plan_type: PlanType,
        show_rate: bool,
        show_eta: bool,
    ) -> String {
        let result = format_burn_rate_component(
            burn_rate,
            plan_type,
            show_rate,
            show_eta,
            &default_thresholds(),
        )
        .unwrap_or_default();
        eprintln!("  {}", result);
        result
    }

    fn strip_ansi_codes(s: &str) -> String {
        let mut result = String::new();
        let mut chars = s
            .chars()
            .peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }
}

use crate::config::{StatusElement, Thresholds};
use crate::types::{ApiUsageData, Block, BurnRate, ContextInfo, LimitType, PlanType};
use chrono::{Duration, Utc};
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
    let remaining_hours = if let Some(reset_time) = api_usage
        .and_then(|a| {
            a.five_hour
                .as_ref()
        })
        .and_then(|w| w.resets_at)
    {
        (reset_time - now).num_seconds() as f64 / 3600.0
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
    let reset_time = api_usage
        .and_then(|a| {
            a.seven_day
                .as_ref()
        })
        .and_then(|w| w.resets_at)?;
    let remaining_hours = (reset_time - now).num_seconds() as f64 / 3600.0;
    Some(format_days_remaining(remaining_hours))
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

/// Compute ETA as reset_in scaled down by ratio (time to hit limit at current burn rate)
fn scaled_eta(reset_in: Duration, ratio: f64) -> Duration {
    Duration::seconds((reset_in.num_seconds() as f64 / ratio) as i64)
}

/// Color a string red/yellow/green based on value vs warning and danger thresholds
fn colorize_by_threshold(s: &str, value: f64, warning: f64, danger: f64) -> String {
    if value >= danger {
        s.red()
            .to_string()
    } else if value >= warning {
        s.yellow()
            .to_string()
    } else {
        s.green()
            .to_string()
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

    let colored_rate = colorize_by_threshold(
        &rate_str,
        burn_rate.ratio,
        thresholds.burn_rate_warning_ratio(),
        thresholds.burn_rate_danger_ratio(),
    );

    let primary_eta = if show_eta && burn_rate.ratio >= thresholds.burn_rate_danger_ratio() {
        burn_rate
            .reset_in
            .map(|reset_in| format!("[⏱{}]", format_eta(scaled_eta(reset_in, burn_rate.ratio))))
            .unwrap_or_default()
    } else {
        String::new()
    };

    let limit_str = burn_rate
        .critical_limit
        .label();

    let seven_day_suffix = if burn_rate.seven_day_ratio >= thresholds.burn_rate_danger_ratio()
        && burn_rate.critical_limit != LimitType::SevenDay
    {
        let pct = (burn_rate.seven_day_ratio * 100.0).round() as i32;
        let seven_day_eta = if show_eta {
            burn_rate
                .seven_day_reset_in
                .map(|reset_in| {
                    format!(
                        "[⏱{}]",
                        format_eta(scaled_eta(reset_in, burn_rate.seven_day_ratio))
                    )
                })
                .unwrap_or_default()
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
                format_eta(scaled_eta(reset_in, burn_rate.ratio))
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

    let limit_str = burn_rate
        .critical_limit
        .label();

    let secondary = if burn_rate.seven_day_ratio >= thresholds.burn_rate_danger_ratio()
        && burn_rate.critical_limit != LimitType::SevenDay
    {
        burn_rate
            .seven_day_reset_in
            .map(|reset_in| {
                format!(
                    " {} 7d",
                    format_eta(scaled_eta(reset_in, burn_rate.seven_day_ratio)).red()
                )
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
            let pct_str = info
                .percentage
                .to_string();
            let color = colorize_by_threshold(
                &pct_str,
                info.percentage as f64,
                thresholds.context_warning as f64,
                thresholds.context_danger as f64,
            );
            format!("{}k({}%)", info.tokens / 1000, color)
        }
        None => "N/A".to_string(),
    }
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
    let w = api_usage.and_then(|a| {
        a.five_hour
            .as_ref()
    })?;
    let pct_int = w.percent as u32;
    let block = decimal_to_block(w.percent);
    Some(if block == ' ' {
        format!("5h:{}%", pct_int)
    } else {
        format!("5h:{}%{}", pct_int, block)
    })
}

/// Format 7d API usage
pub fn format_api_usage_7d(api_usage: Option<&ApiUsageData>) -> Option<String> {
    api_usage
        .and_then(|a| {
            a.seven_day
                .as_ref()
        })
        .map(|w| format!("7d:{}%", w.percent as u32))
}

/// Format Sonnet 7d API usage
pub fn format_api_usage_sonnet(api_usage: Option<&ApiUsageData>) -> Option<String> {
    api_usage
        .and_then(|a| a.seven_day_sonnet)
        .map(|pct| format!("S7d:{}%", pct as u32))
}

/// Format the API metrics group; manages the 📊 prefix and enabled-element filtering.
/// Returns None when no element has data to show.
pub fn format_api_metrics_group(
    enabled: &[StatusElement],
    error_label: Option<&'static str>,
    api_usage: Option<&ApiUsageData>,
) -> Option<String> {
    if let Some(label) = error_label {
        return Some(format!("📊({})", label));
    }

    let mut api_parts: Vec<String> = Vec::new();

    if enabled.contains(&StatusElement::ApiMetrics5h)
        && let Some(text) = format_api_usage_5h(api_usage)
    {
        api_parts.push(format!("📊{}", text));
    }
    if enabled.contains(&StatusElement::ApiMetrics7d)
        && let Some(text) = format_api_usage_7d(api_usage)
    {
        if api_parts.is_empty() {
            api_parts.push(format!("📊{}", text));
        } else {
            api_parts.push(text);
        }
    }
    if enabled.contains(&StatusElement::ApiMetricsSonnet)
        && let Some(text) = format_api_usage_sonnet(api_usage)
    {
        if api_parts.is_empty() {
            api_parts.push(format!("📊{}", text));
        } else {
            api_parts.push(text);
        }
    }

    if api_parts.is_empty() {
        None
    } else {
        Some(api_parts.join(" "))
    }
}

pub fn strip_emojis(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let cp = *c as u32;
            // Strip: Miscellaneous/supplemental emoji blocks, ZWSP separator, and ⏱ (U+23F1)
            // which falls outside the main emoji range but is emitted by burn-rate and ETA formatters
            !(0x1F300..=0x1FAFF).contains(&cp) && cp != 0x200B && cp != 0x23F1
        })
        .collect()
}

/// Format directory path with home replacement and color
pub fn format_directory(path: &str) -> String {
    let home = crate::paths::home_dir()
        .ok()
        .and_then(|p| {
            p.to_str()
                .map(String::from)
        });

    let formatted = match home {
        Some(h) if path.starts_with(&h) => path.replacen(&h, "~", 1),
        _ => path.to_string(),
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
        use crate::types::UsageWindow;
        let data = ApiUsageData {
            five_hour: Some(UsageWindow {
                percent: 37.0,
                resets_at: None,
            }),
            seven_day: None,
            seven_day_sonnet: None,
        };
        let result = format_api_usage_5h(Some(&data)).unwrap();
        assert_eq!(result, "5h:37%");
        assert!(!result.ends_with(' '));
    }

    #[test]
    fn test_format_api_usage_5h_with_block() {
        use crate::types::UsageWindow;
        let data = ApiUsageData {
            five_hour: Some(UsageWindow {
                percent: 37.5,
                resets_at: None,
            }),
            seven_day: None,
            seven_day_sonnet: None,
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
            critical_limit: LimitType::FiveHour,
            ..Default::default()
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
            critical_limit: LimitType::FiveHour,
            ..Default::default()
        };
        let warn =
            format_burn_rate_component(&warning_burn, PlanType::Api, true, false, &t).unwrap();
        assert!(warn.contains("$10.00/h"));
        assert!(warn.contains("5h"));

        let danger_burn = BurnRate {
            cost_per_hour: 15.0,
            ratio: 1.4,
            critical_limit: LimitType::FiveHour,
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            critical_limit: LimitType::FiveHour,
            is_at_limit: true,
            reset_in: Some(Duration::hours(2) + Duration::minutes(15)),
            ..Default::default()
        };
        let result = verbose(&burn, PlanType::Subscription, true, true);
        assert_eq!(result, "🔥limit");
    }

    #[test]
    fn test_format_burn_rate_eta_over_100_5h() {
        let burn = BurnRate {
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 1.57,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::SevenDay,
            reset_in: Some(Duration::hours(73)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 1.4,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 1.5,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            // 178m / 1.5 = 118.67m → 118m (< 2h, shows minutes)
            reset_in: Some(Duration::minutes(178)),
            ..Default::default()
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
            ratio: 0.8,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            critical_limit: LimitType::FiveHour,
            is_at_limit: true,
            reset_in: Some(Duration::hours(2)),
            ..Default::default()
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
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 0.85,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(2) + Duration::minutes(30)),
            ..Default::default()
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
            ratio: 0.5,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 1.4,
            seven_day_ratio: 1.1,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            seven_day_reset_in: Some(Duration::hours(100)),
            ..Default::default()
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
            ratio: 1.4,
            seven_day_ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            reset_in: Some(Duration::hours(3)),
            ..Default::default()
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

    #[test]
    fn test_strip_emojis() {
        assert_eq!(strip_emojis("🤖Claude"), "Claude");
        assert_eq!(strip_emojis("📊5h:37%▅"), "5h:37%▅");
        assert_eq!(strip_emojis("🔥\u{200B}50% 5h"), "50% 5h");
        assert_eq!(strip_emojis("no emojis here"), "no emojis here");
        // ⏱ (U+23F1) is outside the main emoji range but is emitted by ETA formatters
        assert_eq!(strip_emojis("⏱\u{200B}2h 5h"), "2h 5h");
        // │ separator (U+2502) must not be stripped
        assert_eq!(strip_emojis("a │ b"), "a │ b");
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

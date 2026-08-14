use crate::config::{StatusElement, Thresholds};
use crate::types::{ActiveBlock, ApiUsageData, BurnRate, ContextInfo, LimitType, PlanType};
use chrono::{Duration, Utc};
use owo_colors::OwoColorize;

/// Format block cost
pub fn format_block_info(block: Option<&ActiveBlock>) -> String {
    match block {
        Some(b) => format_currency(b.cost_usd),
        None => "No block".to_string(),
    }
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
    block: Option<&ActiveBlock>,
    api_usage: Option<&ApiUsageData>,
    plan_type: PlanType,
) -> Option<String> {
    if matches!(plan_type, PlanType::Api) {
        return None;
    }

    let now = Utc::now();
    // The API reset time is authoritative and needs no local block; requiring one hid
    // this element whenever the transcript scan found nothing.
    let remaining_hours = match api_usage
        .and_then(|a| {
            a.five_hour
                .as_ref()
        })
        .and_then(|w| w.resets_at)
    {
        Some(reset_time) => (reset_time - now).num_seconds() as f64 / 3600.0,
        None => block?.hours_remaining,
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

    let mut hours = remaining_hours.floor() as i64;
    // Rounding the remainder can reach a full 60, which would render as "2h60m"
    let mut mins = ((remaining_hours - hours as f64) * 60.0).round() as i64;
    if mins == 60 {
        hours += 1;
        mins = 0;
    }
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
    } else if hours > 0 {
        format!("📅{}h", hours)
    } else {
        // Under an hour, "0h" reads as expired rather than imminent.
        format!("📅{}m", (remaining_hours * 60.0).ceil() as i64)
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
        let mut days = (total_hours / 24.0).floor() as i64;
        // Rounding the remainder can reach a full 24, which would render as "1d24h"
        let mut hours = (total_hours - days as f64 * 24.0).round() as i64;
        if hours == 24 {
            days += 1;
            hours = 0;
        }
        if hours > 0 {
            format!("{}d{}h", days, hours)
        } else {
            format!("{}d", days)
        }
    }
}

/// Compute ETA as reset_in scaled down by ratio (time to hit limit at current burn rate)
fn scaled_eta(reset_in: Duration, ratio: f64) -> Duration {
    // A zero or NaN ratio divides to infinity, which saturates to i64::MAX seconds;
    // chrono's Duration::seconds panics on that rather than saturating.
    if ratio.is_nan() || ratio <= 0.0 {
        return reset_in;
    }
    Duration::try_seconds((reset_in.num_seconds() as f64 / ratio) as i64).unwrap_or(reset_in)
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

/// Controls what the burn rate component renders; the (false, false) dead combo is unrepresentable
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BurnRateDisplay {
    Rate,
    RateWithEta,
    EtaOnly,
}

impl BurnRateDisplay {
    /// Map (has_rate, has_eta) enabled-element flags to the display mode, or None if both off
    pub fn from_elements(has_rate: bool, has_eta: bool) -> Option<Self> {
        match (has_rate, has_eta) {
            (true, true) => Some(Self::RateWithEta),
            (true, false) => Some(Self::Rate),
            (false, true) => Some(Self::EtaOnly),
            (false, false) => None,
        }
    }
}

/// Unified entry point for all burn rate display modes
pub fn format_burn_rate_component(
    burn_rate: &BurnRate,
    plan_type: PlanType,
    display: BurnRateDisplay,
    thresholds: &Thresholds,
) -> Option<String> {
    let is_subscription = matches!(plan_type, PlanType::Subscription);

    // The show threshold gates visibility, which is what its name and the menu promise.
    // A window already at its limit reports a zero ratio, so it is exempt: that is
    // precisely when the element must not disappear. Cost-based display (Api plan) has
    // no ratio to compare against and is always shown.
    if is_subscription
        && !burn_rate.is_at_limit
        && burn_rate.ratio < thresholds.burn_rate_show_ratio()
        && burn_rate.seven_day_ratio < thresholds.burn_rate_show_ratio()
    {
        return None;
    }

    match display {
        BurnRateDisplay::Rate => Some(format_rate_display(burn_rate, plan_type, false, thresholds)),
        BurnRateDisplay::RateWithEta => Some(format_rate_display(
            burn_rate,
            plan_type,
            is_subscription,
            thresholds,
        )),
        BurnRateDisplay::EtaOnly => {
            if is_subscription {
                format_eta_only(burn_rate, thresholds)
            } else {
                None
            }
        }
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
pub fn format_context(info: &ContextInfo, thresholds: &Thresholds) -> String {
    let pct_str = format!("{}", info.percentage);
    let color = colorize_by_threshold(
        &pct_str,
        info.percentage as f64,
        thresholds.context_warning as f64,
        thresholds.context_danger as f64,
    );
    format!("{}k({}%)", info.tokens / 1000, color)
}

/// Format amount as fixed two-decimal USD string
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
fn format_api_usage_5h(api_usage: Option<&ApiUsageData>) -> Option<String> {
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
fn format_api_usage_7d(api_usage: Option<&ApiUsageData>) -> Option<String> {
    api_usage
        .and_then(|a| {
            a.seven_day
                .as_ref()
        })
        .map(|w| format!("7d:{}%", w.percent as u32))
}

/// Format the per-model 7d API usage, one entry per model bucket the server reports.
/// Labels use the model's initial; two buckets sharing one take their full names instead.
fn format_api_usage_model_7d(api_usage: Option<&ApiUsageData>) -> Vec<String> {
    let Some(windows) = api_usage.map(|a| &a.model_scoped) else {
        return Vec::new();
    };
    windows
        .iter()
        .filter_map(|w| {
            let initial = w
                .display_name
                .chars()
                .next()?;
            let shared = windows
                .iter()
                .filter(|o| {
                    o.display_name
                        .starts_with(initial)
                })
                .count()
                > 1;
            let label = if shared {
                w.display_name
                    .clone()
            } else {
                initial.to_string()
            };
            Some(format!("{}7d:{}%", label, w.percent as u32))
        })
        .collect()
}

/// Format the API metrics group; manages the 📊 prefix and enabled-element filtering.
/// Returns None when no element has data to show.
pub fn format_api_metrics_group(
    enabled: &[StatusElement],
    error_label: Option<&'static str>,
    api_usage: Option<&ApiUsageData>,
) -> Option<String> {
    let mut api_parts: Vec<String> = Vec::new();

    fn push_part(parts: &mut Vec<String>, text: String) {
        if parts.is_empty() {
            parts.push(format!("📊{}", text));
        } else {
            parts.push(text);
        }
    }

    if enabled.contains(&StatusElement::ApiMetrics5h)
        && let Some(text) = format_api_usage_5h(api_usage)
    {
        push_part(&mut api_parts, text);
    }
    if enabled.contains(&StatusElement::ApiMetrics7d)
        && let Some(text) = format_api_usage_7d(api_usage)
    {
        push_part(&mut api_parts, text);
    }
    if enabled.contains(&StatusElement::ApiMetricsModel7d) {
        for text in format_api_usage_model_7d(api_usage) {
            push_part(&mut api_parts, text);
        }
    }

    if !api_parts.is_empty() {
        return Some(api_parts.join(" "));
    }
    // The fetch may have failed while stdin still carried usable windows; the error
    // label belongs here only when nothing else could be rendered.
    error_label.map(|label| format!("📊({})", label))
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

    #[test]
    fn test_format_hours_remaining_carries_rounded_minutes() {
        assert!(
            format_hours_remaining(2.99999).ends_with("3h"),
            "got {}",
            format_hours_remaining(2.99999)
        );
    }

    #[test]
    fn test_format_eta_carries_rounded_hours() {
        assert_eq!(format_eta(Duration::minutes(47 * 60 + 59)), "2d");
    }

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
            model_scoped: Vec::new(),
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
            model_scoped: Vec::new(),
        };
        let result = format_api_usage_5h(Some(&data)).unwrap();
        assert_eq!(result, "5h:37%▅");
    }

    #[test]
    fn test_format_currency() {
        assert_eq!(format_currency(12.345), "$12.35");
        assert_eq!(format_currency(0.0), "$0.00");
    }

    fn scoped_usage(models: &[(&str, f64)]) -> ApiUsageData {
        use crate::types::{ScopedUsageWindow, UsageWindow};
        ApiUsageData {
            five_hour: Some(UsageWindow {
                percent: 17.0,
                resets_at: None,
            }),
            seven_day: Some(UsageWindow {
                percent: 45.0,
                resets_at: None,
            }),
            model_scoped: models
                .iter()
                .map(|(name, percent)| ScopedUsageWindow {
                    display_name: (*name).to_string(),
                    percent: *percent,
                })
                .collect(),
        }
    }

    #[test]
    fn test_api_group_renders_model_scoped_after_windows() {
        let usage = scoped_usage(&[("Fable", 26.0)]);
        let enabled = vec![
            StatusElement::ApiMetrics5h,
            StatusElement::ApiMetrics7d,
            StatusElement::ApiMetricsModel7d,
        ];
        let result = format_api_metrics_group(&enabled, None, Some(&usage)).unwrap();
        assert_eq!(strip_emojis(&result), "5h:17% 7d:45% F7d:26%");
    }

    #[test]
    fn test_model_scoped_shared_initial_uses_full_name() {
        let usage = scoped_usage(&[("Fable", 26.0), ("Fathom", 4.0)]);
        assert_eq!(
            format_api_usage_model_7d(Some(&usage)),
            vec!["Fable7d:26%", "Fathom7d:4%"]
        );
    }

    /// A failed fetch must not discard windows stdin already supplied.
    #[test]
    fn test_api_group_prefers_real_data_over_error_label() {
        use crate::types::UsageWindow;
        let usage = ApiUsageData {
            five_hour: Some(UsageWindow {
                percent: 62.0,
                resets_at: None,
            }),
            seven_day: None,
            model_scoped: Vec::new(),
        };
        let enabled = vec![StatusElement::ApiMetrics5h];

        let with_data =
            format_api_metrics_group(&enabled, Some("api error"), Some(&usage)).unwrap();
        assert!(with_data.contains("62"), "got {with_data}");
        assert!(!with_data.contains("api error"), "got {with_data}");

        // Nothing to render: the error is the only thing left to say.
        let without = format_api_metrics_group(&enabled, Some("api error"), None).unwrap();
        assert_eq!(without, "📊(api error)");
    }

    /// The 5h countdown is driven by the API reset time, which needs no local block.
    #[test]
    fn test_time_remaining_5h_without_local_block() {
        use crate::types::UsageWindow;
        let usage = ApiUsageData {
            five_hour: Some(UsageWindow {
                percent: 10.0,
                resets_at: Some(Utc::now() + Duration::hours(3)),
            }),
            seven_day: None,
            model_scoped: Vec::new(),
        };
        assert!(format_time_remaining_5h(None, Some(&usage), PlanType::Subscription).is_some());
        assert!(format_time_remaining_5h(None, None, PlanType::Subscription).is_none());
    }

    #[test]
    fn test_days_remaining_under_an_hour_shows_minutes() {
        assert_eq!(format_days_remaining(0.5), "📅30m");
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
            format_burn_rate_component(&safe_burn, PlanType::Api, BurnRateDisplay::Rate, &t)
                .unwrap();
        assert!(rate_api.contains("$1.50/h"));
        // A subscription burning at half the safe rate is below the show threshold and
        // renders nothing; the Api plan has no ratio to gate on and still shows cost.
        assert!(
            format_burn_rate_component(
                &safe_burn,
                PlanType::Subscription,
                BurnRateDisplay::Rate,
                &t,
            )
            .is_none()
        );

        let shown_burn = BurnRate {
            ratio: 0.85,
            critical_limit: LimitType::FiveHour,
            ..Default::default()
        };
        let rate_sub = format_burn_rate_component(
            &shown_burn,
            PlanType::Subscription,
            BurnRateDisplay::Rate,
            &t,
        )
        .unwrap();
        assert!(rate_sub.contains("85%"));

        let warning_burn = BurnRate {
            cost_per_hour: 10.0,
            ratio: 0.9,
            critical_limit: LimitType::FiveHour,
            ..Default::default()
        };
        let warn =
            format_burn_rate_component(&warning_burn, PlanType::Api, BurnRateDisplay::Rate, &t)
                .unwrap();
        assert!(warn.contains("$10.00/h"));
        assert!(warn.contains("5h"));

        let danger_burn = BurnRate {
            cost_per_hour: 15.0,
            ratio: 1.4,
            critical_limit: LimitType::FiveHour,
            ..Default::default()
        };
        let danger = format_burn_rate_component(
            &danger_burn,
            PlanType::Subscription,
            BurnRateDisplay::Rate,
            &t,
        )
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
        let result = format_burn_rate_component(
            &burn_with_7d,
            PlanType::Subscription,
            BurnRateDisplay::Rate,
            &t,
        )
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
        let result = format_burn_rate_component(
            &burn_7d_critical,
            PlanType::Subscription,
            BurnRateDisplay::Rate,
            &t,
        )
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
            format_burn_rate_component(&burn, PlanType::Subscription, BurnRateDisplay::Rate, &t)
                .unwrap();
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
            BurnRateDisplay::EtaOnly,
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
        assert!(BurnRateDisplay::from_elements(false, false).is_none());
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
        let display = BurnRateDisplay::from_elements(show_rate, show_eta)
            .expect("invalid (false, false) combo in test");
        let result =
            format_burn_rate_component(burn_rate, plan_type, display, &default_thresholds())
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

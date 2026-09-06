pub mod burn_rate;

use crate::config::{StatusElement, Thresholds};
use crate::types::{ActiveBlock, ApiUsageData, ContextInfo, PlanType};
use chrono::Utc;
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
    let remaining_hours = match api_usage.and_then(ApiUsageData::five_hour_reset) {
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
    let reset_time = api_usage.and_then(ApiUsageData::seven_day_reset)?;
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

/// Band a value falls in relative to the warning and danger thresholds. It decides
/// colour only; what each band renders is the caller's, and they do differ.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tier {
    Danger,
    Warning,
    Safe,
}

impl Tier {
    fn of(value: f64, warning: f64, danger: f64) -> Self {
        if value >= danger {
            Self::Danger
        } else if value >= warning {
            Self::Warning
        } else {
            Self::Safe
        }
    }

    fn paint(self, s: &str) -> String {
        match self {
            Self::Danger => s
                .red()
                .to_string(),
            Self::Warning => s
                .yellow()
                .to_string(),
            Self::Safe => s
                .green()
                .to_string(),
        }
    }
}

/// Color a string red/yellow/green based on value vs warning and danger thresholds
fn colorize_by_threshold(s: &str, value: f64, warning: f64, danger: f64) -> String {
    Tier::of(value, warning, danger).paint(s)
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
fn format_currency(amount: f64) -> String {
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
    fn test_format_hours_remaining_carries_rounded_minutes() {
        assert!(
            format_hours_remaining(2.99999).ends_with("3h"),
            "got {}",
            format_hours_remaining(2.99999)
        );
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
        use crate::testutil::ApiUsageDataBuilder;
        let data = ApiUsageDataBuilder::new()
            .five_hour(37.0)
            .build();
        let result = format_api_usage_5h(Some(&data)).unwrap();
        assert_eq!(result, "5h:37%");
        assert!(!result.ends_with(' '));
    }

    #[test]
    fn test_format_api_usage_5h_with_block() {
        use crate::testutil::ApiUsageDataBuilder;
        let data = ApiUsageDataBuilder::new()
            .five_hour(37.5)
            .build();
        let result = format_api_usage_5h(Some(&data)).unwrap();
        assert_eq!(result, "5h:37%▅");
    }

    #[test]
    fn test_format_currency() {
        assert_eq!(format_currency(12.345), "$12.35");
        assert_eq!(format_currency(0.0), "$0.00");
    }

    #[test]
    fn test_api_group_renders_model_scoped_after_windows() {
        let usage = crate::testutil::scoped_usage(&[("Fable", 26.0)]);
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
        let usage = crate::testutil::scoped_usage(&[("Fable", 26.0), ("Fathom", 4.0)]);
        assert_eq!(
            format_api_usage_model_7d(Some(&usage)),
            vec!["Fable7d:26%", "Fathom7d:4%"]
        );
    }

    /// A failed fetch must not discard windows stdin already supplied.
    #[test]
    fn test_api_group_prefers_real_data_over_error_label() {
        use crate::testutil::ApiUsageDataBuilder;
        let usage = ApiUsageDataBuilder::new()
            .five_hour(62.0)
            .build();
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
        use crate::testutil::ApiUsageDataBuilder;
        use chrono::Duration;
        let usage = ApiUsageDataBuilder::new()
            .five_hour_resetting(10.0, Utc::now() + Duration::hours(3))
            .build();
        assert!(format_time_remaining_5h(None, Some(&usage), PlanType::Subscription).is_some());
        assert!(format_time_remaining_5h(None, None, PlanType::Subscription).is_none());
    }

    #[test]
    fn test_days_remaining_under_an_hour_shows_minutes() {
        assert_eq!(format_days_remaining(0.5), "📅30m");
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
}

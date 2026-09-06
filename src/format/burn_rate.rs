use super::{Tier, colorize_by_threshold, format_currency};
use crate::config::Thresholds;
use crate::types::{BurnRate, LimitType, PlanType};
use chrono::Duration;
use owo_colors::OwoColorize;

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

/// The bracketed ETA a rate display carries, empty when it is not shown or no reset
/// time is known.
fn eta_bracket(reset_in: Option<Duration>, ratio: f64, show: bool) -> String {
    if !show {
        return String::new();
    }
    reset_in
        .map(|reset_in| format!("[⏱{}]", format_eta(scaled_eta(reset_in, ratio))))
        .unwrap_or_default()
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

    let primary_eta = eta_bracket(
        burn_rate.reset_in,
        burn_rate.ratio,
        show_eta && burn_rate.ratio >= thresholds.burn_rate_danger_ratio(),
    );

    let limit_str = burn_rate
        .critical_limit
        .label();

    let seven_day_suffix = if burn_rate.seven_day_ratio >= thresholds.burn_rate_danger_ratio()
        && burn_rate.critical_limit != LimitType::SevenDay
    {
        let pct = (burn_rate.seven_day_ratio * 100.0).round() as i32;
        let seven_day_eta = eta_bracket(
            burn_rate.seven_day_reset_in,
            burn_rate.seven_day_ratio,
            show_eta,
        );
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

    // Only the danger band scales the ETA by the burn ratio; the warning band shows the
    // plain time to reset, which is what test_eta_only_table's warning-zone case guards.
    let tier = Tier::of(
        burn_rate.ratio,
        thresholds.burn_rate_warning_ratio(),
        thresholds.burn_rate_danger_ratio(),
    );
    let primary = match tier {
        Tier::Danger => burn_rate
            .reset_in
            .map(|reset_in| tier.paint(&format_eta(scaled_eta(reset_in, burn_rate.ratio)))),
        Tier::Warning => burn_rate
            .reset_in
            .map(|reset_in| tier.paint(&format_eta(reset_in))),
        Tier::Safe => None,
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
    fn test_format_eta_carries_rounded_hours() {
        assert_eq!(format_eta(Duration::minutes(47 * 60 + 59)), "2d");
    }

    #[test]
    fn test_format_burn_rate() {
        let safe_burn = BurnRate {
            cost_per_hour: 1.5,
            ratio: 0.5,
            critical_limit: LimitType::FiveHour,
            ..Default::default()
        };
        let t = Thresholds::default();
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
        let t = Thresholds::default();
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
        let t = Thresholds::default();
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
        let result = rendered(&burn, PlanType::Subscription, true, true);
        assert_eq!(result, "🔥limit");
    }

    /// One case per near-identical rate+ETA test the table replaces; ratio and reset
    /// times vary, but each still needs the rendered ETA to be present verbatim.
    struct EtaCase {
        ratio: f64,
        seven_day_ratio: f64,
        critical_limit: LimitType,
        reset_in: Duration,
        seven_day_reset_in: Option<Duration>,
        expect: &'static [&'static str],
    }

    #[test]
    fn test_format_burn_rate_eta_table() {
        let cases = [
            EtaCase {
                // 3h / 1.4 = 2.14h → rounds to 2h
                ratio: 1.4,
                seven_day_ratio: 0.5,
                critical_limit: LimitType::FiveHour,
                reset_in: Duration::hours(3),
                seven_day_reset_in: Some(Duration::hours(100)),
                expect: &["[⏱2h]"],
            },
            EtaCase {
                // 73h / 1.57 = 46.5h = 1d22h
                ratio: 1.57,
                seven_day_ratio: 0.5,
                critical_limit: LimitType::SevenDay,
                reset_in: Duration::hours(73),
                seven_day_reset_in: Some(Duration::hours(100)),
                expect: &["[⏱1d22h]"],
            },
            EtaCase {
                // 3h / 1.4 = 2.14h → 2h; 100h / 1.1 = 90.9h = 3d19h
                ratio: 1.4,
                seven_day_ratio: 1.1,
                critical_limit: LimitType::FiveHour,
                reset_in: Duration::hours(3),
                seven_day_reset_in: Some(Duration::hours(100)),
                expect: &["[⏱2h]", "[⏱3d19h]"],
            },
            EtaCase {
                // 178m / 1.5 = 118.67m → 118m (< 2h, shows minutes)
                ratio: 1.5,
                seven_day_ratio: 0.5,
                critical_limit: LimitType::FiveHour,
                reset_in: Duration::minutes(178),
                seven_day_reset_in: None,
                expect: &["[⏱118m]"],
            },
        ];

        for case in cases {
            let burn = BurnRate {
                ratio: case.ratio,
                seven_day_ratio: case.seven_day_ratio,
                critical_limit: case.critical_limit,
                reset_in: Some(case.reset_in),
                seven_day_reset_in: case.seven_day_reset_in,
                ..Default::default()
            };
            let result = rendered(&burn, PlanType::Subscription, true, true);
            let stripped = strip_ansi_codes(&result);
            for substr in case.expect {
                assert!(
                    stripped.contains(substr),
                    "expected '{substr}' in '{stripped}'"
                );
            }
        }
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
        let result = rendered(&burn, PlanType::Subscription, true, true);
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
        let result = rendered(&burn, PlanType::Subscription, true, false);
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
        let result = rendered(&burn, PlanType::Subscription, false, true);
        assert!(result.contains("limit"), "expected 'limit' in '{}'", result);
        assert!(
            !result.contains("🔥"),
            "eta-only should not contain fire emoji"
        );
    }

    #[test]
    fn test_eta_only_table() {
        let cases = [
            EtaCase {
                // 3h / 1.4 = 2.14h → rounds to 2h
                ratio: 1.4,
                seven_day_ratio: 0.5,
                critical_limit: LimitType::FiveHour,
                reset_in: Duration::hours(3),
                seven_day_reset_in: Some(Duration::hours(100)),
                expect: &["2h", "5h"],
            },
            EtaCase {
                // Warning zone: ETA = reset_in = 2h30m → format_eta rounds to 3h
                ratio: 0.85,
                seven_day_ratio: 0.0,
                critical_limit: LimitType::FiveHour,
                reset_in: Duration::hours(2) + Duration::minutes(30),
                seven_day_reset_in: None,
                expect: &["3h"],
            },
            EtaCase {
                ratio: 1.4,
                seven_day_ratio: 1.1,
                critical_limit: LimitType::FiveHour,
                reset_in: Duration::hours(3),
                seven_day_reset_in: Some(Duration::hours(100)),
                expect: &["5h", "7d"],
            },
        ];

        for case in cases {
            let burn = BurnRate {
                ratio: case.ratio,
                seven_day_ratio: case.seven_day_ratio,
                critical_limit: case.critical_limit,
                reset_in: Some(case.reset_in),
                seven_day_reset_in: case.seven_day_reset_in,
                ..Default::default()
            };
            let result = rendered(&burn, PlanType::Subscription, false, true);
            let stripped = strip_ansi_codes(&result);
            for substr in case.expect {
                assert!(
                    stripped.contains(substr),
                    "expected '{substr}' in '{stripped}'"
                );
            }
            assert!(
                !result.contains("🔥"),
                "eta-only should not contain fire emoji"
            );
        }
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
            &Thresholds::default(),
        );
        assert!(
            result.is_none(),
            "eta-only should return None when ratio < 0.8"
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

    fn rendered(
        burn_rate: &BurnRate,
        plan_type: PlanType,
        show_rate: bool,
        show_eta: bool,
    ) -> String {
        let display = BurnRateDisplay::from_elements(show_rate, show_eta)
            .expect("invalid (false, false) combo in test");
        format_burn_rate_component(burn_rate, plan_type, display, &Thresholds::default())
            .unwrap_or_default()
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

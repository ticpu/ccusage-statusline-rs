//! Recent consumption pace of a usage window, measured from the utilization steps the
//! usage endpoint reported.
//!
//! The ⏱ ETA divides what is left by this pace rather than by the window's mean rate:
//! a 7-day mean includes nights and idle days, and during active use it put the limit
//! four times further away than it was.
//!
//! - Steps come only from fetches the usage cache already makes, never from a poll of
//!   their own: the endpoint is heavily rate limited. Stdin readings are not recorded;
//!   they come from a separately anchored limit system, and interleaving the two series
//!   would read as steps that never happened.
//! - Utilization arrives in whole percents, so a step's time is the fetch that first saw
//!   the new value. The first value seen after a reset, a reboot or an account change is
//!   not a step: when it was reached is unknown, and anchoring on it overstates the pace.
//! - The pace spans step times inside the lookback, never `now` or the lookback's edge,
//!   so a step still in progress does not bias it.
//! - The last measured pace is kept, so after a pause the ETA resumes from the pace last
//!   seen rather than the mean. It lives in the usage cache under XDG_RUNTIME_DIR and
//!   does not survive a reboot.
//! - A history belongs to one window: a moved reset time starts a new one.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::rate_limits::same_window;
use crate::types::{ApiUsageData, UsageWindow};

const LOOKBACK: TimeDelta = TimeDelta::hours(1);

/// Each end of a span is late by up to one poll interval (300s by default); a shorter
/// span is mostly that error.
const MIN_SPAN: TimeDelta = TimeDelta::minutes(10);

/// Twelve default polls fit in the lookback; the rest is margin.
const MAX_STEPS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Step {
    at: DateTime<Utc>,
    percent: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowHistory {
    resets_at: Option<DateTime<Utc>>,
    /// Last utilization fetched, whether or not it was a step.
    last_percent: Option<f64>,
    steps: Vec<Step>,
    /// Percent per hour.
    last_pace: Option<f64>,
}

impl WindowHistory {
    fn record(&mut self, window: &UsageWindow, now: DateTime<Utc>) {
        if !same_reset(self.resets_at, window.resets_at) {
            *self = Self {
                resets_at: window.resets_at,
                ..Self::default()
            };
        }
        match self.last_percent {
            Some(last) if window.percent > last => {
                self.steps
                    .push(Step {
                        at: now,
                        percent: window.percent,
                    });
                if self
                    .steps
                    .len()
                    > MAX_STEPS
                {
                    self.steps
                        .remove(0);
                }
            }
            // A drop inside one window leaves the earlier steps measuring nothing.
            Some(last) if window.percent < last => self
                .steps
                .clear(),
            _ => {}
        }
        self.last_percent = Some(window.percent);
        if let Some(pace) = measured_pace(&self.steps, now) {
            self.last_pace = Some(pace);
        }
    }

    /// Percent per hour: measured over the lookback, else the last pace measured.
    fn pace(&self, now: DateTime<Utc>) -> Option<f64> {
        measured_pace(&self.steps, now).or(self.last_pace)
    }
}

fn same_reset(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => same_window(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn measured_pace(steps: &[Step], now: DateTime<Utc>) -> Option<f64> {
    let first = steps
        .iter()
        .find(|s| now - s.at <= LOOKBACK)?;
    let last = steps.last()?;
    let span = last.at - first.at;
    (span >= MIN_SPAN && last.percent > first.percent)
        .then(|| (last.percent - first.percent) / (span.num_seconds() as f64 / 3600.0))
}

/// Step histories of the endpoint's windows, persisted in the usage cache.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaceHistory {
    five_hour: WindowHistory,
    seven_day: WindowHistory,
}

impl PaceHistory {
    /// Record a fresh endpoint reading. Call once per fetch, never on a cached read.
    pub fn record(&mut self, usage: &ApiUsageData, now: DateTime<Utc>) {
        if let Some(w) = &usage.five_hour {
            self.five_hour
                .record(w, now);
        }
        if let Some(w) = &usage.seven_day {
            self.seven_day
                .record(w, now);
        }
    }

    /// Set each window's pace on `usage`.
    pub fn apply(&self, usage: &mut ApiUsageData, now: DateTime<Utc>) {
        if let Some(w) = &mut usage.five_hour {
            w.pace_per_hour = self
                .five_hour
                .pace(now);
        }
        if let Some(w) = &mut usage.seven_day {
            w.pace_per_hour = self
                .seven_day
                .pace(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(percent: f64, resets_at: DateTime<Utc>) -> UsageWindow {
        UsageWindow {
            percent,
            resets_at: Some(resets_at),
            pace_per_hour: None,
        }
    }

    /// Feeds `(minutes before t0, percent)` readings into a fresh history.
    fn history(readings: &[(i64, f64)], reset: DateTime<Utc>, t0: DateTime<Utc>) -> WindowHistory {
        let mut h = WindowHistory::default();
        for &(ago, percent) in readings {
            h.record(&window(percent, reset), t0 - TimeDelta::minutes(ago));
        }
        h
    }

    fn t0() -> DateTime<Utc> {
        "2026-11-20T12:00:00Z"
            .parse()
            .unwrap()
    }

    fn reset() -> DateTime<Utc> {
        t0() + TimeDelta::hours(63)
    }

    #[test]
    fn test_pace_spans_steps_inside_lookback() {
        // Steps at 45, 30, 15 and 0 minutes ago: 3% over 45 minutes.
        let h = history(
            &[(50, 95.0), (45, 96.0), (30, 97.0), (15, 98.0), (0, 99.0)],
            reset(),
            t0(),
        );
        assert_eq!(h.pace(t0()), Some(4.0));
    }

    #[test]
    fn test_first_reading_is_not_a_step() {
        // 95 was reached at some unknown time before the first fetch; anchoring on it
        // would make 1% in 15 minutes.
        let h = history(&[(15, 95.0), (0, 96.0)], reset(), t0());
        assert_eq!(h.pace(t0()), None);
    }

    #[test]
    fn test_repeated_reading_adds_no_step() {
        let h = history(
            &[(40, 95.0), (35, 96.0), (30, 96.0), (20, 96.0)],
            reset(),
            t0(),
        );
        assert_eq!(
            h.steps
                .len(),
            1
        );
    }

    #[test]
    fn test_span_shorter_than_two_polls_is_not_a_pace() {
        let h = history(&[(10, 95.0), (5, 96.0), (0, 97.0)], reset(), t0());
        assert_eq!(h.pace(t0()), None);
    }

    #[test]
    fn test_pause_keeps_last_pace() {
        let h = history(&[(200, 90.0), (180, 91.0), (150, 93.0)], reset(), t0());
        // Nothing inside the last hour: the 2% over 30 minutes measured then still holds.
        assert_eq!(measured_pace(&h.steps, t0()), None);
        assert_eq!(h.pace(t0()), Some(4.0));
    }

    #[test]
    fn test_moved_reset_starts_a_new_history() {
        let mut h = history(&[(50, 95.0), (45, 96.0), (15, 98.0)], reset(), t0());
        h.record(&window(1.0, reset() + TimeDelta::days(7)), t0());
        assert_eq!(
            h,
            WindowHistory {
                resets_at: Some(reset() + TimeDelta::days(7)),
                last_percent: Some(1.0),
                ..WindowHistory::default()
            }
        );
    }

    #[test]
    fn test_sub_second_reset_jitter_keeps_the_history() {
        let mut h = history(&[(50, 95.0), (45, 96.0)], reset(), t0());
        h.record(&window(99.0, reset() + TimeDelta::milliseconds(400)), t0());
        assert_eq!(h.pace(t0()), Some(4.0));
    }

    #[test]
    fn test_drop_inside_window_clears_steps() {
        let h = history(
            &[(50, 95.0), (45, 96.0), (15, 98.0), (0, 40.0)],
            reset(),
            t0(),
        );
        assert!(
            h.steps
                .is_empty()
        );
    }

    #[test]
    fn test_steps_are_capped() {
        let readings: Vec<(i64, f64)> = (0..40)
            .map(|i| (200 - i * 5, 50.0 + i as f64))
            .collect();
        let h = history(&readings, reset(), t0());
        assert_eq!(
            h.steps
                .len(),
            MAX_STEPS
        );
    }

    #[test]
    fn test_apply_sets_each_window_pace() {
        let mut pace = PaceHistory::default();
        for (ago, percent) in [(50, 95.0), (45, 96.0), (0, 99.0)] {
            let usage = ApiUsageData {
                five_hour: None,
                seven_day: Some(window(percent, reset())),
                model_scoped: Vec::new(),
            };
            pace.record(&usage, t0() - TimeDelta::minutes(ago));
        }
        let mut usage = ApiUsageData {
            five_hour: Some(window(2.0, t0() + TimeDelta::hours(3))),
            seven_day: Some(window(99.0, reset())),
            model_scoped: Vec::new(),
        };
        pace.apply(&mut usage, t0());
        assert_eq!(
            usage
                .five_hour
                .unwrap()
                .pace_per_hour,
            None
        );
        assert_eq!(
            usage
                .seven_day
                .unwrap()
                .pace_per_hour,
            Some(4.0)
        );
    }
}

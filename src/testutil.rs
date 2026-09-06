use crate::types::{ApiUsageData, ScopedUsageWindow, UsageWindow};
use crate::warn;
use chrono::{DateTime, Utc};
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};

/// Builder for `ApiUsageData` test fixtures; every window starts absent.
#[derive(Default)]
pub struct ApiUsageDataBuilder {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    model_scoped: Vec<ScopedUsageWindow>,
}

impl ApiUsageDataBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn five_hour(mut self, percent: f64) -> Self {
        self.five_hour = Some(UsageWindow {
            percent,
            resets_at: None,
        });
        self
    }

    pub fn five_hour_resetting(mut self, percent: f64, resets_at: DateTime<Utc>) -> Self {
        self.five_hour = Some(UsageWindow {
            percent,
            resets_at: Some(resets_at),
        });
        self
    }

    pub fn seven_day(mut self, percent: f64) -> Self {
        self.seven_day = Some(UsageWindow {
            percent,
            resets_at: None,
        });
        self
    }

    pub fn model_scoped(mut self, models: &[(&str, f64)]) -> Self {
        self.model_scoped = models
            .iter()
            .map(|(name, percent)| ScopedUsageWindow {
                display_name: (*name).to_string(),
                percent: *percent,
            })
            .collect();
        self
    }

    pub fn build(self) -> ApiUsageData {
        ApiUsageData {
            five_hour: self.five_hour,
            seven_day: self.seven_day,
            model_scoped: self.model_scoped,
        }
    }
}

/// `ApiUsageData` with both windows and the given model-scoped buckets, matching the
/// shape the usage endpoint returns when weekly windows are scoped per model.
pub fn scoped_usage(models: &[(&str, f64)]) -> ApiUsageData {
    ApiUsageDataBuilder::new()
        .five_hour(17.0)
        .seven_day(45.0)
        .model_scoped(models)
        .build()
}

/// One transcript JSONL usage line. The timestamp is fixed: nothing under test reads it.
pub fn usage_line(input: u64, output: u64, cache_write: u64, cache_read: u64) -> String {
    format!(
        r#"{{"timestamp":"2024-01-01T00:00:00Z","message":{{"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read}}}}}}}"#
    )
}

/// Owns a `test_scratch_dir` tree and removes it on drop, so an early return or a
/// panic mid-test cannot leak the directory past its trailing cleanup call.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self(dir)
    }
}

impl Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            warn!(
                "scratch dir cleanup failed for {}: {:#}",
                self.0
                    .display(),
                e
            );
        }
    }
}

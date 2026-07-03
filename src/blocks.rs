use crate::paths::iter_jsonl_files_since;
use crate::pricing::PricingFetcher;
use crate::types::{Block, UsageData};
use anyhow::Result;
use chrono::{DateTime, Duration, Timelike, Utc};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

const BLOCK_DURATION_HOURS: i64 = 5;
const FILE_LOOKBACK_HOURS: i64 = 12; // Look back 12h to catch overlapping blocks
const BUFREADER_CAPACITY: usize = 8192;

/// Floor timestamp to the beginning of the hour in UTC
fn floor_to_hour(timestamp: DateTime<Utc>) -> DateTime<Utc> {
    timestamp
        .with_minute(0)
        .and_then(|dt| dt.with_second(0))
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(timestamp)
}

/// Group pre-parsed, sorted usage entries into 5-hour billing blocks.
pub fn group_into_blocks(
    entries: &[(DateTime<Utc>, UsageData)],
    pricing: &PricingFetcher,
) -> Vec<Block> {
    let session_duration_ms = BLOCK_DURATION_HOURS * 60 * 60 * 1000;
    let mut blocks = Vec::new();
    // (floored block start, last entry time)
    let mut current_span: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut block_entries: Vec<&UsageData> = Vec::new();
    let now = Utc::now();

    for (entry_time, entry) in entries {
        let entry_time = *entry_time;

        if let Some((start, last_time)) = current_span {
            let time_since_start = entry_time.timestamp_millis() - start.timestamp_millis();
            let time_since_last = entry_time.timestamp_millis() - last_time.timestamp_millis();

            if time_since_start > session_duration_ms || time_since_last > session_duration_ms {
                blocks.push(create_block_from_entries(
                    start,
                    last_time,
                    &block_entries,
                    now,
                    session_duration_ms,
                    pricing,
                ));
                current_span = Some((floor_to_hour(entry_time), entry_time));
                block_entries = vec![entry];
            } else {
                current_span = Some((start, entry_time));
                block_entries.push(entry);
            }
        } else {
            current_span = Some((floor_to_hour(entry_time), entry_time));
            block_entries = vec![entry];
        }
    }

    if let Some((start, last_time)) = current_span {
        blocks.push(create_block_from_entries(
            start,
            last_time,
            &block_entries,
            now,
            session_duration_ms,
            pricing,
        ));
    }

    blocks
}

/// Create a block from start time, last-entry time, and entries (matching TypeScript logic).
pub fn create_block_from_entries(
    start_time: DateTime<Utc>,
    actual_end_time: DateTime<Utc>,
    entries: &[&UsageData],
    now: DateTime<Utc>,
    session_duration_ms: i64,
    pricing: &PricingFetcher,
) -> Block {
    let end_time = start_time + Duration::milliseconds(session_duration_ms);

    // TypeScript logic: isActive = now - actualEndTime < sessionDuration && now < endTime
    let time_since_last_activity = now.timestamp_millis() - actual_end_time.timestamp_millis();
    let is_active = time_since_last_activity < session_duration_ms && now < end_time;

    let hours_remaining = if is_active {
        let remaining = (end_time - now).num_seconds() as f64 / 3600.0;
        Some(remaining.max(0.0))
    } else {
        None
    };

    let mut cost_usd = 0.0;
    for entry in entries {
        cost_usd += pricing.calculate_entry_cost(entry);
    }

    Block {
        start_time,
        end_time,
        cost_usd,
        is_active,
        hours_remaining,
    }
}

/// Find active billing block
pub fn find_active_block(claude_paths: &[PathBuf], pricing: &PricingFetcher) -> Result<Block> {
    let mut all_entries: Vec<UsageData> = Vec::with_capacity(1000);
    let mut processed_hashes: HashSet<String> = HashSet::with_capacity(1000);

    let now = Utc::now();
    let file_cutoff_time = now - Duration::hours(FILE_LOOKBACK_HOURS);
    let file_cutoff_timestamp = file_cutoff_time.timestamp();

    for session_file in iter_jsonl_files_since(claude_paths, Some(file_cutoff_timestamp))? {
        // Skip files not modified within lookback window
        if let Ok(metadata) = fs::metadata(&session_file)
            && let Ok(modified) = metadata.modified()
            && let Ok(modified_duration) = modified.duration_since(std::time::UNIX_EPOCH)
            && (modified_duration.as_secs() as i64) < file_cutoff_timestamp
        {
            continue;
        }

        let file = File::open(&session_file)?;
        let reader = BufReader::with_capacity(BUFREADER_CAPACITY, file);

        for line in reader.lines() {
            let line = line?;
            if line
                .trim()
                .is_empty()
            {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<UsageData>(&line) {
                if let (Some(msg_id), Some(req_id)) = (
                    &entry
                        .message
                        .id,
                    &entry.request_id,
                ) {
                    let mut hash = String::with_capacity(msg_id.len() + req_id.len() + 1);
                    hash.push_str(msg_id);
                    hash.push(':');
                    hash.push_str(req_id);

                    if !processed_hashes.insert(hash) {
                        continue;
                    }
                }

                all_entries.push(entry);
            }
        }
    }

    // Parse each timestamp once; skip entries whose timestamps are unparseable
    let mut parsed: Vec<(DateTime<Utc>, UsageData)> = all_entries
        .into_iter()
        .filter_map(|e| {
            DateTime::parse_from_rfc3339(&e.timestamp)
                .ok()
                .map(|dt| (dt.with_timezone(&Utc), e))
        })
        .collect();
    parsed.sort_by_key(|(dt, _)| *dt);

    let blocks = group_into_blocks(&parsed, pricing);

    let now = Utc::now();
    for block in blocks
        .iter()
        .rev()
    {
        if block.is_active && block.end_time > now {
            return Ok(block.clone());
        }
    }

    let next_end = now + Duration::hours(BLOCK_DURATION_HOURS);
    Ok(Block {
        start_time: now,
        end_time: next_end,
        cost_usd: 0.0,
        is_active: false,
        hours_remaining: None,
    })
}

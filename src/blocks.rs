use crate::paths::iter_jsonl_files;
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

/// Group usage entries into blocks (matching TypeScript logic)
pub fn group_into_blocks(entries: &[UsageData], pricing: &PricingFetcher) -> Result<Vec<Block>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let session_duration_ms = BLOCK_DURATION_HOURS * 60 * 60 * 1000; // Block duration in milliseconds
    let mut blocks = Vec::new();
    let mut current_block_start: Option<DateTime<Utc>> = None;
    let mut current_block_entries: Vec<&UsageData> = Vec::new();
    let now = Utc::now();

    for entry in entries {
        let entry_time = DateTime::parse_from_rfc3339(&entry.timestamp)?;
        let entry_time = entry_time.with_timezone(&Utc);

        match current_block_start {
            None => {
                // First entry - floor to hour
                current_block_start = Some(floor_to_hour(entry_time));
                current_block_entries = vec![entry];
            }
            Some(start) => {
                let time_since_block_start =
                    entry_time.timestamp_millis() - start.timestamp_millis();
                let last_entry = current_block_entries.last();

                let should_close_block = if let Some(last) = last_entry {
                    let last_time =
                        DateTime::parse_from_rfc3339(&last.timestamp)?.with_timezone(&Utc);
                    let time_since_last =
                        entry_time.timestamp_millis() - last_time.timestamp_millis();

                    time_since_block_start > session_duration_ms
                        || time_since_last > session_duration_ms
                } else {
                    false
                };

                if should_close_block {
                    // Close current block
                    let block = create_block_from_entries(
                        start,
                        &current_block_entries,
                        now,
                        session_duration_ms,
                        pricing,
                    );
                    blocks.push(block);

                    // Start new block (floored to hour)
                    current_block_start = Some(floor_to_hour(entry_time));
                    current_block_entries = vec![entry];
                } else {
                    // Add to current block
                    current_block_entries.push(entry);
                }
            }
        }
    }

    // Close the last block
    if let Some(start) = current_block_start
        && !current_block_entries.is_empty()
    {
        let block = create_block_from_entries(
            start,
            &current_block_entries,
            now,
            session_duration_ms,
            pricing,
        );
        blocks.push(block);
    }

    Ok(blocks)
}

/// Create a block from start time and entries (matching TypeScript logic)
pub fn create_block_from_entries(
    start_time: DateTime<Utc>,
    entries: &[&UsageData],
    now: DateTime<Utc>,
    session_duration_ms: i64,
    pricing: &PricingFetcher,
) -> Block {
    let end_time = start_time + Duration::milliseconds(session_duration_ms);

    // Find actual end time (last entry timestamp)
    let actual_end_time = entries
        .last()
        .and_then(|e| DateTime::parse_from_rfc3339(&e.timestamp).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(start_time);

    // TypeScript logic: isActive = now - actualEndTime < sessionDuration && now < endTime
    let time_since_last_activity = now.timestamp_millis() - actual_end_time.timestamp_millis();
    let is_active = time_since_last_activity < session_duration_ms && now < end_time;

    // Calculate hours remaining if active
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
    let mut all_entries = Vec::with_capacity(1000);
    let mut processed_hashes: HashSet<String> = HashSet::with_capacity(1000);

    let now = Utc::now();
    let file_cutoff_time = now - Duration::hours(FILE_LOOKBACK_HOURS);
    let file_cutoff_timestamp = file_cutoff_time.timestamp();

    for session_file in iter_jsonl_files(claude_paths)? {
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

    all_entries.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
    });

    let blocks = group_into_blocks(&all_entries, pricing)?;

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

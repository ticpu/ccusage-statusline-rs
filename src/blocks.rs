use crate::pricing::PricingFetcher;
use crate::types::{Block, UsageData};
use anyhow::Result;
use chrono::{DateTime, Duration, Timelike, Utc};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// Floor timestamp to the beginning of the hour in UTC
fn floor_to_hour(timestamp: DateTime<Utc>) -> DateTime<Utc> {
    timestamp
        .with_minute(0)
        .and_then(|dt| dt.with_second(0))
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(timestamp)
}

/// Group usage entries into 5-hour blocks (matching TypeScript logic)
pub fn group_into_blocks(entries: &[UsageData], pricing: &PricingFetcher) -> Result<Vec<Block>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }

    let session_duration_ms = 5 * 60 * 60 * 1000; // 5 hours in milliseconds
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

    // Aggregate costs and tokens
    let mut cost_usd = 0.0;
    let mut total_tokens = 0u64;

    for entry in entries {
        cost_usd += pricing.calculate_entry_cost(entry);
        total_tokens += entry.message.usage.input_tokens + entry.message.usage.output_tokens;
    }

    Block {
        start_time,
        end_time,
        cost_usd,
        total_tokens,
        is_active,
    }
}

/// Find active billing block
pub fn find_active_block(claude_paths: &[PathBuf], pricing: &PricingFetcher) -> Result<Block> {
    let mut all_entries = Vec::new();
    let mut processed_hashes: HashSet<String> = HashSet::new();

    // Collect all usage data from all projects
    for base_path in claude_paths {
        for project_entry in fs::read_dir(base_path)? {
            let project_dir = project_entry?.path();
            if !project_dir.is_dir() {
                continue;
            }

            for session_entry in fs::read_dir(&project_dir)? {
                let session_file = session_entry?.path();
                if session_file.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }

                // Read JSONL file
                let file = File::open(&session_file)?;
                let reader = BufReader::new(file);

                for line in reader.lines() {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(entry) = serde_json::from_str::<UsageData>(&line) {
                        // Create unique hash from message_id and request_id (matching TypeScript logic)
                        let unique_hash = match (&entry.message.id, &entry.request_id) {
                            (Some(msg_id), Some(req_id)) => Some(format!("{}:{}", msg_id, req_id)),
                            _ => None,
                        };

                        // Skip duplicates (matching TypeScript deduplication)
                        if let Some(hash) = &unique_hash {
                            if processed_hashes.contains(hash) {
                                continue; // Skip duplicate entry
                            }
                            processed_hashes.insert(hash.clone());
                        }

                        all_entries.push(entry);
                    }
                }
            }
        }
    }

    // Sort by timestamp
    all_entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Group into 5-hour blocks
    let blocks = group_into_blocks(&all_entries, pricing)?;

    // Find active block
    let now = Utc::now();
    for block in blocks.iter().rev() {
        if block.is_active && block.end_time > now {
            return Ok(Block {
                start_time: block.start_time,
                end_time: block.end_time,
                cost_usd: block.cost_usd,
                total_tokens: block.total_tokens,
                is_active: true,
            });
        }
    }

    // No active block found, return empty
    Ok(Block {
        start_time: now,
        end_time: now + Duration::hours(5),
        cost_usd: 0.0,
        total_tokens: 0,
        is_active: false,
    })
}

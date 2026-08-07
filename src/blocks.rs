use crate::paths::{iter_jsonl_files_since, warn_skipped};
use crate::pricing::PricingFetcher;
use crate::types::{ActiveBlock, UsageData};
use anyhow::Result;
use chrono::{DateTime, Duration, Timelike, Utc};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

const BLOCK_DURATION_HOURS: i64 = 5;
const FILE_LOOKBACK_HOURS: i64 = 12; // catches blocks that span the boundary
const BUFREADER_CAPACITY: usize = 8192;
/// Substring every billable entry carries; used to skip lines before parsing them.
const USAGE_MARKER: &str = "\"usage\"";

/// Internal representation of a billing block span
struct Block {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    cost_usd: f64,
    is_active: bool,
}

/// Floor timestamp to the beginning of the hour in UTC
fn floor_to_hour(timestamp: DateTime<Utc>) -> DateTime<Utc> {
    timestamp
        .with_minute(0)
        .and_then(|dt| dt.with_second(0))
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(timestamp)
}

/// Group pre-parsed, sorted usage entries into 5-hour billing blocks.
fn group_into_blocks(
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
fn create_block_from_entries(
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

    let mut cost_usd = 0.0;
    for entry in entries {
        cost_usd += pricing.calculate_entry_cost(entry);
    }

    Block {
        start_time,
        end_time,
        cost_usd,
        is_active,
    }
}

/// Marker for the field the bisect reads. Cheaper than parsing the line as JSON.
const TIMESTAMP_MARKER: &str = "\"timestamp\":\"";

/// Below this, seeking costs more than just reading the file.
const BISECT_MIN_BYTES: u64 = 256 * 1024;

/// Extract the RFC3339 timestamp from a raw transcript line without parsing it.
fn line_timestamp(line: &str) -> Option<&str> {
    let start = line.find(TIMESTAMP_MARKER)? + TIMESTAMP_MARKER.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Byte offset of a line boundary at or before the first entry newer than `cutoff`.
///
/// A long-running session's transcript is mostly older than the lookback, and reading
/// those bytes only to discard them dominates the render. Entries are appended in
/// order, so the window we want is always a suffix and can be found by bisection.
/// Returns a conservative boundary — never past the first entry we still need.
fn seek_to_cutoff(reader: &mut BufReader<File>, len: u64, cutoff: &str) -> std::io::Result<u64> {
    let mut lo = 0u64;
    let mut hi = len;
    let mut line = String::with_capacity(BUFREADER_CAPACITY);

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        reader.seek(SeekFrom::Start(mid))?;

        // Land on a line boundary: the bytes before it belong to the previous line.
        let mut start = mid;
        if mid > 0 {
            line.clear();
            let skipped = reader.read_line(&mut line)?;
            if skipped == 0 {
                hi = mid;
                continue;
            }
            start += skipped as u64;
        }

        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            hi = mid;
            continue;
        }

        match line_timestamp(&line) {
            // Proven old: nothing at or before this line's end is still needed.
            Some(ts) if ts < cutoff => lo = start + read as u64,
            Some(_) => hi = mid,
            // No timestamp to compare; narrow from the top rather than skip data.
            None => hi = mid,
        }
    }

    Ok(lo)
}

/// Find the most recent active billing block, if any.
pub fn find_active_block(
    claude_paths: &[PathBuf],
    pricing: &PricingFetcher,
) -> Result<Option<ActiveBlock>> {
    let mut all_entries: Vec<UsageData> = Vec::with_capacity(1000);
    let mut processed_hashes: HashSet<String> = HashSet::with_capacity(1000);

    let now = Utc::now();
    let file_cutoff_time = now - Duration::hours(FILE_LOOKBACK_HOURS);
    let file_cutoff_timestamp = file_cutoff_time.timestamp();
    let cutoff_rfc3339 = file_cutoff_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let session_files = crate::timing::phase_counted("block.scan", || {
        let r = iter_jsonl_files_since(claude_paths, Some(file_cutoff_timestamp));
        let n = r
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0);
        (r, n)
    })?;

    let mut line = String::with_capacity(BUFREADER_CAPACITY);

    for session_file in session_files {
        let file = match File::open(&session_file) {
            Ok(f) => f,
            Err(e) => {
                // A transcript can vanish between the scan and the open; one missing
                // session must not blank the whole statusline.
                warn_skipped(&session_file, &e);
                continue;
            }
        };
        let file_len = file
            .metadata()
            .map(|m| m.len())
            .unwrap_or(0);
        let mut reader = BufReader::with_capacity(BUFREADER_CAPACITY, file);

        if file_len >= BISECT_MIN_BYTES {
            match seek_to_cutoff(&mut reader, file_len, &cutoff_rfc3339)
                .and_then(|off| reader.seek(SeekFrom::Start(off)))
            {
                Ok(_) => {}
                Err(e) => {
                    // Fall back to the whole file rather than lose the session.
                    warn_skipped(&session_file, &e);
                    if let Err(e) = reader.seek(SeekFrom::Start(0)) {
                        warn_skipped(&session_file, &e);
                        continue;
                    }
                }
            }
        }

        // One buffer for the whole file: `lines()` heap-allocates per line, and these
        // transcripts run to hundreds of thousands of lines.
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    warn_skipped(&session_file, &e);
                    break;
                }
            }
            // Most transcript lines are prompts and tool results carrying no usage, and
            // they are the large ones. Rejecting them on a substring keeps serde off the
            // bulk of the file: parsing every line dominates the whole render otherwise.
            if !line.contains(USAGE_MARKER) {
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
            let hours_remaining = ((block.end_time - now).num_seconds() as f64 / 3600.0).max(0.0);
            return Ok(Some(ActiveBlock {
                start_time: block.start_time,
                cost_usd: block.cost_usd,
                hours_remaining,
            }));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn line_at(ts: &str, body: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","message":{{"usage":{{"input_tokens":1}}}},"note":"{body}"}}"#
        )
    }

    #[test]
    fn test_line_timestamp_extracts_field() {
        let l = line_at("2026-08-07T10:00:00.000Z", "x");
        assert_eq!(line_timestamp(&l), Some("2026-08-07T10:00:00.000Z"));
        assert_eq!(line_timestamp(r#"{"no":"stamp"}"#), None);
    }

    /// The bisect must never land past an entry still inside the window, whatever the
    /// cutoff falls between — an offset that is too late silently drops billable tokens.
    #[test]
    fn test_seek_to_cutoff_never_skips_needed_entries() {
        let dir = crate::paths::test_scratch_dir("blocks-bisect");
        let path = dir.join("session.jsonl");

        // Padding makes lines long enough that the bisect takes several steps.
        let pad = "p".repeat(2048);
        let mut f = fs::File::create(&path).unwrap();
        for hour in 0..48 {
            writeln!(
                f,
                "{}",
                line_at(&format!("2026-08-07T{hour:02}:00:00.000Z"), &pad)
            )
            .unwrap();
        }
        drop(f);

        let len = fs::metadata(&path)
            .unwrap()
            .len();

        for hour in 0..48 {
            let cutoff = format!("2026-08-07{}{hour:02}:00:00.000Z", "T");
            let mut reader =
                BufReader::with_capacity(BUFREADER_CAPACITY, fs::File::open(&path).unwrap());
            let off = seek_to_cutoff(&mut reader, len, &cutoff).unwrap();

            // Everything from `off` onward must still contain every entry >= cutoff.
            reader
                .seek(SeekFrom::Start(off))
                .unwrap();
            let mut found = 0;
            let mut line = String::new();
            loop {
                line.clear();
                if reader
                    .read_line(&mut line)
                    .unwrap()
                    == 0
                {
                    break;
                }
                if let Some(ts) = line_timestamp(&line)
                    && ts >= cutoff.as_str()
                {
                    found += 1;
                }
            }
            assert_eq!(found, 48 - hour, "cutoff hour {hour} lost entries");
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_seek_to_cutoff_handles_cutoff_past_end() {
        let dir = crate::paths::test_scratch_dir("blocks-bisect-past");
        let path = dir.join("session.jsonl");
        let pad = "p".repeat(2048);
        let mut f = fs::File::create(&path).unwrap();
        for hour in 0..10 {
            writeln!(
                f,
                "{}",
                line_at(&format!("2026-08-07T{hour:02}:00:00.000Z"), &pad)
            )
            .unwrap();
        }
        drop(f);

        let len = fs::metadata(&path)
            .unwrap()
            .len();
        let mut reader =
            BufReader::with_capacity(BUFREADER_CAPACITY, fs::File::open(&path).unwrap());
        let off = seek_to_cutoff(&mut reader, len, "2026-09-01T00:00:00.000Z").unwrap();
        assert!(off <= len);

        fs::remove_dir_all(&dir).unwrap();
    }
}

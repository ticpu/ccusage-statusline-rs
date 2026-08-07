use crate::entry_cache::CachedEntry;
use crate::paths::{iter_jsonl_files_since, warn_skipped};
use crate::pricing::PricingFetcher;
use crate::types::{ActiveBlock, UsageData};
use anyhow::Result;
use chrono::{DateTime, Duration, Timelike, Utc};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const BLOCK_DURATION_HOURS: i64 = 5;
const FILE_LOOKBACK_HOURS: i64 = 12; // only used when no authoritative reset time is available
const BUFREADER_CAPACITY: usize = 8192;
/// Substring every billable entry carries; used to skip lines before parsing them.
const USAGE_MARKER: &[u8] = b"\"usage\"";

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
fn group_into_blocks(entries: &[CachedEntry], pricing: &PricingFetcher) -> Vec<Block> {
    let session_duration_ms = BLOCK_DURATION_HOURS * 60 * 60 * 1000;
    let mut blocks = Vec::new();
    // (floored block start, last entry time)
    let mut current_span: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut block_entries: Vec<&CachedEntry> = Vec::new();
    let now = Utc::now();

    for entry in entries {
        let Some(entry_time) = DateTime::from_timestamp_millis(entry.ts) else {
            continue;
        };

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
    entries: &[&CachedEntry],
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
        cost_usd += pricing.calculate_cost_for(
            entry
                .model
                .as_deref(),
            &entry.usage,
        );
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

/// Parse the appended part of one transcript into cache entries.
fn parse_from(
    session_file: &Path,
    resume_at: u64,
    file_len: u64,
    cutoff_rfc3339: &str,
    line: &mut Vec<u8>,
) -> (Vec<CachedEntry>, u64, u64) {
    let file = match File::open(session_file) {
        Ok(f) => f,
        Err(e) => {
            // A transcript can vanish between the scan and the open; one missing
            // session must not blank the whole statusline.
            warn_skipped(session_file, &e);
            return (Vec::new(), resume_at, 0);
        }
    };
    let mut reader = BufReader::with_capacity(BUFREADER_CAPACITY, file);

    // Nothing cached yet: skip the bulk of a long transcript instead of reading it
    // only to discard everything before the window.
    let mut offset = resume_at;
    if resume_at == 0 && file_len >= BISECT_MIN_BYTES {
        match seek_to_cutoff(&mut reader, file_len, cutoff_rfc3339) {
            Ok(off) => offset = off,
            Err(e) => warn_skipped(session_file, &e),
        }
    }
    if let Err(e) = reader.seek(SeekFrom::Start(offset)) {
        warn_skipped(session_file, &e);
        return (Vec::new(), resume_at, 0);
    }

    let mut entries = Vec::new();
    let mut read_bytes = 0u64;
    let mut consumed = offset;

    loop {
        line.clear();
        match reader.read_until(b'\n', line) {
            Ok(0) => break,
            Ok(n) => {
                read_bytes += n as u64;
                consumed += n as u64;
            }
            Err(e) => {
                warn_skipped(session_file, &e);
                break;
            }
        }
        // Most transcript lines are prompts and tool results carrying no usage, and
        // they are the large ones. Rejecting them on a substring keeps serde off the
        // bulk of the file: parsing every line dominates the whole render otherwise.
        if memchr::memmem::find(line, USAGE_MARKER).is_none() {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<UsageData>(line) else {
            continue;
        };
        let Ok(ts) = DateTime::parse_from_rfc3339(&entry.timestamp) else {
            continue;
        };

        let key = match (
            &entry
                .message
                .id,
            &entry.request_id,
        ) {
            (Some(m), Some(r)) => Some(format!("{m}:{r}")),
            _ => None,
        };
        entries.push(CachedEntry {
            ts: ts.timestamp_millis(),
            key,
            model: entry
                .message
                .model,
            usage: entry
                .message
                .usage,
        });
    }

    (entries, consumed, read_bytes)
}

/// Every deduplicated entry at or after `cutoff`, sorted by timestamp.
///
/// Transcripts are only ever extended, so each render parses the bytes appended since
/// the last one and reuses what was already extracted.
fn collect_entries(
    claude_paths: &[PathBuf],
    cutoff: DateTime<Utc>,
    cache_dir: &Path,
) -> Result<Vec<CachedEntry>> {
    // The cache is filled to the widest horizon any caller can ask for, so a narrow
    // request never leaves it unable to answer a later wider one.
    let widest = Utc::now() - Duration::hours(FILE_LOOKBACK_HOURS);
    let horizon = cutoff.min(widest);
    let cutoff_rfc3339 = horizon.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let session_files = crate::timing::phase_counted("block.scan", || {
        let r = iter_jsonl_files_since(claude_paths, Some(horizon.timestamp()));
        let n = r
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(0);
        (r, n)
    })?;

    // Bytes, not String: `read_until` skips the UTF-8 validation `read_line` would run
    // over every transcript, and most lines are discarded immediately.
    let mut line: Vec<u8> = Vec::with_capacity(BUFREADER_CAPACITY);
    let mut read_bytes = 0u64;

    let cache_file = crate::entry_cache::cache_path(cache_dir);
    let mut collected: Vec<CachedEntry> = Vec::with_capacity(1000);

    crate::entry_cache::with_cache(&cache_file, |cache| {
        let mut changed = false;

        for session_file in &session_files {
            let file_len = fs::metadata(session_file)
                .map(|m| m.len())
                .unwrap_or(0);
            let resume_at = cache.resume_at(session_file, file_len);

            if resume_at < file_len {
                let (entries, consumed, bytes) = parse_from(
                    session_file,
                    resume_at,
                    file_len,
                    &cutoff_rfc3339,
                    &mut line,
                );
                read_bytes += bytes;
                if consumed != resume_at || !entries.is_empty() {
                    cache.record(session_file, consumed, entries);
                    changed = true;
                }
            }

            collected.extend_from_slice(cache.entries_for(session_file));
        }

        if changed {
            cache.prune(horizon.timestamp_millis());
        }
        ((), changed)
    })?;

    crate::timing::note("block.read", read_bytes, collected.len());

    // Deduplication spans files, so it cannot happen while reading any single one.
    let mut seen: HashSet<&str> = HashSet::with_capacity(collected.len());
    let cutoff_ms = cutoff.timestamp_millis();
    let mut out: Vec<CachedEntry> = Vec::with_capacity(collected.len());
    for entry in &collected {
        if entry.ts < cutoff_ms {
            continue;
        }
        if let Some(k) = &entry.key
            && !seen.insert(k.as_str())
        {
            continue;
        }
        out.push(entry.clone());
    }

    out.sort_by_key(|e| e.ts);
    Ok(out)
}

/// Find the most recent active billing block, if any.
///
/// `five_hour_reset` is the authoritative window end when the API or stdin supplied
/// one. Deriving boundaries instead makes each one depend on all earlier activity, so
/// any scan horizon silently moves the active block's start; a known reset time pins
/// it, and bounds the scan to the block itself rather than a guessed lookback.
pub fn find_active_block(
    claude_paths: &[PathBuf],
    pricing: &PricingFetcher,
    cache_dir: &Path,
    five_hour_reset: Option<DateTime<Utc>>,
) -> Result<Option<ActiveBlock>> {
    let now = Utc::now();

    if let Some(reset) = five_hour_reset.filter(|r| *r > now) {
        let start = reset - Duration::hours(BLOCK_DURATION_HOURS);
        let entries = collect_entries(claude_paths, start, cache_dir)?;
        let cost_usd = entries
            .iter()
            .map(|e| {
                pricing.calculate_cost_for(
                    e.model
                        .as_deref(),
                    &e.usage,
                )
            })
            .sum();

        return Ok(Some(ActiveBlock {
            start_time: start,
            cost_usd,
            hours_remaining: ((reset - now).num_seconds() as f64 / 3600.0).max(0.0),
        }));
    }

    // No reset time: fall back to deriving boundaries from activity gaps. The horizon
    // is a compromise — too short re-anchors the chain, too long costs more than the
    // whole render budget.
    let parsed = collect_entries(
        claude_paths,
        now - Duration::hours(FILE_LOOKBACK_HOURS),
        cache_dir,
    )?;
    let blocks = crate::timing::phase("block.group", || group_into_blocks(&parsed, pricing));

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

    fn usage_line(ts: &DateTime<Utc>, id: &str, input: u64) -> String {
        format!(
            r#"{{"timestamp":"{}","requestId":"r{id}","message":{{"id":"m{id}","model":"claude-opus-5","usage":{{"input_tokens":{input},"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        )
    }

    /// Resuming from the recorded offset must produce exactly what a full re-read would.
    /// If it ever does not, cost is silently wrong and nothing reports it.
    #[test]
    fn test_incremental_parse_matches_full_reparse() {
        let root = crate::paths::test_scratch_dir("blocks-incremental");
        let projects = root.join("projects");
        let proj = projects.join("p");
        fs::create_dir_all(&proj).unwrap();
        let transcript = proj.join("session.jsonl");

        let now = Utc::now();
        let mut f = fs::File::create(&transcript).unwrap();
        for i in 0..3 {
            writeln!(
                f,
                "{}",
                usage_line(&(now - Duration::minutes(60 - i)), &i.to_string(), 100)
            )
            .unwrap();
        }
        drop(f);

        let paths = vec![projects.clone()];
        let cutoff = now - Duration::hours(5);

        // Warm the cache, then append and collect again.
        let first = collect_entries(&paths, cutoff, &root).unwrap();
        assert_eq!(first.len(), 3);

        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        for i in 3..6 {
            writeln!(
                f,
                "{}",
                usage_line(&(now - Duration::minutes(60 - i)), &i.to_string(), 100)
            )
            .unwrap();
        }
        drop(f);

        let incremental = collect_entries(&paths, cutoff, &root).unwrap();

        // Same inputs, but with no cache to resume from.
        fs::remove_file(crate::entry_cache::cache_path(&root)).unwrap();
        let full = collect_entries(&paths, cutoff, &root).unwrap();

        assert_eq!(incremental.len(), 6);
        assert_eq!(
            incremental
                .iter()
                .map(|e| (
                    e.ts,
                    e.key
                        .clone(),
                    e.usage
                        .input_tokens
                ))
                .collect::<Vec<_>>(),
            full.iter()
                .map(|e| (
                    e.ts,
                    e.key
                        .clone(),
                    e.usage
                        .input_tokens
                ))
                .collect::<Vec<_>>()
        );

        fs::remove_dir_all(&root).unwrap();
    }

    /// The same message written to two transcripts must be counted once.
    #[test]
    fn test_dedup_spans_files() {
        let root = crate::paths::test_scratch_dir("blocks-dedup");
        let projects = root.join("projects");
        let proj = projects.join("p");
        fs::create_dir_all(&proj).unwrap();

        let now = Utc::now();
        let line = usage_line(&(now - Duration::minutes(10)), "same", 100);
        for name in ["a.jsonl", "b.jsonl"] {
            let mut f = fs::File::create(proj.join(name)).unwrap();
            writeln!(f, "{line}").unwrap();
        }

        let entries = collect_entries(&[projects], now - Duration::hours(5), &root).unwrap();
        assert_eq!(entries.len(), 1);

        fs::remove_dir_all(&root).unwrap();
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

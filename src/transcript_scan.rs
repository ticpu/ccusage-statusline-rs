use crate::entry_cache::CachedEntry;
use crate::paths::warn_skipped;
use crate::types::UsageData;
use chrono::DateTime;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

const BUFREADER_CAPACITY: usize = 8192;

/// Substring every billable entry carries; used to skip lines before parsing them.
const USAGE_MARKER: &[u8] = b"\"usage\"";

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
            // Proven old, and only a same-shaped UTC stamp proves it: an offset form
            // sorts below the cutoff at the same instant and would skip needed entries.
            Some(ts) if ts.len() == cutoff.len() && ts.ends_with('Z') && ts < cutoff => {
                lo = start + read as u64
            }
            // Nothing comparable here; narrow from the top rather than skip data.
            _ => hi = mid,
        }
    }

    Ok(lo)
}

/// What one transcript's appended region yielded.
pub struct ParsedChunk {
    pub entries: Vec<CachedEntry>,
    /// Byte offset the next resumed parse of this transcript must start from.
    pub consumed: u64,
    pub read_bytes: u64,
}

/// Parses transcripts one after another, reusing a single line buffer.
pub struct TranscriptParser {
    /// Bytes, not String: `read_until` skips the UTF-8 validation `read_line` would run
    /// over every transcript, and most lines are discarded immediately.
    line: Vec<u8>,
}

impl TranscriptParser {
    pub fn new() -> Self {
        Self {
            line: Vec::with_capacity(BUFREADER_CAPACITY),
        }
    }

    /// Parse the appended part of one transcript into cache entries.
    pub fn parse(
        &mut self,
        session_file: &Path,
        resume_at: u64,
        file_len: u64,
        cutoff_rfc3339: &str,
    ) -> ParsedChunk {
        let unread = ParsedChunk {
            entries: Vec::new(),
            consumed: resume_at,
            read_bytes: 0,
        };

        let file = match File::open(session_file) {
            Ok(f) => f,
            Err(e) => {
                // A transcript can vanish between the scan and the open; one missing
                // session must not blank the whole statusline.
                warn_skipped(session_file, &e);
                return unread;
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
            return unread;
        }

        let mut entries = Vec::new();
        let mut read_bytes = 0u64;
        let mut consumed = offset;

        loop {
            self.line
                .clear();
            match reader.read_until(b'\n', &mut self.line) {
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
            if memchr::memmem::find(&self.line, USAGE_MARKER).is_none() {
                continue;
            }
            let Ok(entry) = serde_json::from_slice::<UsageData>(&self.line) else {
                continue;
            };
            if entry
                .message
                .is_synthetic()
            {
                continue;
            }
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

        ParsedChunk {
            entries,
            consumed,
            read_bytes,
        }
    }
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
        let dir = crate::paths::test_scratch_dir("scan-bisect");
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
    }

    /// An offset-form timestamp sorts below a `Z` cutoff at the same instant, so treating
    /// it as old drops billable entries. The bisect must give up and keep the whole file.
    #[test]
    fn test_seek_to_cutoff_keeps_foreign_timestamp_shapes() {
        let dir = crate::paths::test_scratch_dir("scan-bisect-shape");
        let path = dir.join("session.jsonl");
        let pad = "p".repeat(2048);
        let mut f = fs::File::create(&path).unwrap();
        for hour in 0..24 {
            writeln!(
                f,
                "{}",
                line_at(&format!("2026-08-07T{hour:02}:00:00.000+00:00"), &pad)
            )
            .unwrap();
        }
        drop(f);

        let len = fs::metadata(&path)
            .unwrap()
            .len();
        let mut reader =
            BufReader::with_capacity(BUFREADER_CAPACITY, fs::File::open(&path).unwrap());
        let off = seek_to_cutoff(&mut reader, len, "2026-08-07T23:00:00.000Z").unwrap();
        assert_eq!(off, 0);
    }

    #[test]
    fn test_seek_to_cutoff_handles_cutoff_past_end() {
        let dir = crate::paths::test_scratch_dir("scan-bisect-past");
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
    }
}

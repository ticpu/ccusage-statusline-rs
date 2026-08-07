use crate::types::UsageTokens;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// One billable transcript entry, reduced to what pricing and dedup need.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Entry timestamp as epoch milliseconds.
    pub ts: i64,
    /// `{messageId}:{requestId}`, absent when the entry carries neither.
    pub key: Option<String>,
    pub model: Option<String>,
    pub usage: UsageTokens,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CachedFile {
    /// Bytes of this transcript already turned into `entries`.
    pub consumed: u64,
    pub entries: Vec<CachedEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EntryCache {
    files: HashMap<String, CachedFile>,
}

impl EntryCache {
    /// Where a resumed parse of `path` must start, and whether prior entries survive.
    ///
    /// A transcript shorter than what was already consumed cannot be the same file
    /// extended, so its entries are dropped and it is read from the beginning.
    pub fn resume_at(&mut self, path: &Path, current_len: u64) -> u64 {
        let key = path
            .to_string_lossy()
            .into_owned();
        match self
            .files
            .get(&key)
        {
            Some(f) if f.consumed <= current_len => f.consumed,
            Some(_) => {
                self.files
                    .remove(&key);
                0
            }
            None => 0,
        }
    }

    pub fn record(&mut self, path: &Path, consumed: u64, mut new_entries: Vec<CachedEntry>) {
        let key = path
            .to_string_lossy()
            .into_owned();
        let slot = self
            .files
            .entry(key)
            .or_default();
        slot.consumed = consumed;
        slot.entries
            .append(&mut new_entries);
    }

    pub fn entries_for(&self, path: &Path) -> &[CachedEntry] {
        self.files
            .get(
                path.to_string_lossy()
                    .as_ref(),
            )
            .map(|f| {
                f.entries
                    .as_slice()
            })
            .unwrap_or(&[])
    }

    /// Drop entries older than `cutoff_ms`, and any transcript left with none.
    pub fn prune(&mut self, cutoff_ms: i64) {
        self.files
            .retain(|_, f| {
                f.entries
                    .retain(|e| e.ts >= cutoff_ms);
                !f.entries
                    .is_empty()
            });
    }
}

pub fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("transcript-entries.json")
}

/// Read the cache, run `f` against it, and write it back if `f` reports a change.
///
/// Held under one exclusive lock for the whole read-modify-write and written through
/// that same descriptor: publishing by rename would strand concurrent renders on the
/// unlinked file and lose whichever update finished first.
pub fn with_cache<T>(path: &Path, f: impl FnOnce(&mut EntryCache) -> (T, bool)) -> Result<T> {
    let mut file = crate::cache::open_private_rw(path)?;

    file.lock()
        .with_context(|| format!("Failed to lock entry cache {}", path.display()))?;

    let mut contents = String::new();
    let mut cache = match file.read_to_string(&mut contents) {
        Ok(0) => EntryCache::default(),
        // A corrupt cache is only ever a slower render, so it is rebuilt rather than
        // reported as a failure.
        Ok(_) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(e) => {
            file.unlock()?;
            return Err(e)
                .with_context(|| format!("Failed to read entry cache {}", path.display()));
        }
    };

    let (out, changed) = f(&mut cache);

    if changed {
        let json = serde_json::to_string(&cache)?;
        let write = (|| -> std::io::Result<()> {
            file.set_len(0)?;
            file.rewind()?;
            file.write_all(json.as_bytes())?;
            file.sync_data()
        })();
        if let Err(e) = write {
            file.unlock()?;
            return Err(e)
                .with_context(|| format!("Failed to write entry cache {}", path.display()));
        }
    }

    file.unlock()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: i64) -> CachedEntry {
        CachedEntry {
            ts,
            key: Some(format!("m{ts}:r{ts}")),
            model: Some("claude-opus-5".into()),
            usage: UsageTokens::default(),
        }
    }

    #[test]
    fn test_resume_at_continues_when_file_grew() {
        let mut c = EntryCache::default();
        let p = Path::new("/x/a.jsonl");
        c.record(p, 100, vec![entry(1)]);
        assert_eq!(c.resume_at(p, 500), 100);
        assert_eq!(
            c.entries_for(p)
                .len(),
            1
        );
    }

    /// A transcript that shrank cannot be the same file extended.
    #[test]
    fn test_resume_at_restarts_when_file_shrank() {
        let mut c = EntryCache::default();
        let p = Path::new("/x/a.jsonl");
        c.record(p, 100, vec![entry(1)]);
        assert_eq!(c.resume_at(p, 50), 0);
        assert!(
            c.entries_for(p)
                .is_empty()
        );
    }

    #[test]
    fn test_prune_drops_old_entries_and_empty_files() {
        let mut c = EntryCache::default();
        let a = Path::new("/x/a.jsonl");
        let b = Path::new("/x/b.jsonl");
        c.record(a, 10, vec![entry(100), entry(900)]);
        c.record(b, 10, vec![entry(100)]);

        c.prune(500);

        assert_eq!(
            c.entries_for(a)
                .len(),
            1
        );
        assert!(
            c.entries_for(b)
                .is_empty()
        );
        // b had nothing left, so a later render re-reads it from the start.
        assert_eq!(c.resume_at(b, 10), 0);
    }

    #[test]
    fn test_with_cache_round_trips() {
        let dir = crate::paths::test_scratch_dir("entry-cache");
        let path = cache_path(&dir);

        with_cache(&path, |c| {
            c.record(Path::new("/x/a.jsonl"), 42, vec![entry(7)]);
            ((), true)
        })
        .unwrap();

        let consumed = with_cache(&path, |c| {
            (c.resume_at(Path::new("/x/a.jsonl"), 4096), false)
        })
        .unwrap();
        assert_eq!(consumed, 42);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

use anyhow::{Context, Result};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("HOME (or USERPROFILE on Windows) not set")
}

pub fn claude_config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(PathBuf::from(dir))
    } else {
        Ok(home_dir()?.join(".claude"))
    }
}

/// Path to `.claude.json`: `$CLAUDE_CONFIG_DIR/.claude.json` when the env var is set,
/// otherwise `$HOME/.claude.json` (Claude Code stores it directly in $HOME by default).
pub fn claude_config_json_path() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(PathBuf::from(dir).join(".claude.json"))
    } else {
        Ok(home_dir()?.join(".claude.json"))
    }
}

pub fn find_claude_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if std::env::var("CLAUDE_CONFIG_DIR").is_ok() {
        let config_path = claude_config_dir()?.join("projects");
        if config_path.exists() {
            paths.push(config_path);
        }
    } else {
        let home = home_dir()?;
        let old_path = home.join(".claude/projects");
        let new_path = home.join(".config/claude/projects");

        if old_path.exists() {
            paths.push(old_path);
        }
        if new_path.exists() {
            paths.push(new_path);
        }
    }

    if paths.is_empty() {
        anyhow::bail!("No Claude data directories found");
    }

    Ok(paths)
}

/// Per-test scratch directory under `target/`, kept off `/tmp` so a predictable
/// name in a world-writable dir cannot be pre-created by another user. The pid
/// suffix keeps concurrent `cargo test` runs from sharing one directory.
#[cfg(test)]
pub fn test_scratch_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-scratch")
        .join(format!("{}-{}", name, std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn iter_jsonl_files(claude_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    iter_jsonl_files_since(claude_paths, None)
}

/// Like `iter_jsonl_files` but skips transcripts whose mtime is older than
/// `min_mtime_secs` (Unix timestamp).
///
/// The cutoff is applied per file, never per directory: a directory's mtime only
/// moves when an entry is added or removed, so a session opened hours ago and still
/// being appended to sits under a stale project mtime and would be pruned while
/// actively spending tokens.
pub fn iter_jsonl_files_since(
    claude_paths: &[PathBuf],
    min_mtime_secs: Option<i64>,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for base_path in claude_paths {
        for project_entry in fs::read_dir(base_path)
            .with_context(|| format!("Failed to read directory: {}", base_path.display()))?
        {
            let project_path = project_entry?.path();
            if !project_path.is_dir() {
                continue;
            }
            collect_project(&project_path, min_mtime_secs, &mut files);
        }
    }

    Ok(files)
}

/// Session transcripts sit directly in the project directory; a session's sub-agent
/// transcripts sit under `<session>/subagents/`.
///
/// Sub-agent trees are only descended when the session itself is within the cutoff.
/// Stat'ing every agent transcript that ever ran costs more than the whole render
/// budget, and the orchestrator appends to its own transcript whenever a sub-agent
/// reports back — so a session with stale mtime has no fresh agents beneath it.
fn collect_project(project: &Path, min_mtime_secs: Option<i64>, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(project) {
        Ok(e) => e,
        Err(e) => {
            warn_skipped(project, &e);
            return;
        }
    };

    let mut has_dirs = false;
    let mut fresh_sessions: Vec<PathBuf> = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn_skipped(project, &e);
                continue;
            }
        };
        let path = entry.path();

        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                has_dirs = true;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                warn_skipped(&path, &e);
                continue;
            }
        }

        if !is_jsonl(&path) {
            continue;
        }
        if entry_is_fresh(&entry, &path, min_mtime_secs) {
            fresh_sessions.push(path.with_extension(""));
            files.push(path);
        }
    }

    if !has_dirs {
        return;
    }

    // Reached only for sessions whose own transcript is current, so this costs nothing
    // on the stale majority and needs no extra stat to decide.
    for session in fresh_sessions {
        let subagents = session.join("subagents");
        if subagents.is_dir() {
            collect_jsonl_files(&subagents, min_mtime_secs, files);
        }
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        == Some("jsonl")
}

fn mtime_secs(meta: std::io::Result<std::fs::Metadata>) -> Option<i64> {
    meta.and_then(|m| m.modified())
        .ok()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(i64::MAX)
        })
}

fn entry_is_fresh(entry: &fs::DirEntry, path: &Path, min_mtime_secs: Option<i64>) -> bool {
    let Some(cutoff) = min_mtime_secs else {
        return true;
    };
    match mtime_secs(entry.metadata()) {
        Some(mtime) => mtime >= cutoff,
        None => {
            warn_unstatable(path);
            true
        }
    }
}

fn warn_unstatable(path: &Path) {
    if std::io::stderr().is_terminal() {
        eprintln!(
            "transcript scan: cannot read mtime of {}, treating as current",
            path.display()
        );
    }
}

/// Collect `*.jsonl` under `dir`, descending into subdirectories. Sub-agent
/// transcripts live at `<project>/<session-uuid>/subagents/agent-*.jsonl`, and their
/// tokens are billed to the same block as the orchestrator's.
/// An unreadable entry anywhere under the tree must not fail the render, so every
/// error here is reported and skipped: the cost is one missing transcript, not a
/// blank statusline.
fn collect_jsonl_files(dir: &Path, min_mtime_secs: Option<i64>, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn_skipped(dir, &e);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn_skipped(dir, &e);
                continue;
            }
        };
        let path = entry.path();

        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                warn_skipped(&path, &e);
                continue;
            }
        };

        if file_type.is_dir() {
            collect_jsonl_files(&path, min_mtime_secs, files);
            continue;
        }

        if is_jsonl(&path) && entry_is_fresh(&entry, &path, min_mtime_secs) {
            files.push(path);
        }
    }
}

pub fn warn_skipped(path: &Path, e: &std::io::Error) {
    if std::io::stderr().is_terminal() {
        eprintln!("transcript scan skipped {}: {}", path.display(), e);
    }
}

use anyhow::{Context, Result};
use std::fs;
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
            collect_jsonl_files(&project_path, min_mtime_secs, &mut files)?;
        }
    }

    Ok(files)
}

/// Collect `*.jsonl` under `dir`, descending into subdirectories. Sub-agent
/// transcripts live at `<project>/<session-uuid>/subagents/agent-*.jsonl`, and their
/// tokens are billed to the same block as the orchestrator's.
fn collect_jsonl_files(
    dir: &Path,
    min_mtime_secs: Option<i64>,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in
        fs::read_dir(dir).with_context(|| format!("Failed to read directory: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to stat {}", path.display()))?;

        if file_type.is_dir() {
            collect_jsonl_files(&path, min_mtime_secs, files)?;
            continue;
        }

        if path
            .extension()
            .and_then(|s| s.to_str())
            != Some("jsonl")
        {
            continue;
        }

        if let Some(cutoff) = min_mtime_secs {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .with_context(|| format!("Failed to read mtime of {}", path.display()))?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(i64::MAX);
            if mtime < cutoff {
                continue;
            }
        }

        files.push(path);
    }

    Ok(())
}

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}

pub fn claude_config_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(PathBuf::from(dir))
    } else {
        Ok(home_dir()?.join(".claude"))
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

pub fn iter_jsonl_files(claude_paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    iter_jsonl_files_since(claude_paths, None)
}

/// Like `iter_jsonl_files` but skips project directories whose mtime is older
/// than `min_mtime_secs` (Unix timestamp). Avoids `read_dir` on stale dirs,
/// which is the main source of latency when many projects exist.
pub fn iter_jsonl_files_since(
    claude_paths: &[PathBuf],
    min_mtime_secs: Option<i64>,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for base_path in claude_paths {
        for project_entry in fs::read_dir(base_path)
            .with_context(|| format!("Failed to read directory: {}", base_path.display()))?
        {
            let project_entry = project_entry?;
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }

            if let Some(cutoff) = min_mtime_secs {
                let mtime = project_entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .ok()
                    })
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(i64::MAX);
                if mtime < cutoff {
                    continue;
                }
            }

            for session_entry in fs::read_dir(&project_path)? {
                let session_path = session_entry?.path();
                if session_path
                    .extension()
                    .and_then(|s| s.to_str())
                    == Some("jsonl")
                {
                    files.push(session_path);
                }
            }
        }
    }

    Ok(files)
}

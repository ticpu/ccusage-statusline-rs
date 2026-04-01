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
    let mut files = Vec::new();

    for base_path in claude_paths {
        for project_entry in fs::read_dir(base_path)
            .with_context(|| format!("Failed to read directory: {}", base_path.display()))?
        {
            let project_path = project_entry?.path();
            if !project_path.is_dir() {
                continue;
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

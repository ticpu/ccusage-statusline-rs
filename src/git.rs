use crate::process::{RunError, run_bounded};
use crate::warn;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(1);

/// Branch checked out at `dir`, `HEAD` when detached.
pub fn current_branch(dir: &Path) -> Option<String> {
    branch_with(Command::new("git"), dir)
}

fn branch_with(mut git: Command, dir: &Path) -> Option<String> {
    let output = match run_bounded(
        git.arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"]),
        GIT_TIMEOUT,
    ) {
        Ok(output) => output,
        Err(RunError::Spawn(e)) => {
            warn!("git branch: git could not start: {e}");
            return None;
        }
        Err(RunError::TimedOut) => {
            warn!("git branch: git timed out in {}", dir.display());
            return None;
        }
        Err(RunError::Wait(e)) => {
            warn!("git branch: waiting for git failed: {e}");
            return None;
        }
    };

    if !output
        .status
        .success()
    {
        warn!(
            "git branch for {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();
    (!branch.is_empty()).then_some(branch)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::paths::test_scratch_dir;

    /// Hooks run tests with GIT_DIR and friends set, and user config carries hooks and
    /// identity; the scratch dir sits inside this repository, hence the ceiling.
    fn isolated_git(ceiling: &Path) -> Command {
        let mut git = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .starts_with("GIT_")
            {
                git.env_remove(key);
            }
        }
        git.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", ceiling);
        git
    }

    fn git(ceiling: &Path, dir: &Path, args: &[&str]) {
        let status = isolated_git(ceiling)
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn test_branch_follows_worktree_and_detach() {
        let root = test_scratch_dir("git-branch");
        let repo = root.join("repo");
        let worktree = root.join("wt");
        git(&root, &root, &["init", "-q", "-b", "main", "repo"]);
        git(
            &root,
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.test",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(
            &root,
            &repo,
            &["worktree", "add", "-q", "-b", "other", "../wt"],
        );

        assert_eq!(
            branch_with(isolated_git(&root), &repo).as_deref(),
            Some("main")
        );
        assert_eq!(
            branch_with(isolated_git(&root), &worktree).as_deref(),
            Some("other")
        );

        git(&root, &repo, &["checkout", "-q", "--detach"]);
        assert_eq!(
            branch_with(isolated_git(&root), &repo).as_deref(),
            Some("HEAD")
        );
    }

    #[test]
    fn test_outside_a_repository_is_none() {
        let root = test_scratch_dir("git-no-repo");
        let plain = root.join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(branch_with(isolated_git(&root), &plain), None);
    }
}

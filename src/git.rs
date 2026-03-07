use std::{io, path::Path, process::Command};

use crate::{ID, merge::{merge_conflicts, MergeAction}};

/// Outcome of a [`sync`] call.
pub struct SyncReport {
    /// Number of commits pulled from the remote.
    pub updates: usize,
    /// Short commit hash before the pull (empty if unknown).
    pub commit_before: String,
    /// Short commit hash after the pull.
    pub commit_after: String,
    /// Draw-conflict renames applied during the pull: `(original_id, renamed_to_id)`.
    pub renames: Vec<(ID, ID)>,
}

/// Returns `true` if the `git` executable is available on PATH.
pub fn git_available() -> bool {
    Command::new("git").arg("--version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

/// Runs a git command in `dir`. Returns trimmed stdout on success, trimmed stderr on failure.
pub fn git_run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git").args(args).current_dir(dir).output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Returns the name of the first configured remote, if any.
pub fn git_remote(dir: &Path) -> Option<String> {
    git_run(dir, &["remote"]).ok()
        .and_then(|s| s.lines().next().map(|l| l.to_string()))
        .filter(|s| !s.is_empty())
}

/// Returns `true` if there are uncommitted changes in `dir`.
pub fn git_has_uncommitted(dir: &Path) -> bool {
    git_run(dir, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)
}

/// Returns the current branch name, or `None` if in detached HEAD state.
pub fn git_current_branch(dir: &Path) -> Option<String> {
    git_run(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()
        .filter(|s| !s.is_empty() && s != "HEAD")
}

/// Returns `true` if the current branch has a tracking upstream.
pub fn git_has_upstream(dir: &Path) -> bool {
    git_run(dir, &["rev-parse", "--abbrev-ref", "@{u}"]).is_ok()
}

/// Returns the number of local commits not yet pushed to the upstream.
pub fn git_unpushed_count(dir: &Path) -> usize {
    git_run(dir, &["rev-list", "--count", "@{u}..HEAD"])
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Commits local changes (if any), pulls, resolves draw conflicts, and pushes.
///
/// Returns `Err` if `dir` is not a git repository, has no remote, or a git
/// step fails with a conflict that cannot be resolved automatically.
pub fn sync(dir: &Path, message: &str) -> io::Result<SyncReport> {
    if !dir.join(".git").is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound,
            format!("{} is not a git repository", dir.display())));
    }
    let remote = git_remote(dir).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no git remote configured")
    })?;

    // Step 1: commit local changes if any.
    if git_has_uncommitted(dir) {
        git_run(dir, &["add", "-A"])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git add failed: {}", e)))?;
        git_run(dir, &["commit", "-m", message])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git commit failed: {}", e)))?;
    }

    let branch   = git_current_branch(dir).unwrap_or_else(|| "main".to_string());
    let tracking = git_has_upstream(dir);

    // Step 2: pull (or fetch+merge on first push when no upstream is set yet).
    let commit_before = git_run(dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let pull_result = if tracking {
        git_run(dir, &["pull"])
    } else {
        match git_run(dir, &["fetch", &remote, &branch]) {
            Ok(_)  => git_run(dir, &["merge", &format!("{}/{}", remote, branch)]),
            Err(_) => Ok(String::new()), // remote branch doesn't exist yet
        }
    };

    let mut renames = Vec::new();
    if let Err(e) = pull_result {
        let has_conflicts = git_run(dir, &["status", "--porcelain"])
            .map(|s| s.lines().any(|l| {
                matches!(l.get(..2), Some("DD"|"AU"|"UD"|"UA"|"DU"|"AA"|"UU"))
            })).unwrap_or(false);
        if !has_conflicts {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("git pull failed: {}", e)));
        }
        let reports = merge_conflicts(dir)?;
        if reports.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("git pull failed with non-draw conflicts: {}", e)));
        }
        for report in &reports {
            for action in &report.actions {
                if let MergeAction::Renamed { original, renamed_to } = action {
                    renames.push((original.clone(), renamed_to.clone()));
                }
            }
        }
        git_run(dir, &["add", "-A"])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git add after merge failed: {}", e)))?;
        git_run(dir, &["commit", "-m", "luze sync: merge"])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git commit after merge failed: {}", e)))?;
    }

    let commit_after = git_run(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let commit_before_short = if commit_before.is_empty() { String::new() }
        else { git_run(dir, &["rev-parse", "--short", &commit_before]).unwrap_or_default() };
    let updates: usize = if commit_before.is_empty() { 0 }
        else { git_run(dir, &["rev-list", "--count", &format!("{}..HEAD", commit_before)])
                   .ok().and_then(|s| s.parse().ok()).unwrap_or(0) };

    // Step 3: push (set upstream tracking on first push).
    if tracking { git_run(dir, &["push"]) } else { git_run(dir, &["push", "-u", &remote, &branch]) }
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git push failed: {}", e)))?;

    Ok(SyncReport { updates, commit_before: commit_before_short, commit_after, renames })
}

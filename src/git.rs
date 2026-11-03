//! Thin wrapper over the `git` CLI.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow};

use crate::sync::committed;
pub use crate::sync::{Change, Remote, State, commit_message};

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    cmd
}

/// Runs git in `dir`, returning stdout on success and the trimmed stderr as the error.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = command(dir, args).output().map_err(|e| anyhow!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("git failed").trim();
        Err(anyhow!("{first}"))
    }
}

pub fn available() -> bool {
    Command::new("git").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn is_repo(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--git-dir"]).is_ok()
}

pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    run(dir, &["rev-parse", "--show-toplevel"]).ok().map(|s| PathBuf::from(s.trim()))
}

fn remotes(root: &Path) -> Vec<String> {
    run(root, &["remote"]).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

fn has_upstream(root: &Path) -> bool {
    run(root, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).is_ok()
}

pub fn changes(root: &Path) -> usize {
    run(root, &["status", "--porcelain", "--", "."]).map(|s| s.lines().count()).unwrap_or(0)
}

/// Local view of the sync state, without touching the network.
pub fn state(root: &Path) -> State {
    let remote = if remotes(root).is_empty() {
        Remote::None
    } else if !has_upstream(root) {
        Remote::NoUpstream
    } else {
        let counts = run(root, &["rev-list", "--left-right", "--count", "HEAD...@{u}"]).unwrap_or_default();
        let mut it = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
        Remote::Tracking { ahead: it.next().unwrap_or(0), behind: it.next().unwrap_or(0) }
    };
    State { remote, changes: changes(root) }
}

/// Fetches, then reports state. If we are strictly behind with a clean tree,
/// fast-forwards and returns how many commits were pulled.
pub fn fetch_and_fast_forward(root: &Path) -> (State, usize) {
    if remotes(root).is_empty() {
        return (state(root), 0);
    }
    if let Err(e) = run(root, &["fetch", "-q"]) {
        return (State { remote: Remote::Offline(e.to_string()), changes: changes(root) }, 0);
    }
    let st = state(root);
    if let Remote::Tracking { ahead: 0, behind } = st.remote
        && behind > 0
        && st.changes == 0
        && run(root, &["merge", "--ff-only", "-q", "@{u}"]).is_ok()
    {
        return (state(root), behind);
    }
    (st, 0)
}

/// Parses `git diff --name-status -z` output.
pub fn parse_name_status(z: &str) -> Vec<Change> {
    let mut out = Vec::new();
    let mut it = z.split('\0').filter(|s| !s.is_empty());
    while let Some(status) = it.next() {
        let Some(path) = it.next() else { break };
        let path = path.to_string();
        match status.chars().next() {
            Some('A') => out.push(Change::Added(path)),
            Some('D') => out.push(Change::Deleted(path)),
            Some('R') | Some('C') => {
                let Some(to) = it.next() else { break };
                out.push(Change::Renamed(path, to.to_string()));
            }
            _ => out.push(Change::Modified(path)),
        }
    }
    out
}

fn rebase_in_progress(root: &Path) -> bool {
    ["rebase-merge", "rebase-apply"].iter().any(|d| {
        run(root, &["rev-parse", "--git-path", d])
            .map(|p| {
                let p = PathBuf::from(p.trim());
                if p.is_absolute() { p } else { root.join(p) }.exists()
            })
            .unwrap_or(false)
    })
}

/// `git add -A`, commit with a generated message, `pull --rebase`, push.
/// Returns a short human-readable summary.
pub fn sync(root: &Path, prefix: &str) -> Result<String> {
    run(root, &["add", "-A", "--", "."])?;
    let staged = run(root, &["diff", "--cached", "--name-status", "-z", "-M", "--relative"])?;
    let changes = parse_name_status(&staged);
    if !changes.is_empty() {
        run(root, &["commit", "-q", "-m", &commit_message(prefix, &changes)])?;
    }
    let committed = committed(changes.len());
    let remotes = remotes(root);
    let Some(remote) = remotes.first() else {
        return Ok(format!("No remote, {committed} locally"));
    };
    if has_upstream(root) {
        if let Err(e) = run(root, &["pull", "--rebase", "--autostash", "-q"]) {
            if rebase_in_progress(root) {
                let _ = run(root, &["rebase", "--abort"]);
                return Err(anyhow!("Sync conflict, resolve with git manually ({committed})"));
            }
            return Err(anyhow!("Pull failed: {e} ({committed})"));
        }
        run(root, &["push", "-q"]).map_err(|e| anyhow!("Push failed: {e} (commit kept locally)"))?;
    } else {
        run(root, &["push", "-q", "-u", remote, "HEAD"])
            .map_err(|e| anyhow!("Push failed: {e} (commit kept locally)"))?;
    }
    Ok(format!("Synced, {committed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_lists_files() {
        let changes = parse_name_status("M\0a.md\0A\0assets/x.png\0R100\0old.md\0new.md\0D\0gone.md\0");
        assert_eq!(
            commit_message("gition:", &changes),
            "gition: edit a.md, add assets/x.png, rename old.md -> new.md, delete gone.md"
        );
    }
}

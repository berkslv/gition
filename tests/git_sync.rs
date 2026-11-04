//! Sync against a temporary bare remote.

use std::fs;
use std::path::Path;

use gition::git::{self, Remote};
use gition::repo;

fn sh(dir: &Path, args: &[&str]) -> String {
    git::run(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn identity(dir: &Path) {
    sh(dir, &["config", "user.name", "Test"]);
    sh(dir, &["config", "user.email", "test@example.com"]);
    sh(dir, &["config", "commit.gpgsign", "false"]);
}

/// Bare remote plus two clones `a` (gition-initialized and pushed) and `b`.
fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.git");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::create_dir_all(&remote).unwrap();
    fs::create_dir_all(&a).unwrap();
    sh(&remote, &["init", "-q", "--bare", "-b", "main"]);
    sh(&a, &["init", "-q", "-b", "main"]);
    identity(&a);
    repo::init(&a, "assets").unwrap();
    sh(&a, &["remote", "add", "origin", remote.to_str().unwrap()]);

    let msg = git::sync(&a, "gition:").unwrap();
    assert!(msg.starts_with("Synced"), "{msg}");
    assert_eq!(git::state(&a).remote, Remote::Tracking { ahead: 0, behind: 0 });

    sh(tmp.path(), &["clone", "-q", remote.to_str().unwrap(), "b"]);
    identity(&b);
    (tmp, a, b)
}

#[test]
fn init_creates_files_and_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sh(dir, &["init", "-q", "-b", "main"]);
    identity(dir);
    repo::init(dir, "assets").unwrap();
    assert!(repo::has_index(dir));
    assert!(dir.join("assets/.gitkeep").exists());
    assert!(dir.join(".gitattributes").exists());
    assert!(dir.join("AGENTS.md").exists());
    gition::config::Config::load(dir).unwrap();
    assert_eq!(sh(dir, &["log", "--format=%s"]).trim(), "gition: init");
    assert_eq!(git::state(dir).remote, Remote::None);
    assert_eq!(git::state(dir).changes, 0);
}

#[test]
fn discover_needs_no_index() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = repo::dunce(tmp.path().canonicalize().unwrap());
    let found = |start: &std::path::Path| match repo::discover(start).unwrap() {
        repo::Found::Repo(p) => Ok(repo::dunce(p.canonicalize().unwrap())),
        repo::Found::NeedsInit(p) => Err(p),
    };
    assert_eq!(found(&dir), Err(dir.clone()));
    sh(&dir, &["init", "-q", "-b", "main"]);
    fs::create_dir_all(dir.join("Notes/Work")).unwrap();
    assert_eq!(found(&dir.join("Notes/Work")), Ok(dir.clone()));
    fs::write(dir.join("Notes/INDEX.md"), "# Notes\n").unwrap();
    assert_eq!(found(&dir.join("Notes/Work")), Ok(dir.join("Notes")));
}

#[test]
fn sync_without_remote_commits_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sh(dir, &["init", "-q", "-b", "main"]);
    identity(dir);
    repo::init(dir, "assets").unwrap();
    fs::write(dir.join("page.md"), "# Page\n").unwrap();
    fs::write(dir.join("INDEX.md"), "# Changed\n").unwrap();
    let msg = git::sync(dir, "gition:").unwrap();
    assert_eq!(msg, "No remote, committed 2 files locally");
    assert_eq!(sh(dir, &["log", "-1", "--format=%s"]).trim(), "gition: edit INDEX.md, add page.md");
}

#[test]
fn changes_flow_between_clones() {
    let (_tmp, a, b) = setup();
    fs::write(b.join("from-b.md"), "# From B\n").unwrap();
    git::sync(&b, "gition:").unwrap();

    let (state, pulled) = git::fetch_and_fast_forward(&a);
    assert_eq!(pulled, 1);
    assert_eq!(state.remote, Remote::Tracking { ahead: 0, behind: 0 });
    assert!(a.join("from-b.md").exists());
}

#[test]
fn no_fast_forward_with_local_changes() {
    let (_tmp, a, b) = setup();
    fs::write(b.join("from-b.md"), "x\n").unwrap();
    git::sync(&b, "gition:").unwrap();
    fs::write(a.join("local.md"), "x\n").unwrap();

    let (state, pulled) = git::fetch_and_fast_forward(&a);
    assert_eq!(pulled, 0);
    assert_eq!(state.remote, Remote::Tracking { ahead: 0, behind: 1 });
    assert_eq!(state.changes, 1);

    // Sync rebases the local commit on top and pushes.
    git::sync(&a, "gition:").unwrap();
    assert!(a.join("from-b.md").exists());
    assert_eq!(git::state(&a).remote, Remote::Tracking { ahead: 0, behind: 0 });
}

#[test]
fn conflict_aborts_rebase() {
    let (_tmp, a, b) = setup();
    fs::write(b.join("INDEX.md"), "# B wins\n").unwrap();
    git::sync(&b, "gition:").unwrap();
    fs::write(a.join("INDEX.md"), "# A wins\n").unwrap();

    let err = git::sync(&a, "gition:").unwrap_err().to_string();
    assert!(err.starts_with("Sync conflict"), "{err}");
    // Rebase was aborted: our commit is intact and the tree is clean.
    assert_eq!(fs::read_to_string(a.join("INDEX.md")).unwrap(), "# A wins\n");
    assert_eq!(git::changes(&a), 0);
    assert!(
        sh(&a, &["status"]).contains("diverged") || matches!(git::state(&a).remote, Remote::Tracking { ahead: 1, .. })
    );
}

//! The browser's working copy of a repo and the GitHub-backed [`Host`].
//!
//! `base` is the last commit we know of; `work` is the editable copy. Ctrl+S
//! writes to `work` (persisted in localStorage), Ctrl+G commits the difference
//! with the Git Data API and fast-forwards the branch.
//!
//! File contents and trees are kept in the [`Cache`]. When it has the base commit's
//! tree, a repo opens without waiting for GitHub, and the first fetch catches up.
//!
//! A repo opens once the first screen's files are here (see [`first_screen`]); the
//! rest download in the background, and stay [`Blob::Remote`] until they do. A page
//! opened before then is downloaded first ([`Host::want`]).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use gition::config::FILE_NAME;
use gition::host::{Host, Msg, Open};
use gition::repo::INDEX;
use gition::storage::{self, Blob, Files, MemStorage};
use gition::sync::{self, Remote, State};
use gition::{paste, paths};
use serde_json::{Map, Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::cache::{Cache, Tree};
use crate::github::{Api, Head, TreeEntry, status};
use crate::pictures::Pictures;

/// A commit and its files.
#[derive(Clone)]
pub struct Snapshot {
    pub commit: String,
    pub tree: String,
    pub files: Files,
    pub modes: HashMap<PathBuf, String>,
}

pub struct Ctx {
    pub api: Api,
    pub branch: String,
    pub base: RefCell<Snapshot>,
    pub work: Rc<MemStorage>,
    /// Images chosen in the file picker, waiting for the app.
    pub picked: RefCell<Vec<(String, Vec<u8>)>>,
    /// Images pasted together, waiting for the app.
    pub pasted: RefCell<Vec<Pasted>>,
    pub pictures: Pictures,
    pub cache: Cache,
    saved_version: RefCell<u64>,
    /// Blobs that aren't text, which stay remote.
    binary: RefCell<HashSet<String>>,
    /// Blobs asked for with [`Host::want`] and downloading.
    wanted: RefCell<HashSet<String>>,
    /// The background download is running.
    filling: Cell<bool>,
    /// Text blobs downloaded since opening, and still to download.
    done: Cell<usize>,
    remaining: Cell<usize>,
    /// Where to tell the app that downloads arrived.
    tx: RefCell<Option<Sender<Msg>>>,
}

/// Text blobs downloaded in one go in the background.
const BATCH: usize = 300;

/// At most this many files are downloaded before a repo opens.
const FIRST_SCREEN: usize = 100;

/// After a failed background batch, waits this long before retrying, doubling up to `RETRY_MAX_MS`.
const RETRY_MS: i32 = 5_000;
const RETRY_MAX_MS: i32 = 60_000;

/// Resolves after `ms` milliseconds.
async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Files downloaded before a repo opens, so the first screen is complete: pages at the root
/// and one folder down, and other files at the root. [`Ctx::load_first`] takes the config
/// and the home page before them.
fn first_screen(p: &Path) -> bool {
    let depth = p.components().count();
    depth == 1 || (depth == 2 && paths::is_markdown(p))
}

/// Images from one paste, as (extension, bytes).
pub type Pasted = Vec<(&'static str, Vec<u8>)>;

/// git's blob id for `data`, to reuse content we already have.
pub fn git_sha(data: &[u8]) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(format!("blob {}\0", data.len()).as_bytes());
    h.update(data);
    h.digest().to_string()
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn get_item(key: &str) -> Option<String> {
    local_storage()?.get_item(key).ok()?
}

pub fn set_item(key: &str, value: &str) -> bool {
    local_storage().is_some_and(|s| s.set_item(key, value).is_ok())
}

/// Builds a commit's snapshot from its tree, taking file contents from `blob` (by git sha).
/// Files it doesn't have stay remote.
fn snapshot(commit: &str, (tree, entries): Tree, blob: impl Fn(&str) -> Option<Vec<u8>>) -> Snapshot {
    let mut files = Files::new();
    let mut modes = HashMap::new();
    for e in entries {
        let path = PathBuf::from(&e.path);
        let blob = match blob(&e.sha) {
            Some(data) => Blob::Data(data),
            None => Blob::Remote(e.sha),
        };
        files.insert(path.clone(), blob);
        modes.insert(path, e.mode);
    }
    Snapshot { commit: commit.into(), tree, files, modes }
}

/// The entries that are loaded as text: all but images.
fn text_entries(entries: &[TreeEntry]) -> impl Iterator<Item = &TreeEntry> {
    entries.iter().filter(|e| !paths::is_image(Path::new(&e.path)))
}

/// Builds a commit's snapshot from the cache alone, with the files it has; `None` without the tree.
async fn cached_snapshot(cache: &Cache, commit: &str) -> Option<Snapshot> {
    let tree = cache.tree(commit).await?;
    let wanted: Vec<String> = text_entries(&tree.1).map(|e| e.sha.clone()).collect();
    let found = cache.blobs(&wanted).await;
    Some(snapshot(commit, tree, |sha| found.get(sha).cloned().flatten()))
}

/// Loads a commit's files. `tree` is the commit's tree sha, if known. Text blobs come
/// from `known` (by git sha), then the cache, then GitHub for those at paths `download`
/// picks; the rest stay remote, as do images. What GitHub sends goes into the cache.
pub async fn load_snapshot(
    api: &Api,
    cache: &Cache,
    commit: &str,
    tree: Option<&str>,
    known: &HashMap<String, Vec<u8>>,
    download: impl Fn(&Path) -> bool,
) -> Result<Snapshot> {
    let tree = match cache.tree(commit).await {
        Some(t) => t,
        None => {
            let (tree, entries) = api.tree(commit, tree).await?;
            cache.put_tree(commit, &tree, &entries);
            (tree, entries)
        }
    };
    let wanted: Vec<String> =
        text_entries(&tree.1).map(|e| &e.sha).filter(|s| !known.contains_key(*s)).cloned().collect();
    let mut found = cache.blobs(&wanted).await;
    let mut missing: Vec<String> = text_entries(&tree.1)
        .filter(|e| download(Path::new(&e.path)) && !known.contains_key(&e.sha) && !found.contains_key(&e.sha))
        .map(|e| e.sha.clone())
        .collect();
    missing.sort();
    missing.dedup();
    let fetched = if missing.is_empty() { HashMap::new() } else { api.texts(&missing).await? };
    // Blobs GitHub didn't send as text are binary.
    cache.put_blobs(missing.iter().map(|s| (s.as_str(), fetched.get(s).map(Vec::as_slice))));
    found.extend(fetched.into_iter().map(|(sha, data)| (sha, Some(data))));
    Ok(snapshot(commit, tree, |sha| known.get(sha).cloned().or_else(|| found.get(sha).cloned().flatten())))
}

/// Saves a commit made here to the cache: its tree, and the files that changed.
fn cache_commit(cache: &Cache, snap: &Snapshot, changed: &BTreeMap<PathBuf, Option<Blob>>) {
    let entries: Vec<TreeEntry> = snap
        .files
        .iter()
        .map(|(p, b)| TreeEntry {
            path: paths::to_slash(p),
            mode: snap.modes.get(p).cloned().unwrap_or_else(|| "100644".into()),
            sha: match b {
                Blob::Data(d) => git_sha(d),
                Blob::Remote(sha) => sha.clone(),
            },
        })
        .collect();
    cache.put_tree(&snap.commit, &snap.tree, &entries);
    let data: Vec<(String, &[u8])> = changed
        .values()
        .filter_map(|b| match b {
            Some(Blob::Data(d)) => Some((git_sha(d), d.as_slice())),
            _ => None,
        })
        .collect();
    cache.put_blobs(data.iter().map(|(sha, d)| (sha.as_str(), Some(*d))));
}

/// Makes `base` and `work` agree where one has a file's content and the other only its sha,
/// as when a download landed while `base` was being built, so that isn't seen as a change.
/// Hashes only those files.
fn settle(base: &mut Files, work: &MemStorage) {
    let mut into_work = HashMap::new();
    work.with_files(|w| {
        for (p, b) in base.iter_mut() {
            match (&*b, w.get(p)) {
                (Blob::Remote(sha), Some(Blob::Data(d))) if git_sha(d) == *sha => *b = Blob::Data(d.clone()),
                (Blob::Data(d), Some(Blob::Remote(sha))) if git_sha(d) == *sha => {
                    into_work.insert(sha.clone(), d.clone());
                }
                _ => {}
            }
        }
    });
    work.fill(&into_work);
}

/// What this browser kept of a repo: the saved state (base commit and unsynced changes),
/// and, when the cache has its tree, the base commit's files.
pub struct Local {
    pub cache: Cache,
    saved: Value,
    /// The saved branch and its base snapshot, built without the network.
    pub cached: Option<(String, Snapshot)>,
}

fn state_key(full_name: &str) -> String {
    format!("gition:repo:{full_name}")
}

impl Local {
    pub async fn load(full_name: &str) -> Local {
        forget_old_blob_caches();
        let cache = Cache::open().await;
        let saved: Value =
            get_item(&state_key(full_name)).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        let cached = match (saved["branch"].as_str(), saved["head"].as_str()) {
            (Some(branch), Some(head)) => cached_snapshot(&cache, head).await.map(|s| (branch.to_string(), s)),
            _ => None,
        };
        Local { cache, saved, cached }
    }

    /// The saved state, if it's for `branch`.
    fn saved(&self, branch: &str) -> Option<&Value> {
        (self.saved["branch"].as_str() == Some(branch)).then_some(&self.saved)
    }
}

/// Removes the blob caches older versions kept in localStorage, to give its quota back.
fn forget_old_blob_caches() {
    let Some(storage) = local_storage() else { return };
    let len = storage.length().unwrap_or(0);
    let old: Vec<String> =
        (0..len).filter_map(|i| storage.key(i).ok()?).filter(|k| k.starts_with("gition:blobs:")).collect();
    for k in old {
        let _ = storage.remove_item(&k);
    }
}

fn known_blobs(files: &[&Files]) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    for f in files {
        for blob in f.values() {
            if let Blob::Data(d) = blob {
                out.insert(git_sha(d), d.clone());
            }
        }
    }
    out
}

fn blob_json(blob: &Option<Blob>) -> Value {
    match blob {
        None => Value::Null,
        Some(Blob::Remote(sha)) => json!({ "sha": sha }),
        Some(Blob::Data(d)) => match std::str::from_utf8(d) {
            Ok(t) => json!({ "text": t }),
            Err(_) => json!({ "base64": B64.encode(d) }),
        },
    }
}

fn json_blob(v: &Value) -> Option<Option<Blob>> {
    if v.is_null() {
        return Some(None);
    }
    if let Some(t) = v["text"].as_str() {
        return Some(Some(Blob::Data(t.as_bytes().to_vec())));
    }
    if let Some(b) = v["base64"].as_str() {
        return Some(Some(Blob::Data(B64.decode(b).ok()?)));
    }
    v["sha"].as_str().map(|s| Some(Blob::Remote(s.into())))
}

impl Ctx {
    /// Opens a repo at `branch`: the last base commit we saw (or `head`) plus unsynced local changes.
    pub async fn open(api: Api, branch: String, head: Head, local: Local) -> Result<Rc<Ctx>> {
        let saved = local.saved(&branch).and_then(|s| s["head"].as_str()).filter(|c| *c != head.commit);
        let none = HashMap::new();
        let later = |_: &Path| false;
        let base = match saved {
            None => load_snapshot(&api, &local.cache, &head.commit, Some(&head.tree), &none, later).await?,
            Some(commit) => match load_snapshot(&api, &local.cache, commit, None, &none, later).await {
                Ok(s) => s,
                // The saved commit may be gone after a force-push; start from the head.
                Err(e) if status(&e) == Some(404) => {
                    load_snapshot(&api, &local.cache, &head.commit, Some(&head.tree), &none, later).await?
                }
                Err(e) => return Err(e),
            },
        };
        let ctx = Ctx::new(api, branch, base, local);
        ctx.load_first().await?;
        Ok(ctx)
    }

    /// Opens a repo at `base`, with the unsynced local changes saved for `branch`.
    pub fn new(api: Api, branch: String, mut base: Snapshot, local: Local) -> Rc<Ctx> {
        let mut work = base.files.clone();
        if let Some(changes) = local.saved(&branch).and_then(|s| s["changes"].as_object()) {
            for (p, v) in changes {
                match json_blob(v) {
                    Some(Some(b)) => work.insert(PathBuf::from(p), b),
                    Some(None) => work.remove(Path::new(p)),
                    None => None,
                };
            }
        }
        let work = Rc::new(MemStorage::new(api.repo.clone(), work));
        settle(&mut base.files, &work);
        let ctx = Rc::new(Ctx {
            work,
            api,
            branch,
            base: RefCell::new(base),
            picked: RefCell::new(Vec::new()),
            pasted: RefCell::new(Vec::new()),
            pictures: Pictures::default(),
            cache: local.cache,
            saved_version: RefCell::new(u64::MAX),
            binary: RefCell::default(),
            wanted: RefCell::default(),
            filling: Cell::new(false),
            done: Cell::new(0),
            remaining: Cell::new(0),
            tx: RefCell::new(None),
        });
        ctx.recount();
        ctx.persist();
        ctx
    }

    /// Text blobs not downloaded yet at paths `pick` picks, in path order.
    fn missing(&self, pick: impl Fn(&Path) -> bool) -> Vec<String> {
        let binary = self.binary.borrow();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut add = |files: &Files| {
            for (p, b) in files {
                if let Blob::Remote(sha) = b
                    && !paths::is_image(p)
                    && !binary.contains(sha)
                    && pick(p)
                    && seen.insert(sha.clone())
                {
                    out.push(sha.clone());
                }
            }
        };
        add(&self.base.borrow().files);
        self.work.with_files(|w| add(w));
        out
    }

    fn recount(&self) {
        self.remaining.set(self.missing(|_| true).len());
    }

    /// Downloads text blobs, from the cache or GitHub, into the base and the working copy.
    /// Files written, moved or deleted since keep what they have now.
    async fn download(&self, shas: &[String]) -> Result<()> {
        if shas.is_empty() {
            return Ok(());
        }
        let cached = self.cache.blobs(shas).await;
        let missing: Vec<String> = shas.iter().filter(|s| !cached.contains_key(*s)).cloned().collect();
        let mut blobs = if missing.is_empty() { HashMap::new() } else { self.api.texts(&missing).await? };
        // Blobs GitHub didn't send as text are binary.
        self.cache.put_blobs(missing.iter().map(|s| (s.as_str(), blobs.get(s).map(Vec::as_slice))));
        let mut binary: Vec<String> = missing.into_iter().filter(|s| !blobs.contains_key(s)).collect();
        for (sha, data) in cached {
            match data {
                Some(d) => {
                    blobs.insert(sha, d);
                }
                None => binary.push(sha),
            }
        }
        self.binary.borrow_mut().extend(binary);
        storage::fill(&mut self.base.borrow_mut().files, &blobs);
        self.work.fill(&blobs);
        let before = self.remaining.get();
        self.recount();
        self.done.set(self.done.get() + before.saturating_sub(self.remaining.get()));
        Ok(())
    }

    /// Downloads what the first screen shows: the config, the home page, then the
    /// [`first_screen`] files.
    pub async fn load_first(&self) -> Result<()> {
        let mut first = self.missing(|p| p == Path::new(FILE_NAME) || p == Path::new(INDEX));
        for sha in self.missing(first_screen) {
            if first.len() >= FIRST_SCREEN {
                break;
            }
            if !first.contains(&sha) {
                first.push(sha);
            }
        }
        self.download(&first).await
    }

    /// Downloads the remaining text files in the background, a batch at a time, telling the
    /// app after each. After an error it reports once and retries with a growing delay, so
    /// going offline doesn't leave pages half loaded. Files are filled in by sha, so it doesn't
    /// matter if the base moves meanwhile.
    pub fn fill_in_background(self: &Rc<Self>) {
        if self.filling.replace(true) {
            return;
        }
        let ctx = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut retry = RETRY_MS;
            loop {
                let missing = ctx.missing(|_| true);
                if missing.is_empty() {
                    // Files deleted or rewritten before they arrived count as done too.
                    if ctx.remaining.replace(0) > 0 {
                        ctx.notify(None);
                    }
                    break;
                }
                match ctx.download(&missing[..missing.len().min(BATCH)]).await {
                    Ok(()) => {
                        retry = RETRY_MS;
                        ctx.notify(None);
                    }
                    Err(e) => {
                        if retry == RETRY_MS {
                            ctx.notify(Some(e));
                        }
                        sleep(retry).await;
                        retry = (retry * 2).min(RETRY_MAX_MS);
                    }
                }
            }
            ctx.filling.set(false);
        });
    }

    fn notify(&self, err: Option<anyhow::Error>) {
        if let Some(tx) = &*self.tx.borrow() {
            let _ = tx.send(Msg::Downloaded(err.map(|e| e.to_string())));
        }
    }

    /// Saves the base commit and local changes, if anything changed since last time.
    pub fn persist(&self) {
        let version = self.work.version();
        if *self.saved_version.borrow() == version {
            return;
        }
        let base = self.base.borrow();
        let changes: Map<String, Value> = self
            .work
            .with_files(|work| sync::changed(&base.files, work))
            .iter()
            .map(|(p, b)| (paths::to_slash(p), blob_json(b)))
            .collect();
        let state = json!({ "branch": self.branch, "head": base.commit, "changes": changes });
        if set_item(&state_key(&self.api.full_name()), &state.to_string()) {
            *self.saved_version.borrow_mut() = version;
        }
    }

    fn changes(&self) -> usize {
        self.work.with_files(|work| sync::changed(&self.base.borrow().files, work).len())
    }

    fn set_base(&self, mut snap: Snapshot, work: Option<Files>) {
        if let Some(w) = work {
            self.work.replace(w);
        }
        settle(&mut snap.files, &self.work);
        *self.base.borrow_mut() = snap;
        self.recount();
        *self.saved_version.borrow_mut() = u64::MAX;
        self.persist();
    }

    fn state(&self, behind: usize) -> State {
        State { remote: Remote::Tracking { ahead: 0, behind }, changes: self.changes() }
    }

    async fn fetch(&self) -> (State, usize) {
        let remote = match self.api.branch_head(&self.branch).await {
            Ok(Some(r)) => r,
            Ok(None) => return (State { remote: Remote::NoUpstream, changes: self.changes() }, 0),
            Err(e) => return (State { remote: Remote::Offline(e.to_string()), changes: self.changes() }, 0),
        };
        let head = self.base.borrow().commit.clone();
        if remote.commit == head {
            return (self.state(0), 0);
        }
        let behind = self.api.behind_by(&head, &remote.commit);
        if self.changes() > 0 {
            return (self.state(behind.await.unwrap_or(1)), 0);
        }
        let version = self.work.version();
        let known = known_blobs(&[&self.base.borrow().files]);
        // New files download in the background, like when opening.
        let snap = load_snapshot(&self.api, &self.cache, &remote.commit, Some(&remote.tree), &known, |_| false);
        let (behind, snap) = futures_util::future::join(behind, snap).await;
        let behind = behind.unwrap_or(1);
        match snap {
            // Only fast-forward if nothing was edited while downloading.
            Ok(snap) if self.work.version() == version && self.changes() == 0 => {
                let files = snap.files.clone();
                self.set_base(snap, Some(files));
                (self.state(0), behind)
            }
            Ok(_) => (self.state(behind), 0),
            Err(e) => (State { remote: Remote::Offline(e.to_string()), changes: self.changes() }, 0),
        }
    }

    /// Moves the base to `remote`, replaying local changes on top (see [`sync::rebase`]).
    /// Waits for the background download: [`sync::rebase`] compares content, and a file here
    /// only by sha but downloaded for `remote` would look changed upstream.
    async fn rebase(&self, remote: &Head) -> Result<()> {
        self.recount();
        if self.remaining.get() > 0 {
            bail!("Still downloading pages, sync again in a moment");
        }
        let version = self.work.version();
        let known = known_blobs(&[&self.base.borrow().files, &self.work.files()]);
        let snap = load_snapshot(&self.api, &self.cache, &remote.commit, Some(&remote.tree), &known, |_| true).await?;
        if self.work.version() != version {
            bail!("Files changed during sync, press sync again");
        }
        let (work, conflicts) = sync::rebase(&self.base.borrow().files, &self.work.files(), &snap.files);
        self.set_base(snap, Some(work));
        if !conflicts.is_empty() {
            bail!("Sync conflict in {}: kept the GitHub version, yours is in *.conflict files", conflicts.join(", "));
        }
        Ok(())
    }

    async fn sync(&self, prefix: &str) -> Result<String> {
        for _ in 0..3 {
            let remote = self.api.branch_head(&self.branch).await?.ok_or_else(|| anyhow!("the branch is gone"))?;
            let pulled = remote.commit != self.base.borrow().commit;
            if pulled {
                self.rebase(&remote).await?;
            }
            let base = self.base.borrow().clone();
            let work = self.work.files();
            let changes = sync::diff(&base.files, &work);
            let changed = sync::changed(&base.files, &work);
            if changes.is_empty() {
                return Ok(if pulled {
                    "Pulled, nothing to commit".into()
                } else {
                    "Up to date, nothing to commit".into()
                });
            }
            let mut entries = Vec::new();
            let mut modes = base.modes.clone();
            for (p, blob) in &changed {
                let path = paths::to_slash(p);
                let mode = modes.get(p).cloned().unwrap_or_else(|| "100644".into());
                let entry = match blob {
                    None => {
                        modes.remove(p);
                        json!({ "path": path, "mode": mode, "type": "blob", "sha": null })
                    }
                    Some(Blob::Remote(sha)) => json!({ "path": path, "mode": mode, "type": "blob", "sha": sha }),
                    Some(Blob::Data(d)) => match std::str::from_utf8(d) {
                        Ok(text) => json!({ "path": path, "mode": mode, "type": "blob", "content": text }),
                        Err(_) => {
                            let sha = self.api.create_blob(d).await?;
                            json!({ "path": path, "mode": mode, "type": "blob", "sha": sha })
                        }
                    },
                };
                if blob.is_some() {
                    modes.insert(p.clone(), mode);
                }
                entries.push(entry);
            }
            let tree = self.api.create_tree(&base.tree, entries).await?;
            let message = sync::commit_message(prefix, &changes);
            let commit = self.api.create_commit(&message, &tree, &base.commit).await?;
            match self.api.update_branch(&self.branch, &commit).await {
                Ok(()) => {
                    let snap = Snapshot { commit, tree, files: work, modes };
                    cache_commit(&self.cache, &snap, &changed);
                    self.set_base(snap, None);
                    return Ok(format!("Synced, {}", sync::committed(changes.len())));
                }
                // Someone pushed in between: rebase and try again.
                Err(e) if status(&e) == Some(422) => continue,
                Err(e) => return Err(e),
            }
        }
        bail!("GitHub kept changing during sync, try again")
    }
}

/// [`Host`] for the web: GitHub for sync, new tabs for opening things.
pub struct WebHost(pub Rc<Ctx>);

impl Host for WebHost {
    fn fetch(&self, tx: Sender<Msg>) {
        let ctx = self.0.clone();
        *ctx.tx.borrow_mut() = Some(tx.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let (state, pulled) = ctx.fetch().await;
            let _ = tx.send(Msg::Fetched(state, pulled));
            ctx.fill_in_background();
        });
    }

    fn sync(&self, commit_prefix: &str, tx: Sender<Msg>) {
        let ctx = self.0.clone();
        ctx.fill_in_background();
        let prefix = commit_prefix.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let res = ctx.sync(&prefix).await.map_err(|e| e.to_string());
            let state = ctx.state(0);
            let _ = tx.send(Msg::Synced(res, state));
        });
    }

    fn changes(&self) -> usize {
        self.0.changes()
    }

    fn want(&self, path: &Path, tx: Sender<Msg>) -> bool {
        let ctx = self.0.clone();
        *ctx.tx.borrow_mut() = Some(tx);
        let sha = match ctx.work.with_files(|f| f.get(path).cloned()) {
            Some(Blob::Remote(sha)) if !paths::is_image(path) && !ctx.binary.borrow().contains(&sha) => sha,
            _ => return false,
        };
        ctx.fill_in_background();
        if ctx.wanted.borrow_mut().insert(sha.clone()) {
            wasm_bindgen_futures::spawn_local(async move {
                let res = ctx.download(std::slice::from_ref(&sha)).await;
                ctx.wanted.borrow_mut().remove(&sha);
                ctx.notify(res.err());
            });
        }
        true
    }

    fn downloading(&self) -> Option<(usize, usize)> {
        let (done, remaining) = (self.0.done.get(), self.0.remaining.get());
        (remaining > 0).then_some((done, done + remaining))
    }

    fn open(&self, target: Open) -> Result<()> {
        let url = match target {
            Open::Url(u) => u.to_string(),
            Open::File(p) => {
                let path: Vec<String> =
                    paths::to_slash(p).split('/').map(|s| js_sys::encode_uri_component(s).into()).collect();
                format!("https://github.com/{}/blob/{}/{}", self.0.api.full_name(), self.0.branch, path.join("/"))
            }
        };
        let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
        // No opener, so the opened page can't navigate the gition tab elsewhere.
        window
            .open_with_url_and_target_and_features(&url, "_blank", "noopener,noreferrer")
            .map_err(|e| anyhow!("{e:?}"))?;
        Ok(())
    }

    fn pick_image(&self) -> bool {
        let ctx = self.0.clone();
        pick_file(move |name, bytes| ctx.picked.borrow_mut().push((name, bytes))).is_ok()
    }

    fn fetch_title(&self, url: &str, tx: Sender<Msg>) {
        let url = url.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let title = crate::github::page_title(&url).await;
            let _ = tx.send(Msg::Title(url, title));
        });
    }

    fn copy(&self, text: &str) -> bool {
        // The write finishes later; browsers allow it from the pointer event that ends a drag.
        web_sys::window().is_some_and(|w| {
            let _ = w.navigator().clipboard().write_text(text);
            true
        })
    }

    fn compressible(&self, bytes: &[u8]) -> bool {
        paste::png_or_jpeg(bytes)
    }

    fn compress(&self, page: PathBuf, images: Vec<(String, Vec<u8>)>, tx: Sender<Msg>) {
        wasm_bindgen_futures::spawn_local(async move {
            let mut out = Vec::new();
            for (name, bytes) in images {
                let webp = if paste::png_or_jpeg(&bytes) { to_webp(&bytes).await } else { None };
                out.push(match webp {
                    Some(webp) => (paste::webp_name(&name), webp),
                    None => (name, bytes),
                });
            }
            let _ = tx.send(Msg::Images(page, out));
        });
    }
}

/// Lossy WebP quality, 0-1, as the native encoder's 80.
const WEBP_QUALITY: f64 = 0.8;

/// Re-encodes an image as lossy WebP with the browser's own encoder, or `None` if it can't be
/// decoded, the browser can't write WebP (Safari gives back a PNG), or it wouldn't get smaller.
async fn to_webp(bytes: &[u8]) -> Option<Vec<u8>> {
    use wasm_bindgen_futures::JsFuture;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).ok()?;
    // Applies a JPEG's EXIF rotation, which WebP doesn't keep.
    let bitmap = web_sys::window()?.create_image_bitmap_with_blob(&blob).ok()?;
    let bitmap: web_sys::ImageBitmap = JsFuture::from(bitmap).await.ok()?.unchecked_into();
    let canvas = web_sys::OffscreenCanvas::new(bitmap.width(), bitmap.height()).ok()?;
    let cx: web_sys::OffscreenCanvasRenderingContext2d = canvas.get_context("2d").ok()??.unchecked_into();
    cx.draw_image_with_image_bitmap(&bitmap, 0.0, 0.0).ok()?;
    bitmap.close();
    let opts = web_sys::ImageEncodeOptions::new();
    opts.set_type("image/webp");
    opts.set_quality(WEBP_QUALITY);
    let out = JsFuture::from(canvas.convert_to_blob_with_options(&opts).ok()?).await.ok()?;
    let out: web_sys::Blob = out.unchecked_into();
    if out.type_() != "image/webp" {
        return None;
    }
    let webp = js_sys::Uint8Array::new(&JsFuture::from(out.array_buffer()).await.ok()?).to_vec();
    (webp.len() < bytes.len()).then_some(webp)
}

/// Opens the browser's file chooser for an image.
fn pick_file(done: impl FnOnce(String, Vec<u8>) + 'static) -> Result<(), JsValue> {
    let document = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let input: web_sys::HtmlInputElement = document.create_element("input")?.unchecked_into();
    input.set_type("file");
    input.set_accept("image/*");
    let picked = input.clone();
    let done = RefCell::new(Some(done));
    let on_change = Closure::<dyn FnMut()>::new(move || {
        let Some(file) = picked.files().and_then(|f| f.get(0)) else { return };
        let Some(done) = done.borrow_mut().take() else { return };
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(buf) = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                done(file.name(), js_sys::Uint8Array::new(&buf).to_vec());
            }
        });
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    input.click();
    Ok(())
}

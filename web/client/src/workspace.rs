//! The browser's working copy of a repo and the GitHub-backed [`Host`].
//!
//! `base` is the last commit we know of; `work` is the editable copy. Ctrl+S
//! writes to `work` (persisted in localStorage), Ctrl+G commits the difference
//! with the Git Data API and fast-forwards the branch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use gition::host::{Host, Msg, Open};
use gition::storage::{Blob, Files, MemStorage};
use gition::sync::{self, Remote, State};
use gition::{paste, paths};
use serde_json::{Map, Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::github::{Api, status};
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
    saved_version: RefCell<u64>,
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

/// Loads a commit's files. Text blobs are downloaded unless `known` (by git sha)
/// has them; images stay remote.
pub async fn load_snapshot(api: &Api, commit: &str, known: &HashMap<String, Vec<u8>>) -> Result<Snapshot> {
    let (tree, entries) = api.tree(commit).await?;
    let wanted: Vec<String> = entries
        .iter()
        .filter(|e| !paths::is_image(Path::new(&e.path)) && !known.contains_key(&e.sha))
        .map(|e| e.sha.clone())
        .collect();
    let fetched = api.texts(&wanted).await?;
    let mut files = Files::new();
    let mut modes = HashMap::new();
    for e in entries {
        let path = PathBuf::from(&e.path);
        let blob = match known.get(&e.sha).or_else(|| fetched.get(&e.sha)) {
            Some(data) => Blob::Data(data.clone()),
            None => Blob::Remote(e.sha.clone()),
        };
        files.insert(path.clone(), blob);
        modes.insert(path, e.mode);
    }
    Ok(Snapshot { commit: commit.into(), tree, files, modes })
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
    fn state_key(&self) -> String {
        format!("gition:repo:{}", self.api.full_name())
    }

    fn blobs_key(&self) -> String {
        format!("gition:blobs:{}", self.api.full_name())
    }

    /// Opens a repo: the last base commit we saw (or the branch head) plus unsynced local changes.
    pub async fn open(api: Api, branch: String, head: String) -> Result<Rc<Ctx>> {
        let key = format!("gition:repo:{}", api.full_name());
        let saved: Value = get_item(&key).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        let saved = if saved["branch"].as_str() == Some(&branch) { saved } else { Value::Null };
        let base_commit = saved["head"].as_str().unwrap_or(&head).to_string();

        let cached: HashMap<String, Vec<u8>> = get_item(&format!("gition:blobs:{}", api.full_name()))
            .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, v.into_bytes()))
            .collect();
        let base = match load_snapshot(&api, &base_commit, &cached).await {
            Ok(s) => s,
            // The saved commit may be gone after a force-push; start from the head.
            Err(e) if base_commit != head && status(&e) == Some(404) => load_snapshot(&api, &head, &cached).await?,
            Err(e) => return Err(e),
        };
        let mut work = base.files.clone();
        if let Some(changes) = saved["changes"].as_object() {
            for (p, v) in changes {
                match json_blob(v) {
                    Some(Some(b)) => work.insert(PathBuf::from(p), b),
                    Some(None) => work.remove(Path::new(p)),
                    None => None,
                };
            }
        }
        let ctx = Rc::new(Ctx {
            work: Rc::new(MemStorage::new(api.repo.clone(), work)),
            api,
            branch,
            base: RefCell::new(base),
            picked: RefCell::new(Vec::new()),
            pasted: RefCell::new(Vec::new()),
            pictures: Pictures::default(),
            saved_version: RefCell::new(u64::MAX),
        });
        ctx.save_blob_cache();
        ctx.persist();
        Ok(ctx)
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
        if set_item(&self.state_key(), &state.to_string()) {
            *self.saved_version.borrow_mut() = version;
        }
    }

    /// Caches the base's text files by git sha so reloads only fetch the tree.
    fn save_blob_cache(&self) {
        let base = self.base.borrow();
        let texts: HashMap<String, &str> = base
            .files
            .values()
            .filter_map(|b| match b {
                Blob::Data(d) => Some((git_sha(d), std::str::from_utf8(d).ok()?)),
                Blob::Remote(_) => None,
            })
            .collect();
        // Best effort: a full localStorage only costs a slower reload.
        set_item(&self.blobs_key(), &serde_json::to_string(&texts).unwrap_or_default());
    }

    fn changes(&self) -> usize {
        self.work.with_files(|work| sync::changed(&self.base.borrow().files, work).len())
    }

    fn set_base(&self, snap: Snapshot, work: Option<Files>) {
        if let Some(w) = work {
            self.work.replace(w);
        }
        *self.base.borrow_mut() = snap;
        *self.saved_version.borrow_mut() = u64::MAX;
        self.save_blob_cache();
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
        if remote == head {
            return (self.state(0), 0);
        }
        let behind = self.api.behind_by(&head, &remote).await.unwrap_or(1);
        if self.changes() > 0 {
            return (self.state(behind), 0);
        }
        let version = self.work.version();
        let known = known_blobs(&[&self.base.borrow().files]);
        match load_snapshot(&self.api, &remote, &known).await {
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
    async fn rebase(&self, remote: &str) -> Result<()> {
        let version = self.work.version();
        let known = known_blobs(&[&self.base.borrow().files, &self.work.files()]);
        let snap = load_snapshot(&self.api, remote, &known).await?;
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
            let pulled = remote != self.base.borrow().commit;
            if pulled {
                self.rebase(&remote).await?;
            }
            let base = self.base.borrow().clone();
            let work = self.work.files();
            let changes = sync::diff(&base.files, &work);
            if changes.is_empty() {
                return Ok(if pulled {
                    "Pulled, nothing to commit".into()
                } else {
                    "Up to date, nothing to commit".into()
                });
            }
            let mut entries = Vec::new();
            let mut modes = base.modes.clone();
            for (p, blob) in sync::changed(&base.files, &work) {
                let path = paths::to_slash(&p);
                let mode = modes.get(&p).cloned().unwrap_or_else(|| "100644".into());
                let entry = match &blob {
                    None => {
                        modes.remove(&p);
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
                    modes.insert(p, mode);
                }
                entries.push(entry);
            }
            let tree = self.api.create_tree(&base.tree, entries).await?;
            let message = sync::commit_message(prefix, &changes);
            let commit = self.api.create_commit(&message, &tree, &base.commit).await?;
            match self.api.update_branch(&self.branch, &commit).await {
                Ok(()) => {
                    self.set_base(Snapshot { commit, tree, files: work, modes }, None);
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
        wasm_bindgen_futures::spawn_local(async move {
            let (state, pulled) = ctx.fetch().await;
            let _ = tx.send(Msg::Fetched(state, pulled));
        });
    }

    fn sync(&self, commit_prefix: &str, tx: Sender<Msg>) {
        let ctx = self.0.clone();
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

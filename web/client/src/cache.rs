//! The content cache in IndexedDB. Git objects never change, so everything is keyed
//! by sha and never invalidated:
//!
//! - `blobs`: blob sha → its bytes, shared by all repos. Text files, and images once
//!   shown. `null` marks a blob GitHub called binary, so it isn't asked for again.
//! - `trees`: commit sha → `{ tree, entries: [[path, mode, sha]] }`, as JSON.
//!
//! Everything is best effort: without IndexedDB (private windows, errors), reads
//! find nothing and writes do nothing, and files come from GitHub instead.

use std::cell::RefCell;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{EventTarget, IdbDatabase, IdbObjectStore, IdbTransaction, IdbTransactionMode};

use crate::github::TreeEntry;

const DB: &str = "gition";
const VERSION: u32 = 1;
const BLOBS: &str = "blobs";
const TREES: &str = "trees";

/// A commit's tree: its sha and its files.
pub type Tree = (String, Vec<TreeEntry>);

#[derive(Serialize, Deserialize)]
struct TreeRecord {
    tree: String,
    entries: Vec<(String, String, String)>,
}

thread_local! {
    static OPENED: RefCell<Option<Cache>> = const { RefCell::new(None) };
}

/// A handle on the cache; cheap to clone.
#[derive(Clone, Default)]
pub struct Cache(Option<IdbDatabase>);

impl Cache {
    /// Opens the cache, once per page.
    pub async fn open() -> Cache {
        if let Some(c) = OPENED.with(|o| o.borrow().clone()) {
            return c;
        }
        let cache = Cache(open_db().await);
        if cache.0.is_some() {
            persist();
        }
        OPENED.with(|o| *o.borrow_mut() = Some(cache.clone()));
        cache
    }

    fn store(&self, name: &str, mode: IdbTransactionMode) -> Option<(IdbTransaction, IdbObjectStore)> {
        let tx = self.0.as_ref()?.transaction_with_str_and_mode(name, mode).ok()?;
        let store = tx.object_store(name).ok()?;
        Some((tx, store))
    }

    /// A commit's tree, if it was cached.
    pub async fn tree(&self, commit: &str) -> Option<Tree> {
        let (tx, store) = self.store(TREES, IdbTransactionMode::Readonly)?;
        let req = store.get(&commit.into()).ok()?;
        if !settle(&tx, "complete", &["error", "abort"]).await {
            return None;
        }
        let record: TreeRecord = serde_json::from_str(&req.result().ok()?.as_string()?).ok()?;
        let entries = record.entries.into_iter().map(|(path, mode, sha)| TreeEntry { path, mode, sha }).collect();
        Some((record.tree, entries))
    }

    pub fn put_tree(&self, commit: &str, tree: &str, entries: &[TreeEntry]) {
        let record = TreeRecord {
            tree: tree.into(),
            entries: entries.iter().map(|e| (e.path.clone(), e.mode.clone(), e.sha.clone())).collect(),
        };
        let Ok(json) = serde_json::to_string(&record) else { return };
        if let Some((_, store)) = self.store(TREES, IdbTransactionMode::Readwrite) {
            let _ = store.put_with_key(&json.into(), &commit.into());
        }
    }

    /// The cached blobs among `shas`. `None` for those known to be binary; missing ones are left out.
    pub async fn blobs(&self, shas: &[String]) -> HashMap<String, Option<Vec<u8>>> {
        let mut out = HashMap::new();
        if shas.is_empty() {
            return out;
        }
        let Some((tx, store)) = self.store(BLOBS, IdbTransactionMode::Readonly) else { return out };
        // All requests go out in one transaction, and are read once it's done.
        let reqs: Vec<_> = shas.iter().filter_map(|s| Some((s, store.get(&s.into()).ok()?))).collect();
        if !settle(&tx, "complete", &["error", "abort"]).await {
            return out;
        }
        for (sha, req) in reqs {
            let Ok(v) = req.result() else { continue };
            if v.is_null() {
                out.insert(sha.clone(), None);
            } else if let Some(bytes) = v.dyn_ref::<js_sys::Uint8Array>() {
                out.insert(sha.clone(), Some(bytes.to_vec()));
            }
        }
        out
    }

    /// One blob's bytes, if they were cached.
    pub async fn blob(&self, sha: &str) -> Option<Vec<u8>> {
        self.blobs(&[sha.to_string()]).await.remove(sha).flatten()
    }

    /// Stores blobs by sha; `None` marks one as binary.
    pub fn put_blobs<'a>(&self, blobs: impl IntoIterator<Item = (&'a str, Option<&'a [u8]>)>) {
        let Some((_, store)) = self.store(BLOBS, IdbTransactionMode::Readwrite) else { return };
        for (sha, bytes) in blobs {
            let value = match bytes {
                Some(b) => js_sys::Uint8Array::from(b).into(),
                None => JsValue::NULL,
            };
            let _ = store.put_with_key(&value, &sha.into());
        }
    }
}

async fn open_db() -> Option<IdbDatabase> {
    let req = web_sys::window()?.indexed_db().ok()??.open_with_u32(DB, VERSION).ok()?;
    let upgrading = req.clone();
    let upgrade = Closure::<dyn FnMut()>::new(move || {
        let Ok(db) = upgrading.result() else { return };
        let db: IdbDatabase = db.unchecked_into();
        for name in [BLOBS, TREES] {
            if !db.object_store_names().contains(name) {
                let _ = db.create_object_store(name);
            }
        }
    });
    req.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    // `blocked`: another tab holds an older version open. Go without the cache this time.
    let ok = settle(&req, "success", &["error", "blocked"]).await;
    req.set_onupgradeneeded(None);
    let db: IdbDatabase = req.result().ok()?.dyn_into().ok().filter(|_| ok)?;
    // Let a newer version in another tab upgrade the database.
    let closing = db.clone();
    let on_change = Closure::<dyn FnMut()>::new(move || closing.close());
    db.set_onversionchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    Some(db)
}

/// Asks the browser not to evict the cache when space runs low. Best effort.
fn persist() {
    if let Some(w) = web_sys::window() {
        let _ = w.navigator().storage().persist();
    }
}

/// Waits for `target` to fire `ok` (true) or one of `fail` (false).
async fn settle(target: &EventTarget, ok: &str, fail: &[&str]) -> bool {
    let mut listeners = Vec::new();
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        for (name, value) in std::iter::once((ok, true)).chain(fail.iter().map(|&f| (f, false))) {
            let resolve = resolve.clone();
            let cb = Closure::<dyn FnMut()>::new(move || {
                let _ = resolve.call1(&JsValue::NULL, &value.into());
            });
            let _ = target.add_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
            listeners.push((name, cb));
        }
    });
    let result = JsFuture::from(promise).await;
    for (name, cb) in listeners {
        let _ = target.remove_event_listener_with_callback(name, cb.as_ref().unchecked_ref());
    }
    result.ok().and_then(|v| v.as_bool()) == Some(true)
}

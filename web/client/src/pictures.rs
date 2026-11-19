//! Images from the repo, shown as `<img>`s with object URLs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gition::storage::Blob;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlImageElement, Url};

use crate::workspace::{Ctx, git_sha};

#[derive(Default)]
pub struct Pictures {
    /// Object URLs by git blob SHA, so a file seen again isn't downloaded again. `None` while
    /// loading.
    urls: RefCell<HashMap<String, Option<String>>>,
    /// Images waiting for a load, by SHA.
    waiting: RefCell<HashMap<String, Vec<HtmlImageElement>>>,
}

impl Pictures {
    fn sha(ctx: &Ctx, path: &Path) -> Option<(String, Blob)> {
        let blob = ctx.work.with_files(|f| f.get(path).cloned())?;
        let sha = match &blob {
            Blob::Data(d) => git_sha(d),
            Blob::Remote(sha) => sha.clone(),
        };
        Some((sha, blob))
    }

    /// Fills in every `<img data-src>` under `root` that has no picture yet.
    pub fn fill(ctx: &Rc<Ctx>, root: &Element) {
        let Ok(list) = root.query_selector_all("img[data-src]:not([src])") else { return };
        for i in 0..list.length() {
            let Some(img) = list.get(i).and_then(|n| n.dyn_into::<HtmlImageElement>().ok()) else { continue };
            if let Some(path) = img.get_attribute("data-src") {
                Self::show(ctx, img, PathBuf::from(path));
            }
        }
    }

    /// Shows the repo image at `path` in `img`, loading it from the cache or GitHub first.
    pub fn show(ctx: &Rc<Ctx>, img: HtmlImageElement, path: PathBuf) {
        let Some((sha, blob)) = Self::sha(ctx, &path) else {
            let _ = img.class_list().add_1("missing");
            return;
        };
        let pics = &ctx.pictures;
        match pics.urls.borrow().get(&sha) {
            Some(Some(url)) => return img.set_src(url),
            Some(None) => return pics.waiting.borrow_mut().entry(sha).or_default().push(img),
            None => {}
        }
        pics.urls.borrow_mut().insert(sha.clone(), None);
        pics.waiting.borrow_mut().entry(sha.clone()).or_default().push(img);
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let bytes = match blob {
                Blob::Data(d) => Some(d),
                Blob::Remote(sha) => match ctx.cache.blob(&sha).await {
                    Some(b) => Some(b),
                    None => {
                        let b = ctx.api.blob(&sha).await.ok();
                        if let Some(b) = &b {
                            ctx.cache.put_blobs([(sha.as_str(), Some(b.as_slice()))]);
                        }
                        b
                    }
                },
            };
            let url = bytes.and_then(|b| {
                let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(b.as_slice()));
                let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).ok()?;
                Url::create_object_url_with_blob(&blob).ok()
            });
            let pics = &ctx.pictures;
            let waiting = pics.waiting.borrow_mut().remove(&sha).unwrap_or_default();
            match &url {
                Some(u) => waiting.iter().for_each(|img| img.set_src(u)),
                None => waiting.iter().for_each(|img| {
                    let _ = img.class_list().add_1("missing");
                }),
            }
            // A failed load is tried again the next time the image is shown.
            match url {
                Some(u) => pics.urls.borrow_mut().insert(sha, Some(u)),
                None => pics.urls.borrow_mut().remove(&sha),
            };
        });
    }
}

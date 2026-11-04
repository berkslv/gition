//! Inline images. The viewer leaves a transparent slot in the grid for each image it shows,
//! and an `<img>` under the grid shows through it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gition::storage::Blob;
use ratzilla::ratatui::layout::Rect;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlElement, HtmlImageElement, Url};

use crate::workspace::{Ctx, git_sha};

#[derive(Clone)]
struct Loaded {
    url: String,
    size: (u32, u32),
}

/// Image paths with their pixel sizes, or `None` for those that can't be shown.
pub type Sizes = Vec<(PathBuf, Option<(u32, u32)>)>;

#[derive(Default)]
pub struct Pictures {
    /// By git blob SHA, so a file seen again isn't downloaded again. `None` if it can't be shown.
    loaded: RefCell<HashMap<String, Option<Loaded>>>,
    /// Sizes that arrived since the last frame, for the app.
    arrived: RefCell<Sizes>,
    /// The `<img>` elements, reused from frame to frame.
    elements: RefCell<Vec<HtmlImageElement>>,
}

impl Pictures {
    pub fn take_arrived(&self) -> Sizes {
        std::mem::take(&mut self.arrived.borrow_mut())
    }

    fn blob(ctx: &Ctx, path: &Path) -> Option<(String, Blob)> {
        let blob = ctx.work.with_files(|f| f.get(path).cloned())?;
        let sha = match &blob {
            Blob::Data(d) => git_sha(d),
            Blob::Remote(sha) => sha.clone(),
        };
        Some((sha, blob))
    }

    /// Loads an image, downloading it first if it's still on GitHub.
    pub fn want(ctx: &Rc<Ctx>, path: PathBuf) {
        let pics = &ctx.pictures;
        let Some((sha, blob)) = Self::blob(ctx, &path) else {
            pics.arrived.borrow_mut().push((path, None));
            return;
        };
        if let Some(l) = pics.loaded.borrow().get(&sha) {
            pics.arrived.borrow_mut().push((path, l.as_ref().map(|l| l.size)));
            return;
        }
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let bytes = match blob {
                Blob::Data(d) => Some(d),
                Blob::Remote(sha) => ctx.api.blob(&sha).await.ok(),
            };
            let loaded = match bytes {
                Some(b) => load(&b).await,
                None => None,
            };
            let pics = &ctx.pictures;
            pics.arrived.borrow_mut().push((path, loaded.as_ref().map(|l| l.size)));
            pics.loaded.borrow_mut().insert(sha, loaded);
            crate::redraw();
        });
    }

    /// Puts an `<img>` under each slot of the last frame. `cell` is in CSS pixels.
    pub fn place(&self, ctx: &Ctx, shown: &[(PathBuf, Rect)], cell: (f32, f32)) {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        let (Some(layer), Some(term)) = (document.get_element_by_id("pics"), document.get_element_by_id("term")) else {
            return;
        };
        let origin = term.get_bounding_client_rect();
        let loaded = self.loaded.borrow();
        let mut elements = self.elements.borrow_mut();
        let mut n = 0;
        for (path, r) in shown {
            let Some(Some(l)) = Self::blob(ctx, path).and_then(|(sha, _)| loaded.get(&sha).cloned()) else { continue };
            if n == elements.len() {
                let Ok(img) = HtmlImageElement::new() else { break };
                let _ = layer.append_child(&img);
                elements.push(img);
            }
            let img = &elements[n];
            n += 1;
            if img.src() != l.url {
                img.set_src(&l.url);
            }
            let px = |cells: u16, size: f32| f64::from(cells) * f64::from(size);
            let style = HtmlElement::style(img.unchecked_ref());
            let _ = style.set_property("left", &format!("{}px", origin.left() + px(r.x, cell.0)));
            let _ = style.set_property("top", &format!("{}px", origin.top() + px(r.y, cell.1)));
            let _ = style.set_property("width", &format!("{}px", px(r.width, cell.0)));
            let _ = style.set_property("height", &format!("{}px", px(r.height, cell.1)));
        }
        for img in elements.drain(n..) {
            img.remove();
        }
    }
}

/// Decodes an image with the browser, which keeps it behind an object URL.
async fn load(bytes: &[u8]) -> Option<Loaded> {
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).ok()?;
    let url = Url::create_object_url_with_blob(&blob).ok()?;
    let img = HtmlImageElement::new().ok()?;
    img.set_src(&url);
    if JsFuture::from(img.decode()).await.is_err() || img.natural_width() == 0 {
        let _ = Url::revoke_object_url(&url);
        return None;
    }
    Some(Loaded { url, size: (img.natural_width(), img.natural_height()) })
}

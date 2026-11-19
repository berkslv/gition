//! Small helpers over web-sys.

use wasm_bindgen::JsCast;
use wasm_bindgen::convert::FromWasmAbi;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element, EventTarget, HtmlElement, HtmlTextAreaElement, Window};

pub fn window() -> Window {
    web_sys::window().expect("no window")
}

pub fn document() -> Document {
    window().document().expect("no document")
}

pub fn by_id<T: JsCast>(id: &str) -> Option<T> {
    document().get_element_by_id(id)?.dyn_into().ok()
}

/// Listens for `name` events on `target` for as long as the page lives.
pub fn on<E: FromWasmAbi + 'static>(target: &EventTarget, name: &str, f: impl FnMut(E) + 'static) {
    let f = Closure::<dyn FnMut(E)>::new(f);
    let _ = target.add_event_listener_with_callback(name, f.as_ref().unchecked_ref());
    f.forget();
}

/// Runs `f` once after `ms` milliseconds.
pub fn timeout(ms: i32, f: impl FnOnce() + 'static) {
    let f = Closure::once_into_js(f);
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(f.unchecked_ref(), ms);
}

/// Runs `f` every `ms` milliseconds.
pub fn every(ms: i32, f: impl FnMut() + 'static) {
    let f = Closure::<dyn FnMut()>::new(f);
    let _ = window().set_interval_with_callback_and_timeout_and_arguments_0(f.as_ref().unchecked_ref(), ms);
    f.forget();
}

/// The nearest element at or above an event's target that matches `selector`.
pub fn closest(target: Option<EventTarget>, selector: &str) -> Option<Element> {
    let el: Element = match target?.dyn_into::<Element>() {
        Ok(el) => el,
        // Text nodes aren't elements; start from the one holding them.
        Err(t) => t.dyn_into::<web_sys::Node>().ok()?.parent_element()?,
    };
    el.closest(selector).ok()?
}

pub fn data(el: &Element, name: &str) -> Option<String> {
    el.get_attribute(&format!("data-{name}"))
}

pub fn set_html(id: &str, html: &str) {
    if let Some(el) = document().get_element_by_id(id) {
        el.set_inner_html(html);
    }
}

pub fn set_text(id: &str, text: &str) {
    if let Some(el) = document().get_element_by_id(id) {
        el.set_text_content(Some(text));
    }
}

pub fn toggle_class(el: &Element, class: &str, on: bool) {
    let _ = el.class_list().toggle_with_force(class, on);
}

/// Whether the focus is in somewhere to type.
pub fn typing() -> bool {
    document().active_element().is_some_and(|e| matches!(e.tag_name().as_str(), "INPUT" | "TEXTAREA"))
}

/// Grows a textarea to fit its text, so the page scrolls instead of the box.
pub fn fit(ta: &HtmlTextAreaElement) {
    let style = HtmlElement::style(ta.unchecked_ref());
    let _ = style.set_property("height", "auto");
    let _ = style.set_property("height", &format!("{}px", ta.scroll_height()));
}

// ---- UTF-16 offsets ---------------------------------------------------------------
// The DOM counts text in UTF-16 units; Rust strings in bytes.

/// The byte offset of the UTF-16 offset `units` in `s`.
pub fn byte_at(s: &str, units: u32) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= units as usize {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}

/// The UTF-16 offset of byte `at` in `s`.
pub fn units_at(s: &str, at: usize) -> u32 {
    s[..at.min(s.len())].encode_utf16().count() as u32
}

/// The rendered text of `el` before the caret a click left there, if it's in `el`. Uses the
/// selection a click on text makes, or else asks where the point `(x, y)` falls.
pub fn text_before_click(el: &Element, x: f64, y: f64) -> Option<String> {
    let doc = document();
    let (node, offset) = match window().get_selection().ok().flatten().filter(|s| s.is_collapsed()) {
        Some(sel) if sel.anchor_node().is_some_and(|n| el.contains(Some(&n))) => {
            (sel.anchor_node()?, sel.anchor_offset())
        }
        _ => caret_at(&doc, x, y)?,
    };
    if !el.contains(Some(&node)) {
        return None;
    }
    let range = doc.create_range().ok()?;
    range.set_start(el, 0).ok()?;
    range.set_end(&node, offset).ok()?;
    Some(String::from(range.to_string()))
}

/// The text position at a point: `caretPositionFromPoint`, or Safari's `caretRangeFromPoint`.
fn caret_at(doc: &Document, x: f64, y: f64) -> Option<(web_sys::Node, u32)> {
    let call = |name: &str| -> Option<JsValue> {
        let f: js_sys::Function = js_sys::Reflect::get(doc, &name.into()).ok()?.dyn_into().ok()?;
        f.call2(doc, &x.into(), &y.into()).ok().filter(|v| !v.is_null() && !v.is_undefined())
    };
    let get = |v: &JsValue, key: &str| js_sys::Reflect::get(v, &key.into()).ok();
    if let Some(pos) = call("caretPositionFromPoint") {
        let node = get(&pos, "offsetNode")?.dyn_into().ok()?;
        return Some((node, get(&pos, "offset")?.as_f64()? as u32));
    }
    let range = call("caretRangeFromPoint")?;
    let node = get(&range, "startContainer")?.dyn_into().ok()?;
    Some((node, get(&range, "startOffset")?.as_f64()? as u32))
}

/// Icons, drawn in the text color.
pub mod icons {
    macro_rules! icon {
        ($($name:ident = $paths:literal;)*) => {$(
            pub const $name: &str = concat!(
                r#"<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">"#,
                $paths,
                "</svg>"
            );
        )*};
    }
    icon! {
        PLUS = r#"<path d="M12 5v14M5 12h14"/>"#;
        PENCIL = r#"<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/>"#;
        TRASH = r#"<path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6"/>"#;
        EXTERNAL = r#"<path d="M15 3h6v6M10 14 21 3M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>"#;
        PALETTE = r#"<circle cx="13.5" cy="6.5" r="1"/><circle cx="17.5" cy="10.5" r="1"/><circle cx="8.5" cy="7.5" r="1"/><circle cx="6.5" cy="12.5" r="1"/><path d="M12 2a10 10 0 0 0 0 20 2 2 0 0 0 2-2 2 2 0 0 1 2-2h2a4 4 0 0 0 4-4c0-5.5-4.5-10-10-10Z"/>"#;
        SUN = r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>"#;
        MOON = r#"<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z"/>"#;
        LOGOUT = r#"<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9"/>"#;
        MONITOR = r#"<rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8M12 17v4"/>"#;
    }
}

// Test runs: `FILO_SMOKE`, the pages and drives `FILO_OPEN` starts and the soak run are in smoke/drives.rs, in `dev`
// builds only. What the screens call for them is here, in every build: while a drive runs, where elements were laid
// out, the pages' scrolls and a note's share menu are recorded; otherwise, and in the packaged app, nothing is.

#[cfg(feature = "dev")]
mod drives;
#[cfg(feature = "dev")]
pub mod fixtures;
#[cfg(feature = "dev")]
pub use drives::*;

/// What a test drive draws over the window (`overlays-drive`: a menu, a tooltip), after everything else in it; nothing
/// otherwise, and in the packaged app.
pub fn overlay(window: &mut gpui::Window, cx: &mut gpui::App) -> Option<gpui::AnyElement> {
    #[cfg(feature = "dev")]
    return drives::overlay(window, cx);
    #[cfg(not(feature = "dev"))]
    {
        let _ = (window, cx);
        None
    }
}

/// `FILO_OPEN=chat-timing` and `chat-scroll-timing` (smoke/drives.rs `run_chat_timing`, `run_chat_scroll_timing`) run
/// on a copy of the user's chats: their routines, coding tasks and group turns don't run in it. Never in a build
/// without the drives.
pub fn quiet_backend() -> bool {
    cfg!(feature = "dev")
        && std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-timing" || open == "chat-scroll-timing")
}

/// A build without the drives never runs one.
#[cfg(not(feature = "dev"))]
fn driving() -> bool {
    false
}

/// Whether a view can be kept as drawn (`AnyView::cached`): not while a drive clicks through the window, as a kept
/// view's elements don't record where they are (`tracked`). The run timing a chat's scrolling keeps them, as the app
/// does.
pub fn cache_views() -> bool {
    !driving() || std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-scroll-timing")
}

/// Where the elements a drive clicks were last laid out (window coordinates), while one runs (`driving`); `tracked` is a
/// no-op otherwise.
static DRIVE_BOUNDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, (gpui::Bounds<gpui::Pixels>, std::time::Instant)>>,
> = std::sync::LazyLock::new(Default::default);

/// `element`, its bounds recorded under `id` for a Feed check to click; just `element` otherwise.
pub fn tracked(id: impl Into<String>, element: impl gpui::IntoElement) -> gpui::AnyElement {
    use gpui::{IntoElement as _, ParentElement as _};
    if !driving() {
        return element.into_any_element();
    }
    let id = id.into();
    gpui::div()
        .on_children_prepainted(move |bounds, _, _| {
            if let Some(bounds) = bounds.first() {
                DRIVE_BOUNDS.lock().unwrap().insert(id.clone(), (*bounds, std::time::Instant::now()));
            }
        })
        .child(element)
        .into_any_element()
}

/// `tracked` for an element that fills its parent's width (a text field): the wrapper fills it too.
pub fn tracked_fill(id: impl Into<String>, element: impl gpui::IntoElement) -> gpui::AnyElement {
    use gpui::{IntoElement as _, ParentElement as _, Styled as _};
    if !driving() {
        return element.into_any_element();
    }
    let id = id.into();
    gpui::div()
        .w_full()
        .on_children_prepainted(move |bounds, _, _| {
            if let Some(bounds) = bounds.first() {
                DRIVE_BOUNDS.lock().unwrap().insert(id.clone(), (*bounds, std::time::Instant::now()));
            }
        })
        .child(element)
        .into_any_element()
}

/// `tracked` for an element that takes the rest of a flex column (a web view): the wrapper grows into it, driving or
/// not, and the element fills the wrapper.
pub fn tracked_grow(id: impl Into<String>, element: impl gpui::IntoElement) -> gpui::AnyElement {
    use gpui::{IntoElement as _, ParentElement as _, Styled as _};
    let wrapper = gpui::div().flex_1().min_h_0().w_full().flex().flex_col();
    if !driving() {
        return wrapper.child(element).into_any_element();
    }
    let id = id.into();
    wrapper
        .on_children_prepainted(move |bounds, _, _| {
            if let Some(bounds) = bounds.first() {
                DRIVE_BOUNDS.lock().unwrap().insert(id.clone(), (*bounds, std::time::Instant::now()));
            }
        })
        .child(element)
        .into_any_element()
}

thread_local! {
    /// The pages' scrolls by name, for `pages-bottom-drive` (see `page_scroll`).
    static PAGE_SCROLLS: std::cell::RefCell<std::collections::HashMap<String, gpui::ScrollHandle>> = Default::default();
}

/// A scroll handle for the page `id`'s scroll while a drive runs (`pages-bottom-drive` scrolls it to the bottom); none
/// otherwise. Use with `.when_some(smoke::page_scroll("…"), |this, handle| this.track_scroll(&handle))`.
pub fn page_scroll(id: &str) -> Option<gpui::ScrollHandle> {
    driving().then(|| PAGE_SCROLLS.with(|scrolls| scrolls.borrow_mut().entry(id.to_string()).or_default().clone()))
}

/// While a drive runs, an empty last line for the page `id`'s column: where its content really ends, which the
/// column's own end, its bottom padding after it, should follow (`pages-bottom-drive`).
pub fn page_end(id: &str) -> Option<gpui::AnyElement> {
    driving().then(|| tracked(format!("{id}-content-end"), gpui::div()))
}

/// `page_scroll` for a page that tracks its scroll already.
pub fn keep_page_scroll(id: &str, handle: &gpui::ScrollHandle) {
    if driving() {
        PAGE_SCROLLS.with(|scrolls| scrolls.borrow_mut().insert(id.to_string(), handle.clone()));
    }
}

/// The items a note's share menu was built with, in order (submenus by their label, then their items).
static NOTE_SHARE_ITEMS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Notes the share menu's items as `screens/notes.rs` builds them, while the note-share check runs.
pub fn note_share_menu_item(label: &str) {
    if driving() {
        NOTE_SHARE_ITEMS.lock().unwrap().push(label.to_string());
    }
}

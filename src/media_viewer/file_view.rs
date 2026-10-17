// A file the viewer shows by itself (file_preview::open), where a picture would be, as filo.im's shared result page
// shows one (its picture-viewer.ts and `filePreview`): a page as tall as the room and up to `PAGE_WIDTH` across, the
// file's name, kind and size in the bar, and Save and Open in its app beside them. Markdown is drawn by the app's
// markdown view in a page's style (untrusted, as a coding result's report is: nothing in it loads from elsewhere),
// JSON laid out and any other text in the monospace font from Appearance, the first `PREVIEW_MAX` of it, a piece at a
// time as it scrolls. A PDF is WebKit's own viewer, and an HTML page runs as an artifact's does (artifact.rs
// `sandboxed_page`: a frame with scripts and no origin, no network, its links in the browser), the pictures and
// videos beside it from their folder (artifact_media.rs). Those two are a web view, a native view that takes the
// keyboard once it's clicked, so while one shows, Esc is watched for in AppKit (`EscWatch`) and closes the viewer
// from there too.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::StreamExt as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    MouseButton, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
    list, prelude::FluentBuilder as _, px,
};
use gpui_component::WindowExt as _;
use gpui_component::text::TextView;
use gpui_component::webview::WebView;
use gpui_component::wry;

use super::{Layout, MediaViewer};
use crate::file_preview::{self, Item, Preview};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};

/// How much of a text file shows (filo.im's `PREVIEW_MAX`); Open in its app has all of it.
const PREVIEW_MAX: usize = 2 * 1024 * 1024;
/// The page's width at most, as a page reads well.
const PAGE_WIDTH: f32 = 980.;
/// A text file is drawn this many lines to a piece, only the pieces on screen.
const PIECE_LINES: usize = 50;
/// A line longer than this is drawn as several, so one minified line isn't laid out whole.
const LONG_LINE: usize = 2000;

/// The shown file: what it is, how big, and how far it has come.
pub(super) struct FileView {
    preview: Preview,
    /// Once it's here.
    size: Option<u64>,
    state: State,
}

enum State {
    Loading {
        _load: Task<()>,
    },
    Markdown {
        id: SharedString,
        text: SharedString,
        cut: bool,
    },
    /// The text in pieces; the list has one item more when it was cut, which says so.
    Text {
        pieces: Rc<Vec<SharedString>>,
        list: ListState,
    },
    /// A PDF or an HTML page. The watch and the links' task go with the web view.
    Web {
        web: Entity<WebView>,
        _esc: Option<EscWatch>,
        _tasks: Vec<Task<()>>,
    },
    /// Not text after all.
    Unreadable,
    Failed(SharedString),
}

/// The file read off the main thread.
enum Read {
    Markdown(String, bool),
    Text(String, bool),
    Html(String, PathBuf),
    Pdf(PathBuf),
    Unreadable,
}

impl FileView {
    /// Downloads `item` (file_preview's cache) and reads it; it shows once it's here.
    pub(super) fn load(item: &Item, window: &mut Window, cx: &mut Context<MediaViewer>) -> FileView {
        let preview = file_preview::preview_of(item).unwrap_or(Preview::Text);
        let saving = file_preview::download(item);
        let load = cx.spawn_in(window, async move |this, cx| {
            let read = match saving.await {
                Ok(path) => cx.background_executor().spawn(async move { read(&path, preview) }).await,
                Err(error) => Err(error),
            };
            this.update_in(cx, |viewer, window, cx| {
                let (size, state) = match read {
                    Ok((size, read)) => (Some(size), shown(read, window, cx)),
                    Err(error) => (None, State::Failed(format!("Couldn’t load the file: {error}").into())),
                };
                if let Some(file) = viewer.file.as_mut() {
                    file.size = size;
                    file.state = state;
                }
                cx.notify();
            })
            .ok();
        });
        FileView { preview, size: None, state: State::Loading { _load: load } }
    }

    /// What the bar says under the name: its kind, and its size once it's here.
    pub(super) fn place(&self) -> String {
        let kind = match self.preview {
            Preview::Markdown => "Markdown",
            Preview::Json => "JSON",
            Preview::Html => "Web page",
            Preview::Pdf => "PDF",
            Preview::Text => "Text",
        };
        match self.size {
            Some(size) => format!("{kind} · {}", crate::format::file_size(size as f64)),
            None => kind.to_string(),
        }
    }

    /// What it shows, for test drives.
    #[cfg(feature = "dev")]
    pub(super) fn smoke_state(&self, cx: &gpui::App) -> String {
        let state = match &self.state {
            State::Loading { .. } => "loading".to_string(),
            State::Markdown { text, cut, .. } => format!("markdown chars={} cut={cut}", text.len()),
            State::Text { pieces, list } => format!(
                "text pieces={} first={:?} cut={}",
                pieces.len(),
                pieces.first().map(|piece| piece.lines().take(3).collect::<Vec<_>>()),
                list.item_count() > pieces.len()
            ),
            State::Web { web, .. } => format!("web visible={}", web.read(cx).visible()),
            State::Unreadable => "unreadable".to_string(),
            State::Failed(error) => format!("failed({error})"),
        };
        format!(" file={:?} {state} place={:?}", self.preview, self.place())
    }

    /// The page in the room, as wide as reads well.
    pub(super) fn render(
        &self,
        item: &Item,
        layout: &Layout,
        window: &mut Window,
        cx: &mut Context<MediaViewer>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let width = layout.room.0.min(PAGE_WIDTH);
        let note = |text: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(space::XL))
                .callout()
                .text_color(colors.text_secondary)
                .child(text)
        };
        let cut_note = move || {
            div().px(px(space::XL)).pb(px(space::XXL)).footnote().text_color(colors.text_secondary).child(format!(
                "Only its first {} are shown. Open it in its app to read it all.",
                crate::format::file_size(PREVIEW_MAX as f64)
            ))
        };
        let content = match &self.state {
            State::Loading { .. } => {
                div().size_full().flex().items_center().justify_center().child(ui::spinner(28., cx)).into_any_element()
            }
            State::Failed(error) => note(error.clone()).into_any_element(),
            State::Markdown { id, text, cut } => div()
                .id("media-viewer-file-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(
                    div()
                        .w_full()
                        .max_w(px(820.))
                        .mx_auto()
                        .px(px(space::XL))
                        .pt(px(28.))
                        .pb(px(space::HUGE))
                        .child(
                            TextView::markdown(id.clone(), text.clone(), window, cx)
                                .style(ui::markdown_page_style().untrusted(true))
                                .selectable(true),
                        )
                        .when(*cut, |this| this.child(cut_note().px_0().pt(px(space::LG)))),
                )
                .into_any_element(),
            State::Text { pieces, list: state } => {
                let (family, size) = (cx.theme().mono_family(), cx.theme().mono_size());
                let (pieces, last) = (pieces.clone(), pieces.len().saturating_sub(1));
                list(state.clone(), move |index, _, _| {
                    let Some(piece) = pieces.get(index) else { return cut_note().into_any_element() };
                    div()
                        .w_full()
                        .px(px(space::XL))
                        .when(index == 0, |this| this.pt(px(space::LG)))
                        .when(index == last, |this| this.pb(px(space::XXXL)))
                        .font_family(family.clone())
                        .text_size(px(size))
                        .line_height(px((size * 1.5).round()))
                        .text_color(colors.text)
                        .child(piece.clone())
                        .into_any_element()
                })
                .size_full()
                .into_any_element()
            }
            State::Web { web, .. } => div().size_full().child(web.clone()).into_any_element(),
            State::Unreadable => {
                let item = item.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(space::XS))
                    .p(px(space::XL))
                    .child(
                        div()
                            .size(px(44.))
                            .mb(px(space::SM))
                            .rounded(px(radius::SM))
                            .bg(colors.surface_strong)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::icon_sized("file-text", 22., colors.text_secondary)),
                    )
                    .child(div().headline().text_color(colors.text).child(SharedString::from(item.name.clone())))
                    .child(div().footnote().text_color(colors.text_secondary).child("This file can’t be shown here."))
                    .child(
                        div().mt(px(space::MD)).child(
                            Button::secondary("media-viewer-file-open", "Open in its app")
                                .small()
                                .icon("external-link")
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    file_preview::open_in_app(item.clone(), cx);
                                }),
                        ),
                    )
                    .into_any_element()
            }
        };
        div()
            .id("media-viewer-file")
            .absolute()
            .left(px(layout.middle().0 - width / 2.))
            .top(px(layout.origin.1))
            .w(px(width))
            .h(px(layout.room.1))
            .rounded(px(radius::MD))
            .overflow_hidden()
            .bg(colors.background)
            .shadow_lg()
            // A click on the page isn't one on the backdrop, which closes the viewer.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(content)
            .into_any_element()
    }
}

/// Reads the file at `path` as `preview` shows it, and gives its size.
fn read(path: &Path, preview: Preview) -> anyhow::Result<(u64, Read)> {
    use std::io::Read as _;
    let size = std::fs::metadata(path)?.len();
    match preview {
        Preview::Pdf => return Ok((size, Read::Pdf(path.to_path_buf()))),
        Preview::Html => {
            return Ok((
                size,
                Read::Html(String::from_utf8_lossy(&std::fs::read(path)?).into_owned(), path.to_path_buf()),
            ));
        }
        _ => {}
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(PREVIEW_MAX as u64).read_to_end(&mut bytes)?;
    if !file_preview::looks_like_text(&bytes) {
        return Ok((size, Read::Unreadable));
    }
    let cut = size > PREVIEW_MAX as u64;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // Cut inside a character: up to the last whole one.
        Err(error) => {
            let whole = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(whole);
            String::from_utf8(bytes).unwrap_or_default()
        }
    };
    let text = text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text);
    Ok((
        size,
        match preview {
            Preview::Markdown => Read::Markdown(text, cut),
            Preview::Json if !cut => Read::Text(indent_json(&text).unwrap_or(text), false),
            _ => Read::Text(text, cut),
        },
    ))
}

/// What the viewer shows for the file read.
fn shown(read: Read, window: &mut Window, cx: &mut Context<MediaViewer>) -> State {
    static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    match read {
        Read::Markdown(text, cut) => {
            let load = LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            State::Markdown { id: format!("media-viewer-markdown-{load}").into(), text: text.into(), cut }
        }
        Read::Text(text, cut) => {
            let pieces = pieces(&text);
            let list = ListState::new(pieces.len() + usize::from(cut), ListAlignment::Top, px(1000.));
            State::Text { pieces: Rc::new(pieces), list }
        }
        Read::Html(document, path) => web_view(Page::Html(document, path), window, cx),
        Read::Pdf(path) => web_view(Page::Pdf(path), window, cx),
        Read::Unreadable => State::Unreadable,
    }
}

/// `text` in pieces of `PIECE_LINES` lines, tabs as four spaces, a line past `LONG_LINE` as several.
fn pieces(text: &str) -> Vec<SharedString> {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let mut line = line.strip_suffix('\r').unwrap_or(line);
        while line.len() > LONG_LINE {
            let mut end = LONG_LINE;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            lines.push(&line[..end]);
            line = &line[end..];
        }
        lines.push(line);
    }
    lines.chunks(PIECE_LINES).map(|chunk| SharedString::from(chunk.join("\n").replace('\t', "    "))).collect()
}

/// `text` laid out two spaces a level, when it's JSON: its keys in their order and its numbers as written.
fn indent_json(text: &str) -> Option<String> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth = 0usize;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        (0..depth).for_each(|_| out.push_str("  "));
    };
    let mut chars = text.chars().peekable();
    while let Some(char) = chars.next() {
        match char {
            '"' => {
                out.push(char);
                let mut escaped = false;
                for char in chars.by_ref() {
                    out.push(char);
                    match char {
                        _ if escaped => escaped = false,
                        '\\' => escaped = true,
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '{' | '[' => {
                while chars.peek().is_some_and(|next| next.is_whitespace()) {
                    chars.next();
                }
                let close = if char == '{' { '}' } else { ']' };
                out.push(char);
                if chars.peek() == Some(&close) {
                    // Empty: on one line.
                    out.push(close);
                    chars.next();
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(char);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            char if char.is_whitespace() => {}
            char => out.push(char),
        }
    }
    Some(out)
}

/// What a web view shows: an HTML page (its text, and where it is), or a PDF.
enum Page {
    Html(String, PathBuf),
    Pdf(PathBuf),
}

/// The scheme a PDF is read from: only that file, from the web view showing it.
const FILE_SCHEME: &str = "filo-file";

/// The page in a web view of its own that keeps nothing (no cookies or storage), its links in the browser.
fn web_view(page: Page, window: &mut Window, cx: &mut Context<MediaViewer>) -> State {
    let (links, mut link_requests) = futures::channel::mpsc::unbounded::<String>();
    let (followed, popup) = (links.clone(), links);
    let builder = wry::WebViewBuilder::new()
        .with_incognito(true)
        // Where the page draws nothing, the viewer's page shows.
        .with_transparent(true)
        .with_new_window_req_handler(move |url, _| {
            let _ = popup.unbounded_send(url);
            wry::NewWindowResponse::Deny
        });
    let builder = match page {
        Page::Html(document, path) => {
            // Its pictures and videos named by a path beside it play from its folder, as an artifact's do.
            let key = format!("file-{}", folder_key(&path));
            crate::screens::artifact_media::register(&key, path.parent().map(Path::to_path_buf).into_iter().collect());
            let document = crate::screens::artifact_media::link_files(&document, &key);
            let theme = crate::screens::chat::conversation::glance::ThemeCss::new(cx);
            builder
                .with_html(crate::screens::artifact::sandboxed_page(&document, &theme))
                .with_custom_protocol(crate::screens::artifact_media::SCHEME.into(), |_, request| {
                    crate::screens::artifact_media::response(&request)
                })
                .with_navigation_handler(move |url| {
                    if url == "about:blank" || url == "about:srcdoc" {
                        return true;
                    }
                    let _ = followed.unbounded_send(url);
                    false
                })
        }
        Page::Pdf(path) => builder
            .with_custom_protocol(FILE_SCHEME.into(), move |_, _| {
                let bytes = std::fs::read(&path).ok();
                wry::http::Response::builder()
                    .status(if bytes.is_some() { 200 } else { 404 })
                    .header("Content-Type", "application/pdf")
                    .body(std::borrow::Cow::Owned(bytes.unwrap_or_default()))
                    .unwrap()
            })
            .with_url(format!("{FILE_SCHEME}://file/file.pdf"))
            .with_navigation_handler(move |url| {
                if url.starts_with(&format!("{FILE_SCHEME}:")) || url == "about:blank" {
                    return true;
                }
                let _ = followed.unbounded_send(url);
                false
            }),
    };
    let built = match builder.build_as_child(&*window) {
        Ok(built) => built,
        Err(error) => return State::Failed(format!("This file can’t be shown here: {error}").into()),
    };
    let web = cx.new(|cx| WebView::new(built, window, cx));
    // A link it follows opens in the browser; anything else it leads to, nowhere.
    let open_links = cx.spawn(async move |_, cx| {
        while let Some(url) = link_requests.next().await {
            if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                cx.update(|cx| cx.open_url(&url)).ok();
            }
        }
    });
    let (esc, mut presses) = futures::channel::mpsc::unbounded::<()>();
    let close = cx.spawn_in(window, async move |_, cx| {
        if presses.next().await.is_some() {
            cx.update(|window, cx| window.close_dialog(cx)).ok();
        }
    });
    let watch = EscWatch::install(web.read(cx), esc);
    State::Web { web, _esc: watch, _tasks: vec![open_links, close] }
}

/// A short name for the folder `path` is in.
fn folder_key(path: &Path) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(path.to_string_lossy().as_bytes())[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Esc pressed in the window while the file's web view shows, wherever the keyboard is (the web view keeps it once
/// clicked, and the app never sees its keys): an AppKit monitor on the app's key presses, which takes Esc and says so.
/// The web view also gets its corners rounded like the page around it. It stops with the web view.
pub(super) struct EscWatch {
    monitor: objc2::rc::Retained<objc2::runtime::AnyObject>,
}

impl EscWatch {
    fn install(web: &WebView, pressed: futures::channel::mpsc::UnboundedSender<()>) -> Option<EscWatch> {
        use gpui_component::wry::WebViewExtMacOS as _;
        use objc2::rc::Retained;
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};

        // NSEventMaskKeyDown; Esc's key code; ⇧ ⌃ ⌥ ⌘.
        const KEY_DOWN: u64 = 1 << 10;
        const ESCAPE: u16 = 53;
        const MODIFIERS: usize = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
        let view = web.raw().webview();
        // SAFETY: AppKit on the main thread, where GPUI renders; `view` keeps the WKWebView alive.
        unsafe {
            let _: () = msg_send![&*view, setWantsLayer: true];
            let layer: *mut AnyObject = msg_send![&*view, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: f64::from(radius::MD)];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
        }
        let watch = block2::RcBlock::new(move |event: std::ptr::NonNull<AnyObject>| -> *mut AnyObject {
            // SAFETY: AppKit calls the monitor on the main thread with a live event; `view` is alive while it's here.
            let esc = unsafe {
                let event = event.as_ref();
                let key: u16 = msg_send![event, keyCode];
                let flags: usize = msg_send![event, modifierFlags];
                let window: *mut AnyObject = msg_send![event, window];
                let ours: *mut AnyObject = msg_send![&*view, window];
                key == ESCAPE && flags & MODIFIERS == 0 && !window.is_null() && window == ours
            };
            if esc {
                let _ = pressed.unbounded_send(());
                return std::ptr::null_mut();
            }
            event.as_ptr()
        });
        // SAFETY: as above; AppKit copies the block.
        let monitor: Option<Retained<AnyObject>> =
            unsafe { msg_send![class!(NSEvent), addLocalMonitorForEventsMatchingMask: KEY_DOWN, handler: &*watch] };
        monitor.map(|monitor| EscWatch { monitor })
    }
}

impl Drop for EscWatch {
    fn drop(&mut self) {
        // SAFETY: removing a monitor AppKit gave us, on the main thread.
        unsafe {
            let _: () = objc2::msg_send![objc2::class!(NSEvent), removeMonitor: &*self.monitor];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_json_as_written() {
        let text = r#"{"b":1,"a":[1, 2.50,{}],"s":"x, {y}: \"z\"","e":[]}"#;
        assert_eq!(
            indent_json(text).unwrap(),
            "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2.50,\n    {}\n  ],\n  \"s\": \"x, {y}: \\\"z\\\"\",\n  \"e\": []\n}"
        );
        // Not JSON (comments, several values): as it is.
        assert_eq!(indent_json("{\"a\": 1} // note"), None);
        assert_eq!(indent_json("{\"a\":1}\n{\"a\":2}"), None);
    }

    #[test]
    fn text_goes_in_pieces_of_whole_lines() {
        let text = (1..=120).map(|line| format!("line {line}\r")).collect::<Vec<_>>().join("\n");
        let pieces = pieces(&text);
        assert_eq!(pieces.len(), 3);
        assert!(pieces[0].starts_with("line 1\nline 2\n"));
        assert!(pieces[1].starts_with("line 51\n"));
        assert!(pieces[2].ends_with("line 120"));
        // A minified line: several, cut on characters.
        let long = "é".repeat(LONG_LINE);
        let pieces = super::pieces(&long);
        assert_eq!(pieces[0].lines().count(), 2);
        assert_eq!(pieces[0].replace('\n', ""), long);
        assert_eq!(super::pieces("a\tb")[0].as_ref(), "a    b");
    }
}

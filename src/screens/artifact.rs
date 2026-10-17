// An artifact's page: Preview,
// Source and its versions from one menu at the top right (picking a version
// shows it in place; an older one shown, the menu adds Show current version
// and Restore), Open in browser, Show in Finder and Download. Artifacts live
// on this Mac (local_backend/artifacts.rs): nothing is published until the user
// shares it. Share puts the current version at a link on filo.im that anyone
// can open (local_backend/artifact_link.rs) and copies it; the page then says
// it's shared, with Copy link, Update (once there's a newer version, at the same
// link) and Unshare, which takes it down, as a coding result's page does
// (coding_result.rs `shared_bar`). When "Share to claude.ai" is on
// (artifact_settings.rs), its own button publishes the current version to the
// user's claude.ai account with Claude Code and copies that link, and a
// shared artifact opens there too.
//
// Preview is a native web view (WKWebView) showing the artifact's `document`, the
// HTML page an agent wrote. A page with the `<!--filo:head-->` marker gets the
// app's theme and font there, as glances do. The page runs in a sandbox
// (`sandboxed_page`): a frame with scripts but no origin, no network (its CSP),
// no other pages (navigation is refused; a link opens in the browser), and a web
// view of its own that keeps no cookies or storage (incognito), so it can't reach
// the Mac's files, the app's sign-ins or anything online. A page's
// `filo.chat(text)` (or the phone's `ReactNativeWebView.postMessage`) asks Filo
// something about it, once the user agrees.
//
// The web view is a native view drawn above everything in the window, so it's
// hidden whenever another mode, a confirmation or another page must show over it.
// The Preview dropdown lives in the trusted host, above its sandboxed frame,
// so the artifact stays visible and keeps its scroll position.

use gpui::{
    AnyElement, AppContext as _, Context, Corner, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, anchored, deferred,
    div, prelude::FluentBuilder as _, px,
};
use serde::Deserialize;
use serde_json::{Value, json};

use futures::StreamExt as _;
use gpui_component::webview::WebView;
use gpui_component::wry;

use super::artifact_settings::ShareSettings;
use super::coding_result::shared_bar;
use crate::backend;
use crate::screens::chat::conversation::glance::{ThemeCss, themed_page};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};

/// Source mode shows at most this much; a page's inlined scripts can run to
/// hundreds of kilobytes on one line. Download has all of it.
const SOURCE_DISPLAY_LIMIT: usize = 40_000;

/// What the artifact's web view says: a message from its page, or that it left for another page.
enum PageEvent {
    Message(String),
    Left(String),
    Ready,
    Phase(Value),
    /// A choice in the page's own menu (Preview, Source, Show current version, Restore, close).
    Menu(String),
    /// A version picked in the page's own menu.
    MenuVersion(f64),
}

/// What Share and Update ask (security review M2), from `canvasArtifacts:sharePreview`: who can see it, and what in
/// the page looks private. The page goes as it is: nothing in it is taken out.
fn link_question(preview: &Value) -> String {
    let count = |key: &str| preview[key].as_u64().unwrap_or(0);
    let mut lines = vec!["Anyone with the link can open the page and run its scripts, until you unshare it. It goes as it is: nothing in it is taken out.".to_string()];
    let mut found = Vec::new();
    match count("keys") {
        0 => {}
        1 => found.push("1 thing that looks like a key or a token".to_string()),
        keys => found.push(format!("{keys} things that look like keys or tokens")),
    }
    match count("paths") {
        0 => {}
        1 => found.push("1 path with your Mac's account name".to_string()),
        paths => found.push(format!("{paths} paths with your Mac's account name")),
    }
    match count("pictures") {
        0 => {}
        1 => found.push("1 picture inside the page".to_string()),
        pictures => found.push(format!("{pictures} pictures inside the page")),
    }
    if found.is_empty() {
        lines.push("Nothing in it looks like a key, a token or a path on this Mac.".into());
    } else {
        lines.push(format!("Found in it: {}. Check them before it goes.", found.join(", ")));
    }
    lines.join("\n\n")
}

/// The host page (`sandboxed_page`), loaded from its HTML: the only page whose messages count.
fn host_page(url: &str) -> bool {
    url == "about:blank"
}

/// Pages the web view may load: the host and the artifact in its frame. Anything else (a link, a script going
/// elsewhere, a frame of a website) is refused.
fn allowed_page(url: &str) -> bool {
    url == "about:blank" || url == "about:srcdoc"
}

/// The artifact's own policy, on top of the host's: scripts and styles only from the page itself, pictures, fonts and
/// media only inline (pictures and media also from the files next to it and the web pictures saved for it,
/// artifact_media.rs), and nothing fetched,
/// framed or submitted.
const FRAME_CSP: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'unsafe-inline' 'unsafe-eval'; \
style-src 'unsafe-inline'; img-src data: blob: filo-media:; font-src data:; media-src data: blob: filo-media:; base-uri 'none'; form-action 'none'\">";

/// In the artifact's frame: `filo.chat(text)`, the phone's `ReactNativeWebView.postMessage` and `NAV_BRIDGE`'s
/// `ipc.postMessage`, each handing its message to the host, which passes it on to the app.
const FRAME_BRIDGE: &str = "(function () { var send = function (message) { parent.postMessage({ filoBridge: String(message) }, '*'); }; \
window.ipc = { postMessage: send }; window.ReactNativeWebView = { postMessage: send }; \
window.filo = { chat: function (text) { send(JSON.stringify({ type: 'filo:chat', text: String(text) })); } }; })();";

/// `inject` at the very start of the page, after its doctype if it has one (so it keeps standards mode).
fn put_first(document: &str, inject: &str) -> String {
    let trimmed = document.trim_start();
    if trimmed.get(..9).is_some_and(|start| start.eq_ignore_ascii_case("<!doctype"))
        && let Some(end) = trimmed.find('>')
    {
        return format!("{}{inject}{}", &trimmed[..=end], &trimmed[end + 1..]);
    }
    format!("{inject}{document}")
}

/// The artifact in a sandbox: a host page with no network that frames it (`sandbox="allow-scripts allow-forms
/// allow-popups"`, no origin, so no storage, cookies or access to the host), with its own policy and the bridge put
/// first. The host passes on the frame's bridge messages and nothing else.
pub(crate) fn sandboxed_page(document: &str, theme: &ThemeCss) -> String {
    sandbox(&themed_page(document, theme), if theme.key.contains("color-scheme:dark") { "dark" } else { "light" })
}

/// The menu is trusted host UI; the artifact cannot access it through its opaque origin.
fn preview_page(document: &str, theme: &ThemeCss, visible: bool, load: u64) -> String {
    let host = sandbox_host(
        &themed_page(document, theme),
        if theme.key.contains("color-scheme:dark") { "dark" } else { "light" },
        visible,
        true,
        load,
    );
    host.replace(
        "</body>",
        &format!("<style>{}</style><script>{}</script></body>", theme.key, include_str!("../../web/artifact-menu.js")),
    )
}

fn font_response(uri: &wry::http::Uri) -> wry::http::Response<std::borrow::Cow<'static, [u8]>> {
    // An exact allowlist of embedded font bytes; never accept a filesystem path or a network URL.
    let path = match uri.to_string().as_str() {
        "filo-font://atkinson/400.ttf" => Some("fonts/AtkinsonHyperlegibleNext_400Regular.ttf"),
        "filo-font://atkinson/500.ttf" => Some("fonts/AtkinsonHyperlegibleNext_500Medium.ttf"),
        "filo-font://atkinson/600.ttf" => Some("fonts/AtkinsonHyperlegibleNext_600SemiBold.ttf"),
        _ => None,
    };
    let data = path.and_then(crate::assets::Embedded::get).map(|file| file.data);
    wry::http::Response::builder()
        .status(if data.is_some() { 200 } else { 404 })
        .header("Content-Type", "font/ttf")
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "public, max-age=31536000, immutable")
        .body(data.unwrap_or(std::borrow::Cow::Borrowed(&[])))
        .unwrap()
}

/// `sandboxed_page` for a page already themed.
fn sandbox(document: &str, scheme: &str) -> String {
    sandbox_host(document, scheme, true, false, 0)
}

/// `load` goes with each message the host sends, so a web view that loads another page can tell its messages from
/// the last page's.
fn sandbox_host(document: &str, scheme: &str, visible: bool, native_fonts: bool, load: u64) -> String {
    let tracing = artifact_tracing();
    let font_policy = if native_fonts { "font-src data: filo-font:;" } else { "font-src data:;" };
    let policy = FRAME_CSP.replace("font-src data:;", font_policy);
    let page = put_first(document, &format!("{policy}<script>{FRAME_BRIDGE}{NAV_BRIDGE}</script>"));
    // As a script string: JSON, with every "<" escaped, so nothing in it can close the host's script.
    let source = serde_json::to_string(&page).unwrap_or_else(|_| "\"\"".into()).replace('<', "\\u003c");
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline' 'unsafe-eval'; style-src 'unsafe-inline'; img-src data: blob: filo-media:; {font_policy} media-src data: blob: filo-media:; connect-src 'none'; object-src 'none'; worker-src 'none'; base-uri 'none'; form-action 'none'">
<title>Artifact</title><style>:root{{color-scheme:{scheme}}}html,body{{margin:0;height:100%;overflow:hidden;background:transparent}}iframe{{display:block;border:0;width:100%;height:100%;background:transparent}}</style></head>
<body><iframe id="a" sandbox="allow-scripts allow-forms allow-popups" title="Artifact"></iframe><script>(()=>{{const f=document.getElementById('a');
const post=window.filoHostPost=message=>{{if(window.ipc)window.ipc.postMessage(JSON.stringify(Object.assign(message,{{load:{load}}})))}};
addEventListener('message',e=>{{if(e.source!==f.contentWindow||!e.data||typeof e.data.filoBridge!=='string')return;post({{frame:e.data.filoBridge.slice(0,10000)}})}});
const phase=name=>{{if({tracing})post({{host:'phase',name,ms:performance.now()}})}};phase('host');
// Ready: two animation frames after the frame loads while the view shows. Hidden WKWebViews throttle rAF, so the
// check starts only when the native view is shown, rather than leaving a throttled callback queued that can delay
// the user's first click. A window behind others can hold frames back too: a timer reports it then.
let shown={visible};let loaded=false,reported=false,painting=false,first=0,second=0,timer=0;
const stop=()=>{{cancelAnimationFrame(first);cancelAnimationFrame(second);clearTimeout(timer);painting=false}};
const report=via=>{{if(reported)return;stop();reported=true;phase('ready-'+via);post({{host:'ready'}})}};
const paint=()=>{{if(!shown||!loaded||reported||painting)return;painting=true;timer=setTimeout(()=>report('timer'),250);
if(!document.hidden)first=requestAnimationFrame(()=>{{second=requestAnimationFrame(()=>report('frames'))}})}};
window.filoArtifactVisibility=visible=>{{shown=visible;if(!shown)stop();else paint();phase('visibility-'+visible+'-'+loaded+'-'+document.hidden+'-'+innerWidth+'x'+innerHeight)}};
document.addEventListener('visibilitychange',()=>{{if(!document.hidden){{stop();paint()}}}});
f.addEventListener('load',()=>{{phase('iframe-load');loaded=true;paint()}});
f.srcdoc={source};}})();</script></body></html>"#
    )
}

/// The web view takes the trackpad and the keyboard from the app, so the app's
/// Back and Forward (two-finger swipe, ⌘← ⌘→ ⌘[ ⌘]) are passed out from the page.
/// A swipe is wheel events until they pause; it counts only where nothing it is
/// over could scroll sideways that way, and fires once, like the app's own swipe.
pub(crate) const NAV_BRIDGE: &str = r#"(function () {
  var send = function (dir) { window.ipc.postMessage(JSON.stringify({ type: 'filo:nav', dir: dir })); };
  var travel = 0, done = false, idle = null;
  var scrollsSideways = function (node, back) {
    for (; node && node.nodeType === 1; node = node.parentElement) {
      var style = getComputedStyle(node);
      var scrolls = node === document.scrollingElement || /(auto|scroll)/.test(style.overflowX);
      if (scrolls && node.scrollWidth > node.clientWidth + 1) {
        if (back ? node.scrollLeft > 0 : node.scrollLeft + node.clientWidth < node.scrollWidth - 1) return true;
      }
    }
    return false;
  };
  window.addEventListener('wheel', function (event) {
    clearTimeout(idle);
    idle = setTimeout(function () { travel = 0; done = false; }, 180);
    if (done || event.ctrlKey || event.deltaMode !== 0) return;
    if (Math.abs(event.deltaY) > Math.abs(event.deltaX) * 2 && Math.abs(event.deltaY) > 2) { done = true; return; }
    if (!event.deltaX) return;
    if (scrollsSideways(event.target, event.deltaX < 0)) { travel = 0; return; }
    travel -= event.deltaX;
    if (Math.abs(travel) > 80) { done = true; send(travel > 0 ? 'back' : 'forward'); }
  }, { passive: true });
  window.addEventListener('keydown', function (event) {
    if (!event.metaKey || event.altKey || event.ctrlKey || event.shiftKey) return;
    var target = event.target;
    if (target && (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))) return;
    var dir = { ArrowLeft: 'back', '[': 'back', ArrowRight: 'forward', ']': 'forward' }[event.key];
    if (dir) { event.preventDefault(); send(dir); }
  }, true);
})();"#;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    #[serde(rename = "_id")]
    id: String,
    title: String,
    current_version: f64,
    opened_at: Option<f64>,
    /// The whole page to show.
    #[serde(default)]
    document: SharedString,
    /// Where its files are on this Mac, for Show in Finder.
    #[serde(default)]
    folder: Option<String>,
    /// The folder of the HTML file it's published from: the videos, sounds and pictures it names by a relative path
    /// are there (artifact_media.rs).
    #[serde(default)]
    source_folder: Option<String>,
    /// Its claude.ai link, once shared there.
    #[serde(default)]
    claude_link: Option<ClaudeLink>,
    /// Its link on filo.im, while it's shared.
    #[serde(default)]
    link: Option<ClaudeLink>,
}

/// A link it's shared at, and the version shown there.
#[derive(Clone, Debug, Deserialize)]
struct ClaudeLink {
    url: String,
    version: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionRow {
    version: f64,
    created_at: f64,
    is_current: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionDetail {
    version: f64,
    #[serde(default)]
    document: SharedString,
}

/// What's on screen: the current version or an older one.
struct Inspected {
    version: f64,
    document: SharedString,
}

#[derive(Clone, PartialEq)]
struct WebKey {
    theme: String,
    version: f64,
    // Compare content, not its length: a same-length edit must refresh the page.
    document: SharedString,
}

/// A preview's web view. Another page (a version, or the artifact the list's preloaded page moved to) loads in the
/// same web view, so only the first waits for WebKit to build one; `load` tells that page's messages from the last's.
struct Preview {
    web: Entity<WebView>,
    load: u64,
    _events: Task<()>,
}

/// Numbers each page a preview loads, across web views.
static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A page loading in a web view is drawn only once it's ready: until then the web view draws nothing (the app's
/// background shows through), so neither WebKit's blank page nor the page it showed before flashes up. It stays
/// shown to WebKit, which keeps painting it and running its animation frames.
fn set_drawn(web: &WebView, drawn: bool) {
    #[cfg(target_os = "macos")]
    {
        use gpui_component::wry::WebViewExtMacOS as _;
        let view = web.raw().webview();
        // SAFETY: AppKit on the main thread, where GPUI renders; `view` keeps the WKWebView alive.
        unsafe {
            let _: () = objc2::msg_send![&*view, setAlphaValue: if drawn { 1.0f64 } else { 0.0 }];
        }
    }
}

struct ParkedPreview {
    key: WebKey,
    preview: Preview,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Preview,
    Source,
}

pub struct ArtifactScreen {
    id: String,
    artifact: Option<Result<Option<Artifact>, String>>,
    versions: Option<Result<Vec<VersionRow>, String>>,
    version_detail: Option<Result<Option<VersionDetail>, String>>,
    version_cache: std::collections::VecDeque<VersionDetail>,
    mode: Mode,
    selected_version: Option<f64>,
    restoring: Option<f64>,
    /// What the page asked to send to Filo, until the user sends it or not (the web view hides meanwhile).
    pending_chat: Option<String>,
    /// When a link the page followed last opened in the browser: at most one a second.
    last_link: Option<std::time::Instant>,
    marked: bool,
    preview: Option<Preview>,
    /// The page `preview` shows; None while it shows nothing yet (or another artifact's page, after `retarget`).
    web_key: Option<WebKey>,
    /// One other ready preview, so switching versions preserves scroll and JS state.
    parked_preview: Option<ParkedPreview>,
    web_error: Option<String>,
    menu_open: bool,
    /// The open menu is the preview page's own (over the artifact, which keeps showing), not the app's.
    menu_in_host: bool,
    menu_right: f32,
    /// Restore's confirmation is up: the web view hides under it (a dialog draws under a native view).
    dialog_open: bool,
    /// False while another page is pushed on top (set by the shell).
    page_visible: bool,
    /// Share to claude.ai's settings: Share shows while it's on.
    share: Option<ShareSettings>,
    /// Share to claude.ai is publishing it now.
    sharing: bool,
    /// Share or Update of its filo.im link is under way.
    linking: bool,
    /// Unshare of it is.
    unlinking: bool,
    _subs: Vec<Task<()>>,
    _version_sub: Option<Task<()>>,
    opened: std::time::Instant,
    ready: bool,
}

/// Timings for `FILO_SMOKE=artifacts`, in `dev` builds only.
fn artifact_tracing() -> bool {
    cfg!(feature = "dev") && std::env::var("FILO_SMOKE").is_ok_and(|mode| mode == "artifacts")
}

fn safe_filename(value: &str) -> String {
    let mut out = String::new();
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "filo-artifact".into() } else { out }
}

impl ArtifactScreen {
    pub fn new(id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_visibility(id, true, window, cx)
    }

    pub(crate) fn preload(id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_visibility(id, false, window, cx)
    }

    fn new_with_visibility(id: String, visible: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Prepare immediately when data arrives, including while the list is still on screen.
        // Waiting for ArtifactScreen::render would keep WebKit startup on the click path.
        let mut answers = backend::backend(cx).subscribe_raw("canvasArtifacts:get", json!({ "id": id }));
        let document_sub = cx.spawn_in(window, async move |this, cx| {
            while let Some(answer) = answers.recv().await {
                let result = answer.and_then(|value| {
                    serde_json::from_value::<Option<Artifact>>(value).map_err(|error| error.to_string())
                });
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.artifact.is_none() {
                            this.trace("data");
                        }
                        this.artifact = Some(result);
                        this.mark_opened(cx);
                        if let Some(inspected) = this.inspected() {
                            this.ensure_web(&inspected, window, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let subs = vec![
            document_sub,
            backend::subscribe(
                cx,
                "canvasArtifacts:listVersions",
                json!({ "id": id, "limit": 50 }),
                |this: &mut Self, result, cx| {
                    this.versions = Some(result);
                    cx.notify();
                },
            ),
            backend::subscribe(
                cx,
                "artifactShare:settings",
                json!({}),
                |this: &mut Self, result: Result<ShareSettings, String>, cx| {
                    this.share = result.ok();
                    cx.notify();
                },
            ),
        ];
        ArtifactScreen {
            id,
            artifact: None,
            versions: None,
            version_detail: None,
            version_cache: std::collections::VecDeque::new(),
            mode: Mode::Preview,
            selected_version: None,
            restoring: None,
            pending_chat: None,
            last_link: None,
            marked: false,
            preview: None,
            web_key: None,
            parked_preview: None,
            web_error: None,
            menu_open: false,
            menu_in_host: false,
            menu_right: 110.,
            dialog_open: false,
            page_visible: visible,
            share: None,
            sharing: false,
            linking: false,
            unlinking: false,
            _subs: subs,
            _version_sub: None,
            opened: std::time::Instant::now(),
            ready: false,
        }
    }

    fn mark_opened(&mut self, cx: &mut Context<Self>) {
        if !self.page_visible || self.marked {
            return;
        }
        if let Some(artifact) = self.current().filter(|artifact| artifact.opened_at.is_none()) {
            let id = artifact.id.clone();
            self.marked = true;
            backend::fire(cx, "canvasArtifacts:markOpened", json!({ "id": id }));
        }
    }

    fn current(&self) -> Option<&Artifact> {
        match &self.artifact {
            Some(Ok(Some(artifact))) => Some(artifact),
            _ => None,
        }
    }

    fn select_version(&mut self, version: Option<f64>, cx: &mut Context<Self>) {
        let current = self.current().map(|artifact| artifact.current_version);
        let version = version.filter(|version| Some(*version) != current);
        if self.selected_version == version {
            return;
        }
        if let Some(Ok(Some(detail))) = self.version_detail.take() {
            self.version_cache.retain(|cached| cached.version != detail.version);
            self.version_cache.push_front(detail);
            self.version_cache.truncate(2);
        }
        self.selected_version = version;
        self.version_detail = version
            .and_then(|version| self.version_cache.iter().find(|detail| detail.version == version).cloned())
            .map(|detail| Ok(Some(detail)));
        self._version_sub = version.map(|version| {
            backend::subscribe(
                cx,
                "canvasArtifacts:getVersion",
                json!({ "id": self.id, "version": version }),
                |this: &mut Self, result: Result<Option<VersionDetail>, String>, cx| {
                    this.version_detail = Some(result);
                    cx.notify();
                },
            )
        });
        cx.notify();
    }

    /// None while an older version is loading.
    fn inspected(&self) -> Option<Inspected> {
        let artifact = self.current()?;
        if self.selected_version.is_some() {
            return match &self.version_detail {
                Some(Ok(Some(detail))) => {
                    Some(Inspected { version: detail.version, document: detail.document.clone() })
                }
                _ => None,
            };
        }
        Some(Inspected { version: artifact.current_version, document: artifact.document.clone() })
    }

    /// Opens the artifact's folder (each version's page, and `index.html`, the current one) in Finder.
    fn show_in_finder(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.current().and_then(|artifact| artifact.folder.clone()) else { return };
        if let Err(error) = open::that(&folder) {
            ui::toast_error(window, cx, "Couldn’t show this artifact in Finder", error.to_string());
        }
    }

    /// Share: the current version to the user's claude.ai account (Claude Code publishes it, which takes a little
    /// while), then its link on the clipboard. Shared before, the same link gets the new version.
    fn share(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sharing {
            return;
        }
        self.sharing = true;
        cx.notify();
        backend::call_in(
            window,
            cx,
            "canvasArtifacts:publish",
            json!({ "id": self.id }),
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.sharing = false;
                match result.map(|value| value["url"].as_str().map(str::to_string)) {
                    Ok(Some(url)) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                        ui::toast(window, cx, "Shared to claude.ai. Link copied");
                    }
                    Ok(None) => ui::toast_error(window, cx, "Couldn’t share it to claude.ai", "No link came back."),
                    Err(error) => ui::toast_error(window, cx, "Couldn’t share it to claude.ai", error),
                }
                cx.notify();
            },
        );
    }

    /// Share (and Update): asks first, saying who can see it and what in the page looks private
    /// (`canvasArtifacts:sharePreview`). Then `send_link`.
    fn share_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.linking || self.unlinking {
            return;
        }
        backend::call_in(
            window,
            cx,
            "canvasArtifacts:sharePreview",
            json!({ "id": self.id }),
            |_: &mut Self, result: Result<Value, String>, window, cx| {
                let preview = match result {
                    Ok(preview) => preview,
                    Err(error) => return ui::toast_error(window, cx, "Couldn’t share it", error),
                };
                let entity = cx.entity().downgrade();
                let updating = preview["updating"] == true;
                let found = ["keys", "paths"].iter().any(|key| preview[*key].as_u64().unwrap_or(0) > 0);
                let (title, ok) =
                    if updating { ("Update the shared page?", "Update") } else { ("Share this page?", "Share") };
                ui::confirm(window, cx, title, link_question(&preview), ok, "Cancel", found, move |window, cx| {
                    entity.update(cx, |this, cx| this.send_link(window, cx)).ok();
                });
            },
        );
    }

    /// The current version to its link on filo.im, then the link on the clipboard. Shared already, the same link, with
    /// this version.
    fn send_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.linking || self.unlinking {
            return;
        }
        self.linking = true;
        cx.notify();
        let updating = self.current().is_some_and(|artifact| artifact.link.is_some());
        backend::call_in(
            window,
            cx,
            "canvasArtifacts:share",
            json!({ "id": self.id }),
            move |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.linking = false;
                match result.map(|value| value["url"].as_str().map(str::to_string)) {
                    Ok(Some(url)) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                        ui::toast(
                            window,
                            cx,
                            if updating {
                                "Updated. The link shows this version. Link copied"
                            } else {
                                "Shared. Link copied"
                            },
                        );
                    }
                    Ok(None) => ui::toast_error(window, cx, "Couldn’t share it", "No link came back."),
                    Err(error) => ui::toast_error(window, cx, "Couldn’t share it", error),
                }
                cx.notify();
            },
        );
    }

    /// Unshare: the page on filo.im is deleted, so its link opens nothing any more.
    fn unshare_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.linking || self.unlinking {
            return;
        }
        self.unlinking = true;
        cx.notify();
        backend::call_in(
            window,
            cx,
            "canvasArtifacts:unshare",
            json!({ "id": self.id }),
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.unlinking = false;
                match result {
                    Ok(_) => ui::toast(window, cx, "Unshared. The link doesn’t open it any more"),
                    Err(error) => ui::toast_error(window, cx, "Couldn’t unshare it", error),
                }
                cx.notify();
            },
        );
    }

    /// The line under the header while it's shared: where, with Copy link, Update when this version isn't the one
    /// shown there, and Unshare.
    fn render_shared_bar(&self, artifact: &Artifact, cx: &mut Context<Self>) -> Option<AnyElement> {
        let link = artifact.link.as_ref()?;
        let behind = link.version != artifact.current_version;
        let headline = if behind {
            format!("Shared: the link shows version {}, not the latest", link.version as i64)
        } else {
            "Shared: anyone with the link can open it".to_string()
        };
        let update = behind.then(|| {
            let button = Button::secondary("artifact-update-link", if self.linking { "Updating…" } else { "Update" })
                .small()
                .icon("share-2")
                .loading(self.linking)
                .disabled(self.linking || self.unlinking)
                .on_click(cx.listener(|this, _, window, cx| this.share_link(window, cx)));
            crate::smoke::tracked("artifact-update-link", button)
        });
        let unshare = cx.listener(|this, _, window, cx| this.unshare_link(window, cx));
        let bar = shared_bar("artifact", &link.url, headline.into(), self.unlinking, update, unshare, cx);
        Some(div().flex_none().px(px(space::LG)).pb(px(space::SM)).child(bar).into_any_element())
    }

    fn open_claude_link(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(link) = self.current().and_then(|artifact| artifact.claude_link.clone()) else { return };
        if let Err(error) = open::that(&link.url) {
            ui::toast_error(window, cx, "Couldn’t open claude.ai", error.to_string());
        }
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(artifact), Some(inspected)) = (self.current(), self.inspected()) else { return };
        if inspected.document.is_empty() {
            return;
        }
        let filename = format!("{}.html", safe_filename(&artifact.title));
        let directory = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
        let answer = cx.prompt_for_new_path(&directory, Some(&filename));
        let source = inspected.document.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = answer.await else { return };
            let result = std::fs::write(&path, source);
            this.update_in(cx, |_, window, cx| match result {
                Ok(()) => {
                    let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
                    ui::toast(window, cx, format!("Saved {name}"));
                }
                Err(error) => ui::toast_error(window, cx, "Couldn’t save the file", error.to_string()),
            })
            .ok();
        })
        .detach();
    }

    /// Writes the preview page, sandboxed as in the app, to a temporary file and opens it in the default browser.
    fn open_in_browser(&self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(artifact), Some(inspected)) = (self.current(), self.inspected()) else { return };
        let directory = std::env::temp_dir().join("filo-artifacts");
        let path = directory.join(format!("{}-v{}.html", safe_filename(&artifact.title), inspected.version as i64));
        let result = std::fs::create_dir_all(&directory)
            .and_then(|_| std::fs::write(&path, sandboxed_page(&inspected.document, &ThemeCss::new(cx))))
            .and_then(|_| open::that(&path));
        if let Err(error) = result {
            ui::toast_error(window, cx, "Couldn’t open this artifact", error.to_string());
        }
    }

    /// Restore, from the menu with an older version shown: asks first. The web view hides while the dialog is up
    /// (it would draw under the native view) and shows again once it closes, whichever way.
    fn confirm_restore(&mut self, version: f64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(artifact) = self.current() else { return };
        if version == artifact.current_version || self.restoring.is_some() || self.dialog_open {
            return;
        }
        let message = "Filo will preserve the current work and create a new version from this snapshot.";
        let this: WeakEntity<Self> = cx.entity().downgrade();
        self.dialog_open = true;
        self.sync_web(cx);
        cx.notify();
        ui::confirm(
            window,
            cx,
            format!("Restore version {}?", version as i64),
            message,
            "Restore",
            "Cancel",
            false,
            move |window, cx| {
                this.update(cx, |this, cx| this.restore(version, window, cx)).ok();
            },
        );
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                let open = this
                    .update_in(cx, |_, window, cx| gpui_component::WindowExt::has_active_dialog(window, cx))
                    .unwrap_or(false);
                if !open {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                this.dialog_open = false;
                this.sync_web(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves the older version again as the new current one, then shows that (in the mode the user is in).
    fn restore(&mut self, version: f64, window: &mut Window, cx: &mut Context<Self>) {
        self.restoring = Some(version);
        cx.notify();
        backend::call_in(
            window,
            cx,
            "canvasArtifacts:restoreVersion",
            json!({ "id": self.id, "version": version }),
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.restoring = None;
                match result {
                    Ok(_) => this.select_version(None, cx),
                    Err(error) => ui::toast_error(
                        window,
                        cx,
                        "Couldn’t restore this version",
                        if error.is_empty() { "Please try again.".into() } else { error },
                    ),
                }
                this.sync_menu(cx);
                cx.notify();
            },
        );
    }

    /// Called by the shell: false while another page is pushed on top.
    pub fn set_page_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.page_visible != visible {
            self.page_visible = visible;
            if visible {
                self.trace("prepared-before-click");
                self.opened = std::time::Instant::now();
                self.mark_opened(cx);
            }
            if !visible {
                self.menu_open = false;
            }
            self.sync_web(cx);
            cx.notify();
        }
    }

    /// Opened again (from the list or a link): the current version in Preview, as when new. Only the page's scroll
    /// and state stay.
    pub(crate) fn reopen(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        self.pending_chat = None;
        self.mode = Mode::Preview;
        self.select_version(None, cx);
        self.sync_web(cx);
        cx.notify();
    }

    /// The list's preloaded page, moved to another artifact: it starts over for `id`, and its web view loads that
    /// artifact instead of WebKit building another.
    pub(crate) fn retarget(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.id == id {
            return;
        }
        let preview = self.preview.take().or_else(|| self.parked_preview.take().map(|parked| parked.preview));
        *self = Self::new_with_visibility(id, self.page_visible, window, cx);
        // Hidden until the new page loads in it (`web_key` is None).
        self.preview = preview;
        self.sync_web(cx);
    }

    /// Drops the web views (Filo out of sight); showing the page again loads it anew.
    pub(crate) fn release_web(&mut self, cx: &mut Context<Self>) {
        self.preview = None;
        self.parked_preview = None;
        self.web_key = None;
        self.ready = false;
        self.menu_open = false;
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.menu_open = false;
        self.sync_web(cx);
        cx.notify();
    }

    fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            // The preview page draws the menu once it's ready. Until then (still loading, or it failed), and in
            // Source, the app draws it, with the web view hidden under it.
            self.menu_in_host =
                self.mode == Mode::Preview && self.ready && self.preview.is_some() && self.pending_chat.is_none();
        }
        self.menu_open = open;
        self.sync_web(cx);
        cx.notify();
    }

    /// Shows the native web view only when nothing has to draw over it.
    fn sync_web(&self, cx: &mut Context<Self>) {
        let show = self.page_visible
            && self.mode == Mode::Preview
            && self.pending_chat.is_none()
            && !self.dialog_open
            && self.web_key.is_some()
            && (!self.menu_open || self.menu_in_host);
        if let Some(preview) = &self.preview {
            preview.web.update(cx, |web, cx| {
                let changed = show != web.visible();
                match (show, web.visible()) {
                    (true, false) => web.show(),
                    (false, true) => web.hide(),
                    _ => {}
                }
                if changed {
                    cx.notify();
                }
                let _ = web.evaluate_script(&format!("window.filoArtifactVisibility?.({show})"));
            });
        }
        self.sync_menu(cx);
    }

    /// Tells the preview page what its menu shows (web/artifact-menu.js), open or not.
    fn sync_menu(&self, cx: &mut Context<Self>) {
        if let Some(preview) = &self.preview {
            let open = self.menu_open
                && self.menu_in_host
                && self.page_visible
                && self.mode == Mode::Preview
                && self.pending_chat.is_none();
            let script = format!("window.filoArtifactMenu?.({})", self.menu_state(open));
            let _ = preview.web.read(cx).evaluate_script(&script);
        }
    }

    /// The menu's content, for the page to draw: the version shown, the versions (newest first, with when each was
    /// made) or why they aren't there yet, and whether an older one is shown (Show current version, Restore).
    fn menu_state(&self, open: bool) -> Value {
        let current = self.current().map_or(0., |artifact| artifact.current_version);
        let shown = self.selected_version.unwrap_or(current) as i64;
        let (versions, error) = match &self.versions {
            Some(Ok(rows)) => {
                let rows: Vec<Value> = rows
                    .iter()
                    .map(|row| json!({ "version": row.version as i64, "when": crate::format::day_time(row.created_at), "current": row.is_current }))
                    .collect();
                (Some(rows), None)
            }
            Some(Err(error)) => (None, Some(error.clone())),
            None => (None, None),
        };
        let more = versions.as_ref().is_some_and(|rows| current as usize > rows.len());
        json!({
            "open": open,
            "right": self.menu_right,
            "shown": shown,
            "older": self.selected_version.is_some(),
            "versions": versions,
            "error": error,
            "more": more,
            "restoring": self.restoring.is_some(),
        })
    }

    /// (Re)loads the preview when the shown version or the app's theme changes.
    fn ensure_web(&mut self, inspected: &Inspected, window: &mut Window, cx: &mut Context<Self>) {
        let theme = ThemeCss::artifact(cx);
        let key = WebKey { theme: theme.key.clone(), version: inspected.version, document: inspected.document.clone() };
        if self.web_key.as_ref() == Some(&key) {
            return;
        }
        if self.web_key.is_some() {
            self.opened = std::time::Instant::now();
        }
        // The page's own menu goes with the page it was on.
        if self.menu_in_host {
            self.menu_open = false;
        }
        let (cached, mut spare) = match self.parked_preview.take() {
            Some(parked) if parked.key == key => (Some(parked.preview), None),
            parked => (None, parked.map(|parked| parked.preview)),
        };
        // A ready page is parked, so going back to it keeps its scroll; one still loading gives its web view to the next.
        if let Some(preview) = self.preview.take() {
            preview.web.update(cx, |web, _| web.hide());
            match self.web_key.take() {
                Some(old) if self.ready => self.parked_preview = Some(ParkedPreview { key: old, preview }),
                _ => spare = Some(preview),
            }
        }
        self.web_key = Some(key);
        self.web_error = None;
        self.ready = false;
        if let Some(preview) = cached {
            self.preview = Some(preview);
            self.ready = true;
            self.sync_web(cx);
            return;
        }
        self.trace("prepare-start");
        let load = LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Files the page names next to it play from where they are, and the web pictures saved for it
        // (filo_local::artifact_images) show from its folder.
        let document = match self.current() {
            Some(artifact) => {
                let folders = [&artifact.source_folder, &artifact.folder]
                    .into_iter()
                    .flatten()
                    .map(std::path::PathBuf::from)
                    .collect();
                super::artifact_media::register(&artifact.id, folders);
                let document = match &artifact.folder {
                    Some(folder) => filo_local::artifact_images::rewrite(
                        &inspected.document,
                        std::path::Path::new(folder),
                        |file, _| {
                            Some(format!(
                                "{}://{}/{}/{file}",
                                super::artifact_media::SCHEME,
                                artifact.id,
                                filo_local::artifact_images::DIR
                            ))
                        },
                    ),
                    None => inspected.document.to_string(),
                };
                super::artifact_media::link_files(&document, &artifact.id)
            }
            None => inspected.document.to_string(),
        };
        let html = preview_page(&document, &theme, self.page_visible && self.mode == Mode::Preview, load);
        self.trace("prepare-end/build-start");
        let preview = match spare {
            Some(preview) => {
                set_drawn(preview.web.read(cx), false);
                match preview.web.read(cx).load_html(&html) {
                    Ok(()) => Ok(Preview { load, ..preview }),
                    Err(error) => Err(error.to_string()),
                }
            }
            None => Self::build_preview(html, load, window, cx),
        };
        self.trace("build-end");
        match preview {
            Ok(preview) => self.preview = Some(preview),
            Err(error) => self.web_error = Some(error),
        }
        self.sync_web(cx);
    }

    fn build_preview(html: String, load: u64, window: &mut Window, cx: &mut Context<Self>) -> Result<Preview, String> {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<(u64, PageEvent)>();
        let (left, popup) = (tx.clone(), tx.clone());
        let webview = wry::WebViewBuilder::new()
            .with_html(html)
            .with_custom_protocol("filo-font".into(), |_, request| font_response(request.uri()))
            .with_custom_protocol(super::artifact_media::SCHEME.into(), |_, request| {
                super::artifact_media::response(&request)
            })
            // Its own data store that keeps nothing: no cookies or storage shared with the app's other web views.
            .with_incognito(true)
            // Nothing of WebKit's own (white) background: where the page draws nothing, the app's shows.
            .with_transparent(true)
            // Messages count only from the host page, which passes on the artifact frame's bridge messages.
            .with_ipc_handler(move |request: wry::http::Request<String>| {
                if !host_page(&request.uri().to_string()) {
                    return;
                }
                let Ok(value) = serde_json::from_str::<Value>(request.body()) else { return };
                let Some(load) = value.get("load").and_then(Value::as_u64) else { return };
                let event = if let Some(frame) = value.get("frame").and_then(Value::as_str) {
                    Some(PageEvent::Message(frame.to_string()))
                } else {
                    match value.get("host").and_then(Value::as_str) {
                        Some("phase") => Some(PageEvent::Phase(value)),
                        Some("ready") => Some(PageEvent::Ready),
                        Some("menu") => match value.get("mode").and_then(Value::as_str) {
                            Some("version") => value.get("version").and_then(Value::as_f64).map(PageEvent::MenuVersion),
                            Some(mode) => Some(PageEvent::Menu(mode.to_string())),
                            None => None,
                        },
                        _ => None,
                    }
                };
                if let Some(event) = event {
                    let _ = tx.unbounded_send((load, event));
                }
            })
            // Only the host and the artifact load; a link the page follows opens in the browser instead.
            .with_navigation_handler(move |url| {
                if allowed_page(&url) {
                    return true;
                }
                let _ = left.unbounded_send((0, PageEvent::Left(url)));
                false
            })
            .with_new_window_req_handler(move |url, _| {
                let _ = popup.unbounded_send((0, PageEvent::Left(url)));
                wry::NewWindowResponse::Deny
            })
            .build_as_child(&*window)
            .map_err(|error| error.to_string())?;
        let web = cx.new(|cx| WebView::new(webview, window, cx));
        set_drawn(web.read(cx), false);
        let web_id = web.entity_id();
        let events = cx.spawn_in(window, async move |this, cx| {
            while let Some((load, event)) = rx.next().await {
                let handled = this.update_in(cx, |this, window, cx| {
                    // Only the page on screen counts: not a version parked out of sight, nor the page this web view
                    // showed before (a link it follows has no load number: it counts if this web view is on screen).
                    let current = this.preview.as_ref().is_some_and(|preview| match &event {
                        PageEvent::Left(_) => preview.web.entity_id() == web_id,
                        _ => preview.load == load,
                    });
                    if !current {
                        return;
                    }
                    if (!this.page_visible || this.mode != Mode::Preview)
                        && !matches!(&event, PageEvent::Ready | PageEvent::Phase(_))
                    {
                        return;
                    }
                    match event {
                        PageEvent::Phase(value) => {
                            if artifact_tracing() {
                                eprintln!(
                                    "artifact: phase {value} at {:.3} ms",
                                    this.opened.elapsed().as_secs_f64() * 1000.
                                );
                            }
                        }
                        PageEvent::Message(message) => this.page_message(&message, window, cx),
                        PageEvent::Left(url) => this.page_left(&url, cx),
                        PageEvent::Ready => {
                            this.ready = true;
                            if let Some(preview) = &this.preview {
                                set_drawn(preview.web.read(cx), true);
                            }
                            this.sync_menu(cx);
                            this.trace("ready");
                        }
                        PageEvent::MenuVersion(version) => {
                            this.set_menu_open(false, cx);
                            this.select_version(Some(version), cx);
                        }
                        PageEvent::Menu(mode) => match mode.as_str() {
                            "preview" => this.set_mode(Mode::Preview, cx),
                            "source" => this.set_mode(Mode::Source, cx),
                            "current" => {
                                this.set_menu_open(false, cx);
                                this.select_version(None, cx);
                            }
                            "restore" => {
                                this.set_menu_open(false, cx);
                                if let Some(version) = this.selected_version {
                                    this.confirm_restore(version, window, cx);
                                }
                            }
                            "close" => this.set_menu_open(false, cx),
                            _ => {}
                        },
                    }
                });
                if handled.is_err() {
                    break;
                }
            }
        });
        Ok(Preview { web, load, _events: events })
    }

    fn trace(&self, phase: &str) {
        if artifact_tracing() {
            eprintln!("artifact: {phase} {:.3} ms", self.opened.elapsed().as_secs_f64() * 1000.);
        }
    }

    #[cfg(feature = "dev")]
    pub(crate) fn smoke_state(&self) -> String {
        format!(
            "data={} web={} visible={} error={:?}",
            self.artifact.is_some(),
            self.preview.is_some(),
            self.page_visible,
            self.web_error
        )
    }

    #[cfg(feature = "dev")]
    pub(crate) fn preview_ready(&self) -> bool {
        self.ready
            && self.inspected().is_some_and(|page| {
                self.web_key.as_ref().is_some_and(|key| key.version == page.version && key.document == page.document)
            })
    }

    #[cfg(feature = "dev")]
    pub(crate) fn smoke_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.set_menu_open(open, cx);
    }

    #[cfg(feature = "dev")]
    pub(crate) fn smoke_source(&mut self, source: bool, cx: &mut Context<Self>) {
        self.set_mode(if source { Mode::Source } else { Mode::Preview }, cx);
    }

    #[cfg(feature = "dev")]
    pub(crate) fn smoke_version(&mut self, version: Option<f64>, cx: &mut Context<Self>) {
        self.select_version(version, cx);
        self.set_mode(Mode::Preview, cx);
    }

    #[cfg(feature = "dev")]
    pub(crate) fn smoke_eval(&self, script: &str, cx: &Context<Self>) -> futures::channel::oneshot::Receiver<String> {
        let (tx, rx) = futures::channel::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        self.preview
            .as_ref()
            .expect("preview exists")
            .web
            .read(cx)
            .evaluate_script_with_callback(script, move |value| {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(value);
                }
            })
            .unwrap();
        rx
    }

    #[cfg(feature = "dev")]
    pub(crate) fn smoke_visible(&self, cx: &Context<Self>) -> bool {
        self.preview.as_ref().is_some_and(|preview| preview.web.read(cx).visible())
    }

    /// How many holes are cut in the preview's web view for what's drawn over it (menus, tooltips).
    #[cfg(feature = "dev")]
    pub(crate) fn smoke_web_holes(&self, cx: &gpui::App) -> Option<usize> {
        self.preview.as_ref().map(|preview| crate::web_holes::holes(preview.web.read(cx).raw()))
    }

    /// (the page in the web view is ready, the web view draws it (see `set_drawn`)).
    #[cfg(feature = "dev")]
    pub(crate) fn smoke_drawn(&self, cx: &Context<Self>) -> (bool, bool) {
        #[cfg(target_os = "macos")]
        let drawn = self.preview.as_ref().is_some_and(|preview| {
            use gpui_component::wry::WebViewExtMacOS as _;
            let view = preview.web.read(cx).raw().webview();
            // SAFETY: as in `set_drawn`.
            let alpha: f64 = unsafe { objc2::msg_send![&*view, alphaValue] };
            alpha > 0.
        });
        (self.ready, drawn)
    }

    /// (menu open, drawn by the preview page, in Preview, an older version shown, the web view).
    #[cfg(feature = "dev")]
    pub(crate) fn smoke_view(&self) -> (bool, bool, bool, Option<f64>, Option<gpui::EntityId>) {
        (
            self.menu_open,
            self.menu_in_host,
            self.mode == Mode::Preview,
            self.selected_version,
            self.preview.as_ref().map(|preview| preview.web.entity_id()),
        )
    }

    /// The page tried to go to `url` (a link; the web view refused it): the browser gets a website, at most one a
    /// second, so a page can't open them in a loop.
    fn page_left(&mut self, url: &str, cx: &mut Context<Self>) {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return;
        }
        let now = std::time::Instant::now();
        if self.last_link.is_some_and(|last| now.duration_since(last) < std::time::Duration::from_secs(1)) {
            return;
        }
        self.last_link = Some(now);
        cx.open_url(url);
    }

    /// `{ type: 'filo:chat', text }` from a website: ask Filo in a chat about this artifact.
    /// `{ type: 'filo:nav', dir }` from `NAV_BRIDGE`: Back or Forward.
    fn page_message(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(message) = serde_json::from_str::<Value>(message) else { return };
        if message.get("type").and_then(Value::as_str) == Some("filo:nav") {
            match message.get("dir").and_then(Value::as_str) {
                Some("back") => crate::nav::back(window, cx),
                Some("forward") => crate::nav::forward(window, cx),
                _ => {}
            }
            return;
        }
        if message.get("type").and_then(Value::as_str) != Some("filo:chat") {
            return;
        }
        let text: String = message.get("text").and_then(Value::as_str).unwrap_or("").chars().take(2_000).collect();
        let text = text.trim().to_string();
        if text.is_empty() || self.pending_chat.is_some() {
            return;
        }
        // The page could be anything an agent wrote: the user decides whether it speaks for them.
        self.pending_chat = Some(text);
        self.sync_web(cx);
        cx.notify();
    }

    /// The user's answer to the page's question for Filo: send it (and open that chat), or not.
    fn answer_chat(&mut self, send: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.pending_chat.take() else { return };
        self.sync_web(cx);
        cx.notify();
        if !send {
            return;
        }
        backend::call_in(
            window,
            cx,
            "canvasArtifacts:sendArtifactMessage",
            json!({ "artifactId": self.id, "text": text }),
            |_: &mut Self, result: Result<Value, String>, window, cx| match result
                .ok()
                .and_then(|value| value.get("sessionId").and_then(Value::as_str).map(str::to_string))
            {
                Some(session) => crate::nav::open_chat(session, window, cx),
                None => ui::toast_error(window, cx, "Couldn’t reach Filo", "Please try again."),
            },
        );
    }

    /// Mode and version, behind one button at the top right, with a dropdown.
    fn render_menu_button(&self, artifact: &Artifact, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let displayed = self.selected_version.unwrap_or(artifact.current_version);
        let older = self.selected_version.is_some();
        let mode_icon = match self.mode {
            Mode::Preview => "eye",
            Mode::Source => "code-xml",
        };
        let button = div()
            .id("artifact-menu")
            .h(px(28.))
            .px(px(space::SM + 2.))
            .flex()
            .items_center()
            .gap(px(space::XS + 2.))
            .rounded(px(radius::FULL))
            .border_1()
            .border_color(if older { colors.warning } else { colors.border })
            .bg(if self.menu_open { colors.selected_fill } else { colors.surface })
            .cursor_pointer()
            .hover(|style| style.bg(colors.selected_fill))
            .child(ui::icon_sized(mode_icon, 14., colors.text))
            .child(
                div()
                    .footnote()
                    .semibold()
                    .text_color(if older { colors.warning } else { colors.text })
                    .child(format!("v{}", displayed as i64)),
            )
            .child(ui::icon_sized(
                if self.menu_open { "chevron-up" } else { "chevron-down" },
                13.,
                colors.text_secondary,
            ))
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                this.menu_right = f32::from(window.viewport_size().width - event.position().x) - 48.;
                let open = !this.menu_open;
                this.set_menu_open(open, cx);
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.menu_open && this.menu_in_host {
                    this.set_menu_open(false, cx);
                }
            }));
        let menu = (self.menu_open && !self.menu_in_host).then(|| {
            let item = |id: &'static str, icon: &'static str, label: String, checked: bool| {
                div()
                    .id(id)
                    .h(px(30.))
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .rounded(px(radius::XS))
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.selected_fill))
                    .child(ui::icon_sized(icon, 15., colors.text_secondary))
                    .child(div().flex_1().subhead().child(label))
                    .when(checked, |this| this.child(ui::icon_sized("check", 14., colors.accent)))
            };
            let rule = || div().my(px(space::XS)).h(px(1.)).bg(colors.border);
            let note = |text: String| {
                div().px(px(space::SM)).py(px(space::XS)).caption().text_color(colors.text_tertiary).child(text)
            };
            let mut list = div()
                .id("artifact-menu-list")
                .mt(px(space::XS))
                .w(px(260.))
                .p(px(space::XS))
                .flex()
                .flex_col()
                .rounded(px(radius::MD))
                .bg(colors.raised)
                .border_1()
                .border_color(colors.border)
                .shadow_lg()
                // The scroll wheel over it doesn't close it (it's one menu of escape.rs's popups).
                .relative()
                .child(gpui_component::one_menu::surface())
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.set_menu_open(false, cx)));
            for (mode, id, icon, label) in [
                (Mode::Preview, "artifact-mode-preview", "eye", "Preview"),
                (Mode::Source, "artifact-mode-source", "code-xml", "Source"),
            ] {
                list = list.child(
                    item(id, icon, label.to_string(), self.mode == mode)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx))),
                );
            }
            // The versions, newest first: the one shown is checked, the current one says so. A long history scrolls.
            list = list.child(rule()).child(
                div()
                    .px(px(space::SM))
                    .pt(px(space::XS))
                    .pb(px(space::XXS))
                    .caption()
                    .text_color(colors.text_tertiary)
                    .child("Versions"),
            );
            let rows = div().id("artifact-menu-versions").max_h(px(264.)).overflow_y_scroll().flex().flex_col();
            let rows = match &self.versions {
                None => rows.child(note("Loading versions…".into())),
                Some(Err(error)) => rows.child(note(error.clone())),
                Some(Ok(versions)) => {
                    let more = artifact.current_version as usize > versions.len();
                    rows.children(versions.iter().map(|row| {
                        let version = row.version;
                        div()
                            .id(SharedString::from(format!("artifact-version-{}", version as i64)))
                            .min_h(px(42.))
                            .px(px(space::SM))
                            .py(px(space::XS))
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .rounded(px(radius::XS))
                            .cursor_pointer()
                            .hover(|style| style.bg(colors.selected_fill))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(space::XXS))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(space::XS + 2.))
                                            .subhead()
                                            .child(format!("Version {}", version as i64))
                                            .when(row.is_current, |this| {
                                                this.child(ui::chip("Current", ui::Tone::Success, true, cx))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .caption()
                                            .text_color(colors.text_secondary)
                                            .child(crate::format::day_time(row.created_at)),
                                    ),
                            )
                            .when(version as i64 == displayed as i64, |this| {
                                this.child(ui::icon_sized("check", 14., colors.accent))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_menu_open(false, cx);
                                this.select_version(Some(version), cx);
                            }))
                    }))
                    .when(more, |this| this.child(note(format!("Showing the latest {} versions.", versions.len()))))
                }
            };
            list = list.child(rows);
            if older {
                let restoring = self.restoring.is_some();
                list = list
                    .child(rule())
                    .child(item("artifact-show-current", "rotate-ccw", "Show current version".into(), false).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.set_menu_open(false, cx);
                            this.select_version(None, cx);
                        }),
                    ))
                    .child(
                        item(
                            "artifact-restore-version",
                            "archive-restore",
                            if restoring { "Restoring…".into() } else { "Restore this version…".into() },
                            false,
                        )
                        .when(restoring, |this| this.text_color(colors.text_tertiary).cursor_default())
                        .when(!restoring, |this| {
                            this.on_click(cx.listener(|this, _, window, cx| {
                                this.set_menu_open(false, cx);
                                if let Some(version) = this.selected_version {
                                    this.confirm_restore(version, window, cx);
                                }
                            }))
                        }),
                    );
            }
            div().absolute().right_0().bottom_0().child(
                deferred(anchored().anchor(Corner::TopRight).snap_to_window_with_margin(px(8.)).child(list))
                    .with_priority(1),
            )
        });
        div().relative().child(button).children(menu).into_any_element()
    }

    /// What the page asked to send to Filo, with Send and Cancel, where the preview was.
    fn render_pending_chat(&self, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(space::XL))
            .child(
                div()
                    .max_w(px(520.))
                    .w_full()
                    .p(px(space::LG))
                    .flex()
                    .flex_col()
                    .gap(px(space::MD))
                    .rounded(px(radius::LG))
                    .bg(colors.surface)
                    .border_1()
                    .border_color(colors.border)
                    .child(div().headline().child("Send this to Filo?"))
                    .child(
                        div()
                            .footnote()
                            .text_color(colors.text_secondary)
                            .child("This artifact wants to ask Filo something for you:"),
                    )
                    .child(
                        div()
                            .id("artifact-chat-text")
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .p(px(space::MD))
                            .rounded(px(radius::MD))
                            .bg(colors.background)
                            .callout()
                            .child(text.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(space::SM))
                            .child(
                                ui::Button::ghost("artifact-chat-cancel", "Cancel")
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| this.answer_chat(false, window, cx))),
                            )
                            .child(
                                ui::Button::primary("artifact-chat-send", "Send")
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| this.answer_chat(true, window, cx))),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_preview(&mut self, inspected: &Inspected, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_web(inspected, window, cx);
        if let Some(text) = self.pending_chat.clone() {
            return self.render_pending_chat(&text, cx);
        }
        match (&self.preview, &self.web_error) {
            (Some(preview), _) => div().size_full().child(preview.web.clone()).into_any_element(),
            (None, Some(error)) => div()
                .p(px(space::XL))
                .child(ui::banner(format!("The preview couldn’t start: {error}"), ui::Tone::Danger, cx))
                .into_any_element(),
            (None, None) => div().p(px(space::XXL)).child(ui::skeleton_document()).into_any_element(),
        }
    }

    fn render_source(&self, inspected: &Inspected, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let source = &inspected.document;
        let truncated = source.len() > SOURCE_DISPLAY_LIMIT;
        div()
            .id("artifact-source-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .px(px(space::LG))
                    .pt(px(space::LG))
                    .pb(px(48.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .pb(px(space::MD))
                            .mb(px(space::LG))
                            .border_b_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .footnote()
                                    .text_color(colors.text_secondary)
                                    .child(format!("HTML · version {}", inspected.version as i64)),
                            ),
                    )
                    .map(|this| {
                        if source.is_empty() {
                            this.child(
                                div()
                                    .footnote()
                                    .text_color(colors.text_secondary)
                                    .child("Source is unavailable for this version."),
                            )
                        } else {
                            let mut end = SOURCE_DISPLAY_LIMIT.min(source.len());
                            while !source.is_char_boundary(end) {
                                end -= 1;
                            }
                            this.child(source_text(&source[..end], colors.text, cx)).when(truncated, |this| {
                                this.child(div().mt(px(space::LG)).footnote().text_color(colors.text_secondary).child(
                                    format!(
                                        "Showing the first {} of {}. Download it to see everything.",
                                        crate::format::file_size(end as f64),
                                        crate::format::file_size(source.len() as f64)
                                    ),
                                ))
                            })
                        }
                    }),
            )
            .into_any_element()
    }
}

/// Source in the monospace font and size from Appearance.
fn source_text(source: &str, color: gpui::Hsla, cx: &gpui::App) -> AnyElement {
    let size = cx.theme().mono_size();
    div()
        .font_family(cx.theme().mono_family())
        .text_size(px(size))
        .line_height(px((size * 1.55).round()))
        .text_color(color)
        .child(source.to_string())
        .into_any_element()
}

impl Render for ArtifactScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        // Esc closes the menu, or answers "not now" to the page's question for Filo.
        if self.pending_chat.is_some() {
            crate::escape::layer(window, cx, |this, window, cx| this.answer_chat(false, window, cx));
        }
        if self.menu_open {
            crate::escape::popup(window, cx, |this, _, cx| this.set_menu_open(false, cx));
        }
        let artifact = match &self.artifact {
            None => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(ui::sub_page_header("artifact", "", Vec::new(), cx))
                    .child(div().flex_1().p(px(space::XXL)).child(ui::skeleton_document()))
                    .into_any_element();
            }
            Some(Err(error)) => {
                let error = error.clone();
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(ui::sub_page_header("artifact", "", Vec::new(), cx))
                    .child(div().p(px(space::XL)).child(ui::banner(error, ui::Tone::Danger, cx)))
                    .into_any_element();
            }
            Some(Ok(None)) => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(ui::sub_page_header("artifact", "", Vec::new(), cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .px(px(space::XXL + space::XS))
                            .child(div().large_title().child("This artifact moved."))
                            .child(
                                div()
                                    .mt(px(space::MD))
                                    .headline()
                                    .font_weight(gpui::FontWeight::NORMAL)
                                    .text_color(colors.text_secondary)
                                    .child("Filo may have archived or replaced it."),
                            ),
                    )
                    .into_any_element();
            }
            Some(Ok(Some(artifact))) => artifact.clone(),
        };

        let inspected = self.inspected();
        let mut trailing: Vec<AnyElement> = vec![
            self.render_menu_button(&artifact, cx),
            IconButton::new("artifact-open-browser", "external-link")
                .tooltip("Open in browser")
                .on_click(cx.listener(|this, _, window, cx| this.open_in_browser(window, cx)))
                .into_any_element(),
        ];
        trailing.push(
            IconButton::new("artifact-show-folder", "folder")
                .tooltip("Show in Finder")
                .disabled(artifact.folder.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.show_in_finder(window, cx)))
                .into_any_element(),
        );
        if let Some(link) = &artifact.claude_link {
            let tooltip = if link.version == artifact.current_version {
                "Open on claude.ai".to_string()
            } else {
                format!("Open on claude.ai (version {} there)", link.version as i64)
            };
            let button = IconButton::new("artifact-claude-link", "link")
                .tooltip(tooltip)
                .on_click(cx.listener(|this, _, window, cx| this.open_claude_link(window, cx)));
            trailing.push(crate::smoke::tracked("artifact-claude-link", button));
        }
        if self.share.as_ref().is_some_and(ShareSettings::sharing) {
            let tooltip = match &artifact.claude_link {
                _ if self.sharing => "Sharing to claude.ai…",
                Some(_) => "Share this version to claude.ai, at the same link",
                None => "Share to claude.ai",
            };
            let button = IconButton::new("artifact-share", "claude")
                .tooltip(tooltip)
                .spinning(self.sharing)
                .disabled(self.sharing)
                .on_click(cx.listener(|this, _, window, cx| this.share(window, cx)));
            trailing.push(crate::smoke::tracked("artifact-share", button));
        }
        let export = IconButton::new("artifact-export", "download")
            .tooltip("Download as HTML")
            .disabled(inspected.as_ref().is_none_or(|inspected| inspected.document.is_empty()))
            .on_click(cx.listener(|this, _, window, cx| this.export(window, cx)));
        trailing.push(crate::smoke::tracked("artifact-export", export));
        // Shared, the bar under the header has the rest.
        if artifact.link.is_none() {
            let share = Button::secondary("artifact-share-link", if self.linking { "Sharing…" } else { "Share" })
                .small()
                .icon("share-2")
                .loading(self.linking)
                .disabled(self.linking)
                .on_click(cx.listener(|this, _, window, cx| this.share_link(window, cx)));
            trailing.push(
                div().ml(px(space::XS)).child(crate::smoke::tracked("artifact-share-link", share)).into_any_element(),
            );
        }

        let body: AnyElement = match (self.mode, &inspected) {
            (_, None) => match &self.version_detail {
                Some(Err(error)) => {
                    div().p(px(space::XL)).child(ui::banner(error.clone(), ui::Tone::Danger, cx)).into_any_element()
                }
                Some(Ok(None)) => div()
                    .p(px(space::XL))
                    .callout()
                    .text_color(colors.text_secondary)
                    .child("Source is unavailable for this version.")
                    .into_any_element(),
                _ => div().p(px(space::XXL)).child(ui::skeleton_document()).into_any_element(),
            },
            (Mode::Preview, Some(inspected)) => self.render_preview(inspected, window, cx),
            (Mode::Source, Some(inspected)) => self.render_source(inspected, cx),
        };

        // The shared bar stays put under the header (the page scrolls in its web view), so it's part of the page's top:
        // the fade under it lies over the page as it scrolls, not over the bar.
        let shared = self.render_shared_bar(&artifact, cx);
        ui::sub_page(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .child(ui::sub_page_header("artifact", artifact.title.clone(), trailing, cx))
                .children(shared),
            div().flex_1().min_h_0().child(body),
            cx,
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_artifact_runs_framed_with_its_policy_first() {
        let host = sandbox(
            "<!DOCTYPE html><html><head><title>x</title></head><body><script>alert('</script><img src=https://e.x/a>')</script></body></html>",
            "dark",
        );
        // The page's text can't end the host's script: every "<" in it is escaped.
        let script = host.split("f.srcdoc=").nth(1).unwrap();
        assert_eq!(script.matches("</script>").count(), 1, "{script}");
        assert!(host.contains(r#"sandbox="allow-scripts allow-forms allow-popups""#));
        assert!(!host.contains("allow-same-origin") && !host.contains("allow-top-navigation"));
        assert!(host.contains("connect-src 'none'") && host.contains("color-scheme:dark"));
        // Inside the frame: the doctype, then the policy and the bridge, before anything of the page.
        let page: String = serde_json::from_str(script.split(";})();</script>").next().unwrap()).unwrap();
        assert!(
            page.starts_with(
                "<!DOCTYPE html><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none';"
            ),
            "{page}"
        );
        assert!(page.find("window.filo").unwrap() < page.find("<title>").unwrap());
    }

    #[test]
    fn preparing_the_host_does_not_rewrite_artifact_scripts() {
        let document = "<script>let shown=true;const policy='font-src data:;';</script>";
        let host = sandbox_host(document, "light", false, true, 7);
        assert!(host.contains("let shown=false;let loaded="));
        // Its messages carry the load's number, so a web view that loads another page can ignore the last one's.
        assert!(host.contains("Object.assign(message,{load:7})"));
        let source = host.split("f.srcdoc=").nth(1).unwrap().split(";})();</script>").next().unwrap();
        let page: String = serde_json::from_str(source).unwrap();
        assert!(page.ends_with(document));
        assert!(page.contains("font-src data: filo-font:;"));
    }

    #[test]
    fn only_the_host_and_its_frame_load_and_only_the_host_speaks() {
        assert_eq!(put_first("<p>hi</p>", "<x>"), "<x><p>hi</p>");
        assert_eq!(put_first("  <!doctype html><p>", "<x>"), "<!doctype html><x><p>");
        assert!(allowed_page("about:blank") && allowed_page("about:srcdoc"));
        for url in
            ["https://example.com", "file:///etc/passwd", "data:text/html,x", "javascript:alert(1)", "about:blank#x"]
        {
            assert!(!allowed_page(url), "{url}");
        }
        assert!(host_page("about:blank") && !host_page("about:srcdoc"));
    }

    #[test]
    fn preview_fonts_only_serve_the_three_bundled_faces() {
        for weight in [400, 500, 600] {
            let response = font_response(&format!("filo-font://atkinson/{weight}.ttf").parse().unwrap());
            assert_eq!(response.status(), 200);
            assert!(response.body().len() > 1000);
        }
        for uri in [
            "filo-font://atkinson/../filo.db",
            "filo-font://atkinson/700.ttf",
            "filo-font://elsewhere/400.ttf",
            "https://atkinson/400.ttf",
        ] {
            let response = font_response(&uri.parse().unwrap());
            assert_eq!(response.status(), 404, "{uri}");
            assert!(response.body().is_empty());
        }
    }

    #[test]
    fn same_length_content_changes_invalidate_the_preview() {
        let one = WebKey { theme: "dark".into(), version: 1., document: "one".into() };
        let two = WebKey { document: "two".into(), ..one.clone() };
        assert!(one != two);
    }
}

// Filo's browser: web pages on macOS's WebKit that agents work in the background and the user can open and use
// (Window › Browser, ⇧⌘B). All tabs share one WebKit data store kept on disk, so logins (cookies, storage) last
// between launches; its id is in `local/browser.json`. A tab's page lives off the screen in a window of its own
// (browser/native.rs) that WebKit counts as visible and focused, and isn't throttled, so an agent's page runs and
// takes keys while another app is in front; the browser window (browser/window.rs) shows one tab at a time by moving
// its page in.
//
// Agents reach it through the `browser_*` tools (crates/filo-local browser_mcp.rs): their MCP server asks over a Unix
// socket in the local data folder, `browser.sock`, one JSON line each way; `serve` answers on the main thread.
// Reading and finding things on a page is a script in a JavaScript world of Filo's own (browser/page.js); clicks
// and keys are real AppKit events handed to the page's web view, so the page sees trusted input. A page's dialogs,
// file choosers and window.close() come through Filo's UI delegate (browser/delegate.rs), its downloads through wry's
// handlers, and what agents do is kept for the user to watch, pause and take over (browser/activity.rs).

mod activity;
// Chromium (CEF) as the page's engine, the one Settings › Browser can pick instead of WebKit (browser/engine.rs).
mod chromium;
// `FILO_OPEN=browser-engine-check`: Settings › Browser's switch between the engines from a local feed (`dev` builds).
#[cfg(feature = "dev")]
mod engine_check;
// `FILO_OPEN=browser-input-check`: an input method, the right-click menu, a drop from another app and printing, as the
// user does them in the browser window (`dev` builds).
#[cfg(feature = "dev")]
mod input_check;
// `FILO_OPEN=chromium-check`: memory, Google's sign-in and WebKit's cookies with Chromium as the engine (`dev` builds).
#[cfg(feature = "dev")]
mod chromium_check;
// `FILO_OPEN=browser-check`: the browser end to end (`dev` builds).
#[cfg(feature = "dev")]
pub mod check;
mod delegate;
mod drag;
// Which engine new pages open in, and the Chromium engine's download, checks and updates (Settings › Browser).
pub mod engine;
// `FILO_OPEN=browser-drag-check`: dragging in a page, mouse-only and drag and drop (`dev` builds).
#[cfg(feature = "dev")]
mod drag_check;
mod import;
// `FILO_OPEN=browser-import-check`: bringing in a profile and its logins (`dev` builds).
#[cfg(feature = "dev")]
mod import_check;
// `FILO_OPEN=browser-keys-check`: the user's keyboard in the browser window (`dev` builds).
#[cfg(feature = "dev")]
mod keys_check;
mod native;
// `FILO_OPEN=browser-tools-check`: uploads, downloads, menus, hover, dialogs, pop-ups, waiting, full-page screenshots
// and the user's Pause and Take over (`dev` builds).
#[cfg(feature = "dev")]
mod tools_check;
pub mod window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures::StreamExt as _;
use gpui::{App, AsyncApp};
use gpui_component::wry;
use gpui_component::wry::{WebViewBuilderExtDarwin as _, WebViewBuilderExtMacos as _, WebViewExtMacOS as _};
use objc2::rc::Retained;
use serde_json::{Value, json};

pub use activity::Control;
pub use native::frontmost;

/// What each tool call runs in the page (see the file).
const PAGE_JS: &str = include_str!("browser/page.js");
/// A new tab's page size, in points: a laptop's browser window.
const PAGE_SIZE: (f64, f64) = (1280., 800.);
/// At most this many tabs at once.
const MAX_TABS: usize = 24;
/// How long a page may take to load before a tool answers anyway.
const LOAD_WAIT: Duration = Duration::from_secs(20);
/// Longest text `browser_type` or `browser_keys` types key by key.
const MAX_TYPED: usize = 4000;
/// The tallest full-page screenshot, in CSS pixels (a taller page is cut there).
const FULL_MAX: f64 = 6000.;
/// How long an agent's call waits while the user has paused it or taken its tab over, before it answers that (under
/// the MCP server's two minutes).
const CONTROL_WAIT: Duration = Duration::from_secs(100);
/// The longest `browser_wait`.
const MAX_WAIT: f64 = 60.;

/// Who opened a tab: an agent (by name) from a chat.
#[derive(Clone, Debug)]
pub struct Owner {
    pub agent: String,
    pub chat: String,
}

/// An agent asking the user to step in on a tab (browser_ask_user).
#[derive(Clone, Debug)]
pub struct Help {
    pub agent: String,
    pub reason: String,
    /// "done" or "declined" once the user answered.
    pub answer: Option<&'static str>,
}

/// A file the page downloaded (or is downloading).
#[derive(Clone, Debug)]
pub struct Download {
    pub url: String,
    pub path: PathBuf,
    /// Some(true) once it's saved, Some(false) when it failed.
    pub done: Option<bool>,
    ended: Option<Instant>,
    /// What the agent was told of it: that it started, that it ended.
    told: (bool, bool),
}

/// What runs a tab's page: everything else about a tab is the same in both engines (native.rs asks browser/chromium.rs
/// first for what it would ask WebKit).
enum Page {
    WebKit(wry::WebView),
    /// Kept for its drop, which closes the page in Chromium.
    Chromium(#[allow(dead_code)] chromium::Page),
}

pub struct Tab {
    pub id: String,
    /// A page's alert, confirm or prompt, waiting for its answer. Answered (dismissed) before the page goes.
    dialog: RefCell<Option<delegate::Dialog>>,
    // Dropped before its window.
    page: Page,
    /// The page's view: WebKit's, or the one a page in Chromium shows in.
    view: Retained<objc2::runtime::AnyObject>,
    park: native::Park,
    pub owner: Option<Owner>,
    /// The tab whose page opened this one (a pop-up, or a link to a new window).
    pub opener: Option<String>,
    created: Instant,
    help: RefCell<Option<Help>>,
    /// Files browser_upload gave, for the page's next file chooser.
    upload: RefCell<Option<Vec<PathBuf>>>,
    downloads: RefCell<Vec<Download>>,
    /// What happened that the agent hasn't been told yet (a pop-up opened, a file chooser left empty), said with its
    /// next answer about this tab.
    news: RefCell<Vec<String>>,
    work: RefCell<activity::Work>,
}

impl Tab {
    fn view(&self) -> &objc2::runtime::AnyObject {
        &self.view
    }

    /// Title, address, loading.
    pub fn state(&self) -> (String, String, bool) {
        native::state(self.view())
    }

    pub fn help(&self) -> Option<Help> {
        self.help.borrow().clone()
    }

    pub fn answer_help(&self, answer: &'static str) {
        if let Some(help) = self.help.borrow_mut().as_mut() {
            help.answer = Some(answer);
        }
        changed();
    }

    pub fn load(&self, url: &str) {
        match &self.page {
            Page::WebKit(web) => drop(web.load_url(url)),
            Page::Chromium(_) => {
                if let Some(page) = chromium::of(self.view()) {
                    page.load(url);
                }
            }
        }
    }

    /// The engine the page runs in.
    pub fn engine(&self) -> engine::Kind {
        match self.page {
            Page::WebKit(_) => engine::Kind::WebKit,
            Page::Chromium(_) => engine::Kind::Chromium,
        }
    }

    pub fn go(&self, how: &str) {
        native::go(self.view(), how);
    }

    pub fn history(&self) -> (bool, bool) {
        native::history(self.view())
    }

    /// Who it is: "the user" or the agent.
    fn who(&self) -> String {
        self.owner.as_ref().map_or("the user".into(), |owner| owner.agent.clone())
    }

    /// The page's dialog waiting for an answer: its kind ("alert", "confirm", "prompt") and message.
    pub fn dialog(&self) -> Option<(&'static str, String)> {
        self.dialog.borrow().as_ref().map(|dialog| (dialog.kind.name(), dialog.message.clone()))
    }

    /// "an alert" and the like, for the dialog waiting.
    fn dialog_a(&self) -> Option<&'static str> {
        self.dialog.borrow().as_ref().map(|dialog| dialog.kind.a())
    }

    /// Answers the page's dialog: OK or Cancel (the browser window's bar, browser_dialog).
    pub fn answer_dialog(&self, accept: bool, text: Option<&str>) -> bool {
        let Some(dialog) = self.dialog.borrow_mut().take() else { return false };
        dialog.answer(accept, text);
        changed();
        true
    }

    /// The agent working here (or that did last), and who has the tab.
    pub fn agent(&self) -> Option<String> {
        self.work.borrow().agent.clone()
    }

    pub fn control(&self) -> Control {
        self.work.borrow().control
    }

    /// Whether the browser window shows the agent's bar over the page: it works here now, or the user holds it.
    pub fn agent_shown(&self) -> bool {
        self.work.borrow().shown()
    }

    pub fn working(&self) -> bool {
        self.work.borrow().working()
    }

    /// The user pauses the agent here, takes the tab over, or lets the agent go on (`Control::Agent`).
    pub fn set_control(&self, control: Control) {
        let mut work = self.work.borrow_mut();
        if control == Control::Agent {
            work.told = match work.control {
                Control::Paused => Some("The user paused you in this tab for a while, then let you go on."),
                Control::User => Some(
                    "The user took this tab over and handed it back to you: the page may have changed, so read it again (browser_read) before using refs you had.",
                ),
                Control::Agent => work.told,
            };
        }
        work.control = control;
        drop(work);
        if control == Control::User && !native::parked(self.view()) {
            native::focus_page(self.view());
        }
        changed();
    }
}

impl Drop for Tab {
    /// Its page leaves the browser window with it (a page in Chromium is a view of Filo's own, which the window would
    /// keep).
    fn drop(&mut self) {
        // SAFETY: AppKit on the main thread; the tab holds the view.
        unsafe {
            let _: () = objc2::msg_send![&*self.view, removeFromSuperview];
        }
    }
}

#[derive(Default)]
struct Browser {
    tabs: Vec<Rc<Tab>>,
    next: u32,
    /// Bumped on every change the browser window shows.
    generation: u64,
}

thread_local! {
    static BROWSER: RefCell<Browser> = RefCell::new(Browser::default());
    /// The tab the browser window shows.
    static SHOWN: RefCell<Option<String>> = const { RefCell::new(None) };
    static SERVING: Cell<bool> = const { Cell::new(false) };
    /// The socket file this Filo bound (device and inode), the only one `quit` takes away: another Filo on the same
    /// folder, whose `listen` failed, leaves the first one's socket alone.
    static BOUND: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
    /// Held while any tab is open (`native::Awake`).
    static AWAKE: RefCell<Option<native::Awake>> = const { RefCell::new(None) };
    /// Tabs whose page closed itself (window.close()), closed by `init`'s task: not inside WebKit's call.
    static CLOSING: RefCell<Option<futures::channel::mpsc::UnboundedSender<String>>> = const { RefCell::new(None) };
}

/// No App Nap while tabs are open, so pages run at speed with Filo in the background.
fn keep_awake() {
    let open = BROWSER.with(|browser| !browser.borrow().tabs.is_empty());
    AWAKE.with(|awake| {
        let mut awake = awake.borrow_mut();
        if open && awake.is_none() {
            *awake = native::Awake::begin();
        } else if !open {
            *awake = None;
        }
    });
}

/// The tabs, oldest first.
pub fn tabs() -> Vec<Rc<Tab>> {
    BROWSER.with(|browser| browser.borrow().tabs.clone())
}

pub fn tab(id: &str) -> Option<Rc<Tab>> {
    let id = id.trim();
    BROWSER.with(|browser| browser.borrow().tabs.iter().find(|tab| tab.id == id).cloned())
}

fn tab_or_error(id: &str) -> Result<Rc<Tab>, String> {
    tab(id).ok_or_else(|| format!("There's no tab {id} (closed, or Filo restarted): browser_tabs lists the open ones."))
}

/// What the browser window last saw changed by: compare to redraw.
pub fn generation() -> u64 {
    BROWSER.with(|browser| browser.borrow().generation)
}

fn changed() {
    BROWSER.with(|browser| browser.borrow_mut().generation += 1);
}

pub fn shown() -> Option<String> {
    SHOWN.with(|shown| shown.borrow().clone())
}

pub fn set_shown(id: Option<String>) {
    SHOWN.with(|shown| *shown.borrow_mut() = id);
    changed();
}

fn local_dir() -> PathBuf {
    crate::config::config_dir().join("local")
}

/// `local/browser.json`: the WebKit data store's id (`store_id`) and the engine Settings › Browser picked
/// (browser/engine.rs).
fn browser_json() -> Value {
    let saved = std::fs::read(local_dir().join("browser.json")).ok();
    saved.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).filter(Value::is_object).unwrap_or(json!({}))
}

/// Sets one key of `local/browser.json`, the others kept.
fn set_browser_json(key: &str, value: Value) -> Result<(), String> {
    let mut saved = browser_json();
    saved[key] = value;
    std::fs::create_dir_all(local_dir()).map_err(|error| error.to_string())?;
    crate::config::write_atomic(&local_dir().join("browser.json"), saved.to_string().as_bytes())
        .map_err(|error| error.to_string())
}

/// The id of the browser's WebKit data store, made once and kept in `local/browser.json`: a test run's data folder
/// has its own, so its logins never mix with the user's.
fn store_id() -> [u8; 16] {
    if let Some(id) = browser_json().get("store").and_then(|id| id.as_str()?.parse::<uuid::Uuid>().ok()) {
        return *id.as_bytes();
    }
    let id = uuid::Uuid::new_v4();
    let _ = set_browser_json("store", json!(id.to_string()));
    *id.as_bytes()
}

/// Safari's user agent, with this Mac's Safari version: some sites turn away a web view that doesn't say it's Safari
/// (sign-ins especially).
fn user_agent() -> String {
    let version = std::fs::read_to_string("/Applications/Safari.app/Contents/Info.plist")
        .ok()
        .and_then(|plist| {
            let after = plist.split("<key>CFBundleShortVersionString</key>").nth(1)?;
            let value = after.split("<string>").nth(1)?.split("</string>").next()?;
            Some(value.trim().to_string())
        })
        .filter(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .unwrap_or_else(|| "18.0".into());
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{version} Safari/605.1.15"
    )
}

/// A new tab loading `url` (none: a page WebKit loads itself, a pop-up's), in the browser's data store.
fn new_tab(url: Option<&str>, owner: Option<Owner>) -> Result<Rc<Tab>, String> {
    new_tab_as(next_id(), url, owner, None)
}

/// `new_tab` with id `id` (and the tab that opened it), in the engine new pages open in (Settings › Browser).
fn new_tab_as(id: String, url: Option<&str>, owner: Option<Owner>, opener: Option<String>) -> Result<Rc<Tab>, String> {
    if engine::current() == engine::Kind::Chromium {
        if tabs().len() >= MAX_TABS {
            return Err(format!(
                "The browser has {MAX_TABS} tabs open, its most: close some you're done with (browser_close)."
            ));
        }
        let (page, view) = chromium::open(&id, url.unwrap_or("about:blank"), PAGE_SIZE)?;
        return add_chromium_tab(id, owner, opener, page, view);
    }
    new_tab_with(id, url, owner, opener, |builder| builder.with_data_store_identifier(store_id()))
}

/// Opens tabs again in the engine new pages open in now: those in the other one (Settings › Browser switched it), and
/// with `chromium`, every tab in Chromium too (the engine stopped under them). Each opens at its address with its id,
/// owner, opener and agent's work; the browser window shows the same tab, and the agent hears of it with its next
/// answer. How many opened again.
pub fn reopen_tabs(chromium: bool) -> usize {
    let now = engine::current();
    let mut reopened = 0;
    for old in tabs() {
        if old.engine() == now && !(chromium && old.engine() == engine::Kind::Chromium) {
            continue;
        }
        let (_, url, _) = old.state();
        let url = if url.is_empty() { "about:blank".to_string() } else { url };
        let Some(index) = BROWSER.with(|browser| browser.borrow().tabs.iter().position(|tab| tab.id == old.id)) else {
            continue;
        };
        // Out of the window and the list first: the new one takes its place.
        old.park.take(old.view(), None);
        BROWSER.with(|browser| browser.borrow_mut().tabs.remove(index));
        match new_tab_as(old.id.clone(), Some(&url), old.owner.clone(), old.opener.clone()) {
            Ok(tab) => {
                *tab.help.borrow_mut() = old.help.borrow_mut().take();
                *tab.upload.borrow_mut() = old.upload.borrow_mut().take();
                *tab.downloads.borrow_mut() = std::mem::take(&mut *old.downloads.borrow_mut());
                *tab.news.borrow_mut() = std::mem::take(&mut *old.news.borrow_mut());
                *tab.work.borrow_mut() = std::mem::take(&mut *old.work.borrow_mut());
                tab.news.borrow_mut().push(format!(
                    "Filo's browser restarted on {} (the user's choice in Settings, or the engine stopped): this tab opened again at {url}. Read the page again (browser_read) before using refs you had.",
                    tab.engine().label()
                ));
                BROWSER.with(|browser| {
                    let mut browser = browser.borrow_mut();
                    let at = browser.tabs.iter().position(|other| other.id == tab.id);
                    if let Some(at) = at {
                        let tab = browser.tabs.remove(at);
                        let index = index.min(browser.tabs.len());
                        browser.tabs.insert(index, tab);
                    }
                });
                reopened += 1;
            }
            Err(error) => eprintln!("browser: couldn't open {} again: {error}", old.id),
        }
    }
    changed();
    keep_awake();
    reopened
}

/// The folder an agent's downloads go to: the local data folder's `downloads` (the user's own go to Downloads, as in
/// a browser).
fn downloads_dir() -> PathBuf {
    local_dir().join("downloads")
}

/// `name` in `dir`, numbered when a file has it already ("report (2).csv").
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_string(), format!(".{extension}")),
        _ => (name.to_string(), String::new()),
    };
    (2..)
        .map(|count| dir.join(format!("{stem} ({count}){extension}")))
        .find(|path| !path.exists())
        .expect("a free name")
}

/// A download begins in tab `id`: an agent's goes to `downloads_dir`, the user's where wry put it (Downloads).
fn download_started(id: &str, url: String, path: &mut PathBuf) -> bool {
    let Some(tab) = tab(id) else { return true };
    if tab.owner.is_some() {
        let dir = downloads_dir();
        let _ = std::fs::create_dir_all(&dir);
        let name =
            path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into());
        *path = unique_path(&dir, &name);
    }
    eprintln!("browser: {id} downloads {url} to {}", path.display());
    tab.downloads.borrow_mut().push(Download {
        url,
        path: path.clone(),
        done: None,
        ended: None,
        told: (false, false),
    });
    changed();
    true
}

fn download_finished(id: &str, url: &str, ok: bool) {
    let Some(tab) = tab(id) else { return };
    if let Some(download) =
        tab.downloads.borrow_mut().iter_mut().rev().find(|download| download.url == url && download.done.is_none())
    {
        download.done = Some(ok);
        download.ended = Some(Instant::now());
    }
    changed();
}

fn tab_of_view(view: *mut objc2::runtime::AnyObject) -> Option<Rc<Tab>> {
    tabs().into_iter().find(|tab| std::ptr::eq(Retained::as_ptr(&tab.view).cast::<objc2::runtime::AnyObject>(), view))
}

/// A page's alert, confirm or prompt (browser/delegate.rs): kept on its tab until it's answered. A web view that's no
/// tab's has it dismissed at once, as before.
fn on_dialog(view: *mut objc2::runtime::AnyObject, dialog: delegate::Dialog) {
    let Some(tab) = tab_of_view(view) else { return };
    eprintln!("browser: {} shows {} dialog", tab.id, dialog.kind.a());
    *tab.dialog.borrow_mut() = Some(dialog);
    changed();
}

/// A page asks for files: what browser_upload gave; else, for a page an agent works or one off the screen, none (the
/// agent is told), and the open panel for the user's page on screen.
fn on_file_chooser(view: *mut objc2::runtime::AnyObject, multiple: bool, _directories: bool) -> delegate::Chooser {
    let Some(tab) = tab_of_view(view) else { return delegate::Chooser::User };
    if let Some(mut files) = tab.upload.borrow_mut().take() {
        if !multiple && files.len() > 1 {
            tab.news.borrow_mut().push(format!("The field takes one file: it got only {}.", files[0].display()));
            files.truncate(1);
        }
        return delegate::Chooser::Files(files);
    }
    let agents = tab.work.borrow().busy > 0;
    if agents || native::parked(tab.view()) {
        tab.news.borrow_mut().push("The page asked for a file and got none: give files with browser_upload (the field's ref, or the button that opens the chooser).".into());
        return delegate::Chooser::Cancel;
    }
    delegate::Chooser::User
}

/// A page closed itself (window.close(), as a sign-in pop-up does when it's done).
fn on_page_closed(view: *mut objc2::runtime::AnyObject) {
    let Some(tab) = tab_of_view(view) else { return };
    eprintln!("browser: {}'s page closed itself", tab.id);
    CLOSING.with(|closing| {
        if let Some(closing) = closing.borrow().as_ref() {
            let _ = closing.unbounded_send(tab.id.clone());
        }
    });
}

/// `new_tab` with the builder's data store, or a pop-up's configuration, set by `configure`; `opener` is the tab a
/// pop-up's page came from.
fn new_tab_with(
    id: String,
    url: Option<&str>,
    owner: Option<Owner>,
    opener: Option<String>,
    configure: impl for<'a> FnOnce(wry::WebViewBuilder<'a>) -> wry::WebViewBuilder<'a>,
) -> Result<Rc<Tab>, String> {
    if tabs().len() >= MAX_TABS {
        return Err(format!(
            "The browser has {MAX_TABS} tabs open, its most: close some you're done with (browser_close)."
        ));
    }
    let park = native::Park::new(PAGE_SIZE.0, PAGE_SIZE.1).ok_or("Couldn't make the page's window")?;
    let (opened_by, opener_owner) = (id.clone(), owner.clone());
    let (started, finished) = (id.clone(), id.clone());
    let mut builder = wry::WebViewBuilder::new()
        .with_user_agent(user_agent())
        // The page runs at full speed while nobody looks at it.
        .with_background_throttling(wry::BackgroundThrottlingPolicy::Disabled)
        .with_back_forward_navigation_gestures(true)
        .with_devtools(cfg!(feature = "dev"))
        .with_bounds(wry::Rect {
            position: wry::dpi::Position::Logical(wry::dpi::LogicalPosition::new(0., 0.)),
            size: wry::dpi::Size::Logical(wry::dpi::LogicalSize::new(PAGE_SIZE.0, PAGE_SIZE.1)),
        })
        // A link to a new window, or a pop-up (a sign-in's): a tab of the same owner, kept as the page's pop-up.
        .with_new_window_req_handler(move |_url, features| popup(&opened_by, opener_owner.clone(), features))
        .with_download_started_handler(move |url, path| download_started(&started, url, path))
        .with_download_completed_handler(move |url, _, ok| download_finished(&finished, &url, ok));
    builder = configure(builder);
    if let Some(url) = url {
        builder = builder.with_url(url);
    }
    let web = native::without_activation(|| builder.build_as_child(&park.handle()))
        .map_err(|error| format!("Couldn't open the page: {error}"))?;
    let view = web.webview();
    park.take(&view, None);
    native::prepare(&view);
    delegate::adopt(&view);
    // SAFETY: a WKWebView is an object.
    let view = unsafe { Retained::cast_unchecked::<objc2::runtime::AnyObject>(view) };
    let tab = Rc::new(Tab {
        id,
        dialog: RefCell::new(None),
        page: Page::WebKit(web),
        view,
        park,
        owner,
        opener,
        created: Instant::now(),
        help: RefCell::new(None),
        upload: RefCell::new(None),
        downloads: RefCell::new(Vec::new()),
        news: RefCell::new(Vec::new()),
        work: RefCell::new(activity::Work::default()),
    });
    BROWSER.with(|browser| {
        let mut browser = browser.borrow_mut();
        browser.tabs.push(tab.clone());
        browser.generation += 1;
    });
    keep_awake();
    Ok(tab)
}

/// The next tab's id ("t4").
fn next_id() -> String {
    BROWSER.with(|browser| {
        let mut browser = browser.borrow_mut();
        browser.next += 1;
        format!("t{}", browser.next)
    })
}

/// A pop-up Chromium opened from tab `opener`'s page: a tab of the same owner (`key`, the id Filo gave it).
fn chromium_popup(
    key: &str,
    opener: &str,
    page: chromium::Page,
    view: Retained<objc2::runtime::AnyObject>,
) -> Result<(), String> {
    if tabs().len() >= MAX_TABS {
        return Err(format!("the browser has {MAX_TABS} tabs open"));
    }
    let owner = tab(opener).and_then(|tab| tab.owner.clone());
    let tab = add_chromium_tab(key.to_string(), owner, Some(opener.to_string()), page, view)?;
    eprintln!("browser: {opener} opened {} as a pop-up", tab.id);
    if let Some(opener) = self::tab(opener) {
        opener.news.borrow_mut().push(format!("The page opened a new tab, {} (a pop-up or a link to a new window): browser_read it there by its id; it's yours like this one.", tab.id));
    }
    Ok(())
}

fn add_chromium_tab(
    id: String,
    owner: Option<Owner>,
    opener: Option<String>,
    page: chromium::Page,
    view: Retained<objc2::runtime::AnyObject>,
) -> Result<Rc<Tab>, String> {
    let park = native::Park::new(PAGE_SIZE.0, PAGE_SIZE.1).ok_or("Couldn't make the page's window")?;
    park.take(&view, None);
    let tab = Rc::new(Tab {
        id,
        dialog: RefCell::new(None),
        page: Page::Chromium(page),
        view,
        park,
        owner,
        opener,
        created: Instant::now(),
        help: RefCell::new(None),
        upload: RefCell::new(None),
        downloads: RefCell::new(Vec::new()),
        news: RefCell::new(Vec::new()),
        work: RefCell::new(activity::Work::default()),
    });
    BROWSER.with(|browser| {
        let mut browser = browser.borrow_mut();
        browser.tabs.push(tab.clone());
        browser.generation += 1;
    });
    keep_awake();
    Ok(tab)
}

fn popup(opener: &str, owner: Option<Owner>, features: wry::NewWindowFeatures) -> wry::NewWindowResponse {
    let configuration = features.opener.target_configuration;
    match new_tab_with(next_id(), None, owner, Some(opener.to_string()), move |builder| {
        builder.with_webview_configuration(configuration)
    }) {
        Ok(tab) => {
            eprintln!("browser: {opener} opened {} as a pop-up", tab.id);
            if let Some(opener) = self::tab(opener) {
                opener.news.borrow_mut().push(format!("The page opened a new tab, {} (a pop-up or a link to a new window): browser_read it there by its id; it's yours like this one.", tab.id));
            }
            // SAFETY: a WebKit tab's view is its WKWebView.
            wry::NewWindowResponse::Create { webview: unsafe { Retained::cast_unchecked(tab.view.clone()) } }
        }
        Err(error) => {
            eprintln!("browser: {opener}'s pop-up: {error}");
            wry::NewWindowResponse::Deny
        }
    }
}

/// Tab `id`'s page in Chromium: its width in points, its scale, and its last frame's width (`dev` builds).
#[cfg(feature = "dev")]
pub fn chromium_drawn(id: &str) -> Option<(f64, f64, usize)> {
    chromium::drawn(tab(id)?.view())
}

/// A page in Chromium's picture for a window screenshot (window_shot.rs): RGBA, `width` × `height`.
#[cfg(feature = "dev")]
pub fn chromium_picture(view: *mut objc2::runtime::AnyObject, width: usize, height: usize) -> Option<Vec<u8>> {
    chromium::picture(view, width, height)
}

/// Prints tab `id`'s page with macOS's print panel, over the browser window (⌘P there, the page's menu or its
/// window.print()): WebKit prints its view, Chromium a PDF it makes of the page.
pub fn print(id: &str) -> Result<(), String> {
    let tab = tab_or_error(id)?;
    if native::parked(tab.view()) {
        return Err("Only a page in the browser window prints.".into());
    }
    let view = Retained::as_ptr(&tab.view) as *mut objc2::runtime::AnyObject;
    match &tab.page {
        Page::WebKit(_) => native::print(tab.view()),
        Page::Chromium(_) => {
            if let Some(page) = chromium::of(tab.view()) {
                page.print(view);
            }
            Ok(())
        }
    }
}

/// A page asked to print (window.print(), its menu's Print): it prints when it's the user's to see, in the browser
/// window with no agent at work there; an agent's hears that it didn't.
fn page_asks_to_print(view: *mut objc2::runtime::AnyObject) {
    let Some(tab) = tab_of_view(view) else { return };
    if native::parked(tab.view()) || tab.working() {
        tab.news
            .borrow_mut()
            .push("The page asked to print: Filo prints only for the user, in the browser window.".into());
        return;
    }
    if let Err(error) = print(&tab.id) {
        eprintln!("browser: couldn't print {}: {error}", tab.id);
    }
}

/// A link or picture the user opened in a new tab from a page's menu: a tab of the user's, shown.
fn open_from_page(url: &str) {
    if url.is_empty() {
        return;
    }
    match open_for_user(url) {
        Ok(tab) => set_shown(Some(tab.id.clone())),
        Err(error) => eprintln!("browser: couldn't open {url}: {error}"),
    }
}

/// Opens a tab for the user (the browser window's +).
pub fn open_for_user(url: &str) -> Result<Rc<Tab>, String> {
    new_tab(Some(&address(url)), None)
}

pub fn close(id: &str) {
    let closed = BROWSER.with(|browser| {
        let mut browser = browser.borrow_mut();
        let index = browser.tabs.iter().position(|tab| tab.id == id)?;
        browser.generation += 1;
        Some(browser.tabs.remove(index))
    });
    if shown().as_deref() == Some(id) {
        SHOWN.with(|shown| *shown.borrow_mut() = None);
    }
    // Its web view and window go once nothing holds it (a tool call may still).
    drop(closed);
    keep_awake();
}

/// Puts the tab's page back in its own window off the screen (the browser window stopped showing it).
pub fn park(id: &str) {
    if let Some(tab) = tab(id) {
        tab.park.take(tab.view(), None);
    }
}

/// Shows the tab's page in the browser window's view `parent` at `frame` (points from its top left).
pub fn place(id: &str, parent: *mut objc2::runtime::AnyObject, frame: (f64, f64, f64, f64)) {
    if let Some(tab) = tab(id) {
        native::show_in(tab.view(), parent, frame);
    }
}

pub fn is_placed(id: &str, parent: *mut objc2::runtime::AnyObject) -> bool {
    tab(id).is_some_and(|tab| native::is_in(tab.view(), parent))
}

pub fn give_keys_to(view: *mut objc2::runtime::AnyObject) {
    native::focus(view);
}

/// Whether the tab's page has the keyboard of the window it's in.
pub fn has_keyboard(id: &str) -> bool {
    tab(id).is_some_and(|tab| native::has_keyboard(tab.view()))
}

/// An address as typed: a search for words, https:// added to a bare host.
pub fn address(typed: &str) -> String {
    let typed = typed.trim();
    if typed.is_empty() {
        return "about:blank".into();
    }
    if typed.contains("://") || typed.starts_with("about:") || typed.starts_with("data:") {
        return typed.to_string();
    }
    let host_like = !typed.contains(char::is_whitespace) && (typed.contains('.') || typed.starts_with("localhost"));
    if host_like {
        let local = typed.starts_with("localhost") || typed.starts_with("127.0.0.1");
        return format!("{}://{typed}", if local { "http" } else { "https" });
    }
    let query: String = url::form_urlencoded::byte_serialize(typed.as_bytes()).collect();
    format!("https://www.google.com/search?q={query}")
}

/// Starts answering the agents' tools (once): the socket in the local data folder, read on a thread of its own, each
/// request answered on the main thread.
pub fn init(cx: &mut App) {
    window::init(cx);
    chromium::init(cx);
    engine::init(cx);
    if SERVING.with(|serving| serving.replace(true)) {
        return;
    }
    let (closing, mut closed) = futures::channel::mpsc::unbounded::<String>();
    CLOSING.with(|slot| *slot.borrow_mut() = Some(closing));
    cx.spawn(async move |cx| {
        while let Some(id) = closed.next().await {
            let opener = tab(&id).and_then(|tab| tab.opener.clone());
            close(&id);
            if let Some(opener) = opener.and_then(|opener| tab(&opener)) {
                opener.news.borrow_mut().push(format!("Tab {id}, this page's pop-up, closed itself."));
            }
            cx.update(|cx| cx.refresh_windows()).ok();
        }
    })
    .detach();
    let path = filo_local::browser_mcp::socket_path(&local_dir());
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<(Value, futures::channel::oneshot::Sender<Value>)>();
    if let Err(error) = listen(&path, tx) {
        eprintln!("browser: no socket for the agents' tools at {}: {error}", path.display());
        return;
    }
    cx.spawn(async move |cx| {
        while let Some((request, reply)) = rx.next().await {
            cx.spawn(async move |cx| {
                let answer = match handle(cx, &request).await {
                    Ok(Reply { text, png }) => {
                        let mut answer = json!({ "ok": true, "text": text });
                        if let Some(png) = png {
                            answer["png"] = json!(base64::engine::general_purpose::STANDARD.encode(png));
                        }
                        answer
                    }
                    Err(error) => json!({ "ok": false, "error": error }),
                };
                let _ = reply.send(answer);
            })
            .detach();
        }
    })
    .detach();
}

fn listen(
    path: &std::path::Path,
    tx: futures::channel::mpsc::UnboundedSender<(Value, futures::channel::oneshot::Sender<Value>)>,
) -> std::io::Result<()> {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A socket left by a Filo that didn't quit cleanly; another Filo running on the same folder would answer it.
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(std::io::Error::other("another Filo answers there"));
    }
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    BOUND.with(|bound| bound.set(socket_file(path)));
    std::thread::Builder::new().name("browser-socket".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                let Ok(reader) = stream.try_clone() else { return };
                if BufReader::new(reader).read_line(&mut line).is_err() {
                    return;
                }
                let request = serde_json::from_str::<Value>(&line).unwrap_or(Value::Null);
                let (reply_tx, reply_rx) = futures::channel::oneshot::channel();
                if tx.unbounded_send((request, reply_tx)).is_err() {
                    return;
                }
                let answer = futures::executor::block_on(reply_rx)
                    .unwrap_or_else(|_| json!({ "ok": false, "error": "Filo's browser stopped." }));
                let mut stream = stream;
                let _ = stream.write_all(format!("{answer}\n").as_bytes());
            });
        }
    })?;
    Ok(())
}

/// Closes every tab and removes the browser's WebKit data store, so a check leaves none under ~/Library/WebKit: only
/// on a test run's own data folder (`dev` builds).
#[cfg(feature = "dev")]
pub async fn remove_test_store(cx: &AsyncApp) {
    if !crate::test_run::own_config() {
        return;
    }
    cx.update(window::close).ok();
    for tab in tabs() {
        close(&tab.id);
    }
    let id = store_id();
    // WebKit lets go of the closed pages' store a moment later.
    let mut said = String::from("it didn't answer");
    for _ in 0..8 {
        sleep(cx, Duration::from_millis(750)).await;
        let (tx, rx) = futures::channel::oneshot::channel();
        cx.update(|_| {
            <wry::WebView as wry::WebViewExtDarwin>::remove_data_store(&id, move |result| {
                drop(tx.send(result.map_err(|error| error.to_string())))
            })
        })
        .ok();
        match futures::future::select(rx, Box::pin(sleep(cx, Duration::from_secs(5)))).await {
            futures::future::Either::Left((Ok(Ok(())), _)) => {
                println!("browser: removed the test run's WebKit data store");
                return;
            }
            futures::future::Either::Left((Ok(Err(error)), _)) => said = error,
            _ => {}
        }
    }
    println!("browser: couldn't remove the test run's WebKit data store: {said}");
}

/// Takes the socket away when Filo quits.
pub fn quit() {
    take_socket_away(&filo_local::browser_mcp::socket_path(&local_dir()));
}

fn take_socket_away(path: &std::path::Path) {
    if BOUND.with(Cell::get).is_some_and(|bound| socket_file(path) == Some(bound)) {
        let _ = std::fs::remove_file(path);
    }
}

fn socket_file(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
}

struct Reply {
    text: String,
    png: Option<Vec<u8>>,
}

impl From<String> for Reply {
    fn from(text: String) -> Self {
        Reply { text, png: None }
    }
}

fn text_arg<'a>(request: &'a Value, key: &str) -> Option<&'a str> {
    request.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

fn number(request: &Value, key: &str) -> Option<f64> {
    request.get(key).and_then(Value::as_f64)
}

async fn sleep(cx: &AsyncApp, time: Duration) {
    cx.background_executor().timer(time).await;
}

/// Waits for what the last action loads: a moment for it to start, then until it's done (or `LOAD_WAIT`).
async fn settle(cx: &AsyncApp, tab: &Tab) {
    let start = Instant::now();
    sleep(cx, Duration::from_millis(300)).await;
    while tab.state().2 && start.elapsed() < LOAD_WAIT {
        sleep(cx, Duration::from_millis(100)).await;
    }
}

/// What the agent is told of a dialog waiting on the page.
fn dialog_note(tab: &Tab) -> Option<String> {
    let (_, message) = tab.dialog()?;
    let kind = tab.dialog_a()?;
    Some(format!(
        "The page shows {kind} dialog: “{message}”. Its script waits until it's answered: answer it with browser_dialog (accept or dismiss) first."
    ))
}

/// A picture of what the tab shows (`native::snapshot`). A page with a dialog up can't draw one (its script waits), so
/// that's the answer then, and the wait for it has an end.
async fn snapshot(cx: &AsyncApp, tab: &Tab) -> Result<image::RgbaImage, String> {
    if let Some(note) = dialog_note(tab) {
        return Err(note);
    }
    let (tx, rx) = futures::channel::oneshot::channel();
    native::snapshot(tab.view(), move |result| {
        let _ = tx.send(result);
    });
    match futures::future::select(rx, Box::pin(sleep(cx, Duration::from_secs(15)))).await {
        futures::future::Either::Left((Ok(result), _)) => result,
        futures::future::Either::Left((Err(_), _)) => Err("The screenshot didn't come.".into()),
        futures::future::Either::Right(_) => {
            Err(dialog_note(tab).unwrap_or_else(|| "The page didn't draw a screenshot in 15 s (busy).".into()))
        }
    }
}

/// Waits while the tab loads, and for a pop-up just opened, until its page begins (it's blank a moment first).
async fn ready(cx: &AsyncApp, tab: &Tab) {
    let start = Instant::now();
    while start.elapsed() < LOAD_WAIT {
        let (_, url, loading) = tab.state();
        let fresh = tab.opener.is_some()
            && (url.is_empty() || url == "about:blank")
            && tab.created.elapsed() < Duration::from_secs(5);
        if !loading && !fresh {
            break;
        }
        sleep(cx, Duration::from_millis(100)).await;
    }
}

/// Runs one of page.js's ops in the tab's page. A dialog the page shows (meanwhile, or already) stops its scripts, so
/// that's the answer then.
async fn page(cx: &AsyncApp, tab: &Tab, op: &str, mut arg: Value) -> Result<Value, String> {
    if let Some(note) = dialog_note(tab) {
        return Err(note);
    }
    arg["op"] = json!(op);
    let (tx, rx) = futures::channel::oneshot::channel();
    native::run_js(tab.view(), PAGE_JS, &arg.to_string(), move |result| {
        let _ = tx.send(result);
    });
    let timeout = sleep(cx, Duration::from_secs(30));
    let dialog = async {
        while tab.dialog.borrow().is_none() {
            sleep(cx, Duration::from_millis(100)).await;
        }
    };
    let stop = futures::future::select(Box::pin(timeout), Box::pin(dialog));
    let text = match futures::future::select(rx, stop).await {
        futures::future::Either::Left((Ok(result), _)) => {
            result.map_err(|error| format!("The page's script failed: {error}"))?
        }
        futures::future::Either::Left((Err(_), _)) => return Err("The page didn't answer.".into()),
        futures::future::Either::Right((futures::future::Either::Right(_), _)) => {
            return Err(dialog_note(tab).unwrap_or_default());
        }
        futures::future::Either::Right(_) => return Err("The page didn't answer in 30 s (busy).".into()),
    };
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(value)
}

fn line_of(value: &Value) -> String {
    value.get("line").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// "Tab t3: title — address" (and "(still loading)"), then what the agent hasn't been told yet (`news`).
fn where_now(tab: &Tab) -> String {
    let (title, url, loading) = tab.state();
    let title = if title.is_empty() { "(no title)".into() } else { title };
    let mut text = format!("Tab {}: {title} — {url}{}", tab.id, if loading { " (still loading)" } else { "" });
    for line in news(tab) {
        text.push('\n');
        text.push_str(&line);
    }
    text
}

/// What happened in the tab since the agent was last told: pop-ups, downloads, a file chooser left empty, and a dialog
/// waiting (said each time until it's answered).
fn news(tab: &Tab) -> Vec<String> {
    let mut lines: Vec<String> = tab.news.borrow_mut().drain(..).collect();
    for download in tab.downloads.borrow_mut().iter_mut() {
        let name = download.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        match download.done {
            Some(true) if !download.told.1 => {
                let size = std::fs::metadata(&download.path)
                    .map(|meta| crate::format::file_size(meta.len() as f64))
                    .unwrap_or_default();
                lines.push(format!("Downloaded {name} ({size}) to {}", download.path.display()));
                download.told = (true, true);
            }
            Some(false) if !download.told.1 => {
                lines.push(format!("The download of {name} ({}) failed.", download.url));
                download.told = (true, true);
            }
            None if !download.told.0 => {
                lines.push(format!(
                    "Downloading {name} to {} (browser_wait with `for` \"download\" waits for it).",
                    download.path.display()
                ));
                download.told.0 = true;
            }
            _ => {}
        }
    }
    lines.extend(dialog_note(tab));
    lines
}

/// A ring where an agent clicks, on a page the user has on screen (`native::flash`), gone once it faded.
fn flash(cx: &AsyncApp, tab: &Tab, x: f64, y: f64, radius: f64) {
    if let Some(ring) = native::flash(tab.view(), x, y, radius) {
        cx.spawn(async move |cx| {
            sleep(cx, Duration::from_millis(1000)).await;
            native::remove_view(&ring);
        })
        .detach();
    }
}

/// A real click at (x, y) of the page: the pointer moved there, then pressed and let go (twice for a double-click).
async fn click_at(cx: &AsyncApp, tab: &Tab, x: f64, y: f64, double: bool, right: bool) -> Result<(), String> {
    let (width, height) = native::viewport(tab.view());
    if !(0. ..width).contains(&x) || !(0. ..height).contains(&y) {
        return Err(format!(
            "({x}, {y}) is outside the page's view, which is {width}×{height}: scroll, or take a new screenshot."
        ));
    }
    native::mouse(tab.view(), native::Mouse::Move, x, y, 0);
    flash(cx, tab, x, y, 18.);
    sleep(cx, Duration::from_millis(40)).await;
    let (down, up) = if right {
        (native::Mouse::RightDown, native::Mouse::RightUp)
    } else {
        (native::Mouse::Down, native::Mouse::Up)
    };
    for count in 1..=if double { 2 } else { 1 } {
        if !native::mouse(tab.view(), down, x, y, count) {
            return Err("Couldn't click: the page has no window.".into());
        }
        sleep(cx, Duration::from_millis(60)).await;
        native::mouse(tab.view(), up, x, y, count);
        sleep(cx, Duration::from_millis(50)).await;
    }
    Ok(())
}

/// How a drag went.
enum Dragged {
    /// Pressed, moved and let go: what that does is the page's own (a slider, a list that follows the pointer).
    Mouse { moves: usize },
    /// The page made it a drag and drop (a draggable element), carried through with WebKit's drag events; `taken`
    /// when WebKit dropped it where it was let go (the page said it would take it there).
    Dropped { moves: usize, taken: bool },
}

/// The spots a drag goes through after it's pressed at `from`: a few short steps first, as a hand starts (a page's
/// threshold sees the drag begin near where it was pressed), then about every 12 points to `to`.
fn drag_path(from: (f64, f64), to: (f64, f64)) -> Vec<(f64, f64)> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let distance = dx.hypot(dy);
    if distance < 1. {
        return vec![to];
    }
    let mut along: Vec<f64> = [2., 5., 9.].into_iter().filter(|start| *start < distance / 2.).collect();
    let start = along.last().copied().unwrap_or(0.);
    let steps = ((distance - start) / 12.).ceil().clamp(6., 80.) as usize;
    along.extend((1..=steps).map(|step| start + (distance - start) * step as f64 / steps as f64));
    along.iter().map(|at| (from.0 + dx * at / distance, from.1 + dy * at / distance)).collect()
}

/// A real drag from `from` to `to` of the page: the pointer pressed at `from`, moved there step by step, held a
/// moment and let go. When the page makes it a drag and drop, the rest goes on with WebKit's drag events
/// (browser/drag.rs), never with a drag session on the user's screen.
async fn drag(cx: &AsyncApp, tab: &Tab, from: (f64, f64), to: (f64, f64)) -> Result<Dragged, String> {
    let (width, height) = native::viewport(tab.view());
    for (x, y) in [from, to] {
        if !(0. ..width).contains(&x) || !(0. ..height).contains(&y) {
            return Err(format!(
                "({x}, {y}) is outside the page's view, which is {width}×{height}: scroll, or take a new screenshot."
            ));
        }
    }
    let mut drag = drag::Drag::begin(tab.view())?;
    // Chromium carries the page's own drag and drop in the page: Filo's world watches it start and end.
    let chromium = chromium::of(tab.view()).is_some();
    if chromium {
        run_quietly(cx, tab, DRAG_WATCH).await;
    }
    native::mouse(tab.view(), native::Mouse::Move, from.0, from.1, 0);
    flash(cx, tab, from.0, from.1, 14.);
    sleep(cx, Duration::from_millis(40)).await;
    if !native::mouse(tab.view(), native::Mouse::Down, from.0, from.1, 1) {
        return Err("Couldn't drag: the page has no window.".into());
    }
    sleep(cx, Duration::from_millis(120)).await;
    let path = drag_path(from, to);
    let mut operation = 0;
    let mut step = |drag: &mut drag::Drag, (x, y): (f64, f64)| {
        if drag.dropping() {
            operation = drag.moved(x, y);
        } else {
            native::mouse(tab.view(), native::Mouse::Dragged, x, y, 1);
        }
    };
    for (index, &spot) in path.iter().enumerate() {
        step(&mut drag, spot);
        sleep(cx, Duration::from_millis(if index < 4 { 40 } else { 16 })).await;
    }
    // A moment at the end, as a hand stops before letting go, for the page (and its drop target) to catch up.
    for _ in 0..4 {
        step(&mut drag, to);
        sleep(cx, Duration::from_millis(60)).await;
    }
    let moves = path.len();
    flash(cx, tab, to.0, to.1, 18.);
    if chromium {
        drop(drag);
        native::mouse(tab.view(), native::Mouse::Up, to.0, to.1, 1);
        sleep(cx, Duration::from_millis(150)).await;
        let watched: Value = serde_json::from_str(&run_quietly(cx, tab, DRAG_WATCHED).await).unwrap_or(Value::Null);
        return Ok(if watched["started"] == true {
            Dragged::Dropped { moves, taken: watched["effect"].as_str().is_some_and(|effect| effect != "none") }
        } else {
            Dragged::Mouse { moves }
        });
    }
    if drag.dropping() {
        let taken = drag.let_go(operation);
        drop(drag);
        // The press let go for WebKit too, which AppKit's drag session would swallow: WebKit keeps the pointer it
        // cancelled at dragstart until a release, and the page got no pointerdown at the next press.
        sleep(cx, Duration::from_millis(50)).await;
        native::mouse(tab.view(), native::Mouse::Up, to.0, to.1, 1);
        Ok(Dragged::Dropped { moves, taken })
    } else {
        native::mouse(tab.view(), native::Mouse::Up, to.0, to.1, 1);
        Ok(Dragged::Mouse { moves })
    }
}

/// Watches the page's drag and drop from Filo's world: whether one started, and its drop effect at its end ("none":
/// the drop wasn't taken).
const DRAG_WATCH: &str = "window.__filoDrag = { started: false, effect: '' };
if (!window.__filoDragWatched) {
  window.__filoDragWatched = true;
  document.addEventListener('dragstart', () => { window.__filoDrag.started = true; }, true);
  document.addEventListener('dragend', (e) => { window.__filoDrag.effect = e.dataTransfer ? e.dataTransfer.dropEffect : 'none'; }, true);
}
return '';";
const DRAG_WATCHED: &str = "return JSON.stringify(window.__filoDrag || {});";

/// A script of Filo's own in the page's world (`native::run_js`): its answer, or nothing.
async fn run_quietly(cx: &AsyncApp, tab: &Tab, body: &str) -> String {
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.update(|_| native::run_js(tab.view(), body, "", move |result| drop(tx.send(result)))).ok();
    rx.await.ok().and_then(Result::ok).unwrap_or_default()
}

/// Real key presses typing `text`.
async fn type_text(cx: &AsyncApp, tab: &Tab, text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_TYPED {
        return Err(format!("That's over {MAX_TYPED} characters to type: type it in parts."));
    }
    for character in text.chars() {
        if !native::key(tab.view(), &native::Key::typing(character)) {
            return Err("Couldn't type: the page has no window.".into());
        }
        sleep(cx, Duration::from_millis(12)).await;
    }
    Ok(())
}

/// The ops that change a page, which wait while the user has paused the agent there or taken the tab over.
fn acts(op: &str) -> bool {
    !matches!(op, "open" | "read" | "screenshot" | "tabs" | "ask_user" | "wait")
}

/// Waits while the user holds the tab (Pause, Take over), at most `CONTROL_WAIT`. Says what to tell the agent once
/// it's let go on.
async fn wait_for_control(cx: &AsyncApp, tab: &Tab) -> Result<Option<&'static str>, String> {
    let start = Instant::now();
    loop {
        let control = tab.control();
        if control == Control::Agent {
            return Ok(tab.work.borrow_mut().told.take());
        }
        if self::tab(&tab.id).is_none() {
            return Err(format!("The user closed tab {}.", tab.id));
        }
        if start.elapsed() >= CONTROL_WAIT {
            return Err(match control {
                Control::Paused => format!(
                    "The user paused you in tab {} (Pause in Filo's browser) and hasn't let you go on yet: call again to keep waiting, or say in your reply what you'd do next.",
                    tab.id
                ),
                _ => format!(
                    "The user took tab {} over and is using it themselves; it's yours again when they hand it back: call again to keep waiting, or go on with something else.",
                    tab.id
                ),
            });
        }
        sleep(cx, Duration::from_millis(300)).await;
    }
}

/// Redraws Filo's windows (the browser window's bar, the agent's badge in its chat) as an agent starts or stops
/// working, and once more when it stops counting as working.
fn redraw(cx: &AsyncApp, later: bool) {
    changed();
    cx.update(|cx| cx.refresh_windows()).ok();
    if later {
        cx.spawn(async move |cx| {
            sleep(cx, activity::WORKING_FOR + Duration::from_millis(200)).await;
            changed();
            cx.update(|cx| cx.refresh_windows()).ok();
        })
        .detach();
    }
}

/// The agent `agent` working in a tab now, or held there by the user: the tab and who has it (its chat's badge).
pub fn working_for(agent: &str) -> Option<(String, Control)> {
    tabs()
        .into_iter()
        .find(|tab| tab.agent().as_deref() == Some(agent) && tab.agent_shown())
        .map(|tab| (tab.id.clone(), tab.control()))
}

/// A tool call: who works where for the user to see (browser/activity.rs), the wait while the user holds the tab, then
/// the op itself (`act`).
async fn handle(cx: &mut AsyncApp, request: &Value) -> Result<Reply, String> {
    let op = request.get("op").and_then(Value::as_str).unwrap_or_default().to_string();
    let agent = text_arg(request, "agent").unwrap_or("An agent").to_string();
    let target = if op == "tabs" || op == "open" { None } else { tab(text_arg(request, "tab").unwrap_or_default()) };
    let mut told = None;
    if let Some(tab) = &target {
        if acts(&op) {
            told = wait_for_control(cx, tab).await?;
        }
        let mut work = tab.work.borrow_mut();
        work.agent = Some(agent.clone());
        work.busy += 1;
        drop(work);
        redraw(cx, false);
    }
    let result = act(cx, request).await;
    // A new tab is the agent's from its first step.
    let opened = (op == "open")
        .then(|| result.as_ref().ok()?.text.strip_prefix("Opened tab ")?.split('.').next().and_then(tab))
        .flatten();
    let worked = target.clone().or(opened);
    if let Some(tab) = &worked {
        let mut work = tab.work.borrow_mut();
        work.agent = Some(agent.clone());
        work.busy = work.busy.saturating_sub(u32::from(target.is_some()));
        work.last = Some(Instant::now());
    }
    let tab_id = worked.as_ref().map(|tab| tab.id.clone()).or_else(|| text_arg(request, "tab").map(String::from));
    if let (Some(tab_id), Some(step)) = (
        tab_id,
        activity::step_of(&op, request, result.as_ref().map(|reply| reply.text.as_str()).map_err(String::as_str)),
    ) {
        activity::record(&agent, &tab_id, step);
    }
    if worked.is_some() {
        redraw(cx, true);
    }
    result.map(|mut reply| {
        if let Some(told) = told {
            reply.text = format!("{told}\n{}", reply.text);
        }
        reply
    })
}

async fn act(cx: &mut AsyncApp, request: &Value) -> Result<Reply, String> {
    let op = request.get("op").and_then(Value::as_str).unwrap_or_default();
    let owner = || Owner {
        agent: text_arg(request, "agent").unwrap_or("An agent").to_string(),
        chat: text_arg(request, "chat").unwrap_or_default().to_string(),
    };
    let tab_id = text_arg(request, "tab").unwrap_or_default();
    let reply: Reply = match op {
        "open" => {
            let url = text_arg(request, "url").ok_or("Give the page's `url`.")?;
            let url = address(url);
            if !(url.starts_with("http://") || url.starts_with("https://") || url == "about:blank") {
                return Err(format!("{url}: only web addresses (http and https) open in the browser."));
            }
            let tab = new_tab(Some(&url), Some(owner()))?;
            settle(cx, &tab).await;
            let read = page(cx, &tab, "read", json!({}))
                .await
                .map(|value| value["text"].as_str().unwrap_or_default().to_string());
            format!(
                "Opened tab {}.\n{}",
                tab.id,
                read.unwrap_or_else(|error| format!("{}\n(couldn't read it: {error})", where_now(&tab)))
            )
            .into()
        }
        "navigate" => {
            let tab = tab_or_error(tab_id)?;
            match (text_arg(request, "url"), text_arg(request, "go")) {
                (Some(url), _) => {
                    let url = address(url);
                    if !(url.starts_with("http://") || url.starts_with("https://") || url == "about:blank") {
                        return Err(format!("{url}: only web addresses (http and https) open in the browser."));
                    }
                    tab.load(&url);
                }
                (None, Some(how @ ("back" | "forward" | "reload"))) => tab.go(how),
                _ => return Err("Give a `url`, or `go`: back, forward or reload.".into()),
            }
            settle(cx, &tab).await;
            where_now(&tab).into()
        }
        "read" => {
            let tab = tab_or_error(tab_id)?;
            ready(cx, &tab).await;
            let value = page(cx, &tab, "read", json!({ "find": text_arg(request, "find") })).await?;
            let news = news(&tab);
            let news = if news.is_empty() { String::new() } else { format!("\n\n{}", news.join("\n")) };
            format!("Tab {}: {}{news}", tab.id, value["text"].as_str().unwrap_or_default()).into()
        }
        "click" => {
            let tab = tab_or_error(tab_id)?;
            let reference = text_arg(request, "ref").ok_or("Give the element's `ref`, like e5 (from browser_read).")?;
            let target = page(cx, &tab, "target", json!({ "ref": reference })).await?;
            if target["select"] == true {
                return Err(format!(
                    "{} is a menu: pick its option with browser_select (a click would open a menu only the user can work).",
                    line_of(&target)
                ));
            }
            // A moment for the scroll to land, then the pointer onto it, and the element found again: what the pointer
            // shows as it passes (a menu on hover) can move it.
            sleep(cx, Duration::from_millis(80)).await;
            let target = if target["hit"] == true {
                native::mouse(
                    tab.view(),
                    native::Mouse::Move,
                    target["x"].as_f64().unwrap_or(0.),
                    target["y"].as_f64().unwrap_or(0.),
                    0,
                );
                sleep(cx, Duration::from_millis(60)).await;
                page(cx, &tab, "target", json!({ "ref": reference })).await?
            } else {
                target
            };
            let how = if target["hit"].as_bool() == Some(true) {
                click_at(
                    cx,
                    &tab,
                    target["x"].as_f64().unwrap_or(0.),
                    target["y"].as_f64().unwrap_or(0.),
                    false,
                    false,
                )
                .await?;
                "with real mouse events"
            } else {
                flash(cx, &tab, target["x"].as_f64().unwrap_or(-1.), target["y"].as_f64().unwrap_or(-1.), 18.);
                page(cx, &tab, "click", json!({ "ref": reference })).await?;
                "through the page (something covers it, or it isn't shown)"
            };
            settle(cx, &tab).await;
            format!("Clicked {} {how}.\n{}", line_of(&target), where_now(&tab)).into()
        }
        "type" => {
            let tab = tab_or_error(tab_id)?;
            let reference = text_arg(request, "ref").ok_or("Give the field's `ref`, like e7 (from browser_read).")?;
            let text = request.get("text").and_then(Value::as_str).unwrap_or_default();
            let clear = request.get("clear").and_then(Value::as_bool).unwrap_or(true);
            let submit = request.get("submit").and_then(Value::as_bool).unwrap_or(false);
            let focus = page(cx, &tab, "focus", json!({ "ref": reference, "clear": clear })).await?;
            let mut how = "with real key presses";
            if focus["kind"] == "select" {
                page(cx, &tab, "choose", json!({ "ref": reference, "text": text })).await?;
                how = "by picking the option";
            } else {
                if focus["focused"].as_bool() != Some(true) {
                    // It took no focus from the page's script: a real click on it.
                    let target = page(cx, &tab, "target", json!({ "ref": reference })).await?;
                    if target["hit"].as_bool() == Some(true) {
                        click_at(
                            cx,
                            &tab,
                            target["x"].as_f64().unwrap_or(0.),
                            target["y"].as_f64().unwrap_or(0.),
                            false,
                            false,
                        )
                        .await?;
                        if clear {
                            native::key(tab.view(), &native::Key::named("cmd+a").expect("cmd+a"));
                        }
                    }
                }
                if text.is_empty() && clear {
                    native::key(tab.view(), &native::Key::named("Backspace").expect("Backspace"));
                } else {
                    type_text(cx, &tab, text).await?;
                }
                sleep(cx, Duration::from_millis(60)).await;
                let value = page(cx, &tab, "value", json!({ "ref": reference })).await?;
                let now = value["value"].as_str().unwrap_or_default();
                if !now.contains(text) && !text.contains('\n') {
                    page(cx, &tab, "fill", json!({ "ref": reference, "text": text, "clear": clear })).await?;
                    how = "by setting it through the page (the key presses didn't reach it)";
                }
            }
            if submit {
                native::key(tab.view(), &native::Key::named("Enter").expect("Enter"));
                settle(cx, &tab).await;
            }
            let line = page(cx, &tab, "focus_line", json!({})).await.map(|value| line_of(&value)).unwrap_or_default();
            let after = if submit { format!("\nPressed Return. {}", where_now(&tab)) } else { String::new() };
            format!("Typed into {} {how}.{after}\nFocus: {line}", line_of(&focus)).into()
        }
        "screenshot" if request.get("full").and_then(Value::as_bool) == Some(true) => {
            let tab = tab_or_error(tab_id)?;
            let size = page(cx, &tab, "size", json!({})).await?;
            let (width, height) = (size["width"].as_f64().unwrap_or(0.), size["height"].as_f64().unwrap_or(0.));
            let (view_width, view_height) = native::viewport(tab.view());
            let full = height.min(FULL_MAX).round().max(view_height.round());
            // WebKit draws only what shows: the page scrolled a view at a time from its top, each piece put where it
            // was, then the page back where it was.
            let mut picture =
                image::RgbaImage::from_pixel(view_width.round() as u32, full as u32, image::Rgba([255, 255, 255, 255]));
            let mut at = 0.;
            let pieced = loop {
                let moved = page(cx, &tab, "scroll_to", json!({ "x": size["x"], "y": at }))
                    .await
                    .and_then(|value| value["y"].as_f64().ok_or_else(|| "The page didn't scroll.".to_string()));
                let top = match moved {
                    Ok(top) => top,
                    Err(error) => break Err(error),
                };
                sleep(cx, Duration::from_millis(if at == 0. { 250 } else { 120 })).await;
                match snapshot(cx, &tab).await {
                    Ok(piece) => image::imageops::replace(&mut picture, &piece, 0, top.round() as i64),
                    Err(error) => break Err(error),
                }
                at = top + view_height;
                if at >= full || top + view_height >= height {
                    break Ok(());
                }
            };
            page(cx, &tab, "scroll_to", json!({ "x": size["x"], "y": size["y"] })).await.ok();
            pieced?;
            let png = native::png(&picture)?;
            let cut = if height > full {
                format!(" The page is {} tall: this is its first {full}.", height.round())
            } else {
                String::new()
            };
            let wide = if width > view_width + 1. {
                format!(" It's {} wide; the part right of the view isn't in it.", width.round())
            } else {
                String::new()
            };
            Reply {
                text: format!(
                    "{}\nThe whole page from its top, {}×{full}, one pixel per page pixel, pieced from views scrolled down it (a bar that stays on screen shows in each).{cut}{wide} Its spots are page coordinates, not the view's: to click one, scroll it into view (browser_scroll) and take a plain screenshot for its spot.",
                    where_now(&tab),
                    view_width.round()
                ),
                png: Some(png),
            }
        }
        "screenshot" => {
            let tab = tab_or_error(tab_id)?;
            ready(cx, &tab).await;
            let png = native::png(&snapshot(cx, &tab).await?)?;
            let (width, height) = native::viewport(tab.view());
            let scrolled = page(cx, &tab, "scroll", json!({}))
                .await
                .ok()
                .and_then(|value| value["text"].as_str().map(String::from))
                .unwrap_or_default();
            Reply {
                text: format!(
                    "{}\nThe picture is {}×{}: one pixel per page pixel, so browser_click_at and browser_drag take x and y as they are in it. {scrolled}",
                    where_now(&tab),
                    width.round(),
                    height.round()
                ),
                png: Some(png),
            }
        }
        "click_at" => {
            let tab = tab_or_error(tab_id)?;
            let (Some(x), Some(y)) = (number(request, "x"), number(request, "y")) else {
                return Err("Give `x` and `y`.".into());
            };
            let at = page(cx, &tab, "at", json!({ "x": x, "y": y })).await.unwrap_or_default();
            if at["select"] == true {
                return Err(format!(
                    "({x}, {y}) is on {}, a menu: pick its option with browser_select (a click would open a menu only the user can work).",
                    line_of(&at)
                ));
            }
            let what = line_of(&at);
            let double = request.get("double").and_then(Value::as_bool).unwrap_or(false);
            let right = text_arg(request, "button") == Some("right");
            click_at(cx, &tab, x, y, double, right).await?;
            settle(cx, &tab).await;
            let focus = page(cx, &tab, "focus_line", json!({})).await.map(|value| line_of(&value)).unwrap_or_default();
            format!(
                "{} at ({x}, {y}), on {what}, with real mouse events.\n{}\nFocus: {focus}",
                if double { "Double-clicked" } else { "Clicked" },
                where_now(&tab)
            )
            .into()
        }
        "drag" => {
            let tab = tab_or_error(tab_id)?;
            let end = |at: &str| match (
                text_arg(request, &format!("{at}_ref")),
                number(request, &format!("{at}_x")),
                number(request, &format!("{at}_y")),
            ) {
                (Some(reference), _, _) => Ok(json!({ "ref": reference })),
                (None, Some(x), Some(y)) => Ok(json!({ "x": x, "y": y })),
                _ => Err(format!(
                    "Give where the drag {}: `{at}_ref` (from browser_read), or `{at}_x` and `{at}_y` (a spot of browser_screenshot's picture).",
                    if at == "from" { "starts" } else { "ends" }
                )),
            };
            let (from, to) = (end("from")?, end("to")?);
            let ends = page(cx, &tab, "drag_ends", json!({ "from": from, "to": to })).await?;
            let spot = |at: &str| (ends[at]["x"].as_f64().unwrap_or(0.), ends[at]["y"].as_f64().unwrap_or(0.));
            let (start, finish) = (spot("from"), spot("to"));
            page(cx, &tab, "drag_watch", json!({})).await?;
            // A moment for a scroll to land before pressing where it put the element.
            sleep(cx, Duration::from_millis(80)).await;
            let dragged = drag(cx, &tab, start, finish).await?;
            // The page's own word on the drop, when it ended in the top page (a frame's isn't seen there).
            let seen = page(cx, &tab, "drag_seen", json!({})).await.unwrap_or_default();
            let how = match dragged {
                Dragged::Mouse { moves } => {
                    format!("with real mouse events: pressed, moved in {moves} steps and let go")
                }
                Dragged::Dropped { moves, taken } => {
                    let end = match (seen["taken"].as_bool(), seen["dropped"].as_bool()) {
                        (Some(true), _) => "and took the drop there",
                        (_, Some(true)) => "and got the drop there (its drop event ran)",
                        (None, _) if taken => "and dropped it there",
                        _ => "but didn't take the drop there: nothing was dropped",
                    };
                    format!("in {moves} steps; the page made it a drag and drop (real drag events), {end}")
                }
            };
            settle(cx, &tab).await;
            let rounded = |(x, y): (f64, f64)| format!("({}, {})", x.round(), y.round());
            format!(
                "Dragged from {}, on {}, to {}, on {}, {how}.\n{}\nbrowser_read or browser_screenshot shows what it did.",
                rounded(start),
                line_of(&ends["from"]),
                rounded(finish),
                line_of(&ends["to"]),
                where_now(&tab)
            )
            .into()
        }
        "keys" => {
            let tab = tab_or_error(tab_id)?;
            let text = request.get("text").and_then(Value::as_str).unwrap_or_default();
            let keys: Vec<String> = request
                .get("keys")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            if text.is_empty() && keys.is_empty() {
                return Err("Give `text` to type or `keys` to press.".into());
            }
            let parsed = keys.iter().map(|spec| native::Key::named(spec).ok_or_else(|| format!("No key \"{spec}\": use names like Enter, Tab, Escape, Backspace, ArrowDown, PageDown, Space, or combinations like cmd+a."))).collect::<Result<Vec<_>, _>>()?;
            // Space, Return and the arrows on a menu (<select>) open AppKit's menu, which only the user can work.
            let opens_menu = parsed.iter().any(|key| matches!(key.code, 49 | 36 | 125 | 126)) || text.contains(' ');
            if opens_menu && page(cx, &tab, "focus_line", json!({})).await.is_ok_and(|focus| focus["select"] == true) {
                return Err(
                    "The focus is on a menu (<select>): pick its option with browser_select instead of keys.".into()
                );
            }
            type_text(cx, &tab, text).await?;
            for key in &parsed {
                native::key(tab.view(), key);
                sleep(cx, Duration::from_millis(40)).await;
            }
            settle(cx, &tab).await;
            let focus = page(cx, &tab, "focus_line", json!({})).await.map(|value| line_of(&value)).unwrap_or_default();
            let mut done = Vec::new();
            if !text.is_empty() {
                done.push(format!("typed {} characters", text.chars().count()));
            }
            if !keys.is_empty() {
                done.push(format!("pressed {}", keys.join(", ")));
            }
            format!("With real key presses: {}.\n{}\nFocus: {focus}", done.join(" and "), where_now(&tab)).into()
        }
        "scroll" => {
            let tab = tab_or_error(tab_id)?;
            let value = page(
                cx,
                &tab,
                "scroll",
                json!({ "ref": text_arg(request, "ref"), "x": number(request, "x"), "y": number(request, "y") }),
            )
            .await?;
            format!("{}\n{}", where_now(&tab), value["text"].as_str().unwrap_or_default()).into()
        }
        "tabs" => {
            let shown = shown();
            let lines: Vec<String> = tabs()
                .iter()
                .map(|tab| {
                    let (title, url, _) = tab.state();
                    let mine =
                        tab.owner.as_ref().is_some_and(|owner| Some(owner.chat.as_str()) == text_arg(request, "chat"));
                    let who = if mine { "you".to_string() } else { tab.who() };
                    let on_screen =
                        if shown.as_deref() == Some(tab.id.as_str()) { ", on the user's screen" } else { "" };
                    let popup = tab.opener.as_ref().map(|opener| format!(", a pop-up of {opener}")).unwrap_or_default();
                    let held = match tab.control() {
                        Control::Paused => ", the user paused agents here",
                        Control::User => ", the user took it over",
                        Control::Agent => "",
                    };
                    let dialog = tab.dialog_a().map(|kind| format!(", showing {kind} dialog")).unwrap_or_default();
                    format!(
                        "{}: {} — {url} (opened by {who}{popup}{on_screen}{held}{dialog})",
                        tab.id,
                        if title.is_empty() { "(no title)" } else { &title }
                    )
                })
                .collect();
            if lines.is_empty() { "No tabs are open.".to_string() } else { lines.join("\n") }.into()
        }
        "close" => {
            let tab = tab_or_error(tab_id)?;
            let id = tab.id.clone();
            drop(tab);
            close(&id);
            format!("Closed tab {id}.").into()
        }
        "ask_user" => {
            let tab = tab_or_error(tab_id)?;
            let reason = text_arg(request, "reason").ok_or("Give the `reason`: what they should do.")?.to_string();
            let agent = owner().agent;
            *tab.help.borrow_mut() = Some(Help { agent: agent.clone(), reason: reason.clone(), answer: None });
            changed();
            let id = tab.id.clone();
            cx.update(|cx| window::help_card(&id, &agent, &reason, cx)).map_err(|_| "Filo is quitting.".to_string())?;
            let start = Instant::now();
            let answer = loop {
                sleep(cx, Duration::from_millis(300)).await;
                if self::tab(&id).is_none() {
                    break Some("closed");
                }
                if let Some(answer) = tab.help().and_then(|help| help.answer) {
                    break Some(answer);
                }
                if start.elapsed() >= Duration::from_secs(filo_local::browser_mcp::ASK_WAIT_SECONDS) {
                    break None;
                }
            };
            if answer.is_some() {
                *tab.help.borrow_mut() = None;
                changed();
                cx.update(|cx| window::remove_help_card(&id, cx)).ok();
            }
            match answer {
                Some("done") => format!("The user says they're done.\n{}", where_now(&tab)),
                Some("declined") => format!("The user said no: they won't do it now.\n{}", where_now(&tab)),
                Some(_) => format!("The user closed tab {id}."),
                None => format!("The user hasn't finished yet; their card is still up. Call browser_ask_user again to keep waiting.\n{}", where_now(&tab)),
            }
            .into()
        }
        "select" => {
            let tab = tab_or_error(tab_id)?;
            let reference = text_arg(request, "ref").ok_or("Give the menu's `ref`, like e5 (from browser_read).")?;
            let mut texts: Vec<String> = request
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            if let Some(option) = text_arg(request, "option") {
                texts.insert(0, option.to_string());
            }
            if texts.is_empty() {
                return Err(
                    "Give the `option` to pick (its text or value), or `options` for a list that takes several.".into(),
                );
            }
            let chose = page(cx, &tab, "choose", json!({ "ref": reference, "text": texts[0], "texts": texts })).await?;
            if chose["native"] == true {
                settle(cx, &tab).await;
                format!(
                    "Picked {} in {}.\n{}",
                    texts.iter().map(|text| format!("\"{text}\"")).collect::<Vec<_>>().join(", "),
                    line_of(&chose),
                    where_now(&tab)
                )
                .into()
            } else {
                // A menu the page draws itself (a combo box, a list box, a button with a menu): opened with a real
                // click, then its option clicked.
                if texts.len() > 1 {
                    return Err(format!(
                        "{} isn't a list that takes several: pick one option at a time.",
                        line_of(&chose)
                    ));
                }
                let target = page(cx, &tab, "target", json!({ "ref": reference })).await?;
                sleep(cx, Duration::from_millis(80)).await;
                let (x, y) = (target["x"].as_f64().unwrap_or(0.), target["y"].as_f64().unwrap_or(0.));
                if target["hit"] == true {
                    click_at(cx, &tab, x, y, false, false).await?;
                } else {
                    page(cx, &tab, "click", json!({ "ref": reference })).await?;
                }
                let mut option = Value::Null;
                for _ in 0..8 {
                    sleep(cx, Duration::from_millis(150)).await;
                    option = page(cx, &tab, "option", json!({ "text": texts[0] })).await?;
                    if option.get("ref").is_some() {
                        break;
                    }
                }
                if option.get("ref").is_none() {
                    let shown: Vec<&str> =
                        option["options"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                    let listed = if shown.is_empty() {
                        "none shows: browser_screenshot shows what opened".to_string()
                    } else {
                        format!("shown: {}", shown.join(" | "))
                    };
                    return Err(format!(
                        "Clicked {} but no option \"{}\" showed ({listed}).",
                        line_of(&target),
                        texts[0]
                    ));
                }
                sleep(cx, Duration::from_millis(60)).await;
                if option["hit"] == true {
                    click_at(
                        cx,
                        &tab,
                        option["x"].as_f64().unwrap_or(0.),
                        option["y"].as_f64().unwrap_or(0.),
                        false,
                        false,
                    )
                    .await?;
                } else {
                    page(cx, &tab, "click", json!({ "ref": option["ref"] })).await?;
                }
                settle(cx, &tab).await;
                let now =
                    page(cx, &tab, "focus_line", json!({})).await.map(|value| line_of(&value)).unwrap_or_default();
                format!(
                    "Opened {} with a click and clicked its option {}, with real mouse events.\n{}\nFocus: {now}",
                    line_of(&target),
                    line_of(&option),
                    where_now(&tab)
                )
                .into()
            }
        }
        "upload" => {
            let tab = tab_or_error(tab_id)?;
            let reference = text_arg(request, "ref").ok_or("Give the file field's `ref` (a `file` line of browser_read), or the button that opens the page's file chooser.")?;
            let mut given: Vec<String> = request
                .get("paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            if let Some(path) = text_arg(request, "path") {
                given.push(path.to_string());
            }
            if given.is_empty() {
                return Err("Give `paths`: the files' full paths on this Mac.".into());
            }
            let home = std::env::var("HOME").unwrap_or_default();
            let mut paths = Vec::new();
            for path in &given {
                let path = PathBuf::from(
                    path.trim()
                        .strip_prefix("~/")
                        .map(|rest| format!("{home}/{rest}"))
                        .unwrap_or_else(|| path.trim().to_string()),
                );
                if !path.is_absolute() {
                    return Err(format!("{}: give the file's full path.", path.display()));
                }
                if !path.exists() {
                    return Err(format!("There's no file {}.", path.display()));
                }
                paths.push(path);
            }
            let field = page(cx, &tab, "file_input", json!({ "ref": reference })).await?;
            let input = field["ref"].as_str().map(String::from);
            let taken = async |tab: &Tab, wait: u64| {
                let start = Instant::now();
                while tab.upload.borrow().is_some() && start.elapsed() < Duration::from_millis(wait) {
                    sleep(cx, Duration::from_millis(100)).await;
                }
                tab.upload.borrow().is_none()
            };
            *tab.upload.borrow_mut() = Some(paths.clone());
            // The file field clicked through the page (it's often hidden); else the element itself, for real, as the
            // button that opens the chooser.
            let mut chosen = false;
            if let Some(input) = &input {
                page(cx, &tab, "open_chooser", json!({ "ref": input })).await.ok();
                chosen = taken(&tab, 1500).await;
            }
            if !chosen {
                let target = page(cx, &tab, "target", json!({ "ref": reference })).await?;
                sleep(cx, Duration::from_millis(80)).await;
                if target["hit"] == true {
                    click_at(
                        cx,
                        &tab,
                        target["x"].as_f64().unwrap_or(0.),
                        target["y"].as_f64().unwrap_or(0.),
                        false,
                        false,
                    )
                    .await?;
                } else {
                    page(cx, &tab, "click", json!({ "ref": reference })).await?;
                }
                chosen = taken(&tab, 3000).await;
            }
            if tab.upload.borrow_mut().take().is_some() || !chosen {
                // A file chooser the page didn't open: nothing waits for the files any more.
                tab.news.borrow_mut().retain(|line| !line.starts_with("The page asked for a file"));
                return Err(format!(
                    "The page didn't ask for a file: give the ref of its file field (a `file` line of browser_read) or of the button that opens its chooser{}.",
                    if field["count"].as_u64().unwrap_or(0) > 1 { "; it has several file fields" } else { "" }
                ));
            }
            settle(cx, &tab).await;
            let names: Vec<String> = paths
                .iter()
                .map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default())
                .collect();
            let asked = page(cx, &tab, "line", json!({ "ref": reference }))
                .await
                .map(|value| line_of(&value))
                .unwrap_or_else(|_| format!("[{reference}]"));
            let field = match input.as_deref() {
                Some(input) if input != reference.trim_matches(['[', ']']) => {
                    page(cx, &tab, "files", json!({ "ref": input }))
                        .await
                        .map(|value| format!(", in its file field {}", line_of(&value)))
                        .unwrap_or_default()
                }
                _ => String::new(),
            };
            format!("Chose {} for {asked}{field}.\n{}", names.join(", "), where_now(&tab)).into()
        }
        "hover" => {
            let tab = tab_or_error(tab_id)?;
            let (x, y, what) = match (text_arg(request, "ref"), number(request, "x"), number(request, "y")) {
                (Some(reference), _, _) => {
                    let target = page(cx, &tab, "target", json!({ "ref": reference })).await?;
                    sleep(cx, Duration::from_millis(80)).await;
                    (target["x"].as_f64().unwrap_or(0.), target["y"].as_f64().unwrap_or(0.), line_of(&target))
                }
                (None, Some(x), Some(y)) => (x, y, line_of(&page(cx, &tab, "at", json!({ "x": x, "y": y })).await.unwrap_or_default())),
                _ => return Err("Give the element's `ref` (from browser_read), or `x` and `y` (a spot of browser_screenshot's picture).".into()),
            };
            let (width, height) = native::viewport(tab.view());
            if !(0. ..width).contains(&x) || !(0. ..height).contains(&y) {
                return Err(format!(
                    "({}, {}) is outside the page's view, which is {width}×{height}: scroll to it first.",
                    x.round(),
                    y.round()
                ));
            }
            let lines = |value: &Value| -> Vec<String> {
                value["text"].as_str().unwrap_or_default().lines().skip(3).map(String::from).collect()
            };
            let before = page(cx, &tab, "read", json!({})).await.map(|value| lines(&value)).unwrap_or_default();
            native::mouse(tab.view(), native::Mouse::Move, x, y, 0);
            flash(cx, &tab, x, y, 10.);
            sleep(cx, Duration::from_millis(500)).await;
            let after = page(cx, &tab, "read", json!({})).await.map(|value| lines(&value)).unwrap_or_default();
            let seen: std::collections::HashSet<&String> = before.iter().collect();
            let new: Vec<&String> = after.iter().filter(|line| !seen.contains(line)).take(30).collect();
            let shown = if new.is_empty() {
                "Nothing new shows in the page's text; browser_screenshot shows what changed.".to_string()
            } else {
                format!("New on the page now:\n{}", new.iter().map(|line| line.as_str()).collect::<Vec<_>>().join("\n"))
            };
            format!("Pointed at {what} ({}, {}) with a real mouse move: the pointer stays there until the next one.\n{}\n{shown}", x.round(), y.round(), where_now(&tab)).into()
        }
        "dialog" => {
            let tab = tab_or_error(tab_id)?;
            let accept = match text_arg(request, "action").unwrap_or("accept") {
                "accept" | "ok" => true,
                "dismiss" | "cancel" => false,
                other => return Err(format!("`action` is accept or dismiss, not {other}.")),
            };
            let Some((kind, message)) = tab.dialog() else {
                return Err(format!("The page shows no dialog now.\n{}", where_now(&tab)));
            };
            let text = request.get("text").and_then(Value::as_str);
            tab.answer_dialog(accept, text);
            settle(cx, &tab).await;
            let with = match (kind, accept, text) {
                ("prompt", true, Some(text)) => format!(" with \"{text}\""),
                _ => String::new(),
            };
            format!(
                "{} the page's {kind} “{message}”{with}.\n{}",
                if accept { "Accepted" } else { "Dismissed" },
                where_now(&tab)
            )
            .into()
        }
        "wait" => {
            let tab = tab_or_error(tab_id)?;
            let most = Duration::from_secs_f64(number(request, "seconds").unwrap_or(10.).clamp(1., MAX_WAIT));
            let start = Instant::now();
            let gone = request.get("gone").and_then(Value::as_bool).unwrap_or(false);
            let text = text_arg(request, "text");
            let done = if text_arg(request, "for") == Some("download") {
                // Every download going on ended, one of them lately (the click before may have started it).
                let since = start.checked_sub(Duration::from_secs(30)).unwrap_or(start);
                let ended = |tab: &Tab| {
                    let downloads = tab.downloads.borrow();
                    downloads.iter().all(|download| download.done.is_some())
                        && downloads.iter().any(|download| download.ended.is_some_and(|ended| ended >= since))
                };
                while !ended(&tab) && start.elapsed() < most {
                    sleep(cx, Duration::from_millis(200)).await;
                }
                let last = tab
                    .downloads
                    .borrow()
                    .iter()
                    .rev()
                    .find(|download| download.done.is_some())
                    .map(|download| (download.path.clone(), download.done == Some(true)));
                match last {
                    Some((path, saved)) if ended(&tab) => Ok(format!(
                        "{} {}.",
                        if saved { "The download ended:" } else { "The download failed:" },
                        path.display()
                    )),
                    _ => Err(format!("No download ended in {} s.", most.as_secs())),
                }
            } else if let Some(text) = text {
                let mut found = !gone;
                while start.elapsed() < most {
                    found = page(cx, &tab, "has", json!({ "text": text })).await?["found"] == true;
                    if found != gone {
                        break;
                    }
                    sleep(cx, Duration::from_millis(250)).await;
                }
                match (found != gone, gone) {
                    (true, false) => {
                        let lines = page(cx, &tab, "read", json!({ "find": text }))
                            .await
                            .map(|value| {
                                value["text"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .lines()
                                    .skip(3)
                                    .take(12)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        Ok(format!("\"{text}\" shows after {:.1} s:\n{lines}", start.elapsed().as_secs_f64()))
                    }
                    (true, true) => Ok(format!("\"{text}\" is gone after {:.1} s.", start.elapsed().as_secs_f64())),
                    (false, false) => Err(format!("\"{text}\" didn't show in {} s.", most.as_secs())),
                    (false, true) => Err(format!("\"{text}\" is still there after {} s.", most.as_secs())),
                }
            } else {
                // The page loaded, then still for half a second.
                while tab.state().2 && start.elapsed() < most {
                    sleep(cx, Duration::from_millis(100)).await;
                }
                let left = most.saturating_sub(start.elapsed()).as_millis().max(500) as u64;
                let quiet = page(cx, &tab, "quiet", json!({ "most": left })).await?;
                if !tab.state().2 && quiet["quiet"] == true {
                    Ok(format!("The page loaded and stopped changing after {:.1} s.", start.elapsed().as_secs_f64()))
                } else {
                    Err(format!(
                        "The page still {} after {} s.",
                        if tab.state().2 { "loads" } else { "changes" },
                        most.as_secs()
                    ))
                }
            };
            match done {
                Ok(said) => format!("{said}\n{}", where_now(&tab)).into(),
                Err(said) => return Err(format!("{said}\n{}", where_now(&tab))),
            }
        }
        "" => return Err("No op.".into()),
        other => return Err(format!("Unknown browser op {other}")),
    };
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_as_typed() {
        assert_eq!(address("example.com"), "https://example.com");
        assert_eq!(address("localhost:8080/x"), "http://localhost:8080/x");
        assert_eq!(address("http://a.b"), "http://a.b");
        assert_eq!(address("cheap flights to Rome"), "https://www.google.com/search?q=cheap+flights+to+Rome");
        assert_eq!(address(""), "about:blank");
    }

    #[test]
    fn a_drag_starts_with_short_steps_and_ends_where_asked() {
        let path = drag_path((100., 100.), (100., 400.));
        assert_eq!(&path[..3], &[(100., 102.), (100., 105.), (100., 109.)]);
        assert_eq!(path.last(), Some(&(100., 400.)));
        assert!(path.windows(2).all(|pair| pair[1].1 > pair[0].1 && pair[1].1 - pair[0].1 <= 12.), "{path:?}");
        // A short drag still moves in several steps; one that goes nowhere is one move on the spot.
        assert!(drag_path((0., 0.), (6., 0.)).len() >= 6);
        assert_eq!(drag_path((5., 5.), (5., 5.)), vec![(5., 5.)]);
    }

    /// A second Filo on the same folder, whose `listen` fails, leaves the first one's socket on quit; the first takes
    /// away only the file it bound. (Each thread stands for one Filo: `BOUND` is thread-local.)
    #[test]
    fn quitting_takes_away_only_the_socket_it_bound() {
        let dir = std::env::temp_dir().join(format!("filo-sock-{}", std::process::id()));
        let path = dir.join("browser.sock");
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        listen(&path, tx.clone()).unwrap();
        let second = {
            let (path, tx) = (path.clone(), tx.clone());
            std::thread::spawn(move || {
                assert!(listen(&path, tx).is_err());
                take_socket_away(&path);
            })
        };
        second.join().unwrap();
        assert!(path.exists(), "the second Filo took the first one's socket away");
        take_socket_away(&path);
        assert!(!path.exists());
        // The first one's file gone and another Filo's socket in its place: quitting leaves that one too.
        listen(&path, tx.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        let third = {
            let (path, tx) = (path.clone(), tx.clone());
            std::thread::spawn(move || listen(&path, tx).unwrap())
        };
        third.join().unwrap();
        take_socket_away(&path);
        assert!(path.exists(), "the first Filo took another one's socket away");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

// Chromium as the browser's engine, picked in Settings › Browser (browser/engine.rs, which downloads it): new tabs
// open in Chromium (through CEF, windowless) instead of WebKit. The engine is an app of its own, Filo Chromium
// (chromium/filo_chromium.mm), started with the first such tab and spoken to over its standard input and output, a JSON
// line each way; it runs only when it speaks this Filo's protocol (`engine::PROTOCOL`), and stops when Filo closes its
// input. Its profile (cookies, site storage) is the local data folder's `chromium`. Should it stop on its own (a
// crash), its tabs open again in a new one (browser.rs `reopen_tabs`).
//
// A tab here is a `Tab` like WebKit's, with a view of Filo's own (`FiloChromiumView`) in its place: parked off the
// screen, or moved into the browser window, showing the frames Chromium draws into shared memory, and handing the
// user's mouse and keys on. native.rs asks this module first for what it would ask WebKit (`of`): the page's state,
// scripts (page.js, in an isolated world of Filo's own, through DevTools), screenshots (the last frame), and real
// input (DevTools' Input domain, which pages get as trusted events).
//
// A page's dialogs, file choosers and downloads come back as events and go to the same places WebKit's do (the tab's
// dialog, browser_upload's files, the downloads folder). A page draws at the screen's scale while it shows in the
// browser window, and at 1x while it's parked off the screen, which takes far less memory (an agent's screenshots are
// one pixel per CSS pixel either way).
//
// The view takes what the user does there as a Mac app's view does: keys go through the input method
// (NSTextInputClient: composing Chinese or Japanese, accented letters, the emoji picker), a right-click shows the
// page's menu as a native one (Filo opens and copies links and pictures itself), files, text and links dragged in from
// another app drop into the page; the page's own drag and drop Chromium carries in the page.
// window.print(), the menu's Print and ⌘P print the page as a PDF Chromium makes, with macOS's print panel
// (browser.rs `print`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{App, AsyncApp};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use serde_json::{Value, json};

use super::native::{self, COMMAND, CONTROL, Key, Mouse, OPTION, SHIFT};

/// The engine's process: its input, and the number of the last request.
struct Helper {
    input: Option<std::process::ChildStdin>,
    child: Option<std::process::Child>,
    next: u64,
    /// Which start this is: its output's end says so (`gone`).
    start: u64,
}

impl Drop for Helper {
    /// The engine quits once its input closes, its cookies written out; one that hasn't in 5 s is stopped.
    fn drop(&mut self) {
        drop(self.input.take());
        let Some(mut child) = self.child.take() else { return };
        std::thread::spawn(move || {
            for _ in 0..50 {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

/// What Filo knows of a page in Chromium, kept up by the engine's events.
struct Shared {
    view: usize,
    title: String,
    url: String,
    loading: bool,
    back: bool,
    forward: bool,
    /// The engine made the page (its first answer), so DevTools calls reach it.
    made: bool,
    /// The last frame: its shared memory's name, width and height in pixels.
    frame: Option<(String, usize, usize)>,
    mapped: Option<Mapped>,
    cursor: i64,
    /// The isolated world page.js runs in, while its document lasts.
    world: Option<i64>,
    /// The page's size in points and the pixels per point, as the engine was last told.
    size: (f64, f64, f64),
    /// An input method's text being composed (its length, in UTF-16), and where it shows (points in the view).
    marked: Option<usize>,
    ime_rect: Option<(f64, f64, f64, f64)>,
    /// What the page said it would do with a drag from another app over it (NSDragOperation's bits).
    drop_operation: usize,
}

/// A frame's shared memory, mapped to read.
struct Mapped {
    name: String,
    at: *mut c_void,
    len: usize,
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: unmapping what `map` mapped, once.
        unsafe {
            libc::munmap(self.at, self.len);
        }
    }
}

thread_local! {
    static HELPER: RefCell<Option<Helper>> = const { RefCell::new(None) };
    static WAITING: RefCell<HashMap<u64, futures::channel::oneshot::Sender<Result<Value, String>>>> =
        RefCell::new(HashMap::new());
    static PAGES: RefCell<HashMap<String, Rc<RefCell<Shared>>>> = RefCell::new(HashMap::new());
    static CX: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
    /// The engine's starts so far.
    static STARTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Pop-ups the engine named ("p1") and the tab ids Filo gave them, for events sent before the engine renamed them.
    static RENAMED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// The engine's output, read on a thread of its own and handled on the main thread.
static EVENTS: OnceLock<futures::channel::mpsc::UnboundedSender<Value>> = OnceLock::new();

/// Handles the engine's answers and events (once, from browser::init).
pub fn init(cx: &mut App) {
    if EVENTS.get().is_some() {
        return;
    }
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Value>();
    let _ = EVENTS.set(tx);
    CX.with(|slot| *slot.borrow_mut() = Some(cx.to_async()));
    cx.spawn(async move |cx| {
        while let Some(message) = rx.next().await {
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                let waiting = WAITING.with(|waiting| waiting.borrow_mut().remove(&id));
                if let Some(waiting) = waiting {
                    let answer = if message["ok"] == true {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(message["error"].as_str().unwrap_or("Chromium said no.").to_string())
                    };
                    let _ = waiting.send(answer);
                }
                continue;
            }
            event(cx, &message);
        }
    })
    .detach();
}

/// Starts the engine, unless it runs.
fn start() -> Result<(), String> {
    if HELPER.with(|helper| helper.borrow().is_some()) {
        return Ok(());
    }
    let app = super::engine::app_path().ok_or("Chromium isn't installed: pick it in Settings › Browser.")?;
    let executable = app.join("Contents/MacOS/Filo Chromium");
    if !executable.exists() {
        return Err(format!("There's no Chromium engine at {}.", app.display()));
    }
    let profile = super::local_dir().join("chromium");
    let _ = std::fs::create_dir_all(&profile);
    let mut child = std::process::Command::new(&executable)
        .arg(format!("--filo-profile={}", profile.display()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|error| format!("Couldn't start Chromium: {error}"))?;
    let output = child.stdout.take().ok_or("Chromium has no output")?;
    let input = child.stdin.take().ok_or("Chromium has no input")?;
    let events = EVENTS.get().cloned().ok_or("The browser isn't set up yet.")?;
    let start = STARTS.with(|starts| {
        starts.set(starts.get() + 1);
        starts.get()
    });
    std::thread::Builder::new()
        .name("chromium".into())
        .spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    let _ = events.unbounded_send(message);
                }
            }
            let _ = events.unbounded_send(json!({ "ev": "gone", "start": start }));
        })
        .map_err(|error| error.to_string())?;
    eprintln!("browser: started Chromium (pid {}) from {}", child.id(), app.display());
    HELPER.with(|helper| {
        *helper.borrow_mut() = Some(Helper { input: Some(input), child: Some(child), next: 0, start });
    });
    Ok(())
}

/// Stops the engine (Settings › Browser picked WebKit, or a newer engine waits to start): it closes its pages and
/// quits. Its tabs go first (browser.rs `reopen_tabs`).
pub fn stop() {
    let helper = HELPER.with(|helper| helper.borrow_mut().take());
    if helper.is_some() {
        eprintln!("browser: stopping Chromium");
    }
    WAITING.with(|waiting| waiting.borrow_mut().clear());
    drop(helper);
}

/// Every cookie in the engine's profile.
pub async fn cookies() -> Result<Vec<Value>, String> {
    let answer = ask(json!({ "op": "cookies" })).await?;
    Ok(answer["cookies"].as_array().cloned().unwrap_or_default())
}

/// Puts `cookies` in the engine's profile, only those it hasn't when `missing`: how many it set, and passed over.
pub async fn set_cookies(cookies: Vec<Value>, missing: bool) -> Result<(usize, usize), String> {
    let answer = ask(json!({ "op": "set_cookies", "cookies": cookies, "missing": missing })).await?;
    Ok((answer["set"].as_u64().unwrap_or(0) as usize, answer["skipped"].as_u64().unwrap_or(0) as usize))
}

/// Sends a request; with `answer`, it gets a number and its answer is waited for.
fn send(
    mut request: Value,
    answer: bool,
) -> Result<Option<futures::channel::oneshot::Receiver<Result<Value, String>>>, String> {
    start()?;
    HELPER.with(|helper| {
        let mut helper = helper.borrow_mut();
        let helper = helper.as_mut().ok_or("Chromium isn't running.")?;
        let waiting = answer.then(|| {
            helper.next += 1;
            request["id"] = json!(helper.next);
            let (tx, rx) = futures::channel::oneshot::channel();
            WAITING.with(|waiting| waiting.borrow_mut().insert(helper.next, tx));
            rx
        });
        let input = helper.input.as_mut().ok_or("Chromium is stopping.")?;
        writeln!(input, "{request}").map_err(|error| format!("Chromium stopped: {error}"))?;
        Ok(waiting)
    })
}

/// A request without an answer.
fn tell(request: Value) {
    if let Err(error) = send(request, false) {
        eprintln!("browser: {error}");
    }
}

fn cx() -> Option<AsyncApp> {
    CX.with(|cx| cx.borrow().clone())
}

/// A request and its answer, given up on after 60 s.
async fn ask(request: Value) -> Result<Value, String> {
    let rx = send(request, true)?.ok_or("no answer")?;
    let cx = cx().ok_or("The browser isn't set up yet.")?;
    let timeout = cx.background_executor().timer(Duration::from_secs(60));
    match futures::future::select(rx, Box::pin(timeout)).await {
        futures::future::Either::Left((Ok(answer), _)) => answer,
        futures::future::Either::Left((Err(_), _)) => Err("Chromium stopped.".into()),
        futures::future::Either::Right(_) => Err("Chromium didn't answer in 60 s.".into()),
    }
}

/// A DevTools method in tab `key`'s page, once the engine made it.
async fn devtools(key: &str, method: &str, params: Value) -> Result<Value, String> {
    let cx = cx().ok_or("The browser isn't set up yet.")?;
    for _ in 0..100 {
        match shared(key) {
            Some(page) if page.borrow().made => break,
            Some(_) => cx.background_executor().timer(Duration::from_millis(100)).await,
            None => return Err(format!("There's no tab {key} in Chromium.")),
        }
    }
    ask(json!({ "op": "cdp", "tab": key, "method": method, "params": params })).await
}

/// A DevTools method sent without waiting for its answer (input, whose order the engine keeps).
fn devtools_now(key: &str, method: &str, params: Value) {
    tell(json!({ "op": "cdp", "tab": key, "method": method, "params": params }));
}

fn shared(key: &str) -> Option<Rc<RefCell<Shared>>> {
    PAGES.with(|pages| pages.borrow().get(key).cloned())
}

/// A tab's page in Chromium: owned by its `Tab`, closed with it.
pub struct Page {
    key: String,
    shared: Rc<RefCell<Shared>>,
    /// The engine's start it's in: a tab opened again in a new engine has the same key.
    start: u64,
}

impl Drop for Page {
    fn drop(&mut self) {
        // As Filo quits, the thread's other values may be gone already (the engine goes with its input then).
        let _ = PAGES.try_with(|pages| {
            let mut pages = pages.borrow_mut();
            if pages.get(&self.key).is_some_and(|page| Rc::ptr_eq(page, &self.shared)) {
                pages.remove(&self.key);
            }
        });
        let running = HELPER.try_with(|helper| helper.borrow().as_ref().map(|helper| helper.start)).ok().flatten();
        if running == Some(self.start) {
            tell(json!({ "op": "close", "tab": self.key }));
        }
    }
}

/// Opens a page in Chromium for tab `key`, `width` × `height` points: the page, and the view that shows it.
pub fn open(key: &str, url: &str, (width, height): (f64, f64)) -> Result<(Page, Retained<AnyObject>), String> {
    start()?;
    // Parked to begin with.
    let scale = scale_for(None);
    let (page, view) = adopt(key, (width, height, scale))?;
    let answer = send(
        json!({ "op": "create", "tab": key, "url": url, "width": width, "height": height, "scale": scale }),
        true,
    )?;
    if let (Some(answer), Some(cx)) = (answer, cx()) {
        let key = key.to_string();
        cx.spawn(async move |_| {
            let made = answer.await.ok().and_then(Result::ok).is_some();
            match shared(&key) {
                Some(page) if made => {
                    page.borrow_mut().made = true;
                    prepare(&key).await;
                }
                _ => eprintln!("browser: Chromium didn't make {key}'s page"),
            }
        })
        .detach();
    }
    Ok((page, view))
}

/// The binding the page's window.print() calls (`PRINT_SCRIPT`), heard as a DevTools event.
const PRINT_BINDING: &str = "__filoPrint";
const PRINT_SCRIPT: &str = "window.print = function print() { window.__filoPrint(''); };";

/// Sets up a page the engine made: its window.print() comes to Filo, in this document and every one after.
async fn prepare(key: &str) {
    for (method, params) in [
        ("Runtime.enable", json!({})),
        ("Runtime.addBinding", json!({ "name": PRINT_BINDING })),
        ("Page.addScriptToEvaluateOnNewDocument", json!({ "source": PRINT_SCRIPT, "runImmediately": true })),
    ] {
        if let Err(error) = devtools(key, method, params).await {
            eprintln!("browser: {key}'s page didn't take {method}: {error}");
        }
    }
}

/// A page the engine made itself (a pop-up), with the view that shows it.
fn adopt(key: &str, size: (f64, f64, f64)) -> Result<(Page, Retained<AnyObject>), String> {
    let view = new_view(size.0, size.1).ok_or("Couldn't make the page's view")?;
    let shared = Shared {
        view: Retained::as_ptr(&view) as usize,
        title: String::new(),
        url: String::new(),
        // Until the engine says otherwise: a tool waits for the page to load.
        loading: true,
        back: false,
        forward: false,
        made: false,
        frame: None,
        mapped: None,
        cursor: 0,
        world: None,
        size,
        marked: None,
        ime_rect: None,
        drop_operation: 1,
    };
    let shared = Rc::new(RefCell::new(shared));
    PAGES.with(|pages| pages.borrow_mut().insert(key.to_string(), shared.clone()));
    Ok((Page { key: key.to_string(), shared, start: STARTS.with(std::cell::Cell::get) }, view))
}

/// The pixels per point a page in `view` draws at: its window's (the browser window's), or 1 parked off the screen
/// (no view yet: parked). `dev` builds: FILO_CHROMIUM_SCALE draws at that scale wherever the page is.
fn scale_for(view: Option<&AnyObject>) -> f64 {
    if let Some(scale) =
        crate::test_run::dev_override("FILO_CHROMIUM_SCALE").and_then(|scale| scale.parse::<f64>().ok())
    {
        return scale.clamp(1., 3.);
    }
    let Some(view) = view.filter(|view| !native::parked(view)) else { return 1. };
    // SAFETY: AppKit read-only queries on the main thread.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() { 1. } else { msg_send![window, backingScaleFactor] }
    }
}

/// One of the engine's events.
fn event(cx: &mut AsyncApp, message: &Value) {
    let key = message["tab"].as_str().unwrap_or_default();
    let renamed = RENAMED.with(|renamed| renamed.borrow().get(key).cloned());
    let key = renamed.as_deref().unwrap_or(key);
    match message["ev"].as_str().unwrap_or_default() {
        "ready" => {
            let protocol = message["protocol"].as_u64().unwrap_or(0);
            eprintln!(
                "browser: Chromium {} (CEF {}, protocol {protocol}) is ready",
                message["version"].as_str().unwrap_or("?"),
                message["cef"].as_str().unwrap_or("?")
            );
            if protocol != super::engine::PROTOCOL {
                eprintln!("browser: that engine doesn't speak this Filo's protocol ({})", super::engine::PROTOCOL);
                stop();
            }
        }
        "state" => {
            if let Some(page) = shared(key) {
                let mut page = page.borrow_mut();
                page.title = message["title"].as_str().unwrap_or_default().to_string();
                page.url = message["url"].as_str().unwrap_or_default().to_string();
                page.loading = message["loading"] == true;
                page.back = message["back"] == true;
                page.forward = message["forward"] == true;
                page.made = true;
            }
            super::changed();
        }
        "frame" => {
            if let Some(page) = shared(key) {
                let view = {
                    let mut page = page.borrow_mut();
                    page.frame = Some((
                        message["shm"].as_str().unwrap_or_default().to_string(),
                        message["width"].as_u64().unwrap_or(0) as usize,
                        message["height"].as_u64().unwrap_or(0) as usize,
                    ));
                    page.view
                };
                // SAFETY: the view is alive while its page is in PAGES (the tab holds both).
                let view = unsafe { &*(view as *const AnyObject) };
                if !native::parked(view) {
                    show_frame(view);
                }
            }
        }
        "cursor" => {
            if let Some(page) = shared(key) {
                page.borrow_mut().cursor = message["type"].as_i64().unwrap_or(0);
            }
        }
        "popup" => {
            let opener = message["opener"].as_str().unwrap_or_default();
            // Parked, at its opener's size.
            let size = super::tab(opener)
                .and_then(|tab| of(tab.view()))
                .map(|page| page.0.borrow().size)
                .map_or((super::PAGE_SIZE.0, super::PAGE_SIZE.1, 1.), |(width, height, _)| {
                    (width, height, scale_for(None))
                });
            // Numbered as Filo's tabs are, and renamed so in the engine.
            let id = super::next_id();
            RENAMED.with(|renamed| renamed.borrow_mut().insert(key.to_string(), id.clone()));
            tell(json!({ "op": "rename", "tab": key, "to": id }));
            match adopt(&id, size) {
                Ok((page, view)) => {
                    if let Some(shared) = shared(&id) {
                        shared.borrow_mut().made = true;
                    }
                    let prepared = id.clone();
                    cx.spawn(async move |_| prepare(&prepared).await).detach();
                    if let Err(error) = super::chromium_popup(&id, opener, page, view) {
                        eprintln!("browser: {opener}'s pop-up: {error}");
                        tell(json!({ "op": "close", "tab": id }));
                    }
                }
                Err(error) => eprintln!("browser: {opener}'s pop-up: {error}"),
            }
            cx.update(|cx| cx.refresh_windows()).ok();
        }
        "closed" => {
            if let Some(page) = shared(key) {
                let view = page.borrow().view as *mut AnyObject;
                super::on_page_closed(view);
            }
        }
        "dialog" => {
            let Some(page) = shared(key) else { return };
            let kind = match message["kind"].as_str() {
                Some("alert") => super::delegate::Kind::Alert,
                Some("confirm") => super::delegate::Kind::Confirm,
                _ => super::delegate::Kind::Prompt,
            };
            let tab = key.to_string();
            let dialog = super::delegate::Dialog::chromium(
                kind,
                message["message"].as_str().unwrap_or_default().to_string(),
                message["default"].as_str().unwrap_or_default().to_string(),
                move |accept, text| tell(json!({ "op": "dialog", "tab": tab, "accept": accept, "text": text })),
            );
            super::on_dialog(page.borrow().view as *mut AnyObject, dialog);
            cx.update(|cx| cx.refresh_windows()).ok();
        }
        "files" => {
            let Some(page) = shared(key) else { return };
            let view = page.borrow().view as *mut AnyObject;
            let (multiple, folder) = (message["multiple"] == true, message["folder"] == true);
            let paths = match super::on_file_chooser(view, multiple, folder) {
                super::delegate::Chooser::Files(files) => files,
                super::delegate::Chooser::Cancel => Vec::new(),
                super::delegate::Chooser::User => open_panel(multiple, folder),
            };
            let paths: Vec<String> = paths.iter().map(|path| path.display().to_string()).collect();
            tell(json!({ "op": "files", "tab": key, "paths": paths }));
        }
        "download" => {
            let url = message["url"].as_str().unwrap_or_default().to_string();
            let name = message["name"].as_str().filter(|name| !name.is_empty()).unwrap_or("download");
            let downloads = dirs::download_dir().unwrap_or_else(super::downloads_dir);
            let mut path = super::unique_path(&downloads, name);
            let path =
                if super::download_started(key, url, &mut path) { path.display().to_string() } else { String::new() };
            tell(json!({ "op": "download", "tab": key, "item": message["item"], "path": path }));
        }
        "downloaded" => {
            super::download_finished(key, message["url"].as_str().unwrap_or_default(), message["ok"] == true);
        }
        "menu" => {
            let Some(page) = shared(key) else { return };
            let view = page.borrow().view as *mut AnyObject;
            let picked = context_menu(view, message);
            tell(json!({ "op": "menu", "tab": key, "command": picked.unwrap_or(-1) }));
            match picked {
                Some(OPEN_LINK) => super::open_from_page(message["link"].as_str().unwrap_or_default()),
                Some(OPEN_IMAGE) => super::open_from_page(message["image"].as_str().unwrap_or_default()),
                Some(COPY_LINK) => copy_text(message["link"].as_str().unwrap_or_default()),
                Some(COPY_IMAGE) => copy_text(message["image"].as_str().unwrap_or_default()),
                Some(PRINT) => super::page_asks_to_print(view),
                _ => {}
            }
            cx.update(|cx| cx.refresh_windows()).ok();
        }
        "ime" => {
            if let (Some(page), Some(rect)) = (shared(key), message["rect"].as_array()) {
                let at = |index: usize| rect.get(index).and_then(Value::as_f64).unwrap_or(0.);
                page.borrow_mut().ime_rect = Some((at(0), at(1), at(2), at(3)));
            }
        }
        "cdp" => {
            let printing = message["method"] == "Runtime.bindingCalled" && message["params"]["name"] == PRINT_BINDING;
            if let (true, Some(page)) = (printing, shared(key)) {
                let view = page.borrow().view as *mut AnyObject;
                super::page_asks_to_print(view);
            }
        }
        "crashed" => eprintln!("browser: Chromium's page {key} crashed: {}", message["error"]),
        "gone" => {
            // The engine that's running, not one stopped before it: it stopped on its own.
            let running = HELPER.with(|helper| helper.borrow().as_ref().map(|helper| helper.start));
            if running.is_none() || running != message["start"].as_u64() {
                return;
            }
            eprintln!("browser: Chromium stopped on its own");
            HELPER.with(|helper| helper.borrow_mut().take());
            WAITING.with(|waiting| waiting.borrow_mut().clear());
            let reopened = super::reopen_tabs(true);
            if reopened > 0 {
                eprintln!("browser: opened {reopened} tabs again in a new Chromium");
            }
            cx.update(|cx| cx.refresh_windows()).ok();
        }
        _ => {}
    }
}

/// AppKit's open panel, for a file chooser of the user's own page on screen.
fn open_panel(multiple: bool, folder: bool) -> Vec<PathBuf> {
    // SAFETY: AppKit on the main thread; the panel runs modally and its URLs are read before it goes.
    unsafe {
        let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
        let _: () = msg_send![panel, setAllowsMultipleSelection: multiple];
        let _: () = msg_send![panel, setCanChooseDirectories: folder];
        let _: () = msg_send![panel, setCanChooseFiles: !folder];
        let chosen: isize = msg_send![panel, runModal];
        if chosen != 1 {
            return Vec::new();
        }
        let urls: *mut AnyObject = msg_send![panel, URLs];
        let count: usize = msg_send![urls, count];
        (0..count)
            .filter_map(|index| {
                let url: *mut AnyObject = msg_send![urls, objectAtIndex: index];
                let path: *mut AnyObject = msg_send![url, path];
                let text: *const std::ffi::c_char = msg_send![path, UTF8String];
                (!text.is_null()).then(|| PathBuf::from(std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned()))
            })
            .collect()
    }
}

/// The page shown by `view`, when it's Chromium's (native.rs asks first).
pub fn of(view: &AnyObject) -> Option<Handle> {
    let at = view as *const AnyObject as usize;
    // As Filo quits, a closing tab's view leaves its window (`viewDidMoveToWindow`) after the pages have gone.
    PAGES
        .try_with(|pages| {
            pages
                .borrow()
                .iter()
                .find(|(_, page)| page.borrow().view == at)
                .map(|(key, page)| Handle(page.clone(), key.clone()))
        })
        .ok()
        .flatten()
}

pub struct Handle(Rc<RefCell<Shared>>, String);

impl Handle {
    pub fn state(&self) -> (String, String, bool) {
        let page = self.0.borrow();
        (page.title.clone(), page.url.clone(), page.loading)
    }

    pub fn history(&self) -> (bool, bool) {
        let page = self.0.borrow();
        (page.back, page.forward)
    }

    pub fn go(&self, how: &str) {
        if how != "stop" {
            self.0.borrow_mut().loading = true;
        }
        tell(json!({ "op": "go", "tab": self.1, "how": how }));
    }

    pub fn load(&self, url: &str) {
        self.0.borrow_mut().loading = true;
        tell(json!({ "op": "load", "tab": self.1, "url": url }));
    }

    /// page.js's body run with `arg` as `a`, in an isolated world of Filo's own in the main frame: it shares the
    /// page's DOM, not its scripts. A world lasts as long as its document; a new one is made when it's gone.
    pub fn run_js(&self, body: &str, arg: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let Some(cx) = cx() else { return done(Err("The browser isn't set up yet.".into())) };
        let expression = format!("(async (a) => {{\n{body}\n}})({})", Value::String(arg.to_string()));
        let (page, key) = (self.0.clone(), self.1.clone());
        cx.spawn(async move |_| done(evaluate(&page, &key, &expression).await)).detach();
    }

    /// The last frame, at one pixel per CSS pixel (the view's size).
    pub fn snapshot(&self, view: &AnyObject, done: impl FnOnce(Result<image::RgbaImage, String>) + 'static) {
        let (width, height) = native::viewport(view);
        let picture = pixels(&self.0).ok_or_else(|| "The page hasn't drawn yet.".to_string()).map(|(bgra, w, h)| {
            let mut rgba = bgra;
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba).expect("frame size");
            let (width, height) = (width.round().max(1.) as u32, height.round().max(1.) as u32);
            if (image.width(), image.height()) == (width, height) {
                image
            } else {
                image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
            }
        });
        done(picture);
    }

    /// One mouse event at (x, y), points from the page's top left, as DevTools input (trusted in the page).
    pub fn mouse(&self, kind: Mouse, x: f64, y: f64, clicks: isize) -> bool {
        let (kind, button, buttons) = match kind {
            Mouse::Move => ("mouseMoved", "none", 0),
            Mouse::Dragged => ("mouseMoved", "left", 1),
            Mouse::Down => ("mousePressed", "left", 1),
            Mouse::Up => ("mouseReleased", "left", 0),
            Mouse::RightDown => ("mousePressed", "right", 2),
            Mouse::RightUp => ("mouseReleased", "right", 0),
        };
        devtools_now(
            &self.1,
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": clicks.max(0) }),
        );
        true
    }

    /// Prints the page, as a PDF Chromium makes of it, with macOS's print panel over `window`'s.
    pub fn print(&self, view: *mut AnyObject) {
        let (Some(cx), key) = (cx(), self.1.clone()) else { return };
        cx.spawn(async move |_| {
            let made = devtools(
                &key,
                "Page.printToPDF",
                json!({ "printBackground": true, "preferCSSPageSize": true, "generateDocumentOutline": true }),
            )
            .await;
            let pdf = made.and_then(|made| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(made["data"].as_str().unwrap_or_default())
                    .map_err(|error| error.to_string())
            });
            match pdf {
                Ok(pdf) => {
                    if let Err(error) = native::print_pdf(view, &pdf) {
                        eprintln!("browser: couldn't print {key}: {error}");
                    }
                }
                Err(error) => eprintln!("browser: Chromium didn't make {key}'s PDF: {error}"),
            }
        })
        .detach();
    }

    /// One key pressed and let go.
    pub fn key(&self, key: &Key) -> bool {
        for params in key_events(key, true).into_iter().chain(key_events(key, false)) {
            devtools_now(
                &self.1,
                if params.get("text_only").is_some() { "Input.insertText" } else { "Input.dispatchKeyEvent" },
                without_marker(params),
            );
        }
        true
    }
}

fn without_marker(mut params: Value) -> Value {
    if let Some(object) = params.as_object_mut() {
        object.remove("text_only");
    }
    params
}

async fn evaluate(page: &Rc<RefCell<Shared>>, key: &str, expression: &str) -> Result<String, String> {
    for attempt in 0..2 {
        let world = page.borrow().world;
        let world = match world {
            Some(world) => world,
            None => {
                let tree = devtools(key, "Page.getFrameTree", json!({})).await?;
                let frame = tree["frameTree"]["frame"]["id"].as_str().ok_or("The page has no frame yet.")?.to_string();
                let made = devtools(
                    key,
                    "Page.createIsolatedWorld",
                    json!({ "frameId": frame, "worldName": "filo", "grantUniveralAccess": false }),
                )
                .await?;
                let world = made["executionContextId"].as_i64().ok_or("No world for Filo's script.")?;
                page.borrow_mut().world = Some(world);
                world
            }
        };
        let result = devtools(
            key,
            "Runtime.evaluate",
            json!({ "expression": expression, "contextId": world, "awaitPromise": true, "returnByValue": true }),
        )
        .await;
        match result {
            Ok(result) => {
                if let Some(details) = result.get("exceptionDetails") {
                    let text = details["exception"]["description"]
                        .as_str()
                        .or_else(|| details["text"].as_str())
                        .unwrap_or("an exception");
                    return Err(text.to_string());
                }
                return Ok(result["result"]["value"].as_str().unwrap_or_default().to_string());
            }
            // The document (and its world) went: a navigation.
            Err(error) if attempt == 0 && (error.contains("context") || error.contains("Context")) => {
                page.borrow_mut().world = None;
            }
            Err(error) => return Err(error),
        }
    }
    Err("The page kept changing.".into())
}

/// The last frame's pixels (BGRA, premultiplied), its width and height.
fn pixels(page: &Rc<RefCell<Shared>>) -> Option<(Vec<u8>, usize, usize)> {
    let mut page = page.borrow_mut();
    let (name, width, height) = page.frame.clone()?;
    let len = width * height * 4;
    if page.mapped.as_ref().is_none_or(|mapped| mapped.name != name || mapped.len != len) {
        page.mapped = None;
        let c_name = CString::new(name.clone()).ok()?;
        // SAFETY: opening and mapping a shared memory object read-only; the mapping is unmapped by `Mapped`.
        unsafe {
            let fd = libc::shm_open(c_name.as_ptr(), libc::O_RDONLY);
            if fd < 0 {
                return None;
            }
            let at = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if at == libc::MAP_FAILED {
                return None;
            }
            page.mapped = Some(Mapped { name, at, len });
        }
    }
    let mapped = page.mapped.as_ref()?;
    // SAFETY: `len` bytes mapped at `at`.
    let bytes = unsafe { std::slice::from_raw_parts(mapped.at as *const u8, mapped.len) };
    Some((bytes.to_vec(), width, height))
}

/// The view's picture for a screenshot of its window (window_shot.rs): RGBA, premultiplied, `width` × `height`.
#[cfg(feature = "dev")]
pub fn picture(view: *mut AnyObject, width: usize, height: usize) -> Option<Vec<u8>> {
    // SAFETY: a view of the window being shot, alive on the main thread.
    let page = of(unsafe { &*view })?;
    let (bgra, w, h) = pixels(&page.0)?;
    let mut rgba = bgra;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let image = if (w, h) == (width, height) {
        image
    } else {
        image::imageops::resize(&image, width as u32, height as u32, image::imageops::FilterType::Triangle)
    };
    Some(image.into_raw())
}

// Core Graphics and Core Animation, for the frame shown in the view's layer.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
    fn CGColorSpaceRelease(space: *const c_void);
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *const c_void;
    fn CGDataProviderRelease(provider: *const c_void);
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: *const c_void,
        bitmap_info: u32,
        provider: *const c_void,
        decode: *const f64,
        should_interpolate: bool,
        intent: i32,
    ) -> *const c_void;
    fn CGImageRelease(image: *const c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// Puts the page's last frame in its view's layer.
fn show_frame(view: &AnyObject) {
    let Some(page) = of(view) else { return };
    let Some((bgra, width, height)) = pixels(&page.0) else { return };
    // kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little: BGRA in memory.
    const BGRA: u32 = 2 | (2 << 12);
    // SAFETY: Core Graphics objects made and released here; the layer retains the image it's given.
    unsafe {
        let data = CFDataCreate(std::ptr::null(), bgra.as_ptr(), bgra.len() as isize);
        let provider = CGDataProviderCreateWithCFData(data);
        let space = CGColorSpaceCreateDeviceRGB();
        let image = CGImageCreate(width, height, 8, 32, width * 4, space, BGRA, provider, std::ptr::null(), false, 0);
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        CFRelease(data);
        if image.is_null() {
            return;
        }
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            let _: () = msg_send![layer, setContents: image as *mut AnyObject];
            let _: () = msg_send![class!(CATransaction), commit];
        }
        CGImageRelease(image);
    }
}

/// The class of the views Chromium's pages show in: flipped, layer-hosting (the layer shows the frames), taking the
/// keyboard, and handing the user's mouse and keys to the page.
fn view_class() -> Option<&'static AnyClass> {
    static CLASS: OnceLock<Option<usize>> = OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let name = c"FiloChromiumView";
        if let Some(class) = AnyClass::get(name) {
            return Some(class as *const AnyClass as usize);
        }
        let mut builder = ClassBuilder::new(name, AnyClass::get(c"NSView")?)?;
        extern "C-unwind" fn yes(_: &AnyObject, _: Sel) -> Bool {
            Bool::YES
        }
        extern "C-unwind" fn first_mouse(_: &AnyObject, _: Sel, _: *mut AnyObject) -> Bool {
            Bool::YES
        }
        extern "C-unwind" fn mouse(this: &AnyObject, selector: Sel, event: *mut AnyObject) {
            user_mouse(this, selector, event);
        }
        extern "C-unwind" fn key_down(this: &AnyObject, _: Sel, event: *mut AnyObject) {
            key_through_input_method(this, event);
        }
        extern "C-unwind" fn key_up(this: &AnyObject, selector: Sel, event: *mut AnyObject) {
            // A key the input method took went no further.
            if of(this).is_some_and(|page| page.0.borrow().marked.is_none()) {
                user_key(this, selector, event);
            }
        }
        extern "C-unwind" fn command(this: &AnyObject, selector: Sel, _: *mut AnyObject) {
            user_command(this, selector);
        }
        extern "C-unwind" fn set_frame_size(this: &AnyObject, _: Sel, size: NSSize) {
            // SAFETY: NSView's own setter, on the main thread.
            unsafe {
                let _: () = msg_send![super(this, class!(NSView)), setFrameSize: size];
            }
            resized(this, size);
        }
        extern "C-unwind" fn moved_to_window(this: &AnyObject, _: Sel) {
            // Into the browser window or back to its own: drawn at that window's scale.
            // SAFETY: NSView's frame, on the main thread.
            let frame: NSRect = unsafe { msg_send![this, frame] };
            resized(this, frame.size);
            show_frame(this);
        }
        extern "C-unwind" fn cursor_update(this: &AnyObject, _: Sel, _: *mut AnyObject) {
            set_cursor(this);
        }
        // SAFETY: each method's signature matches AppKit's.
        unsafe {
            builder.add_method(sel!(isFlipped), yes as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(acceptsFirstResponder), yes as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(acceptsFirstMouse:), first_mouse as extern "C-unwind" fn(_, _, _) -> _);
            for selector in [
                sel!(mouseDown:),
                sel!(mouseUp:),
                sel!(mouseDragged:),
                sel!(mouseMoved:),
                sel!(rightMouseDown:),
                sel!(rightMouseUp:),
                sel!(rightMouseDragged:),
                sel!(scrollWheel:),
            ] {
                builder.add_method(selector, mouse as extern "C-unwind" fn(_, _, _));
            }
            builder.add_method(sel!(keyDown:), key_down as extern "C-unwind" fn(_, _, _));
            builder.add_method(sel!(keyUp:), key_up as extern "C-unwind" fn(_, _, _));
            input_method::add(&mut builder);
            dropping::add(&mut builder);
            for selector in [sel!(copy:), sel!(cut:), sel!(paste:), sel!(selectAll:), sel!(undo:), sel!(redo:)] {
                builder.add_method(selector, command as extern "C-unwind" fn(_, _, _));
            }
            builder.add_method(sel!(setFrameSize:), set_frame_size as extern "C-unwind" fn(_, _, _));
            builder.add_method(sel!(viewDidMoveToWindow), moved_to_window as extern "C-unwind" fn(_, _));
            builder.add_method(sel!(cursorUpdate:), cursor_update as extern "C-unwind" fn(_, _, _));
        }
        Some(builder.register() as *const AnyClass as usize)
    });
    // SAFETY: a registered class lives as long as the process.
    class.map(|class| unsafe { &*(class as *const AnyClass) })
}

/// The keys being handed through the input method (`interpretKeyEvents:`): the event, and whether it went to the page.
struct Keying {
    event: *mut AnyObject,
    handled: bool,
}

thread_local! {
    static KEYING: RefCell<Option<Keying>> = const { RefCell::new(None) };
}

/// A key down, through the input method: it composes text (`input_method`), or hands the key back as text or a
/// command, which go to the page as the key. A key it says nothing about goes to the page as it is.
fn key_through_input_method(view: &AnyObject, event: *mut AnyObject) {
    let Some(page) = of(view) else { return };
    KEYING.with(|keying| *keying.borrow_mut() = Some(Keying { event, handled: false }));
    // SAFETY: AppKit on the main thread; the array holds the event AppKit is handing the view.
    unsafe {
        let events: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: event];
        let _: () = msg_send![view, interpretKeyEvents: events];
    }
    let keying = KEYING.with(|keying| keying.borrow_mut().take());
    if !keying.is_some_and(|keying| keying.handled) && page.0.borrow().marked.is_none() {
        user_key(view, sel!(keyDown:), event);
    }
}

/// The key being handed through the input method goes to the page as it is (once).
fn key_as_it_is(view: &AnyObject) -> bool {
    let event = KEYING.with(|keying| {
        let mut keying = keying.borrow_mut();
        let keying = keying.as_mut().filter(|keying| !keying.handled)?;
        keying.handled = true;
        Some(keying.event)
    });
    if let Some(event) = event {
        user_key(view, sel!(keyDown:), event);
    }
    event.is_some()
}

/// NSRange, as AppKit's text input passes it.
#[repr(C)]
#[derive(Clone, Copy)]
struct Range {
    location: usize,
    length: usize,
}

// SAFETY: the same layout and encoding as NSRange.
unsafe impl objc2::Encode for Range {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("_NSRange", &[objc2::Encoding::ULong, objc2::Encoding::ULong]);
}

// SAFETY: as NSRange's.
unsafe impl objc2::RefEncode for Range {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&<Range as objc2::Encode>::ENCODING);
}

const NOT_FOUND: usize = isize::MAX as usize;

/// The view as AppKit's text input client (NSTextInputClient): an input method's text, marked while it's composed
/// (`ime` set), then committed, goes to the page as composition (Chromium's IME calls), and its candidate window shows
/// at the text (`firstRectForCharacterRange:`, from the engine's `ime` event).
mod input_method {
    use super::*;

    /// An NSString, or an NSAttributedString's string.
    fn text_of(object: *mut AnyObject) -> String {
        if object.is_null() {
            return String::new();
        }
        // SAFETY: AppKit's text input hands an NSString or an NSAttributedString, read on the main thread.
        unsafe {
            let attributed: bool = msg_send![object, isKindOfClass: class!(NSAttributedString)];
            let string: *mut AnyObject = if attributed { msg_send![object, string] } else { object };
            let text: *const std::ffi::c_char = msg_send![string, UTF8String];
            if text.is_null() { String::new() } else { std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned() }
        }
    }

    extern "C-unwind" fn insert_text(this: &AnyObject, _: Sel, text: *mut AnyObject, _: Range) {
        let Some(page) = of(this) else { return };
        let text = text_of(text);
        let composing = page.0.borrow().marked.is_some();
        let in_key = KEYING.with(|keying| keying.borrow().is_some());
        if !composing && in_key {
            // A plain key's own text: the key itself, as before.
            key_as_it_is(this);
            return;
        }
        // The input method's text, or text from no key (the emoji picker, dictation).
        KEYING.with(|keying| keying.borrow_mut().as_mut().map(|keying| keying.handled = true));
        page.0.borrow_mut().marked = None;
        tell(json!({ "op": "ime", "tab": page.1, "action": "commit", "text": text }));
    }

    extern "C-unwind" fn set_marked_text(this: &AnyObject, _: Sel, text: *mut AnyObject, selected: Range, _: Range) {
        let Some(page) = of(this) else { return };
        let text = text_of(text);
        KEYING.with(|keying| keying.borrow_mut().as_mut().map(|keying| keying.handled = true));
        if text.is_empty() {
            page.0.borrow_mut().marked = None;
            tell(json!({ "op": "ime", "tab": page.1, "action": "cancel" }));
            return;
        }
        page.0.borrow_mut().marked = Some(text.encode_utf16().count());
        let (from, to) = (selected.location, selected.location + selected.length);
        tell(json!({ "op": "ime", "tab": page.1, "action": "set", "text": text, "from": from, "to": to }));
    }

    extern "C-unwind" fn unmark_text(this: &AnyObject, _: Sel) {
        let Some(page) = of(this) else { return };
        if page.0.borrow_mut().marked.take().is_some() {
            tell(json!({ "op": "ime", "tab": page.1, "action": "finish" }));
        }
    }

    extern "C-unwind" fn has_marked_text(this: &AnyObject, _: Sel) -> Bool {
        Bool::new(of(this).is_some_and(|page| page.0.borrow().marked.is_some()))
    }

    extern "C-unwind" fn marked_range(this: &AnyObject, _: Sel) -> Range {
        match of(this).and_then(|page| page.0.borrow().marked) {
            Some(length) => Range { location: 0, length },
            None => Range { location: NOT_FOUND, length: 0 },
        }
    }

    extern "C-unwind" fn selected_range(_: &AnyObject, _: Sel) -> Range {
        Range { location: NOT_FOUND, length: 0 }
    }

    extern "C-unwind" fn valid_attributes(_: &AnyObject, _: Sel) -> *mut AnyObject {
        // SAFETY: an empty, autoreleased NSArray.
        unsafe { msg_send![class!(NSArray), array] }
    }

    extern "C-unwind" fn substring(_: &AnyObject, _: Sel, _: Range, _: *mut Range) -> *mut AnyObject {
        std::ptr::null_mut()
    }

    extern "C-unwind" fn character_index(_: &AnyObject, _: Sel, _: NSPoint) -> usize {
        NOT_FOUND
    }

    /// Where the text being composed is, on the screen: the candidate window shows under it.
    extern "C-unwind" fn first_rect(this: &AnyObject, _: Sel, _: Range, _: *mut Range) -> NSRect {
        let (x, y, width, height) = of(this).and_then(|page| page.0.borrow().ime_rect).unwrap_or((0., 0., 0., 20.));
        // SAFETY: AppKit on the main thread; the view's in a window while it takes keys.
        unsafe {
            let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(width.max(1.), height.max(1.)));
            let in_window: NSRect = msg_send![this, convertRect: rect, toView: std::ptr::null_mut::<AnyObject>()];
            let window: *mut AnyObject = msg_send![this, window];
            if window.is_null() { in_window } else { msg_send![window, convertRectToScreen: in_window] }
        }
    }

    extern "C-unwind" fn do_command(this: &AnyObject, _: Sel, _: Sel) {
        // A key the input method hands back as a command (Return, Delete, an arrow): the key itself, which carries
        // its editing command (`key_events`).
        key_as_it_is(this);
    }

    pub(super) fn add(builder: &mut ClassBuilder) {
        if let Some(protocol) = objc2::runtime::AnyProtocol::get(c"NSTextInputClient") {
            builder.add_protocol(protocol);
        }
        // SAFETY: each method's signature matches NSTextInputClient's.
        unsafe {
            builder.add_method(sel!(insertText:replacementRange:), insert_text as extern "C-unwind" fn(_, _, _, _));
            builder.add_method(
                sel!(setMarkedText:selectedRange:replacementRange:),
                set_marked_text as extern "C-unwind" fn(_, _, _, _, _),
            );
            builder.add_method(sel!(unmarkText), unmark_text as extern "C-unwind" fn(_, _));
            builder.add_method(sel!(hasMarkedText), has_marked_text as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(markedRange), marked_range as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(selectedRange), selected_range as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(validAttributesForMarkedText), valid_attributes as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(
                sel!(attributedSubstringForProposedRange:actualRange:),
                substring as extern "C-unwind" fn(_, _, _, _) -> _,
            );
            builder.add_method(sel!(characterIndexForPoint:), character_index as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(
                sel!(firstRectForCharacterRange:actualRange:),
                first_rect as extern "C-unwind" fn(_, _, _, _) -> _,
            );
            builder.add_method(sel!(doCommandBySelector:), do_command as extern "C-unwind" fn(_, _, _));
        }
    }
}

/// The view as a drag destination: files, text or a link dragged in from another app (Finder, Safari) go to the
/// page as a drag over it, then a drop (the engine's `drop`); what the page says it would do comes back for the
/// pointer's badge.
mod dropping {
    use super::*;

    /// The pasteboard types a page takes dropped.
    pub(super) const TYPES: [&str; 3] = ["public.file-url", "public.url", "public.utf8-plain-text"];

    /// The drag's files, link and text, read from its pasteboard.
    fn carried(info: *mut AnyObject) -> (Vec<String>, Option<String>, Option<String>) {
        let mut files = Vec::new();
        let mut link = None;
        // SAFETY: AppKit on the main thread, reading the drag's pasteboard while AppKit hands it over.
        unsafe {
            let pasteboard: *mut AnyObject = msg_send![info, draggingPasteboard];
            let classes: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: class!(NSURL)];
            let urls: *mut AnyObject =
                msg_send![pasteboard, readObjectsForClasses: classes, options: std::ptr::null_mut::<AnyObject>()];
            let count: usize = if urls.is_null() { 0 } else { msg_send![urls, count] };
            for index in 0..count {
                let url: *mut AnyObject = msg_send![urls, objectAtIndex: index];
                let file: bool = msg_send![url, isFileURL];
                if file {
                    let path: *mut AnyObject = msg_send![url, path];
                    files.push(super::input_method_text(path));
                } else if link.is_none() {
                    let text: *mut AnyObject = msg_send![url, absoluteString];
                    link = Some(super::input_method_text(text));
                }
            }
            let kind = NSString::from_str(TYPES[2]);
            let text: *mut AnyObject = msg_send![pasteboard, stringForType: &*kind];
            let text = (!text.is_null()).then(|| super::input_method_text(text)).filter(|text| !text.is_empty());
            (files, link, text)
        }
    }

    /// Hands the drag's step to the page: its spot, what it carries (entering), and what its source allows.
    fn step(view: &AnyObject, action: &str, info: *mut AnyObject) -> usize {
        let Some(page) = of(view) else { return 0 };
        // SAFETY: AppKit on the main thread, with the dragging info AppKit is handing the view.
        let (at, allowed): (NSPoint, usize) = unsafe {
            let location: NSPoint = msg_send![info, draggingLocation];
            (
                msg_send![view, convertPoint: location, fromView: std::ptr::null_mut::<AnyObject>()],
                msg_send![info, draggingSourceOperationMask],
            )
        };
        let mut request =
            json!({ "op": "drop", "tab": page.1, "action": action, "x": at.x, "y": at.y, "ops": allowed & 0x3f });
        if action == "enter" {
            let (files, link, text) = carried(info);
            request["files"] = json!(files);
            if let Some(link) = link {
                request["url"] = json!(link);
            }
            if let Some(text) = text {
                request["text"] = json!(text);
            }
            page.0.borrow_mut().drop_operation = allowed & 1;
        }
        if let (Ok(Some(answer)), Some(cx)) = (send(request, true), cx()) {
            let shared = page.0.clone();
            cx.spawn(async move |_| {
                if let Ok(Ok(answer)) = answer.await {
                    shared.borrow_mut().drop_operation = answer["operation"].as_u64().unwrap_or(0) as usize;
                }
            })
            .detach();
        }
        page.0.borrow().drop_operation
    }

    extern "C-unwind" fn entered(this: &AnyObject, _: Sel, info: *mut AnyObject) -> usize {
        step(this, "enter", info)
    }

    extern "C-unwind" fn updated(this: &AnyObject, _: Sel, info: *mut AnyObject) -> usize {
        step(this, "over", info)
    }

    extern "C-unwind" fn exited(this: &AnyObject, _: Sel, info: *mut AnyObject) {
        step(this, "leave", info);
    }

    extern "C-unwind" fn prepare(_: &AnyObject, _: Sel, _: *mut AnyObject) -> Bool {
        Bool::YES
    }

    extern "C-unwind" fn perform(this: &AnyObject, _: Sel, info: *mut AnyObject) -> Bool {
        step(this, "drop", info);
        Bool::YES
    }

    pub(super) fn add(builder: &mut ClassBuilder) {
        // SAFETY: each method's signature matches NSDraggingDestination's.
        unsafe {
            builder.add_method(sel!(draggingEntered:), entered as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(draggingUpdated:), updated as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(draggingExited:), exited as extern "C-unwind" fn(_, _, _));
            builder.add_method(sel!(prepareForDragOperation:), prepare as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(performDragOperation:), perform as extern "C-unwind" fn(_, _, _) -> _);
        }
    }
}

fn input_method_text(object: *mut AnyObject) -> String {
    if object.is_null() {
        return String::new();
    }
    // SAFETY: an NSString, read on the main thread.
    unsafe {
        let text: *const std::ffi::c_char = msg_send![object, UTF8String];
        if text.is_null() { String::new() } else { std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned() }
    }
}

/// Chromium's command ids for the menu items Filo does itself (chromium/filo_chromium.mm), and its Print.
const OPEN_LINK: i64 = 26501;
const COPY_LINK: i64 = 26502;
const OPEN_IMAGE: i64 = 26503;
const COPY_IMAGE: i64 = 26504;
const PRINT: i64 = 131;

thread_local! {
    /// The menu item a right-click menu's pick set (`FiloChromiumMenuTarget`).
    static PICKED: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
    /// A check's pick, by label, made without showing the menu; and the last menu's labels (`dev` builds).
    static MENU_PICK: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST_MENU: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The next right-click menu is answered with the item labelled `label` (none: shown as usual) without showing it
/// (`dev` builds).
#[cfg(feature = "dev")]
pub fn pick_from_menu(label: Option<&str>) {
    MENU_PICK.with(|pick| *pick.borrow_mut() = label.map(str::to_string));
}

/// The last right-click menu's items (`dev` builds).
#[cfg(feature = "dev")]
pub fn last_menu() -> Vec<String> {
    LAST_MENU.with(|menu| menu.borrow().clone())
}

/// The target of a right-click menu's items: the pick is the item's tag.
fn menu_target() -> Option<Retained<AnyObject>> {
    static CLASS: OnceLock<Option<usize>> = OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let name = c"FiloChromiumMenuTarget";
        if let Some(class) = AnyClass::get(name) {
            return Some(class as *const AnyClass as usize);
        }
        let mut builder = ClassBuilder::new(name, AnyClass::get(c"NSObject")?)?;
        extern "C-unwind" fn pick(_: &AnyObject, _: Sel, item: *mut AnyObject) {
            // SAFETY: the NSMenuItem AppKit hands its action.
            let tag: isize = unsafe { msg_send![item, tag] };
            PICKED.with(|picked| picked.set(Some(tag as i64)));
        }
        // SAFETY: the action's signature.
        unsafe {
            builder.add_method(sel!(pick:), pick as extern "C-unwind" fn(_, _, _));
        }
        Some(builder.register() as *const AnyClass as usize)
    });
    let class = (*class)?;
    // SAFETY: a registered class; the target is returned retained.
    unsafe {
        let class = &*(class as *const AnyClass);
        msg_send![class, new]
    }
}

/// A right-click menu (the engine's `menu` event) shown as a native one where the page was clicked, for the user's own
/// page on screen: the command picked. None for a page off the screen or an agent's at work (its right-click is for
/// the page's own menu, which this one would cover).
fn context_menu(view: *mut AnyObject, message: &Value) -> Option<i64> {
    // SAFETY: the view is alive while its page is (the tab holds both), on the main thread.
    let view_ref = unsafe { &*view };
    let items = message["items"].as_array().cloned().unwrap_or_default();
    // "&Back" is Back, "Save && Close" Save & Close.
    let label = |item: &Value| {
        item["label"].as_str().unwrap_or_default().replace("&&", "\u{1}").replace('&', "").replace('\u{1}', "&")
    };
    LAST_MENU.with(|menu| {
        *menu.borrow_mut() =
            items.iter().map(|item| if item["separator"] == true { "-".to_string() } else { label(item) }).collect();
    });
    if let Some(wanted) = MENU_PICK.with(|pick| pick.borrow_mut().take()) {
        return items.iter().find(|item| label(item) == wanted).and_then(|item| item["id"].as_i64());
    }
    if native::parked(view_ref) || super::tab_of_view(view).is_some_and(|tab| tab.working()) {
        return None;
    }
    let target = menu_target()?;
    PICKED.with(|picked| picked.set(None));
    // SAFETY: AppKit on the main thread; the menu and its items are retained here while it shows (modally, until a
    // pick or a click away).
    unsafe {
        let menu: Retained<AnyObject> = msg_send![class!(NSMenu), new];
        let _: () = msg_send![&*menu, setAutoenablesItems: false];
        for item in &items {
            if item["separator"] == true {
                let separator: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
                let _: () = msg_send![&*menu, addItem: separator];
                continue;
            }
            let title = NSString::from_str(&label(item));
            let key = NSString::from_str("");
            let allocated: Allocated<AnyObject> = msg_send![class!(NSMenuItem), alloc];
            let entry: Retained<AnyObject> =
                msg_send![allocated, initWithTitle: &*title, action: sel!(pick:), keyEquivalent: &*key];
            let _: () = msg_send![&*entry, setTarget: &*target];
            let _: () = msg_send![&*entry, setTag: item["id"].as_i64().unwrap_or(-1) as isize];
            let _: () = msg_send![&*entry, setEnabled: item["enabled"] != false];
            if item["checked"] == true {
                let _: () = msg_send![&*entry, setState: 1isize];
            }
            let _: () = msg_send![&*menu, addItem: &*entry];
        }
        let at = NSPoint::new(message["x"].as_f64().unwrap_or(0.), message["y"].as_f64().unwrap_or(0.));
        let _: Bool = msg_send![&*menu, popUpMenuPositioningItem: std::ptr::null_mut::<AnyObject>(), atLocation: at, inView: view];
    }
    PICKED.with(std::cell::Cell::take)
}

/// Puts `text` on the clipboard (a link's or a picture's address).
fn copy_text(text: &str) {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        let _: isize = msg_send![pasteboard, clearContents];
        let text = NSString::from_str(text);
        let kind = NSString::from_str("public.utf8-plain-text");
        let _: Bool = msg_send![pasteboard, setString: &*text, forType: &*kind];
    }
}

fn new_view(width: f64, height: f64) -> Option<Retained<AnyObject>> {
    let class = view_class()?;
    // NSTrackingMouseMoved | CursorUpdate | ActiveInKeyWindow | InVisibleRect
    const TRACKING: usize = 0x02 | 0x04 | 0x20 | 0x200;
    // SAFETY: AppKit on the main thread; the view is returned retained, its layer and tracking area kept by it.
    unsafe {
        let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(width, height));
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        let view: Option<Retained<AnyObject>> = msg_send![allocated, initWithFrame: frame];
        let view = view?;
        // A layer of its own first, then wantsLayer: layer-hosting, so AppKit never draws over its contents.
        let layer: *mut AnyObject = msg_send![class!(CALayer), layer];
        let white: *mut AnyObject = msg_send![class!(NSColor), whiteColor];
        let cg: *const c_void = msg_send![white, CGColor];
        let _: () = msg_send![layer, setBackgroundColor: cg];
        let gravity = NSString::from_str("resize");
        let _: () = msg_send![layer, setContentsGravity: &*gravity];
        let _: () = msg_send![&*view, setLayer: layer];
        let _: () = msg_send![&*view, setWantsLayer: true];
        let allocated: Allocated<AnyObject> = msg_send![class!(NSTrackingArea), alloc];
        let area: Option<Retained<AnyObject>> = msg_send![allocated, initWithRect: NSRect::ZERO, options: TRACKING, owner: &*view, userInfo: std::ptr::null_mut::<AnyObject>()];
        if let Some(area) = area {
            let _: () = msg_send![&*view, addTrackingArea: &*area];
        }
        let types: Vec<Retained<NSString>> = dropping::TYPES.iter().map(|kind| NSString::from_str(kind)).collect();
        let types: Vec<&NSString> = types.iter().map(|kind| &**kind).collect();
        let types = objc2_foundation::NSArray::from_slice(&types);
        let _: () = msg_send![&*view, registerForDraggedTypes: &*types];
        Some(view)
    }
}

/// The view's size changed (moved into the browser window, or back): the page follows it.
fn resized(view: &AnyObject, size: NSSize) {
    let Some(page) = of(view) else { return };
    let scale = scale_for(Some(view));
    let (width, height) = (size.width.round().max(1.), size.height.round().max(1.));
    let changed = {
        let mut shared = page.0.borrow_mut();
        let changed = shared.size != (width, height, scale);
        shared.size = (width, height, scale);
        changed
    };
    if changed {
        tell(json!({ "op": "resize", "tab": page.1, "width": width, "height": height, "scale": scale }));
    }
}

fn set_cursor(view: &AnyObject) {
    let Some(page) = of(view) else { return };
    let cursor = page.0.borrow().cursor;
    // SAFETY: AppKit on the main thread.
    unsafe {
        let cursor: *mut AnyObject = match cursor {
            1 => msg_send![class!(NSCursor), crosshairCursor],
            2 => msg_send![class!(NSCursor), pointingHandCursor],
            3 => msg_send![class!(NSCursor), IBeamCursor],
            44 => msg_send![class!(NSCursor), operationNotAllowedCursor],
            _ => msg_send![class!(NSCursor), arrowCursor],
        };
        let _: () = msg_send![cursor, set];
    }
}

/// DevTools' modifier bits for AppKit's flags.
fn modifiers(flags: usize) -> u32 {
    let mut bits = 0;
    if flags & OPTION != 0 {
        bits |= 1;
    }
    if flags & CONTROL != 0 {
        bits |= 2;
    }
    if flags & COMMAND != 0 {
        bits |= 4;
    }
    if flags & SHIFT != 0 {
        bits |= 8;
    }
    bits
}

/// The user's mouse in the browser window, to the page.
fn user_mouse(view: &AnyObject, selector: Sel, event: *mut AnyObject) {
    let Some(page) = of(view) else { return };
    // SAFETY: AppKit on the main thread, with the event AppKit is handing the view.
    unsafe {
        let location: NSPoint = msg_send![event, locationInWindow];
        let at: NSPoint = msg_send![view, convertPoint: location, fromView: std::ptr::null_mut::<AnyObject>()];
        let flags: usize = msg_send![event, modifierFlags];
        let modifiers = modifiers(flags);
        if selector == sel!(scrollWheel:) {
            let precise: bool = msg_send![event, hasPreciseScrollingDeltas];
            let dx: f64 = msg_send![event, scrollingDeltaX];
            let dy: f64 = msg_send![event, scrollingDeltaY];
            let lines = if precise { 1. } else { 40. };
            devtools_now(
                &page.1,
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseWheel", "x": at.x, "y": at.y, "deltaX": -dx * lines, "deltaY": -dy * lines, "modifiers": modifiers }),
            );
            return;
        }
        let clicks: isize = if selector == sel!(mouseMoved:) { 0 } else { msg_send![event, clickCount] };
        let (kind, button, buttons) = if selector == sel!(mouseDown:) {
            let window: *mut AnyObject = msg_send![view, window];
            if !window.is_null() {
                let _: bool = msg_send![window, makeFirstResponder: view];
            }
            ("mousePressed", "left", 1)
        } else if selector == sel!(mouseUp:) {
            ("mouseReleased", "left", 0)
        } else if selector == sel!(mouseDragged:) {
            ("mouseMoved", "left", 1)
        } else if selector == sel!(rightMouseDown:) {
            ("mousePressed", "right", 2)
        } else if selector == sel!(rightMouseUp:) {
            ("mouseReleased", "right", 0)
        } else if selector == sel!(rightMouseDragged:) {
            ("mouseMoved", "right", 2)
        } else {
            set_cursor(view);
            ("mouseMoved", "none", 0)
        };
        devtools_now(
            &page.1,
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": at.x, "y": at.y, "button": button, "buttons": buttons, "clickCount": clicks, "modifiers": modifiers }),
        );
    }
}

/// The user's keys in the browser window, to the page.
fn user_key(view: &AnyObject, selector: Sel, event: *mut AnyObject) {
    let Some(page) = of(view) else { return };
    let string = |object: *mut AnyObject| -> String {
        if object.is_null() {
            return String::new();
        }
        // SAFETY: an NSString, read on the main thread.
        unsafe {
            let text: *const std::ffi::c_char = msg_send![object, UTF8String];
            if text.is_null() { String::new() } else { std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned() }
        }
    };
    // SAFETY: AppKit on the main thread, with the event AppKit is handing the view.
    let key = unsafe {
        let code: u16 = msg_send![event, keyCode];
        let flags: usize = msg_send![event, modifierFlags];
        let characters: *mut AnyObject = msg_send![event, characters];
        let plain: *mut AnyObject = msg_send![event, charactersIgnoringModifiers];
        Key { characters: string(characters), plain: string(plain), code, flags: flags & 0xFFFF_0000 }
    };
    let down = selector == sel!(keyDown:);
    for params in key_events(&key, down) {
        let method = if params.get("text_only").is_some() { "Input.insertText" } else { "Input.dispatchKeyEvent" };
        devtools_now(&page.1, method, without_marker(params));
    }
}

/// The Edit menu's commands (⌘C and the like reach the view as these), to the page.
fn user_command(view: &AnyObject, selector: Sel) {
    let Some(page) = of(view) else { return };
    let spec = match selector.name().to_str().unwrap_or_default() {
        "copy:" => "cmd+c",
        "cut:" => "cmd+x",
        "paste:" => "cmd+v",
        "selectAll:" => "cmd+a",
        "undo:" => "cmd+z",
        _ => "cmd+shift+z",
    };
    if let Some(key) = Key::named(spec) {
        page.key(&key);
    }
}

/// A Mac key code's DOM code and Windows key code (what DevTools' key events carry).
fn codes(code: u16) -> Option<(&'static str, u32)> {
    Some(match code {
        0 => ("KeyA", 65),
        1 => ("KeyS", 83),
        2 => ("KeyD", 68),
        3 => ("KeyF", 70),
        4 => ("KeyH", 72),
        5 => ("KeyG", 71),
        6 => ("KeyZ", 90),
        7 => ("KeyX", 88),
        8 => ("KeyC", 67),
        9 => ("KeyV", 86),
        11 => ("KeyB", 66),
        12 => ("KeyQ", 81),
        13 => ("KeyW", 87),
        14 => ("KeyE", 69),
        15 => ("KeyR", 82),
        16 => ("KeyY", 89),
        17 => ("KeyT", 84),
        18 => ("Digit1", 49),
        19 => ("Digit2", 50),
        20 => ("Digit3", 51),
        21 => ("Digit4", 52),
        22 => ("Digit6", 54),
        23 => ("Digit5", 53),
        24 => ("Equal", 187),
        25 => ("Digit9", 57),
        26 => ("Digit7", 55),
        27 => ("Minus", 189),
        28 => ("Digit8", 56),
        29 => ("Digit0", 48),
        30 => ("BracketRight", 221),
        31 => ("KeyO", 79),
        32 => ("KeyU", 85),
        33 => ("BracketLeft", 219),
        34 => ("KeyI", 73),
        35 => ("KeyP", 80),
        36 => ("Enter", 13),
        37 => ("KeyL", 76),
        38 => ("KeyJ", 74),
        39 => ("Quote", 222),
        40 => ("KeyK", 75),
        41 => ("Semicolon", 186),
        42 => ("Backslash", 220),
        43 => ("Comma", 188),
        44 => ("Slash", 191),
        45 => ("KeyN", 78),
        46 => ("KeyM", 77),
        47 => ("Period", 190),
        48 => ("Tab", 9),
        49 => ("Space", 32),
        50 => ("Backquote", 192),
        51 => ("Backspace", 8),
        53 => ("Escape", 27),
        115 => ("Home", 36),
        116 => ("PageUp", 33),
        117 => ("Delete", 46),
        119 => ("End", 35),
        121 => ("PageDown", 34),
        123 => ("ArrowLeft", 37),
        124 => ("ArrowRight", 39),
        125 => ("ArrowDown", 40),
        126 => ("ArrowUp", 38),
        _ => return None,
    })
}

/// The editing command a key runs on a Mac (Chromium leaves these to the browser's view, which has none here), as
/// Playwright sends them.
fn mac_command(dom_code: &str, flags: usize) -> Option<&'static str> {
    let shift = flags & SHIFT != 0;
    let option = flags & OPTION != 0;
    let command = flags & COMMAND != 0;
    Some(match (dom_code, command, option, shift) {
        ("Backspace", false, false, _) => "deleteBackward",
        ("Backspace", false, true, _) => "deleteWordBackward",
        ("Backspace", true, _, _) => "deleteToBeginningOfLine",
        ("Delete", false, false, _) => "deleteForward",
        ("Enter", false, false, _) => "insertNewline",
        ("Tab", false, false, false) => "insertTab",
        ("Tab", false, false, true) => "insertBacktab",
        ("Escape", false, false, _) => "cancelOperation",
        ("ArrowLeft", false, false, false) => "moveLeft",
        ("ArrowLeft", false, false, true) => "moveLeftAndModifySelection",
        ("ArrowLeft", false, true, false) => "moveWordLeft",
        ("ArrowLeft", true, _, false) => "moveToLeftEndOfLine",
        ("ArrowRight", false, false, false) => "moveRight",
        ("ArrowRight", false, false, true) => "moveRightAndModifySelection",
        ("ArrowRight", false, true, false) => "moveWordRight",
        ("ArrowRight", true, _, false) => "moveToRightEndOfLine",
        ("ArrowUp", false, false, false) => "moveUp",
        ("ArrowUp", false, false, true) => "moveUpAndModifySelection",
        ("ArrowUp", true, _, _) => "moveToBeginningOfDocument",
        ("ArrowDown", false, false, false) => "moveDown",
        ("ArrowDown", false, false, true) => "moveDownAndModifySelection",
        ("ArrowDown", true, _, _) => "moveToEndOfDocument",
        ("Home", false, false, _) => "scrollToBeginningOfDocument",
        ("End", false, false, _) => "scrollToEndOfDocument",
        ("PageUp", false, false, _) => "scrollPageUp",
        ("PageDown", false, false, _) => "scrollPageDown",
        ("KeyA", true, false, false) => "selectAll",
        ("KeyC", true, false, false) => "copy",
        ("KeyX", true, false, false) => "cut",
        ("KeyV", true, false, false) => "paste",
        ("KeyZ", true, false, false) => "undo",
        ("KeyZ", true, false, true) => "redo",
        _ => return None,
    })
}

/// DevTools' events for a key going down (`down`) or up. A character no key types (é, emoji) is inserted as text, as
/// an input method does (marked `text_only`, for Input.insertText).
fn key_events(key: &Key, down: bool) -> Vec<Value> {
    // Key code 0 is A's, and also what `Key::typing` gives a character no key types.
    let no_key = key.code == 0 && !key.plain.eq_ignore_ascii_case("a");
    let Some((dom_code, windows)) = codes(key.code).filter(|_| !no_key) else {
        return if down
            && !key.characters.is_empty()
            && key.characters.chars().all(|c| !c.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&c))
        {
            vec![json!({ "text_only": true, "text": key.characters })]
        } else {
            Vec::new()
        };
    };
    let dom_key = match dom_code {
        "Enter" | "Tab" | "Backspace" | "Escape" | "Delete" | "Home" | "End" | "PageUp" | "PageDown" | "ArrowLeft"
        | "ArrowRight" | "ArrowUp" | "ArrowDown" => dom_code.to_string(),
        "Space" => " ".to_string(),
        _ if key.flags & (COMMAND | CONTROL) != 0 => key.plain.clone(),
        _ => key.characters.clone(),
    };
    let modifiers = modifiers(key.flags);
    if !down {
        return vec![
            json!({ "type": "keyUp", "key": dom_key, "code": dom_code, "windowsVirtualKeyCode": windows, "nativeVirtualKeyCode": key.code, "modifiers": modifiers }),
        ];
    }
    // What the key types: printable characters and Return, not with ⌘ or ⌃.
    let text = if key.flags & (COMMAND | CONTROL) != 0 {
        String::new()
    } else if dom_code == "Enter" {
        "\r".to_string()
    } else if key.characters.chars().all(|c| !c.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&c)) {
        key.characters.clone()
    } else {
        String::new()
    };
    let mut params = json!({
        "type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key": dom_key,
        "code": dom_code,
        "windowsVirtualKeyCode": windows,
        "nativeVirtualKeyCode": key.code,
        "modifiers": modifiers,
    });
    if !text.is_empty() {
        params["text"] = json!(text);
        params["unmodifiedText"] = json!(if dom_code == "Enter" { "\r".to_string() } else { key.plain.clone() });
    }
    if let Some(command) = mac_command(dom_code, key.flags) {
        params["commands"] = json!([command]);
    }
    vec![params]
}

/// Cookies from a WebKit cookie file (`Cookies.binarycookies`, read from a copy; FILO_OPEN=chromium-check), as the
/// engine takes them (`set_cookies`). WebKit's format: "cook", the number of pages and their sizes (big-endian), then pages of cookies, each with its
/// flags (1 secure, 4 HTTP only), the offsets of its domain, name, path and value, and its expiry (seconds since 2001).
#[cfg(feature = "dev")]
pub fn webkit_cookies(data: &[u8]) -> Result<Vec<Value>, String> {
    let be = |at: usize| -> Option<u32> { Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?)) };
    if data.get(..4) != Some(b"cook") {
        return Err("not a WebKit cookie file".into());
    }
    let pages = be(4).ok_or("cut short")? as usize;
    let mut at = 8 + pages * 4;
    let mut cookies = Vec::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0., |time| time.as_secs_f64());
    for index in 0..pages {
        let size = be(8 + index * 4).ok_or("cut short")? as usize;
        let page = data.get(at..at + size).ok_or("cut short")?;
        at += size;
        let le = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(page.get(at..at + 4)?.try_into().ok()?)) };
        let count = le(4).ok_or("cut short")? as usize;
        for number in 0..count {
            let Some(start) = le(8 + number * 4).map(|start| start as usize) else { continue };
            let Some(cookie) = page.get(start..) else { continue };
            let word = |at: usize| -> Option<usize> {
                Some(u32::from_le_bytes(cookie.get(at..at + 4)?.try_into().ok()?) as usize)
            };
            let text = |at: usize| -> Option<String> {
                let bytes = cookie.get(at..)?;
                let end = bytes.iter().position(|byte| *byte == 0)?;
                Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
            };
            let (Some(flags), Some(domain), Some(name), Some(path), Some(value)) = (
                word(8),
                word(16).and_then(text),
                word(20).and_then(text),
                word(24).and_then(text),
                word(28).and_then(text),
            ) else {
                continue;
            };
            let expiry = cookie
                .get(40..48)
                .and_then(|bytes| bytes.try_into().ok())
                .map(f64::from_le_bytes)
                .map_or(0., |seconds| seconds + 978_307_200.);
            let mut entry = json!({
                "name": name,
                "value": value,
                "domain": domain,
                "path": if path.is_empty() { "/".to_string() } else { path },
                "secure": flags & 1 != 0,
                "httpOnly": flags & 4 != 0,
            });
            // A cookie with no expiry (WebKit keeps them only while it runs) stays a session cookie.
            if expiry > now {
                entry["expires"] = json!(expiry);
            }
            cookies.push(entry);
        }
    }
    Ok(cookies)
}

/// A page's size in points, the scale it draws at, and its last frame's width in pixels (a check's, `dev` builds).
#[cfg(feature = "dev")]
pub fn drawn(view: &AnyObject) -> Option<(f64, f64, usize)> {
    let page = of(view)?;
    let page = page.0.borrow();
    Some((page.size.0, page.size.2, page.frame.as_ref().map_or(0, |frame| frame.1)))
}

/// The engine's processes, for the memory a check measures: pid and kind (main, renderer, gpu, utility).
#[cfg(feature = "dev")]
pub fn processes() -> Vec<(u32, String)> {
    let Some(root) =
        HELPER.with(|helper| helper.borrow().as_ref().and_then(|helper| Some(helper.child.as_ref()?.id())))
    else {
        return Vec::new();
    };
    let listing = std::process::Command::new("/bin/ps").args(["-axo", "pid=,ppid=,command="]).output();
    let Ok(listing) = listing else { return Vec::new() };
    let mut found = vec![(root, "main".to_string())];
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid)) =
            (parts.next().and_then(|p| p.parse::<u32>().ok()), parts.next().and_then(|p| p.parse::<u32>().ok()))
        else {
            continue;
        };
        if ppid != root {
            continue;
        }
        let kind = line
            .split("--type=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("helper")
            .to_string();
        found.push((pid, kind));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_carry_what_chromium_needs_on_a_mac() {
        let typed = key_events(&Key::typing('A'), true);
        assert_eq!(typed[0]["type"], "keyDown");
        assert_eq!(typed[0]["text"], "A");
        assert_eq!(typed[0]["code"], "KeyA");
        assert_eq!(typed[0]["modifiers"], 8);
        let enter = key_events(&Key::named("Enter").unwrap(), true);
        assert_eq!(enter[0]["text"], "\r");
        assert_eq!(enter[0]["commands"], json!(["insertNewline"]));
        let back = key_events(&Key::named("Backspace").unwrap(), true);
        assert_eq!(back[0]["type"], "rawKeyDown");
        assert_eq!(back[0]["commands"], json!(["deleteBackward"]));
        let all = key_events(&Key::named("cmd+a").unwrap(), true);
        assert_eq!((all[0]["type"].as_str(), all[0]["modifiers"].as_u64()), (Some("rawKeyDown"), Some(4)));
        assert_eq!(all[0]["commands"], json!(["selectAll"]));
        assert_eq!(key_events(&Key::typing('é'), true), vec![json!({ "text_only": true, "text": "é" })]);
        assert_eq!(key_events(&Key::named("Tab").unwrap(), false)[0]["type"], "keyUp");
    }

    #[test]
    #[cfg(feature = "dev")]
    fn reads_a_webkit_cookie_file() {
        // One page with one cookie: secure and HTTP only, expiring in 2100.
        let mut cookie = vec![0u8; 56];
        let strings = [".example.com\0", "sid\0", "/\0", "abc\0"];
        let mut offsets = Vec::new();
        for text in strings {
            offsets.push(cookie.len() as u32);
            cookie.extend_from_slice(text.as_bytes());
        }
        let size = cookie.len() as u32;
        cookie[0..4].copy_from_slice(&size.to_le_bytes());
        cookie[8..12].copy_from_slice(&5u32.to_le_bytes());
        for (index, offset) in offsets.iter().enumerate() {
            cookie[16 + index * 4..20 + index * 4].copy_from_slice(&offset.to_le_bytes());
        }
        cookie[40..48].copy_from_slice(&(3_124_137_600f64 - 978_307_200.).to_le_bytes());
        let mut page = vec![0, 0, 1, 0];
        page.extend_from_slice(&1u32.to_le_bytes());
        page.extend_from_slice(&16u32.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.extend_from_slice(&cookie);
        let mut file = b"cook".to_vec();
        file.extend_from_slice(&1u32.to_be_bytes());
        file.extend_from_slice(&(page.len() as u32).to_be_bytes());
        file.extend_from_slice(&page);
        let cookies = webkit_cookies(&file).unwrap();
        assert_eq!(cookies.len(), 1);
        let cookie = &cookies[0];
        assert_eq!((cookie["name"].as_str(), cookie["value"].as_str()), (Some("sid"), Some("abc")));
        assert_eq!((cookie["domain"].as_str(), cookie["path"].as_str()), (Some(".example.com"), Some("/")));
        assert_eq!((cookie["secure"].as_bool(), cookie["httpOnly"].as_bool()), (Some(true), Some(true)));
        assert_eq!(cookie["expires"].as_f64(), Some(3_124_137_600.));
        assert!(webkit_cookies(b"nope").is_err());
    }
}

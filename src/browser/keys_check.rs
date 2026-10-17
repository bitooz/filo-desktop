// `FILO_OPEN=browser-keys-check` (`dev` builds, on a data folder of its own): the user's keyboard in the browser
// window, against the test site's `/chat` (browser/check.rs). Clicks and keys are queued for the app the way the mouse
// and keyboard send them (`native::as_user`), so AppKit and GPUI's window see them as they see the user's. Each case
// starts with the address bar focused, as after typing an address, then clicks into the page. It checks that Return
// submits a form, sends a chat box and presses a button the page acts on from its script, leaving the key unused (as
// Google's sign-in does: the key went on to the address bar, which reloaded the page), that ⌘A selects the page's
// field and not the address bar's, that an agent's keys (handed to the web view) still reach the page in the window
// and don't reach the address bar either, and that Return in the address bar still opens what's typed there. Each
// check prints a line; the process ends with 1 when any failed. `FILO_FIXTURE_SHOTS=<dir>`: a picture of the window
// after the page's keys.

use std::time::Duration;

use gpui::{App, AsyncApp, Entity, WindowHandle};
use gpui_component::Root;
use serde_json::json;

use super::native::{self, Key};
use super::window::BrowserWindow;

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        println!(
            "browser keys check: {} {name}: {}",
            if ok { "ok" } else { "FAILED" },
            detail.as_ref().replace('\n', " | ")
        );
        if !ok {
            self.failed += 1;
        }
    }
}

pub fn open(cx: &mut App) {
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    let port = super::check::serve();
    let site = format!("http://127.0.0.1:{port}");
    cx.spawn(async move |cx| {
        let mut checks = Checks { failed: 0 };
        run(cx, &mut checks, &site, shots.as_deref()).await;
        println!(
            "browser-keys-check: {}",
            if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) }
        );
        crate::window_shot::flush(cx.background_executor()).await;
        super::remove_test_store(cx).await;
        cx.update(|_| super::quit()).ok();
        std::process::exit(if checks.failed == 0 { 0 } else { 1 });
    })
    .detach();
}

async fn sleep(cx: &AsyncApp, ms: u64) {
    cx.background_executor().timer(Duration::from_millis(ms)).await;
}

/// What `body` (a function body) returns in the page, as a string.
async fn js(cx: &AsyncApp, tab: &super::Tab, body: &str) -> String {
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.update(|_| native::run_js(tab.view(), body, "", move |result| drop(tx.send(result)))).ok();
    match rx.await {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => format!("error: {error}"),
        Err(_) => "no answer".into(),
    }
}

/// What the page says it got (`#said`).
async fn said(cx: &AsyncApp, tab: &super::Tab) -> String {
    js(cx, tab, "return document.getElementById('said')?.textContent ?? ''").await
}

/// A click at `spot` of the page, then `keys` one at a time, as a hand types them.
async fn click_then_keys(cx: &AsyncApp, tab: &super::Tab, spot: (f64, f64), keys: &[Key]) {
    cx.update(|_| native::as_user(tab.view(), Some(spot), &[])).ok();
    sleep(cx, 150).await;
    for key in keys {
        cx.update(|_| native::as_user(tab.view(), None, std::slice::from_ref(key))).ok();
        sleep(cx, 40).await;
    }
}

/// `text` key by key, then Return.
fn typed_then_return(text: &str) -> Vec<Key> {
    text.chars().map(Key::typing).chain(Key::named("Enter")).collect()
}

struct Browser {
    handle: WindowHandle<Root>,
    view: Entity<BrowserWindow>,
}

impl Browser {
    /// The address bar focused, as after typing an address (`text` there, if any).
    async fn focus_address(&self, cx: &mut AsyncApp, text: Option<&str>) {
        self.handle
            .update(cx, |_, window, cx| self.view.update(cx, |view, cx| view.focus_address(text, window, cx)))
            .ok();
        sleep(cx, 300).await;
    }

    fn address(&self, cx: &AsyncApp) -> String {
        cx.update(|cx| self.view.read(cx).address_value(cx)).unwrap_or_default()
    }
}

async fn run(cx: &mut AsyncApp, checks: &mut Checks, site: &str, shots: Option<&str>) {
    let opened = cx.update(|cx| {
        let tab = super::open_for_user(&format!("{site}/chat"))?;
        super::window::open(Some(tab.id.clone()), cx);
        let (handle, view) = BrowserWindow::open_view(cx).ok_or("no browser window")?;
        Ok::<_, String>((tab, Browser { handle, view }))
    });
    let (tab, browser) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(error)) => return checks.check("the test page opens in the browser window", false, error),
        Err(error) => return checks.check("the test page opens in the browser window", false, error.to_string()),
    };
    let mut page = String::new();
    for _ in 0..50 {
        page = said(cx, &tab).await;
        if page.contains("loads 1") && cx.update(|_| native::has_keys(tab.view()).0).unwrap_or(false) {
            break;
        }
        sleep(cx, 200).await;
    }
    let (key_window, _) = cx.update(|_| native::has_keys(tab.view())).unwrap_or_default();
    checks.check(
        "the page loads in the browser window, the key window",
        page.contains("loads 1") && key_window,
        format!("{page}; key window {key_window}"),
    );

    // A form: Return submits it.
    browser.focus_address(cx, None).await;
    click_then_keys(cx, &tab, (200., 98.), &typed_then_return("hi")).await;
    sleep(cx, 800).await;
    let page = said(cx, &tab).await;
    checks.check(
        "Return in a page's field submits its form",
        page.contains("submitted hi") && page.contains("loads 1"),
        &page,
    );

    // A button the page acts on from its own script, leaving Return unused: the page carries on, not reloaded.
    browser.focus_address(cx, None).await;
    click_then_keys(cx, &tab, (70., 308.), &typed_then_return("")).await;
    sleep(cx, 1200).await;
    let page = said(cx, &tab).await;
    let url = cx.update(|_| tab.state().1).unwrap_or_default();
    checks.check(
        "Return the page leaves unused doesn't reach the address bar",
        page.contains("next") && page.contains("loads 1"),
        format!("{page}; page at {url}"),
    );

    // ⌘A in the page's field selects its text, not the address bar's.
    browser.focus_address(cx, None).await;
    let mut keys: Vec<Key> = "abc".chars().map(Key::typing).collect();
    keys.extend([Key::named("cmd+a").expect("cmd+a"), Key::typing('x')]);
    click_then_keys(cx, &tab, (200., 98.), &keys).await;
    sleep(cx, 800).await;
    let value = js(cx, &tab, "return document.getElementById('field').value").await;
    let address = browser.address(cx);
    checks.check(
        "⌘A in a page's field selects the field's text",
        value == "x" && address.ends_with("/chat"),
        format!("field {value:?}, address bar {address:?}"),
    );
    js(cx, &tab, "document.getElementById('field').value = ''; return ''").await;

    // A chat box that sends on Return (the page's own keydown handler).
    browser.focus_address(cx, None).await;
    click_then_keys(cx, &tab, (200., 216.), &typed_then_return("yo")).await;
    sleep(cx, 800).await;
    let page = said(cx, &tab).await;
    checks.check(
        "Return in a page's chat box sends it",
        page.contains("sent yo trusted=true") && page.contains("loads 1"),
        &page,
    );
    if let Some(dir) = shots {
        crate::window_shot::save_largest(format!("{dir}/browser-return-sends.png")).await;
    }

    // An agent's keys, handed to the web view: they reach the page in the window, and a Return the page leaves
    // unused doesn't reach the address bar.
    browser.focus_address(cx, None).await;
    let clicked = super::check::text(
        &super::check::ask(cx, json!({ "op": "click_at", "tab": tab.id, "x": 200, "y": 216 })).await,
    );
    let keyed = super::check::text(
        &super::check::ask(cx, json!({ "op": "keys", "tab": tab.id, "text": "agent", "keys": ["Enter"] })).await,
    );
    browser.focus_address(cx, None).await;
    let unused = super::check::text(
        &super::check::ask(cx, json!({ "op": "click_at", "tab": tab.id, "x": 600, "y": 500 })).await,
    );
    let pressed =
        super::check::text(&super::check::ask(cx, json!({ "op": "keys", "tab": tab.id, "keys": ["Enter"] })).await);
    sleep(cx, 800).await;
    let page = said(cx, &tab).await;
    checks.check(
        "an agent's typing and Return reach the page, and not the address bar",
        page.contains("sent agent trusted=true") && page.contains("loads 1"),
        format!("{page} || {clicked} || {keyed} || {unused} || {pressed}"),
    );

    // The address bar still takes its own Return.
    let whoami = format!("{site}/whoami");
    browser.focus_address(cx, Some(&whoami)).await;
    cx.update(|_| native::as_user(tab.view(), None, &[Key::named("Enter").expect("Enter")])).ok();
    let mut url = String::new();
    for _ in 0..30 {
        sleep(cx, 200).await;
        url = cx.update(|_| tab.state().1).unwrap_or_default();
        if url.ends_with("/whoami") {
            break;
        }
    }
    checks.check(
        "Return in the address bar opens what's typed",
        url.ends_with("/whoami"),
        format!("page at {url}, address bar {}", browser.address(cx)),
    );
}

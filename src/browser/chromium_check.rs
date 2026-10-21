// `FILO_OPEN=chromium-check` (`dev` builds, on a data folder of its own, Filo Chromium built by chromium/build.sh):
// what Chromium as the browser's engine (browser/chromium.rs) would change, measured. Every step is an agent's tool call
// through the socket, as in browser/check.rs. It prints a line per finding:
//
// - memory: the same pages opened one by one in Chromium tabs and in WebKit tabs, and what each engine's processes
//   take (`footprint`'s physical footprint, what Activity Monitor shows) after each;
// - Google's sign-in: the address `FILO_CHECK_GOOGLE_EMAIL` (a made-up one by default) typed into accounts.google.com
//   in each engine, and what Google answers (WebKit's: "This browser or app may not be secure");
// - `FILO_CHROMIUM_COOKIES=<a copy of a WebKit Cookies.binarycookies>`: its cookies put in Chromium's profile, then
//   each of `FILO_CHROMIUM_SITES` (comma-separated addresses) opened and said signed in or not, by its address and its
//   links (no page text is printed);
// - the browser window on a Chromium tab, with a click and keys as the user's (`native::as_user`), and its picture
//   (`FILO_FIXTURE_SHOTS=<dir>`: chromium-window.png).

use std::time::Duration;

use gpui::{App, AsyncApp};
use regex::Regex;
use serde_json::{Value, json};

use super::check::{ask, text};
use super::native::{self, Key};

/// The pages memory is measured on: an article, a busy app-like page, a plain list.
const MEMORY_SITES: [&str; 3] =
    ["https://en.wikipedia.org/wiki/Web_browser", "https://github.com/rust-lang/rust", "https://news.ycombinator.com"];

async fn sleep(cx: &AsyncApp, ms: u64) {
    cx.background_executor().timer(Duration::from_millis(ms)).await;
}

fn say(line: impl AsRef<str>) {
    println!("chromium check: {}", line.as_ref().replace('\n', " | "));
}

/// The tab an "Opened tab t3." answer names.
fn opened(answer: &str) -> String {
    Regex::new(r"Opened tab ([tp]\d+)")
        .ok()
        .and_then(|found| found.captures(answer).map(|found| found[1].to_string()))
        .unwrap_or_default()
}

/// Physical footprints in MB of `pids`, as `footprint` says them, and their sum.
fn footprints(pids: &[u32]) -> (Vec<(u32, f64)>, f64) {
    if pids.is_empty() {
        return (Vec::new(), 0.);
    }
    let mut command = std::process::Command::new("/usr/bin/footprint");
    command.args(["-f", "bytes"]);
    for pid in pids {
        command.args(["-p", &pid.to_string()]);
    }
    let Ok(output) = command.output() else { return (Vec::new(), 0.) };
    let line = Regex::new(r"\[(\d+)\]: \S+\s+Footprint: (\d+) B").expect("pattern");
    let found: Vec<(u32, f64)> = line
        .captures_iter(&String::from_utf8_lossy(&output.stdout))
        .filter_map(|found| Some((found[1].parse().ok()?, found[2].parse::<f64>().ok()? / 1_048_576.)))
        .collect();
    let sum = found.iter().map(|(_, mb)| mb).sum();
    (found, sum)
}

/// WebKit's processes on this Mac (WebContent, GPU, Networking), by pid.
fn webkit_processes() -> Vec<(u32, String)> {
    let Ok(listing) = std::process::Command::new("/bin/ps").args(["-axo", "pid=,comm="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            let kind = command.rsplit('/').next()?.strip_prefix("com.apple.WebKit.")?;
            Some((pid.parse().ok()?, kind.to_string()))
        })
        .collect()
}

pub fn open(cx: &mut App) {
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    cx.spawn(async move |cx| {
        sleep(cx, 1500).await;
        // FILO_CHROMIUM_CHECK: the steps to take (memory, google, cookies, window), comma-separated; all by default.
        let steps = std::env::var("FILO_CHROMIUM_CHECK").unwrap_or_default();
        let step = |name: &str| steps.is_empty() || steps.split(',').any(|step| step.trim() == name);
        if step("memory") {
            memory(cx).await;
        }
        if step("google") {
            google(cx).await;
        }
        if let (true, Ok(path)) = (step("cookies"), std::env::var("FILO_CHROMIUM_COOKIES")) {
            cookies(cx, &path).await;
        }
        if step("window") {
            window(cx, shots.as_deref()).await;
        }
        say("done");
        crate::window_shot::flush(cx.background_executor()).await;
        for tab in super::tabs() {
            super::close(&tab.id);
        }
        super::remove_test_store(cx).await;
        cx.update(|_| super::quit()).ok();
        sleep(cx, 1500).await;
        std::process::exit(0);
    })
    .detach();
}

/// Opens `MEMORY_SITES` one at a time in one engine, measuring after each; then closes them.
async fn memory_of(cx: &mut AsyncApp, chromium: bool) {
    let engine = if chromium { "Chromium" } else { "WebKit" };
    super::engine::force(Some(if chromium { super::engine::Kind::Chromium } else { super::engine::Kind::WebKit }));
    let before: Vec<u32> = webkit_processes().into_iter().map(|(pid, _)| pid).collect();
    let mut opened_tabs = Vec::new();
    let mut last = 0.;
    for (index, site) in MEMORY_SITES.iter().enumerate() {
        let answer = text(&ask(cx, json!({ "op": "open", "url": site, "agent": "Check" })).await);
        let tab = opened(&answer);
        if tab.is_empty() {
            say(format!("{engine}: {site} didn't open: {}", answer.lines().next().unwrap_or_default()));
            continue;
        }
        opened_tabs.push(tab.clone());
        // A moment for the page to settle its scripts and images.
        sleep(cx, 6000).await;
        let processes: Vec<(u32, String)> = if chromium {
            cx.update(|_| super::chromium::processes()).unwrap_or_default()
        } else {
            webkit_processes().into_iter().filter(|(pid, _)| !before.contains(pid)).collect()
        };
        let pids: Vec<u32> = processes.iter().map(|(pid, _)| *pid).collect();
        let (each, total) = footprints(&pids);
        let kinds: Vec<String> = processes
            .iter()
            .map(|(pid, kind)| {
                let mb = each.iter().find(|(found, _)| found == pid).map_or(0., |(_, mb)| *mb);
                format!("{kind} {mb:.0}")
            })
            .collect();
        say(format!(
            "memory {engine} {} tab(s), after {site}: {total:.0} MB in {} processes (+{:.0} MB) [{}]",
            index + 1,
            pids.len(),
            total - last,
            kinds.join(", ")
        ));
        last = total;
    }
    if !chromium {
        say(
            "memory WebKit: the WebKit processes counted are the ones this run started (its own web content, GPU and networking processes)",
        );
    }
    for tab in opened_tabs {
        ask(cx, json!({ "op": "close", "tab": tab })).await.ok();
    }
    super::engine::force(None);
    sleep(cx, 2000).await;
}

async fn memory(cx: &mut AsyncApp) {
    memory_of(cx, true).await;
    memory_of(cx, false).await;
}

/// Google's sign-in in each engine, as far as typing an address and Next.
async fn google(cx: &mut AsyncApp) {
    let email = std::env::var("FILO_CHECK_GOOGLE_EMAIL").unwrap_or_else(|_| "filo.engine.check.2026@gmail.com".into());
    let email_field = Regex::new(r#"\[(e\d+)\] textbox "[^"]*(?:[Ee]mail|phone)"#).expect("pattern");
    for chromium in [true, false] {
        let engine = if chromium { "Chromium" } else { "WebKit" };
        super::engine::force(Some(if chromium { super::engine::Kind::Chromium } else { super::engine::Kind::WebKit }));
        let answer = text(
            &ask(
                cx,
                json!({ "op": "open", "url": "https://accounts.google.com/ServiceLogin?hl=en", "agent": "Check" }),
            )
            .await,
        );
        super::engine::force(None);
        let tab = opened(&answer);
        let field = email_field.captures(&answer).map(|found| found[1].to_string());
        let Some(field) = field else {
            say(format!("google {engine}: no email field: {}", answer.lines().take(3).collect::<Vec<_>>().join(" / ")));
            if !tab.is_empty() {
                ask(cx, json!({ "op": "close", "tab": tab })).await.ok();
            }
            continue;
        };
        let typed =
            text(&ask(cx, json!({ "op": "type", "tab": tab, "ref": field, "text": email, "agent": "Check" })).await);
        // The address exactly, before Next: set through the page when the keys didn't make it so.
        sleep(cx, 1000).await;
        let field_now = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "Email or phone" })).await);
        if !field_now.contains(&format!("value=\"{email}\"")) {
            page_value(cx, &tab, "fill", json!({ "ref": field, "text": email, "clear": true })).await;
            sleep(cx, 500).await;
            say(format!("google {engine}: the typed address wasn't exact, set it through the page"));
        }
        ask(cx, json!({ "op": "keys", "tab": tab, "keys": ["Enter"], "agent": "Check" })).await.ok();
        sleep(cx, 6000).await;
        let page = text(&ask(cx, json!({ "op": "read", "tab": tab, "agent": "Check" })).await);
        let verdict = if page.contains("may not be secure") {
            "BLOCKED: \"This browser or app may not be secure\""
        } else if page.contains("find your Google Account") || page.contains("find this account") {
            "went on: \"Couldn't find your Google Account\" (the address isn't an account; no browser block)"
        } else if page.to_lowercase().contains("password") {
            "went on to the password step"
        } else {
            "something else"
        };
        say(format!(
            "google {engine}: {verdict} — {} || typed: {}",
            page.lines().next().unwrap_or_default(),
            typed.lines().next().unwrap_or_default()
        ));
        // What the page says, without the address typed.
        let lines: Vec<String> = page
            .lines()
            .filter(|line| !line.trim().is_empty())
            .skip(1)
            .take(12)
            .map(|line| line.replace(&email, "<address>"))
            .collect();
        say(format!("google {engine} page: {}", lines.join(" / ")));
        ask(cx, json!({ "op": "close", "tab": tab })).await.ok();
    }
}

/// WebKit's cookies (a copy of its file) into Chromium, and which sites are signed in after.
async fn cookies(cx: &mut AsyncApp, path: &str) {
    let parsed =
        std::fs::read(path).map_err(|error| error.to_string()).and_then(|data| super::chromium::webkit_cookies(&data));
    let cookies = match parsed {
        Ok(cookies) => cookies,
        Err(error) => return say(format!("cookies: couldn't read {path}: {error}")),
    };
    super::engine::force(Some(super::engine::Kind::Chromium));
    let answer = text(&ask(cx, json!({ "op": "open", "url": "about:blank", "agent": "Check" })).await);
    let tab = opened(&answer);
    let taken = super::chromium::set_cookies(cookies.clone(), false).await.map_or(0, |(set, _)| set);
    let lasting = cookies.iter().filter(|cookie| cookie.get("expires").is_some()).count();
    say(format!("cookies: {} in the copy ({lasting} with an expiry), Chromium took {taken}", cookies.len()));
    let sites = std::env::var("FILO_CHROMIUM_SITES").unwrap_or_default();
    let sign_in = Regex::new(
        r#"(?i)\] (?:link|button) "(?:sign in|log in|login|sign up|sign in to [^"]*|get started|create account)""#,
    )
    .expect("pattern");
    for site in sites.split(',').map(str::trim).filter(|site| !site.is_empty()) {
        let page = text(&ask(cx, json!({ "op": "navigate", "tab": tab, "url": site, "agent": "Check" })).await);
        sleep(cx, 4000).await;
        let read = text(&ask(cx, json!({ "op": "read", "tab": tab, "agent": "Check" })).await);
        let address = cx.update(|_| super::tab(&tab).map(|tab| tab.state().1)).ok().flatten().unwrap_or_default();
        let to_login = [
            "login",
            "signin",
            "sign_in",
            "ServiceLogin",
            "accounts.google.com/v3/signin",
            "/i/flow/login",
            "authwall",
            "uas/login",
        ]
        .iter()
        .any(|word| address.contains(word));
        let offers = sign_in.find_iter(&read).count();
        let verdict = if to_login {
            "signed out (sent to a sign-in page)"
        } else if offers > 0 {
            "probably signed out (offers to sign in)"
        } else {
            "signed in (no sign-in offered)"
        };
        let host = address.split('/').take(3).collect::<Vec<_>>().join("/");
        say(format!(
            "cookies: {site} → {host}…: {verdict}; sign-in links {offers}; {}",
            page.lines().next().unwrap_or_default().split(" — ").next().unwrap_or_default()
        ));
    }
    ask(cx, json!({ "op": "close", "tab": tab })).await.ok();
    super::engine::force(None);
}

/// A Chromium tab in the browser window, clicked and typed in as the user, and its picture.
async fn window(cx: &mut AsyncApp, shots: Option<&str>) {
    super::engine::force(Some(super::engine::Kind::Chromium));
    let answer = text(
        &ask(cx, json!({ "op": "open", "url": "https://en.wikipedia.org/wiki/Special:Search", "agent": "Check" }))
            .await,
    );
    super::engine::force(None);
    let tab = opened(&answer);
    if tab.is_empty() {
        return say(format!("window: no tab: {}", answer.lines().next().unwrap_or_default()));
    }
    cx.update(|cx| super::window::open(Some(tab.clone()), cx)).ok();
    sleep(cx, 2500).await;
    // The search field's middle, from the page.
    let spot = page_value(cx, &tab, "box_of", json!({ "selector": "input[name=search]" })).await;
    let (x, y) = (spot["x"].as_f64().unwrap_or(-1.), spot["y"].as_f64().unwrap_or(-1.));
    let size =
        cx.update(|_| super::tab(&tab).map(|tab| native::viewport(tab.view()))).ok().flatten().unwrap_or_default();
    say(format!("window: the page shows at {}×{} points; search field at ({x:.0}, {y:.0})", size.0, size.1));
    if x >= 0. {
        if let Some(open) = super::tab(&tab) {
            cx.update(|_| native::as_user(open.view(), Some((x, y)), &[])).ok();
            sleep(cx, 300).await;
            for key in "filo browser".chars().map(Key::typing) {
                cx.update(|_| native::as_user(open.view(), None, std::slice::from_ref(&key))).ok();
                sleep(cx, 40).await;
            }
            sleep(cx, 400).await;
        }
        let read = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "filo browser" })).await);
        say(format!(
            "window: the user's click and keys {} ({})",
            if read.contains("value=\"filo browser\"") { "reached the page" } else { "did NOT reach the page" },
            read.lines().find(|line| line.contains("filo browser")).unwrap_or("no field with it")
        ));
    }
    if let Some(dir) = shots {
        crate::window_shot::save_largest(format!("{dir}/chromium-window.png")).await;
        say(format!("window: picture at {dir}/chromium-window.png"));
    }
}

/// One of page.js's ops, run straight in the tab's page.
async fn page_value(cx: &AsyncApp, tab: &str, op: &str, mut arg: Value) -> Value {
    let Some(open) = super::tab(tab) else { return Value::Null };
    arg["op"] = json!(op);
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.update(|_| native::run_js(open.view(), super::PAGE_JS, &arg.to_string(), move |result| drop(tx.send(result))))
        .ok();
    match rx.await {
        Ok(Ok(text)) => serde_json::from_str(&text).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

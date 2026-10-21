// `FILO_OPEN=browser-check` (`dev` builds, on a data folder of its own): Filo's browser checked end to end with no
// window of Filo's in front, against a small web site it serves itself on 127.0.0.1. Each step goes the way an
// agent's tool call does, through the socket (filo-local browser_mcp.rs), and one runs the real MCP server
// (`filo --local-mcp`) over stdin and stdout. It checks that a page in the background keeps running (frames, timers,
// visible, focused), that reading, clicking and typing by ref work, that a screenshot comes back and real clicks and
// keys at a spot reach the page (inside a frame from another site too), all while Filo isn't the active app, and asks
// the user to step in. `FILO_BROWSER_CHECK=again` is the second launch on the same data folder: the sign-in cookie the
// first one got must still be there, and it removes the run's WebKit data store at its end. Each check prints a line;
// the process ends with 1 when any failed.
// `FILO_FIXTURE_SHOTS=<dir>`: the agent's screenshot of the test page and a picture of the browser window.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use gpui::{App, AppContext as _, AsyncApp};
use regex::Regex;
use serde_json::{Value, json};

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| {
        matches!(
            open.as_str(),
            "browser-check"
                | "browser-import-check"
                | "browser-keys-check"
                | "browser-drag-check"
                | "browser-tools-check"
                | "chromium-check"
                | "browser-engine-check"
                | "browser-input-check"
        )
    })
}

/// The test site: `/form` (fields, a button, a key and click log, a frame counter), `/frame` (a frame from another
/// host, localhost, with a button), `/inner` (that frame), `/login` (sets a lasting cookie and a session one) and
/// `/whoami` (the cookies it was sent), `/login-again` (another lasting cookie), `/input` (browser/input_check.rs), and
/// `/signin` (a login form that says what was filled into it, for
/// browser/import_check.rs), `/chat` (a form, a chat box and a button of the page's own that say what Return did, for
/// browser/keys_check.rs), `/drag` (lists to reorder by dragging and a slider, for browser/drag_check.rs), and `/tools`
/// (file fields, a download, menus, hover, dialogs, a pop-up, `/popup`, that closes itself, for browser/tools_check.rs),
/// with `/report.csv` its download.
pub(super) fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test site");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    return;
                }
                let mut cookie = String::new();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = header.strip_prefix("Cookie:").or_else(|| header.strip_prefix("cookie:")) {
                        cookie = value.trim().to_string();
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut headers = String::new();
                let body = match path.as_str() {
                    "/form" => FORM.replace("{port}", &port.to_string()),
                    "/frame" => format!("<!doctype html><title>Frame page</title><body style='margin:0'><h1>Frame page</h1><p id=got>frame: nothing yet</p><iframe src='http://localhost:{port}/inner' title='Check' style='position:absolute;left:100px;top:200px;width:300px;height:150px;border:0'></iframe><script>addEventListener('message', (e) => document.getElementById('got').textContent = 'frame: ' + e.data);</script>"),
                    "/inner" => "<!doctype html><body style='margin:0;background:#eef'><button id=b style='width:300px;height:150px'>I'm not a robot</button><script>b.addEventListener('click', (e) => parent.postMessage('clicked trusted=' + e.isTrusted, '*'));</script>".into(),
                    "/login" => {
                        headers.push_str("Set-Cookie: filo_lasting=yes; Max-Age=2592000; Path=/\r\nSet-Cookie: filo_session=yes; Path=/\r\n");
                        "<!doctype html><title>Signed in</title><p>Signed in</p>".into()
                    }
                    // A second sign-in, in the other engine (browser/engine_check.rs).
                    "/login-again" => {
                        headers.push_str("Set-Cookie: filo_again=yes; Max-Age=2592000; Path=/; HttpOnly; SameSite=Lax\r\n");
                        "<!doctype html><title>Signed in again</title><p>Signed in again</p>".into()
                    }
                    "/signin" => SIGNIN.into(),
                    "/chat" => CHAT.into(),
                    "/drag" => super::drag_check::PAGE.into(),
                    "/tools" => super::tools_check::PAGE.into(),
                    "/popup" => super::tools_check::POPUP.into(),
                    "/report.csv" => REPORT_CSV.into(),
                    "/input" => super::input_check::PAGE.into(),
                    "/whoami" => format!("<!doctype html><title>Who am I</title><p id=cookies>cookies: [{cookie}]</p>"),
                    _ => "<!doctype html><title>Nothing</title>".into(),
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{headers}\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let mut rest = Vec::new();
                let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                let _ = stream.read_to_end(&mut rest);
            });
        }
    });
    port
}

/// What `/report.csv` downloads.
pub(super) const REPORT_CSV: &str = "name,total\nApples,3\nPears,5\n";

const SIGNIN: &str = r#"<!doctype html><title>Check sign-in</title>
<style>body{font:16px -apple-system;margin:24px} input{font:16px -apple-system;padding:6px;width:320px;display:block;margin:8px 0}</style>
<h1>Sign in</h1>
<form onsubmit="return false"><label>Email <input id=user type=email autocomplete=username></label>
<label>Password <input id=pass type=password autocomplete=current-password></label><button>Sign in</button></form>
<p id=filled>filled: nothing</p>
<script>
const show = () => filled.textContent = 'filled: user=' + user.value + ' pass=' + (pass.value === 'check-pass-123' ? 'right' : pass.value ? 'wrong' : 'empty');
user.addEventListener('input', show);
pass.addEventListener('input', show);
</script>"#;

const CHAT: &str = r#"<!doctype html><title>Check chat</title>
<style>body{font:16px -apple-system;margin:0} input,textarea{font:16px -apple-system;padding:6px;position:absolute;left:40px;width:320px;box-sizing:border-box}
#field{top:80px;height:36px} #chat{top:180px;height:72px} #next{position:absolute;top:290px;left:40px;padding:8px 18px;background:#1a73e8;color:#fff;border-radius:4px} #said{position:absolute;top:340px;left:40px}</style>
<form id=form action="/chat" method=get><input id=field name=q placeholder="Search"></form>
<textarea id=chat placeholder="Message"></textarea>
<div id=next role=button tabindex=0>Next</div>
<p id=said></p>
<script>
const loads = Number(sessionStorage.loads || 0) + 1; sessionStorage.loads = loads;
const said = [];
const show = () => document.getElementById('said').textContent = `loads ${loads} said [${said.join(', ')}]`;
form.addEventListener('submit', (e) => { e.preventDefault(); said.push('submitted ' + field.value); field.value = ''; show(); });
chat.addEventListener('keydown', (e) => {
  if (e.key !== 'Enter' || e.shiftKey) return;
  e.preventDefault(); said.push('sent ' + chat.value + ' trusted=' + e.isTrusted); chat.value = ''; show();
});
// As Google's sign-in does: a button of its own that goes on at Return from its script, leaving the key unused.
next.addEventListener('keydown', (e) => { if (e.key === 'Enter') setTimeout(() => { said.push('next'); show(); }, 300); });
show();
</script>"#;

const FORM: &str = r#"<!doctype html><title>Check form</title>
<style>body{font:16px -apple-system;margin:24px} input,textarea{font:16px -apple-system;padding:6px;width:320px;display:block;margin:8px 0}
button{font:16px -apple-system;padding:8px 18px} #other{position:absolute;left:600px;top:120px}</style>
<h1>Check form</h1>
<label>Your name <input id=name></label>
<button id=save>Save</button>
<p id=saved>saved 0</p>
<input id=other placeholder="Other field">
<p id=keys>keys: none</p>
<p id=ticks>frames 0 timers 0</p>
<p id=geom></p>
<script>
let saved = 0, frames = 0, timers = 0, keys = [];
document.getElementById('save').addEventListener('click', (e) => { saved++; document.getElementById('saved').textContent = `saved ${saved} trusted=${e.isTrusted}`; });
addEventListener('keydown', (e) => { keys.push(`${e.key}:${e.isTrusted}`); document.getElementById('keys').textContent = 'keys: ' + keys.slice(-6).join(' ') + ` (${keys.length})`; });
document.getElementById('other').addEventListener('keydown', (e) => { if (e.key === 'Enter') document.getElementById('other').dataset.enter = 'yes'; });
const step = () => { frames++; requestAnimationFrame(step); }; requestAnimationFrame(step);
setInterval(() => timers++, 10);
setInterval(() => {
  const r = document.getElementById('other').getBoundingClientRect();
  const seconds = performance.now() / 1000;
  document.getElementById('ticks').textContent = `frames ${frames} timers ${timers} fps ${Math.round(frames / seconds)} tps ${Math.round(timers / seconds)} visibility ${document.visibilityState} focus ${document.hasFocus()} enter ${document.getElementById('other').dataset.enter || 'no'}`;
  document.getElementById('geom').textContent = `other at ${Math.round(r.left + r.width / 2)},${Math.round(r.top + r.height / 2)}`;
}, 100);
</script>"#;

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        let (active, _) = super::frontmost();
        println!(
            "browser check: {} {name}{}: {}",
            if ok { "ok" } else { "FAILED" },
            if active { " [Filo active]" } else { "" },
            detail.as_ref().replace('\n', " | ")
        );
        if !ok {
            self.failed += 1;
        }
    }
}

/// A tool call through the socket, as an agent's MCP server makes it.
pub(super) async fn ask(cx: &AsyncApp, request: Value) -> Result<Value, String> {
    let socket = filo_local::browser_mcp::socket_path(&crate::config::config_dir().join("local"));
    cx.background_spawn(async move { filo_local::browser_mcp::ask(&socket, &request, Duration::from_secs(90)) }).await
}

pub(super) fn text(reply: &Result<Value, String>) -> String {
    match reply {
        Ok(value) if value["ok"] == true => value["text"].as_str().unwrap_or_default().to_string(),
        Ok(value) => format!("error: {}", value["error"].as_str().unwrap_or_default()),
        Err(error) => format!("error: {error}"),
    }
}

/// The ref of the line in `page` that `pattern` matches (a regex with the ref as its first group).
fn find_ref(page: &str, pattern: &str) -> Option<String> {
    Regex::new(pattern).ok()?.captures(page).map(|found| found[1].to_string())
}

fn number_after(page: &str, label: &str) -> Option<f64> {
    let found = Regex::new(&format!(r"{label} (\d+)")).ok()?.captures(page)?;
    found[1].parse().ok()
}

pub fn open(cx: &mut App) {
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "browser-import-check") {
        return super::import_check::open(cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "browser-keys-check") {
        return super::keys_check::open(cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "browser-drag-check") {
        return super::drag_check::open(cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "browser-tools-check") {
        return super::tools_check::open(cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "chromium-check") {
        return super::chromium_check::open(cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "browser-engine-check") {
        return super::engine_check::open(cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "browser-input-check") {
        return super::input_check::open(cx);
    }
    let again = std::env::var("FILO_BROWSER_CHECK").is_ok_and(|value| value == "again");
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    let port = serve();
    let site = format!("http://127.0.0.1:{port}");
    cx.spawn(async move |cx| {
        let mut checks = Checks { failed: 0 };
        let sleep = |cx: &AsyncApp, ms: u64| cx.background_executor().timer(Duration::from_millis(ms));
        // Filo is running long before an agent opens a page. A process just launched from a terminal can be brought
        // forward by macOS when its first window shows: one window first, and Filo sent back if it was.
        let warm = cx.update(|_| super::native::Park::new(200., 200.)).ok().flatten();
        sleep(cx, 1500).await;
        let (active, _) = cx.update(|_| super::frontmost()).unwrap_or_default();
        if active {
            println!("browser check: note: macOS brought the just-launched check forward with its first window; sending it back");
            cx.update(|_| super::native::deactivate()).ok();
            sleep(cx, 1000).await;
        }
        drop(warm);
        let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
        checks.check("Filo isn't the active app", !active, format!("active: {active}, in front: {front}"));
        if again {
            let reply = ask(cx, json!({ "op": "open", "url": format!("{site}/whoami"), "agent": "Check" })).await;
            let page = text(&reply);
            checks.check("the sign-in lasted (a cookie with an expiry)", page.contains("filo_lasting=yes"), &page);
            println!("browser check: note: a session cookie (no expiry) {}", if page.contains("filo_session=yes") { "lasted too" } else { "didn't last, as in Safari" });
        } else {
            first_launch(cx, &mut checks, &site, shots.as_deref()).await;
        }
        let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
        println!("browser check: at the end Filo active: {active}, in front: {front}");
        println!("browser-check: {}", if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) });
        crate::window_shot::flush(cx.background_executor()).await;
        // The second launch is done with the store; the first leaves it for the second.
        if again {
            super::remove_test_store(cx).await;
        }
        // A moment for WebKit to write its cookies out, then gone at once, harder on them than quitting.
        cx.update(|_| super::quit()).ok();
        sleep(cx, 3000).await;
        std::process::exit(if checks.failed == 0 { 0 } else { 1 });
    })
    .detach();
}

async fn first_launch(cx: &mut AsyncApp, checks: &mut Checks, site: &str, shots: Option<&str>) {
    let sleep = |cx: &AsyncApp, ms: u64| cx.background_executor().timer(Duration::from_millis(ms));
    let reply =
        ask(cx, json!({ "op": "open", "url": format!("{site}/form"), "agent": "Check", "chat": "check-chat" })).await;
    let page = text(&reply);
    let tab =
        Regex::new(r"Opened tab (t\d+)").unwrap().captures(&page).map(|found| found[1].to_string()).unwrap_or_default();
    checks.check(
        "open a page and read it with refs",
        !tab.is_empty() && page.contains("Check form") && page.contains("button \"Save\""),
        &page,
    );
    let name = find_ref(&page, r#"\[(e\d+)\] textbox "Your name""#).unwrap_or_default();
    let save = find_ref(&page, r#"\[(e\d+)\] button "Save""#).unwrap_or_default();

    // The page runs at full speed in the background.
    if let Some(open) = super::tab(&tab) {
        println!("browser check: note: {}", cx.update(|_| super::native::diagnose(open.view())).unwrap_or_default());
    }
    sleep(cx, 1000).await;
    let before = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "frames" })).await);
    sleep(cx, 3000).await;
    let page = text(&ask(cx, json!({ "op": "read", "tab": tab })).await);
    // A hidden page gets about one frame and one timer a second; one on screen 60 frames and up to 100 timers at 10 ms.
    let rate =
        |label: &str| (number_after(&page, label).unwrap_or(0.) - number_after(&before, label).unwrap_or(0.)) / 3.;
    let (fps, tps) = (rate("frames"), rate("timers"));
    checks.check("frames run in the background (20 a second or more)", fps >= 20., format!("{fps:.0} a second"));
    checks.check("timers aren't throttled (20 a second or more at 10 ms)", tps >= 20., format!("{tps:.0} a second"));
    let background = (fps, tps);
    checks.check(
        "the page counts as visible and focused",
        page.contains("visibility visible") && page.contains("focus true"),
        page.lines().find(|line| line.contains("frames")).unwrap_or(""),
    );

    // Typing by ref, with real keys, while another app is in front.
    let typed = text(&ask(cx, json!({ "op": "type", "tab": tab, "ref": name, "text": "Hello Filo" })).await);
    checks.check(
        "type by ref with real key presses",
        typed.contains("with real key presses") && typed.contains("value=\"Hello Filo\""),
        &typed,
    );
    let page = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "keys:" })).await);
    checks.check("the page got trusted keydowns", page.contains("o:true") && !page.contains(":false"), &page);

    let clicked = text(&ask(cx, json!({ "op": "click", "tab": tab, "ref": save })).await);
    checks.check("click by ref with real mouse events", clicked.contains("with real mouse events"), &clicked);
    let page = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "saved" })).await);
    checks.check("the page got a trusted click", page.contains("saved 1 trusted=true"), &page);

    // A screenshot, then a real click and keys at a spot in it.
    let shot = ask(cx, json!({ "op": "screenshot", "tab": tab })).await;
    let png = shot
        .as_ref()
        .ok()
        .and_then(|value| value["png"].as_str())
        .and_then(|png| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).ok());
    let size = png
        .as_ref()
        .and_then(|png| image::load_from_memory(png).ok())
        .map(|picture| (picture.width(), picture.height()));
    checks.check(
        "a screenshot comes back at the page's size",
        size == Some((1280, 800)),
        format!("{size:?}; {}", text(&shot)),
    );
    if let (Some(dir), Some(png)) = (shots, png.as_ref()) {
        let _ = std::fs::write(format!("{dir}/agent-screenshot.png"), png);
    }
    let page = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "other at" })).await);
    let spot = Regex::new(r"other at (\d+),(\d+)")
        .unwrap()
        .captures(&page)
        .map(|found| (found[1].parse::<f64>().unwrap_or(0.), found[2].parse::<f64>().unwrap_or(0.)));
    let (x, y) = spot.unwrap_or_default();
    let clicked = text(&ask(cx, json!({ "op": "click_at", "tab": tab, "x": x, "y": y })).await);
    checks.check(
        "a real click at a spot focuses the field there",
        clicked.contains("Focus: [e") && clicked.contains("Other field"),
        &clicked,
    );
    let keyed = text(&ask(cx, json!({ "op": "keys", "tab": tab, "text": "typed blind", "keys": ["Enter"] })).await);
    let page = text(&ask(cx, json!({ "op": "read", "tab": tab })).await);
    checks.check(
        "real keys at the spot reach the field while Filo isn't active",
        page.contains("value=\"typed blind\"") && page.contains("enter yes"),
        format!(
            "{keyed} || {}",
            page.lines()
                .filter(|line| line.contains("Other") || line.contains("enter"))
                .collect::<Vec<_>>()
                .join(" / ")
        ),
    );
    let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
    checks.check("Filo still isn't the active app after the keys", !active, format!("in front: {front}"));

    // A frame from another site (localhost, not 127.0.0.1): not readable, clicked at its spot for real.
    let page = text(&ask(cx, json!({ "op": "navigate", "tab": tab, "url": format!("{site}/frame") })).await);
    sleep(cx, 500).await;
    let page = format!("{page}\n{}", text(&ask(cx, json!({ "op": "read", "tab": tab })).await));
    let frame = Regex::new(r"\[frame from localhost:\d+ .*at x=(\d+) y=(\d+) (\d+)×(\d+)")
        .unwrap()
        .captures(&page)
        .map(|found| {
            let n = |index: usize| found[index].parse::<f64>().unwrap_or(0.);
            (n(1) + n(3) / 2., n(2) + n(4) / 2.)
        });
    checks.check(
        "a frame from another site is listed with its spot",
        frame.is_some(),
        page.lines().find(|line| line.contains("[frame")).unwrap_or(&page),
    );
    if let Some((x, y)) = frame {
        let clicked = text(&ask(cx, json!({ "op": "click_at", "tab": tab, "x": x, "y": y })).await);
        sleep(cx, 300).await;
        let page = text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "frame:" })).await);
        checks.check(
            "a real click inside the other site's frame is trusted",
            page.contains("clicked trusted=true"),
            format!("{clicked} || {page}"),
        );
    }

    // Signing in: the cookie must still be there on the next launch (FILO_BROWSER_CHECK=again).
    let page = text(&ask(cx, json!({ "op": "navigate", "tab": tab, "url": format!("{site}/login") })).await);
    checks.check("sign in (cookies set)", page.contains("Signed in"), &page);

    // The real MCP server, as a coding tool starts it, over stdin and stdout.
    let mcp = mcp_round_trip(cx, site).await;
    checks.check(
        "the MCP server's browser tools reach the app",
        mcp.as_ref().is_ok_and(|text| text.contains("Opened tab") && text.contains("Check form")),
        mcp.unwrap_or_else(|error| error),
    );

    // Asking the user: answered with Done in the browser window after a moment.
    let asking = cx.background_spawn({
        let socket = filo_local::browser_mcp::socket_path(&crate::config::config_dir().join("local"));
        let tab = tab.clone();
        async move {
            filo_local::browser_mcp::ask(
                &socket,
                &json!({ "op": "ask_user", "tab": tab, "agent": "Check", "reason": "Solve the check, please." }),
                Duration::from_secs(60),
            )
        }
    });
    sleep(cx, 800).await;
    let shown = cx.update(|cx| super::window::open(Some(tab.clone()), cx));
    sleep(cx, 2500).await;
    if let (Some(dir), Ok(())) = (shots, shown) {
        crate::window_shot::save_largest(format!("{dir}/browser-window.png")).await;
    }
    // The same page on screen, in the browser window: the background one must run about as fast (macOS's Low Power
    // Mode caps both at 30 frames a second).
    if let Some(other) = super::tabs().iter().find(|other| other.id != tab).map(|other| other.id.clone()) {
        cx.update(|cx| super::window::open(Some(other.clone()), cx)).ok();
        sleep(cx, 1500).await;
        let before = text(&ask(cx, json!({ "op": "read", "tab": other, "find": "frames" })).await);
        sleep(cx, 3000).await;
        let after = text(&ask(cx, json!({ "op": "read", "tab": other, "find": "frames" })).await);
        let rate =
            |label: &str| (number_after(&after, label).unwrap_or(0.) - number_after(&before, label).unwrap_or(0.)) / 3.;
        let (fps, tps) = (rate("frames"), rate("timers"));
        let low_power = std::process::Command::new("/usr/bin/pmset").arg("-g").output().is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.contains("lowpowermode") && line.trim_end().ends_with('1'))
        });
        checks.check(
            "in the background a page runs as fast as on screen (within 20%)",
            background.0 >= fps * 0.8 && background.1 >= tps * 0.8,
            format!(
                "background {:.0} frames and {:.0} timers a second, on screen {fps:.0} and {tps:.0}; Low Power Mode {}",
                background.0,
                background.1,
                if low_power { "on" } else { "off" }
            ),
        );
        cx.update(|cx| super::window::open(Some(tab.clone()), cx)).ok();
    }
    let bar = super::tab(&tab)
        .and_then(|tab| tab.help())
        .is_some_and(|help| help.answer.is_none() && help.reason.contains("Solve"));
    checks.check("the browser window shows the agent's ask", bar, "");
    if let Some(tab) = super::tab(&tab) {
        tab.answer_help("done");
    }
    let answer = text(&asking.await);
    checks.check("browser_ask_user hears Done", answer.contains("they're done"), &answer);
}

/// `filo --local-mcp` with a made-up chat: initialize, then browser_open, as Claude Code would.
async fn mcp_round_trip(cx: &AsyncApp, site: &str) -> Result<String, String> {
    let root = crate::config::config_dir().join("local");
    let url = format!("{site}/form");
    cx.background_spawn(async move {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut child = std::process::Command::new(exe)
            .args(["--local-mcp", "--chat", "check-chat", "--turn", "check-turn", "--dir"])
            .arg(&root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let requests = [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "browser_open", "arguments": { "url": url } } }),
        ];
        for request in requests {
            writeln!(stdin, "{request}").map_err(|error| error.to_string())?;
        }
        drop(stdin);
        let output = child.wait_with_output().map_err(|error| error.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reply: Value = stdout.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).find(|reply| reply["id"] == 2).ok_or_else(|| format!("no reply: {stdout}"))?;
        Ok(reply["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string())
    })
    .await
}

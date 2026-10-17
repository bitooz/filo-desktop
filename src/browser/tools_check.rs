// `FILO_OPEN=browser-tools-check` (`dev` builds, on a data folder of its own): the browser tools that make real sites
// work, end to end through the socket as an agent's calls go, in a tab in the background, against the test site's
// `/tools` (browser/check.rs): uploading to a file field and through a drop area's hidden one, a download, menus (a
// <select>, a list that takes several, a combo box the page draws), hover, alert / confirm / prompt, a pop-up that
// closes itself, waiting for text, a full-page screenshot, and the user's Pause and Take over (the agent's calls wait,
// then say so), with the agent's steps in its trail. Each check prints a line; the process ends with 1 when any failed.
// `FILO_BROWSER_REAL=1` then works the same tools on real pages (the-internet.herokuapp.com, a long Wikipedia page).
// `FILO_FIXTURE_SHOTS=<dir>`: the browser window with the agent's bar and trail, paused, and the full-page shots (not
// the dialog bar: a page with a dialog up draws nothing, and the window's picture waits for the page's).

use std::time::Duration;

use gpui::{App, AsyncApp};
use regex::Regex;
use serde_json::{Value, json};

use super::Control;
use super::check::{ask, text};

pub const PAGE: &str = r##"<!doctype html><title>Check tools</title>
<style>body{font:15px -apple-system;margin:24px} section{margin:0 0 14px} .hov{display:inline-block;padding:6px 10px;background:#eee}
.hov .sub{display:none} .hov:hover .sub{display:block} #drop{display:inline-block;padding:14px;border:2px dashed #99a;cursor:pointer}
#list{list-style:none;padding:0;margin:4px 0;border:1px solid #ccc;width:160px} #list li{padding:4px 8px;cursor:pointer}</style>
<h1>Check tools</h1>
<section><label>Resume <input type=file id=file1></label>
<div id=drop role=button tabindex=0>Drop files here</div><input type=file id=hidden multiple style="display:none"></section>
<section><a id=dl href="/report.csv" download="report.csv">Download report</a></section>
<section><select id=fruit aria-label="Fruit"><option>Apple</option><option>Banana</option><option value=c>Cherry</option></select>
<select id=many multiple aria-label="Colors"><option>Red</option><option>Green</option><option>Blue</option></select>
<button id=combo role=combobox aria-expanded=false aria-label="Size">Pick a size</button><ul id=list role=listbox hidden><li role=option>Small</li><li role=option>Medium</li><li role=option>Large</li></ul></section>
<section><div class=hov id=account>Account<div class=sub><a href="#profile">Your profile</a> <a href="#out">Log out</a></div></div></section>
<section><button id=alertb>Say hi</button> <button id=confirmb>Delete everything</button> <button id=promptb>Rename</button> <button id=pop>Open pop-up</button> <button id=later>Load later</button> <button id=count>Count</button></section>
<p id=said>said:</p><div id=slot></div>
<div style="height:3200px;background:linear-gradient(#fff,#cde)"></div><p id=bottom style="font-size:40px">Bottom of the page</p>
<script>
const log = [];
const say = (what) => { log.push(what); said.textContent = 'said: ' + log.slice(-12).join(' | '); };
const names = (input) => [...input.files].map((f) => `${f.name}:${f.size}`).join(',');
file1.addEventListener('change', () => say('file1 ' + names(file1)));
hidden.addEventListener('change', () => say('hidden ' + names(hidden)));
drop.addEventListener('click', () => hidden.click());
fruit.addEventListener('change', () => say('fruit ' + fruit.value));
many.addEventListener('change', () => say('many ' + [...many.selectedOptions].map((o) => o.text).join(',')));
combo.addEventListener('click', () => { list.hidden = !list.hidden; combo.setAttribute('aria-expanded', String(!list.hidden)); });
list.addEventListener('click', (e) => { const li = e.target.closest('li'); if (!li) return; list.hidden = true; combo.textContent = li.textContent; say('size ' + li.textContent + ' ' + e.isTrusted); });
account.addEventListener('mouseenter', (e) => say('hover ' + e.isTrusted));
alertb.addEventListener('click', () => { alert('Hi there'); say('alert done'); });
confirmb.addEventListener('click', () => say('confirm ' + confirm('Delete this?')));
promptb.addEventListener('click', () => say('prompt ' + prompt('New name?', 'old name')));
pop.addEventListener('click', () => window.open('/popup', 'signin', 'width=420,height=320'));
addEventListener('message', (e) => say('popup ' + e.data));
later.addEventListener('click', () => { slot.textContent = 'Loading…'; setTimeout(() => { slot.textContent = 'Loaded at last'; }, 2000); });
let counted = 0, moved = false;
addEventListener('mousemove', (e) => { if (!moved) { moved = true; say('moved ' + e.isTrusted); } });
count.addEventListener('click', (e) => say('count ' + (++counted) + ' ' + e.isTrusted));
</script>"##;

pub const POPUP: &str = r#"<!doctype html><title>Pop-up sign-in</title><h1>Sign in to the check</h1>
<button id=ok>Sign in</button>
<script>ok.addEventListener('click', () => { if (window.opener) window.opener.postMessage('signed in', '*'); setTimeout(() => window.close(), 100); });</script>"#;

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        let (active, _) = super::frontmost();
        let detail = detail.as_ref().replace('\n', " | ");
        let detail: String = detail.chars().take(900).collect();
        println!(
            "browser tools check: {} {name}{}: {detail}",
            if ok { "ok" } else { "FAILED" },
            if active { " [Filo active]" } else { "" }
        );
        if !ok {
            self.failed += 1;
        }
    }
}

pub fn open(cx: &mut App) {
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    let real = std::env::var("FILO_BROWSER_REAL").is_ok_and(|value| value == "1");
    let port = super::check::serve();
    let site = format!("http://127.0.0.1:{port}");
    cx.spawn(async move |cx| {
        let mut checks = Checks { failed: 0 };
        // As in the browser check: a process just launched from a terminal can be brought forward with its first
        // window; one first, and Filo sent back if it was.
        let warm = cx.update(|_| super::native::Park::new(200., 200.)).ok().flatten();
        sleep(cx, 1500).await;
        if cx.update(|_| super::frontmost().0).unwrap_or(false) {
            println!("browser tools check: note: macOS brought the just-launched check forward with its first window; sending it back");
            cx.update(|_| super::native::deactivate()).ok();
            sleep(cx, 1000).await;
        }
        drop(warm);
        run(cx, &mut checks, &site, shots.as_deref()).await;
        if real {
            real_pages(cx, &mut checks, shots.as_deref()).await;
        }
        println!("browser-tools-check: {}", if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) });
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

/// A call as the agent "Check" makes it.
async fn call(cx: &AsyncApp, mut request: Value) -> String {
    request["agent"] = json!("Check");
    request["chat"] = json!("check-chat");
    text(&ask(cx, request).await)
}

async fn said(cx: &AsyncApp, tab: &str) -> String {
    call(cx, json!({ "op": "read", "tab": tab, "find": "said:" })).await
}

fn find(page: &str, pattern: &str) -> String {
    Regex::new(pattern)
        .ok()
        .and_then(|regex| regex.captures(page))
        .map(|found| found[1].to_string())
        .unwrap_or_default()
}

/// The PNG of a screenshot call, and its size.
async fn shot(cx: &AsyncApp, request: Value) -> (String, Option<image::RgbaImage>, Vec<u8>) {
    let mut request = request;
    request["agent"] = json!("Check");
    let reply = ask(cx, request).await;
    let png = reply
        .as_ref()
        .ok()
        .and_then(|value| value["png"].as_str())
        .and_then(|png| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).ok())
        .unwrap_or_default();
    let picture = image::load_from_memory(&png).ok().map(|picture| picture.to_rgba8());
    (text(&reply), picture, png)
}

async fn run(cx: &mut AsyncApp, checks: &mut Checks, site: &str, shots: Option<&str>) {
    let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
    checks.check("Filo isn't the active app", !active, format!("in front: {front}"));
    let page = call(cx, json!({ "op": "open", "url": format!("{site}/tools") })).await;
    let tab = find(&page, r"Opened tab (t\d+)");
    let reference = |pattern: &str| find(&page, pattern);
    let (file1, drop_area, link, fruit, many, combo, account) = (
        reference(r#"\[(e\d+)\] file "Resume""#),
        reference(r#"\[(e\d+)\] button "Drop files here""#),
        reference(r#"\[(e\d+)\] link "Download report""#),
        reference(r#"\[(e\d+)\] select "Fruit""#),
        reference(r#"\[(e\d+)\] select "Colors""#),
        reference(r#"\[(e\d+)\] combobox "Size""#),
        reference(r#"\[(e\d+)\] link "Your profile""#),
    );
    let button = |name: &str| find(&page, &format!(r#"\[(e\d+)\] button "{name}""#));
    checks.check(
        "the page's fields, menus and buttons have refs, a file field says it has no file",
        ![&tab, &file1, &drop_area, &link, &fruit, &many, &combo].iter().any(|found| found.is_empty())
            && page.contains("(no file chosen)")
            && account.is_empty(),
        &page,
    );

    // The agent is seen working, from its first call.
    let working = cx.update(|_| super::working_for("Check")).ok().flatten();
    checks.check(
        "the agent counts as working in its tab",
        working.as_ref().is_some_and(|(id, control)| *id == tab && *control == Control::Agent),
        format!("{working:?}"),
    );

    // Uploads: files of the check's own, never a panel.
    let dir = crate::config::config_dir().join("local/tmp/check-upload");
    let _ = std::fs::create_dir_all(&dir);
    let (one, two) = (dir.join("cv.txt"), dir.join("photo.txt"));
    let _ = std::fs::write(&one, "my cv");
    let _ = std::fs::write(&two, "a photo, kind of");
    let reply = call(cx, json!({ "op": "upload", "tab": tab, "ref": file1, "paths": [one.to_string_lossy()] })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "browser_upload fills a file field",
        reply.contains("Chose cv.txt") && reply.contains(r#"chosen="cv.txt""#) && now.contains("file1 cv.txt:5"),
        format!("{reply} || {now}"),
    );
    let reply = call(cx, json!({ "op": "upload", "tab": tab, "ref": drop_area, "paths": [one.to_string_lossy(), two.to_string_lossy()] })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "through a drop area, its hidden field gets both files",
        now.contains("hidden cv.txt:5,photo.txt:16"),
        format!("{reply} || {now}"),
    );
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": file1 })).await;
    checks.check(
        "a file chooser the agent opens with no files gets none, and it's told",
        reply.contains("asked for a file and got none"),
        &reply,
    );

    // A download: told where it went, and it's there.
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": link })).await;
    let waited = call(cx, json!({ "op": "wait", "tab": tab, "for": "download", "seconds": 10 })).await;
    let saved = crate::config::config_dir().join("local/downloads/report.csv");
    let both = format!("{reply} || {waited}");
    checks.check(
        "a download goes to Filo's downloads folder and the agent is told",
        waited.contains("The download ended")
            && both.contains("Downloaded report.csv")
            && both.contains(&saved.to_string_lossy().to_string())
            && std::fs::read_to_string(&saved).is_ok_and(|text| text == super::check::REPORT_CSV),
        &both,
    );

    // Menus.
    let reply = call(cx, json!({ "op": "select", "tab": tab, "ref": fruit, "option": "Cherry" })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "browser_select picks a <select>'s option",
        reply.contains(r#"selected="Cherry""#) && now.contains("fruit c"),
        format!("{reply} || {now}"),
    );
    let reply = call(cx, json!({ "op": "select", "tab": tab, "ref": many, "options": ["Red", "Blue"] })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "and several in a list that takes several",
        now.contains("many Red,Blue"),
        format!("{reply} || {now}"),
    );
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": fruit })).await;
    checks.check(
        "a <select> isn't clicked (its menu would hold the app)",
        reply.starts_with("error:") && reply.contains("browser_select"),
        &reply,
    );
    let reply = call(cx, json!({ "op": "select", "tab": tab, "ref": combo, "option": "Medium" })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "a combo box the page draws is opened and its option clicked, for real",
        now.contains("size Medium true"),
        format!("{reply} || {now}"),
    );

    let trail = |cx: &mut AsyncApp| {
        cx.update(|_| super::activity::trail("Check", 30))
            .unwrap_or_default()
            .into_iter()
            .map(|step| step.what)
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let early = trail(cx);
    checks.check(
        "the agent's trail has its steps in words",
        [
            "Opened 127.0.0.1",
            "Chose cv.txt for “Resume”",
            "Chose cv.txt and 1 more for “Drop files here”",
            "Picked “Cherry” in “Fruit”",
            "Clicked “Download report”",
        ]
        .iter()
        .all(|step| early.contains(step)),
        &early,
    );

    // Hover: a menu that shows on :hover, its mouseenter trusted.
    let box_y = page_y(cx, &tab, "account").await;
    let reply = call(cx, json!({ "op": "hover", "tab": tab, "x": 40, "y": box_y })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "browser_hover moves the pointer for real and says what showed",
        reply.contains("Your profile") && now.contains("hover true"),
        format!("{reply} || {now}"),
    );

    // Dialogs.
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": button("Delete everything") })).await;
    let shown = cx.update(|_| super::tab(&tab).and_then(|tab| tab.dialog())).ok().flatten();
    checks.check(
        "a confirm is held and the agent is told",
        reply.contains("confirm dialog: “Delete this?”") && shown.is_some(),
        &reply,
    );
    let blocked = call(cx, json!({ "op": "read", "tab": tab })).await;
    checks.check("while it's up, reading says so", blocked.contains("browser_dialog"), &blocked);
    let reply = call(cx, json!({ "op": "dialog", "tab": tab, "action": "dismiss" })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "dismissed, confirm() is false",
        reply.contains("Dismissed") && now.contains("confirm false"),
        format!("{reply} || {now}"),
    );
    call(cx, json!({ "op": "click", "tab": tab, "ref": button("Delete everything") })).await;
    call(cx, json!({ "op": "dialog", "tab": tab })).await;
    let now = said(cx, &tab).await;
    checks.check("accepted, confirm() is true", now.contains("confirm true"), &now);
    call(cx, json!({ "op": "click", "tab": tab, "ref": button("Rename") })).await;
    let reply = call(cx, json!({ "op": "dialog", "tab": tab, "action": "accept", "text": "New name" })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "a prompt gets the agent's text",
        reply.contains("prompt") && now.contains("prompt New name"),
        format!("{reply} || {now}"),
    );
    call(cx, json!({ "op": "click", "tab": tab, "ref": button("Say hi") })).await;
    call(cx, json!({ "op": "dialog", "tab": tab })).await;
    let now = said(cx, &tab).await;
    checks.check("an alert waits until it's answered", now.contains("alert done"), &now);

    // A pop-up: a tab of the agent's, which closes itself.
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": button("Open pop-up") })).await;
    let popup = find(&reply, r"opened a new tab, (t\d+)");
    let read = call(cx, json!({ "op": "read", "tab": popup })).await;
    checks.check(
        "a pop-up opens as a tab and the agent is told",
        !popup.is_empty() && read.contains("Sign in to the check"),
        format!("{reply} || {read}"),
    );
    let sign_in = find(&read, r#"\[(e\d+)\] button "Sign in""#);
    call(cx, json!({ "op": "click", "tab": popup, "ref": sign_in })).await;
    sleep(cx, 800).await;
    let tabs = call(cx, json!({ "op": "tabs" })).await;
    let now = said(cx, &tab).await;
    checks.check(
        "the pop-up closed itself, its tab gone, and its message reached the opener",
        !popup.is_empty()
            && !tabs.contains(&format!("{popup}:"))
            && now.contains("popup signed in")
            && now.contains("closed itself"),
        format!("{tabs} || {now}"),
    );

    // Waiting.
    call(cx, json!({ "op": "click", "tab": tab, "ref": button("Load later") })).await;
    let reply = call(cx, json!({ "op": "wait", "tab": tab, "text": "Loading…", "gone": true, "seconds": 8 })).await;
    let shows = call(cx, json!({ "op": "wait", "tab": tab, "text": "Loaded at last", "seconds": 5 })).await;
    checks.check(
        "browser_wait waits for text to go and to show",
        reply.contains("is gone after") && shows.contains("shows after"),
        format!("{reply} || {shows}"),
    );
    let none = call(cx, json!({ "op": "wait", "tab": tab, "text": "never there", "seconds": 1 })).await;
    checks.check("and says when it didn't show", none.starts_with("error:") && none.contains("didn't show"), &none);

    // A full-page screenshot.
    let (reply, picture, png) = shot(cx, json!({ "op": "screenshot", "tab": tab, "full": true })).await;
    let size = picture.as_ref().map(|picture| (picture.width(), picture.height())).unwrap_or_default();
    // The page's bottom is drawn too (the gradient to blue, the last line's dark text), not left blank.
    let bottom_drawn = picture.as_ref().is_some_and(|picture| {
        (picture.height().saturating_sub(120)..picture.height())
            .any(|y| (0..picture.width()).any(|x| picture.get_pixel(x, y).0[0] < 120))
    });
    checks.check(
        "a full-page screenshot has the whole page, its bottom drawn",
        size.1 > 3400 && size.0 >= 1200 && bottom_drawn && reply.contains("The whole page"),
        format!("{size:?}, bottom drawn {bottom_drawn}: {reply}"),
    );
    if let Some(dir) = shots {
        let _ = std::fs::write(format!("{dir}/tools-full-page.png"), &png);
    }

    // The user pauses the agent, then takes the tab over: its calls wait, then say so.
    let count = button("Count");
    cx.update(|_| super::tab(&tab).map(|tab| tab.set_control(Control::Paused))).ok();
    let shown = cx.update(|cx| super::window::open(Some(tab.clone()), cx));
    let pending = gpui::AppContext::background_spawn(cx, {
        let socket = filo_local::browser_mcp::socket_path(&crate::config::config_dir().join("local"));
        let request = json!({ "op": "click", "tab": tab, "ref": count, "agent": "Check", "chat": "check-chat" });
        async move { filo_local::browser_mcp::ask(&socket, &request, Duration::from_secs(120)) }
    });
    sleep(cx, 2000).await;
    let before = said(cx, &tab).await;
    if let (Some(dir), Ok(())) = (shots, shown) {
        crate::window_shot::save_largest(format!("{dir}/tools-paused.png")).await;
    }
    cx.update(|_| super::tab(&tab).map(|tab| tab.set_control(Control::Agent))).ok();
    let reply = text(&pending.await);
    let after = said(cx, &tab).await;
    checks.check(
        "paused, the agent's click waits, and goes on once resumed, told so",
        !before.contains("count 1") && after.contains("count 1 true") && reply.contains("paused you"),
        format!("{before} || {reply} || {after}"),
    );
    cx.update(|_| super::tab(&tab).map(|tab| tab.set_control(Control::User))).ok();
    let pending = gpui::AppContext::background_spawn(cx, {
        let socket = filo_local::browser_mcp::socket_path(&crate::config::config_dir().join("local"));
        let request = json!({ "op": "click", "tab": tab, "ref": count, "agent": "Check", "chat": "check-chat" });
        async move { filo_local::browser_mcp::ask(&socket, &request, Duration::from_secs(120)) }
    });
    sleep(cx, 1500).await;
    let held = cx.update(|_| super::working_for("Check")).ok().flatten();
    let before = said(cx, &tab).await;
    cx.update(|_| super::tab(&tab).map(|tab| tab.set_control(Control::Agent))).ok();
    let reply = text(&pending.await);
    let after = said(cx, &tab).await;
    checks.check(
        "taken over, it waits until handed back, then is told to read again",
        held.is_some_and(|(_, control)| control == Control::User)
            && !before.contains("count 2")
            && after.contains("count 2 true")
            && reply.contains("handed it back"),
        format!("{before} || {reply} || {after}"),
    );

    // The page on screen now (the browser window): the agent's clicks show as rings over it, and a dialog as a bar
    // the user can answer too.
    let rings = cx.update(|_| super::native::flashed()).unwrap_or(0);
    // (No picture of it: a page with a dialog up draws none, and the window's picture takes the page's.)
    call(cx, json!({ "op": "click", "tab": tab, "ref": button("Say hi") })).await;
    sleep(cx, 400).await;
    let shot_now = call(cx, json!({ "op": "screenshot", "tab": tab })).await;
    checks.check("a screenshot with a dialog up says so, not waiting", shot_now.contains("alert dialog"), &shot_now);
    let answered = cx.update(|_| super::tab(&tab).is_some_and(|tab| tab.answer_dialog(true, None))).unwrap_or(false);
    let now = said(cx, &tab).await;
    let drawn = cx.update(|_| super::native::flashed()).unwrap_or(0) - rings;
    checks.check(
        "on screen, the agent's click shows a ring, and the user's OK answers the page's alert",
        drawn >= 1 && answered && now.contains("alert done"),
        format!("rings {drawn}, {now}"),
    );

    // The browser window with the agent's bar and its trail, while it works.
    cx.update(|cx| {
        if let Some((_, view)) = super::window::BrowserWindow::open_view(cx) {
            view.update(cx, |view, cx| view.open_trail(cx));
        }
    })
    .ok();
    call(cx, json!({ "op": "click", "tab": tab, "ref": count })).await;
    sleep(cx, 400).await;
    if let Some(dir) = shots {
        crate::window_shot::save_largest(format!("{dir}/tools-working.png")).await;
    }
    let late = trail(cx);
    checks.check(
        "and later ones, never what it typed",
        [
            "Clicked “Delete everything”",
            "Dismissed the page's dialog",
            "Waited for “Loaded at last”",
            "Took a screenshot",
            "Clicked “Count”",
        ]
        .iter()
        .all(|step| late.contains(step))
            && !late.contains("New name"),
        &late,
    );
    let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
    println!(
        "browser tools check: note: at the end Filo active: {active}, in front: {front} (the browser window was opened for the pictures)"
    );
}

/// Where an element (by id) is, top to bottom in the view, scrolled into it.
async fn page_y(cx: &AsyncApp, tab: &str, id: &str) -> f64 {
    let tab = cx.update(|_| super::tab(tab)).ok().flatten();
    let Some(tab) = tab else { return 0. };
    super::page(cx, &tab, "box_of", json!({ "id": id })).await.ok().and_then(|value| value["y"].as_f64()).unwrap_or(0.)
}

/// The same tools on real pages: the-internet.herokuapp.com's test pages and a long Wikipedia article.
async fn real_pages(cx: &mut AsyncApp, checks: &mut Checks, shots: Option<&str>) {
    let base = "https://the-internet.herokuapp.com";
    let open = async |cx: &AsyncApp, path: &str| {
        let page = call(cx, json!({ "op": "open", "url": format!("{base}{path}") })).await;
        (find(&page, r"Opened tab (t\d+)"), page)
    };

    let (tab, page) = open(cx, "/upload").await;
    let field = find(&page, r#"\[(e\d+)\] file"#);
    let file = crate::config::config_dir().join("local/tmp/check-upload/cv.txt");
    let reply = call(cx, json!({ "op": "upload", "tab": tab, "ref": field, "paths": [file.to_string_lossy()] })).await;
    let submit = find(&page, r#"\[(e\d+)\] button "Upload""#);
    call(cx, json!({ "op": "click", "tab": tab, "ref": submit })).await;
    let done = call(cx, json!({ "op": "wait", "tab": tab, "text": "File Uploaded!", "seconds": 20 })).await;
    let read = call(cx, json!({ "op": "read", "tab": tab, "find": "cv.txt" })).await;
    checks.check(
        "real: herokuapp /upload takes the file",
        reply.contains("Chose cv.txt") && done.contains("shows after") && read.contains("cv.txt"),
        format!("{reply} || {done} || {read}"),
    );
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let (tab, page) = open(cx, "/download").await;
    let link = find(&page, r#"\[(e\d+)\] link "[^"]+\.(?:txt|json|png|jpg|pdf|csv)""#);
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": link })).await;
    let waited = call(cx, json!({ "op": "wait", "tab": tab, "for": "download", "seconds": 30 })).await;
    let both = format!("{reply} || {waited}");
    checks.check(
        "real: herokuapp /download is saved and reported",
        both.contains("Downloaded ") && both.contains("local/downloads/"),
        &both,
    );
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let (tab, page) = open(cx, "/dropdown").await;
    let menu = find(&page, r#"\[(e\d+)\] select"#);
    let reply = call(cx, json!({ "op": "select", "tab": tab, "ref": menu, "option": "Option 2" })).await;
    checks.check("real: herokuapp /dropdown picks Option 2", reply.contains(r#"selected="Option 2""#), &reply);
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let (tab, _) = open(cx, "/hovers").await;
    let y = page_y_selector(cx, &tab, ".figure img").await;
    let reply = call(cx, json!({ "op": "hover", "tab": tab, "x": y.0, "y": y.1 })).await;
    checks.check(
        "real: herokuapp /hovers shows a user's caption on hover",
        reply.contains("name: user1") || reply.contains("View profile"),
        &reply,
    );
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let (tab, page) = open(cx, "/javascript_alerts").await;
    let confirm = find(&page, r#"\[(e\d+)\] button "Click for JS Confirm""#);
    let prompt = find(&page, r#"\[(e\d+)\] button "Click for JS Prompt""#);
    let clicked = call(cx, json!({ "op": "click", "tab": tab, "ref": confirm })).await;
    call(cx, json!({ "op": "dialog", "tab": tab })).await;
    let after = call(cx, json!({ "op": "read", "tab": tab, "find": "You clicked" })).await;
    call(cx, json!({ "op": "click", "tab": tab, "ref": prompt })).await;
    call(cx, json!({ "op": "dialog", "tab": tab, "text": "hello from Filo" })).await;
    let typed = call(cx, json!({ "op": "read", "tab": tab, "find": "You entered" })).await;
    checks.check(
        "real: herokuapp /javascript_alerts confirm and prompt are answered",
        clicked.contains("confirm dialog") && after.contains("You clicked: Ok") && typed.contains("hello from Filo"),
        format!("{clicked} || {after} || {typed}"),
    );
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let (tab, page) = open(cx, "/windows").await;
    let link = find(&page, r#"\[(e\d+)\] link "Click Here""#);
    let reply = call(cx, json!({ "op": "click", "tab": tab, "ref": link })).await;
    let popup = find(&reply, r"opened a new tab, (t\d+)");
    let read = call(cx, json!({ "op": "read", "tab": popup })).await;
    checks.check(
        "real: herokuapp /windows opens its new window as a tab",
        read.contains("New Window"),
        format!("{reply} || {read}"),
    );
    call(cx, json!({ "op": "close", "tab": popup })).await;
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let (tab, page) = open(cx, "/dynamic_loading/2").await;
    let start = find(&page, r#"\[(e\d+)\] button "Start""#);
    call(cx, json!({ "op": "click", "tab": tab, "ref": start })).await;
    let shows = call(cx, json!({ "op": "wait", "tab": tab, "text": "Hello World!", "seconds": 20 })).await;
    checks.check("real: herokuapp /dynamic_loading waits for Hello World!", shows.contains("shows after"), &shows);
    call(cx, json!({ "op": "close", "tab": tab })).await;

    let page = call(cx, json!({ "op": "open", "url": "https://en.wikipedia.org/wiki/WebKit" })).await;
    let tab = find(&page, r"Opened tab (t\d+)");
    let (reply, picture, png) = shot(cx, json!({ "op": "screenshot", "tab": tab, "full": true })).await;
    let size = picture.as_ref().map(|picture| (picture.width(), picture.height())).unwrap_or_default();
    checks.check("real: a long Wikipedia page's full-page screenshot", size.1 > 3000, format!("{size:?}: {reply}"));
    if let Some(dir) = shots {
        let _ = std::fs::write(format!("{dir}/real-wikipedia-full-page.png"), &png);
    }
    call(cx, json!({ "op": "close", "tab": tab })).await;
}

/// The middle of the first element `selector` matches, in the view.
async fn page_y_selector(cx: &AsyncApp, tab: &str, selector: &str) -> (f64, f64) {
    let tab = cx.update(|_| super::tab(tab)).ok().flatten();
    let Some(tab) = tab else { return (0., 0.) };
    super::page(cx, &tab, "box_of", json!({ "selector": selector }))
        .await
        .ok()
        .map(|value| (value["x"].as_f64().unwrap_or(0.), value["y"].as_f64().unwrap_or(0.)))
        .unwrap_or_default()
}

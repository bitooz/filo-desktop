// `FILO_OPEN=browser-drag-check` (`dev` builds, on a data folder of its own): browser_drag end to end, in a tab in the
// background with another app in front, against the test site's `/drag` (browser/check.rs): an HTML5 drag-and-drop
// list (draggable tiles, reordered on drop with the dragstart's data), a list reordered by pointer events (as a
// gallery or dnd-kit does), and a range slider. Each drag goes through the socket as an agent's tool call does. It
// checks that the tiles get real drag events and reorder, by ref; that the cards follow real pointer events with the
// button down and reorder, by spot; that the slider moves; that a drop where the page takes none drops nothing; that a
// click after them all is an ordinary one; and that the user's pointer never moved and Filo never came forward. Each
// check prints a line; the process ends with 1 when any failed. `FILO_FIXTURE_SHOTS=<dir>`: the agent's screenshot
// after the drags.

use std::time::Duration;

use gpui::{App, AsyncApp};
use regex::Regex;
use serde_json::json;

use super::check::{ask, text};

pub const PAGE: &str = r#"<!doctype html><title>Check drag</title>
<style>body{font:16px -apple-system;margin:0} h1{position:absolute;left:40px;top:0;margin:12px 0;font-size:20px}
ul{list-style:none;margin:0;padding:0;position:absolute;width:200px} li{height:40px;line-height:40px;margin:0 0 8px;padding-left:12px;border-radius:6px;user-select:none}
#tiles{left:40px;top:60px} #tiles li{background:#dde} #cards{left:300px;top:60px} #cards li{background:#ede;touch-action:none}
#range{position:absolute;left:560px;top:70px;width:300px;margin:0} #after{position:absolute;left:560px;top:160px} #said{position:absolute;left:40px;top:320px;right:40px}</style>
<h1>Check drag</h1>
<ul id=tiles><li draggable=true id=A>Tile A</li><li draggable=true id=B>Tile B</li><li draggable=true id=C>Tile C</li><li draggable=true id=D>Tile D</li></ul>
<ul id=cards><li data-n=1>Card 1</li><li data-n=2>Card 2</li><li data-n=3>Card 3</li><li data-n=4>Card 4</li></ul>
<input type=range id=range min=0 max=100 value=0 aria-label="Volume">
<button id=after>After</button>
<p id=said></p>
<script>
const log = [];
const show = () => said.textContent = `tiles ${[...tiles.children].map((li) => li.id).join(',')} cards ${[...cards.children].map((li) => li.dataset.n).join(',')} range ${range.value} | ${log.slice(-14).join(' ')}`;
tiles.addEventListener('dragstart', (e) => { e.dataTransfer.setData('text/plain', e.target.id); e.dataTransfer.effectAllowed = 'move'; log.push(`dragstart:${e.isTrusted}:${e.target.id}`); show(); });
tiles.addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; });
tiles.addEventListener('drop', (e) => {
  e.preventDefault();
  const id = e.dataTransfer.getData('text/plain');
  const item = document.getElementById(id), over = e.target.closest('li');
  log.push(`drop:${e.isTrusted}:${id}`);
  if (item && over && item !== over) { const r = over.getBoundingClientRect(); tiles.insertBefore(item, e.clientY > r.top + r.height / 2 ? over.nextSibling : over); }
  show();
});
tiles.addEventListener('dragend', (e) => { log.push(`dragend:${e.isTrusted}:${e.dataTransfer.dropEffect}`); show(); });
let held = null, startY = 0, moves = 0, buttons = 0;
cards.addEventListener('pointerdown', (e) => { const li = e.target.closest('li'); if (!li) return; held = li; startY = e.clientY; moves = 0; buttons = e.buttons; li.setPointerCapture(e.pointerId); log.push(`pointerdown:${e.isTrusted}:b${e.buttons}`); show(); });
cards.addEventListener('pointermove', (e) => { if (!held) return; moves++; buttons = Math.min(buttons, e.buttons); held.style.transform = `translateY(${e.clientY - startY}px)`; });
cards.addEventListener('pointerup', (e) => {
  if (!held) return;
  held.style.transform = '';
  const before = [...cards.children].filter((li) => li !== held).find((li) => { const r = li.getBoundingClientRect(); return e.clientY < r.top + r.height / 2; });
  cards.insertBefore(held, before || null);
  log.push(`pointerup:${e.isTrusted}:moves${moves}:b${buttons}`);
  held = null; show();
});
range.addEventListener('input', show);
addEventListener('click', (e) => { log.push(e.target === after ? `after:${e.isTrusted}` : `click:${e.target.id || e.target.tagName}`); show(); });
show();
</script>"#;

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        let (active, _) = super::frontmost();
        println!(
            "browser drag check: {} {name}{}: {}",
            if ok { "ok" } else { "FAILED" },
            if active { " [Filo active]" } else { "" },
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
        // As in the browser check: a process just launched from a terminal can be brought forward with its first
        // window; one first, and Filo sent back if it was.
        let warm = cx.update(|_| super::native::Park::new(200., 200.)).ok().flatten();
        sleep(cx, 1500).await;
        if cx.update(|_| super::frontmost().0).unwrap_or(false) {
            println!("browser drag check: note: macOS brought the just-launched check forward with its first window; sending it back");
            cx.update(|_| super::native::deactivate()).ok();
            sleep(cx, 1000).await;
        }
        drop(warm);
        run(cx, &mut checks, &site, shots.as_deref()).await;
        println!("browser-drag-check: {}", if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) });
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

async fn said(cx: &AsyncApp, tab: &str) -> String {
    text(&ask(cx, json!({ "op": "read", "tab": tab, "find": "tiles " })).await)
}

async fn run(cx: &mut AsyncApp, checks: &mut Checks, site: &str, shots: Option<&str>) {
    let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
    checks.check("Filo isn't the active app", !active, format!("in front: {front}"));
    let pointer = cx.update(|_| super::native::pointer()).unwrap_or_default();

    let page = text(
        &ask(cx, json!({ "op": "open", "url": format!("{site}/drag"), "agent": "Check", "chat": "check-chat" })).await,
    );
    let tab =
        Regex::new(r"Opened tab (t\d+)").unwrap().captures(&page).map(|found| found[1].to_string()).unwrap_or_default();
    let find = |pattern: &str| {
        Regex::new(pattern)
            .ok()
            .and_then(|regex| regex.captures(&page))
            .map(|found| found[1].to_string())
            .unwrap_or_default()
    };
    let (tile_a, tile_c, slider) = (
        find(r#"\[(e\d+)\] draggable "Tile A""#),
        find(r#"\[(e\d+)\] draggable "Tile C""#),
        find(r#"\[(e\d+)\] slider"#),
    );
    checks.check(
        "draggable tiles have refs in browser_read",
        !tab.is_empty() && !tile_a.is_empty() && !tile_c.is_empty() && !slider.is_empty(),
        &page,
    );

    // HTML5 drag and drop, by ref: Tile A dropped on Tile C's middle goes before it.
    let dragged = text(&ask(cx, json!({ "op": "drag", "tab": tab, "from_ref": tile_a, "to_ref": tile_c })).await);
    let now = said(cx, &tab).await;
    checks.check(
        "by ref, a drag and drop reaches the page and is taken",
        dragged.contains("made it a drag and drop") && dragged.contains("took the drop"),
        &dragged,
    );
    checks.check(
        "the tiles got trusted dragstart, drop (with the dragstart's data) and dragend, and reordered, with no click",
        now.contains("tiles B,A,C,D")
            && now.contains("dragstart:true:A")
            && now.contains("drop:true:A")
            && now.contains("dragend:true:move")
            && !now.contains("click"),
        &now,
    );

    // Pointer events, by spots: Card 1 (middle 400, 80) let go below Card 3's middle (176) goes after it.
    let dragged = text(
        &ask(cx, json!({ "op": "drag", "tab": tab, "from_x": 400, "from_y": 80, "to_x": 400, "to_y": 190 })).await,
    );
    let now = said(cx, &tab).await;
    checks.check("by spots, a drag with real mouse events", dragged.contains("with real mouse events"), &dragged);
    let moves = Regex::new(r"pointerup:true:moves(\d+)")
        .unwrap()
        .captures(&now)
        .and_then(|found| found[1].parse::<u32>().ok())
        .unwrap_or(0);
    checks.check(
        "the cards followed trusted pointer events in steps and reordered",
        now.contains("cards 2,3,1,4") && now.contains("pointerdown:true") && moves >= 6,
        &now,
    );
    let held = Regex::new(r"pointerup:true:moves\d+:b1\b").unwrap().is_match(&now);
    checks.check(
        "the button is down in the press and every move (buttons 1)",
        now.contains("pointerdown:true:b1") && held,
        &now,
    );

    // A slider: from its middle to its right end.
    let dragged =
        text(&ask(cx, json!({ "op": "drag", "tab": tab, "from_ref": slider, "to_x": 858, "to_y": 80 })).await);
    let now = said(cx, &tab).await;
    let value =
        Regex::new(r"range (\d+)").unwrap().captures(&now).and_then(|found| found[1].parse::<u32>().ok()).unwrap_or(0);
    checks.check("a slider dragged to its end goes there", value >= 95, format!("{dragged} || {now}"));

    // A drop where the page takes none: nothing moves, and the drag ends with no drop.
    let dragged =
        text(&ask(cx, json!({ "op": "drag", "tab": tab, "from_ref": tile_a, "to_x": 700, "to_y": 600 })).await);
    let now = said(cx, &tab).await;
    checks.check(
        "a drop the page doesn't take drops nothing",
        dragged.contains("didn't take the drop")
            && now.contains("tiles B,A,C,D")
            && now.contains("dragstart:true:A dragend:true:"),
        format!("{dragged} || {now}"),
    );

    // After them all, a click is an ordinary one.
    let clicked = text(&ask(cx, json!({ "op": "click_at", "tab": tab, "x": 585, "y": 170 })).await);
    let now = said(cx, &tab).await;
    checks.check(
        "a click after the drags is an ordinary one",
        now.trim_end().ends_with("after:true"),
        format!("{clicked} || {now}"),
    );

    if let Some(dir) = shots {
        let shot = ask(cx, json!({ "op": "screenshot", "tab": tab })).await;
        if let Some(png) = shot
            .as_ref()
            .ok()
            .and_then(|value| value["png"].as_str())
            .and_then(|png| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).ok())
        {
            let _ = std::fs::write(format!("{dir}/drag-after.png"), png);
        }
    }

    let after = cx.update(|_| super::native::pointer()).unwrap_or_default();
    checks.check("the user's pointer never moved", pointer == after, format!("{pointer:?} then {after:?}"));
    let (active, front) = cx.update(|_| super::frontmost()).unwrap_or_default();
    checks.check("Filo still isn't the active app", !active, format!("in front: {front}"));
}

// `FILO_OPEN=glances:<file>`: a glance in a made-up chat, in a window of its own (no backend). The file is
// a glance as `glances:get` returns it (`node scripts/glance-fixture.mjs > /tmp/glance.json`). Checks,
// as AppKit has the page's views, that the card's page is as tall as its content, stays inside the chat's column and
// above the composer as the chat scrolls, lets the wheel over it scroll the chat, leaves the composer typeable, hides
// for its text view, and opens larger over the chat (as the media viewer shows a picture), its page as wide as the
// viewer's card, with the inline one hidden. Its card's row holds its title, its page and its padding, the next row
// starts under it (also after the window is resized), and the header's work popover over it shows through the page and
// takes its clicks, as does a right-click menu hanging over it (any menu of gpui-component's one_menu.rs). Prints a line
// per check and quits, exit 1 when one fails.
// `FILO_FIXTURE_SHOTS=<dir>` saves a screenshot at each step.

use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, Bounds, Context, Entity, IntoElement, ParentElement as _, Pixels, Render,
    Styled as _, Window, div, px, size,
};
use serde_json::{Value, json};

use super::Conversation;
use super::glance::{self, GlanceData, GlanceSheet};
use crate::screens::chat::working_panel;

/// The window's width, `FILO_WINDOW=<width>x…` (780 by default): the fixture's clicks and keys find the window by it.
fn width() -> f32 {
    crate::test_run::window_size((780., 0.)).0
}
const GLANCE: &str = "fixture-glance";
const MESSAGE: &str = "m-glance";

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open.starts_with("glances:"))
}

pub fn open(cx: &mut App) {
    let path = std::env::var("FILO_OPEN").unwrap_or_default().trim_start_matches("glances:").to_string();
    let data: GlanceData = match std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
    {
        Ok(data) => data,
        Err(error) => {
            println!("glances: can't read {path}: {error}");
            std::process::exit(1);
        }
    };
    // Focused and in front: a web view is only made in the active window (making one brings its app forward).
    let host = |_: &mut Window, cx: &mut App| cx.new(|_| Host { shown: None, popover: None, menu_strip: None });
    crate::smoke::fixtures::open(cx, "glances", "Filo glances", (width(), 720.), true, host, async move |host, cx| {
        drive(host, data, cx).await
    });
}

struct Host {
    shown: Option<Entity<Conversation>>,
    /// The header's work popover, this far down the window, and what its row does.
    popover: Option<(f32, working_panel::OnOpen)>,
    /// A strip with a right-click menu, there, and whether its item was clicked.
    menu_strip: Option<(Bounds<Pixels>, std::rc::Rc<std::cell::Cell<bool>>)>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // As the app shell draws it: the dialog layer (the larger view opens there), then the sweep of glance pages.
        let popover = self
            .popover
            .clone()
            .map(|(top, on_open)| working_panel::popover("glance-work", top, &[work_row()], on_open, cx));
        let menu_strip = self.menu_strip.clone().map(|(bounds, clicked)| {
            use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
            div().absolute().left(bounds.left()).top(bounds.top()).w(bounds.size.width).h(bounds.size.height).child(
                div().size_full().context_menu(move |menu, _, _| {
                    let clicked = clicked.clone();
                    menu.item(PopupMenuItem::new("Copy image").on_click(move |_, _, _| clicked.set(true)))
                        .item(PopupMenuItem::new("Copy link"))
                        .item(PopupMenuItem::new("Save as…"))
                }),
            )
        });
        div()
            .size_full()
            .relative()
            .children(self.shown.clone())
            .children(popover)
            .children(menu_strip)
            .children(gpui_component::Root::render_dialog_layer(window, cx))
            .child(glance::sweep())
    }
}

/// A coding task working in the chat, as its header's popover lists it.
fn work_row() -> working_panel::PanelRow {
    working_panel::PanelRow {
        key: "coding-fixture".into(),
        tool: "claude".into(),
        name: "Claude Code".into(),
        title: "Agents header view menu".into(),
        detail: Some("grep -v …".into()),
        since: None,
        state: "running",
        label: "Working",
        target: working_panel::Target::CodingTask("coding-fixture".into()),
        facts: Vec::new(),
    }
}

const MINUTE: f64 = 60_000.;

/// A made-up chat: a question yesterday, a few turns today, then a question, the glance and Filo's reply under it. The
/// day markers sit above the "New messages" line the chat opens at, so the card still starts under the composer.
fn messages(data: &GlanceData) -> Vec<Value> {
    let base = crate::format::now_ms() - 30. * MINUTE;
    let mut messages: Vec<Value> = (0..8)
        .map(|index| {
            let user = index % 2 == 0;
            let content = if user { format!("Question {index}?") } else { format!("Reply {index}, long enough to wrap over a couple of lines of the bubble so rows aren't all one height.") };
            let yesterday = if index == 0 { 24. * 60. } else { 0. };
            json!({ "_id": format!("m{index}"), "_creationTime": base - yesterday * MINUTE + index as f64 * MINUTE, "role": if user { "user" } else { "assistant" }, "content": content })
        })
        .collect();
    messages.push(json!({ "_id": "m-ask", "_creationTime": base + 20. * MINUTE, "role": "user", "content": "How did my focus time go these last weeks?" }));
    messages.push(json!({
        "_id": MESSAGE, "_creationTime": base + 21. * MINUTE, "role": "assistant",
        "content": format!("{} (Aug 10 to Oct 4)\n{}", data.title, data.text),
        "glance": { "glanceId": GLANCE, "title": data.title, "asOf": "Aug 10 to Oct 4", "icon": data.icon },
    }));
    messages.push(json!({ "_id": "m-reply", "_creationTime": base + 21. * MINUTE + 1000., "role": "assistant", "content": "Meetings have been eating your focus time since late August." }));
    messages
}

struct Driver<'a> {
    view: Entity<Conversation>,
    cx: &'a mut AsyncWindowContext,
    failures: usize,
    shots: Option<String>,
}

type Probe = Option<(bool, Option<f32>, Option<Bounds<Pixels>>)>;

impl Driver<'_> {
    async fn wait(&mut self, ms: u64) {
        self.cx.background_executor().timer(Duration::from_millis(ms)).await;
    }

    /// A few frames drawn.
    async fn settle(&mut self) {
        self.cx.update(|window, _| window.refresh()).ok();
        self.wait(300).await;
    }

    fn shot(&mut self, name: &str) {
        crate::smoke::fixtures::shot(self.shots.as_deref(), width() as f64, name);
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        if ok {
            println!("glances: {name}: ok {detail:?}");
        } else {
            self.failures += 1;
            println!("glances: {name}: FAILED {detail:?}");
        }
    }

    fn probe(&mut self) -> Probe {
        let view = self.view.clone();
        self.cx.update(|window, cx| view.read(cx).glance_probe(GLANCE, window)).ok().flatten()
    }

    fn column(&mut self) -> (Bounds<Pixels>, f32) {
        self.view.read_with(self.cx, |view, _| view.glance_column()).expect("view")
    }

    fn scroll_position(&mut self) -> (usize, f32) {
        self.view.read_with(self.cx, |view, _| view.scroll_position()).expect("view")
    }

    /// The page shown, inside the list's sides and above the composer's top.
    fn inside(&mut self, name: &str) -> Option<Bounds<Pixels>> {
        let probe = self.probe();
        let (viewport, composer) = self.column();
        let line = viewport.bottom() - px(composer);
        let shown = probe.as_ref().and_then(|(_, _, shown)| *shown);
        let ok = shown.is_some_and(|shown| {
            shown.left() >= viewport.left() - px(0.5)
                && shown.right() <= viewport.right() + px(0.5)
                && shown.top() >= viewport.top() - px(0.5)
                && shown.bottom() <= line + px(0.5)
        });
        self.check(name, ok, (shown, viewport, line));
        shown
    }

    /// A few wheel steps up (`dy` negative) or down at (x, y), window points, sent to the window as AppKit's current
    /// event (`wheel_to_window`): without that the glance's clip view doesn't see a wheel and hands it to the page, and
    /// one posted to the queue goes where the real pointer is (the Mac is in use while this runs).
    async fn wheel(&mut self, x: Pixels, y: Pixels, dy: f64) {
        for _ in 0..3 {
            #[cfg(target_os = "macos")]
            wheel_to_window(width() as f64, f64::from(f32::from(x)), f64::from(f32::from(y)), 0., dy);
            self.wait(60).await;
        }
        self.settle().await;
    }

    /// The glance's row and the next one, scrolled up a little from the end (at the end the list follows it and gives
    /// no rows' bounds; its scroll top is then the end, a view's height under the top of what shows), as laid out
    /// (window points).
    async fn glance_rows(&mut self) -> Option<(Bounds<Pixels>, Bounds<Pixels>)> {
        let index = self
            .view
            .read_with(self.cx, |view, _| {
                view.rows
                    .iter()
                    .position(|row| matches!(row, super::ViewRow::Message { message, .. } if message.id == MESSAGE))
            })
            .ok()
            .flatten()?;
        let (viewport, _) = self.column();
        self.view
            .update(self.cx, |view, cx| {
                view.list_state.scroll_by(-viewport.size.height - px(80.));
                cx.notify();
            })
            .ok();
        self.settle().await;
        self.settle().await;
        self.view
            .read_with(self.cx, |view, _| {
                Some((view.list_state.bounds_for_item(index)?, view.list_state.bounds_for_item(index + 1)?))
            })
            .ok()
            .flatten()
    }

    /// The page shows inside its card's row, under the card's head and above its bottom padding, and the next row
    /// starts under the card's row. Back at the end of the chat after.
    async fn in_order(&mut self, name: &str) {
        let rows = self.glance_rows().await;
        let probe = self.probe();
        let (shown, height) =
            (probe.as_ref().and_then(|(_, _, shown)| *shown), probe.as_ref().and_then(|(_, height, _)| *height));
        // The card's border, top padding and head over the page; its padding and border under it.
        let (over, under) = (1. + 10. + 28. + crate::theme::space::SM, 14. + 1.);
        let ok = rows.zip(shown).zip(height).is_some_and(|(((glance, next), shown), height)| {
            let page_bottom = shown.top() + px(height);
            next.top() >= glance.bottom() - px(0.5)
                && shown.top() >= glance.top() + px(over - 0.5)
                && page_bottom <= glance.bottom() - px(under - 0.5)
        });
        self.check(name, ok, (rows, shown, height));
        self.to_bottom().await;
    }

    /// Scrolled to the end of the chat.
    async fn to_bottom(&mut self) {
        self.view
            .update(self.cx, |view, cx| {
                view.list_state.scroll_by(px(10_000.));
                cx.notify();
            })
            .ok();
        self.settle().await;
        self.settle().await;
    }
}

async fn drive(host: Entity<Host>, data: GlanceData, cx: &mut AsyncWindowContext) -> usize {
    let view = cx
        .update(|window, cx| {
            let view = cx.new(|cx| Conversation::new(None, "Filo".into(), window, cx));
            view.update(cx, |view, _| {
                view.fixture = true;
                view.session_id = Some("glance-chat".into());
            });
            view
        })
        .expect("window");
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok();
    if let Some(dir) = &shots {
        let _ = std::fs::create_dir_all(dir);
    }
    let messages = messages(&data);
    let mut driver = Driver { view, cx, failures: 0, shots };
    let d = &mut driver;
    let shown = d.view.clone();
    host.update(d.cx, |host, cx| {
        host.shown = Some(shown);
        cx.notify();
    })
    .ok();
    let detail = serde_json::from_value(
        json!({ "session": { "_id": "glance-chat", "title": "Glances" }, "messages": messages, "hasMore": false }),
    )
    .expect("fixture detail");
    d.view
        .update(d.cx, |view, cx| {
            view.set_attending(true, cx);
            view.set_fixture_glance(GLANCE.into(), data, cx);
            view.detail = Some(Ok(Some(detail)));
            view.data_changed(cx);
        })
        .ok();

    // 1. Opened at "New messages", the card starts under the composer: its page is cut at the composer's top.
    let mut probe = None;
    for _ in 0..50 {
        d.settle().await;
        probe = d.probe();
        if probe.as_ref().is_some_and(|(ready, _, shown)| *ready && shown.is_some()) {
            break;
        }
    }
    let active = d.cx.update(|window, _| window.is_window_active()).unwrap_or(false);
    d.check("the window is active (pages are made only then)", active, active);
    d.inside("under the composer, the page's view stops at the composer's top");
    d.shot("glance-1-under-composer");

    // 2. At the bottom of the chat, all of it shows, as tall as its content.
    d.to_bottom().await;
    let probe = d.probe().or(probe);
    let height = probe.as_ref().and_then(|(_, height, _)| *height);
    d.check(
        "the page measures its content (chart and legend, about 180 px)",
        height.is_some_and(|height| (150. ..=260.).contains(&height)),
        probe,
    );
    let shown = probe.as_ref().and_then(|(_, _, shown)| *shown);
    d.check(
        "the page's view is as tall as its content",
        shown.zip(height).is_some_and(|(shown, height)| (f32::from(shown.size.height) - height).abs() < 1.5),
        (shown, height),
    );
    d.inside("the page's view is inside the chat's column, above the composer");
    // As wide as its content needs, from a reply bubble's width up to the timeline's rail less the bubbles' gap from
    // it, starting exactly where the replies do (glance.rs `card_place`; the page sits 15 points inside the card).
    let (viewport, _) = d.column();
    let list = f32::from(viewport.size.width);
    let chat = d.view.read_with(d.cx, |view, _| view.width).unwrap_or(0.);
    let gutter = super::timeline::gutter(chat);
    let natural = d.view.read_with(d.cx, |view, _| view.glance_natural_width(GLANCE)).ok().flatten();
    let (left, card) = glance::card_place(list, gutter, natural, false);
    let column = list.min(crate::theme::MAX_CONTENT_WIDTH) - 2. * crate::theme::PAGE_PADDING;
    let replies = (list - list.min(crate::theme::MAX_CONTENT_WIDTH)) / 2. + crate::theme::PAGE_PADDING;
    let least = (column * 0.86).floor();
    let rail = chat - super::timeline::CLEAR;
    d.check(
        "the card starts at the replies' left edge, at least a bubble wide, its right edge clear of the rail",
        shown.is_some_and(|shown| {
            let (x, w) = (f32::from(shown.left() - viewport.left()), f32::from(shown.size.width));
            (x - (left + 15.)).abs() < 1.
                && (left - replies).abs() < 0.5
                && (w - (card - 30.)).abs() < 1.
                && card >= least - 0.5
                && left + card <= rail.max(left + least) + 0.5
        }),
        (shown, (list, chat, gutter), natural, (left, card), (replies, least, rail)),
    );
    d.shot("glance-2-inline");
    d.in_order("the page shows under the card's title, inside its card, and the next row starts under the card").await;

    // 2b. Narrower, then wider again: the page measures itself anew and the rows move with it.
    let full = d.cx.update(|window, _| window.viewport_size()).ok();
    if let Some(full) = full {
        d.cx.update(|window, _| window.resize(size(full.width - px(260.), full.height))).ok();
        for _ in 0..6 {
            d.settle().await;
        }
        d.to_bottom().await;
        d.in_order("narrower, the page still shows inside its card, and the next row under it").await;
        d.cx.update(|window, _| window.resize(full)).ok();
        for _ in 0..6 {
            d.settle().await;
        }
        d.to_bottom().await;
        d.in_order("wide again, the page still shows inside its card, and the next row under it").await;
    }

    // 2c. The header's work popover over the card: it shows through a hole the card's shape cut in the page, and a
    // click on its row is the popover's, not the page's.
    if let Some(shown) = d.probe().and_then(|(_, _, shown)| shown) {
        let opened = std::rc::Rc::new(std::cell::Cell::new(false));
        let on_open: working_panel::OnOpen = {
            let opened = opened.clone();
            std::rc::Rc::new(move |_, _, _| opened.set(true))
        };
        let top = f32::from(shown.top()) + 20.;
        host.update(d.cx, |host, cx| {
            host.popover = Some((top, on_open));
            cx.notify();
        })
        .ok();
        d.settle().await;
        d.settle().await;
        let holes = d.view.read_with(d.cx, |view, _| view.glance_holes(GLANCE)).unwrap_or(0);
        d.check("the work popover over the page shows through a hole in it", holes == 1, holes);
        d.shot("glance-2c-work-popover");
        // The popover is centred in the window, its first row just under its top.
        let point = gpui::point(
            d.cx.update(|window, _| window.viewport_size().width / 2.).unwrap_or(shown.center().x),
            px(top + 24.),
        );
        crate::smoke::native_input::click(width() as f64, f64::from(f32::from(point.x)), f64::from(f32::from(point.y)));
        d.settle().await;
        d.check(
            "a click on the popover's row over the page opens its task",
            opened.get() && shown.contains(&point),
            (opened.get(), point, shown),
        );
        host.update(d.cx, |host, cx| {
            host.popover = None;
            cx.notify();
        })
        .ok();
        d.settle().await;
        d.settle().await;
        let holes = d.view.read_with(d.cx, |view, _| view.glance_holes(GLANCE)).unwrap_or(0);
        d.check("the popover closed, the page's hole closes", holes == 0, holes);
    }

    // 2d. A right-click menu opened just above the page, hanging over it: it shows through a hole in the page, and a
    // click on its item there is the menu's.
    if let Some(shown) = d.probe().and_then(|(_, _, shown)| shown) {
        let clicked = std::rc::Rc::new(std::cell::Cell::new(false));
        let strip = Bounds::new(gpui::point(shown.left(), shown.top() - px(28.)), size(shown.size.width, px(24.)));
        host.update(d.cx, |host, cx| {
            host.menu_strip = Some((strip, clicked.clone()));
            cx.notify();
        })
        .ok();
        d.settle().await;
        let at = strip.origin + gpui::point(px(40.), px(16.));
        crate::smoke::native_input::right_click(width() as f64, f64::from(f32::from(at.x)), f64::from(f32::from(at.y)));
        d.settle().await;
        d.settle().await;
        let holes = d.view.read_with(d.cx, |view, _| view.glance_holes(GLANCE)).unwrap_or(0);
        d.check("a right-click menu over the page shows through a hole in it", holes == 1, holes);
        d.shot("glance-2d-menu-over-page");
        // Its first item is just under where it opened, over the page.
        let item = at + gpui::point(px(30.), px(16.));
        crate::smoke::native_input::click(width() as f64, f64::from(f32::from(item.x)), f64::from(f32::from(item.y)));
        d.settle().await;
        d.check(
            "a click on the menu's item over the page is the menu's",
            clicked.get() && shown.contains(&item),
            (clicked.get(), item, shown),
        );
        d.settle().await;
        let holes = d.view.read_with(d.cx, |view, _| view.glance_holes(GLANCE)).unwrap_or(0);
        d.check("the menu closed, the page's hole closes", holes == 0, holes);
        host.update(d.cx, |host, cx| {
            host.menu_strip = None;
            cx.notify();
        })
        .ok();
        d.settle().await;
    }

    // 3. The wheel over a bubble, then over the page, scrolls the chat; the page moves with it and is cut at the
    // composer's top.
    let (viewport, composer) = d.column();
    let before = d.scroll_position();
    d.wheel(viewport.center().x, viewport.top() + px(120.), -60.).await;
    let after = d.scroll_position();
    d.check("the wheel over a bubble scrolls the chat (the control)", after != before, (before, after));
    d.to_bottom().await;
    let before = d.scroll_position();
    if let Some(shown) = shown {
        let center = shown.center();
        d.wheel(center.x, center.y, -60.).await;
    }
    let after = d.scroll_position();
    d.check("the wheel over the page scrolls the chat", after != before, (before, after));
    let cut = d.inside("scrolled, the page's view stays above the composer");
    d.check(
        "scrolled, the page's view is cut at the composer, not drawn over it",
        cut.zip(height).is_some_and(|(cut, height)| f32::from(cut.size.height) < height - 1.),
        (cut, height),
    );
    let holes = d.view.read_with(d.cx, |view, _| view.glance_holes(GLANCE)).unwrap_or(0);
    d.check("scrolled up, the jump-to-latest button shows through a hole in the page", holes == 1, holes);
    d.shot("glance-3-scrolled-under-composer");

    // 4. The composer still takes a click and typing, with the page just above it, after a click in the page (which
    // gives the page the keyboard).
    if let Some(cut) = cut {
        let center = cut.center();
        crate::smoke::native_input::click(
            width() as f64,
            f64::from(f32::from(center.x)),
            f64::from(f32::from(center.y)),
        );
        d.wait(300).await;
    }
    let point =
        (f64::from(f32::from(viewport.center().x)), f64::from(f32::from(viewport.bottom())) - f64::from(composer) / 2.);
    crate::smoke::native_input::click(width() as f64, point.0, point.1);
    d.wait(150).await;
    crate::smoke::native_input::key(width() as f64, "a", 0, 0);
    d.settle().await;
    let typed = d.view.read_with(d.cx, |view, cx| view.input.read(cx).value().to_string()).unwrap_or_default();
    let keys = crate::smoke::native_input::has_keys(width() as f64);
    d.check(
        "after a click in the page, the composer takes a click and typing",
        typed == "a",
        (&typed, "app active, window key", keys),
    );
    d.to_bottom().await;

    // 5. As text: the page hides; back to the chart: it shows again.
    d.view.update(d.cx, |view, cx| view.toggle_glance_text_for(GLANCE, cx)).ok();
    d.settle().await;
    let hidden = d.probe();
    d.check("as text, the page's view hides", hidden.as_ref().is_some_and(|(_, _, shown)| shown.is_none()), hidden);
    d.shot("glance-4-as-text");
    d.view.update(d.cx, |view, cx| view.toggle_glance_text_for(GLANCE, cx)).ok();
    d.settle().await;
    let back = d.probe();
    d.check("back to the chart, it shows again", back.as_ref().is_some_and(|(_, _, shown)| shown.is_some()), back);

    // 5b. Content wider than its card: a sideways swipe over the page scrolls it inside the card, not the chat.
    if let Some(shown) = back
        .as_ref()
        .and_then(|(_, _, shown)| *shown)
        .filter(|shown| natural.is_some_and(|natural| natural > f32::from(shown.size.width) + 0.5))
    {
        d.shot("glance-4a-before-sideways");
        let before = d.scroll_position();
        let center = shown.center();
        for _ in 0..4 {
            #[cfg(target_os = "macos")]
            wheel_to_window(width() as f64, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)), 120., 0.);
            d.wait(60).await;
        }
        d.settle().await;
        let after = d.scroll_position();
        d.check(
            "a sideways swipe over a page wider than its card leaves the chat where it was",
            after == before,
            (before, after),
        );
        d.shot("glance-4b-scrolled-sideways");
    }

    // 6. Open larger: its own page in the dialog, the chat's hidden under it.
    let view = d.view.clone();
    let sheet: Option<Entity<GlanceSheet>> =
        d.cx.update(|window, cx| view.update(cx, |view, cx| view.open_glance_for(MESSAGE, window, cx))).ok().flatten();
    // The card's size from the viewer's first frame, before its page shows.
    let mut first = None;
    for _ in 0..60 {
        first = sheet.as_ref().and_then(|sheet| sheet.read_with(d.cx, |sheet, _| sheet.opened_height()).ok().flatten());
        if first.is_some() {
            break;
        }
        d.wait(20).await;
    }
    let mut large = None;
    for _ in 0..30 {
        d.settle().await;
        large = sheet.as_ref().and_then(|sheet| d.cx.update(|window, cx| sheet.read(cx).probe(window)).ok().flatten());
        if large.as_ref().is_some_and(|(ready, shown)| *ready && shown.is_some()) {
            break;
        }
    }
    d.check(
        "opened larger, its page shows",
        large.as_ref().is_some_and(|(ready, shown)| *ready && shown.is_some()),
        large,
    );
    let last = sheet.as_ref().and_then(|sheet| sheet.read_with(d.cx, |sheet, _| sheet.opened_height()).ok().flatten());
    d.check(
        "opened larger, the card is its final size from the first frame",
        first.zip(last).is_some_and(|(first, last)| (first - last).abs() < 1.),
        (first, last),
    );
    // Laid out in a box as wide as the viewer's card (once, a 0 px wide box showed an empty sheet), inside the window.
    let window = d.cx.update(|window, _| window.viewport_size()).ok();
    let large_shown = large.as_ref().and_then(|(_, shown)| *shown);
    d.check(
        "opened larger, its page is as wide as the card's inside, within the window",
        large_shown.zip(window).is_some_and(|(shown, window)| {
            f32::from(shown.size.width) >= 400.
                && shown.left() >= px(0.)
                && shown.right() <= window.width
                && shown.top() >= px(0.)
                && shown.bottom() <= window.height
        }),
        (large_shown, window),
    );
    let inline = d.probe();
    d.check(
        "opened larger, the chat's page hides",
        inline.as_ref().is_none_or(|(_, _, shown)| shown.is_none()),
        inline,
    );
    d.shot("glance-5-larger");
    if let Some(sheet) = &sheet {
        sheet.update(d.cx, |sheet, cx| sheet.show_tab(true, cx)).ok();
        d.settle().await;
        let table = d.cx.update(|window, cx| sheet.read(cx).probe(window)).ok().flatten();
        d.check("the table tab hides the larger page", table.as_ref().is_none_or(|(_, shown)| shown.is_none()), table);
        d.shot("glance-6-larger-table");
    }
    d.cx.update(gpui_component::WindowExt::close_dialog).ok();
    d.settle().await;
    d.settle().await;
    let closed = d.probe();
    d.check(
        "closed, the chat's page shows again",
        closed.as_ref().is_some_and(|(_, _, shown)| shown.is_some()),
        closed,
    );
    d.failures
}

/// A wheel step by (dx, dy) points (right and down are positive) at (x, y), points from the top left of the window
/// `width` points wide, made with CoreGraphics and sent to the window as `smoke::native_input::scroll_in` does, but as
/// AppKit's current event, as a real one is.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2's macros
fn wheel_to_window(width: f64, x: f64, y: f64, dx: f64, dy: f64) -> bool {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Point,
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateScrollWheelEvent(
            source: *const std::ffi::c_void,
            units: u32,
            count: u32,
            wheel1: i32,
            ...
        ) -> *mut std::ffi::c_void;
        fn CGEventSetLocation(event: *mut std::ffi::c_void, location: Point);
        fn CGEventSetIntegerValueField(event: *mut std::ffi::c_void, field: u32, value: i64);
        fn CFRelease(value: *const std::ffi::c_void);
    }
    // kCGMouseEventWindowUnderMousePointer and …ThatCanHandleThisEvent.
    const UNDER_POINTER: u32 = 91;
    const CAN_HANDLE: u32 = 92;
    // SAFETY: AppKit and CoreGraphics on the main thread, as smoke.rs's input does.
    unsafe {
        let Some(number) = crate::smoke::native_input::window_number(width) else { return false };
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let window: *mut Object = msg_send![app, windowWithWindowNumber: number];
        if window.is_null() {
            return false;
        }
        let view: *mut Object = msg_send![window, contentView];
        let bounds: Rect = msg_send![view, bounds];
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        let main: *mut Object = msg_send![screens, objectAtIndex: 0usize];
        let screen: Rect = msg_send![main, frame];
        // The event has no window, so AppKit reads its place on the screen as its place in the window: it's put where
        // the window's point would be with the window at the screen's corner.
        let location = Point { x, y: screen.size.y - bounds.size.y + y };
        let cg = CGEventCreateScrollWheelEvent(std::ptr::null(), 0, 2, (-dy) as i32, (-dx) as i32);
        if cg.is_null() {
            return false;
        }
        CGEventSetLocation(cg, location);
        CGEventSetIntegerValueField(cg, UNDER_POINTER, number as i64);
        CGEventSetIntegerValueField(cg, CAN_HANDLE, number as i64);
        let event: *mut Object = msg_send![class!(NSEvent), eventWithCGEvent: cg];
        CFRelease(cg);
        if event.is_null() {
            return false;
        }
        // AppKit's current event while the window handles it, as when the run loop takes a real one from the queue:
        // the glance's clip view reads it to let the wheel through (`hit_test`).
        let current: *mut Object = msg_send![app, currentEvent];
        let settable: bool = msg_send![app, respondsToSelector: sel!(_setCurrentEvent:)];
        if settable {
            let _: () = msg_send![app, _setCurrentEvent: event];
        }
        let _: () = msg_send![window, sendEvent: event];
        if settable {
            let _: () = msg_send![app, _setCurrentEvent: current];
        }
        settable
    }
}

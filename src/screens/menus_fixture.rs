// `FILO_OPEN=menus`: one menu or popover open at a time (gpui-component's one_menu.rs), in a window of its own with no
// backend. A picture with a right-click menu ("Copy image") and a badge drawn over its corner with its own
// (Top center, Right sidebar), as in a chat with work going on; two rows side by side whose right-click menus share an
// id; a link (its "Copy link" menu); a dropdown button (View); one of the app's own popups (escape.rs); and a web view (as an artifact's page) with a
// right-click menu that opens over it (it gets a hole there, web_holes.rs). Real AppKit clicks, keys and scroll-wheel
// events (smoke.rs `native_input`) open one while another is open and count the menus drawn. Then a row the View
// dropdown, the popup and the picture's right-click menu open over: with the pointer, the scroll wheel and a click on
// each menu where the row is under it, the row doesn't hover, show its tooltip, scroll or take the click (it does
// each with nothing open). Prints a line per check and quits, exit 1 when one fails. `FILO_FIXTURE_SHOTS=<dir>` saves a
// window screenshot of each step.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, Bounds, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, rgb, size,
};
use gpui_component::button::Button;
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::one_menu;
use gpui_component::webview::WebView;

use crate::smoke::native_input;

const WIDTH: f32 = 640.;
const HEIGHT: f32 = 480.;
const SETTLE: Duration = Duration::from_millis(350);
/// Longer than GPUI's tooltip delay (500 ms): a tooltip the pointer brought up has shown by then.
const TIP_WAIT: Duration = Duration::from_millis(900);
/// The row behind the menus, window points: the View dropdown, the popup and the picture's right-click menu (at 300, 300)
/// each open over part of it.
const ROW: [f32; 4] = [380., 240., 240., 100.];

fn row() -> Bounds<Pixels> {
    let [x, y, width, height] = ROW;
    Bounds::new(gpui::point(px(x), px(y)), size(px(width), px(height)))
}

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "menus")
}

pub fn open(cx: &mut App) {
    let host = |window: &mut Window, cx: &mut App| {
        crate::web_holes::init(cx);
        let web = gpui_component::wry::WebViewBuilder::new()
            .with_html("<!doctype html><body style='margin:0;background:#2f6f4f'></body>")
            .build_as_child(&*window)
            .ok()
            .map(|webview| cx.new(|cx| WebView::new(webview, window, cx)));
        cx.new(|_| Host {
            popup_open: false,
            built: Rc::default(),
            web,
            web_item: Rc::default(),
            behind: Rc::default(),
        })
    };
    crate::smoke::fixtures::open(cx, "menus", "Filo menus", (WIDTH, HEIGHT), false, host, drive);
}

struct Host {
    popup_open: bool,
    /// The menus made, in order (each is made as it opens): which one a click opened.
    built: Rc<RefCell<Vec<&'static str>>>,
    web: Option<Entity<WebView>>,
    /// The web view's menu's first item was clicked.
    web_item: Rc<std::cell::Cell<bool>>,
    /// What the row behind the menus heard.
    behind: Rc<Behind>,
}

/// What the row behind the menus heard: the pointer on it (GPUI's hover, as its hover colour), its tooltip showing, a
/// click, the scroll wheel (it scrolls).
#[derive(Default)]
struct Behind {
    hovered: Cell<bool>,
    tip: Cell<bool>,
    clicked: Cell<bool>,
    scroll: ScrollHandle,
}

#[derive(Debug, PartialEq)]
struct Heard {
    hovered: bool,
    tip: bool,
    clicked: bool,
    scrolled: bool,
}

const NOTHING: Heard = Heard { hovered: false, tip: false, clicked: false, scrolled: false };

impl Behind {
    fn heard(&self) -> Heard {
        Heard {
            hovered: self.hovered.get(),
            tip: self.tip.get(),
            clicked: self.clicked.get(),
            scrolled: self.scroll.offset().y != px(0.),
        }
    }

    /// Forgets the tooltip, the click and the scrolling (the hover stays what the pointer makes it).
    fn reset(&self) {
        self.tip.set(false);
        self.clicked.set(false);
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
    }
}

impl Host {
    fn set_popup(&mut self, open: bool, cx: &mut Context<Self>) {
        self.popup_open = open;
        cx.notify();
    }
}

/// A menu of `items` that notes `name` in `built` when it's made.
fn menu(
    built: &Rc<RefCell<Vec<&'static str>>>,
    name: &'static str,
    items: &'static [&'static str],
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let built = built.clone();
    move |menu, _, _| {
        built.borrow_mut().push(name);
        items.iter().fold(menu, |menu, item| menu.item(PopupMenuItem::new(*item)))
    }
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::escape::frame(window, cx);
        if self.popup_open {
            crate::escape::popup(window, cx, |this, _, cx| this.set_popup(false, cx));
        }
        let built = &self.built;
        div()
            .size_full()
            .relative()
            .bg(rgb(0x1c1c1e))
            // The picture, and the badge over its top corner (drawn after it, so on top).
            .child(div().id("menus-tile").absolute().left(px(40.)).top(px(100.)).w(px(320.)).h(px(220.)).child(
                div().size_full().rounded(px(12.)).bg(rgb(0x3a3a3c)).context_menu(menu(built, "tile", &["Copy image"])),
            ))
            .child(div().id("menus-badge").absolute().left(px(220.)).top(px(80.)).w(px(220.)).h(px(44.)).child(
                div().size_full().rounded(px(22.)).bg(rgb(0x2c4a7a)).context_menu(menu(
                    built,
                    "badge",
                    &["Top center (default)", "Right sidebar"],
                )),
            ))
            // A link, whose right-click menu is the text view's own.
            .child(
                div().absolute().left(px(40.)).top(px(36.)).w(px(160.)).text_color(rgb(0xffffff)).child(
                    gpui_component::text::TextView::markdown(
                        "menus-link",
                        "[Filo](https://example.com) link",
                        window,
                        cx,
                    )
                    .selectable(true),
                ),
            )
            // Two rows whose menus have the same id (no id of their own around them).
            .child(
                div()
                    .absolute()
                    .left(px(40.))
                    .top(px(360.))
                    .w(px(320.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().h(px(32.)).bg(rgb(0x2c2c2e)).context_menu(menu(built, "row-a", &["Row A"])))
                    .child(div().h(px(32.)).bg(rgb(0x2c2c2e)).context_menu(menu(built, "row-b", &["Row B"]))),
            )
            // The row behind the menus: a hover colour, a tooltip, a click and the scroll wheel, each noted.
            .child({
                let [left, top, width, height] = ROW;
                let (hover, tip, click) = (self.behind.clone(), self.behind.clone(), self.behind.clone());
                div()
                    .id("menus-behind")
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(8.))
                    .bg(rgb(0x2c2c2e))
                    .hover(|style| style.bg(rgb(0x3a5a8a)))
                    .cursor_pointer()
                    .overflow_y_scroll()
                    .track_scroll(&self.behind.scroll)
                    .on_hover(move |hovered, _, _| hover.hovered.set(*hovered))
                    .tooltip(move |window, cx| {
                        tip.tip.set(true);
                        gpui_component::tooltip::Tooltip::new("The row behind").build(window, cx)
                    })
                    .on_click(move |_, _, _| click.clicked.set(true))
                    .child(div().h(px(height * 4.)))
            })
            .child(
                div().absolute().left(px(420.)).top(px(200.)).child(
                    Button::new("menus-view").label("View").dropdown_menu(menu(built, "view", &["Small", "Large"])),
                ),
            )
            // The web view, and a strip above it whose menu opens over it.
            .when_some(self.web.clone(), |this, web| {
                let (built, clicked) = (built.clone(), self.web_item.clone());
                this.child(div().absolute().left(px(380.)).top(px(410.)).w(px(240.)).h(px(60.)).child(web)).child(
                    div().absolute().left(px(380.)).top(px(384.)).w(px(240.)).h(px(22.)).child(
                        div().size_full().bg(rgb(0x2c2c2e)).context_menu(move |menu, _, _| {
                            built.borrow_mut().push("web");
                            let clicked = clicked.clone();
                            menu.item(PopupMenuItem::new("Open").on_click(move |_, _, _| clicked.set(true)))
                                .item(PopupMenuItem::new("Copy link"))
                        }),
                    ),
                )
            })
            .when(self.popup_open, |this| {
                this.child(
                    div()
                        .id("menus-popup")
                        .absolute()
                        .left(px(420.))
                        .top(px(300.))
                        .w(px(180.))
                        .h(px(80.))
                        .rounded(px(8.))
                        .bg(rgb(0x48484a))
                        // Not `occlude()`: like the app's own popups (the chat's reaction picker), its `surface` keeps
                        // the pointer from what's under it.
                        .child(one_menu::surface())
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.set_popup(false, cx))),
                )
            })
    }
}

/// What a check reads after a step: how many menus drew, the last one made, and whether the popup shows.
#[derive(Debug, PartialEq)]
struct Seen {
    drawn: usize,
    last: Option<&'static str>,
    popup: bool,
}

async fn drive(host: Entity<Host>, cx: &mut AsyncWindowContext) -> usize {
    let executor = cx.background_executor().clone();
    let width = f64::from(WIDTH);
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok();
    let mut failures = 0;
    executor.timer(Duration::from_millis(800)).await;

    let check = async |cx: &mut AsyncWindowContext, step: &str, want: Seen| {
        executor.timer(SETTLE).await;
        let seen = cx
            .update(|window, cx| {
                let host = host.read(cx);
                Seen {
                    drawn: one_menu::drawn(window, cx),
                    last: host.built.borrow().last().copied(),
                    popup: host.popup_open,
                }
            })
            .expect("window");
        let ok = seen == want;
        println!(
            "menus: {} {step}: {seen:?}{}",
            if ok { "PASS" } else { "FAIL" },
            if ok { String::new() } else { format!(", wanted {want:?}") }
        );
        if let (Some(dir), Some(number)) = (&shots, native_input::window_number(width)) {
            let name: String =
                step.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
            crate::window_shot::save(number, format!("{dir}/{name}.png"));
        }
        ok
    };
    let set_popup = |cx: &mut AsyncWindowContext, open: bool| {
        host.update(cx, |host, cx| host.set_popup(open, cx)).ok();
    };
    let esc = || native_input::key(width, "\u{1b}", 53, 0);
    // The wheel at a window point. `scroll_in` lands it off by the window's place on screen in this pop-up window
    // (x right of it by its left, y above by its top), so it's asked for that much the other way.
    let wheel = |cx: &mut AsyncWindowContext, x: f32, y: f32| {
        let origin = cx.update(|window, _| window.bounds().origin).expect("window");
        native_input::scroll_in(width, f64::from(x - f32::from(origin.x)), f64::from(y + f32::from(origin.y)), 20.);
    };
    let mut tally = |ok: bool| failures += usize::from(!ok);

    // The screenshot's case: a right-click where the badge covers the picture opens the badge's menu only.
    native_input::right_click(width, 300., 112.);
    tally(
        check(
            cx,
            "right-click where the badge covers the picture",
            Seen { drawn: 1, last: Some("badge"), popup: false },
        )
        .await,
    );
    // Another right-click, on the picture: its menu replaces the badge's.
    native_input::right_click(width, 100., 260.);
    tally(
        check(
            cx,
            "right-click the picture with the badge's menu open",
            Seen { drawn: 1, last: Some("tile"), popup: false },
        )
        .await,
    );
    // A right-click on a link with the picture's menu open: the link's menu alone.
    native_input::right_click(width, 52., 48.);
    tally(
        check(
            cx,
            "right-click a link with the picture's menu open",
            Seen { drawn: 1, last: Some("tile"), popup: false },
        )
        .await,
    );
    native_input::right_click(width, 100., 260.);
    tally(
        check(
            cx,
            "right-click the picture with the link's menu open",
            Seen { drawn: 1, last: Some("tile"), popup: false },
        )
        .await,
    );
    // The scroll wheel over the menu leaves it; anywhere else closes it.
    wheel(cx, 110., 272.);
    tally(check(cx, "scroll over the open menu", Seen { drawn: 1, last: Some("tile"), popup: false }).await);
    wheel(cx, 520., 140.);
    tally(check(cx, "scroll elsewhere", Seen { drawn: 0, last: Some("tile"), popup: false }).await);
    // Rows sharing a menu id: one menu, not one per row.
    native_input::right_click(width, 200., 376.);
    tally(
        check(
            cx,
            "right-click a row whose menu id its neighbour shares",
            Seen { drawn: 1, last: Some("row-a"), popup: false },
        )
        .await,
    );
    // The app's own popup opening (not by a click) closes the open right-click menu.
    set_popup(cx, true);
    tally(
        check(cx, "the app's popup opens over a right-click menu", Seen { drawn: 1, last: Some("row-a"), popup: true })
            .await,
    );
    // Esc closes it.
    esc();
    tally(check(cx, "Esc", Seen { drawn: 0, last: Some("row-a"), popup: false }).await);
    // The dropdown; then the popup opening closes it.
    native_input::click(width, 445., 214.);
    tally(check(cx, "the View dropdown", Seen { drawn: 1, last: Some("view"), popup: false }).await);
    set_popup(cx, true);
    tally(
        check(cx, "the app's popup opens over the dropdown", Seen { drawn: 1, last: Some("view"), popup: true }).await,
    );
    // A right-click with the popup open: the picture's menu alone.
    native_input::right_click(width, 100., 260.);
    tally(
        check(cx, "right-click the picture with the popup open", Seen { drawn: 1, last: Some("tile"), popup: false })
            .await,
    );
    // The dropdown with a right-click menu open: the dropdown alone.
    native_input::click(width, 445., 214.);
    tally(
        check(
            cx,
            "the View dropdown with the picture's menu open",
            Seen { drawn: 0, last: Some("tile"), popup: false },
        )
        .await,
    );
    native_input::click(width, 445., 214.);
    tally(check(cx, "the View dropdown again", Seen { drawn: 1, last: Some("view"), popup: false }).await);
    esc();
    tally(check(cx, "Esc on the dropdown", Seen { drawn: 0, last: Some("view"), popup: false }).await);
    // A menu over the web view: a hole in it where the menu is, and a click on its item there is the menu's.
    let web = host.read_with(cx, |host, _| host.web.clone()).ok().flatten();
    let web_holes = |cx: &mut AsyncWindowContext| {
        web.as_ref().map(|web| web.read_with(cx, |web, _| crate::web_holes::holes(web.raw())).unwrap_or(0))
    };
    native_input::right_click(width, 400., 395.);
    tally(
        check(cx, "right-click the strip above the web view", Seen { drawn: 1, last: Some("web"), popup: false }).await,
    );
    let holes = web_holes(cx);
    println!(
        "menus: {} the web view has a hole for the menu over it: {holes:?}",
        if holes == Some(1) { "PASS" } else { "FAIL" }
    );
    tally(holes == Some(1));
    native_input::click(width, 430., 416.);
    tally(
        check(cx, "click the menu's item over the web view", Seen { drawn: 0, last: Some("web"), popup: false }).await,
    );
    let clicked = host.read_with(cx, |host, _| host.web_item.get()).unwrap_or(false);
    println!("menus: {} the item over the web view took the click: {clicked}", if clicked { "PASS" } else { "FAIL" });
    tally(clicked);
    executor.timer(SETTLE).await;
    let holes = web_holes(cx);
    println!(
        "menus: {} the web view's hole closed with the menu: {holes:?}",
        if holes == Some(0) { "PASS" } else { "FAIL" }
    );
    tally(holes == Some(0));

    // Behind a menu: the pointer on one is the menu's alone. The row under it doesn't hover, show its tooltip, scroll or
    // take the click, whichever kind of menu it is.
    let heard = |cx: &mut AsyncWindowContext, step: &str, want: Heard| {
        let seen = host.read_with(cx, |host, _| host.behind.heard()).expect("window");
        let ok = seen == want;
        println!(
            "menus: {} {step}, the row behind: {seen:?}{}",
            if ok { "PASS" } else { "FAIL" },
            if ok { String::new() } else { format!(", wanted {want:?}") }
        );
        ok
    };
    let reset = |cx: &mut AsyncWindowContext| host.read_with(cx, |host, _| host.behind.reset()).ok();
    // Off the row and every menu.
    let away = || native_input::hover(width, 300., 450.);
    // The middle of where the open menu covers the row.
    let over_row = |cx: &mut AsyncWindowContext| -> Option<Point<Pixels>> {
        cx.update(|window, cx| {
            one_menu::boxes(window, cx)
                .iter()
                .map(|menu| menu.intersect(&row()))
                .find(|both| both.size.width > px(16.) && both.size.height > px(16.))
                .map(|both| both.center())
        })
        .ok()
        .flatten()
    };
    // The pointer to `at`, a wait for a tooltip, then the scroll wheel there.
    let point_and_scroll = async |cx: &mut AsyncWindowContext, at: Point<Pixels>| {
        native_input::hover(width, f64::from(f32::from(at.x)), f64::from(f32::from(at.y)));
        executor.timer(TIP_WAIT).await;
        wheel(cx, f32::from(at.x), f32::from(at.y));
    };
    let click_at =
        |at: Point<Pixels>| native_input::click(width, f64::from(f32::from(at.x)), f64::from(f32::from(at.y)));

    // With nothing open the row hears each of them.
    let bare = gpui::point(px(400.), px(260.));
    point_and_scroll(cx, bare).await;
    click_at(bare);
    tally(
        check(
            cx,
            "the pointer, the scroll wheel and a click on the row behind, nothing open",
            Seen { drawn: 0, last: Some("web"), popup: false },
        )
        .await,
    );
    tally(heard(cx, "nothing open", Heard { hovered: true, tip: true, clicked: true, scrolled: true }));
    away();
    // The View dropdown over it.
    native_input::click(width, 445., 214.);
    reset(cx);
    tally(check(cx, "the View dropdown over the row", Seen { drawn: 1, last: Some("view"), popup: false }).await);
    match over_row(cx) {
        Some(at) => {
            point_and_scroll(cx, at).await;
            tally(
                check(
                    cx,
                    "the pointer and the scroll wheel on the View dropdown over the row",
                    Seen { drawn: 1, last: Some("view"), popup: false },
                )
                .await,
            );
            tally(heard(cx, "the pointer and the scroll wheel on the View dropdown", NOTHING));
            click_at(at);
            tally(
                check(
                    cx,
                    "a click on the View dropdown's item over the row",
                    Seen { drawn: 0, last: Some("view"), popup: false },
                )
                .await,
            );
            tally(heard(cx, "a click on the View dropdown's item", NOTHING));
        }
        None => {
            println!("menus: FAIL the View dropdown doesn't cover the row");
            tally(false);
        }
    }
    away();
    // The app's own popup over it.
    set_popup(cx, true);
    reset(cx);
    tally(check(cx, "the app's popup over the row", Seen { drawn: 1, last: Some("view"), popup: true }).await);
    match over_row(cx) {
        Some(at) => {
            point_and_scroll(cx, at).await;
            click_at(at);
            tally(
                check(
                    cx,
                    "the pointer, the scroll wheel and a click on the app's popup over the row",
                    Seen { drawn: 1, last: Some("view"), popup: true },
                )
                .await,
            );
            tally(heard(cx, "the pointer, the scroll wheel and a click on the app's popup", NOTHING));
        }
        None => {
            println!("menus: FAIL the app's popup doesn't cover the row");
            tally(false);
        }
    }
    esc();
    away();
    // The picture's right-click menu over it.
    native_input::right_click(width, 300., 300.);
    reset(cx);
    tally(
        check(cx, "the picture's right-click menu over the row", Seen { drawn: 1, last: Some("tile"), popup: false })
            .await,
    );
    match over_row(cx) {
        Some(at) => {
            point_and_scroll(cx, at).await;
            tally(
                check(
                    cx,
                    "the pointer and the scroll wheel on the right-click menu over the row",
                    Seen { drawn: 1, last: Some("tile"), popup: false },
                )
                .await,
            );
            tally(heard(cx, "the pointer and the scroll wheel on the right-click menu", NOTHING));
        }
        None => {
            println!("menus: FAIL the right-click menu doesn't cover the row");
            tally(false);
        }
    }
    esc();
    away();
    tally(check(cx, "Esc on the right-click menu", Seen { drawn: 0, last: Some("tile"), popup: false }).await);
    failures
}

// `FILO_OPEN=markdown-compare`: one of Filo's chat replies drawn the way the chat draws it now and with a candidate
// typography for messages, in a window of its own with no backend, so the owner can judge screenshots before
// anything ships. Nothing here is saved: the saved interface font and size stay as they are.
//
// `FILO_COMPARE=now|candidate` draws one style (both, stacked, by default; `page`: on the page the way a coding Result
// draws it, no bubble; `group`: the Group's, whose @names are links drawn as names), `FILO_COMPARE_SAMPLE=extras` a
// reply with a heading, a code block and a table instead (`edges`: inline code's hard cases, `tables`: a coding Result's
// table with long cells, `table-edges`: long words, alignment and more columns than fit, `links`: links, bold, line
// breaks and commit hashes, `wrap`: code and links at the ends of lines), `FILO_COMPARE_MODE=light|dark` the
// appearance, `FILO_WINDOW=WxH` the window size. `FILO_FIXTURE_SHOTS=<dir>` saves a window screenshot there (named for
// the style, sample and mode) and quits. With it, `FILO_COMPARE_DRIVE` posts the window AppKit input before more shots,
// without moving the real pointer: `scroll@X,Y` a sideways then a vertical wheel at that point (a wide table scrolls
// sideways; the vertical one scrolls the page, whose offset is printed), `select@X1,Y1:X2,Y2` a drag from one point to
// the other (text selection), `held@X1,Y1:X2,Y2` the same drag with its release missed (the window never hears the
// button come up), `quick@X1,Y1:X2,Y2` the drag posted all at once, so it ends before a frame draws, `hover@X,Y` the pointer moved there with no button down, `click@X,Y`, `dblclick@X,Y` and `rclick@X,Y` a click,
// a double-click and a right-click (a link's menu; what a click then copies is printed); steps joined with `;` run in turn. The "now" style has the chat's commit links and Quote button: a link clicked here, and the text the Quote
// button would put in the composer, are printed instead of opened.

#![allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare
use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window, WindowBounds,
    WindowOptions, div, px, relative, size,
};
use gpui_component::text::{InlineCodeStyle, TextView, TextViewStyle};

use crate::config;
use crate::smoke::native_input;
use crate::theme::{self, ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "markdown-compare")
}

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Now,
    Candidate,
    Page,
    Group,
}

// A reply as another app renders it in a reference screenshot, so the two can be put side by side. Fixture text only.
const REPLY: &str = "Syncing should be reliable again. The fixes are in the last two commits, and the app is running the new build.

### What was wrong

- **No progress in the thread (your screenshot):** the server was a version behind the app. It turned down every progress update the sync sent, about 900 in an hour, so threads only got the final result. I updated the server and the updates came through again. Progress from runs that finished before the fix is lost, including the one in your screenshot.
- **Export failed with `PAGE_SIZE_LIMIT is not defined`:** the exporter read a setting that a half-finished change had renamed. \"Export the weekly report\" failed on that and is still failed. I haven't run it again; you can start it from the Reports page.
- **One error ended a run for good:** a busy server, a full disk, or a timeout failed the run immediately.
- **Retries saved duplicates:** a run that was retried saved its rows a second time, in `a41c2e09` and `7d3b90f1` \"Import March invoices\". The totals are right, so I left them.

### What I changed

- **Exports from the last release:** `export.sh` now runs the exporter from the last release, so a half-made change can't break it. `LATEST=1 scripts/export.sh` goes back to running the newest code.";

const EXTRAS: &str = "## Disk space on your Mac

You have **41 GB** free. Most of the rest is caches that are safe to clear:

| Folder | Size | Safe to clear |
|---|---|---|
| Xcode derived data | 18 GB | Yes |
| Cargo target folders | 12 GB | Yes, rebuilds take longer |
| Photos library | 9 GB | No |

### How to clear them

Run this in Terminal, then empty the Bin:

```
rm -rf ~/Library/Developer/Xcode/DerivedData
cargo clean --manifest-path apps/desktop/Cargo.toml
```

1. Close Xcode first.
2. Keep the `Photos` library: it holds your only copies.";

// Where small inline code is most likely to go wrong: code longer than a line, code that isn't plain ASCII, code in a
// link and in italics, one-character code and code next to punctuation.
const EDGES: &str = "Short: `x`, (`a`,`b`) and `v2.1`.

Long: `apps/desktop/vendor/gpui-component/src/text/format/markdown.rs::with_code_inside_and_a_long_name` then text.

Unicode: `café ✓` beside `cafe`, and a link: [`docs/PERFORMANCE.md`](https://example.com/perf).

*Italic with `code` inside* and **bold with `code` inside**.";

// A coding Result's table whose long cells were cut off on one line (a reported layout). Fixture text only.
const TABLES: &str = "**Read-only tests on dev** (counts only, as Filo reported them)

| App | Result |
|---|---|
| Docs (2 accounts) | Works. Search found 1 and 5 files (the second with a next page). Reads returned 1,503 and 48 characters. |
| Drive (1 account) | Works. Search returned 10 files with a next page. A Google Sheet (3,085 characters) and a JSON file were read. |
| Gmail (3 accounts) | Works. Search returned 5 threads each, and reading the newest message worked in all three accounts. |
| ClickUp | Works. 61 tools found, and reading the workspace returned 8 spaces. A bad argument gave a clear error. |
| Sheets (1 account) | Search worked but found 0 spreadsheets, so no reads were tested. The broken-query check failed as expected. |
| Slides (1 account) | Search worked but found 0 decks, so no reads were tested. |
| Calendar | 2 calendars, but 0 events in the past 30 days or the next 7. |
| Contacts | 0 contacts. |";

// Tables' hard cases: long URLs and code, aligned columns, and more columns than fit, which scroll sideways.
const TABLE_EDGES: &str = "Long words:

| File | Where | Note |
|:--|:-:|--:|
| `apps/desktop/vendor/gpui-component/src/text/node.rs` | https://github.com/longbridge/gpui-component/blob/main/crates/ui/src/text/node.rs#L1031 | right |
| `a24df51d` | centred | 1,503 |

Too many columns:

| Day | Morning | Midday | Afternoon | Evening | Night | Notes |
|---|---|---|---|---|---|---|
| Monday | Standup with the team | Lunch with Sam | Deep work on the desktop table renderer | Gym | Read | Remember the dentist on Tuesday |
| Tuesday | Dentist | Errands | Review pull requests | Dinner | Sleep early | |";

// Links, bold and line breaks around them, and commit hashes: what must stay clickable and keep its spacing. Fixture
// text only.
const LINKS: &str =
    "See [the performance notes](https://example.com/perf) first, then **the runner** and *the app* after it.

From: someone@example.com  
To: **Filo**  
Re: https://example.com/a-long-path, done.

Pushed `fd806ce6` and b2bc0a22 to main; v1-8d40428f and 1234567 are not commits.

**Bold with [a link](https://example.com/inside) inside** stays a link.";

// Code chips and links at the ends of lines, with punctuation right after them (a reported layout: the `)` after
// `0959ce6` went to the next line on its own, under a stray piece of the link's underline). Try it at several window
// widths. Fixture text only.
const WRAP: &str = "Both are live at **https://filo-desktop.pages.dev/** (`f6d565a`, `0959ce6`), according to Claude Code. The hero now shows **\"3 tasks are working\"** in the header, with its popover open.

Pushed 0959ce6 and f6d565a (both on main), then [the notes](https://example.com/notes)) and [`docs/PERFORMANCE.md`](https://example.com/perf); is `fd806ce6`? Ask [@Builder](mention:1/builder)! \"`b2bc0a22`\" was reverted (see `a24df51d`).

A long one wraps inside its chip: `apps/desktop/vendor/gpui-component/src/text/format/markdown.rs::with_code_inside_and_a_long_name`) and then a [link whose words run over the end of a line](https://example.com/long) (done).";

pub fn open(cx: &mut App) {
    // Links are printed, not opened (commit_links.rs's handler would open GitHub).
    gpui_component::text::safety::set_link_handler(cx, |url, _, _| {
        eprintln!("markdown-compare: opened {url}");
        true
    });
    let mut prefs = config::load_prefs();
    if let Ok(mode) = std::env::var("FILO_COMPARE_MODE") {
        prefs.theme_mode = mode;
    }
    let mode = prefs.theme_mode.clone();
    // Applied, never saved.
    theme::apply(prefs, None, cx);
    let variants = match std::env::var("FILO_COMPARE").as_deref() {
        Ok("now") => vec![Variant::Now],
        Ok("candidate") => vec![Variant::Candidate],
        Ok("page") => vec![Variant::Page],
        Ok("group") => vec![Variant::Group],
        _ => vec![Variant::Now, Variant::Candidate],
    };
    let sample = std::env::var("FILO_COMPARE_SAMPLE").unwrap_or_default();
    let (sample_name, text) = match sample.as_str() {
        "extras" => ("extras", EXTRAS),
        "edges" => ("edges", EDGES),
        "tables" => ("tables", TABLES),
        "table-edges" => ("table-edges", TABLE_EDGES),
        "links" => ("links", LINKS),
        "wrap" => ("wrap", WRAP),
        _ => ("reply", REPLY),
    };
    let (width, height) = crate::test_run::window_size((760., 760.));
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Filo markdown compare".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            // Above other windows without taking the focus: the screenshot is taken while the Mac is in use.
            kind: gpui::WindowKind::PopUp,
            focus: false,
            ..Default::default()
        },
        |window, cx| {
            let name = format!(
                "{}-{}-{}",
                match variants.as_slice() {
                    [Variant::Now] => "now",
                    [Variant::Candidate] => "candidate",
                    [Variant::Page] => "page",
                    [Variant::Group] => "group",
                    _ => "both",
                },
                sample_name,
                mode,
            );
            let page = ScrollHandle::new();
            if let Ok(shots) = std::env::var("FILO_FIXTURE_SHOTS") {
                let drive: Vec<Drive> = std::env::var("FILO_COMPARE_DRIVE")
                    .ok()
                    .map(|drive| drive.split(';').filter_map(Drive::parse).collect())
                    .unwrap_or_default();
                let page = page.clone();
                cx.spawn(async move |cx| {
                    let shot = |suffix: &str| {
                        if let Some(number) = native_input::window_number(f64::from(width)) {
                            let path = format!("{shots}/{name}{suffix}.png");
                            crate::window_shot::save(number, &path);
                            eprintln!("markdown-compare: shot {path}");
                        }
                    };
                    // Markdown is parsed off the main thread; give it and the fonts time to settle.
                    cx.background_executor().timer(Duration::from_millis(2500)).await;
                    shot("");
                    for (index, drive) in drive.into_iter().enumerate() {
                        let width = f64::from(width);
                        let pause = |ms| cx.background_executor().timer(Duration::from_millis(ms));
                        let report = |step: &str| {
                            shot(&format!("-{step}"));
                            eprintln!("markdown-compare: after {step}, page scrolled {:?}", page.offset());
                        };
                        match drive {
                            Drive::Scroll(at) => {
                                native_input::hover(width, at.0, at.1);
                                pause(200).await;
                                for _ in 0..3 {
                                    wheel(width, at.0, at.1, 80., 0.);
                                    pause(50).await;
                                }
                                pause(600).await;
                                report("sideways");
                                for _ in 0..2 {
                                    wheel(width, at.0, at.1, 0., 80.);
                                    pause(50).await;
                                }
                                pause(600).await;
                                report("vertical");
                            }
                            Drive::Select(from, to, release) => {
                                let paced = !matches!(release, Release::Quick);
                                native_input::hover(width, from.0, from.1);
                                pause(100).await;
                                native_input::drag_step(width, native_input::Drag::Down, from.0, from.1);
                                if paced {
                                    pause(100).await;
                                }
                                for step in 1..=10 {
                                    let t = f64::from(step) / 10.;
                                    native_input::drag_step(
                                        width,
                                        native_input::Drag::Move,
                                        from.0 + (to.0 - from.0) * t,
                                        from.1 + (to.1 - from.1) * t,
                                    );
                                    if paced {
                                        pause(30).await;
                                    }
                                }
                                if !matches!(release, Release::Missed) {
                                    native_input::drag_step(width, native_input::Drag::Up, to.0, to.1);
                                }
                                pause(600).await;
                                report(&format!(
                                    "{index}-{}",
                                    match release {
                                        Release::Up => "selected",
                                        Release::Missed => "held",
                                        Release::Quick => "quick-selected",
                                    }
                                ));
                            }
                            Drive::Hover(at) => {
                                native_input::hover(width, at.0, at.1);
                                pause(600).await;
                                report(&format!("{index}-hovered"));
                            }
                            Drive::Click(at) => {
                                native_input::click(width, at.0, at.1);
                                pause(600).await;
                                report(&format!("{index}-clicked"));
                                let copied = cx
                                    .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
                                    .ok()
                                    .flatten();
                                eprintln!("markdown-compare: clipboard {copied:?}");
                            }
                            Drive::RightClick(at) => {
                                native_input::right_click(width, at.0, at.1);
                                pause(600).await;
                                report(&format!("{index}-right-clicked"));
                            }
                            Drive::DoubleClick(at) => {
                                native_input::double_click(width, at.0, at.1);
                                pause(600).await;
                                report(&format!("{index}-double-clicked"));
                            }
                        }
                    }
                    let _ = cx.update(|cx| cx.quit());
                })
                .detach();
            }
            let compare = cx.new(|_| MarkdownCompare { variants, text, page });
            cx.new(|cx| gpui_component::Root::new(compare, window, cx))
        },
    )
    .expect("open window");
}

struct MarkdownCompare {
    variants: Vec<Variant>,
    text: &'static str,
    page: ScrollHandle,
}

/// `FILO_COMPARE_DRIVE`: input posted to the window, at window points from the top left.
enum Drive {
    Scroll((f64, f64)),
    Select((f64, f64), (f64, f64), Release),
    Hover((f64, f64)),
    Click((f64, f64)),
    RightClick((f64, f64)),
    DoubleClick((f64, f64)),
}

/// How a `Drive::Select` drag ends.
enum Release {
    /// The button comes up at the end, a frame or more after each step.
    Up,
    /// The window never hears it come up.
    Missed,
    /// Down, moves and up posted at once: it all reaches the app before the next frame.
    Quick,
}

impl Drive {
    fn parse(value: &str) -> Option<Self> {
        let at = |value: &str| {
            let (x, y) = value.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        };
        let (kind, points) = value.split_once('@')?;
        match kind {
            "scroll" => Some(Drive::Scroll(at(points)?)),
            "click" => Some(Drive::Click(at(points)?)),
            "dblclick" => Some(Drive::DoubleClick(at(points)?)),
            "rclick" => Some(Drive::RightClick(at(points)?)),
            "hover" => Some(Drive::Hover(at(points)?)),
            "select" | "held" | "quick" => {
                let (from, to) = points.split_once(':')?;
                let release = match kind {
                    "held" => Release::Missed,
                    "quick" => Release::Quick,
                    _ => Release::Up,
                };
                Some(Drive::Select(at(from)?, at(to)?, release))
            }
            _ => None,
        }
    }
}

/// A pixel wheel event on both axes at (x, y), window points from the top left, in the window `width` points wide: `dx`
/// right and `dy` down are positive. Unlike `native_input::scroll` it scrolls sideways too, and it goes straight to the
/// window's view: a posted wheel goes to whichever app's window is on top at that point, and the fixture's is behind.
fn wheel(width: f64, x: f64, y: f64, dx: f64, dy: f64) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Location {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Location,
        size: Location,
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
        fn CGEventSetLocation(event: *mut std::ffi::c_void, location: Location);
        fn CFRelease(value: *const std::ffi::c_void);
    }
    let Some(number) = native_input::window_number(width) else { return };
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let window: *mut Object = msg_send![app, windowWithWindowNumber: number];
        if window.is_null() {
            return;
        }
        let content: *mut Object = msg_send![window, contentView];
        let bounds: Rect = msg_send![content, bounds];
        // GPUI's view sits in the content view.
        let subviews: *mut Object = msg_send![content, subviews];
        let first: *mut Object = msg_send![subviews, firstObject];
        let view = if first.is_null() { content } else { first };
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        let primary: *mut Object = msg_send![screens, objectAtIndex: 0usize];
        let screen: Rect = msg_send![primary, frame];
        let cg = CGEventCreateScrollWheelEvent(std::ptr::null(), 0, 2, -dy as i32, -dx as i32);
        if cg.is_null() {
            return;
        }
        // A window-less event's `locationInWindow` is its screen point, bottom-left origin: put that on the window point.
        CGEventSetLocation(cg, Location { x, y: screen.size.y - (bounds.size.y - y) });
        let event: *mut Object = msg_send![class!(NSEvent), eventWithCGEvent: cg];
        CFRelease(cg);
        if !event.is_null() {
            let _: () = msg_send![view, scrollWheel: event];
        }
    }
}

impl Render for MarkdownCompare {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::set_content_width(f32::from(window.viewport_size().width));
        let colors = *cx.colors();
        let mut column = div()
            .w_full()
            .font_family(cx.theme().font_family())
            .pt(px(36.))
            .pb(px(space::XXL))
            .flex()
            .flex_col()
            .gap(px(space::XXL));
        for variant in self.variants.clone() {
            let label = match variant {
                Variant::Now => {
                    format!("Now: {} {} px, bold 700, boxed code", cx.theme().font_family(), cx.theme().prefs.font_size)
                }
                Variant::Candidate => format!(
                    "Candidate: SF Pro {} px, semibold, small code chips, indented bullets",
                    cx.theme().prefs.font_size
                ),
                Variant::Page => "Page: a coding Result's markdown".to_string(),
                Variant::Group => "Group: @names drawn as names".to_string(),
            };
            let key = match variant {
                Variant::Now => "md-now",
                Variant::Candidate => "md-candidate",
                Variant::Page => "md-page",
                Variant::Group => "md-group",
            };
            let view = TextView::markdown(key, self.text, window, cx).selectable(true);
            let body = match variant {
                // The chat's (conversation.rs `render_message`): commit links and the Quote button.
                Variant::Now => div().callout().child(
                    view.style(ui::markdown_style().commit_links(true)).selection_actions(|selection, _, _| {
                        // Quotes as it goes down, as the chat's does.
                        div()
                            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                eprintln!("markdown-compare: quoted {:?}", selection.text(cx));
                                selection.clear(cx);
                            })
                            .child(ui::Button::secondary("quote-selection", "Quote").small().icon("reply"))
                    }),
                ),
                Variant::Candidate => candidate_body(div(), cx).child(view.style(candidate_style(cx))),
                // The Group's (conversation.rs `render_message` with `mention_markdown`'s `mention:` links).
                Variant::Group => div()
                    .callout()
                    .child(view.style(ui::markdown_style().commit_links(true).mention_links(colors.accent))),
                // coding_result.rs `render_result`: straight on the page's column.
                Variant::Page => {
                    column = column.child(
                        div().w_full().flex().justify_center().child(
                            div()
                                .w_full()
                                .max_w(px(super::coding_result::PAGE_WIDTH))
                                .px(px(theme::PAGE_PADDING))
                                .flex()
                                .flex_col()
                                .gap(px(space::SM))
                                .child(div().footnote().text_color(colors.text_secondary).child(label))
                                .child(view.style(ui::markdown_page_style().untrusted(true))),
                        ),
                    );
                    continue;
                }
            };
            // The chat's assistant bubble (conversation.rs `render_message`), on its page column.
            let bubble = div()
                .max_w(relative(0.86))
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .rounded(px(16.))
                .bg(colors.assistant_bubble)
                .border_1()
                .border_color(colors.border)
                .text_color(colors.on_assistant_bubble)
                .px(px(12.))
                .py(px(7.))
                .child(body);
            column = column.child(
                div().w_full().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(theme::MAX_CONTENT_WIDTH))
                        .px(px(theme::PAGE_PADDING))
                        .flex()
                        .flex_col()
                        .gap(px(space::SM))
                        .child(div().footnote().text_color(colors.text_secondary).child(label))
                        .child(div().flex().child(bubble)),
                ),
            );
        }
        // A scrolling page like the Result view's, so a vertical wheel over a wide table can be checked. Toasts show
        // over it, as in the app's window (a link's Copy link says "Copied").
        let page = div()
            .id("compare-page")
            .size_full()
            .bg(colors.chat_background)
            .overflow_y_scroll()
            .track_scroll(&self.page)
            .child(column);
        div().size_full().relative().child(page).children(gpui_component::Root::render_notification_layer(window, cx))
    }
}

/// The candidate's body text, after the owner's reference: SF Pro at the chosen size, softer than the headings and code,
/// with more room between lines (1.55 instead of 1.41).
fn candidate_body(div: gpui::Div, cx: &App) -> gpui::Div {
    let body = cx.theme().prefs.font_size;
    div.font_family(".SystemUIFont")
        .text_size(px(body))
        .line_height(px((body * 1.55).round()))
        .font_weight(FontWeight::NORMAL)
        .text_color(cx.colors().on_assistant_bubble.opacity(0.84))
}

/// The candidate's markdown, after the owner's reference: inline code drawn smaller than the text (about 0.8) on a
/// tinted, outlined chip, semibold lead-ins and headings at about the text's size, round bullets in their own column,
/// and paragraph, heading and list rhythm measured from the reference.
fn candidate_style(cx: &App) -> TextViewStyle {
    let colors = *cx.colors();
    let body = cx.theme().prefs.font_size;
    let (mono, mono_size) = (cx.theme().mono_family(), cx.theme().mono_size());
    // gpui 0.2 refines an element's text style as one value, so these name the font and size again, not just what differs.
    let code_block = StyleRefinement::default()
        .font_family(mono)
        .text_size(px(mono_size))
        .line_height(relative(1.55))
        .bg(colors.text.opacity(0.05))
        .border_color(colors.border)
        .rounded(px(radius::XS))
        .px(px(space::MD))
        .py(px(space::SM + 2.));
    TextViewStyle::default()
        .paragraph_gap(gpui::rems(0.65))
        .heading(move |level| {
            let size = (body
                * match level {
                    1 => 1.25,
                    2 => 1.12,
                    _ => 1.,
                })
            .round();
            StyleRefinement::default()
                .text_size(px(size))
                .line_height(px((size * 1.55).round()))
                .font_weight(FontWeight::SEMIBOLD)
        })
        .bold_weight(FontWeight::SEMIBOLD)
        .inline_code(InlineCodeStyle {
            background: colors.accent.opacity(0.12),
            border: Some(colors.accent.opacity(0.32)),
            font_family: None,
            color: Some(colors.on_assistant_bubble),
            compact: true,
        })
        .list_gap(gpui::rems(0.2))
        .list_indent(gpui::rems(1.35))
        .code_in_styled_text(true)
        .code_block(code_block)
}

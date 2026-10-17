// `FILO_OPEN=chat-composer`: the composer's text row (composer_layout.rs) on a made-up chat, in a window of its own (no
// backend). Types up to the wrap point and past it with real key presses, adds a line, puts in a long
// text, deletes back to one line and sends, checking at each step which layout shows, that it doesn't flip at the
// wrap point, that the height glides (sampled every few milliseconds) and stops at about 40% of the window, and that
// a long text scrolls inside with its last line (the caret's) in sight. Then a long reply draft made of quotes and
// answers: typing and pasting at its end keep the caret's line at the box's bottom, and the wheel over the box scrolls
// only its text, never the chat behind it. Prints a line per check and quits, exit 1 when one fails. `FILO_FIXTURE_SHOTS=<dir>` saves a screenshot at each step.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, AsyncWindowContext, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Window, div,
};
use serde_json::{Value, json};

use super::{Conversation, Detail};

/// The window's width, which is how the key presses find it.
const WIDTH: f64 = 780.;
const HEIGHT: f32 = 700.;
/// One row: the 28-point buttons and text with 4 points around them.
const ONE_ROW: f32 = 36.;

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-composer")
}

pub fn open(cx: &mut App) {
    crate::smoke::fixtures::open(
        cx,
        "chat-composer",
        "Filo composer",
        (WIDTH as f32, HEIGHT),
        false,
        |_, cx| cx.new(|_| Host { shown: None }),
        drive,
    );
}

struct Host {
    shown: Option<Entity<Conversation>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.shown.clone())
    }
}

fn opening() -> Vec<Value> {
    let base = crate::format::now_ms() - 30. * 60_000.;
    // Enough of them that the chat scrolls behind a tall composer.
    (0..14)
        .map(|index| {
            let user = index % 2 == 0;
            let content = if user { format!("Question {index}?") } else { format!("Reply {index}, long enough to wrap over a couple of lines of the bubble so rows aren't all one height.") };
            json!({ "_id": format!("m{index}"), "_creationTime": base + index as f64 * 60_000., "role": if user { "user" } else { "assistant" }, "content": content })
        })
        .collect()
}

/// What the composer shows, read between frames.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    stacked: bool,
    /// The row's height as drawn now.
    height: f32,
    /// Lines the input shows (past them it scrolls), and how far it's scrolled.
    rows: usize,
    scroll: f32,
    text_chars: usize,
}

struct Driver<'a> {
    view: Entity<Conversation>,
    cx: &'a mut AsyncWindowContext,
    failures: usize,
    shots: Option<String>,
    log: Vec<String>,
}

impl Driver<'_> {
    async fn wait(&mut self, ms: u64) {
        self.cx.background_executor().timer(Duration::from_millis(ms)).await;
    }

    /// Long enough for any glide to end.
    async fn settle(&mut self) {
        self.wait(320).await;
    }

    fn shot(&mut self, name: &str) {
        crate::smoke::fixtures::shot(self.shots.as_deref(), WIDTH, name);
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        let line =
            if ok { format!("chat-composer: {name}: ok") } else { format!("chat-composer: {name}: FAILED {detail:?}") };
        if !ok {
            self.failures += 1;
        }
        println!("{line}");
        self.log.push(line);
    }

    fn shown(&mut self) -> Shown {
        self.view
            .read_with(self.cx, |view, cx| {
                let input = view.input.read(cx);
                Shown {
                    stacked: view.composer_layout.stacked(),
                    height: view.composer_layout.height_now(),
                    rows: input.shown_rows(),
                    scroll: f32::from(input.scroll_offset().y),
                    text_chars: input.value().chars().count(),
                }
            })
            .expect("view")
    }

    /// Each frame's height for `ms` milliseconds, sampled every 4.
    async fn heights(&mut self, ms: u64) -> Vec<(u64, f32)> {
        let start = Instant::now();
        let mut samples: Vec<(u64, f32)> = Vec::new();
        while start.elapsed() < Duration::from_millis(ms) {
            let height = self.shown().height;
            if samples.last().is_none_or(|(_, last)| (last - height).abs() > 0.01) {
                samples.push((start.elapsed().as_millis() as u64, height));
            }
            self.wait(4).await;
        }
        samples
    }

    /// The heights from one value to another: in steps (more than a jump), one way only, over 100–260 ms.
    fn glided(&mut self, what: &str, samples: &[(u64, f32)], from: f32, to: f32) {
        let heights: Vec<f32> = samples.iter().map(|(_, height)| *height).collect();
        let one_way = heights
            .windows(2)
            .all(|pair| if to > from { pair[1] >= pair[0] - 0.01 } else { pair[1] <= pair[0] + 0.01 });
        let steps =
            heights.iter().filter(|height| (**height - from).abs() > 0.5 && (**height - to).abs() > 0.5).count();
        let ends = heights.last().is_some_and(|last| (last - to).abs() < 0.5);
        let took = samples.iter().find(|(_, height)| (height - to).abs() < 0.5).map(|(at, _)| *at).unwrap_or(u64::MAX);
        self.log.push(format!(
            "  {what}: {}",
            samples.iter().map(|(at, height)| format!("{at}ms {height:.1}")).collect::<Vec<_>>().join(", ")
        ));
        self.check(
            &format!("{what}: glides from {from:.0} to {to:.0} in steps, one way, in about 180 ms"),
            one_way && steps >= 3 && ends && (100..=280).contains(&took),
            samples,
        );
    }

    /// Runs `f` on the input (`set_value` leaves the caret at the start: empty it and `insert` to type at the end).
    fn update_input(
        &mut self,
        f: impl FnOnce(
            &mut gpui_component::input::InputState,
            &mut Window,
            &mut gpui::Context<gpui_component::input::InputState>,
        ) + 'static,
    ) {
        let view = self.view.clone();
        self.cx
            .update(|window, cx| view.update(cx, |view, cx| view.input.update(cx, |input, cx| f(input, window, cx))))
            .ok();
    }

    /// Messages the scripted backend was given (send_fixture.rs).
    fn delivered(&mut self) -> usize {
        self.cx.update(|_, cx| super::send_fixture::delivered_count(cx)).unwrap_or(0)
    }

    /// The caret's line from the input's top, and the input's height, as last drawn.
    fn caret(&mut self) -> Option<(f32, f32, f32)> {
        self.view
            .read_with(self.cx, |view, cx| {
                view.input
                    .read(cx)
                    .caret_line()
                    .map(|(line, height)| (f32::from(line.start), f32::from(line.end), f32::from(height)))
            })
            .ok()
            .flatten()
    }

    /// Checks the caret's line is in sight, and with the text scrolled, the box's last line.
    fn caret_at_bottom(&mut self, what: &str) {
        let caret = self.caret();
        let shown = self.shown();
        let ok = caret.is_some_and(|(top, bottom, height)| {
            top >= -0.5 && bottom <= height + 0.5 && (shown.scroll > -0.5 || (height - bottom).abs() <= 1.)
        });
        self.check(&format!("{what}: the caret's line in sight, at the bottom"), ok, (caret, shown));
    }

    async fn type_text(&mut self, text: &str) {
        for key in text.chars() {
            if key == '\n' {
                crate::smoke::native_input::key(WIDTH, "\r", 36, 1 << 17);
                self.wait(30).await;
            } else {
                self.type_key(&key.to_string()).await;
            }
        }
    }

    async fn type_key(&mut self, characters: &str) {
        crate::smoke::native_input::key(WIDTH, characters, 0, 0);
        self.wait(30).await;
    }

    async fn backspace(&mut self) {
        crate::smoke::native_input::key(WIDTH, "\u{7f}", 51, 0);
        self.wait(30).await;
    }
}

async fn drive(host: Entity<Host>, cx: &mut AsyncWindowContext) -> usize {
    let view = cx
        .update(|window, cx| {
            let view = cx.new(|cx| Conversation::new(None, "Filo".into(), window, cx));
            view.update(cx, |view, _| {
                view.fixture = true;
                view.session_id = Some("composer-chat".into());
            });
            view
        })
        .expect("window");
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok();
    if let Some(dir) = &shots {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut driver = Driver { view: view.clone(), cx, failures: 0, shots, log: Vec::new() };
    let d = &mut driver;
    host.update(d.cx, |host, cx| {
        host.shown = Some(view.clone());
        cx.notify();
    })
    .ok();
    let detail: Detail = serde_json::from_value(
        json!({ "session": { "_id": "composer-chat", "title": "Composer" }, "messages": opening(), "hasMore": false }),
    )
    .expect("detail");
    d.view
        .update(d.cx, |view, cx| {
            view.detail = Some(Ok(Some(detail)));
            view.data_changed(cx);
        })
        .ok();
    d.update_input(|input, window, cx| input.focus(window, cx));
    d.settle().await;

    // 1. Empty and short: one slim row.
    let empty = d.shown();
    d.check("empty: one row", !empty.stacked && (empty.height - ONE_ROW).abs() < 0.5, &empty);
    d.shot("1-empty");
    for key in "Hello".chars() {
        d.type_key(&key.to_string()).await;
    }
    d.settle().await;
    let short = d.shown();
    // The window takes the keyboard while it types: a key pressed on the Mac meanwhile can land in it too.
    let typed = d.view.read_with(d.cx, |view, cx| view.input.read(cx).value().to_string()).unwrap_or_default();
    d.check(
        "typed a short line with the keyboard: one row",
        !short.stacked && typed.ends_with("Hello") && short.rows == 1,
        (&short, &typed),
    );
    d.shot("2-short-one-row");

    // 2. Up to the wrap point a word at a time: one row while the input shows one line, stacked from the first text
    // that would wrap there.
    let mut wrapped_in_one_row = None;
    let mut words = 0;
    loop {
        d.update_input(|input, window, cx| input.insert(" word", window, cx));
        d.wait(40).await;
        let now = d.shown();
        words += 1;
        if now.stacked || words > 60 {
            break;
        }
        if now.rows != 1 {
            wrapped_in_one_row = Some(now);
        }
    }
    d.check(
        "up to the wrap point the text never wraps inside the one row",
        wrapped_in_one_row.is_none(),
        &wrapped_in_one_row,
    );
    // Back a word to the last text that fits, then character by character with the keyboard to the point it doesn't.
    for _ in 0.." word".len() {
        d.backspace().await;
    }
    d.settle().await;
    let fits = d.shown();
    d.check("a word less: back to one row", !fits.stacked && fits.rows == 1, &fits);
    // Each key's frames sampled as it goes: the one that stacks it is sampled from its press.
    let mut typed = 0;
    let mut samples = Vec::new();
    while !d.shown().stacked && typed < 12 {
        crate::smoke::native_input::key(WIDTH, "x", 0, 0);
        samples = d.heights(300).await;
        typed += 1;
    }
    let wrapped = d.shown();
    d.check(
        "typing past the wrap point: stacked, the text on one line over the whole width",
        wrapped.stacked && wrapped.rows == 1,
        &wrapped,
    );
    let stacked_height = wrapped.height;
    d.glided("one row to stacked", &samples, ONE_ROW, stacked_height);
    d.shot("3-past-wrap-point-stacked");
    // At the wrap point nothing flips back and forth while nothing changes.
    let mut flips = Vec::new();
    for _ in 0..20 {
        d.wait(16).await;
        flips.push(d.shown().stacked);
    }
    d.check("stacked stays stacked frame after frame at the wrap point", flips.iter().all(|stacked| *stacked), &flips);
    crate::smoke::native_input::key(WIDTH, "\u{7f}", 51, 0);
    let samples = d.heights(300).await;
    let back = d.shown();
    d.check("deleting that character: one row again", !back.stacked && back.rows == 1, &back);
    d.glided("stacked back to one row", &samples, stacked_height, ONE_ROW);
    let mut flips = Vec::new();
    for _ in 0..20 {
        d.wait(16).await;
        flips.push(d.shown().stacked);
    }
    d.check("one row stays one row frame after frame at the wrap point", flips.iter().all(|stacked| !*stacked), &flips);

    // 3. A new line (Shift+Return): stacked, two lines.
    d.update_input(|input, window, cx| {
        input.set_value("", window, cx);
        input.insert("Two lines", window, cx);
    });
    d.settle().await;
    crate::smoke::native_input::key(WIDTH, "\r", 36, 1 << 17);
    d.wait(30).await;
    d.type_key("o").await;
    d.type_key("k").await;
    d.settle().await;
    let lines = d.shown();
    d.check("Shift+Return: stacked, two lines", lines.stacked && lines.rows == 2, &lines);
    d.shot("4-two-lines");

    // 4. A long text (put in as pasting text under the paste-as-file limit does): it grows to about 40% of the window,
    // 12 lines at most, then scrolls with the caret's line (the last) in sight and the top fading.
    let long: String =
        (1..=24).map(|line| format!("Line {line}: some pasted text that goes on for a while.\n")).collect::<String>()
            + "The last line, where the caret is.";
    d.update_input(move |input, window, cx| input.insert(long, window, cx));
    let samples = d.heights(400).await;
    let pasted = d.shown();
    let composer = d.view.read_with(d.cx, |view, _| view.composer_height).unwrap_or(0.);
    d.check(
        "long text: stacked, at most 12 lines shown",
        pasted.stacked && pasted.rows <= 12 && pasted.rows >= 3,
        &pasted,
    );
    d.check(
        "long text: the composer stays within about 40% of the window",
        composer <= HEIGHT * 0.4 + 24.,
        (composer, HEIGHT * 0.4),
    );
    let one_way = samples.windows(2).all(|pair| pair[1].1 >= pair[0].1 - 0.01);
    d.check("long text: the height only grows on the way", one_way, &samples);
    d.log.push(format!(
        "  long text: {}",
        samples.iter().map(|(at, height)| format!("{at}ms {height:.1}")).collect::<Vec<_>>().join(", ")
    ));
    // 26 lines of 20 points, `rows` of them in sight: scrolled to the bottom, where the caret is.
    let hidden = (26 - pasted.rows) as f32 * 20.;
    d.check(
        "long text: scrolled to the caret's line at the bottom",
        pasted.scroll < -0.5 && (pasted.scroll + hidden).abs() <= 21.,
        (pasted.scroll, hidden),
    );
    d.shot("5-long-text-scrolls");
    // Up to the top: nothing above, so no fade.
    d.update_input(|input, window, cx| {
        input.set_cursor_position(gpui_component::input::Position::new(0, 0), window, cx)
    });
    d.settle().await;
    let top = d.shown();
    d.check("long text: the caret at the start scrolls to the top", top.scroll.abs() < 0.5, &top);
    d.shot("6-long-text-top");

    // 5. Deleting back to one line.
    let before = d.shown().height;
    d.update_input(|input, window, cx| {
        input.set_value("", window, cx);
        input.insert("Back to one line", window, cx);
    });
    let samples = d.heights(300).await;
    let short = d.shown();
    d.check("deleted back to one line: one row", !short.stacked && short.rows == 1, &short);
    d.glided("long text back to one row", &samples, before, ONE_ROW);
    d.shot("7-back-to-one-row");

    // 6. The rest of the composer still works around the new row: an @mention picked from its list, a file's chip in
    // the text, and the effort picker and Send clicked with the pointer.
    d.view
        .update(d.cx, |view, cx| {
            view.set_mentions(
                vec![(
                    "Researcher".into(),
                    gpui::ImageSource::Resource(gpui::Resource::Embedded("avatars/owl-idle.webp".into())),
                    None,
                )],
                cx,
            )
        })
        .ok();
    d.update_input(|input, window, cx| {
        input.set_value("", window, cx);
        input.insert("Ask @Re", window, cx);
    });
    d.settle().await;
    d.shot("9-mention-list");
    crate::smoke::native_input::key(WIDTH, "\r", 36, 0);
    d.settle().await;
    let text = d.view.read_with(d.cx, |view, cx| view.input.read(cx).value().to_string()).unwrap_or_default();
    d.check("mention: Return picks it from the list", text.starts_with("Ask @Researcher"), &text);
    let picture = std::env::temp_dir().join("filo-composer-fixture.png");
    image::RgbImage::from_fn(120, 80, |x, y| image::Rgb([(x * 2) as u8, (y * 3) as u8, 160]))
        .save(&picture)
        .expect("test picture");
    let view = d.view.clone();
    d.cx.update(move |window, cx| view.update(cx, |view, cx| view.attach_files(vec![picture], window, cx))).ok();
    d.settle().await;
    let (files, chips) = d
        .view
        .read_with(d.cx, |view, cx| (view.attachments.len(), view.input.read(cx).atoms().len()))
        .unwrap_or((0, 0));
    d.check("attachment: attached, its chip in the text", files == 1 && chips == 1, (files, chips));
    d.shot("10-mention-and-attachment");
    let reasoning = d.view.read_with(d.cx, |view, _| view.reasoning()).unwrap_or_default();
    let clicked = crate::smoke::on_screen("composer-reasoning").is_some_and(|bounds| {
        crate::smoke::native_input::click(
            WIDTH,
            f64::from(f32::from(bounds.center().x)),
            f64::from(f32::from(bounds.center().y)),
        )
    });
    d.settle().await;
    d.shot("11-effort-menu");
    // The menu opens over the row, the current level ticked; Down then Return picks the next one.
    crate::smoke::native_input::key(WIDTH, "\u{f701}", 125, 0);
    d.wait(60).await;
    crate::smoke::native_input::key(WIDTH, "\r", 36, 0);
    d.settle().await;
    let picked = d.view.read_with(d.cx, |view, _| view.reasoning()).unwrap_or_default();
    d.check(
        "effort: clicked, its menu picks another level",
        clicked && picked != reasoning,
        (clicked, reasoning, picked),
    );
    let before = d.delivered();
    let clicked = crate::smoke::on_screen("composer-action").is_some_and(|bounds| {
        crate::smoke::native_input::click(
            WIDTH,
            f64::from(f32::from(bounds.center().x)),
            f64::from(f32::from(bounds.center().y)),
        )
    });
    d.wait(600).await;
    let (sent, after) = (d.shown(), d.delivered());
    d.check(
        "send: clicking Send sends it, the composer empty and one row",
        clicked && after > before && sent.text_chars == 0 && !sent.stacked,
        (clicked, sent),
    );
    d.shot("12-sent-with-click");
    // Its reply finishing, so the next message starts from a still chat.
    d.view
        .update(d.cx, |view, cx| {
            view.pending = None;
            view.turns.clear();
            view.data_changed(cx);
        })
        .ok();
    d.settle().await;

    // 7. Stacked, a file's chip first (as the user's message had): the text wraps at the full width, with only the
    // caret's room at the right. Thin letters one at a time find where it wraps, to within one of them.
    d.update_input(|input, window, cx| input.set_value("", window, cx));
    let picture = std::env::temp_dir().join("filo-composer-fixture.png");
    let view = d.view.clone();
    d.cx.update(move |window, cx| view.update(cx, |view, cx| view.attach_files(vec![picture], window, cx))).ok();
    d.update_input(|input, window, cx| input.insert(" when i hit send it should scroll", window, cx));
    d.settle().await;
    let mut last_one_line = None;
    let mut first_wrapped = None;
    for _ in 0..400 {
        let now = d.shown();
        let text = d.view.read_with(d.cx, |view, cx| view.input.read(cx).value().to_string()).unwrap_or_default();
        if now.stacked && now.rows == 1 {
            last_one_line = Some(text);
        } else if now.stacked && now.rows == 2 && last_one_line.is_some() {
            first_wrapped = Some(text);
            break;
        }
        d.update_input(|input, window, cx| input.insert("i", window, cx));
        d.wait(20).await;
    }
    d.shot("14-chip-line-wraps-past-the-edge");
    let view = d.view.clone();
    let verdict =
        d.cx.update(|window, cx| {
            let layout = &view.read(cx).composer_layout;
            let width = layout.stacked_wrap_width();
            let fits = last_one_line.as_deref().map(|text| !layout.wraps(text, width, window));
            let wraps = first_wrapped.as_deref().map(|text| layout.wraps(text, width, window));
            (width, layout.row_width(), fits, wraps)
        })
        .ok();
    d.check(
        "chip first, stacked: one line while it fits the full width less the caret's room, two lines once it doesn't",
        verdict.is_some_and(|(_, _, fits, wraps)| fits == Some(true) && wraps == Some(true)),
        (&verdict, &last_one_line.map(|text| text.len()), &first_wrapped.map(|text| text.len())),
    );
    // A letter less: back on one line, running to the row's right edge.
    crate::smoke::native_input::key(WIDTH, "\u{7f}", 51, 0);
    d.settle().await;
    let back = d.shown();
    d.check("chip first, stacked: a letter less is one line again", back.stacked && back.rows == 1, &back);
    d.shot("13-chip-line-runs-to-the-edge");
    d.view
        .update(d.cx, |view, _| {
            view.attachments.clear();
            view.chips.clear();
        })
        .ok();

    // 8. Sending from stacked: empty and one row again.
    d.update_input(|input, window, cx| {
        input.set_value("", window, cx);
        input.insert("A message on\ntwo lines to send", window, cx);
    });
    d.settle().await;
    let before = d.shown().height;
    let view = d.view.clone();
    d.cx.update(|window, cx| view.update(cx, |view, cx| view.send(window, cx))).ok();
    let samples = d.heights(300).await;
    let sent = d.shown();
    d.check("sent: empty, one row", !sent.stacked && sent.text_chars == 0 && sent.rows == 1, &sent);
    d.glided("sending shrinks to one row", &samples, before, ONE_ROW);
    d.shot("8-sent");

    // The sent message's reply finishing, so the chat stays where it is.
    d.view
        .update(d.cx, |view, cx| {
            view.pending = None;
            view.turns.clear();
            view.data_changed(cx);
        })
        .ok();

    // 9. A long reply draft, made as quoting a message's points and answering under each makes it (the box at its most
    // lines, scrolling inside): typing and pasting at its end keep the caret's line at the bottom, and the wheel over
    // the box scrolls only its text, the chat behind staying still, also at the box's top and bottom ends.
    d.update_input(|input, window, cx| input.set_value("", window, cx));
    d.settle().await;
    let points = [
        (
            "Pictures: are assets/images/filo/workers/*.png and assets/icons/skills.svg yours? Their source couldn't be found.",
            "wdym",
        ),
        (
            "Animal avatars: their source images were made with an image model. Should the notices say so? Not required, but honest.",
            "no need",
        ),
        (
            "Dataviz skill: crates/filo-local/src/dataviz_skill.md reads like your own short rewrite (64 lines), not a copy. Can you confirm?",
            "yes it is mine",
        ),
        (
            "Analytics: desktop counts go to the Dev project. Keep that, or switch to the main one before the launch?",
            "keep it",
        ),
    ];
    for (index, (point, answer)) in points.iter().enumerate() {
        let view = d.view.clone();
        let point = point.to_string();
        d.cx.update(move |window, cx| view.update(cx, |view, cx| view.quote(&point, window, cx))).ok();
        d.settle().await;
        d.caret_at_bottom(&format!("draft: quote {}", index + 1));
        d.type_text(answer).await;
        d.settle().await;
        d.caret_at_bottom(&format!("draft: typed answer {}", index + 1));
    }
    d.type_text("\n\nAnd one more thing, typed at the end of the draft, long enough to wrap onto a line of its own.")
        .await;
    d.settle().await;
    d.caret_at_bottom("draft: typed a wrapping paragraph at the end");
    let draft = d.shown();
    d.check("draft: at its most lines, scrolled inside", draft.stacked && draft.scroll < -0.5, &draft);
    d.shot("15-long-draft-caret-at-bottom");
    let pasted: String = (1..=6)
        .map(|line| format!("\nPasted line {line}, as a paste under the paste-as-file limit puts it in."))
        .collect();
    d.update_input(move |input, window, cx| input.insert(pasted, window, cx));
    d.settle().await;
    d.caret_at_bottom("draft: pasted at the end");
    d.shot("16-pasted-caret-at-bottom");
    let end = d.shown().scroll;

    // The wheel over the box, up past its top and back down past its bottom: its text scrolls, the chat doesn't.
    let input = crate::smoke::on_screen("composer-input");
    let feed_before = d.view.read_with(d.cx, |view, _| view.scroll_position()).ok();
    let (mut tops, mut bottoms) = (Vec::new(), Vec::new());
    if let Some(bounds) = input {
        let (x, y) = (f64::from(f32::from(bounds.center().x)), f64::from(f32::from(bounds.center().y)));
        crate::smoke::native_input::hover(WIDTH, x, y);
        d.wait(60).await;
        for _ in 0..40 {
            crate::smoke::native_input::scroll_in(WIDTH, x, y, -40.);
            d.wait(16).await;
            tops.push(d.shown().scroll);
        }
        d.settle().await;
        let feed_up = d.view.read_with(d.cx, |view, _| view.scroll_position()).ok();
        d.check(
            "wheel up over the box, past its top: the text scrolls to its top, the chat stays",
            tops.last().is_some_and(|scroll| scroll.abs() < 0.5) && feed_up == feed_before,
            (&tops, &feed_before, &feed_up),
        );
        d.shot("17-wheel-up-box-only");
        for _ in 0..40 {
            crate::smoke::native_input::scroll_in(WIDTH, x, y, 40.);
            d.wait(16).await;
            bottoms.push(d.shown().scroll);
        }
        d.settle().await;
        let feed_down = d.view.read_with(d.cx, |view, _| view.scroll_position()).ok();
        d.check(
            "wheel down over the box, past its bottom: the text scrolls to its end, the chat stays",
            bottoms.last().is_some_and(|scroll| (scroll - end).abs() < 0.5) && feed_down == feed_before,
            (&bottoms, &feed_before, &feed_down),
        );
        d.caret_at_bottom("draft: scrolled back down with the wheel");
    } else {
        d.check("wheel over the box: the box is on screen", false, "composer-input not tracked");
    }
    // Over the chat itself the wheel still scrolls the chat.
    crate::smoke::native_input::hover(WIDTH, WIDTH / 2., 200.);
    d.wait(60).await;
    for _ in 0..10 {
        crate::smoke::native_input::scroll_in(WIDTH, WIDTH / 2., 200., -40.);
        d.wait(16).await;
    }
    d.settle().await;
    let feed_after = d.view.read_with(d.cx, |view, _| view.scroll_position()).ok();
    d.check("wheel over the chat: the chat scrolls", feed_after != feed_before, (&feed_before, &feed_after));

    if let Some(dir) = &d.shots {
        let _ = std::fs::write(format!("{dir}/composer-checks.log"), d.log.join("\n") + "\n");
    }
    driver.failures
}

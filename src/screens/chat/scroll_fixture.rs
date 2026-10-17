// `FILO_OPEN=chat-scroll`: where a chat's feed sits as it's opened, left and come back to, as history loads and as
// replies arrive, on made-up chats in a window of its own (no backend). It takes the steps the app takes:
// the tab's chat shown before the shell's once-a-second check says the user is looking at it (switching to the
// Agents tab, a banner), pages and live data arriving after the first layout, Back and Forward between chats, and real
// scroll-wheel events for reading older messages, and posted pointer events on the timeline. Prints a line per check
// and quits, exit 1 when one fails. `FILO_FIXTURE_SHOTS=<dir>` saves screenshots of the timeline's steps;
// `FILO_FIXTURE_THEME=dark` (or `light`) draws either theme whatever the app's appearance setting says.
// `FILO_SCROLL_DEBUG=1` prints the list's state at every look, and with `FILO_SCROLL_ROWS=1` its rows too.

use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Window, div, px,
};
use serde_json::{Value, json};

use super::timeline::{Drawn, Timeline};
use super::{ChatTurn, CodingSession, Conversation, Detail, ViewRow};

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-scroll")
}

pub fn open(cx: &mut App) {
    if let Ok(mode) = std::env::var("FILO_FIXTURE_THEME") {
        let mut prefs = crate::config::load_prefs();
        prefs.theme_mode = mode;
        crate::theme::apply(prefs, None, cx);
    }
    let window = crate::test_run::window_size((760., 720.));
    crate::smoke::fixtures::open(
        cx,
        "chat-scroll",
        "Filo chat scroll",
        window,
        false,
        |_, cx| cx.new(|_| Host { shown: None }),
        drive,
    );
}

/// The chat on screen, the way the Chat and Agents tabs show one at a time.
struct Host {
    shown: Option<Entity<Conversation>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.shown.clone())
    }
}

const MINUTE: f64 = 60_000.;
const SETTLE: Duration = Duration::from_millis(250);

/// When the made-up chat began: fixed for the run, so a page delivered again has the same messages.
fn base() -> f64 {
    static BASE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| crate::format::now_ms() - 600. * MINUTE)
}

/// Message `index` of a made-up chat: the user and Filo take turns, Filo's replies run to several lines.
fn message(index: i64) -> Value {
    let user = index.rem_euclid(2) == 0;
    let content = if user {
        format!("Question {index}: what's next on the list?")
    } else {
        format!(
            "Reply {index}. Here's where things stand, in enough words to wrap over a few lines of the bubble so the rows \
             aren't all one height.\n\n- The first point of reply {index}\n- A second point, a little longer than the first one\n\nThat's all for now."
        )
    };
    json!({ "_id": format!("m{index}"), "_creationTime": base() + index as f64 * MINUTE, "role": if user { "user" } else { "assistant" }, "content": content })
}

/// Messages `from..to`, with the user's cursor after `seen_through` (all seen when None).
fn detail(from: i64, to: i64, has_more: bool, seen_through: Option<i64>) -> Detail {
    detail_with(from, to, has_more, seen_through, &[])
}

/// `detail` with a picture in each of `pictures`' messages, from its file.
fn detail_with(from: i64, to: i64, has_more: bool, seen_through: Option<i64>, pictures: &[(i64, String)]) -> Detail {
    let seen = seen_through.map_or(base() + 10_000. * MINUTE, |index| base() + index as f64 * MINUTE);
    let messages: Vec<Value> = (from..to)
        .map(|index| {
            let mut message = message(index);
            if let Some((_, file)) = pictures.iter().find(|(at, _)| *at == index) {
                message["attachments"] = json!([{
                    "storageId": format!("picture-{index}"), "name": "picture.png", "contentType": "image/png", "size": 4096.0, "localFile": file,
                }]);
            }
            message
        })
        .collect();
    serde_json::from_value(json!({
        "session": { "_id": "scroll-chat", "title": "Scroll", "seenUntil": seen },
        "messages": messages,
        "hasMore": has_more,
    }))
    .expect("fixture detail")
}

/// `detail` as the agents' group has it: Filo and an agent take turns answering.
fn group_detail(from: i64, to: i64, has_more: bool) -> Detail {
    let mut detail = detail(from, to, has_more, None);
    for message in &mut detail.messages {
        if !message.is_user() {
            let index: i64 = message.id.trim_start_matches('m').parse().unwrap_or(0);
            message.speaker = Some(
                if index.rem_euclid(4) == 1 { crate::local_backend::DEFAULT_AGENT_ID } else { "agent-a" }.to_string(),
            );
        }
    }
    detail
}

fn group_speakers() -> std::collections::HashMap<String, (String, gpui::ImageSource, Option<gpui::Hsla>)> {
    [(crate::local_backend::DEFAULT_AGENT_ID, "Filo"), ("agent-a", "Researcher")]
        .into_iter()
        .map(|(id, name)| (id.to_string(), (name.to_string(), gpui::ImageSource::from("icons/user.svg"), None)))
        .collect()
}

/// Where message `id` is drawn, down from the top of the view; None when it isn't laid out.
fn message_y(view: &Conversation, id: &str) -> Option<f32> {
    let index = view.rows.iter().position(|row| matches!(row, ViewRow::Message { message, .. } if message.id == id))?;
    let bounds = view.list_state.bounds_for_item(index)?;
    Some(f32::from(bounds.top() - view.list_state.viewport_bounds().top()))
}

/// Tall pictures (300 × 900 in shape) in a folder of the fixture's own, as a chat's files are on this Mac:
/// `<folder>/<name>.png`. Camera-sized, so each takes a while to decode, as a real photo does, and a chat lays it out
/// 4:3 until it has.
fn picture_folder() -> String {
    let dir = std::env::temp_dir().join("filo-chat-scroll-fixture");
    std::fs::create_dir_all(&dir).expect("fixture folder");
    for name in ["a", "b", "c", "d"] {
        let path = dir.join(format!("{name}.png"));
        if !path.exists() {
            image::RgbaImage::from_fn(1500, 4500, |x, y| image::Rgba([(x / 7) as u8, (y / 19) as u8, 200, 255]))
                .save(&path)
                .expect("fixture picture");
        }
    }
    dir.display().to_string()
}

/// A reply under way to the user's message `index` (the Thinking row under it).
fn replying(index: i64) -> Vec<ChatTurn> {
    serde_json::from_value(json!([{ "turnId": "turn", "requestId": "request", "createdAt": base() + index as f64 * MINUTE, "userMessageId": format!("m{index}"), "state": "pending" }])).expect("fixture turns")
}

/// A Claude Code session started from the chat at `at` (`coding:sessionsForChat`).
fn session(id: &str, at: f64, status: &str, now: &str) -> CodingSession {
    serde_json::from_value(json!({
        "taskId": id, "tool": "claude-code", "title": format!("Task {id}"), "status": status, "now": now, "pending": 0.0,
        "createdAt": at, "updatedAt": at,
    }))
    .expect("fixture session")
}

/// The sessions of a busy agent's chat, oldest first, as the backend sends its 20 newest: most started before the
/// loaded page (days of work), a few between today's messages, one after the unseen replies began. With `live`, the
/// one from the middle of the page works again and a new one starts after the newest message, as (status, now).
fn sessions(live: Option<(&str, &str)>) -> Vec<CodingSession> {
    let at = |index: f64| base() + index * MINUTE;
    let mut list: Vec<CodingSession> =
        (0..14).map(|n| session(&format!("old{n}"), at(-3000. + n as f64 * 200.), "done", "Done")).collect();
    for (id, index) in [("s10", 10.5), ("s30", 30.5), ("s38", 38.5)] {
        list.push(session(id, at(index), "done", "Done"));
    }
    match live {
        None => list.push(session("s20", at(20.5), "done", "Done")),
        Some((status, now)) => {
            list.push(session("s20", at(20.5), status, now));
            list.push(session("s40", at(39.5), status, now));
            // The backend sends the 20 newest: the oldest drops out.
            list.remove(0);
        }
    }
    list.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
    list
}

/// What a check reads off the chat after its last layout.
#[derive(Debug)]
struct Seen {
    /// The end of the feed (the room under the newest message) shows.
    at_end: bool,
    /// The jump pill shows.
    pill: bool,
    /// The first message showing, and how far down the view its top is.
    top: Option<(String, f32)>,
    /// The "New messages" divider shows.
    divider: bool,
}

fn look(view: &Conversation) -> Seen {
    let list = &view.list_state;
    let viewport = list.viewport_bounds();
    let at_end = view
        .rows
        .len()
        .checked_sub(1)
        .and_then(|last| list.bounds_for_item(last))
        .is_some_and(|spacer| spacer.bottom() <= viewport.bottom() + px(2.))
        || list.logical_scroll_top().item_ix >= list.item_count();
    let visible = |index: usize| {
        list.bounds_for_item(index)
            .filter(|bounds| bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom())
    };
    let top = view.rows.iter().enumerate().find_map(|(index, row)| match row {
        ViewRow::Message { message, .. } => {
            visible(index).map(|bounds| (message.id.clone(), f32::from(bounds.top() - viewport.top())))
        }
        _ => None,
    });
    let divider =
        view.rows.iter().position(|row| matches!(row, ViewRow::Unseen)).is_some_and(|index| visible(index).is_some());
    Seen { at_end, pill: !view.at_end && !view.rows.is_empty(), top, divider }
}

#[derive(Clone, Copy)]
enum Pointer {
    Down,
    Drag,
    Hover,
    Up,
}

struct Driver<'a> {
    host: Entity<Host>,
    cx: &'a mut AsyncWindowContext,
    failures: usize,
    /// `FILO_FIXTURE_SHOTS`: where screenshots go.
    shots: Option<String>,
}

impl Driver<'_> {
    /// Lets what the last step started land, then lays the window out as a frame would. macOS gives a covered window
    /// no frames (another Space, a full-screen app, the screen locked: GPUI stops its display link), and checks would
    /// read the layout from before it was covered.
    async fn settle(&mut self) {
        self.cx.background_executor().timer(SETTLE).await;
        // Twice: a layout can change what the next one shows (the composer's height, the list's measure).
        for _ in 0..2 {
            self.cx.update(|window, cx| window.draw(cx).clear()).ok();
        }
    }

    /// A conversation as the app makes one: its tab shows it, the user not yet counted as looking.
    fn make(&mut self) -> Entity<Conversation> {
        self.cx
            .update(|window, cx| {
                let view = cx.new(|cx| Conversation::new(None, "Filo".into(), window, cx));
                view.update(cx, |view, _| view.fixture = true);
                view
            })
            .expect("window")
    }

    /// Puts `view` on screen, as Back, Forward, a History click or opening an agent does.
    fn show(&mut self, view: &Entity<Conversation>) {
        let view = view.clone();
        self.host
            .update(self.cx, |host, cx| {
                host.shown = Some(view);
                cx.notify();
            })
            .ok();
    }

    fn attend(&mut self, view: &Entity<Conversation>, attending: bool) {
        view.update(self.cx, |view, cx| view.set_attending(attending, cx)).ok();
    }

    /// `voiceSessions:detail` (and the rest of the chat's subscriptions) delivering.
    fn load(&mut self, view: &Entity<Conversation>, detail: Detail) {
        view.update(self.cx, |view, cx| view.detail_arrived(Ok(Some(detail)), cx)).ok();
    }

    fn sessions(&mut self, view: &Entity<Conversation>, sessions: Vec<CodingSession>) {
        view.update(self.cx, |view, cx| {
            view.coding_sessions = sessions;
            view.data_changed(cx);
        })
        .ok();
    }

    /// Left for other chats, past the few Agents keeps live.
    fn park(&mut self, view: &Entity<Conversation>) {
        self.attend(view, false);
        view.update(self.cx, |view, _| view.park()).ok();
    }

    /// Shown again after `park`, the way the tabs do it: on screen, its queries started again, then the shell's check
    /// says the user is looking. Its answers come from `load`, or not at all while offline.
    fn come_back(&mut self, view: &Entity<Conversation>) {
        self.show(view);
        view.update(self.cx, |view, cx| view.unpark(cx)).ok();
        self.attend(view, true);
    }

    /// The jump pill's click.
    fn jump(&mut self, view: &Entity<Conversation>) {
        view.update(self.cx, |view, cx| view.jump_to_latest(cx)).ok();
    }

    /// Waits `ms` (a picture's download), then settles.
    async fn wait(&mut self, ms: u64) {
        self.cx.background_executor().timer(Duration::from_millis(ms)).await;
        self.settle().await;
    }

    fn labels(&mut self, view: &Entity<Conversation>) -> Vec<String> {
        view.read_with(self.cx, |view, _| view.row_labels()).unwrap_or_default()
    }

    fn turns(&mut self, view: &Entity<Conversation>, turns: Vec<ChatTurn>) {
        view.update(self.cx, |view, cx| {
            view.turns = turns;
            view.data_changed(cx);
        })
        .ok();
    }

    /// The reader scrolling the feed (positive goes up, to older messages). A wheel event posted through AppKit doesn't
    /// reach a window that isn't key, and this one never takes the focus, so it's the list's scroll and the handler
    /// the wheel would call.
    fn wheel(&mut self, view: &Entity<Conversation>, distance: f32) {
        view.update(self.cx, |view, cx| {
            view.list_state.scroll_by(px(-distance));
            let top = view.list_state.logical_scroll_top().item_ix;
            let count = view.list_state.item_count();
            view.scrolled(top..(top + 4).min(count), count, cx);
            cx.notify();
        })
        .ok();
    }

    /// One frame, as the screen would show it next.
    fn frame(&mut self) {
        self.cx.update(|window, cx| window.draw(cx).clear()).ok();
    }

    /// The reader at the very top of what's loaded, the row saying older messages are loading in view.
    fn read_top(&mut self, view: &Entity<Conversation>) {
        view.update(self.cx, |view, cx| {
            view.list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.) });
            let count = view.list_state.item_count();
            view.scrolled(0..4.min(count), count, cx);
            cx.notify();
        })
        .ok();
    }

    /// The reader scrolled to message `id`, `offset` into it.
    fn read_at(&mut self, view: &Entity<Conversation>, id: &str, offset: f32) {
        view.update(self.cx, |view, cx| {
            let Some(index) =
                view.rows.iter().position(|row| matches!(row, ViewRow::Message { message, .. } if message.id == id))
            else {
                return;
            };
            view.list_state.scroll_to(gpui::ListOffset { item_ix: index, offset_in_item: px(offset) });
            let count = view.list_state.item_count();
            view.scrolled(index..(index + 4).min(count), count, cx);
            cx.notify();
        })
        .ok();
    }

    /// The timeline as last drawn.
    fn timeline(&mut self, view: &Entity<Conversation>) -> Option<Drawn> {
        view.read_with(self.cx, |view, _| view.timeline.drawn()).ok().flatten()
    }

    /// A pointer event at `at` (window points), posted through AppKit as a real one would come (`smoke::native_input`),
    /// then a settle.
    async fn pointer(&mut self, at: gpui::Point<gpui::Pixels>, kind: Pointer) {
        use crate::smoke::native_input::{self, Drag};
        let width = self.cx.update(|window, _| f64::from(f32::from(window.bounds().size.width))).unwrap_or(760.);
        let (x, y) = (f64::from(f32::from(at.x)), f64::from(f32::from(at.y)));
        let posted = match kind {
            Pointer::Down => native_input::drag_step(width, Drag::Down, x, y),
            Pointer::Drag => native_input::drag_step(width, Drag::Move, x, y),
            Pointer::Up => native_input::drag_step(width, Drag::Up, x, y),
            Pointer::Hover => native_input::hover(width, x, y),
        };
        if !posted {
            println!("chat-scroll: couldn't post a pointer event");
        }
        self.settle().await;
    }

    /// Esc, as the window's key handling gets it (the fixture's window never takes the keyboard, so a posted key
    /// wouldn't reach it).
    async fn escape(&mut self) {
        self.cx
            .update(|window, cx| window.dispatch_keystroke(gpui::Keystroke::parse("escape").expect("escape"), cx))
            .ok();
        self.settle().await;
    }

    /// A screenshot of the window, named `name`, with `FILO_FIXTURE_SHOTS`.
    fn shot(&mut self, name: &str) {
        let width = self.cx.update(|window, _| f64::from(f32::from(window.bounds().size.width))).unwrap_or(760.);
        crate::smoke::fixtures::shot(self.shots.as_deref(), width, name);
    }

    fn top_row(&mut self, view: &Entity<Conversation>) -> gpui::ListOffset {
        view.read_with(self.cx, |view, _| view.list_state.logical_scroll_top()).expect("view")
    }

    fn look(&mut self, view: &Entity<Conversation>) -> Seen {
        view.read_with(self.cx, |view, _| {
            if std::env::var_os("FILO_SCROLL_DEBUG").is_some() {
                println!(
                    "chat-scroll: debug top={:?} count={} rows={} at_end={} attending={} limit={}",
                    view.list_state.logical_scroll_top(),
                    view.list_state.item_count(),
                    view.rows.len(),
                    view.at_end,
                    view.attending,
                    view.limit
                );
                if std::env::var_os("FILO_SCROLL_ROWS").is_some() {
                    println!("chat-scroll: rows {:?}", view.row_labels());
                }
            }
            look(view)
        })
        .expect("view")
    }

    fn check(&mut self, name: &str, ok: bool, seen: &Seen) {
        if ok {
            println!("chat-scroll: {name}: ok");
        } else {
            self.failures += 1;
            println!("chat-scroll: {name}: FAILED {seen:?}");
        }
    }
}

async fn drive(host: Entity<Host>, cx: &mut AsyncWindowContext) -> usize {
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok();
    if let Some(dir) = &shots {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut driver = Driver { host, cx, failures: 0, shots };
    let d = &mut driver;
    d.settle().await;

    // 1. Opened while the shell still counts the user as away (a tab switch or a banner, up to a second before the
    //    check): the first page lays out, the chat's other data lands after it, then the check says they're looking.
    for (name, agent) in [("Filo's chat", false), ("an agent's chat", true)] {
        let a = d.make();
        if agent {
            a.update(d.cx, |view, cx| view.set_agent_name("Desktop App Developer".into(), cx)).ok();
        }
        d.show(&a);
        d.attend(&a, false);
        d.settle().await;
        d.load(&a, detail(0, 40, true, None));
        d.settle().await;
        d.turns(&a, replying(38));
        d.settle().await;
        let before = d.look(&a);
        d.check(&format!("{name} opened before the look check shows the newest messages"), before.at_end, &before);
        d.attend(&a, true);
        d.settle().await;
        let seen = d.look(&a);
        d.check(
            &format!("{name} opened before the look check ends at the newest messages"),
            seen.at_end && !seen.pill,
            &seen,
        );
    }

    // 2. Opened while looking (a History click on the Chat tab).
    let a = d.make();
    d.show(&a);
    d.attend(&a, true);
    d.settle().await;
    d.load(&a, detail(0, 40, true, None));
    d.settle().await;
    d.turns(&a, replying(38));
    d.settle().await;
    let seen = d.look(&a);
    d.check("opened while looking ends at the newest messages", seen.at_end && !seen.pill, &seen);

    // 3. A to B and back to A, A left at its newest: the user's own message from elsewhere (a voice message from another app) lands in A meanwhile.
    let b = d.make();
    d.attend(&a, false);
    d.show(&b);
    d.attend(&b, true);
    d.settle().await;
    d.load(&b, detail(100, 160, true, None));
    d.settle().await;
    let seen = d.look(&b);
    d.check("B opened from A shows its newest messages", seen.at_end, &seen);
    d.load(&a, detail(0, 41, true, None));
    d.turns(&a, replying(40));
    d.settle().await;
    d.attend(&b, false);
    d.show(&a);
    d.attend(&a, true);
    d.settle().await;
    let seen = d.look(&a);
    d.check("back to A, left at its newest, shows its newest", seen.at_end && !seen.pill, &seen);

    // 4. Reading older messages in A, then B, then A: A keeps its place, B keeps its own.
    d.wheel(&a, 900.);
    d.settle().await;
    let reading = d.look(&a);
    d.check("scrolled up in A", !reading.at_end, &reading);
    d.attend(&a, false);
    d.show(&b);
    d.attend(&b, true);
    d.settle().await;
    let seen = d.look(&b);
    d.check("B after A was scrolled up shows B's newest", seen.at_end, &seen);
    d.attend(&b, false);
    d.show(&a);
    d.attend(&a, true);
    d.settle().await;
    let seen = d.look(&a);
    d.check(
        "back to A keeps the older messages it was reading",
        seen.top.as_ref().map(|top| &top.0) == reading.top.as_ref().map(|top| &top.0) && !seen.at_end,
        &seen,
    );

    // 5. A reply while reading older messages doesn't move them.
    d.turns(&a, Vec::new());
    d.load(&a, detail(0, 42, true, Some(40)));
    d.settle().await;
    let seen = d.look(&a);
    d.check("a reply while reading older messages stays put", seen.top == reading.top && seen.pill, &seen);

    // 6. Older history loading while near the top keeps the message being read where it is.
    d.wheel(&a, 100_000.);
    d.settle().await;
    let top = d.look(&a);
    d.load(&a, detail(-40, 42, true, None));
    d.settle().await;
    let seen = d.look(&a);
    let kept = matches!((&top.top, &seen.top), (Some((_, _)), Some(_)))
        && a.read_with(d.cx, |view, _| {
            let id = &top.top.as_ref().unwrap().0;
            view.rows
                .iter()
                .position(|row| matches!(row, ViewRow::Message { message, .. } if &message.id == id))
                .and_then(|index| view.list_state.bounds_for_item(index))
                .is_some_and(|bounds| {
                    (f32::from(bounds.top() - view.list_state.viewport_bounds().top()) - top.top.as_ref().unwrap().1)
                        .abs()
                        < 2.
                })
        })
        .unwrap_or(false);
    d.check("an older page arriving keeps the message being read in place", kept, &seen);

    // 7. Back at the newest, a reply while looking follows it.
    a.update(d.cx, |view, cx| view.jump_to_latest(cx)).ok();
    d.settle().await;
    d.load(&a, detail(-40, 44, true, None));
    d.settle().await;
    let seen = d.look(&a);
    d.check("a reply while at the newest follows it", seen.at_end && !seen.pill, &seen);

    // 8. A quoted message jumped to shows, and stays shown.
    a.update(d.cx, |view, cx| view.jump_to("m2", cx)).ok();
    d.settle().await;
    let seen = a.read_with(d.cx, |view, _| {
        let index =
            view.rows.iter().position(|row| matches!(row, ViewRow::Message { message, .. } if message.id == "m2"));
        index.and_then(|index| view.list_state.bounds_for_item(index)).is_some_and(|bounds| {
            let viewport = view.list_state.viewport_bounds();
            bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom()
        })
    });
    let after = d.look(&a);
    d.check("jumping to a quoted message shows it", seen.unwrap_or(false), &after);

    // 9. Unseen replies on opening: the divider, as designed (seen.rs).
    let c = d.make();
    d.attend(&a, false);
    d.show(&c);
    d.attend(&c, true);
    d.settle().await;
    d.load(&c, detail(0, 40, true, Some(30)));
    d.settle().await;
    let seen = d.look(&c);
    d.check("opening with unseen replies starts at the divider", seen.divider, &seen);

    // 10. An empty chat, then its first messages.
    let e = d.make();
    d.attend(&c, false);
    d.show(&e);
    d.attend(&e, true);
    d.settle().await;
    d.load(&e, detail(0, 0, false, None));
    d.settle().await;
    let seen = d.look(&e);
    d.check("an empty chat shows no pill", !seen.pill, &seen);
    d.load(&e, detail(0, 2, false, None));
    d.turns(&e, replying(0));
    d.settle().await;
    let seen = d.look(&e);
    d.check("an empty chat's first messages show", seen.at_end && !seen.pill, &seen);

    // 11. The screenshot (an agent's chat with three unseen replies): its coding sessions arrive after the first page,
    //     most of them older than it. The divider keeps its place, and sessions from before the page aren't shown on top
    //     of it, all at once (they show with the page they belong to).
    let s = d.make();
    s.update(d.cx, |view, cx| view.set_agent_name("Desktop App Developer".into(), cx)).ok();
    d.attend(&e, false);
    d.show(&s);
    d.attend(&s, true);
    d.settle().await;
    d.load(&s, detail(0, 40, true, Some(34)));
    d.settle().await;
    let opened = d.look(&s);
    d.check("an agent's chat with unseen replies opens at the divider", opened.divider, &opened);
    d.sessions(&s, sessions(None));
    d.turns(&s, Vec::new());
    d.settle().await;
    let seen = d.look(&s);
    d.check(
        "coding sessions arriving after the page leave the divider where it was",
        seen.divider && seen.top.is_some() && seen.top == opened.top,
        &seen,
    );
    let labels = s.read_with(d.cx, |view, _| view.row_labels()).unwrap_or_default();
    let first_message = labels.iter().position(|label| label.starts_with('m')).unwrap_or(0);
    let piled = labels[..first_message].iter().any(|label| label == "coding-session");
    d.check("sessions from before the loaded page wait for it", !piled, &seen);

    // 12. Reading the middle of it: a session starts below and drops the oldest of the 20 above, sessions above and below
    //     work, a reply gets under way.
    d.read_at(&s, "m24", 40.);
    d.settle().await;
    let reading = d.look(&s);
    d.check(
        "reading the middle of it",
        reading.top.as_ref().is_some_and(|top| top.0 == "m24" || top.0 == "m25") && !reading.at_end,
        &reading,
    );
    d.sessions(&s, sessions(Some(("running", "Running cargo test"))));
    d.settle().await;
    let seen = d.look(&s);
    d.check(
        "a session starting while reading keeps the place being read",
        seen.top.is_some() && seen.top == reading.top && !seen.at_end,
        &seen,
    );
    d.turns(&s, replying(38));
    d.sessions(&s, sessions(Some(("running", "Editing conversation.rs"))));
    d.settle().await;
    let seen = d.look(&s);
    d.check(
        "a session's progress while reading keeps the place being read",
        seen.top.is_some() && seen.top == reading.top,
        &seen,
    );

    // 13. Parked while reading (past the chats kept live), shown again while offline (no answer for a while), then the
    //     backend's answer with replies that came meanwhile: it shows at once where it was read, keeps that while
    //     offline, and the replies arrive below it without moving it.
    let p = d.make();
    d.attend(&s, false);
    d.show(&p);
    d.attend(&p, true);
    // Seen through its newest (`None`'s cursor is far ahead and would hide what comes later).
    d.load(&p, detail(0, 40, true, Some(39)));
    d.settle().await;
    d.read_at(&p, "m20", 30.);
    d.settle().await;
    let reading = d.look(&p);
    d.check(
        "a chat read in the middle",
        reading.top.as_ref().is_some_and(|top| top.0 == "m20" || top.0 == "m21") && !reading.at_end,
        &reading,
    );
    d.park(&p);
    d.show(&s);
    d.attend(&s, true);
    d.settle().await;
    d.attend(&s, false);
    d.come_back(&p);
    d.settle().await;
    let back = d.look(&p);
    d.check(
        "parked and shown again, it shows at once where it was read",
        back.top.is_some() && back.top == reading.top,
        &back,
    );
    d.wait(1500).await;
    let offline = d.look(&p);
    d.check(
        "offline, it keeps what it showed and where",
        offline.top.is_some() && offline.top == reading.top,
        &offline,
    );
    d.load(&p, detail(0, 44, true, Some(39)));
    d.settle().await;
    let online = d.look(&p);
    let labels = d.labels(&p);
    let arrived = labels.iter().any(|label| label == "m43") && labels.iter().any(|label| label == "Unseen");
    d.check(
        "back online, replies from while it was parked come under a divider, below the place read",
        arrived && online.pill && online.top == reading.top,
        &online,
    );

    // 14. Parked at its newest: back with nothing new, it's at the newest; back with replies that came meanwhile, it
    //     shows where it was left with the divider and the pill, not scrolled through them unseen.
    d.jump(&p);
    d.settle().await;
    let newest = d.look(&p);
    d.check("at the newest before parking", newest.at_end, &newest);
    d.park(&p);
    d.show(&s);
    d.attend(&s, true);
    d.settle().await;
    d.attend(&s, false);
    d.come_back(&p);
    d.settle().await;
    let back = d.look(&p);
    d.check("parked at the newest and shown again, it's at the newest", back.at_end, &back);
    d.load(&p, detail(0, 44, true, Some(43)));
    d.settle().await;
    let same = d.look(&p);
    d.check("the server's answer with nothing new leaves it at the newest", same.at_end && !same.pill, &same);
    d.park(&p);
    d.show(&s);
    d.attend(&s, true);
    d.settle().await;
    d.attend(&s, false);
    d.come_back(&p);
    d.settle().await;
    d.load(&p, detail(0, 48, true, Some(43)));
    d.settle().await;
    let seen = d.look(&p);
    // Following the bottom, gpui gives no row's bounds, so the place it was left can't be read before: the divider
    // shows, and what's above it is what showed then.
    let known =
        seen.top.as_ref().and_then(|top| top.0.strip_prefix('m')?.parse::<i64>().ok()).is_some_and(|index| index < 44);
    d.check(
        "replies that came while parked wait under the divider, with the pill",
        seen.divider && known && seen.pill && !seen.at_end,
        &seen,
    );

    // 15. Pictures that load late (laid out 4:3 until they have decoded, then taller): at the newest it stays there as
    //     they load; left before one loaded and shown again after, still at the newest; reading above one that loads
    //     below, the place holds.
    let folder = picture_folder();
    let m = d.make();
    d.attend(&p, false);
    d.show(&m);
    d.attend(&m, true);
    d.load(&m, detail_with(0, 40, true, None, &[(37, format!("{folder}/a.png")), (39, format!("{folder}/b.png"))]));
    d.settle().await;
    let loading = d.look(&m);
    d.check("opened while its pictures load, at the newest", loading.at_end, &loading);
    d.wait(1200).await;
    let loaded = d.look(&m);
    d.check("its pictures loaded, still at the newest", loaded.at_end && !loaded.pill, &loaded);
    let n = d.make();
    d.attend(&m, false);
    d.show(&n);
    d.attend(&n, true);
    d.load(&n, detail_with(0, 40, true, None, &[(39, format!("{folder}/c.png"))]));
    d.settle().await;
    d.park(&n);
    d.show(&m);
    d.attend(&m, true);
    d.wait(1500).await;
    d.attend(&m, false);
    d.come_back(&n);
    d.settle().await;
    let back = d.look(&n);
    d.check("left while its picture loaded, back after (offline), at the newest", back.at_end, &back);
    d.load(&n, detail_with(0, 40, true, None, &[(39, format!("{folder}/c.png"))]));
    d.settle().await;
    let answered = d.look(&n);
    d.check("and the server's answer leaves it there", answered.at_end && !answered.pill, &answered);
    d.load(&n, detail_with(0, 40, true, None, &[(39, format!("{folder}/c.png")), (33, format!("{folder}/d.png"))]));
    d.read_at(&n, "m30", 20.);
    d.settle().await;
    let reading = d.look(&n);
    d.wait(1200).await;
    let grown = d.look(&n);
    d.check(
        "reading above a picture that loads below it, the place holds",
        grown.top.is_some() && grown.top == reading.top,
        &grown,
    );

    // 16. Older pages arriving while the reader is at the very top, the loading row in view (in an agent's chat and in
    //     the group): the message under it stays where it was on screen from the first frame the page shows in, as the
    //     page's rows settle, and when the last page (nothing older) takes the loading row away; no further page is
    //     asked for meanwhile, and the wheel scrolls from there as usual.
    let mut last = n.clone();
    for group in [false, true] {
        let name = if group { "in the group" } else { "in a chat" };
        let page =
            |from: i64, more: bool| if group { group_detail(from, 40, more) } else { detail(from, 40, more, None) };
        let o = d.make();
        if group {
            o.update(d.cx, |view, cx| view.set_group(group_speakers(), cx)).ok();
        }
        d.attend(&last, false);
        d.show(&o);
        d.attend(&o, true);
        d.load(&o, page(0, true));
        d.settle().await;
        for (from, more, first) in [(-40, true, "m0"), (-80, false, "m-40")] {
            d.read_top(&o);
            d.settle().await;
            let before = o.read_with(d.cx, |view, _| message_y(view, first)).ok().flatten();
            let limit = o.read_with(d.cx, |view, _| view.limit).unwrap_or_default();
            let loading =
                o.read_with(d.cx, |view, _| matches!(view.rows.first(), Some(ViewRow::Older))).unwrap_or(false);
            let seen = d.look(&o);
            d.check(
                &format!("{name}, at the top: the loading row and {first} show ({before:?})"),
                loading && before.is_some(),
                &seen,
            );
            d.load(&o, page(from, more));
            let mut ys = Vec::new();
            d.frame();
            ys.push(o.read_with(d.cx, |view, _| message_y(view, first)).ok().flatten());
            d.frame();
            ys.push(o.read_with(d.cx, |view, _| message_y(view, first)).ok().flatten());
            d.wait(400).await;
            ys.push(o.read_with(d.cx, |view, _| message_y(view, first)).ok().flatten());
            let kept =
                before.is_some() && ys.iter().all(|y| y.zip(before).is_some_and(|(y, before)| (y - before).abs() < 1.));
            let older = if more { "an older page" } else { "the last page" };
            let seen = d.look(&o);
            d.check(
                &format!("{name}, {older} arriving keeps {first} in place, frame by frame ({before:?} → {ys:?})"),
                kept,
                &seen,
            );
            let asked = o.read_with(d.cx, |view, _| view.limit).unwrap_or_default();
            let loading =
                o.read_with(d.cx, |view, _| matches!(view.rows.first(), Some(ViewRow::Older))).unwrap_or(false);
            let seen = d.look(&o);
            d.check(
                &format!("{name}, nothing more is asked for until the reader scrolls (limit {limit} → {asked})"),
                asked == limit && loading == more,
                &seen,
            );
        }
        let held = d.look(&o);
        d.wheel(&o, 150.);
        d.settle().await;
        let scrolled = d.look(&o);
        let moved = held
            .top
            .as_ref()
            .zip(scrolled.top.as_ref())
            .is_some_and(|(held, scrolled)| held.0 != scrolled.0 || scrolled.1 > held.1 + 100.);
        d.check(&format!("{name}, the wheel scrolls up from there"), moved, &scrolled);
        last = o;
    }

    // 17. The timeline (timeline.rs): a dash per sent message at the right edge, its strip clear of the bubbles in
    //     this narrow window; a hovered dash shows its message's card; pressing and dragging slides the chat with the
    //     bright place following; Esc puts it back; a click goes to that dash's message; a release that never came
    //     (a move with no button) still counts; dragging past the last dash goes to the newest.
    let t = d.make();
    d.attend(&last, false);
    d.show(&t);
    d.attend(&t, true);
    d.load(&t, detail(0, 60, false, None));
    d.settle().await;
    d.read_at(&t, "m30", 0.);
    d.settle().await;
    let seen = d.look(&t);
    let Some(rail) = d.timeline(&t) else {
        d.check("the timeline shows", false, &seen);
        return driver.failures;
    };
    d.check(&format!("the timeline shows a dash per sent message ({})", rail.dashes), rail.dashes == 30, &seen);
    let bubbles_end = t
        .read_with(d.cx, |view, _| view.list_state.viewport_bounds().right() - px(crate::theme::PAGE_PADDING))
        .expect("view");
    let gap = f32::from(rail.strip.left() - bubbles_end);
    d.check(
        &format!("its {} px strip leaves {gap} px to the bubbles", f32::from(rail.strip.size.width)),
        gap >= 8. && rail.strip.size.width >= px(36.),
        &seen,
    );
    let x = rail.strip.center().x;
    let at = |dash: f32| gpui::point(x, Timeline::dash_y(&rail, dash));
    d.check(
        &format!("reading m30, the bright place is dash 15 ({:.2})", rail.place),
        (rail.place - 15.).abs() < 0.5,
        &seen,
    );
    d.shot("timeline-rest");

    d.pointer(at(5.), Pointer::Hover).await;
    let hovered = d.timeline(&t).expect("timeline");
    d.check(
        &format!("hovering dash 5 shows its message's card ({:?})", hovered.card),
        hovered.hover == Some(5) && hovered.card.as_deref() == Some("m10"),
        &seen,
    );
    d.shot("timeline-hover");

    let before = d.top_row(&t);
    d.pointer(at(5.), Pointer::Down).await;
    let mut places = Vec::new();
    let mut tops = Vec::new();
    for step in 1..=10 {
        d.pointer(at(5. + step as f32 * 2.), Pointer::Drag).await;
        let drawn = d.timeline(&t).expect("timeline");
        places.push(drawn.place);
        tops.push(d.top_row(&t).item_ix);
    }
    let dragging = d.timeline(&t).expect("timeline");
    d.shot("timeline-dragging");
    let smooth = places.windows(2).all(|pair| pair[1] > pair[0]) && tops.windows(2).all(|pair| pair[1] > pair[0]);
    d.check(&format!("dragging down the strip slides the chat down, step by step (rows {tops:?})"), smooth, &seen);
    d.check(
        &format!(
            "the bright place follows the pointer to dash 25 ({:.2}) with its card ({:?})",
            dragging.place, dragging.card
        ),
        (dragging.place - 25.).abs() < 0.01 && dragging.card.as_deref() == Some("m50"),
        &seen,
    );
    d.escape().await;
    let after = d.top_row(&t);
    let cancelled = d.timeline(&t).expect("timeline");
    d.check(
        &format!("Esc puts the chat back ({before:?} → {after:?})"),
        (after.item_ix, after.offset_in_item) == (before.item_ix, before.offset_in_item) && !cancelled.pressed,
        &seen,
    );
    d.pointer(at(25.), Pointer::Up).await;

    d.pointer(at(3.), Pointer::Down).await;
    d.pointer(at(3.), Pointer::Up).await;
    let clicked = d.look(&t);
    d.check("a click on dash 3 goes to its message", clicked.top.as_ref().is_some_and(|(id, _)| id == "m6"), &clicked);

    d.pointer(at(20.), Pointer::Down).await;
    d.pointer(at(20.), Pointer::Hover).await;
    let lost = d.look(&t);
    let released = d.timeline(&t).expect("timeline");
    d.check(
        "a press whose release never came ends on the next move, as a click",
        !released.pressed && lost.top.as_ref().is_some_and(|(id, _)| id == "m40"),
        &lost,
    );

    d.pointer(at(10.), Pointer::Down).await;
    d.pointer(gpui::point(x, rail.strip.bottom() - px(2.)), Pointer::Drag).await;
    d.pointer(gpui::point(x, rail.strip.bottom() - px(2.)), Pointer::Up).await;
    let end = d.look(&t);
    let last = d.timeline(&t).expect("timeline");
    d.check(
        &format!("dragging past the last dash goes to the newest, the last dash bright ({:.2})", last.place),
        end.at_end && last.place > 28.9,
        &end,
    );

    driver.failures
}

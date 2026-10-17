// `FILO_OPEN=chat-overlap`: an agent's replies coming one after another while it works, in a window of its own (no
// backend), in the system font at 16 px, as in the chat they were reported in. Checks, frame by frame, that each
// message's bubble ends above the next one's top while the Thinking row's activity changes, as replies arrive mid-turn,
// as a reply's text grows and once the turn ends, and that every bubble is as tall as in a chat opened fresh on the same
// messages. The report's "✅ **Fable** is public as …" lies on one line exactly as wide as the column at that size, and
// was drawn over the reply above it (`Conversation::row_shell`). Prints a line per check and quits, exit 1 when one
// fails. `FILO_FIXTURE_SHOTS=<dir>` saves a screenshot at each step.

use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, Bounds, Context, Entity, IntoElement, ParentElement as _, Pixels, Render,
    Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::{Conversation, Detail, ViewRow};

/// Wide enough that the chat's column is at its widest (720 inside the padding), as in a full-size window.
const WIDTH: f32 = 1012.;

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-overlap")
}

pub fn open(cx: &mut App) {
    // The report's text: the system font at 16 px.
    let mut prefs = crate::config::load_prefs();
    prefs.font_family = ".SystemUIFont".into();
    prefs.font_size = 16.;
    crate::theme::apply(prefs, None, cx);
    crate::smoke::fixtures::open(
        cx,
        "chat-overlap",
        "Filo chat overlap",
        (WIDTH, 760.),
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

const MINUTE: f64 = 60_000.;

fn base() -> f64 {
    static BASE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| crate::format::now_ms() - 30. * MINUTE)
}

fn message(id: &str, user: bool, minute: f64, content: &str) -> Value {
    json!({ "_id": id, "_creationTime": base() + minute * MINUTE, "role": if user { "user" } else { "assistant" }, "content": content })
}

/// The chat in the report: an agent's plan, the user's go-ahead, and the agent's replies coming in under it.
fn opening() -> Vec<Value> {
    vec![
        message("ask", true, 0., "so we can't upload the fable's video to product hunt?"),
        message(
            "plan",
            false,
            1.,
            "We probably can, actually. After I saved Opus, Product Hunt moved it into the **gallery** as a tile and the video field went **empty** again, so a second video looks possible.\n\nThere's one catch: Product Hunt only finds **public** YouTube videos, and Fable is unlisted. So I'd:\n1. Make Fable public and title it **\"Filo demo: Ship an app (made by Fable)\"**.\n2. Add its link the same way and save. If Product Hunt replaces Opus instead of adding a second video, I'll put Opus back and tell you.\n\nGo ahead? And should Sonnet go in too, the same way?",
        ),
        message("go", true, 2., "yes yes"),
    ]
}

const ON_IT: &str = "On it: Fable first, then Sonnet. Each gets \"(made by \u{2026})\" in its title, goes public, and gets added to Product Hunt.";
const FABLE: &str = "\u{2705} **Fable** is public as \"Filo demo: Ship an app (made by Fable)\". Doing Sonnet now.";
const SONNET: &str = "Sonnet's title now ends in \"(made by Sonnet)\". Making it public. Also a correction: Sonnet's real link ends in a capital I, **https://youtu.be/DwL1dornjgI**. The one I sent you earlier had a lowercase l and doesn't work.";

fn detail(messages: &[Value]) -> Detail {
    serde_json::from_value(
        json!({ "session": { "_id": "overlap-chat", "title": "Overlap" }, "messages": messages, "hasMore": false }),
    )
    .expect("fixture detail")
}

/// The user's go-ahead's turn: under way (with what the agent is doing) or ended.
fn turn(activity: Option<&str>, state: &str) -> Value {
    json!([{ "turnId": "turn-go", "requestId": "request-go", "userMessageId": "go", "state": state, "createdAt": base() + 2. * MINUTE, "activity": activity }])
}

/// The messages' bubbles as last drawn, top to bottom: (message id, bounds).
type Bubbles = Vec<(String, Bounds<Pixels>)>;

struct Driver<'a> {
    view: Entity<Conversation>,
    cx: &'a mut AsyncWindowContext,
    messages: Vec<Value>,
    failures: usize,
    shots: Option<String>,
}

impl Driver<'_> {
    async fn wait(&mut self, ms: u64) {
        self.cx.background_executor().timer(Duration::from_millis(ms)).await;
    }

    fn shot(&mut self, name: &str) {
        crate::smoke::fixtures::shot(self.shots.as_deref(), WIDTH as f64, name);
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        if ok {
            println!("chat-overlap: {name}: ok");
        } else {
            self.failures += 1;
            println!("chat-overlap: {name}: FAILED {detail:?}");
        }
    }

    fn load(&mut self) {
        let detail = detail(&self.messages);
        self.view
            .update(self.cx, |view, cx| {
                view.detail = Some(Ok(Some(detail)));
                view.data_changed(cx);
            })
            .ok();
    }

    fn set_turn(&mut self, activity: Option<&str>, state: &str) {
        let turns = serde_json::from_value(turn(activity, state)).expect("fixture turns");
        self.view
            .update(self.cx, |view, cx| {
                view.turns = turns;
                view.data_changed(cx);
            })
            .ok();
    }

    /// The bubbles drawn in the next frame, in the feed's order.
    async fn bubbles(&mut self) -> Bubbles {
        let _ = self.view.read_with(self.cx, |view, _| view.drawn.borrow_mut().clear());
        let _ = self.cx.update(|window, _| window.refresh());
        self.wait(40).await;
        self.view
            .read_with(self.cx, |view, _| {
                let drawn = view.drawn.borrow();
                view.rows
                    .iter()
                    .filter_map(|row| match row {
                        ViewRow::Message { message, .. } => {
                            drawn.get(&format!("bubble-{}", message.id)).map(|bounds| (message.id.clone(), *bounds))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Bubbles that reach into the one after them: (upper, lower, how far).
    fn overlaps(bubbles: &Bubbles) -> Vec<(String, String, f32)> {
        bubbles
            .windows(2)
            .filter(|pair| pair[0].1.bottom() > pair[1].1.top() + px(0.5))
            .map(|pair| (pair[0].0.clone(), pair[1].0.clone(), f32::from(pair[0].1.bottom() - pair[1].1.top())))
            .collect()
    }

    /// Watches `frames` frames, checking each one's bubbles don't overlap; the bubbles of the last.
    async fn watch(&mut self, name: &str, frames: usize) -> Bubbles {
        let mut bad = Vec::new();
        let mut last = Vec::new();
        for _ in 0..frames {
            last = self.bubbles().await;
            bad.extend(Self::overlaps(&last));
        }
        bad.dedup();
        self.check(&format!("{name}: no bubble reaches into the next"), bad.is_empty() && !last.is_empty(), &bad);
        last
    }

    /// Each bubble as tall as in a chat opened fresh on the same messages.
    fn same_heights(&mut self, name: &str, live: &Bubbles, fresh: &Bubbles) {
        let off: Vec<_> = live
            .iter()
            .filter_map(|(id, bounds)| {
                let (_, other) = fresh.iter().find(|(other, _)| other == id)?;
                ((bounds.size.height - other.size.height).abs() > px(0.5))
                    .then(|| (id.clone(), f32::from(bounds.size.height), f32::from(other.size.height)))
            })
            .collect();
        self.check(
            &format!("{name}: every bubble is as tall as in the chat opened fresh"),
            off.is_empty() && !live.is_empty(),
            &off,
        );
    }
}

fn new_view(cx: &mut AsyncWindowContext) -> Entity<Conversation> {
    cx.update(|window, cx| {
        let view = cx.new(|cx| Conversation::new(None, "Product Launch".into(), window, cx));
        view.update(cx, |view, _| {
            view.fixture = true;
            view.session_id = Some("overlap-chat".into());
        });
        view
    })
    .expect("window")
}

fn show(host: &Entity<Host>, view: &Entity<Conversation>, cx: &mut AsyncWindowContext) {
    let shown = view.clone();
    host.update(cx, |host, cx| {
        host.shown = Some(shown);
        cx.notify();
    })
    .ok();
    view.update(cx, |view, cx| view.set_attending(true, cx)).ok();
}

async fn drive(host: Entity<Host>, cx: &mut AsyncWindowContext) -> usize {
    let view = new_view(cx);
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok();
    if let Some(dir) = &shots {
        let _ = std::fs::create_dir_all(dir);
    }
    show(&host, &view, cx);
    let mut driver = Driver { view, cx, messages: opening(), failures: 0, shots };
    let d = &mut driver;
    d.load();
    d.set_turn(Some("Thinking"), "pending");
    d.wait(300).await;

    // 1. The first reply comes while the agent works, and the activity under it changes.
    d.messages.push(message("on-it", false, 3., ON_IT));
    d.load();
    d.watch("first reply mid-turn", 6).await;
    for activity in ["Running a command", "Clicking in Helium", "Running a command"] {
        d.set_turn(Some(activity), "pending");
        d.watch(&format!("activity \u{201c}{activity}\u{201d}"), 4).await;
    }
    d.shot("1-first-reply");

    // 2. The next reply arrives under it, still mid-turn ("Running a command" under them, as in the report).
    d.messages.push(message("fable", false, 4., FABLE));
    d.load();
    d.watch("second reply mid-turn", 10).await;
    d.shot("2-second-reply");

    // 3. A reply whose text grows as it comes in, a few words at a time.
    let words: Vec<&str> = SONNET.split(' ').collect();
    d.messages.push(message("sonnet", false, 5., ""));
    for count in (3..=words.len()).step_by(4).chain([words.len()]) {
        let text = words[..count].join(" ");
        if let Some(last) = d.messages.last_mut() {
            last["content"] = json!(text);
        }
        d.load();
        d.watch(&format!("growing reply, {count} words"), 2).await;
    }
    let live = d.watch("growing reply, done", 6).await;
    d.shot("3-growing-reply");

    // 4. The turn ends.
    d.set_turn(None, "completed");
    let ended = d.watch("turn ended", 6).await;
    d.shot("4-turn-ended");

    // The same messages in a chat opened on them, no turn under way: the bubbles measure as they did live.
    let fresh_view = new_view(d.cx);
    show(&host, &fresh_view, d.cx);
    d.view = fresh_view;
    d.load();
    d.wait(300).await;
    let fresh = d.watch("opened fresh", 4).await;
    d.shot("5-opened-fresh");
    d.same_heights("mid-turn", &live, &fresh);
    d.same_heights("turn ended", &ended, &fresh);
    driver.failures
}

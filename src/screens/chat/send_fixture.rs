// `FILO_OPEN=chat-send`: sending messages, on a made-up chat in a window of its own (no backend; uploads
// and deliveries are scripted, slow or failing on cue). Checks that the bubble a message shows as while it's sent is the
// one it stays once the backend has it (same row, same place, same size), that a picture shows at once from its file
// (no spinner over it while it uploads), opens full size from that file at once, and isn't downloaded when the sent
// message arrives, that a failed upload or delivery keeps the bubble, with Retry under it, until it goes, and that
// pressing Quote over a reply's text quotes the words selected in it. Prints a line per check and quits, exit 1 when one fails. `FILO_FIXTURE_SHOTS=<dir>`
// saves a screenshot at each step.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncWindowContext, Bounds, Context, Entity, Global, IntoElement, ParentElement as _, Pixels,
    Render, Resource, Styled as _, Task, Window, div, px,
};
use gpui_component::WindowExt as _;
use serde_json::{Value, json};

use super::{Conversation, Detail, Uploaded, ViewRow};
use crate::screens::chat::attachments;

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-send")
}

pub fn open(cx: &mut App) {
    crate::smoke::fixtures::open(
        cx,
        "chat-send",
        "Filo chat send",
        (760., 720.),
        false,
        |_, cx| cx.new(|_| Host { shown: None }),
        drive,
    );
}

struct Host {
    shown: Option<Entity<Conversation>>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The dialog layer, as the app shell draws it: the picture viewer opens there.
        div().size_full().children(self.shown.clone()).children(gpui_component::Root::render_dialog_layer(window, cx))
    }
}

/// How the next upload or delivery goes: after how long, and whether it fails.
#[derive(Clone, Copy)]
struct Step {
    after: Duration,
    fails: bool,
}

const QUICK: Step = Step { after: Duration::from_millis(50), fails: false };

/// What the scripted backend does next, and the `sendMessage` args it was given.
#[derive(Default)]
struct Script {
    uploads: VecDeque<Step>,
    deliveries: VecDeque<Step>,
    delivered: Vec<Value>,
    /// The file each upload was read from, by its storage id: the backend keeps its own copy of each.
    files: std::collections::HashMap<String, PathBuf>,
}

impl Global for Script {}

/// The scripted `attachments::upload`.
pub(super) fn upload(path: PathBuf, name: String, cx: &mut Context<Conversation>) -> Task<Result<Uploaded, String>> {
    let step = cx.default_global::<Script>().uploads.pop_front().unwrap_or(QUICK);
    let content_type = attachments::content_type_for(&path);
    let size = std::fs::metadata(&path).map(|meta| meta.len() as usize).unwrap_or(0);
    let timer = cx.background_executor().timer(step.after);
    cx.spawn(async move |_, cx| {
        timer.await;
        if step.fails {
            return Err(attachments::UPLOAD_FAILED.to_string());
        }
        let key = format!("fixture/{}", uuid::Uuid::new_v4());
        cx.update(|cx| cx.default_global::<Script>().files.insert(key.clone(), path)).ok();
        Ok(Uploaded { name, content_type, size, key })
    })
}

/// How many messages the scripted `voiceSessions:sendMessage` was given (composer_fixture.rs).
pub(super) fn delivered_count(cx: &mut App) -> usize {
    cx.default_global::<Script>().delivered.len()
}

/// The scripted `voiceSessions:sendMessage`.
pub(super) fn deliver(args: Value, cx: &mut Context<Conversation>) -> Task<Result<Value, String>> {
    let step = cx.default_global::<Script>().deliveries.pop_front().unwrap_or(QUICK);
    let timer = cx.background_executor().timer(step.after);
    cx.spawn(async move |_, cx| {
        timer.await;
        if step.fails {
            return Err("network error".to_string());
        }
        cx.update(|cx| cx.default_global::<Script>().delivered.push(args)).ok();
        Ok(json!({ "turnId": "fixture-turn" }))
    })
}

const MINUTE: f64 = 60_000.;
const SETTLE: Duration = Duration::from_millis(250);

fn base() -> f64 {
    static BASE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| crate::format::now_ms() - 30. * MINUTE)
}

/// A made-up chat: turns enough to be taller than the window, the user's last.
fn opening() -> Vec<Value> {
    (0..14)
        .map(|index| {
            let user = index % 2 == 0;
            let content = if user { format!("Question {index}?") } else { format!("Reply {index}, long enough to wrap over a couple of lines of the bubble so rows aren't all one height.") };
            json!({ "_id": format!("m{index}"), "_creationTime": base() + index as f64 * MINUTE, "role": if user { "user" } else { "assistant" }, "content": content })
        })
        .collect()
}

fn detail(messages: &[Value]) -> Detail {
    serde_json::from_value(
        json!({ "session": { "_id": "send-chat", "title": "Send" }, "messages": messages, "hasMore": false }),
    )
    .expect("fixture detail")
}

/// The message the backend makes of `args`, as `voiceSessions:detail` returns it: each file with its copy on this Mac
/// (`files`, by storage id), as the local backend sends it.
fn stored(args: &Value, index: usize, files: &std::collections::HashMap<String, PathBuf>) -> Value {
    let attachments: Vec<Value> = args["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut attachment| {
            attachment["localFile"] = json!(attachment["storageId"].as_str().and_then(|key| files.get(key)));
            attachment
        })
        .collect();
    json!({
        "_id": format!("server-{index}"),
        "_creationTime": crate::format::now_ms(),
        "role": "user",
        "content": args["message"],
        "attachments": attachments,
    })
}

/// A test picture, 1200 × 800, in a folder of the fixture's own.
fn photo() -> PathBuf {
    let dir = std::env::temp_dir().join("filo-chat-send-fixture");
    let _ = std::fs::create_dir_all(&dir);
    // Camera-sized, so it takes longer than a frame to decode, as a real photo does: a picture sent before it has
    // decoded must still be drawn at its own shape.
    let path = dir.join("photo.jpg");
    let picture = image::RgbImage::from_fn(4800, 3200, |x, y| {
        image::Rgb([(x / 19) as u8, (y / 13) as u8, ((x ^ y) % 97 + 120) as u8])
    });
    picture.save(&path).expect("test picture");
    path
}

/// A picture as pasting one saves it (pasteboard.rs): in the config folder, where it stays, so it goes with its path.
fn pasted_picture() -> PathBuf {
    let dir = crate::config::config_dir().join("pasted");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("pasted-image-2026-10-10-191941.png");
    let picture = image::RgbImage::from_fn(600, 400, |x, y| image::Rgb([(x / 3) as u8, (y / 2) as u8, 160]));
    picture.save(&path).expect("pasted picture");
    path
}

/// Where a message is: its row's index in the feed, and the window bounds its bubble and pictures were drawn at,
/// found by the element ids it draws with (the same ids from sending to sent, or they're not found).
#[derive(Clone, Debug, PartialEq)]
struct Place {
    index: usize,
    bubble: Option<Bounds<Pixels>>,
    media: Option<Bounds<Pixels>>,
}

fn near(a: Option<Bounds<Pixels>>, b: Option<Bounds<Pixels>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            (a.origin.x - b.origin.x).abs() < px(0.5)
                && (a.origin.y - b.origin.y).abs() < px(0.5)
                && (a.size.width - b.size.width).abs() < px(0.5)
                && (a.size.height - b.size.height).abs() < px(0.5)
        }
        (None, None) => true,
        _ => false,
    }
}

struct Driver<'a> {
    host: Entity<Host>,
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

    async fn settle(&mut self) {
        self.cx.background_executor().timer(SETTLE).await;
    }

    fn shot(&mut self, name: &str) {
        crate::smoke::fixtures::shot(self.shots.as_deref(), 760., name);
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Debug) {
        if ok {
            println!("chat-send: {name}: ok");
        } else {
            self.failures += 1;
            println!("chat-send: {name}: FAILED {detail:?}");
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

    /// The backend's copy of the last message delivered arriving, then its turn, message `key` checked to stay where it
    /// was at `first` after each.
    async fn acknowledge(&mut self, what: &str, key: &Option<String>, first: &Option<Place>) {
        let Some((args, files)) = self
            .cx
            .update(|_, cx| {
                let script = cx.default_global::<Script>();
                Some((script.delivered.last().cloned()?, script.files.clone()))
            })
            .ok()
            .flatten()
        else {
            return;
        };
        let index = self.messages.len();
        let message = stored(&args, index, &files);
        self.messages.push(message);
        self.load();
        self.settle().await;
        let pending = self.pending();
        self.check(&format!("{what}: the server's copy takes the sending bubble's place"), !pending.0, pending);
        self.kept(&format!("{what}: stays put when the server's copy arrives"), key, first).await;
        let turns = serde_json::from_value(json!([{
            "turnId": format!("turn-{index}"), "requestId": args["requestId"], "userMessageId": format!("server-{index}"), "state": "pending", "createdAt": 0
        }]))
        .expect("fixture turns");
        self.view
            .update(self.cx, |view, cx| {
                view.turns = turns;
                view.data_changed(cx);
            })
            .ok();
        self.settle().await;
        self.kept(&format!("{what}: stays put when its reply starts"), key, first).await;
    }

    /// The reply to it finishing (no turn under way, so the next send starts from a still chat).
    async fn reply(&mut self) {
        let index = self.messages.len();
        self.messages.push(json!({ "_id": format!("reply-{index}"), "_creationTime": crate::format::now_ms(), "role": "assistant", "content": "Got it." }));
        self.load();
        self.view
            .update(self.cx, |view, cx| {
                view.turns.clear();
                view.data_changed(cx);
            })
            .ok();
        self.settle().await;
    }

    /// The bubble id of the message being sent.
    fn key(&mut self) -> Option<String> {
        self.view.read_with(self.cx, |view, _| view.pending.as_ref().map(|pending| pending.key.clone())).ok().flatten()
    }

    /// Where message `key` is drawn: the one being sent, or the backend's copy of it.
    /// Where message `key` is drawn now: forgets where it was drawn before and waits for the next frame to draw it
    /// again (a window macOS has covered may not draw for a while).
    async fn place(&mut self, key: &Option<String>) -> Option<Place> {
        let key = key.clone()?;
        let names = [format!("bubble-{key}"), format!("media-{key}")];
        let _ = self
            .view
            .read_with(self.cx, |view, _| names.iter().for_each(|name| _ = view.drawn.borrow_mut().remove(name)));
        let _ = self.cx.update(|window, _| window.refresh());
        for _ in 0..100 {
            if self.view.read_with(self.cx, |view, _| view.drawn.borrow().contains_key(&names[0])).unwrap_or(true) {
                break;
            }
            self.wait(16).await;
        }
        self.view
            .read_with(self.cx, |view, _| {
                let index = view.rows.iter().position(|row| {
                    matches!(row, ViewRow::Message { message, .. } if message.id == key || view.handed_over.get(&message.id) == Some(&key))
                })?;
                let drawn = view.drawn.borrow();
                Some(Place {
                    index,
                    bubble: drawn.get(&format!("bubble-{key}")).copied(),
                    media: drawn.get(&format!("media-{key}")).copied(),
                })
            })
            .ok()
            .flatten()
    }

    /// The feed held a point short of its end, as `hold_position` leaves it while the user is away (or a scroll that
    /// stops just short): not following the end, so a send has to bring the end into view itself. The scrollable height.
    async fn short_of_end(&mut self) -> f32 {
        let max = self
            .view
            .update(self.cx, |view, _| {
                let max = view.list_state.max_offset_for_scrollbar().height;
                view.list_state.set_offset_from_scrollbar(gpui::point(px(0.), -(max - px(1.))));
                f32::from(max)
            })
            .unwrap_or(0.);
        let _ = self.cx.update(|window, _| window.refresh());
        self.settle().await;
        max
    }

    /// Message `key` all in view, above the composer, and no pill to jump down.
    async fn in_view(&mut self, name: &str, key: &Option<String>) {
        let place = self.place(key).await;
        let (viewport, composer, pill) = self
            .view
            .read_with(self.cx, |view, _| (view.list_state.viewport_bounds(), view.composer_height, !view.at_end))
            .unwrap_or_default();
        let ok = place.as_ref().and_then(|place| place.bubble).is_some_and(|bubble| {
            bubble.top() >= viewport.top() && bubble.bottom() <= viewport.bottom() - px(composer)
        });
        self.check(name, ok && !pill, (&place, viewport, composer, pill));
    }

    /// Message `key` drawn where it was at `first`: same row, row box, bubble and pictures, to half a pixel.
    async fn kept(&mut self, name: &str, key: &Option<String>, first: &Option<Place>) {
        let now = self.place(key).await;
        let same = match (first, &now) {
            // Only a drawn bubble counts: two missing ones aren't proof that nothing moved.
            (Some(a), Some(b)) => {
                a.index == b.index && a.bubble.is_some() && near(a.bubble, b.bubble) && near(a.media, b.media)
            }
            _ => false,
        };
        self.check(name, same, (first, &now));
    }

    fn pending(&mut self) -> (bool, Option<String>) {
        self.view
            .read_with(self.cx, |view, _| (view.pending.is_some(), view.pending.as_ref().and_then(|p| p.error.clone())))
            .unwrap_or((false, None))
    }

    fn type_and_send(&mut self, text: &str) {
        let text = text.to_string();
        let view = self.view.clone();
        self.cx
            .update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.input.update(cx, |input, cx| input.set_value(text, window, cx));
                    view.send(window, cx);
                })
            })
            .ok();
    }

    /// Attaches `path` as the composer does a picked or pasted file, a chip at the cursor, types `text` after it and
    /// sends.
    fn attach_and_send(&mut self, path: &Path, text: &str) {
        let (view, path, text) = (self.view.clone(), path.to_path_buf(), text.to_string());
        self.cx
            .update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.input.update(cx, |input, cx| input.set_value("", window, cx));
                    view.attach_files(vec![path], window, cx);
                    view.input.update(cx, |input, cx| input.insert(text, window, cx));
                    view.send(window, cx);
                })
            })
            .ok();
    }

    /// The text message `key` shows, the one being sent or the backend's copy.
    fn shown_text(&mut self, key: &Option<String>) -> Option<String> {
        let key = key.clone()?;
        self.view
            .read_with(self.cx, |view, _| {
                view.rows.iter().find_map(|row| match row {
                    ViewRow::Message { message, .. }
                        if message.id == key || view.handed_over.get(&message.id) == Some(&key) =>
                    {
                        Some(Conversation::shown_text(message))
                    }
                    _ => None,
                })
            })
            .ok()
            .flatten()
    }

    fn add(&mut self, path: &Path) {
        let (view, path) = (self.view.clone(), path.to_path_buf());
        self.cx.update(|window, cx| view.update(cx, |view, cx| view.add_files(vec![path], window, cx))).ok();
    }

    /// A click in the middle of `bounds`, posted to the window as the pointer makes it.
    fn click(&mut self, bounds: Bounds<Pixels>) -> bool {
        let center = bounds.center();
        crate::smoke::native_input::click(760., f64::from(f32::from(center.x)), f64::from(f32::from(center.y)))
    }

    fn viewer_open(&mut self) -> bool {
        self.cx.update(|window, cx| window.has_active_dialog(cx)).unwrap_or(false)
    }

    fn close_viewer(&mut self) {
        self.cx.update(|window, cx| window.close_dialog(cx)).ok();
    }

    fn retry(&mut self) {
        let view = self.view.clone();
        self.cx.update(|window, cx| view.update(cx, |view, cx| view.retry_pending(window, cx))).ok();
    }

    fn script(&mut self, uploads: &[Step], deliveries: &[Step]) {
        let (uploads, deliveries) = (uploads.to_vec(), deliveries.to_vec());
        self.cx
            .update(|_, cx| {
                let script = cx.default_global::<Script>();
                script.uploads = uploads.into();
                script.deliveries = deliveries.into();
            })
            .ok();
    }

    fn delivered(&mut self) -> usize {
        self.cx.update(|_, cx| cx.default_global::<Script>().delivered.len()).unwrap_or(0)
    }

    /// Where the Quote button was drawn, once it shows (a frame or two after the selection ends).
    async fn quote_button(&mut self) -> Option<Bounds<Pixels>> {
        let _ = self.view.read_with(self.cx, |view, _| view.drawn.borrow_mut().remove("quote-selection"));
        for _ in 0..60 {
            let _ = self.cx.update(|window, _| window.refresh());
            self.wait(16).await;
            if let Some(bounds) = self
                .view
                .read_with(self.cx, |view, _| view.drawn.borrow().get("quote-selection").copied())
                .ok()
                .flatten()
            {
                return Some(bounds);
            }
        }
        None
    }

    async fn quote(&mut self) {
        const REPLY: &str = "The Quote button now takes its own click: clicking it only quotes, and nothing underneath it reacts \
            (no row, no link, no new selection). This holds whether it shows below the selected text or is pushed up over it. \
            It also tested with real mouse clicks on the markdown compare screen, which uses the chat's own Quote button, in \
            both positions. It didn't test over a finished-result row in the real chat, so please try that on your Mac.";
        let id = format!("reply-{}", self.messages.len());
        self.messages.push(
            json!({ "_id": id, "_creationTime": crate::format::now_ms(), "role": "assistant", "content": REPLY }),
        );
        self.load();
        self.settle().await;
        let Some(bubble) = self.place(&Some(id)).await.and_then(|place| place.bubble) else {
            self.check("quote: the reply is drawn", false, "no bubble");
            return;
        };
        // From the reply's first word to the middle of a line halfway down: Quote shows over the line under it.
        let (x, y, w, h) = (
            f64::from(f32::from(bubble.origin.x)),
            f64::from(f32::from(bubble.origin.y)),
            f64::from(f32::from(bubble.size.width)),
            f64::from(f32::from(bubble.size.height)),
        );
        let (from, to) = ((x + 14., y + 14.), (x + w * 0.45, y + h * 0.5));
        use crate::smoke::native_input::{Drag, drag_step, hover};
        hover(760., from.0, from.1);
        self.wait(100).await;
        drag_step(760., Drag::Down, from.0, from.1);
        self.wait(100).await;
        for step in 1..=10 {
            let t = f64::from(step) / 10.;
            drag_step(760., Drag::Move, from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.wait(30).await;
        }
        drag_step(760., Drag::Up, to.0, to.1);
        self.settle().await;
        let button = self.quote_button().await;
        self.check("quote: Quote shows under the selection", button.is_some(), button);
        self.shot("5-quote-selected");
        let Some(button) = button else { return };
        let over_text = button.origin.y < bubble.bottom() - px(7.) && button.origin.x < bubble.right();
        self.check("quote: Quote sits over the reply's text", over_text, (button, bubble));
        // Pressed the way a hand does: the pointer comes to it, and the button is held a moment before it's let go.
        let (bx, by) = (f64::from(f32::from(button.center().x)), f64::from(f32::from(button.center().y)));
        for step in 0..=6 {
            let t = f64::from(step) / 6.;
            hover(760., to.0 + (bx - to.0) * t, to.1 + (by - to.1) * t);
            self.wait(16).await;
        }
        drag_step(760., Drag::Down, bx, by);
        self.wait(120).await;
        let composer =
            self.view.read_with(self.cx, |view, cx| view.input.read(cx).value().to_string()).unwrap_or_default();
        self.check(
            "quote: pressing Quote over the text puts the selection in the composer, before it's let go",
            composer.starts_with("> The Quote button now takes its own click")
                && composer.contains("real mouse clicks"),
            &composer,
        );
        drag_step(760., Drag::Up, bx, by);
        self.settle().await;
        self.shot("5-quote-pressed");
        let after =
            self.view.read_with(self.cx, |view, cx| view.input.read(cx).value().to_string()).unwrap_or_default();
        let shown = self.quote_button().await;
        self.check(
            "quote: letting go quotes nothing more and selects nothing",
            after == composer && shown.is_none(),
            (&after, shown),
        );
    }
}

async fn drive(host: Entity<Host>, cx: &mut AsyncWindowContext) -> usize {
    let view = cx
        .update(|window, cx| {
            let view = cx.new(|cx| Conversation::new(None, "Filo".into(), window, cx));
            view.update(cx, |view, _| {
                view.fixture = true;
                view.session_id = Some("send-chat".into());
            });
            view
        })
        .expect("window");
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok();
    if let Some(dir) = &shots {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut driver = Driver { host, view, cx, messages: opening(), failures: 0, shots };
    let d = &mut driver;
    let shown = d.view.clone();
    d.host
        .update(d.cx, |host, cx| {
            host.shown = Some(shown);
            cx.notify();
        })
        .ok();
    d.view.update(d.cx, |view, cx| view.set_attending(true, cx)).ok();
    d.load();
    d.settle().await;

    // 1. Text: the bubble it shows as while it's sent is the one it stays, through delivery and the backend's copy.
    d.script(&[], &[Step { after: Duration::from_millis(400), fails: false }]);
    let max = d.short_of_end().await;
    d.check("text: the chat is taller than its window, held short of its end", max > 1., max);
    d.type_and_send("How's the weather looking for tomorrow's walk?");
    d.wait(20).await;
    let key = d.key();
    let first = d.place(&key).await;
    d.check(
        "text: shows at once, drawn as a bubble",
        first.as_ref().is_some_and(|place| place.bubble.is_some()),
        &first,
    );
    d.in_view("text: shows above the composer, no pill", &key).await;
    d.shot("1-text-sending");
    d.wait(200).await;
    d.kept("text: stays put while it's delivered", &key, &first).await;
    d.wait(400).await;
    d.kept("text: stays put once delivered", &key, &first).await;
    d.acknowledge("text", &key, &first).await;
    d.shot("1-text-sent");
    d.reply().await;
    d.in_view("text: still above the composer with its reply under it", &key).await;
    d.shot("1-text-replied");

    // 2. A picture sent straight after it's attached (before the composer has drawn or decoded it), while it's still
    //    uploading: shows at once from its file, at its own shape, and is loaded once, never downloaded.
    let photo = photo();
    let resource = Resource::Path(photo.as_path().into());
    d.script(&[Step { after: Duration::from_millis(1500), fails: false }], &[QUICK]);
    let started = d.cx.update(|_, cx| crate::pictures::stats(cx).started).unwrap_or(0);
    d.add(&photo);
    d.type_and_send("Here's the view from the hill");
    d.wait(20).await;
    let key = d.key();
    let first = d.place(&key).await;
    let state = d.cx.update(|_, cx| crate::pictures::state_of(&resource, cx)).unwrap_or("none");
    let delivered = d.delivered();
    d.check(
        "picture: shows at once while it uploads",
        delivered == 1 && first.as_ref().is_some_and(|place| place.media.is_some()),
        &first,
    );
    d.check("picture: drawn from its file", matches!(state, "loading" | "loaded"), state);
    println!("chat-send: picture: its file was {state} when first drawn");
    let shape = first
        .as_ref()
        .and_then(|place| place.media)
        .map(|media| f32::from(media.size.width) / f32::from(media.size.height));
    d.check(
        "picture: drawn at its own shape (3:2), not a placeholder's",
        shape.is_some_and(|shape| (shape - 1.5).abs() < 0.05),
        shape,
    );
    d.shot("2-picture-uploading");
    // Opens full size at once while it uploads, from its file: no URL yet, nothing loaded again or downloaded.
    let clicked = first.as_ref().and_then(|place| place.media).is_some_and(|media| d.click(media));
    d.settle().await;
    let open = d.viewer_open();
    let (loads, state, url_state) =
        d.cx.update(|_, cx| {
            (
                crate::pictures::stats(cx).started,
                crate::pictures::state_of(&resource, cx),
                crate::pictures::state("", cx),
            )
        })
        .unwrap_or((0, "none", "none"));
    let delivered = d.delivered();
    d.check(
        "picture: opens full size from its file while it uploads",
        clicked
            && open
            && delivered == 1
            && loads == started + 1
            && matches!(state, "loading" | "loaded")
            && url_state == "none",
        (clicked, open, delivered, started, loads, state, url_state),
    );
    d.shot("2-picture-open-while-uploading");
    d.close_viewer();
    d.wait(450).await;
    d.kept("picture: stays put while it uploads", &key, &first).await;
    d.shot("2-picture-still-uploading");
    d.wait(1000).await;
    let delivered = d.delivered();
    d.check("picture: delivered once it has uploaded", delivered == 2, delivered);
    d.kept("picture: stays put when the upload finishes", &key, &first).await;
    d.acknowledge("picture", &key, &first).await;
    let after = d.cx.update(|_, cx| crate::pictures::stats(cx).started).unwrap_or(0);
    let url_state =
        d.cx.update(|_, cx| last_url(cx).map_or("none", |url| crate::pictures::state(&url, cx))).unwrap_or("none");
    let state = d.cx.update(|_, cx| crate::pictures::state_of(&resource, cx)).unwrap_or("none");
    d.check(
        "picture: its file loaded once, the sent copy never",
        after == started + 1 && state == "loaded" && url_state == "none",
        (started, after, state, url_state),
    );
    d.shot("2-picture-sent");
    d.reply().await;

    // 3. An upload that fails: the message stays, the same size, with the error and Retry; Retry uploads it again,
    //    and it ends where it was sent.
    d.script(
        &[
            Step { after: Duration::from_millis(300), fails: true },
            Step { after: Duration::from_millis(300), fails: false },
        ],
        &[QUICK],
    );
    d.add(&photo);
    d.type_and_send("Second try at the photo");
    d.wait(20).await;
    let key = d.key();
    let first = d.place(&key).await;
    d.wait(500).await;
    let (pending, error) = d.pending();
    d.check(
        "upload failure: the message stays, with its error",
        pending && error.as_deref() == Some(attachments::UPLOAD_FAILED),
        (pending, &error),
    );
    let failed = d.place(&key).await;
    let same_size = |a: &Option<Place>, b: &Option<Place>| {
        a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| {
            a.index == b.index
                && a.bubble.zip(b.bubble).is_some_and(|(x, y)| {
                    (x.size.width - y.size.width).abs() < px(0.5) && (x.size.height - y.size.height).abs() < px(0.5)
                })
                && a.media.zip(b.media).is_some_and(|(x, y)| {
                    (x.size.width - y.size.width).abs() < px(0.5) && (x.size.height - y.size.height).abs() < px(0.5)
                })
        })
    };
    d.check(
        "upload failure: same row, bubble and picture the same size",
        same_size(&first, &failed),
        (&first, &failed),
    );
    d.shot("3-upload-failed");
    let delivered = d.delivered();
    d.retry();
    d.wait(700).await;
    let (now, pending) = (d.delivered(), d.pending());
    d.check(
        "upload failure: Retry uploads it again and sends it",
        now == delivered + 1 && pending.1.is_none(),
        (now, pending),
    );
    d.kept("upload failure: back where it was sent after Retry", &key, &first).await;
    d.acknowledge("retried upload", &key, &first).await;
    d.reply().await;

    // 4. A delivery that fails, then Retry.
    d.script(&[], &[Step { after: Duration::from_millis(200), fails: true }, QUICK]);
    d.type_and_send("And one that fails to deliver at first");
    d.wait(20).await;
    let key = d.key();
    let first = d.place(&key).await;
    d.wait(400).await;
    let (pending, error) = d.pending();
    d.check("delivery failure: the message stays, with its error", pending && error.is_some(), (pending, &error));
    let failed = d.place(&key).await;
    d.check(
        "delivery failure: same row, bubble the same size",
        first.as_ref().zip(failed.as_ref()).is_some_and(|(a, b)| {
            a.index == b.index
                && a.bubble.zip(b.bubble).is_some_and(|(x, y)| (x.size.height - y.size.height).abs() < px(0.5))
        }),
        (&first, &failed),
    );
    d.shot("4-delivery-failed");
    let delivered = d.delivered();
    d.retry();
    d.settle().await;
    let (now, pending) = (d.delivered(), d.pending());
    d.check("delivery failure: Retry delivers it", now == delivered + 1 && pending.1.is_none(), (now, pending));
    d.kept("delivery failure: back where it was sent after Retry", &key, &first).await;
    d.acknowledge("redelivered", &key, &first).await;
    d.shot("4-delivery-retried");
    d.reply().await;

    // 5. Quote: words dragged over in a reply, then Quote pressed where it sits over the reply's next line, go to the
    //    composer as a quote as soon as it's pressed.
    d.quote().await;

    // 6. A pasted picture, a chip in the text before the words: it goes with its path where the chip was, for Filo to
    //    hand on, and shows once, above the bubble, which holds only the words, while it's sent and once the backend
    //    has it (as it comes back from history).
    let pasted = pasted_picture();
    let words = "there is still an issue on corners";
    d.script(&[QUICK], &[QUICK]);
    d.attach_and_send(&pasted, words);
    d.wait(20).await;
    let key = d.key();
    let first = d.place(&key).await;
    let shown = d.shown_text(&key);
    d.check(
        "chip: shows its picture above a bubble of only the words while it's sent",
        first.as_ref().is_some_and(|place| place.media.is_some() && place.bubble.is_some())
            && shown.as_deref() == Some(words),
        (&first, &shown),
    );
    d.shot("6-chip-sending");
    d.wait(300).await;
    let sent =
        d.cx.update(|_, cx| {
            cx.default_global::<Script>()
                .delivered
                .last()
                .map(|args| args["message"].as_str().unwrap_or_default().to_string())
        })
        .ok()
        .flatten()
        .unwrap_or_default();
    let path = attachments::mention(&pasted);
    d.check("chip: goes as its path where the chip was", sent == format!("{path} {words}"), &sent);
    d.acknowledge("chip", &key, &first).await;
    let shown = d.shown_text(&key);
    d.check("chip: the server's copy shows only the words too", shown.as_deref() == Some(words), &shown);
    d.shot("6-chip-sent");
    d.reply().await;

    driver.failures
}

/// The URL the last picture delivered would be downloaded from.
fn last_url(cx: &App) -> Option<String> {
    let args = cx.try_global::<Script>()?.delivered.last()?;
    let key = args["attachments"][0]["storageId"].as_str()?;
    Some(format!("https://fixture.invalid/{key}"))
}

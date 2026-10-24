// One conversation: the feed, the live rows under it and the composer.
// A draft (no session yet) creates its session on the first send and keeps
// this view, so the optimistic bubble stays put.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Corner, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, ImageSource, InteractiveElement as _, IntoElement, ListAlignment, ListScrollEvent,
    ListState, ObjectFit, ParentElement, Pixels, Render, Resource, SharedString, StatefulInteractiveElement as _,
    Styled, StyledImage as _, Subscription, Task as GpuiTask, Window, div, img, list, prelude::FluentBuilder as _, px,
    relative,
};
use gpui_component::Sizable as _;
use gpui_component::WindowExt as _;
use gpui_component::button::{Button as MenuButton, ButtonRounded, ButtonVariants as _};
use gpui_component::input::{Input, InputAtom, InputEvent, InputState, Paste};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::text::TextView;
use serde_json::{Value, json};

use super::attachments::{self, PendingAttachment};
use super::coding_requests::{self, Answerer, ChatRequest};
use super::coding_sessions::{self, CodingSession};
use super::feed::{self, FeedItem, Row};
use super::image_editor::{ImageEditor, ImageEditorEvent, Source};
use super::mention_suggestions::{self, MentionSources, Suggestion};
use super::note_share::{self, SharedNote};
use super::question_panel::{PanelEvent, QuestionPanel};
use super::types::*;
use super::work_share::{self, DraggedWork, SharedWork};
use crate::backend::{self, backend};
use crate::format;
use crate::local_backend::{CLAUDE_EFFORTS, EFFORT_ORDER, effort_label, nearest_effort};
use crate::media;
use crate::nav::{self, Route};
use crate::screens::feed_rows::{self, RowSig};
use crate::screens::search_menu;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};
use filo_local::Engine;
use filo_local::engine::ModelOption;

#[path = "composer_layout.rs"]
mod composer_layout;
#[path = "voice.rs"]
mod voice;
pub use voice::{AwayRecording, AwayVoice, send_away};
use voice::{VoiceReceipt, VoiceState};
#[path = "reply_link.rs"]
mod reply_link;
#[path = "seen.rs"]
mod seen;
#[path = "timeline.rs"]
mod timeline;
use reply_link::ReplyLink;
#[cfg(feature = "dev")]
#[path = "composer_fixture.rs"]
pub mod composer_fixture;
#[path = "glance.rs"]
pub mod glance;
#[cfg(feature = "dev")]
#[path = "glance_fixture.rs"]
pub mod glance_fixture;
#[cfg(feature = "dev")]
#[path = "overlap_fixture.rs"]
pub mod overlap_fixture;
#[cfg(feature = "dev")]
#[path = "scroll_fixture.rs"]
pub mod scroll_fixture;
#[cfg(feature = "dev")]
#[path = "send_fixture.rs"]
pub mod send_fixture;

const PAGE: f64 = 40.;
/// How far the fade behind the questions panel reaches above it (px).
const PANEL_FADE: f32 = 56.;
const MAX_LOADED: f64 = 600.;
/// Longest a sent message's Thinking row waits for its turn to arrive (`awaiting_turn`).
const AWAIT_TURN: Duration = Duration::from_secs(15);
/// How long a message kept in place as older history came in above it stays held there while that history's rows
/// settle (pictures loading), unless the reader scrolls first.
const HOLD_FOR: Duration = Duration::from_millis(1500);
/// Lines of the group's ask shown on its card in an agent's chat before "Show more".
const GROUP_CALL_LINES: usize = 4;
const REACTIONS: [(&str, &str); 7] = [
    ("👍", "Like"),
    ("👌", "Okay"),
    ("👀", "Seen"),
    ("🤣", "Laugh"),
    ("❤️", "Love"),
    ("👎", "Dislike"),
    ("🔥", "Fire"),
];
/// How long the levels of a model picked here and not saved yet are kept: they come from the tools' CLIs.
pub(crate) const REASONING_LEVELS_FRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// `level` as one of the levels the tools name (local_backend `EFFORT_ORDER`), so the composer keeps it without a copy;
/// None for one it doesn't know.
pub(crate) fn reasoning_level(level: &str) -> Option<&'static str> {
    EFFORT_ORDER.iter().find(|known| **known == level).copied()
}

/// The level a chat that has `level` picked thinks at: the nearest its model takes (`offered`, local_backend
/// `nearest_effort`, as its turns get it); None when its levels aren't known.
pub(crate) fn reasoning_used(level: &str, offered: &[String]) -> Option<&'static str> {
    nearest_effort(level, offered).as_deref().and_then(reasoning_level)
}

/// The reasoning picker's menu under `trigger`: `levels` (a model's, least first) with `current` checked, and its label
/// ("Default" for none). The composer's, and the group's routing step's beside its model (agents.rs `setup_routing`).
pub(crate) fn reasoning_picker(
    trigger: MenuButton,
    anchor: Corner,
    levels: Vec<String>,
    current: Option<&'static str>,
    label_color: gpui::Hsla,
    pick: impl Fn(&'static str, &mut App) + 'static,
) -> impl IntoElement {
    let pick = std::rc::Rc::new(pick);
    trigger
        .child(
            div().footnote().text_color(label_color).child(current.map_or_else(|| "Default".to_string(), effort_label)),
        )
        .child(ui::icon_sized("chevron-down", 12., label_color))
        .dropdown_menu_with_anchor(anchor, move |menu, _, _| {
            levels.iter().filter_map(|level| reasoning_level(level)).fold(menu, |menu, level| {
                let pick = pick.clone();
                menu.item(
                    PopupMenuItem::new(effort_label(level))
                        .checked(Some(level) == current)
                        .on_click(move |_, _, cx| pick(level, cx)),
                )
            })
        })
}

/// The level a message carries (`sendMessage`'s `reasoning`): the user's pick, when the chat doesn't have it yet.
fn reasoning_to_send(pick: Option<&'static str>, stored: Option<&str>) -> Option<&'static str> {
    pick.filter(|pick| Some(*pick) != stored)
}

fn reasoning_tip(agent_name: &str) -> SharedString {
    format!("How hard {agent_name} thinks").into()
}

/// Who answers in a chat: one of the coding tools, and its model (none is the engine's own default).
pub(crate) type ModelChoice = (Engine, Option<String>);

/// How a chat's model reads in the composer: the model's name, or for the engine's own default the model its CLI
/// reports; None while that isn't known (the button shows the provider's logo alone).
pub(crate) fn model_label((engine, model): &ModelChoice, rows: &[ModelOption]) -> Option<SharedString> {
    let mut rows = rows.iter().filter(|row| row.engine == *engine);
    match model {
        Some(model) => {
            Some(rows.find(|row| row.matches(model)).map_or_else(|| model.clone(), |row| row.label.clone()).into())
        }
        None => rows.find(|row| row.default).map(|row| row.label.clone().into()),
    }
}

/// The picker's row for a chat's model: the one it names, or the engine's default when it names none.
fn model_row_checked(row: &ModelOption, (engine, model): &ModelChoice) -> bool {
    row.engine == *engine && model.as_deref().map_or(row.default, |model| row.matches(model))
}

/// The model picker's rows: the models of the tools signed in, as Filo's database had them (none without a backend:
/// fixtures).
pub(crate) fn model_rows(cx: &App) -> std::sync::Arc<Vec<ModelOption>> {
    cx.try_global::<backend::BackendGlobal>().map(|backend| backend.0.model_options()).unwrap_or_default()
}

/// Every tool's models, for a chip naming the model a chat or agent has (`model_label`), its tool signed out or not.
pub(crate) fn all_model_rows(cx: &App) -> std::sync::Arc<Vec<ModelOption>> {
    cx.try_global::<backend::BackendGlobal>().map(|backend| backend.0.all_model_options()).unwrap_or_default()
}

/// Has the backend ask the CLIs about their models in the background (once per run, not while the cache is fresh),
/// then draws the windows again if the picker's rows changed. The picker shows the cached rows meanwhile.
pub(crate) fn refresh_models<T: 'static>(cx: &mut Context<T>) {
    let Some(refresh) = cx.try_global::<backend::BackendGlobal>().and_then(|backend| backend.0.refresh_models()) else {
        return;
    };
    cx.spawn(async move |_, cx| {
        if refresh.await {
            cx.update(|cx| cx.refresh_windows()).ok();
        }
    })
    .detach();
}

/// The provider's logo for an engine's models.
pub(crate) fn engine_logo(engine: Engine) -> &'static str {
    match engine {
        Engine::Claude => "claude",
        Engine::Codex => "openai",
        Engine::Muse => "meta",
        Engine::OpenCode => "opencode",
        Engine::Pi => "pi",
    }
}

/// The colour an engine's logo is drawn in where it stands for the tool (the Coding page's tiles, the Agents list's
/// marks): Claude's orange, Meta's blue, OpenAI's, OpenCode's and pi's in the text colour.
pub(crate) fn engine_color(engine: Engine, cx: &App) -> gpui::Hsla {
    match engine {
        Engine::Claude => gpui::rgb(0xD97757).into(),
        Engine::Muse => gpui::rgb(0x0081FB).into(),
        Engine::Codex | Engine::OpenCode | Engine::Pi => cx.colors().text,
    }
}

/// The model picker (search_menu.rs) `trigger` opens: every tool's models, each by logo and name, its CLI's default
/// marked, `current` checked and a rule between the tools; searched by name, id or tool, as OpenCode alone has
/// hundreds. The composer's and the agent form's.
pub(crate) fn model_picker<T: gpui_component::Selectable + IntoElement + 'static>(
    id: &str,
    trigger: T,
    anchor: Corner,
    rows: std::sync::Arc<Vec<ModelOption>>,
    current: &ModelChoice,
    pick: impl Fn(ModelChoice, &mut App) + 'static,
) -> gpui_component::popover::Popover {
    let current = current.clone();
    let choices = {
        let rows = rows.clone();
        move || {
            std::rc::Rc::new(
                rows.iter()
                    .map(|row| search_menu::Choice {
                        icon: Some(engine_logo(row.engine)),
                        // A model that can't answer now says why instead (its sign-in doesn't work).
                        note: row.unavailable.clone().map(Into::into).or_else(|| row.default.then(|| "Default".into())),
                        off: row.unavailable.is_some(),
                        checked: model_row_checked(row, &current),
                        group: row.engine as usize,
                        also: format!("{} {}", row.id, row.engine.label()),
                        ..search_menu::Choice::new(row.label.clone())
                    })
                    .collect::<Vec<_>>(),
            )
        }
    };
    search_menu::search_menu(id.to_string(), trigger, anchor, 360., "Search models", choices, move |index, _, cx| {
        let row = &rows[index];
        pick((row.engine, Some(row.id.clone())), cx);
    })
}

/// The model a message carries (`sendMessage`'s `engine` and `model`): the user's pick, when the chat doesn't have it yet.
fn model_to_send(pick: Option<&ModelChoice>, stored: Option<&ModelChoice>) -> Option<ModelChoice> {
    pick.filter(|pick| Some(*pick) != stored).cloned()
}

/// `args` with a chat's engine and model (`voiceSessions:setModel`, `sendMessage`).
fn with_model(mut args: Value, (engine, model): &ModelChoice) -> Value {
    args["engine"] = json!(engine);
    args["model"] = json!(model);
    args
}

pub enum ConversationEvent {
    /// An @mention in the agents' group was clicked: the agent's id.
    OpenSpeaker(String),
    /// A line in an agent's chat pointing at the group was clicked: the group message it points at, when known.
    OpenGroup(Option<String>),
    /// A chip pointing at another agent's chat was clicked (an ask between agents while the group is off): that
    /// agent's id, and the message there, when known.
    OpenAgent(String, Option<String>),
    /// A message carrying the context `set_send_context` gave went through: that context's key.
    ContextSent(String),
}

impl EventEmitter<ConversationEvent> for Conversation {}

/// A send the backend hasn't confirmed yet (the optimistic bubble). It shows as the message it will be, laid out the
/// same way, so nothing moves when the saved copy takes its place.
#[derive(Clone, Debug, PartialEq)]
struct PendingSend {
    /// Its bubble's id while it's sent; it stays the same when a voice message's request id changes.
    key: String,
    request_id: String,
    text: String,
    attachments: Vec<PendingAttachment>,
    reply: Option<ReplyTarget>,
    created_at: f64,
    visible_ids: HashSet<String>,
    /// `sendMessage` args, kept so Retry resends the identical request.
    args: Option<Value>,
    turn_id: Option<String>,
    error: Option<String>,
    /// A voice message's length; its text arrives with the transcript.
    voice_ms: Option<f64>,
    /// The key of the context (`set_send_context`) its args carry.
    context_key: Option<String>,
    /// Notes shared with it, tasks it points at and the context `set_send_context` gave, kept until its files have
    /// uploaded.
    notes: Vec<SharedNote>,
    work: Vec<SharedWork>,
    context: Option<(String, String)>,
    /// Files whose upload failed (local id, file, name): Retry uploads them again.
    failed_uploads: Vec<(u64, std::path::PathBuf, String)>,
}

impl PendingSend {
    fn new(request_id: String, text: String, reply: Option<ReplyTarget>, visible_ids: HashSet<String>) -> Self {
        PendingSend {
            key: format!("sending-{}", uuid::Uuid::new_v4()),
            request_id,
            text,
            attachments: Vec::new(),
            reply,
            created_at: format::now_ms(),
            visible_ids,
            args: None,
            turn_id: None,
            error: None,
            voice_ms: None,
            context_key: None,
            notes: Vec::new(),
            work: Vec::new(),
            context: None,
            failed_uploads: Vec::new(),
        }
    }

    /// The message it will be, at `at` (no earlier than the newest one shown, so it stays at the end).
    fn message(&self, at: f64) -> Message {
        Message {
            id: self.key.clone(),
            creation_time: at,
            role: "user".into(),
            content: self.text.clone(),
            voice_recording: self.voice_ms.map(|duration_ms| VoiceRecording { duration_ms, url: None, gone: false }),
            reaction: None,
            reply_to_message_id: self.reply.as_ref().map(|reply| reply.message_id.clone()),
            turn: None,
            attachments: self.attachments.iter().map(PendingAttachment::shown).collect(),
            speaker: None,
            coding_result: None,
            glance: None,
            group_link: None,
            group_call: None,
            routine: None,
            asked_by: None,
            mark: None,
            report: None,
        }
    }

    /// Its files have all uploaded.
    fn uploaded(&self) -> bool {
        self.attachments.iter().all(|attachment| attachment.storage_id.is_some())
    }
}

/// A file that uploaded: what it went up as (a HEIC as a JPEG) and its storage key.
pub(super) struct Uploaded {
    pub name: String,
    pub content_type: String,
    pub size: usize,
    pub key: String,
}

/// A message not sent yet: why it hasn't gone, when it failed (with Retry under it).
#[derive(Clone, Debug, PartialEq)]
struct Sending {
    error: Option<String>,
}

impl PartialEq for PendingAttachment {
    fn eq(&self, other: &Self) -> bool {
        self.local_id == other.local_id && self.storage_id == other.storage_id
    }
}

/// What a result's row is led by: the task's title and its tool (coding_sessions.rs), not the whole session,
/// whose progress changes would measure the row again for nothing.
#[derive(Clone, Debug, PartialEq)]
struct ProofTask {
    title: String,
    tool: String,
    /// The pill's state and words: while the task still works (its turn ended with background work going, or a later
    /// round runs), that state; else how the round ended (what the task came to, for one that only waited on
    /// background work).
    state: (&'static str, &'static str),
    /// The task still works: the row opens its live thread, not the round's page.
    live: bool,
    /// The pill's words with how long the finished task worked ("Done · 18m").
    label: String,
}

/// The marker under a message whose turn compacted the session (`ChatTurn::compacted`).
const COMPACTED_MARK: &str = "Earlier conversation summarized";
/// The marker under a message whose turn started a fresh session with the newest messages (`ChatTurn::reseeded`).
const RESEEDED_MARK: &str = "New session from here, with the recent messages";

#[derive(Clone, Debug, PartialEq)]
enum ViewRow {
    Older,
    /// Space between two days (the later day's key).
    Gap(String),
    /// A time marker (feed::day_rows): "Today · 3:07 PM", or "5:40 PM" after a pause.
    Mark(String),
    /// "New messages", above the first one the user hadn't seen.
    Unseen,
    Message {
        message: Message,
        joins_prev: bool,
        joins_next: bool,
        /// A reply that came more than two minutes after the message it answers: its time shows under it.
        late: bool,
        stopped: Option<bool>,
        failed: Option<ChatTurn>,
        /// The message the user just sent, until the backend has it.
        sending: Option<Sending>,
        /// A glance's card (glance.rs): what it shows, so the row is measured again when that changes.
        glance: Option<glance::Look>,
        /// The coding task its result row reports, once the chat's sessions have it: its title and tool head the
        /// proof, and the row is measured again when they arrive.
        proof_task: Option<ProofTask>,
        /// The message it answers, when someone else wrote after it (`link_replies`).
        reply: Option<ReplyLink>,
    },
    /// In an agent's own chat: where it asked other members in the group, or one asked it (the message, its words
    /// and the part of the group message it quotes).
    GroupLink {
        id: String,
        link: GroupLink,
        quote: String,
    },
    /// In an agent's own chat: a turn the group gave it, as a card with the ask (the message's id), whether a long
    /// ask is open past its first lines, and the turn when it failed (what went wrong and Retry, under the card).
    GroupCall {
        id: String,
        call: GroupCall,
        expanded: bool,
        failed: Option<ChatTurn>,
    },
    /// A routine's run coming due, as one line: its title and schedule, or that it had nothing to report.
    Routine {
        id: String,
        mark: RoutineMark,
    },
    /// An artifact a reply made, as a card with Open.
    Task(Task),
    /// A failure at its triggering event, when that event has no visible message bubble.
    OrphanFailed(ChatTurn),
    /// A coding task's questions waiting on the user, as one line where they were asked (coding_requests.rs); the
    /// panel above the composer answers them.
    CodingAsk(Vec<ChatRequest>),
    /// A coding request answered or closed, as a card of its own where it was asked: the answer the panel sent that
    /// the backend doesn't say yet, and whether it's open to the question.
    CodingAnswered {
        request: ChatRequest,
        sent: Option<String>,
        expanded: bool,
    },
    /// Coding sessions started here, where they started (coding_sessions.rs).
    CodingSessions(Vec<CodingSession>),
    /// A reply under way, and what the agent is doing for it; in the group, the member answering in its own chat.
    Thinking(Option<String>, Option<String>),
    /// In the group: the routing model, of this tool, picking who answers the user's post.
    Routing(Option<filo_local::Engine>),
    /// Room under the newest message for the floating composer (its height, px).
    Spacer(u32),
}

pub struct Conversation {
    session_id: Option<String>,
    agent_name: String,
    /// In the agents' group chat: each speaker's name and picture.
    group: Option<HashMap<String, (String, gpui::ImageSource, Option<gpui::Hsla>)>>,
    /// The user's agents by id (name, picture, colour), for the chips between agents asking each other (`set_faces`).
    faces: HashMap<String, (String, gpui::ImageSource, Option<gpui::Hsla>)>,
    faces_key: String,
    /// Who an "@" in the composer can name (the user's agents, and Filo in an agent's chat), the row picked in the
    /// open list, and the "@" whose list Escape closed.
    mentions: Vec<mention_suggestions::Mention>,
    mention_index: usize,
    mention_dismissed: Option<usize>,
    /// The user's apps and skills an "@" or "/" offers (mention_suggestions.rs), once one was typed.
    mention_sources: Option<Entity<MentionSources>>,
    /// The agent this conversation is (Filo in Filo's chats), for the apps "@" offers: only those it may use. None
    /// until known: only apps open to every agent.
    mention_agent: Option<String>,
    /// What picking from the list typed ("@gmail:me@x.com"), drawn in the accent colour like the names it offers.
    picked_mentions: Vec<String>,
    _mention_subs: Vec<Subscription>,
    limit: f64,
    detail: Option<Result<Option<Detail>, String>>,
    turns: Vec<ChatTurn>,
    /// Coding questions handed to the user here (`coding:requestsForSession`), in the chat's order; those waiting are
    /// answered in the panel above the composer (question_panel.rs), and their lines in the chat open by first id.
    coding_requests: Vec<ChatRequest>,
    question_panel: Entity<QuestionPanel>,
    _question_panel_sub: Subscription,
    expanded_asks: HashSet<String>,
    /// The group's asks in an agent's chat opened past their first lines (by message id).
    open_group_calls: HashSet<String>,
    /// The panel showed questions when last drawn: new ones take the keyboard from an empty composer.
    panel_was_open: bool,
    /// The panel is on a question, so what's typed in the composer answers it (its placeholder says so), and the
    /// placeholder it has otherwise.
    answering: bool,
    placeholder: &'static str,
    /// Give the panel the keyboard at the next draw (it has no window before).
    focus_panel: bool,
    /// Coding tool sessions started from this chat, oldest first, and the clock of the bar over them.
    pub(super) coding_sessions: Vec<CodingSession>,
    /// The chat's width, from the composer's measure.
    width: f32,
    coding_tick: Option<GpuiTask<()>>,
    _detail_sub: Option<GpuiTask<()>>,
    _session_subs: Vec<GpuiTask<()>>,
    /// Out of sight a while (`park`): no live queries, what it showed and where kept for coming back.
    parked: bool,
    /// Shown again after `park`, until the first answer says what came meanwhile (seen.rs `came_back`).
    stale: bool,
    slow_load: bool,
    /// Known to have no messages yet (an agent's new chat, which shows the agent instead): no skeleton while it loads.
    known_empty: bool,
    _slow_timer: Option<GpuiTask<()>>,

    input: Entity<InputState>,
    _input_sub: Subscription,
    focus: FocusHandle,
    reply_to: Option<ReplyTarget>,
    attachments: Vec<PendingAttachment>,
    /// The attachments shown as a chip in the composer's text (their local ids): deleting the chip takes the file off.
    chips: Vec<u64>,
    next_local_id: u64,
    hint: Option<String>,
    /// A picture open to draw on, over the whole window (image_editor.rs).
    image_editor: Option<(Entity<ImageEditor>, Subscription)>,
    voice: Option<VoiceState>,
    /// Recording while a key is held: letting go sends it, or with `dictating` writes it into the message box.
    voice_held: bool,
    /// The held recording is fn+⌃'s: its words go into the message box at the caret, nothing is sent (voice.rs
    /// `dictate`).
    dictating: bool,
    /// Measured height of the composer's text row; the voice rows take it, so
    /// recording doesn't change the composer's height. The mic only shows with
    /// the field empty, so this is its one-line height.
    text_row_height: f32,
    voice_receipt: Option<VoiceReceipt>,
    /// A screenshot to send with the voice message being recorded (hold ⌥⌘ in any app; screenshot.rs).
    voice_screenshot: Option<crate::screenshot::Screenshot>,
    _voice_tick: Option<GpuiTask<()>>,

    pending: Option<PendingSend>,
    /// Sent messages by the bubble id they had while they were sent: the saved copy keeps drawing as that bubble.
    handed_over: HashMap<String, String>,
    /// A sent message whose reply has started but whose turn hasn't arrived here yet (request id, message
    /// id, since when): its Thinking row stays meanwhile, so the feed doesn't drop and come back.
    awaiting_turn: Option<(String, String, std::time::Instant)>,
    /// How tall the composer's reply, note and file strips were when last drawn.
    strips_height: Rc<std::cell::Cell<f32>>,
    /// The composer's text row: one row or stacked, and its glide between them (composer_layout.rs).
    composer_layout: composer_layout::ComposerLayout,
    /// Where bubbles and pictures were last drawn, by `bubble-<id>` / `media-<id>` (fixtures only).
    drawn: Drawn,
    sending: bool,
    stopping: bool,
    retrying: Option<String>,
    /// A Retry transport error belongs only to the turn whose retry failed.
    retry_error: Option<(String, String)>,
    reaction_overrides: HashMap<String, Option<String>>,
    picker_for: Option<String>,
    /// The picker that just closed because the mouse went down outside it, and when: a press on its own react button
    /// closes it that way first, and that button's click mustn't open it again.
    picker_closed: Option<(String, std::time::Instant)>,
    /// A message flashed after jumping to it from a reply quote.
    highlighted: Option<String>,
    /// A message to jump to once it has loaded (`reveal_message`), and how many loads to wait for it.
    reveal_pending: Option<(String, u8)>,

    list_state: ListState,
    /// False once the reader scrolls up from the newest message.
    at_bottom: bool,
    /// Measured height of the floating composer; the feed keeps that much room below.
    composer_height: f32,
    rows: Vec<ViewRow>,
    /// What each row showed when the list last measured it (`apply_rows`).
    row_sigs: Vec<RowSig>,
    /// A message kept where it was on screen as older history came in above it (`hold_pin`).
    held: Option<Held>,

    /// The user is looking at this chat (seen.rs).
    attending: bool,
    /// `_creationTime` of the newest message seen here, ahead of the backend's while a mark is on its way.
    seen_until: f64,
    /// The first page arrived and placed the divider.
    seen_ready: bool,
    /// The cursor when the user came back: the divider sits above Filo's first message after it.
    divider_at: Option<f64>,
    /// Opened at the divider already.
    positioned: bool,
    /// The newest messages show (no jump pill).
    at_end: bool,
    /// The user has looked at this chat since it opened: before that, there's no place of theirs to hold.
    attended: bool,
    /// Held at the newest messages while the user was away (`hold_position`), and not moved since: back with nothing
    /// unseen, the feed follows the newest again, whatever arrived below the held place meanwhile.
    held_at_end: bool,
    _mark_seen: Option<GpuiTask<()>>,
    /// Shows made-up data (`load_fixture`): nothing it does reaches the backend.
    #[cfg(feature = "dev")]
    fixture: bool,
    /// What the next message tells the model besides what's typed, not shown in its bubble (Feed posts asked about),
    /// and the key `ConversationEvent::ContextSent` names once it went.
    send_context: Option<(String, String)>,
    /// Notes shared from the Notes tab (note_share.rs), waiting above the composer for the next message.
    shared_notes: Vec<SharedNote>,
    /// Running tasks the next message points at (work_share.rs): dragged from the Working rows or picked after "@".
    shared_work: Vec<SharedWork>,
    /// Said in a new chat's empty space; None leaves it blank (the Feed's panel shows its posts above instead).
    draft_prompt: Option<SharedString>,
    /// The reasoning level the user picked in the composer, until the chat (`detail`) has it: a new chat's goes with
    /// its first message.
    reasoning_pick: Option<&'static str>,
    /// The reasoning picker's tooltip, made when the agent's name is known rather than on every frame.
    reasoning_tip: SharedString,
    /// The engine and model the user picked in the composer, until the chat (`detail`) has them: a new chat's go with
    /// its first message.
    model_pick: Option<ModelChoice>,
    /// An agent's chat: it answers with the model picked for the agent, so the composer has no model picker.
    model_fixed: bool,
    /// The reasoning levels last asked of the backend (`reasoning_levels`): for which chat or picked model, and when.
    asked_levels: Option<(String, std::time::Instant, Vec<String>)>,
    /// Glances Filo showed here: their pages and cards (glance.rs).
    glances: glance::Glances,
    /// The timeline's hovered dash and a press on it (timeline.rs).
    timeline: timeline::Timeline,
}

/// Which row this is, the same however what it shows changes (a reaction, a task's progress), so the list keeps the
/// reader's place by it when other rows come and go around it.
/// A pinned row (feed_rows::Pin), held from `since`. `placed`: where the list was left after the last frame, so a
/// scroll since (the wheel, the scrollbar, a jump) lets it go. `viewport`: the list's height then.
struct Held {
    pin: feed_rows::Pin,
    since: std::time::Instant,
    placed: Option<(usize, Pixels)>,
    viewport: Pixels,
}

fn row_key(row: &ViewRow, handed_over: &HashMap<String, String>) -> String {
    match row {
        ViewRow::Older => "older".into(),
        ViewRow::Gap(day) => format!("gap:{day}"),
        ViewRow::Mark(label) => format!("mark:{label}"),
        ViewRow::Unseen => "unseen".into(),
        // The sent message keeps its sending bubble's key (`handed_over`).
        ViewRow::Message { message, .. } => format!("m:{}", handed_over.get(&message.id).unwrap_or(&message.id)),
        ViewRow::GroupLink { id, .. } => format!("group-link:{id}"),
        ViewRow::GroupCall { id, .. } => format!("group-call:{id}"),
        ViewRow::Routine { id, .. } => format!("routine:{id}"),
        ViewRow::Task(task) => format!("task:{}", task.id),
        ViewRow::OrphanFailed(turn) => format!("failed:{}", turn.turn_id),
        ViewRow::CodingAsk(requests) => {
            format!("coding-ask:{}", requests.first().map_or("", |request| request.request_id.as_str()))
        }
        ViewRow::CodingAnswered { request, .. } => format!("coding-answered:{}", request.request_id),
        ViewRow::CodingSessions(sessions) => {
            format!("coding:{}", sessions.first().map_or("", |session| session.task_id.as_str()))
        }
        ViewRow::Thinking(..) => "thinking".into(),
        ViewRow::Routing(_) => "routing".into(),
        ViewRow::Spacer(_) => "spacer".into(),
    }
}

/// Whether the row keyed `key` (`row_key`) can keep the reader's place as older history comes in: a message's row, which
/// stays among the same messages. Not the "loading" row, a coding card or a time mark, which stay at the top of the
/// chat while history comes in under them or move up above the same day's older messages (the chat went with them,
/// to the top of the page that came in).
fn holds_place(key: &str) -> bool {
    ["m:", "group-link:", "group-call:", "routine:"].iter().any(|kind| key.starts_with(kind))
}

/// Whether a turn has a line under the message that asked for it: its error, or, settled, a quiet line.
fn shows_failure(turn: &ChatTurn) -> bool {
    matches!(turn.state.as_str(), "failed" | "settled")
}

/// A settled failure's quiet line: what went wrong then, its first paragraph (the rest is advice for when it stood, or
/// what the agent wrote instead), without the words that lead into the rest ("What it wrote instead:").
fn settled_words(turn: &ChatTurn) -> String {
    let reason = turn
        .error
        .as_deref()
        .and_then(|error| error.trim().split("\n\n").next())
        .map(|first| match first.trim().strip_suffix(':') {
            Some(lead) => lead.rfind(". ").map_or(lead, |end| &lead[..=end]).trim(),
            None => first.trim(),
        })
        .filter(|reason| !reason.is_empty());
    reason.map_or_else(|| "Couldn't reply then.".into(), |reason| format!("Couldn't reply then: {reason}"))
}

/// When a failure on a usage limit is tried again on its own: "Will try again at 2:52 PM", with the day when it isn't
/// today's.
fn retry_words(at: f64, now: f64) -> String {
    if at <= now {
        "Trying again in a moment".into()
    } else if crate::format::same_day(crate::format::local(at), crate::format::local(now)) {
        format!("Will try again at {}", crate::format::clock(at))
    } else {
        format!("Will try again {}", crate::format::short_date_time(at))
    }
}

fn failure_message(turn: &ChatTurn, retry_error: Option<&(String, String)>) -> String {
    retry_error
        .filter(|(id, _)| id == &turn.turn_id)
        .map(|(_, error)| error.clone())
        .or_else(|| turn.error.clone())
        .unwrap_or_else(|| "Filo could not finish this reply.".into())
}

impl Conversation {
    /// A fixture's made-up chat (`load_fixture`); never in the packaged app.
    fn fixture(&self) -> bool {
        #[cfg(feature = "dev")]
        return self.fixture;
        #[cfg(not(feature = "dev"))]
        false
    }

    pub fn new(session_id: Option<String>, agent_name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        refresh_models(cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 8)
                .scrollbar_show(gpui_component::scroll::ScrollbarShow::Scrolling)
                .wrap_margin(px(composer_layout::WRAP_MARGIN))
                .placeholder("Message")
        });
        let input_sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                ui::cap_length(input, attachments::MAX_MESSAGE_CHARS, window, cx);
                this.hint = None;
                this.mention_index = 0;
                // An "@" or "/" deleted and typed again opens its list again.
                if this
                    .mention_dismissed
                    .is_some_and(|at| !matches!(input.read(cx).value().as_bytes().get(at), Some(b'@' | b'/')))
                {
                    this.mention_dismissed = None;
                }
                // A file's chip deleted from the text takes the file off the message.
                let shown: Vec<u64> = input.read(cx).atoms().iter().map(|atom| atom.id).collect();
                let gone: Vec<u64> = this.chips.iter().copied().filter(|id| !shown.contains(id)).collect();
                if !gone.is_empty() {
                    this.chips.retain(|id| !gone.contains(id));
                    this.attachments.retain(|attachment| !gone.contains(&attachment.local_id));
                }
                cx.notify();
            }
        });
        let question_panel = cx.new(|cx| QuestionPanel::new(Answerer::Backend, cx));
        let question_panel_sub =
            cx.subscribe_in(&question_panel, window, |this, _, event: &PanelEvent, window, cx| match event {
                PanelEvent::Compose(typed) => this.compose_answer(typed.clone(), window, cx),
                PanelEvent::Changed => this.data_changed(cx),
            });
        let list_state = ListState::new(0, ListAlignment::Bottom, px(800.));
        list_state.set_scroll_handler(cx.listener(|this, event: &ListScrollEvent, _, cx| {
            this.scrolled(event.visible_range.clone(), event.count, cx)
        }));
        let mut this = Conversation {
            session_id: None,
            reasoning_tip: reasoning_tip(&agent_name),
            glances: glance::Glances::default(),
            timeline: timeline::Timeline::default(),
            agent_name,
            group: None,
            faces: HashMap::new(),
            faces_key: String::new(),
            mentions: Vec::new(),
            mention_index: 0,
            mention_dismissed: None,
            mention_sources: None,
            mention_agent: None,
            picked_mentions: Vec::new(),
            _mention_subs: Vec::new(),
            limit: PAGE,
            detail: None,
            turns: Vec::new(),
            coding_requests: Vec::new(),
            question_panel,
            _question_panel_sub: question_panel_sub,
            expanded_asks: HashSet::new(),
            open_group_calls: HashSet::new(),
            panel_was_open: false,
            answering: false,
            placeholder: "Message",
            focus_panel: false,
            coding_sessions: Vec::new(),
            width: 0.,
            coding_tick: None,
            _detail_sub: None,
            _session_subs: Vec::new(),
            parked: false,
            stale: false,
            slow_load: false,
            known_empty: false,
            _slow_timer: None,
            input,
            _input_sub: input_sub,
            focus: cx.focus_handle(),
            reply_to: None,
            attachments: Vec::new(),
            chips: Vec::new(),
            next_local_id: 1,
            hint: None,
            image_editor: None,
            voice: None,
            voice_held: false,
            dictating: false,
            text_row_height: 37.,
            voice_receipt: None,
            voice_screenshot: None,
            _voice_tick: None,
            pending: None,
            handed_over: HashMap::new(),
            awaiting_turn: None,
            strips_height: Rc::default(),
            composer_layout: Default::default(),
            drawn: Drawn::default(),
            sending: false,
            stopping: false,
            retrying: None,
            retry_error: None,
            reaction_overrides: HashMap::new(),
            picker_for: None,
            picker_closed: None,
            highlighted: None,
            reveal_pending: None,
            list_state,
            at_bottom: true,
            composer_height: 72.,
            rows: Vec::new(),
            row_sigs: Vec::new(),
            held: None,
            attending: false,
            seen_until: 0.,
            seen_ready: false,
            divider_at: None,
            positioned: false,
            at_end: true,
            attended: false,
            held_at_end: false,
            _mark_seen: None,
            #[cfg(feature = "dev")]
            fixture: false,
            send_context: None,
            shared_notes: Vec::new(),
            shared_work: Vec::new(),
            draft_prompt: Some("What's on your mind?".into()),
            reasoning_pick: None,
            model_pick: None,
            model_fixed: false,
            asked_levels: None,
        };
        if let Some(id) = session_id {
            this.attach_session(id, cx);
        }
        this.rebuild(cx);
        this
    }

    /// Gives the keyboard to the questions of coding task `task_id` (the working panel's "Needs you" row); false when
    /// they aren't waiting here.
    pub fn reveal_question(&mut self, task_id: &str, cx: &mut Context<Self>) -> bool {
        let waiting =
            self.coding_requests.iter().any(|request| request.task_id == task_id && request.state == "pending");
        if waiting {
            self.focus_panel = true;
            cx.notify();
        }
        waiting
    }

    /// Where things sit (the fixture's checks): the composer's measured height, the list's scroll, and where message
    /// `id`'s bubble was last drawn (its top, in window points).
    #[cfg(feature = "dev")]
    pub fn layout_probe(&self, id: &str) -> (f32, (usize, f32), Option<f32>) {
        let bubble = self.drawn.borrow().get(&format!("bubble-{id}")).map(|bounds| f32::from(bounds.origin.y));
        (self.composer_height, self.scroll_position(), bubble)
    }

    /// The questions panel above the composer (the fixture's checks).
    #[cfg(feature = "dev")]
    pub fn question_panel(&self) -> Entity<QuestionPanel> {
        self.question_panel.clone()
    }

    /// The fixture's coding sessions (their open questions follow the panel).
    #[cfg(feature = "dev")]
    pub(super) fn set_fixture_sessions(&mut self, sessions: Vec<CodingSession>, cx: &mut Context<Self>) {
        self.coding_sessions = sessions;
        self.data_changed(cx);
        cx.notify();
    }

    /// The panel wants an answer in the user's words: the composer takes the keyboard, with what they started typing.
    fn compose_answer(&mut self, typed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::EntityInputHandler as _;
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            if let Some(typed) = typed {
                // As typed (so the composer hears of the change), at the caret.
                input.replace_text_in_range(None, &typed, window, cx);
            }
        });
        cx.notify();
    }

    /// Where the list is scrolled to: the top row and how far into it (fixture checks).
    #[cfg(feature = "dev")]
    pub fn scroll_position(&self) -> (usize, f32) {
        let top = self.list_state.logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    }

    /// The messages loaded, and whether an older page can still come (`request_older`) (`FILO_OPEN=chat-scroll-timing`).
    #[cfg(feature = "dev")]
    pub fn history_loaded(&self) -> (usize, bool) {
        let loaded = self.detail().map_or(0, |detail| detail.messages.len());
        let more = self.detail().is_some_and(|detail| detail.has_more) && self.limit < MAX_LOADED;
        (loaded, more)
    }

    /// The first message at or below the top of the view: its id, and how many loaded messages are older
    /// (`FILO_OPEN=chat-scroll-timing`).
    #[cfg(feature = "dev")]
    pub fn top_message(&self) -> Option<(String, usize)> {
        let top = self.list_state.logical_scroll_top();
        let message = self.rows.iter().skip(top.item_ix).find_map(|row| match row {
            ViewRow::Message { message, .. } => Some(message),
            _ => None,
        })?;
        let older = self.detail()?.messages.iter().filter(|other| other.creation_time < message.creation_time).count();
        Some((message.id.clone(), older))
    }

    /// The timeline's strip in the window as last drawn, and its bright place in dashes (`FILO_OPEN=chat-scroll-timing`).
    #[cfg(feature = "dev")]
    pub fn timeline_strip(&self) -> Option<(gpui::Bounds<Pixels>, f32)> {
        self.timeline.drawn().map(|drawn| (drawn.strip, drawn.place))
    }

    /// Whether the feed's messages go from the oldest to the newest (`FILO_OPEN=chat-scroll-timing`).
    #[cfg(feature = "dev")]
    pub fn messages_in_order(&self) -> bool {
        let times: Vec<f64> = self
            .rows
            .iter()
            .filter_map(|row| match row {
                ViewRow::Message { message, .. } => Some(message.creation_time),
                _ => None,
            })
            .collect();
        times.windows(2).all(|pair| pair[0] <= pair[1])
    }

    /// The timeline in short (the fixture's checks): messages by id, cards as "?id", other rows by kind.
    #[cfg(feature = "dev")]
    pub fn row_labels(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| match row {
                ViewRow::Message { message, .. } => message.id.clone(),
                ViewRow::CodingAsk(requests) => {
                    requests.iter().map(|request| format!("?{}", request.request_id)).collect::<Vec<_>>().join(" ")
                }
                ViewRow::CodingAnswered { request, .. } => format!("✓{}", request.request_id),
                ViewRow::CodingSessions(_) => "coding-session".into(),
                ViewRow::Spacer(_) => "spacer".into(),
                ViewRow::Mark(_) | ViewRow::Gap(_) => "day".into(),
                other => format!("{other:?}").chars().take(16).collect(),
            })
            .collect()
    }

    /// Debug view (`FILO_OPEN=coding-cards`): made-up messages and coding questions, with no backend behind them;
    /// answers go to a script (`coding_requests::Answerer::Fixture`).
    #[cfg(feature = "dev")]
    pub fn load_fixture(&mut self, detail: Detail, requests: Vec<ChatRequest>, cx: &mut Context<Self>) {
        self.fixture = true;
        self.detail = Some(Ok(Some(detail)));
        self.question_panel.update(cx, |panel, _| panel.set_answerer(Answerer::Fixture));
        self.coding_requests = coding_requests::ordered(requests);
        self.data_changed(cx);
        cx.notify();
    }

    /// A coding question the fixture asks later (`FILO_OPEN=coding-cards`, its `ask` step).
    #[cfg(feature = "dev")]
    pub fn add_fixture_request(&mut self, request: ChatRequest, cx: &mut Context<Self>) {
        let mut requests = self.coding_requests.clone();
        requests.push(request);
        self.coding_requests = coding_requests::ordered(requests);
        self.data_changed(cx);
        cx.notify();
    }

    /// The fixture's turns (`FILO_OPEN=coding-cards`): a reply under way, or none once it's done.
    #[cfg(feature = "dev")]
    pub fn set_fixture_turns(&mut self, turns: Vec<ChatTurn>, cx: &mut Context<Self>) {
        self.turns = turns;
        cx.notify();
    }

    /// Debug view (`FILO_OPEN=attach:<text>|<path>|<text>…`): text typed and files attached in turn, as if dropped
    /// where the cursor was.
    #[cfg(feature = "dev")]
    pub fn attach_draft(&mut self, parts: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
        for (ix, part) in parts.into_iter().enumerate() {
            if ix % 2 == 0 {
                self.input.update(cx, |input, cx| input.insert(part, window, cx));
            } else {
                self.attach_files(vec![part.into()], window, cx);
            }
        }
        self.focus_composer(window, cx);
    }

    /// Debug view (`FILO_OPEN=mention:<text>`): the text typed into the composer, so the "@" list can be checked
    /// without typing.
    pub fn type_draft(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::RopeExt as _;
        self.input.update(cx, |input, cx| {
            let end = text.len();
            input.set_value(text, window, cx);
            let end = input.text().offset_to_position(end);
            input.set_cursor_position(end, window, cx);
        });
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Shows this conversation as the agents' group: who said what. The user writes there too; an "@" names a member.
    /// Each speaker's name, picture and, for an agent given one, the colour behind its picture.
    pub fn set_group(
        &mut self,
        speakers: HashMap<String, (String, gpui::ImageSource, Option<gpui::Hsla>)>,
        cx: &mut Context<Self>,
    ) {
        let first = self.group.is_none();
        self.group = Some(speakers);
        if first {
            self.open_mentions(cx);
            self.rebuild(cx);
        }
        cx.notify();
    }

    /// The message flashing now (`jump_to`), for the scripted checks (`FILO_OPEN=search-drive`).
    #[cfg(feature = "dev")]
    pub fn drive_highlighted(&self) -> Option<String> {
        self.highlighted.clone()
    }

    /// Scrolls to message `id` and flashes it, as a reply quote does; when it hasn't loaded yet, once it has.
    pub fn reveal_message(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.rows.iter().any(|row| matches!(row, ViewRow::Message { message, .. } if message.id == id)) {
            self.jump_to(id, cx);
        } else {
            self.reveal_pending = Some((id.to_string(), 3));
        }
    }

    /// `reveal_message` for a message `depth` messages from the newest (1: the newest), as ⌘P's search and the Shared
    /// list's Show in Chat find one: the chat loads back that far first, past `MAX_LOADED` when it's older than that.
    pub fn reveal_message_at(&mut self, id: &str, depth: usize, cx: &mut Context<Self>) {
        let needed = (depth as f64 / PAGE).ceil() * PAGE;
        if needed > self.limit
            && self.session_id.is_some()
            && !self.rows.iter().any(|row| row_message_id(row) == Some(id))
        {
            self.limit = needed;
            self.subscribe_detail(cx);
        }
        self.reveal_message(id, cx);
    }

    /// Who an "@" in the composer offers.
    /// The user's agents by id, for the chips between agents asking each other while the group is off.
    /// `key` tells one set from another (the faces' pictures can't be compared).
    pub fn set_faces(
        &mut self,
        key: String,
        faces: HashMap<String, (String, gpui::ImageSource, Option<gpui::Hsla>)>,
        cx: &mut Context<Self>,
    ) {
        if key != self.faces_key {
            self.faces_key = key;
            self.faces = faces;
            self.open_mentions(cx);
            self.data_changed(cx);
        }
    }

    /// Its messages' @names open the agents they name (`open_mention`).
    fn open_mentions(&self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let chats = &mut cx.default_global::<MentionChats>().0;
        chats.retain(|chat| chat.upgrade().is_some());
        if !chats.contains(&this) {
            chats.push(this);
        }
    }

    /// Who an @name in its messages can be, by id and name (`mention_markdown`): in the group its speakers, in an
    /// agent's chat every agent.
    fn mention_members(&self) -> Vec<(String, String)> {
        let named = self.group.as_ref().unwrap_or(&self.faces);
        named.iter().map(|(speaker, (name, _, _))| (speaker.clone(), name.clone())).collect()
    }

    /// An agent's full report under its post (send_message's `report`), with who posted it and when, for its page: in
    /// the group the speaker, in the agent's own chat the agent.
    fn post_report(&self, message: &Message) -> Option<crate::screens::coding_result::PostReport> {
        let report = message.report.as_ref().filter(|report| !report.trim().is_empty() && !message.is_user())?;
        let named = message.speaker.as_ref().and_then(|speaker| Some(self.group.as_ref()?.get(speaker)?.0.clone()));
        Some(crate::screens::coding_result::PostReport {
            id: message.id.clone(),
            agent: message.speaker.clone(),
            author: named.unwrap_or_else(|| self.agent_name.clone()),
            created_at: message.creation_time,
            report: report.clone(),
        })
    }

    /// An agent's face, `size` across, on its colour.
    fn face(&self, agent: &str, size: f32, cx: &App) -> Option<AnyElement> {
        let (_, picture, color) = self.faces.get(agent).or_else(|| self.group.as_ref()?.get(agent))?;
        Some(crate::screens::agents::picture_on(picture.clone(), *color, size, cx).into_any_element())
    }

    pub fn set_mentions(&mut self, mentions: Vec<mention_suggestions::Mention>, cx: &mut Context<Self>) {
        let same = mentions.len() == self.mentions.len()
            && mentions.iter().zip(&self.mentions).all(|(new, old)| new.0 == old.0 && new.2 == old.2);
        if !same {
            self.mentions = mentions;
            cx.notify();
        }
    }

    /// The "@" or "/" being typed before the cursor (at the start or after a space), and what follows it.
    fn mention_trigger(&self, cx: &App) -> Option<(usize, char, String)> {
        let input = self.input.read(cx);
        let value = input.value();
        let cursor = input.cursor().min(value.len());
        let before = &value[..cursor];
        // In the group an "@" names a member, and there is nothing else to name.
        let at = if self.group.is_some() { before.rfind('@')? } else { before.rfind(['@', '/'])? };
        if before[..at].chars().next_back().is_some_and(|previous| !previous.is_whitespace())
            || self.mention_dismissed == Some(at)
        {
            return None;
        }
        let typed = &before[at + 1..];
        if typed.chars().any(char::is_whitespace) || typed.len() > 64 {
            return None;
        }
        Some((at, before.as_bytes()[at] as char, typed.to_string()))
    }

    /// Where the "@" or "/" is, and what the list offers for what follows it: agents, apps, skills.
    fn mention_query(&self, cx: &App) -> Option<(usize, Vec<Suggestion>)> {
        let (at, trigger, typed) = self.mention_trigger(cx)?;
        if self.group.is_some() {
            let rows =
                mention_suggestions::suggestions(trigger, &typed, &self.mentions, &[], &[], &[], &[], MAX_MENTION_ROWS);
            return (!rows.is_empty()).then_some((at, rows));
        }
        let (apps, skills, projects) = match &self.mention_sources {
            Some(sources) => {
                let sources = sources.read(cx);
                (
                    mention_suggestions::apps_on(sources, self.mention_agent.as_deref(), cx),
                    sources.skills.clone(),
                    sources.projects.clone(),
                )
            }
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        // This chat's Working rows, less those the message already points at.
        let work: Vec<SharedWork> = super::working_panel::here_for(self)
            .iter()
            .map(SharedWork::from_row)
            .filter(|task| !self.shared_work.iter().any(|shared| shared.key() == task.key()))
            .collect();
        let rows = mention_suggestions::suggestions(
            trigger,
            &typed,
            &self.mentions,
            &apps,
            &skills,
            &work,
            &projects,
            MAX_MENTION_ROWS,
        );
        (!rows.is_empty()).then_some((at, rows))
    }

    /// The apps and skills load the first time an "@" or "/" is typed, and the list follows them as they arrive. The
    /// projects load while "@projects:" is being typed.
    fn ensure_mention_sources(&mut self, cx: &mut Context<Self>) {
        let trigger = if self.group.is_some() { None } else { self.mention_trigger(cx) };
        if self.mention_sources.is_none() && trigger.is_some() {
            let sources = mention_suggestions::sources(cx);
            let store = crate::screens::connectors_store::store(cx);
            let chat = cx.entity_id();
            let wanting = sources.clone();
            self._mention_subs = vec![
                cx.observe(&sources, |_, _, cx| cx.notify()),
                cx.observe(&store, |_, _, cx| cx.notify()),
                // A chat closed while typing "@projects:" stops wanting them.
                cx.on_release(move |_, cx| wanting.update(cx, |sources, cx| sources.want_projects(chat, false, cx))),
            ];
            self.mention_sources = Some(sources);
            if let Some(session) = self.session_id.clone() {
                backend::call(
                    cx,
                    "apps:sessionAgent",
                    json!({ "sessionId": session }),
                    |this: &mut Self, result: Result<String, String>, cx| {
                        this.mention_agent = result.ok();
                        cx.notify();
                    },
                );
            }
        }
        if let Some(sources) = self.mention_sources.clone() {
            let want = trigger
                .is_some_and(|(_, trigger, typed)| trigger == '@' && mention_suggestions::wants_projects(&typed));
            let chat = cx.entity_id();
            sources.update(cx, |sources, cx| sources.want_projects(chat, want, cx));
        }
    }

    fn move_mention(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some((_, matches)) = self.mention_query(cx) else { return };
        let count = matches.len() as isize;
        self.mention_index = ((self.mention_index.min(matches.len() - 1) as isize + step).rem_euclid(count)) as usize;
        cx.notify();
    }

    /// Types the picked row in place of the "@" or "/" and what follows it.
    fn pick_mention(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::EntityInputHandler as _;
        let Some((at, matches)) = self.mention_query(cx) else { return };
        let Some(picked) = matches.get(index.unwrap_or(self.mention_index).min(matches.len() - 1)).cloned() else {
            return;
        };
        let value = self.input.read(cx).value().to_string();
        let cursor = self.input.read(cx).cursor().min(value.len());
        let inserted = picked.insert();
        // Only what was typed after the "@" or "/" is replaced, as typing would: setting the whole text again took
        // every file's chip out of it, its picture with it, leaving the file's name and size as words.
        let start = value[..at].encode_utf16().count();
        let end = start + value[at..cursor].encode_utf16().count();
        self.input.update(cx, |input, cx| input.replace_text_in_range(Some(start..end), &inserted, window, cx));
        self.mention_index = 0;
        let typed = inserted.trim_end();
        if !typed.is_empty() && !self.picked_mentions.iter().any(|picked| picked == typed) {
            self.picked_mentions.push(typed.to_string());
        }
        if let Suggestion::Work(task) = picked {
            self.share_work(task, window, cx);
        }
        cx.notify();
    }

    /// What the composer and the user's messages draw in the accent colour (mention_suggestions::mention_ranges): the
    /// names "@" offers here, the apps and skills once they've loaded, and whatever was picked from the list.
    fn mention_words(&self, cx: &App) -> Vec<String> {
        let mut words: Vec<String> = self.mentions.iter().map(|(name, ..)| format!("@{name}")).collect();
        if self.group.is_none() {
            words.extend(["@skills".to_string(), "@projects".to_string()]);
            if let Some(sources) = &self.mention_sources {
                let sources = sources.read(cx);
                words.extend(
                    mention_suggestions::apps_on(sources, self.mention_agent.as_deref(), cx)
                        .into_iter()
                        .map(|app| format!("@{}", app.handle)),
                );
                words.extend(sources.skills.iter().map(|skill| format!("/{}", skill.name)));
            }
        }
        words.extend(self.picked_mentions.iter().cloned());
        words
    }

    /// The list over the composer while an "@" or "/" is being typed.
    fn render_mentions(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.ensure_mention_sources(cx);
        let colors = *cx.colors();
        let (_, matches) = self.mention_query(cx)?;
        let picked = self.mention_index.min(matches.len() - 1);
        Some(
            div()
                .absolute()
                .left(px(space::SM))
                .bottom(relative(1.))
                .mb(px(space::XS))
                .w(px(300.))
                .p(px(space::XS))
                .flex()
                .flex_col()
                .rounded(px(radius::MD))
                .bg(colors.raised)
                .border_1()
                .border_color(colors.border)
                .shadow_lg()
                .children(matches.into_iter().enumerate().map(|(index, row)| {
                    let (leading, title, subtitle): (AnyElement, String, Option<String>) = match row {
                        // On its colour's disc, as in the Agents list.
                        Suggestion::Agent { name, picture, background } => {
                            let face = match background {
                                Some(color) => {
                                    crate::screens::agents::picture_on(picture, Some(color), 24., cx).into_any_element()
                                }
                                None => crate::avatars::picture(picture, 24.).into_any_element(),
                            };
                            (face, name, None)
                        }
                        Suggestion::App { id, name, .. } => {
                            let icon = match id.as_str() {
                                "computer" => ui::icon_sized("laptop", 18., colors.text_secondary).into_any_element(),
                                "browser" => ui::icon_sized("globe", 18., colors.text_secondary).into_any_element(),
                                _ => crate::screens::connectors_store::icon_for_app(&id, 18., cx),
                            };
                            (icon, name.to_string(), None)
                        }
                        Suggestion::Skills => (
                            ui::icon_sized("skills", 18., colors.text_secondary).into_any_element(),
                            "Skills".to_string(),
                            Some("Attach one of your skills".to_string()),
                        ),
                        Suggestion::Skill { name, description, .. } => (
                            ui::icon_sized("skills", 18., colors.text_secondary).into_any_element(),
                            name,
                            (!description.is_empty()).then_some(description),
                        ),
                        // Which tool works on it and its state, as its Working row says.
                        Suggestion::Work(task) => (
                            crate::screens::coding::tool_tile(&task.tool, 22., cx).into_any_element(),
                            task.label(),
                            Some(format!("{} · {}", task.by, task.status)),
                        ),
                        // As the Coding page lists them.
                        Suggestion::Projects => (
                            ui::icon_sized("folder-git-2", 18., colors.text_secondary).into_any_element(),
                            "Projects".to_string(),
                            Some("Point the message at one of your projects".to_string()),
                        ),
                        Suggestion::Project { name, path, .. } => (
                            ui::icon_sized("folder-git-2", 18., colors.text_secondary).into_any_element(),
                            name,
                            Some(path),
                        ),
                    };
                    div()
                        .id(SharedString::from(format!("mention-{index}")))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .px(px(space::SM))
                        .py(px(space::XS))
                        .rounded(px(radius::SM))
                        .cursor_pointer()
                        .when(index == picked, |this| this.bg(colors.selected_fill))
                        .hover(|style| style.bg(colors.selected_fill.opacity(0.6)))
                        .child(div().size(px(24.)).flex_none().flex().items_center().justify_center().child(leading))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().callout().truncate().child(title))
                                .when_some(subtitle, |this, subtitle| {
                                    this.child(
                                        div().caption().text_color(colors.text_secondary).truncate().child(subtitle),
                                    )
                                }),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.pick_mention(Some(index), window, cx)
                        }))
                }))
                .into_any_element(),
        )
    }

    /// Whoever opened the chat knows it has no messages yet: while it loads, it shows nothing rather than the skeleton.
    pub fn set_known_empty(&mut self, empty: bool, cx: &mut Context<Self>) {
        if self.known_empty != empty {
            self.known_empty = empty;
            cx.notify();
        }
    }

    /// Makes this an agent's chat, whose model is the agent's (agents.rs): no model picker in the composer.
    pub fn fix_model(&mut self, cx: &mut Context<Self>) {
        if !self.model_fixed {
            self.model_fixed = true;
            self.model_pick = None;
            cx.notify();
        }
    }

    pub fn set_agent_name(&mut self, name: String, cx: &mut Context<Self>) {
        if self.agent_name != name {
            self.reasoning_tip = reasoning_tip(&name);
            self.agent_name = name;
            cx.notify();
        }
    }

    /// The note above the composer (smoke test).
    #[cfg(feature = "dev")]
    pub fn composer_hint(&self) -> Option<String> {
        self.hint.clone()
    }

    /// The message box.
    pub fn composer(&self) -> Entity<InputState> {
        self.input.clone()
    }

    /// Files attached to the message being written (smoke test).
    #[cfg(feature = "dev")]
    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
    }

    /// Whether closing this chat would lose something: a send in flight, or a draft,
    /// reply or attachment in the composer.
    pub fn has_unsent_work(&self, cx: &App) -> bool {
        self.pending.is_some()
            || self.voice.is_some()
            || self.reply_to.is_some()
            || !self.attachments.is_empty()
            || !self.shared_notes.is_empty()
            || !self.shared_work.is_empty()
            || !self.input.read(cx).value().is_empty()
    }

    /// What the composer holds and hasn't sent, for the chat lists' "Draft" line: its first line of text, else the
    /// files or notes waiting on it. None while a message is on its way, or when there's nothing.
    pub fn draft_preview(&self, cx: &App) -> Option<String> {
        if self.sending || self.pending.is_some() {
            return None;
        }
        let text = self.sent_text(cx);
        let line = text.lines().map(str::trim).find(|line| !line.is_empty());
        if let Some(line) = line {
            return Some(line.to_string());
        }
        if let ([one], [], []) =
            (self.attachments.as_slice(), self.shared_notes.as_slice(), self.shared_work.as_slice())
        {
            return Some(one.name.clone());
        }
        let count =
            |n: usize, one: &str, many: &str| (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }));
        let parts: Vec<String> = [
            count(self.attachments.len(), "file", "files"),
            count(self.shared_notes.len(), "note", "notes"),
            count(self.shared_work.len(), "task", "tasks"),
        ]
        .into_iter()
        .flatten()
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// A note shared from the Notes tab waits above the composer for the next message; what's typed stays.
    pub fn share_note(&mut self, note: SharedNote, window: &mut Window, cx: &mut Context<Self>) {
        match note_share::add(&self.shared_notes, note) {
            Ok(notes) => {
                self.shared_notes = notes;
                self.hint = None;
            }
            Err(problem) => self.hint = Some(problem.into()),
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn remove_shared_note(&mut self, key: &str, cx: &mut Context<Self>) {
        self.shared_notes.retain(|note| note.key != key);
        cx.notify();
    }

    /// A running task dragged onto the composer or picked after "@": the next message points at it; what's typed
    /// stays.
    pub fn share_work(&mut self, task: SharedWork, window: &mut Window, cx: &mut Context<Self>) {
        match work_share::add(&self.shared_work, task) {
            Ok(work) => {
                self.shared_work = work;
                self.hint = None;
            }
            Err(problem) => self.hint = Some(problem.into()),
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn remove_shared_work(&mut self, key: &str, cx: &mut Context<Self>) {
        self.shared_work.retain(|task| task.key() != key);
        cx.notify();
    }

    /// The notes waiting above the composer, and the chat this is (the note-share check, smoke.rs).
    #[cfg(feature = "dev")]
    pub fn shared_notes(&self) -> &[SharedNote] {
        &self.shared_notes
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn set_placeholder(&mut self, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.placeholder = placeholder;
        if !self.answering {
            self.input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
    }

    pub fn set_draft_prompt(&mut self, prompt: Option<SharedString>, cx: &mut Context<Self>) {
        self.draft_prompt = prompt;
        cx.notify();
    }

    /// Context the next messages carry for the model until one goes through (then `ContextSent(key)`): `sendMessage`'s
    /// `contextNote`.
    pub fn set_send_context(&mut self, context: Option<(String, String)>) {
        self.send_context = context;
    }

    /// A message is on its way (the scripted checks).
    #[cfg(feature = "dev")]
    pub fn delivering(&self) -> bool {
        self.sending || self.pending.is_some()
    }

    /// The scripted checks' look at the context the next message would carry.
    #[cfg(feature = "dev")]
    pub fn send_context(&self) -> Option<&(String, String)> {
        self.send_context.as_ref()
    }

    fn attach_session(&mut self, id: String, cx: &mut Context<Self>) {
        self.session_id = Some(id);
        self.coding_requests.clear();
        self.question_panel.update(cx, |panel, cx| panel.set_requests(Vec::new(), cx));
        self.subscribe_session(cx);
    }

    /// Stops the chat's live queries while it's out of sight (Agents keeps a few live): what it
    /// shows, its rows' measured heights and the reader's place stay, so coming back shows it at once, where it was,
    /// instead of loading and measuring it all again (`unpark`).
    pub fn park(&mut self) {
        if self.parked {
            return;
        }
        self.parked = true;
        self._detail_sub = None;
        self._session_subs.clear();
        self._slow_timer = None;
        self.coding_tick = None;
        self.drop_glance_pages();
    }

    /// Shown again after `park`: its queries start again, and what it showed stays until they answer.
    pub fn unpark(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.parked) {
            return;
        }
        // Only a chat with queries has an answer to wait for (the fixture's come from its driver).
        self.stale = self.session_id.is_some() || self.fixture();
        self.subscribe_session(cx);
    }

    /// The chat's queries.
    fn subscribe_session(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session_id.clone() else { return };
        self.subscribe_detail(cx);
        self._session_subs = vec![
            backend::subscribe(
                cx,
                "coding:requestsForSession",
                json!({ "sessionId": id }),
                |this: &mut Self, result: Result<Vec<ChatRequest>, String>, cx| {
                    if let Ok(requests) = result {
                        this.coding_requests = coding_requests::ordered(requests);
                        this.data_changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "voiceSessions:chatTurns",
                json!({ "sessionId": id }),
                |this: &mut Self, result, cx| {
                    if let Ok(turns) = result {
                        this.turns = turns;
                        this.data_changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "coding:sessionsForChat",
                json!({ "sessionId": id }),
                |this: &mut Self, result: Result<Vec<CodingSession>, String>, cx| {
                    if let Ok(sessions) = result {
                        let working = sessions.iter().any(|session| session.is_live());
                        if working != this.coding_tick.is_some() {
                            this.coding_tick = working.then(|| coding_sessions::ticker(cx));
                        }
                        this.coding_sessions = sessions;
                        this.data_changed(cx);
                    }
                },
            ),
        ];
        self._slow_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            this.update(cx, |this, cx| {
                this.slow_load = true;
                cx.notify();
            })
            .ok();
        }));
    }

    fn subscribe_detail(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.session_id.clone() else { return };
        // The previous page stays on screen until the bigger one arrives.
        self._detail_sub = Some(backend::subscribe(
            cx,
            "voiceSessions:detail",
            json!({ "id": id, "limit": self.limit }),
            |this: &mut Self, result: Result<Option<Detail>, String>, cx| this.detail_arrived(result, cx),
        ));
    }

    /// `voiceSessions:detail`'s answer. The first after `unpark` also says what came while the chat was parked: with
    /// the user looking, where it opens (seen.rs `came_back`) waited for it.
    fn detail_arrived(&mut self, result: Result<Option<Detail>, String>, cx: &mut Context<Self>) {
        let back = std::mem::take(&mut self.stale) && self.attending;
        if back {
            // Replies that came meanwhile go below what showed, not scrolled through unseen.
            self.hold_position();
        }
        #[cfg(feature = "dev")]
        let started = std::time::Instant::now();
        self.detail = Some(result);
        self.data_changed(cx);
        #[cfg(feature = "dev")]
        crate::smoke::detail_handled(started.elapsed());
        if back {
            self.came_back(cx);
            cx.notify();
        }
    }

    /// The reader scrolled the feed: the rows in view out of `count`.
    fn scrolled(&mut self, visible: std::ops::Range<usize>, count: usize, cx: &mut Context<Self>) {
        self.at_bottom = visible.end >= count;
        self.held_at_end = false;
        self.held = None;
        // Within a few rows of the top: load the next page of older messages.
        if visible.start <= 2 {
            self.request_older(cx);
        }
    }

    fn request_older(&mut self, cx: &mut Context<Self>) {
        let has_more = matches!(&self.detail, Some(Ok(Some(detail))) if detail.has_more);
        let loaded = match &self.detail {
            Some(Ok(Some(detail))) => detail.messages.len() as f64,
            _ => 0.,
        };
        // Only widen once the current page has arrived (and not while parked: no queries then).
        if !has_more || self.limit >= MAX_LOADED || loaded < self.limit || self.parked {
            return;
        }
        self.limit = (self.limit + PAGE).min(MAX_LOADED);
        self.subscribe_detail(cx);
    }

    fn detail(&self) -> Option<&Detail> {
        match &self.detail {
            Some(Ok(Some(detail))) => Some(detail),
            _ => None,
        }
    }

    /// `activeTurn`: the oldest turn still queued, dispatching or pending.
    fn active_turn(&self) -> Option<&ChatTurn> {
        self.turns.iter().rev().find(|turn| turn.is_active())
    }

    /// A reply is under way (the pet works at its laptop; companion.rs).
    pub fn replying(&self) -> bool {
        self.active_turn().is_some() || self.delivering_text()
    }

    /// When the reply under way started: the user's message its turn answers, as sent. None when that isn't known
    /// (no turn, or its message not loaded), never another piece of work's start.
    pub fn reply_started_at(&self) -> Option<f64> {
        let message = self.active_turn()?.user_message_id.as_deref()?;
        self.detail()?.messages.iter().find(|candidate| candidate.id == message).map(|message| message.creation_time)
    }

    /// The chat's newest message from Filo, as (chat id, chat title, text), for the pet to say.
    pub fn latest_reply(&self) -> Option<(String, String, String)> {
        let detail = self.detail()?;
        let message = detail
            .messages
            .iter()
            .filter(|message| !message.is_user())
            .max_by(|a, b| a.creation_time.total_cmp(&b.creation_time))?;
        Some((detail.session.id.clone(), detail.session.title.clone(), message.content.clone()))
    }

    fn data_changed(&mut self, cx: &mut Context<Self>) {
        if self.reasoning_pick.is_some() && self.reasoning_pick == self.stored_reasoning() {
            self.reasoning_pick = None;
        }
        if self.model_pick.is_some() && self.model_pick == self.stored_model() {
            self.model_pick = None;
        }
        let awaiting = self.awaiting_turn.is_some();
        self.reconcile_pending();
        if !awaiting && self.awaiting_turn.is_some() {
            // In case the turn never shows: its Thinking row goes after a while.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(AWAIT_TURN).await;
                this.update(cx, |this, cx| this.data_changed(cx)).ok();
            })
            .detach();
        }
        self.reaction_overrides.retain(|id, value| {
            let server =
                self.detail.as_ref().and_then(|detail| detail.as_ref().ok().cloned().flatten()).and_then(|detail| {
                    detail.messages.iter().find(|message| &message.id == id).map(|message| message.reaction.clone())
                });
            server.is_some_and(|server| server != *value)
        });
        let requests = self.coding_requests.clone();
        self.question_panel.update(cx, |panel, cx| panel.set_requests(requests, cx));
        self.sync_seen();
        // Nobody's looking: new replies arrive below the place they left.
        if !self.attending {
            self.hold_position();
        }
        self.rebuild(cx);
        if self.seen_ready {
            self.position_at_divider();
        }
        cx.notify();
    }

    /// Drops the optimistic bubble once the persisted user message shows up.
    fn reconcile_pending(&mut self) {
        if self.awaiting_turn.as_ref().is_some_and(|(request_id, _, since)| {
            since.elapsed() > AWAIT_TURN || self.turns.iter().any(|turn| &turn.request_id == request_id)
        }) {
            self.awaiting_turn = None;
        }
        let Some(pending) = &self.pending else { return };
        let Some(detail) = self.detail() else { return };
        let ids: HashSet<&str> = detail.messages.iter().map(|message| message.id.as_str()).collect();
        let by_turn = self
            .turns
            .iter()
            .filter(|turn| turn.request_id == pending.request_id)
            .find_map(|turn| turn.user_message_id.clone().filter(|id| ids.contains(id.as_str())));
        let storage: Vec<String> = pending.attachments.iter().filter_map(|a| a.storage_id.clone()).collect();
        let by_content = detail.messages.iter().find(|message| {
            message.is_user()
                && !pending.visible_ids.contains(&message.id)
                && message.voice_recording.is_some() == pending.voice_ms.is_some()
                && (pending.voice_ms.is_some() || message.own_content().trim() == pending.text)
                && message.reply_to_message_id == pending.reply.as_ref().map(|reply| reply.message_id.clone())
                && message.attachments.iter().map(|a| a.storage_id.clone()).collect::<Vec<_>>() == storage
        });
        if let Some(id) = by_turn.or(by_content.map(|message| message.id.clone())) {
            let key = pending.key.clone();
            if pending.turn_id.is_some() && !self.turns.iter().any(|turn| turn.request_id == pending.request_id) {
                self.awaiting_turn = Some((pending.request_id.clone(), id.clone(), std::time::Instant::now()));
            }
            self.handed_over.insert(id, key);
            self.pending = None;
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        if let Some(detail) = self.detail().cloned() {
            if detail.has_more && self.limit < MAX_LOADED {
                rows.push(ViewRow::Older);
            }
            let mut messages = detail.messages.clone();
            for message in &mut messages {
                if let Some(reaction) = self.reaction_overrides.get(&message.id) {
                    message.reaction = reaction.clone();
                }
            }
            // The message being sent goes in as the message it will be: grouped, dated and laid out like it.
            if let Some(pending) = &self.pending {
                let newest = messages.iter().map(|message| message.creation_time).fold(f64::NEG_INFINITY, f64::max);
                messages.push(pending.message(pending.created_at.max(newest + 1.)));
            }
            // Sessions and cards older than the loaded history wait for their page.
            let loaded_from = detail
                .has_more
                .then(|| messages.iter().map(|message| message.creation_time).fold(f64::INFINITY, f64::min))
                .filter(|from| from.is_finite());
            let items = feed::place_failed_turns(
                feed::build_feed(&messages, &detail.tasks),
                &self.turns,
                &messages,
                self.pending.as_ref().map(|pending| pending.request_id.as_str()),
                loaded_from,
            );
            let items = coding_sessions::place(items, &self.coding_sessions, loaded_from);
            let items = coding_requests::place(items, &self.coding_requests, loaded_from);
            let sent = self.question_panel.read(cx).sent();
            let glance_looks: HashMap<String, glance::Look> = messages
                .iter()
                .filter_map(|message| {
                    message.glance.as_ref().map(|glance| (message.id.clone(), self.glance_look(glance, cx)))
                })
                .collect();
            let stopped = feed::stopped_labels(&messages, &self.turns);
            let turns_by_user: HashMap<&str, &ChatTurn> =
                self.turns.iter().filter_map(|turn| turn.user_message_id.as_deref().map(|id| (id, turn))).collect();
            let day_rows = feed::day_rows(items);
            let late = feed::late_replies(&day_rows);
            for (index, row) in day_rows.iter().enumerate() {
                match row {
                    Row::Gap(day) => rows.push(ViewRow::Gap(day.clone())),
                    Row::Mark(label) => rows.push(ViewRow::Mark(label.clone())),
                    // In the group, where it went off or on: a line like a time's, with its time.
                    Row::Item(FeedItem::Message { message, .. }) if message.mark.is_some() => {
                        rows.push(ViewRow::Mark(format!(
                            "{} · {}",
                            message.mark.as_deref().unwrap_or_default(),
                            format::clock(message.creation_time)
                        )));
                    }
                    Row::Item(FeedItem::Message { message, .. }) if message.group_call.is_some() => {
                        let call = message.group_call.clone().expect("matched");
                        let failed = turns_by_user
                            .get(message.id.as_str())
                            .filter(|turn| shows_failure(turn))
                            .map(|turn| (*turn).clone());
                        rows.push(ViewRow::GroupCall {
                            id: message.id.clone(),
                            call,
                            expanded: self.open_group_calls.contains(&message.id),
                            failed,
                        });
                    }
                    // What it posted in the group reads in full, as a reply of its own (`render_message`).
                    Row::Item(FeedItem::Message { message, .. })
                        if message.group_link.is_some() && message.posted_in_group().is_none() =>
                    {
                        let link = message.group_link.clone().expect("matched");
                        let quote = message
                            .content
                            .split_once(" in the group: ")
                            .map_or(String::new(), |(_, quote)| quote.to_string());
                        rows.push(ViewRow::GroupLink { id: message.id.clone(), link, quote });
                    }
                    // A run that failed keeps its bubble, with what went wrong under it.
                    Row::Item(FeedItem::Message { message, .. })
                        if message.routine.is_some()
                            && turns_by_user.get(message.id.as_str()).is_none_or(|turn| turn.state != "failed") =>
                    {
                        rows.push(ViewRow::Routine {
                            id: message.id.clone(),
                            mark: message.routine.clone().expect("matched"),
                        });
                    }
                    Row::Item(FeedItem::Message { message }) => {
                        if self.divider_before(message) && !rows.contains(&ViewRow::Unseen) {
                            rows.push(ViewRow::Unseen);
                        }
                        let neighbour = |offset: isize| match day_rows.get((index as isize + offset) as usize) {
                            Some(Row::Item(FeedItem::Message { message, .. })) if index as isize + offset >= 0 => {
                                Some(message)
                            }
                            _ => None,
                        };
                        let joins_prev = neighbour(-1).is_some_and(|previous| feed::share_group(previous, message));
                        let joins_next = neighbour(1).is_some_and(|next| feed::share_group(message, next));
                        let failed = turns_by_user
                            .get(message.id.as_str())
                            .filter(|turn| shows_failure(turn))
                            .map(|turn| (*turn).clone());
                        let glance = glance_looks.get(&message.id).cloned();
                        rows.push(ViewRow::Message {
                            message: message.clone(),
                            joins_prev,
                            joins_next,
                            late: late.contains(&message.id),
                            stopped: stopped.get(&message.id).copied(),
                            failed,
                            sending: self
                                .pending
                                .as_ref()
                                .filter(|pending| pending.key == message.id)
                                .map(|pending| Sending { error: pending.error.clone() }),
                            glance,
                            proof_task: proof_task(message, &self.coding_sessions),
                            reply: None,
                        });
                        // Under the message whose turn compacted the session: the engine summarized what came before.
                        if turns_by_user.get(message.id.as_str()).is_some_and(|turn| turn.compacted) {
                            rows.push(ViewRow::Mark(COMPACTED_MARK.into()));
                        }
                        // Under the message whose turn started a fresh session: the engine has the newest messages.
                        if turns_by_user.get(message.id.as_str()).is_some_and(|turn| turn.reseeded) {
                            rows.push(ViewRow::Mark(RESEEDED_MARK.into()));
                        }
                    }
                    Row::Item(FeedItem::Task(task)) => {
                        if !task.summary.is_empty() || task.artifact.is_some() {
                            rows.push(ViewRow::Task(task.clone()));
                        }
                    }
                    Row::Item(FeedItem::FailedTurn(turn)) => rows.push(ViewRow::OrphanFailed(turn.clone())),
                    Row::Item(FeedItem::Coding(sessions)) => rows.push(ViewRow::CodingSessions(sessions.clone())),
                    // A task's questions waiting together share one line; each answered or closed one is its own card.
                    Row::Item(FeedItem::CodingRequest(request)) => {
                        let sent =
                            sent.iter().find(|(id, _)| *id == request.request_id).map(|(_, answer)| answer.clone());
                        let waiting = request.state == "pending" && sent.is_none();
                        match rows.last_mut() {
                            Some(ViewRow::CodingAsk(requests))
                                if waiting && requests.last().is_some_and(|last| last.task_id == request.task_id) =>
                            {
                                requests.push(request.clone())
                            }
                            _ if waiting => rows.push(ViewRow::CodingAsk(vec![request.clone()])),
                            _ => rows.push(ViewRow::CodingAnswered {
                                request: request.clone(),
                                sent,
                                expanded: self.expanded_asks.contains(&request.request_id),
                            }),
                        }
                    }
                }
            }
        } else if let Some(pending) = &self.pending {
            // A new chat's first message, before the chat has loaded.
            rows.push(ViewRow::Message {
                message: pending.message(pending.created_at),
                joins_prev: false,
                joins_next: false,
                late: false,
                stopped: None,
                failed: None,
                sending: Some(Sending { error: pending.error.clone() }),
                glance: None,
                proof_task: None,
                reply: None,
            });
        }
        self.link_replies(&mut rows);
        let awaiting_turn = self.pending.as_ref().is_some_and(|pending| {
            pending.error.is_none() && !self.turns.iter().any(|turn| turn.request_id == pending.request_id)
        }) || self.awaiting_turn.as_ref().is_some_and(|(_, message_id, _)| {
            // Until something comes after the sent message.
            self.detail().and_then(|detail| detail.messages.last()).is_some_and(|last| &last.id == message_id)
        });
        let routing = self
            .active_turn()
            .filter(|turn| turn.activity.as_deref() == Some(ROUTING) && self.group.is_some())
            .map(|turn| turn.routing);
        if let Some(engine) = routing {
            rows.push(ViewRow::Routing(engine));
        } else if self.active_turn().is_some() || awaiting_turn {
            let speaker = self.active_turn().and_then(|turn| turn.speaker.clone()).filter(|_| self.group.is_some());
            rows.push(ViewRow::Thinking(self.active_turn().and_then(|turn| turn.activity.clone()), speaker));
        }
        // Clear of the composer, with room for a reaction chip hanging off the last bubble.
        rows.push(ViewRow::Spacer((self.composer_height + space::XXXL).round() as u32));
        self.apply_rows(rows);
        if let Some((id, tries)) = self.reveal_pending.take() {
            if self.rows.iter().any(|row| matches!(row, ViewRow::Message { message, .. } if message.id == id)) {
                self.jump_to(&id, cx);
            } else if tries > 0 && self.detail().is_none() {
                self.reveal_pending = Some((id, tries - 1));
            } else if self.detail().is_some_and(|detail| detail.has_more && (detail.messages.len() as f64) < self.limit)
            {
                // A longer page is on its way (`reveal_message_at`).
                self.reveal_pending = Some((id, tries));
            } else if tries > 0 && self.session_id.is_some() && self.detail().is_some_and(|detail| detail.has_more) {
                // Not on the pages loaded (newer messages came since its depth was counted): a page further back.
                self.limit += PAGE;
                self.subscribe_detail(cx);
                self.reveal_pending = Some((id, tries - 1));
            }
        }
    }

    /// The feed's room under its rows follows the composer's height, every frame while it glides: the spacer is
    /// spliced alone, since nothing above it changed.
    fn fit_spacer(&mut self, cx: &mut Context<Self>) {
        let height = (self.composer_height + space::XXXL).round() as u32;
        let (last, sigs_match) = (self.rows.len().saturating_sub(1), self.row_sigs.len() == self.rows.len());
        match self.rows.last_mut() {
            Some(ViewRow::Spacer(spacer)) if sigs_match => {
                if *spacer != height {
                    *spacer = height;
                    self.row_sigs[last].sig = feed_rows::sig(format!("{:?}", self.rows[last]));
                    self.list_state.splice(last..last + 1, 1);
                }
            }
            _ => self.rebuild(cx),
        }
    }

    /// Tells the list which rows came, went or changed, by key, so the row at the top of the view stays where it was
    /// (feed_rows::sync_list); following the newest, the list keeps following. One splice over everything that
    /// changed would put the view at the first changed row whenever rows changed both above and below the one being
    /// read (a coding session starting under it while an older one leaves the list above it).
    fn apply_rows(&mut self, rows: Vec<ViewRow>) {
        let mut seen: HashMap<String, usize> = HashMap::new();
        let sigs: Vec<RowSig> = rows
            .iter()
            .map(|row| {
                let mut key = row_key(row, &self.handed_over);
                let repeats = seen.entry(key.clone()).or_default();
                if *repeats > 0 {
                    key = format!("{key}#{repeats}");
                }
                *repeats += 1;
                RowSig { key, sig: feed_rows::sig(format!("{row:?}")) }
            })
            .collect();
        // Older history arriving above the message being read leaves it where it is on screen (`holds_place`).
        let pin = feed_rows::sync_list_anchored(&self.list_state, &self.row_sigs, &sigs, holds_place);
        if let Some(pin) = pin {
            self.held = Some(Held { pin, since: std::time::Instant::now(), placed: None, viewport: px(0.) });
        } else if let Some(held) = &mut self.held {
            // The list was just moved to keep its place: that isn't the reader scrolling.
            held.placed = None;
        }
        self.rows = rows;
        self.row_sigs = sigs;
    }

    /// Before the list lays out: the held message goes back to where it was on screen, whatever the rows above it
    /// measure now (feed_rows::place_pin), until the reader scrolls or the rows have had time to settle.
    fn hold_pin(&mut self) {
        let Some(held) = &mut self.held else { return };
        let top = self.list_state.logical_scroll_top();
        let moved = held.placed.is_some_and(|placed| placed != (top.item_ix, top.offset_in_item));
        let index = self.row_sigs.iter().position(|row| row.key == held.pin.key);
        match index {
            Some(index) if !moved && held.since.elapsed() < HOLD_FOR => {
                held.viewport = self.list_state.viewport_bounds().size.height;
                feed_rows::place_pin(&self.list_state, index, &held.pin);
            }
            _ => self.held = None,
        }
    }

    /// After the list laid out: where the held message's frame left it.
    fn note_held(&mut self) {
        if let Some(held) = &mut self.held {
            let top = self.list_state.logical_scroll_top();
            held.placed = Some((top.item_ix, top.offset_in_item));
        }
    }

    /// The message being sent shows at the end of the feed, above the composer: the list follows its end (gpui's
    /// bottom), which holds whatever the new rows measure. Revealing the last row instead would go by their heights
    /// before they're measured, and stop short with the message under the composer.
    pub(super) fn show_sent(&mut self) {
        self.list_state.scroll_to(gpui::ListOffset { item_ix: self.list_state.item_count(), offset_in_item: px(0.) });
        self.held = None;
        self.held_at_end = false;
        self.at_bottom = true;
        self.at_end = true;
    }

    // --- Composer ---

    pub(crate) fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn show_send_action(&self, cx: &App) -> bool {
        !self.text(cx).is_empty() || !self.attachments.is_empty()
    }

    fn delivering_text(&self) -> bool {
        (self.sending || self.pending.is_some())
            && self.pending.as_ref().is_none_or(|pending| pending.error.is_none() && pending.voice_ms.is_none())
    }

    fn show_stop(&self, cx: &App) -> bool {
        !self.show_send_action(cx)
            && self.session_id.is_some()
            && (self.active_turn().is_some() || self.delivering_text())
    }

    pub(crate) fn send_disabled(&self, cx: &App) -> bool {
        self.sending
            || self.pending.is_some()
            || (self.text(cx).is_empty() && self.attachments.is_empty())
            // Every chat has its conversation before it shows (an agent's opens it first, `agents:openChat`).
            || self.session_id.is_none()
            || self.detail().is_none()
    }

    /// Sends what the composer holds. It shows in the chat at once, as the message it will be; files still uploading
    /// go with it, and it's delivered once they're up.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // On a question in the panel above, what's typed is the answer to it.
        if self.answering && self.attachments.is_empty() && !self.text(cx).is_empty() {
            let text = self.text(cx);
            if self.question_panel.update(cx, |panel, cx| panel.take_typed(text, window, cx)) {
                self.input.update(cx, |input, cx| input.set_value("", window, cx));
                self.focus_panel = true;
                cx.notify();
                return;
            }
        }
        if self.send_disabled(cx) {
            return;
        }
        // A file's name and size left as words would go instead of the file.
        if let Err(name) = self.restore_chips(window, cx) {
            self.hint = Some(format!("{name} isn't attached any more. Attach it again, or delete its name."));
            cx.notify();
            return;
        }
        let text = self.sent_text(cx);
        if self.reply_to.is_some()
            || !self.shared_notes.is_empty()
            || !self.shared_work.is_empty()
            || !self.attachments.is_empty()
        {
            // Its strips go with it: the feed's room for the composer shrinks now, with the composer, not a frame later
            // (the message would show above where it lands, then drop).
            self.composer_height = (self.composer_height - self.strips_height.get()).max(0.);
        }
        // So do the text's lines, though the row glides down to one line (composer_layout.rs).
        self.composer_height = (self.composer_height - self.composer_layout.sent()).max(0.);
        let reply = self.reply_to.take();
        let visible_ids =
            self.detail().map(|detail| detail.messages.iter().map(|m| m.id.clone()).collect()).unwrap_or_default();
        let mut pending = PendingSend::new(uuid::Uuid::new_v4().to_string(), text, reply, visible_ids);
        pending.attachments = std::mem::take(&mut self.attachments);
        pending.notes = std::mem::take(&mut self.shared_notes);
        pending.work = std::mem::take(&mut self.shared_work);
        pending.context = self.send_context.clone();
        self.pending = Some(pending);
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
        self.sending = true;
        self.hint = None;
        self.rebuild(cx);
        self.show_sent();
        cx.notify();
        self.deliver_pending(cx);
    }

    /// Delivers the message being sent once there's a conversation for it and its files have uploaded.
    fn deliver_pending(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else { return };
        let model = self.unsaved_model();
        let Some(pending) = &mut self.pending else { return };
        if pending.voice_ms.is_some()
            || pending.args.is_some()
            || pending.error.is_some()
            || !pending.failed_uploads.is_empty()
            || !pending.uploaded()
        {
            return;
        }
        let stored = match &self.detail {
            Some(Ok(Some(detail))) => detail.session.reasoning.as_deref(),
            _ => None,
        };
        let mut args = message_args(&session_id, pending, reasoning_to_send(self.reasoning_pick, stored));
        if let Some(model) = &model {
            args = with_model(args, model);
        }
        let context_key = pending.context.as_ref().map(|(key, _)| key.clone());
        pending.args = Some(args.clone());
        pending.context_key = context_key.clone();
        self.deliver_args(args, context_key, cx);
    }

    /// The chat's reasoning level as the backend has it, when it has one.
    fn stored_reasoning(&self) -> Option<&'static str> {
        self.detail()?.session.reasoning.as_deref().and_then(reasoning_level)
    }

    /// The chat's reasoning level: the user's pick on its way, else the chat's. None when it never picked one: its turns
    /// then think at the tool's own level (Muse Code's and OpenCode's at the Coding page's).
    pub(crate) fn reasoning(&self) -> Option<&'static str> {
        self.reasoning_pick.or_else(|| self.stored_reasoning())
    }

    /// The reasoning levels the chat's model takes, least first: the chat's own (`detail`, an agent's model in its
    /// chat), or for a model picked here that it doesn't have yet the backend's. Asked again after a while, sooner while
    /// none are known (Muse Code and OpenCode tell theirs in the background), rather than on every frame. Claude Code's
    /// without a backend (fixtures).
    fn reasoning_levels(&mut self, cx: &App) -> Vec<String> {
        let pick = self.model_pick.clone().filter(|_| !self.model_fixed && self.model_pick != self.stored_model());
        let session = self.session_id.clone().filter(|_| pick.is_none() && !self.fixture());
        if let Some(detail) =
            self.detail().filter(|detail| session.is_some() && !detail.session.reasoning_levels.is_empty())
        {
            return detail.session.reasoning_levels.clone();
        }
        let key = match &session {
            Some(id) => format!("chat {id}"),
            None => format!("{:?}", pick.clone().unwrap_or_else(|| self.model_choice())),
        };
        let fresh = |(kept, at, levels): &(String, std::time::Instant, Vec<String>)| {
            *kept == key
                && at.elapsed() < if levels.is_empty() { REASONING_LEVELS_FRESH / 12 } else { REASONING_LEVELS_FRESH }
        };
        if !self.asked_levels.as_ref().is_some_and(fresh) {
            let levels = match (cx.try_global::<backend::BackendGlobal>().filter(|_| !self.fixture()), &session) {
                (Some(backend), Some(id)) => backend.0.session_reasoning_levels(id),
                (Some(backend), None) => {
                    let (engine, model) = pick.unwrap_or_else(|| self.model_choice());
                    backend.0.reasoning_levels(engine, model.as_deref())
                }
                (None, _) => CLAUDE_EFFORTS.iter().map(|level| level.to_string()).collect(),
            };
            self.asked_levels = Some((key, std::time::Instant::now(), levels));
        }
        self.asked_levels.as_ref().map(|(_, _, levels)| levels.clone()).unwrap_or_default()
    }

    /// The composer's reasoning picker: shows at once, saved on the chat (`voiceSessions:setReasoning`), or, in a new
    /// chat with no session yet, sent with its first message.
    pub(crate) fn set_reasoning(&mut self, level: &str, cx: &mut Context<Self>) {
        let Some(level) = reasoning_level(level) else { return };
        if self.reasoning() == Some(level) {
            return;
        }
        self.reasoning_pick = Some(level);
        cx.notify();
        let Some(session_id) = self.session_id.clone().filter(|_| !self.fixture()) else { return };
        let args = json!({ "sessionId": session_id, "reasoning": level });
        backend::call(
            cx,
            "voiceSessions:setReasoning",
            args,
            move |this: &mut Self, result: Result<Value, String>, cx| {
                if result.is_err() && this.reasoning_pick == Some(level) {
                    this.reasoning_pick = None;
                    this.hint = Some("Reasoning level wasn’t saved. Try again.".into());
                    cx.notify();
                }
            },
        );
    }

    /// The chat's engine and model as the backend has them, when the user picked them for it.
    fn stored_model(&self) -> Option<ModelChoice> {
        let session = &self.detail()?.session;
        Some((Engine::parse(session.engine.as_deref()?)?, session.model.clone()))
    }

    /// Who answers in this chat: the user's pick on its way, else the chat's, else the app's default engine (the
    /// header's) with its own model.
    pub(crate) fn model_choice(&self) -> ModelChoice {
        self.model_pick
            .clone()
            .or_else(|| self.stored_model())
            .unwrap_or_else(|| (crate::local_backend::default_engine(), None))
    }

    /// The composer's model picker: shows at once, saved on the chat (`voiceSessions:setModel`), or, in a new chat with
    /// no session yet, sent with its first message. Picking the default engine's own model still keeps it for the chat.
    pub(crate) fn set_model(&mut self, choice: ModelChoice, cx: &mut Context<Self>) {
        if self.model_pick.clone().or_else(|| self.stored_model()).as_ref() == Some(&choice) {
            return;
        }
        self.model_pick = Some(choice.clone());
        cx.notify();
        let Some(session_id) = self.session_id.clone().filter(|_| !self.fixture()) else { return };
        let args = with_model(json!({ "sessionId": session_id }), &choice);
        backend::call(cx, "voiceSessions:setModel", args, move |this: &mut Self, result: Result<Value, String>, cx| {
            if result.is_err() && this.model_pick.as_ref() == Some(&choice) {
                this.model_pick = None;
                this.hint = Some("Model wasn’t saved. Try again.".into());
                cx.notify();
            }
        });
    }

    /// The model picked in a new chat that its session doesn't have yet (a voice message saves it apart).
    pub(super) fn unsaved_model(&self) -> Option<ModelChoice> {
        model_to_send(self.model_pick.as_ref(), self.stored_model().as_ref())
    }

    fn deliver_args(&mut self, args: Value, context_key: Option<String>, cx: &mut Context<Self>) {
        self.sending = true;
        let delivered = match self.fixture() {
            #[cfg(feature = "dev")]
            true => send_fixture::deliver(args, cx),
            _ => {
                let backend = backend(cx);
                cx.spawn(async move |_, _| backend.call_raw("voiceSessions:sendMessage", args).await)
            }
        };
        cx.spawn(async move |this, cx| {
            let result = delivered.await;
            this.update(cx, |this, cx| {
                this.sending = false;
                this.handle_send_result(result, context_key, cx);
            })
            .ok();
        })
        .detach();
    }

    fn handle_send_result(
        &mut self,
        result: Result<Value, String>,
        context_key: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let (Ok(_), Some(key)) = (&result, context_key) {
            cx.emit(ConversationEvent::ContextSent(key));
        }
        match result {
            Ok(value) => {
                if let Some(pending) = &mut self.pending {
                    pending.turn_id = value.get("turnId").and_then(Value::as_str).map(str::to_string);
                }
            }
            Err(error) => {
                if let Some(pending) = &mut self.pending {
                    pending.error = Some(
                        if error.contains("Up to 5 files") || error.contains("limit") || error.contains("on this Mac") {
                            error
                        } else {
                            "Message delivery could not be confirmed. Retry to check again.".into()
                        },
                    );
                }
            }
        }
        self.data_changed(cx);
    }

    /// Uploads the files that failed again, or resends the identical request (the backend dedupes on `requestId`).
    fn retry_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = &mut self.pending else { return };
        pending.error = None;
        let failed = std::mem::take(&mut pending.failed_uploads);
        let resend = pending.args.clone().map(|args| (args, pending.context_key.clone()));
        self.sending = true;
        if !failed.is_empty() {
            for (local_id, path, name) in failed {
                self.start_upload(local_id, path, name, window, cx);
            }
        } else if let Some((args, context_key)) = resend {
            self.deliver_args(args, context_key, cx);
        } else {
            self.deliver_pending(cx);
        }
        self.rebuild(cx);
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else { return };
        if self.stopping {
            return;
        }
        self.stopping = true;
        cx.notify();
        backend::call(
            cx,
            "voiceSessions:stopChatTurns",
            json!({ "sessionId": session_id }),
            |this: &mut Self, result: Result<Value, String>, cx| {
                this.stopping = false;
                if result.is_err() {
                    this.hint = Some("Filo could not be stopped. Please try again.".into());
                }
                cx.notify();
            },
        );
    }

    fn retry_turn(&mut self, turn_id: String, cx: &mut Context<Self>) {
        if self.retrying.is_some() {
            return;
        }
        self.retrying = Some(turn_id.clone());
        self.retry_error = None;
        cx.notify();
        let retried = backend(cx)
            .call_raw("voiceSessions:retryChatTurn", json!({ "turnId": turn_id, "sessionId": self.session_id }));
        cx.spawn(async move |this, cx| {
            let result = retried.await;
            this.update(cx, |this, cx| {
                this.retrying = None;
                if result.is_err() {
                    this.retry_error = Some((turn_id, "Retry could not be confirmed. Check your connection.".into()));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn react(&mut self, message: &Message, emoji: &str, cx: &mut Context<Self>) {
        let current = self.reaction_overrides.get(&message.id).cloned().unwrap_or(message.reaction.clone());
        let next = if current.as_deref() == Some(emoji) { None } else { Some(emoji.to_string()) };
        self.reaction_overrides.insert(message.id.clone(), next.clone());
        self.picker_for = None;
        self.rebuild(cx);
        cx.notify();
        let mut args = json!({ "messageId": message.id, "sessionId": self.session_id });
        if let Some(reaction) = &next {
            args["reaction"] = json!(reaction);
        }
        let id = message.id.clone();
        backend::call(
            cx,
            "voiceSessions:reactToMessage",
            args,
            move |this: &mut Self, result: Result<Value, String>, cx| {
                if result.is_err() {
                    this.reaction_overrides.remove(&id);
                    this.hint = Some("Reaction wasn’t saved. Try again.".into());
                    this.rebuild(cx);
                    cx.notify();
                }
            },
        );
    }

    fn reply(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) {
        let content =
            if message.voice_recording.is_some() { "Voice message".to_string() } else { Self::shown_text(message) };
        self.reply_to = Some(ReplyTarget {
            message_id: message.id.clone(),
            is_user: message.is_user(),
            author: self.author(message),
            content,
        });
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Words selected in a message, put at the end of what's typed as a quote, with the cursor under it.
    pub(super) fn quote(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::input::RopeExt as _;
        let block = quote_block(text);
        if block.is_empty() {
            return;
        }
        self.input.update(cx, |input, cx| {
            let typed = input.value().to_string();
            let end = input.text().offset_to_position(typed.len());
            input.set_cursor_position(end, window, cx);
            input.insert(format!("{}{block}", quote_gap(&typed)), window, cx);
        });
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn copy_text(message: &Message) -> String {
        if message.voice_recording.is_some() {
            return String::new();
        }
        Self::shown_text(message)
    }

    /// A message's text as its bubble shows it: the user's without the paths of their files.
    pub(super) fn shown_text(message: &Message) -> String {
        if message.is_user() {
            attachments::without_files(message.own_content(), &message.attachments)
        } else {
            message.content.clone()
        }
    }

    pub fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |this, window, cx| this.attach_files(paths, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Files the user attached (picked, dropped, pasted): uploaded, and each shown as a chip where the cursor is, so
    /// their words can point at it ("here is this image: …"). The message goes with the file's path in the chip's place
    /// (or its name, for a copy Filo made), so Filo can hand the path on to a coding tool.
    fn attach_files(&mut self, paths: Vec<std::path::PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        for (local_id, path) in self.add_files(paths, window, cx) {
            self.show_chip(local_id, path, window, cx);
        }
        self.input.focus_handle(cx).focus(window);
    }

    /// Puts attachment `local_id`'s chip in the composer at the cursor, and its picture in it once that's ready.
    fn show_chip(&mut self, local_id: u64, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attachment) = self.attachments.iter().find(|attachment| attachment.local_id == local_id) else {
            return;
        };
        let picture = attachment.content_type.starts_with("image/");
        let chip = attachments::chip_text(&attachment.name, attachment.size, picture);
        let atom = Self::chip_atom(local_id, &chip, cx);
        attachments::remember_chip(&chip.text, &path, cx);
        self.input.update(cx, |input, cx| {
            let value = input.value();
            let before = value.get(..input.cursor()).unwrap_or(&value);
            if !before.is_empty() && !before.ends_with(char::is_whitespace) {
                input.insert(" ", window, cx);
            }
            input.insert_atom(&chip.text, atom, window, cx);
            input.insert(" ", window, cx);
        });
        self.chips.push(local_id);
        if picture {
            self.load_chip_picture(local_id, path, cx);
        }
    }

    /// The input's atom for attachment `local_id`'s chip (`range` set where it goes).
    fn chip_atom(local_id: u64, chip: &attachments::ChipText, cx: &App) -> InputAtom {
        let colors = *cx.colors();
        InputAtom {
            id: local_id,
            range: 0..0,
            image: None,
            image_len: chip.image_len,
            muted_from: chip.muted_from,
            background: colors.surface_strong,
            border: colors.border,
            muted: colors.text_secondary,
        }
    }

    /// Chip text in the composer that lost its chip (copied from a message being written and pasted here, in another
    /// chat or a fresh window, or brought back by undo) becomes its file's chip again, the file attached again. The
    /// name of one whose file this Mac doesn't know, which would otherwise go as words and leave the picture behind.
    fn restore_chips(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let (value, atoms, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.atoms().to_vec(), input.cursor())
        };
        let ranges: Vec<std::ops::Range<usize>> = atoms.iter().map(|atom| atom.range.clone()).collect();
        let loose = attachments::loose_chips(&value, &ranges);
        if loose.is_empty() {
            return Ok(());
        }
        let mut missing = None;
        // (where the loose text is, its new chip's text and atom)
        let mut restored = Vec::new();
        for (range, name) in loose {
            let added = attachments::chip_file(&value[range.clone()], cx)
                .and_then(|file| self.add_files(vec![file], window, cx).into_iter().next());
            let Some((local_id, path)) = added else {
                missing.get_or_insert(name);
                continue;
            };
            let Some(attachment) = self.attachments.iter().find(|attachment| attachment.local_id == local_id) else {
                continue;
            };
            let picture = attachment.content_type.starts_with("image/");
            let chip = attachments::chip_text(&attachment.name, attachment.size, picture);
            attachments::remember_chip(&chip.text, &path, cx);
            restored.push((range, Self::chip_atom(local_id, &chip, cx), chip.text, picture.then_some(path)));
        }
        if !restored.is_empty() {
            // The text again with the chips in it; the chips already there move with what changed before them.
            let mut text = String::new();
            let mut kept = atoms;
            let mut new_atoms = Vec::new();
            let mut new_cursor = cursor;
            let mut copied = 0;
            for (range, atom, chip, _) in restored.iter_mut() {
                text.push_str(&value[copied..range.start]);
                atom.range = text.len()..text.len() + chip.len();
                text.push_str(chip);
                let shift = chip.len() as isize - range.len() as isize;
                for atom in kept.iter_mut().filter(|atom| atom.range.start >= range.end) {
                    atom.range =
                        (atom.range.start as isize + shift) as usize..(atom.range.end as isize + shift) as usize;
                }
                if cursor >= range.end {
                    new_cursor = (new_cursor as isize + shift) as usize;
                }
                new_atoms.push(atom.clone());
                copied = range.end;
            }
            text.push_str(&value[copied..]);
            kept.extend(new_atoms);
            self.chips.extend(restored.iter().map(|(_, atom, _, _)| atom.id));
            self.input.update(cx, |input, cx| {
                input.set_value(text, window, cx);
                input.set_atoms(kept, cx);
                let position =
                    gpui_component::RopeExt::offset_to_position(input.text(), new_cursor.min(input.value().len()));
                input.set_cursor_position(position, window, cx);
            });
            for (_, atom, _, picture) in restored {
                if let Some(path) = picture {
                    self.load_chip_picture(atom.id, path, cx);
                }
            }
        }
        match missing {
            Some(name) => Err(name),
            None => Ok(()),
        }
    }

    fn load_chip_picture(&mut self, local_id: u64, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let input = self.input.clone();
        let thumbnail = cx.background_spawn(async move { attachments::thumbnail(&path) });
        cx.spawn(async move |_, cx| {
            if let Some(image) = thumbnail.await {
                input.update(cx, |input, cx| input.set_atom_image(local_id, image, cx)).ok();
            }
        })
        .detach();
    }

    /// The composer's text as it is sent: each chip replaced by its file's path, or its name when it has none.
    fn sent_text(&self, cx: &App) -> String {
        let input = self.input.read(cx);
        let chips: Vec<(std::ops::Range<usize>, String)> = input
            .atoms()
            .iter()
            .map(|atom| {
                let file = self.attachments.iter().find(|attachment| attachment.local_id == atom.id);
                let text = file.map_or_else(String::new, |file| {
                    file.local_path.as_deref().map_or_else(|| file.name.clone(), attachments::mention)
                });
                (atom.range.clone(), text)
            })
            .collect();
        attachments::with_paths(&input.value(), &chips).trim().to_string()
    }

    /// Takes attachment `local_id` off the message, with its chip.
    fn remove_attachment(&mut self, local_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.retain(|attachment| attachment.local_id != local_id);
        if self.chips.contains(&local_id) {
            self.chips.retain(|id| *id != local_id);
            self.input.update(cx, |input, cx| input.remove_atom(local_id, window, cx));
        }
        cx.notify();
    }

    /// Adds and uploads the files; returns those added, with their local ids.
    fn add_files(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(u64, std::path::PathBuf)> {
        let mut added = Vec::new();
        for path in paths {
            if !path.is_file() {
                continue;
            }
            if self.attachments.len() >= attachments::MAX_CHAT_ATTACHMENTS {
                self.hint = Some(attachments::TOO_MANY.into());
                break;
            }
            let local_id = self.next_local_id;
            self.next_local_id += 1;
            let content_type = attachments::content_type_for(&path);
            let name = if crate::pasteboard::is_pasted(&path) {
                let taken: Vec<&str> = self.attachments.iter().map(|attachment| attachment.name.as_str()).collect();
                attachments::pasted_name(&path, &taken)
            } else {
                path.file_name().and_then(|name| name.to_str()).unwrap_or("file").to_string()
            };
            let size = std::fs::metadata(&path).map(|meta| meta.len() as usize).unwrap_or(0);
            let local_path = attachments::users_file(&path).then(|| path.clone());
            added.push((local_id, path.clone()));
            let upload_name = name.clone();
            self.attachments.push(PendingAttachment {
                local_id,
                name,
                content_type: content_type.clone(),
                size,
                preview: content_type.starts_with("image/").then(|| path.clone()),
                storage_id: None,
                local_path,
            });
            self.start_upload(local_id, path, upload_name, window, cx);
        }
        cx.notify();
        added
    }

    /// Uploads file `local_id`, whether it's still in the composer or already on its way in a message.
    fn start_upload(
        &mut self,
        local_id: u64,
        path: std::path::PathBuf,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let uploaded = match self.fixture() {
            #[cfg(feature = "dev")]
            true => send_fixture::upload(path.clone(), name.clone(), cx),
            _ => {
                let backend = backend(cx);
                let (path, name) = (path.clone(), name.clone());
                let prepared = cx.background_spawn(async move { attachments::prepare_attachment(&path, &name) });
                cx.spawn(async move |_, _| {
                    let (name, content_type, size, prepared) = prepared.await.map_err(|error| error.to_string())?;
                    let key = attachments::upload_prepared(backend, name.clone(), content_type.clone(), prepared)
                        .await
                        .map_err(|_| attachments::UPLOAD_FAILED.to_string())?;
                    Ok(Uploaded { name, content_type, size, key })
                })
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = uploaded.await;
            this.update_in(cx, |this, window, cx| this.upload_finished(local_id, path, name, result, window, cx)).ok();
        })
        .detach();
    }

    fn upload_finished(
        &mut self,
        local_id: u64,
        path: std::path::PathBuf,
        name: String,
        result: Result<Uploaded, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let in_message =
            self.pending.as_ref().is_some_and(|pending| pending.attachments.iter().any(|a| a.local_id == local_id));
        match result {
            Ok(uploaded) => {
                let sending = self.pending.iter_mut().flat_map(|pending| pending.attachments.iter_mut());
                if let Some(attachment) = self.attachments.iter_mut().chain(sending).find(|a| a.local_id == local_id) {
                    attachment.name = uploaded.name;
                    attachment.content_type = uploaded.content_type;
                    attachment.size = uploaded.size;
                    attachment.storage_id = Some(uploaded.key);
                }
                if in_message {
                    self.rebuild(cx);
                    self.deliver_pending(cx);
                }
            }
            Err(message) if in_message => {
                if let Some(pending) = &mut self.pending {
                    pending.failed_uploads.push((local_id, path, name));
                    pending.error = Some(message);
                }
                self.sending = false;
                self.rebuild(cx);
            }
            Err(message) => {
                self.remove_attachment(local_id, window, cx);
                self.hint = Some(message);
            }
        }
        cx.notify();
    }

    // --- Image editor ---

    /// Opens a picture to draw on: one from the chat goes back as a new message, one in the composer is replaced.
    pub fn edit_picture(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        self.close_editor(window, cx);
        let editor = cx.new(|cx| ImageEditor::new(source, window, cx));
        let subscription =
            cx.subscribe_in(&editor, window, |this, editor, event: &ImageEditorEvent, window, cx| match event {
                ImageEditorEvent::Close => this.close_editor(window, cx),
                ImageEditorEvent::Send { attachment, prompt } => {
                    if this.send_picture(attachment.clone(), prompt.clone(), window, cx) {
                        this.close_editor(window, cx);
                    } else {
                        editor.update(cx, |editor, cx| {
                            editor.failed("Wait for the last message to go, then send again.", cx)
                        });
                    }
                }
                ImageEditorEvent::Replace { local_id, path } => {
                    this.replace_attachment(*local_id, path.clone(), window, cx);
                    this.close_editor(window, cx);
                }
            });
        // Over the whole window, like Telegram's editor: the dialog layer, sized to the window, with nothing of the
        // dialog's own showing. The editor takes its own keys (Esc, ⌘Z), so the dialog's are off.
        let shown = editor.clone();
        window.open_dialog(cx, move |dialog, window, _| {
            let size = window.viewport_size();
            dialog
                .w(size.width)
                .margin_top(px(-crate::media_viewer::DIALOG_SLIDE))
                .close_button(false)
                .keyboard(false)
                .overlay_closable(false)
                .p_0()
                .border_0()
                .rounded(px(0.))
                .bg(gpui::transparent_black())
                .child(div().w(size.width).h(size.height).relative().child(shown.clone()))
        });
        // Opening the dialog took the focus; the prompt or the picture has it back.
        editor.update(cx, |editor, cx| editor.focus_first(window, cx));
        self.image_editor = Some((editor, subscription));
        cx.notify();
    }

    /// The open image editor (smoke test).
    #[cfg(feature = "dev")]
    pub fn image_editor(&self) -> Option<Entity<ImageEditor>> {
        self.image_editor.as_ref().map(|(editor, _)| editor.clone())
    }

    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((editor, _)) = self.image_editor.take() else { return };
        editor.read(cx).release(window);
        window.close_dialog(cx);
        if self.group.is_none() {
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// Sends an uploaded picture with its prompt as its own message; what the composer held stays there. False
    /// when a message is still on its way.
    fn send_picture(
        &mut self,
        mut attachment: PendingAttachment,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.sending || self.pending.is_some() {
            return false;
        }
        attachment.local_id = self.next_local_id;
        self.next_local_id += 1;
        let draft = self.input.read(cx).value().to_string();
        let draft_chips = self.input.read(cx).atoms().to_vec();
        let draft_attachments = std::mem::replace(&mut self.attachments, vec![attachment]);
        let draft_reply = self.reply_to.take();
        self.input.update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.send(window, cx);
        let sent = self.pending.is_some();
        if !sent {
            self.attachments.clear();
        }
        self.attachments.extend(draft_attachments);
        self.reply_to = draft_reply;
        self.input.update(cx, |input, cx| {
            input.set_value(draft, window, cx);
            input.set_atoms(draft_chips, cx);
        });
        self.hint = None;
        sent
    }

    /// The composer's picture `local_id` becomes the marked-up file, in the same place.
    fn replace_attachment(
        &mut self,
        local_id: u64,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.attachments.iter().position(|attachment| attachment.local_id == local_id) else {
            return;
        };
        let old = self.attachments.remove(index);
        let added = self.add_files(vec![path], window, cx);
        let Some((new_id, new_path)) = added.into_iter().next() else {
            self.remove_attachment(local_id, window, cx);
            return;
        };
        let mut file = self.attachments.pop().expect("just added");
        // The marked-up copy stands for the user's file: the message still points at where they keep it.
        file.local_path = old.local_path;
        self.attachments.insert(index, file);
        // Its chip stays where it was, now for the new file.
        if self.chips.contains(&local_id) {
            self.chips.retain(|id| *id != local_id);
            self.chips.push(new_id);
            let chip = self.input.update(cx, |input, _| {
                let atom = input.atom_mut(local_id)?;
                atom.id = new_id;
                let range = atom.range.clone();
                input.value().get(range).map(str::to_string)
            });
            // Copied and pasted, the chip brings the marked-up picture.
            if let Some(chip) = chip {
                attachments::remember_chip(&chip, &new_path, cx);
            }
            self.load_chip_picture(new_id, new_path, cx);
        }
    }
}

impl Focusable for Conversation {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// --- Rendering ---

fn bubble_radius(user: bool, joins_prev: bool, joins_next: bool) -> [f32; 4] {
    // [top-left, top-right, bottom-right, bottom-left]
    let (big, small) = (16., 5.);
    if user {
        [big, if joins_prev { small } else { big }, if joins_next { small } else { big }, big]
    } else {
        [if joins_prev { small } else { big }, big, big, if joins_next { small } else { big }]
    }
}

impl Conversation {
    /// A row's column: a set width, the list's own (`ui::page_column_in`), not a full one capped and centred by a flex
    /// row. That flex row had the column measured at its content's own width too, where a bubble's 86% cap doesn't apply
    /// and its text lies on one line. When that came out exactly as wide as the column (a reply's line, the bubble's
    /// padding and its actions: "✅ **Fable** is public as …" at 16 px), Taffy kept the measure's one-line height for the
    /// bubble's line, and the bubble, set at the line's bottom, rose over the message above (`FILO_OPEN=chat-overlap`).
    fn row_shell(&self, content: impl IntoElement) -> gpui::Div {
        let list = if self.width > 0. { self.width - timeline::gutter(self.width) } else { ui::content_width() };
        div().w_full().child(ui::page_column_in(list).child(content))
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        let colors = *cx.colors();
        let glance_row = matches!(row, ViewRow::Message { glance: Some(_), .. });
        let element: AnyElement = match row {
            ViewRow::Older => {
                div().py(px(space::MD)).flex().justify_center().child(ui::spinner(18., cx)).into_any_element()
            }
            ViewRow::Gap(_) => div().h(px(space::LG)).into_any_element(),
            ViewRow::Unseen => seen::render_divider(cx),
            ViewRow::Mark(label) => div()
                .py(px(space::SM))
                .flex()
                .justify_center()
                .child(
                    div()
                        .px(px(space::MD))
                        .py(px(space::XXS))
                        .rounded(px(radius::FULL))
                        .bg(colors.surface)
                        .caption()
                        .text_color(colors.text_secondary)
                        .child(label),
                )
                .into_any_element(),
            ViewRow::Message { message, glance: Some(look), joins_prev, .. } => {
                self.render_glance(&message, &look, joins_prev, window, cx)
            }
            ViewRow::Message {
                message,
                joins_prev,
                joins_next,
                late,
                stopped,
                failed,
                sending,
                glance: None,
                proof_task,
                reply,
            } => self.render_message(
                &message,
                joins_prev,
                joins_next,
                late,
                stopped,
                failed,
                sending,
                proof_task.as_ref(),
                reply.as_ref(),
                window,
                cx,
            ),
            ViewRow::GroupLink { id, link, quote } => self.render_group_link(&id, &link, &quote, cx),
            ViewRow::GroupCall { id, call, expanded, failed } => {
                self.render_group_call(&id, &call, expanded, failed.as_ref(), cx)
            }
            ViewRow::Routine { id, mark } => render_routine(&id, &mark, cx),
            ViewRow::Task(task) => self.render_task_card(&task, cx),
            ViewRow::OrphanFailed(turn) => self.render_failed(&turn, false, cx),
            ViewRow::CodingSessions(sessions) => coding_sessions::render_rows(&sessions, cx),
            ViewRow::CodingAsk(requests) => {
                // The panel above the composer answers them: a click gives it the keyboard.
                let view = cx.entity().downgrade();
                let on_click = move |_: &mut Window, cx: &mut App| {
                    view.update(cx, |this, cx| {
                        this.focus_panel = true;
                        cx.notify();
                    })
                    .ok();
                };
                gap_above(coding_requests::render_waiting(&requests, on_click, cx), space::SM)
            }
            ViewRow::CodingAnswered { request, sent, expanded } => {
                let view = cx.entity().downgrade();
                let key = request.request_id.clone();
                let on_toggle = move |_: &mut Window, cx: &mut App| {
                    view.update(cx, |this, cx| {
                        if !this.expanded_asks.remove(&key) {
                            this.expanded_asks.insert(key.clone());
                        }
                        this.data_changed(cx);
                    })
                    .ok();
                };
                // As wide as Filo's longest bubbles, in pixels (wrapped text in a list row measures right then).
                let column = if self.width > 0. {
                    ui::column_width_in(self.width - timeline::gutter(self.width))
                } else {
                    ui::column_width()
                };
                let card = coding_requests::render_answered(
                    &request,
                    sent.as_deref(),
                    &self.agent_name,
                    expanded,
                    (column * 0.86).floor(),
                    on_toggle,
                    window,
                    cx,
                );
                gap_above(card, space::SM)
            }
            ViewRow::Routing(engine) => {
                crate::smoke::tracked("routing-line", render_routing(engine, cx)).into_any_element()
            }
            ViewRow::Thinking(activity, None) => render_thinking(&self.agent_name, activity.as_deref(), cx),
            // In the group, a member at work in its own chat: the row opens that chat.
            ViewRow::Thinking(activity, Some(speaker)) => {
                let name = self
                    .group
                    .as_ref()
                    .and_then(|group| group.get(&speaker))
                    .map_or_else(|| self.agent_name.clone(), |(name, _, _)| name.clone());
                let view = cx.entity().downgrade();
                let row = div()
                    .id("thinking-open-chat")
                    .cursor_pointer()
                    .tooltip({
                        let label = SharedString::from(format!("Open {name}\u{2019}s chat"));
                        move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                    })
                    .on_click(move |_, _, cx| {
                        view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenSpeaker(speaker.clone()))).ok();
                    })
                    .child(render_thinking(&name, activity.as_deref(), cx));
                crate::smoke::tracked("thinking-open-chat", row).into_any_element()
            }
            ViewRow::Spacer(height) => div().h(px(height as f32)).into_any_element(),
        };
        // A glance places itself across the whole list (glance.rs `card_place`); everything else is in the column.
        let shell =
            |element: AnyElement| if glance_row { div().w_full().child(element) } else { self.row_shell(element) };
        match &self.held {
            Some(held) if self.row_sigs.get(ix).is_some_and(|row| row.key == held.pin.key) => div()
                .relative()
                .child(shell(element))
                .child(feed_rows::hold(&held.pin, held.viewport))
                .into_any_element(),
            _ => shell(element).into_any_element(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_message(
        &mut self,
        message: &Message,
        joins_prev: bool,
        joins_next: bool,
        late: bool,
        stopped: Option<bool>,
        failed: Option<ChatTurn>,
        sending: Option<Sending>,
        proof_task: Option<&ProofTask>,
        reply: Option<&ReplyLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let user = message.is_user();
        let id = message.id.clone();
        // Its elements' ids: a sent message keeps the ones it had while it was sent.
        let key = self.handed_over.get(&id).cloned().unwrap_or_else(|| id.clone());
        // The user's pictures keep their chips in the text, where they were in the composer. A voice message's text is
        // what was said, shown under its recording.
        let (text, chips) = if message.voice_recording.is_some() {
            (message.own_content().trim().to_string(), Vec::new())
        } else if user {
            attachments::with_chips(message.own_content(), &message.attachments)
        } else {
            (message.content.clone(), Vec::new())
        };
        let [tl, tr, br, bl] = bubble_radius(user, joins_prev, joins_next);
        let media: Vec<&Attachment> = message
            .attachments
            .iter()
            .filter(|a| a.content_type.starts_with("image/") || a.content_type.starts_with("video/"))
            .collect();
        let pictures = copyable_pictures(&media);
        let files: Vec<&Attachment> = message
            .attachments
            .iter()
            .filter(|a| !(a.content_type.starts_with("image/") || a.content_type.starts_with("video/")))
            .collect();
        let proof = proof_route(message, proof_task);
        let report = self.post_report(message);
        // Pictures and videos sit above the bubble, on their own; the bubble holds the rest.
        let has_content = !text.trim().is_empty() || message.voice_recording.is_some() || !files.is_empty();
        let has_bubble = has_content || proof.is_some() || report.is_some();
        let group = SharedString::from(format!("msg-{key}"));
        let highlighted = self.highlighted.as_deref() == Some(id.as_str());

        let mut bubble = div()
            .max_w(relative(0.86))
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .rounded_tl(px(tl))
            .rounded_tr(px(tr))
            .rounded_br(px(br))
            .rounded_bl(px(bl));
        bubble = if user {
            bubble.bg(colors.user_bubble).text_color(colors.on_user_bubble)
        } else {
            bubble
                .bg(colors.assistant_bubble)
                .border_1()
                .border_color(colors.border)
                .text_color(colors.on_assistant_bubble)
        };
        if highlighted {
            bubble = bubble.border_2().border_color(colors.accent);
        }
        bubble = bubble.px(px(12.)).py(px(7.));

        let media = (!media.is_empty()).then(|| {
            // In the agents' group a picture opens to look at, without Draw on it.
            let editor = self.group.is_none().then(|| cx.entity().downgrade());
            render_media(&key, &media, editor, window, cx)
                .when(self.fixture(), |this| this.relative().children(probe(&self.drawn, format!("media-{key}"))))
                .when(highlighted && !has_bubble, |this| this.rounded(px(18.)).border_2().border_color(colors.accent))
        });
        let (media_line, media_only) =
            if has_bubble { (media.map(|media| media.mb(px(space::XS))), None) } else { (None, media) };
        for (index, file) in files.iter().enumerate() {
            bubble = bubble.child(render_file(&key, index, file, user, window, cx));
        }
        if let Some(voice) = message.voice_recording.as_ref().filter(|voice| voice.gone) {
            bubble = bubble.child(gone_voice(voice.duration_ms, user, cx));
        } else if let Some(voice) = &message.voice_recording {
            let url = voice.url.clone().or_else(|| voice::uploading_voice_url(&id));
            bubble = bubble.child(render_voice(&id, url, voice.duration_ms, "Voice message", user, cx));
        }
        if !text.trim().is_empty() && message.voice_recording.is_some() {
            // Divided from the recording above it, in the bubble's own text colour.
            let fg = if user { colors.on_user_bubble } else { colors.on_assistant_bubble };
            bubble = bubble.child(
                div()
                    .pt(px(space::SM))
                    .border_t_1()
                    .border_color(fg.opacity(0.2))
                    .child(user_text(&key, &text, &[], &message.attachments, &self.mention_words(cx), None, cx))
                    .when(self.fixture(), |this| {
                        this.relative().children(probe(&self.drawn, format!("transcript-{key}")))
                    }),
            );
        } else if !text.trim().is_empty() {
            bubble = if user {
                let editor = self.group.is_none().then(|| cx.entity().downgrade());
                bubble.child(user_text(&key, &text, &chips, &message.attachments, &self.mention_words(cx), editor, cx))
            } else {
                // The agents' @names are `mention:` links that open them (`mention_markdown`), in the group and in an
                // agent's chat alike (what it posted in the group too).
                let members = self.mention_members();
                let mentions = (!members.is_empty()).then(|| mention_markdown(&text, &members, cx.entity_id()));
                // Commit hashes open on GitHub (commit_links.rs); selected words can be quoted in the composer.
                let view = cx.entity().downgrade();
                let drawn = self.fixture().then(|| self.drawn.clone());
                let style = ui::markdown_style().commit_links(true);
                let style = if mentions.is_some() { style.mention_links(colors.accent) } else { style };
                let markdown = TextView::markdown(
                    SharedString::from(format!("md-{id}")),
                    mentions.unwrap_or_else(|| text.clone()),
                    window,
                    cx,
                )
                .style(style)
                .selectable(true)
                .selection_actions(move |selection, _, _| {
                    let view = view.clone();
                    // Quotes as the button goes down, not on the click: the press reaches it, but in the real chat
                    // the release didn't always come back to it, so the click never happened and Quote stayed
                    // pressed over the text.
                    div()
                        .relative()
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            let text = selection.text(cx);
                            selection.clear(cx);
                            view.update(cx, |this, cx| this.quote(&text, window, cx)).ok();
                        })
                        .child(Button::secondary("quote-selection", "Quote").small().icon("reply"))
                        .when_some(drawn.as_ref(), |this, drawn| this.children(probe(drawn, "quote-selection".into())))
                });
                bubble.child(div().callout().child(markdown))
            };
        }
        if let Some(route) = proof {
            // Divided only from something above it: a proof alone in its bubble is just the link.
            bubble = bubble.child(proof_footer(&id, route, proof_task, has_content, cx));
        } else if let Some(report) = report {
            // An agent's full report under its short post: the same row, opening it as a page. A post about a coding
            // round has that round's row alone.
            let title = crate::screens::coding_result::report_title(&report.report);
            bubble = bubble.child(report_footer(&id, title, Route::PostReport(report), has_content, cx));
        }
        if self.fixture() {
            bubble = bubble.relative().children(probe(&self.drawn, format!("bubble-{key}")));
        }

        let picker_open = self.picker_for.as_deref() == Some(id.as_str());
        let current_reaction = message.reaction.clone();
        let picker = picker_open.then(|| {
            let message = message.clone();
            div()
                .id(SharedString::from(format!("picker-{id}")))
                .mt(px(space::XS))
                .flex()
                .gap(px(2.))
                .p(px(space::XS))
                .rounded(px(radius::FULL))
                .bg(colors.raised)
                .border_1()
                .border_color(colors.border)
                .shadow_md()
                .relative()
                // The scroll wheel over it doesn't close it (it's one menu of escape.rs's popups).
                .child(gpui_component::one_menu::surface())
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.picker_closed = this.picker_for.take().map(|id| (id, std::time::Instant::now()));
                    cx.notify();
                }))
                .children(REACTIONS.iter().map(|(emoji, label)| {
                    let selected = current_reaction.as_deref() == Some(*emoji);
                    let message = message.clone();
                    let emoji = emoji.to_string();
                    div()
                        .id(SharedString::from(format!("emoji-{}-{label}", message.id)))
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::FULL))
                        .text_size(px(18.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(colors.selected_fill))
                        .hover(|style| style.bg(colors.selected_fill))
                        .tooltip({
                            let label = SharedString::from(label.to_string());
                            move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                        })
                        .child(emoji.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.react(&message, &emoji, cx)))
                }))
        });

        // The picker opens under the react button, where it was asked for (deferred paints it above later rows); it
        // never takes layout space, so nothing shifts.
        let picker = picker.map(|picker| {
            div().absolute().left_0().bottom_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Corner::TopLeft).snap_to_window_with_margin(px(8.)).child(picker),
                )
                .with_priority(1),
            )
        });
        let actions = {
            let (reply_message, copy_message, react_id) = (message.clone(), message.clone(), id.clone());
            let copy_text = Self::copy_text(message);
            let pictures = pictures.clone();
            div()
                .flex()
                .gap(px(2.))
                .opacity(if picker_open { 1. } else { 0. })
                .group_hover(group.clone(), |style| style.opacity(1.))
                .when(!user, |this| {
                    this.child(
                        div()
                            .relative()
                            .child(
                                IconButton::new(SharedString::from(format!("react-{id}")), "smile-plus")
                                    .small()
                                    .tooltip("React")
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        // The press already closed this message's picker: the click doesn't reopen it.
                                        let just_closed = this
                                            .picker_closed
                                            .take()
                                            .is_some_and(|(id, at)| id == react_id && at.elapsed().as_millis() < 500);
                                        this.picker_for =
                                            if just_closed || this.picker_for.as_deref() == Some(react_id.as_str()) {
                                                None
                                            } else {
                                                Some(react_id.clone())
                                            };
                                        cx.notify();
                                    })),
                            )
                            .children(picker),
                    )
                })
                .child(
                    IconButton::new(SharedString::from(format!("reply-{id}")), "reply")
                        .small()
                        .tooltip("Reply")
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| this.reply(&reply_message, window, cx)),
                        ),
                )
                .when(!copy_text.is_empty(), |this| {
                    this.child(
                        IconButton::new(SharedString::from(format!("copy-{id}")), "copy")
                            .small()
                            .tooltip("Copy")
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(Self::copy_text(&copy_message)));
                                ui::toast(window, cx, "Copied");
                            }),
                    )
                })
                // A message with several pictures copies them all at once (one picture: right-click it).
                .when(pictures.len() > 1, |this| {
                    this.child(
                        IconButton::new(SharedString::from(format!("copy-images-{id}")), "image")
                            .small()
                            .tooltip("Copy images")
                            .on_click(move |_, window, cx| {
                                crate::media_viewer::copy_pictures_and_tell(pictures.clone(), window, cx)
                            }),
                    )
                })
        };

        let reaction_chip = message.reaction.clone().map(|reaction| {
            let message = message.clone();
            // Over the bubble's bottom-left edge. It's absolute; the row leaves room under it (below),
            // since the next row paints over whatever reaches into it.
            div()
                .id(SharedString::from(format!("reaction-{id}")))
                .flex_none()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::FULL))
                .bg(colors.raised)
                .border_1()
                .border_color(colors.border)
                .text_size(px(11.))
                .cursor_pointer()
                .child(reaction)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    // Only the reaction: not a picture or bubble under it as well.
                    cx.stop_propagation();
                    this.picker_for = Some(message.id.clone());
                    cx.notify();
                }))
        });

        // Not deferred: deferred elements escape the list's clipping and would draw over
        // the composer. Painted after the bubble, it sits on top of its own bubble.
        let reaction_chip =
            reaction_chip.map(|chip| div().absolute().left(px(space::SM)).bottom(px(-REACTION_HANG)).child(chip));
        let bubble_line = div()
            .relative()
            .w_full()
            .flex()
            .items_end()
            .gap(px(space::XS))
            .when(user, |this| this.flex_row_reverse())
            // Pictures alone take the bubble's place, beside the actions; above a bubble they get their own line.
            .map(|this| match (has_bubble, media_only) {
                (true, _) => this.child(bubble),
                (false, Some(media)) => this.child(media),
                (false, None) => this,
            })
            .children(reaction_chip)
            // Not there for a message still being sent (nothing to reply to yet), but keeping their room, so the
            // bubble wraps the same once it's sent.
            .child(actions.when(sending.is_some(), |actions| actions.invisible()));
        // In the group, a run of one speaker's messages opens with their picture and name.
        let speaker = (!user && !joins_prev)
            .then(|| {
                self.group
                    .as_ref()
                    .zip(message.speaker.as_ref())
                    .and_then(|(speakers, speaker)| speakers.get(speaker).cloned())
            })
            .flatten()
            .map(|(name, picture, background)| {
                // An agent given a colour shows on a disc of it, as in the Agents list.
                let face = match background {
                    Some(color) => crate::screens::agents::picture_on(picture, Some(color), 22., cx).into_any_element(),
                    None => crate::avatars::picture(picture, 22.).into_any_element(),
                };
                div()
                    .mb(px(space::XS))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(face)
                    .child(div().footnote().semibold().text_color(colors.text_secondary).child(name))
            });

        // Over the bubble (or the pictures above it): the message it answers, when someone else wrote after it.
        let reply_link = reply.map(|link| {
            let target = link.target.clone();
            reply_link::render(format!("reply-link-{key}"), link, user, cx)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.jump_to(&target, cx)))
                .when(self.fixture(), |this| this.relative().children(probe(&self.drawn, format!("reply-link-{key}"))))
        });
        // What an agent posted in the group, in its own chat: a label over it opening the group at the post.
        let posted = message.posted_in_group().map(|link| {
            let target = link.message_id.clone();
            let view = cx.entity().downgrade();
            let label = div()
                .id(SharedString::from(format!("posted-{key}")))
                .mb(px(space::XS))
                .flex()
                .items_center()
                .gap(px(space::XS))
                .cursor_pointer()
                .caption()
                .semibold()
                .text_color(colors.link)
                .hover(|style| style.opacity(0.8))
                .child(ui::icon_sized("message-circle", 13., colors.link))
                .child(link.label())
                .on_click(move |_, _, cx| {
                    view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenGroup(target.clone()))).ok();
                });
            crate::smoke::tracked(format!("posted-{key}"), label)
        });
        // Another agent asked it this from its chat (the group off): a chip over it opening that chat.
        let asked = message.asked_by.as_ref().map(|asked| {
            let (agent, target) = (asked.speaker.clone(), asked.message_id.clone());
            let view = cx.entity().downgrade();
            let chip = div()
                .id(SharedString::from(format!("asked-by-{key}")))
                .mb(px(space::XS))
                .flex()
                .items_center()
                .gap(px(space::XS))
                .px(px(space::SM))
                .py(px(2.))
                .rounded(px(radius::FULL))
                .bg(colors.surface)
                .cursor_pointer()
                .hover(|style| style.bg(colors.selected_fill))
                .children(self.face(&asked.speaker, 18., cx))
                .child(div().caption().semibold().text_color(colors.text).child(asked.name.clone()))
                .child(ui::icon_sized("chevron-right", 12., colors.text_tertiary))
                .on_click(move |_, _, cx| {
                    view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenAgent(agent.clone(), target.clone()))).ok();
                });
            crate::smoke::tracked(format!("asked-by-{key}"), chip)
        });
        let time = late.then(|| {
            div()
                .mt(px(space::XXS))
                .caption()
                .text_color(colors.text_tertiary)
                .child(format::clock(message.creation_time))
        });
        let stopped_label =
            stopped.map(|_| div().mt(px(space::XXS)).caption().text_color(colors.text_tertiary).child("Stopped"));
        let failed_row = failed.map(|turn| self.render_failed(&turn, true, cx));
        let unsent_row = sending.and_then(|sending| sending.error).map(|error| {
            div()
                .max_w_full()
                .mt(px(space::XS))
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(div().min_w_0().footnote().text_color(colors.danger).child(error))
                .child(
                    Button::ghost("pending-retry", "Retry")
                        .small()
                        .loading(self.sending)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.retry_pending(window, cx))),
                )
        });

        div()
            .id(SharedString::from(format!("row-{id}")))
            .group(group)
            .w_full()
            .pt(px(if joins_prev { space::XS } else { space::MD }))
            .flex()
            .flex_col()
            .when(user, |this| this.items_end())
            .when(!user, |this| this.items_start())
            .children(speaker)
            .children(posted)
            .children(asked)
            .children(reply_link)
            .children(media_line)
            .child(bubble_line)
            .children(time)
            .children(stopped_label)
            .children(failed_row)
            .children(unsent_row)
            // Room for the reaction hanging under the bubble, and a gap after it: the next row would cover it.
            .when(message.reaction.is_some(), |this| this.pb(px(REACTION_HANG + space::XS)))
            .into_any_element()
    }

    /// A line in an agent's chat pointing at the group: "Called by Context Engineer" or "Asked Filo", and what was
    /// asked, in one line (what it posted there is a bubble of its own, `render_message`). It opens the group at that message.
    fn render_group_link(&self, id: &str, link: &GroupLink, quote: &str, cx: &mut Context<Self>) -> AnyElement {
        if let Some(agent) = &link.agent {
            return self.render_agent_link(id, link, agent, cx);
        }
        let colors = *cx.colors();
        let target = link.message_id.clone();
        let view = cx.entity().downgrade();
        let quote = feed::strip_markdown_for_quote(quote);
        let chip = div()
            .id(SharedString::from(format!("group-link-{id}")))
            .max_w(relative(0.86))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .px(px(space::MD))
            .py(px(space::XXS))
            .rounded(px(radius::FULL))
            .bg(colors.surface)
            .cursor_pointer()
            .hover(|style| style.bg(colors.accent_soft))
            .child(ui::icon_sized("message-circle", 13., colors.link))
            .child(div().flex_none().caption().semibold().text_color(colors.link).child(link.label()))
            .when(!quote.is_empty(), |chip| {
                chip.child(div().min_w_0().caption().text_color(colors.text_secondary).truncate().child(quote))
            })
            .on_click(move |_, _, cx| {
                view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenGroup(target.clone()))).ok();
            });
        let chip = crate::smoke::tracked(format!("group-link-{id}"), chip);
        div().py(px(space::SM)).flex().justify_center().child(chip).into_any_element()
    }

    /// While the group is off, where this agent asked another ("Asked Developer") or that one's answer came back
    /// ("Developer answered"): a chip under the agent's message with the other's face and the ask's state, opening
    /// the other's chat at the ask or the answer.
    fn render_agent_link(&self, id: &str, link: &GroupLink, agent: &str, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let name = link.who.first().map_or_else(|| "An agent".to_string(), |who| who.name.clone());
        let words = match link.kind.as_str() {
            "answered" => div()
                .flex()
                .items_center()
                .gap(px(3.))
                .child(div().semibold().text_color(colors.text).child(name))
                .child("answered"),
            _ => div()
                .flex()
                .items_center()
                .gap(px(3.))
                .child("Asked")
                .child(div().semibold().text_color(colors.text).child(name)),
        };
        let pill = match (link.kind.as_str(), link.state.as_deref()) {
            ("asked", Some("working")) => Some(crate::screens::agent_work::state_pill("active", "Working", true, cx)),
            ("asked", Some("done")) => Some(crate::screens::agent_work::state_pill(
                "completed",
                crate::screens::coding::with_worked("Done", "done", link.worked_ms),
                true,
                cx,
            )),
            (_, Some("failed")) => Some(crate::screens::agent_work::state_pill("failed", "Couldn’t answer", true, cx)),
            _ => None,
        };
        let (agent, target) = (agent.to_string(), link.message_id.clone());
        let view = cx.entity().downgrade();
        let chip = div()
            .id(SharedString::from(format!("agent-link-{id}")))
            .max_w(relative(0.86))
            .flex()
            .items_center()
            .gap(px(space::XS + 2.))
            .pl(px(space::XS))
            .pr(px(space::SM))
            .py(px(3.))
            .rounded(px(radius::FULL))
            .bg(colors.surface)
            .cursor_pointer()
            .hover(|style| style.bg(colors.selected_fill))
            .children(self.face(&agent, 18., cx))
            .child(words.caption().text_color(colors.text_secondary))
            .children(pill)
            .child(ui::icon_sized("chevron-right", 12., colors.text_tertiary))
            .on_click(move |_, _, cx| {
                view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenAgent(agent.clone(), target.clone()))).ok();
            });
        let chip = crate::smoke::tracked(format!("agent-link-{id}"), chip);
        div().pt(px(space::XS)).flex().child(chip).into_any_element()
    }

    /// The group's ask in an agent's chat, where its turn for the group started: who asked, opening the group at that
    /// message, and what was asked, cut to `GROUP_CALL_LINES` lines with "Show more" when it runs longer. The group's
    /// messages the agent read before it aren't shown here. A turn that failed has what went wrong under the card, with
    /// Retry (the group runs it again) and Report.
    fn render_group_call(
        &self,
        id: &str,
        call: &GroupCall,
        expanded: bool,
        failed: Option<&ChatTurn>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::screens::agent_work::max_lines;
        use crate::theme::TypeVariant;
        let colors = *cx.colors();
        let column = if self.width > 0. {
            ui::column_width_in(self.width - timeline::gutter(self.width))
        } else {
            ui::column_width()
        };
        let card_width = (column * 0.86).floor();
        let target = call.message_id.clone();
        let view = cx.entity().downgrade();
        let head = div()
            .id(SharedString::from(format!("group-call-{id}")))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .cursor_pointer()
            .child(ui::icon_sized("message-circle", 13., colors.link))
            .child(div().min_w_0().caption().semibold().text_color(colors.link).truncate().child(call.label()))
            .on_click(move |_, _, cx| {
                view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenGroup(target.clone()))).ok();
            });
        let head = crate::smoke::tracked(format!("group-call-{id}"), head);
        let ask = feed::strip_markdown_for_quote(&call.ask);
        // How many lines the ask wraps to in the card, as GPUI will break it.
        let long = !ask.is_empty() && {
            let font = gpui::font(cx.theme().font_family());
            let mut wrapper = cx.text_system().line_wrapper(font, px(TypeVariant::Footnote.size()));
            let breaks = wrapper
                .wrap_line(&[gpui::LineFragment::text(&ask)], px(card_width - space::MD * 2.))
                .take(GROUP_CALL_LINES)
                .count();
            breaks >= GROUP_CALL_LINES
        };
        let toggle = long.then(|| {
            let view = cx.entity().downgrade();
            let key = id.to_string();
            div()
                .id(SharedString::from(format!("group-call-more-{id}")))
                .flex()
                .items_center()
                .gap(px(space::XXS))
                .cursor_pointer()
                .caption()
                .text_color(colors.text_tertiary)
                .hover(|style| style.text_color(colors.text_secondary))
                .child(if expanded { "Show less" } else { "Show more" })
                .child(ui::icon_sized(if expanded { "chevron-up" } else { "chevron-down" }, 12., colors.text_tertiary))
                .on_click(move |event: &ClickEvent, _, cx| {
                    if !event.standard_click() {
                        return;
                    }
                    view.update(cx, |this, cx| {
                        if !this.open_group_calls.remove(&key) {
                            this.open_group_calls.insert(key.clone());
                        }
                        this.data_changed(cx);
                    })
                    .ok();
                })
        });
        let text = (!ask.is_empty()).then(|| {
            let text = div().footnote().text_color(colors.text).child(ask);
            if long && !expanded { max_lines(text, GROUP_CALL_LINES as f32, 17.) } else { text }
        });
        let card = div()
            .w(px(card_width))
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .px(px(space::MD))
            .py(px(space::SM))
            .rounded(px(radius::LG))
            .bg(colors.surface)
            .child(head)
            .children(text)
            .children(toggle);
        let failed_row = failed.map(|turn| self.render_failed(turn, true, cx));
        let column = div().w(px(card_width)).flex().flex_col().child(card).children(failed_row);
        div().pt(px(space::MD)).flex().justify_center().child(column).into_any_element()
    }

    /// Scrolls to a quoted message and flashes it.
    fn jump_to(&mut self, target: &str, cx: &mut Context<Self>) {
        let index = self.rows.iter().position(|row| row_message_id(row) == Some(target));
        let Some(index) = index else { return };
        self.list_state.scroll_to_reveal_item(index);
        // Not held where the reader was as the older page that brought it came in (`hold_pin`).
        self.held = None;
        self.held_at_end = false;
        self.highlighted = Some(target.to_string());
        let target = target.to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1400)).await;
            this.update(cx, |this, cx| {
                if this.highlighted.as_deref() == Some(target.as_str()) {
                    this.highlighted = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Each reply's link to the message it answers (reply_link.rs), when someone else wrote after it (`apart`). A reply
    /// to a message older than what's loaded links it with the quote saved with it, when it has one (the user's).
    fn link_replies(&self, rows: &mut [ViewRow]) {
        let has_more = self.detail().is_some_and(|detail| detail.has_more);
        for index in 0..rows.len() {
            let ViewRow::Message { message, .. } = &rows[index] else { continue };
            let Some(target) = message.reply_to_message_id.as_deref() else { continue };
            let link = match apart(rows, index, target, self.group.is_some()) {
                Some(apart) => {
                    let answered =
                        self.detail().and_then(|detail| detail.messages.iter().find(|answered| answered.id == target));
                    answered.filter(|_| apart).map(|answered| self.reply_link(message, answered))
                }
                None if has_more => message.reply_quote().map(|(quote, _)| ReplyLink {
                    target: target.to_string(),
                    author: None,
                    quote: feed::strip_markdown_for_quote(&quote),
                }),
                None => None,
            };
            if let ViewRow::Message { reply, .. } = &mut rows[index] {
                *reply = link;
            }
        }
    }

    /// The link of `reply` to the message it answers: its author's name, when it isn't obvious (in the group, or one's
    /// own message answered in a chat of two), and its words in one line.
    fn reply_link(&self, reply: &Message, answered: &Message) -> ReplyLink {
        let shown = (self.group.is_some() || answered.is_user() == reply.is_user())
            .then(|| if answered.is_user() { "You".to_string() } else { self.author(answered) });
        let words = if answered.voice_recording.is_some() {
            "Voice message".to_string()
        } else if let Some(routine) = &answered.routine {
            format!("Routine · {}", routine.title)
        } else {
            feed::strip_markdown_for_quote(&Self::shown_text(answered))
        };
        let quote = match words.trim() {
            "" => answered.attachments.first().map_or_else(|| "Message".to_string(), |file| file.name.clone()),
            words => words.to_string(),
        };
        ReplyLink { target: answered.id.clone(), author: shown, quote }
    }

    /// Who wrote a message that isn't the user's: in the group its member, else this chat's Filo or agent.
    fn author(&self, message: &Message) -> String {
        let member = self.group.as_ref().zip(message.speaker.as_ref()).and_then(|(group, speaker)| group.get(speaker));
        member.map_or_else(|| self.agent_name.clone(), |(name, _, _)| name.clone())
    }

    fn render_task_card(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let text = if task.summary.trim().is_empty() {
            task.artifact.as_ref().map(|artifact| artifact.title.clone()).unwrap_or_default()
        } else {
            feed::markdown_preview(&task.summary)
        };
        let route = task.artifact.as_ref().map(|artifact| Route::Artifact(artifact.id.clone()));
        gap_above(
            div()
                .w(relative(0.86))
                .p(px(space::LG))
                .rounded(px(radius::LG))
                .bg(colors.surface)
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .child(
                    div()
                        .footnote()
                        .semibold()
                        .text_color(colors.text_secondary)
                        .child(format!("About “{}”", task.title)),
                )
                .child(div().callout().line_clamp(6).child(text))
                .when_some(route, |this, route| {
                    this.child(
                        div().flex().child(
                            Button::secondary(SharedString::from(format!("task-open-{}", task.id)), "Open")
                                .small()
                                .on_click(move |_, window, cx| nav::push(route.clone(), window, cx)),
                        ),
                    )
                }),
            space::MD,
        )
    }

    fn render_failed(&self, turn: &ChatTurn, under_message: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        // A turn here went through since: what went wrong is a quiet line, with nothing to do about it.
        if turn.state == "settled" {
            return div()
                .max_w_full()
                .mt(px(space::XS))
                .footnote()
                .text_color(colors.text_tertiary)
                .when(!under_message, |this| this.text_right())
                .child(settled_words(turn))
                .into_any_element();
        }
        let message = failure_message(turn, self.retry_error.as_ref());
        // On a usage limit, when it's tried again on its own.
        let retry_line = turn.retry_at.map(|at| {
            div().footnote().text_color(colors.text_secondary).child(retry_words(at, crate::format::now_ms()))
        });
        let turn_id = turn.turn_id.clone();
        // Report a problem about this reply (screens/report.rs), with the error as the chat shows it.
        let report = crate::report::Origin::Turn { turn_id: turn.turn_id.clone(), error: turn.error.clone() };
        // No wider than the column, its text wrapping inside it: at the text's full width the row would run past the
        // column's left edge.
        div()
            .max_w_full()
            .mt(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .when(!under_message, |this| this.justify_end())
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .when(!under_message, |this| this.items_end())
                    .child(
                        div()
                            .footnote()
                            .text_color(colors.danger)
                            .when(!under_message, |this| this.text_right())
                            .child(message),
                    )
                    .children(retry_line),
            )
            .child(
                Button::ghost(SharedString::from(format!("retry-{}", turn.turn_id)), "Retry")
                    .small()
                    .loading(self.retrying.as_deref() == Some(turn.turn_id.as_str()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.retry_turn(turn_id.clone(), cx))),
            )
            .child(
                Button::ghost(SharedString::from(format!("report-{}", turn.turn_id)), "Report")
                    .small()
                    .on_click(move |_, window, cx| nav::push(Route::Report(report.clone()), window, cx)),
            )
            .into_any_element()
    }

    /// The questions panel above the composer, when questions wait (never in the group). New
    /// ones take the keyboard from an empty composer; answered, it hands the keyboard back. On a question the
    /// composer's placeholder says it answers it.
    fn sync_question_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (open, on_question, panel_focused) = {
            let panel = self.question_panel.read(cx);
            (self.group.is_none() && panel.is_open(), panel.on_question(), panel.has_focus(window))
        };
        let input_focus = self.input.read(cx).focus_handle(cx);
        if open && !self.panel_was_open {
            let elsewhere = window.focused(cx).is_some_and(|focused| focused != input_focus && focused != self.focus);
            if self.input.read(cx).value().is_empty() && !elsewhere {
                self.focus_panel = true;
            }
        }
        if !open && self.panel_was_open && panel_focused {
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        if std::mem::take(&mut self.focus_panel) && open {
            self.question_panel.read(cx).focus_handle().focus(window);
        }
        self.panel_was_open = open;
        let answering = open && on_question;
        if answering != self.answering {
            self.answering = answering;
            let placeholder = if answering { "Or type your own answer…" } else { self.placeholder };
            self.input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
        }
        open.then(|| self.question_panel.clone().into_any_element())
    }

    fn render_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let mention_list = self.render_mentions(cx);
        let mentioning = mention_list.is_some();
        let panel = self.sync_question_panel(window, cx);
        let back_to_questions = self.answering && !mentioning;
        // Mentions in the text in the accent colour, found again as it's edited.
        let (words, accent) = (self.mention_words(cx), colors.accent);
        self.input.update(cx, |input, _| {
            input.set_text_colors(Some(Rc::new(move |text: &str| {
                mention_suggestions::mention_ranges(text, &words).into_iter().map(|range| (range, accent)).collect()
            })))
        });
        let show_send = self.show_send_action(cx);
        let show_stop = self.show_stop(cx);
        let send_disabled = self.send_disabled(cx);
        let reply = self.reply_to.clone();
        let voice_row = self.render_voice_row(cx);
        let recording = voice_row.is_some();
        // An empty composer offers the mic.
        let action: AnyElement = if !show_send && !show_stop {
            self.mic_button(cx)
        } else if show_stop && !show_send {
            IconButton::new("composer-stop", "square")
                .primary()
                .small()
                .tooltip("Stop")
                .disabled(self.stopping)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
                .into_any_element()
        } else {
            IconButton::new("composer-send", "arrow-up")
                .primary()
                .small()
                .tooltip("Send")
                .disabled(send_disabled)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx)))
                .into_any_element()
        };
        // How hard Filo (or the agent) thinks in this chat: quiet, so it doesn't compete with Send. Its label and colours
        // are its own children, since the menu button's own label turns red under the pointer.
        let levels = self.reasoning_levels(cx);
        // None picked: "Default", what its turns get.
        let reasoning = self.reasoning().and_then(|level| reasoning_used(level, &levels));
        let reasoning_picker = levels.iter().any(|level| reasoning_level(level).is_some()).then(|| {
            let entity = cx.entity().downgrade();
            let trigger = MenuButton::new("composer-reasoning")
                .ghost()
                .small()
                .compact()
                .h(px(28.))
                .rounded(ButtonRounded::Size(px(radius::FULL)))
                .tooltip(self.reasoning_tip.clone());
            reasoning_picker(
                trigger,
                Corner::BottomRight,
                levels,
                reasoning,
                colors.text_secondary,
                move |level, cx| {
                    entity.update(cx, |this, cx| this.set_reasoning(level, cx)).ok();
                },
            )
        });
        // Which model answers in this chat, by its provider's logo and its name, quiet like the reasoning picker beside it.
        let model = self.model_choice();
        let model_rows = model_rows(cx);
        let model_picker = {
            let entity = cx.entity().downgrade();
            let button = MenuButton::new("composer-model")
                .ghost()
                .small()
                .compact()
                .h(px(28.))
                .rounded(ButtonRounded::Size(px(radius::FULL)))
                .tooltip("Model for this chat")
                .child(ui::icon_sized(engine_logo(model.0), 12., colors.text_secondary))
                .children(
                    model_label(&model, &all_model_rows(cx))
                        .map(|label| div().footnote().text_color(colors.text_secondary).child(label)),
                )
                .child(ui::icon_sized("chevron-down", 12., colors.text_secondary));
            model_picker(
                "composer-model-menu",
                button,
                Corner::BottomRight,
                model_rows.clone(),
                &model,
                move |choice, cx| {
                    entity.update(cx, |this, cx| this.set_model(choice, cx)).ok();
                },
            )
        };
        // Glued to the top of the input: an accent bar with who and
        // what you're replying to, and an X.
        let reply_strip = reply.map(|reply| {
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .pl(px(space::MD))
                .pr(px(space::XS))
                .pt(px(space::SM + 2.))
                .pb(px(space::XXS))
                .child(ui::icon_sized("reply", 16., colors.accent))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(space::SM))
                        .border_l(px(2.))
                        .border_color(colors.accent)
                        .flex()
                        .flex_col()
                        .child(div().caption().semibold().text_color(colors.accent).child(if reply.is_user {
                            "Reply to your message".to_string()
                        } else {
                            format!("Reply to {}", reply.author)
                        }))
                        .child(
                            div()
                                .footnote()
                                .text_color(colors.text_secondary)
                                .truncate()
                                .child(feed::strip_markdown_for_quote(&reply.content)),
                        ),
                )
                .child(IconButton::new("cancel-reply", "x").small().tooltip("Cancel reply").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.reply_to = None;
                        cx.notify();
                    },
                )))
        });
        // Notes shared from the Notes tab and running tasks the message points at: a chip each, with an × to take it off
        // before sending.
        let work_chips: Vec<AnyElement> = self
            .shared_work
            .iter()
            .map(|task| {
                let key = task.key();
                work_share::chip(task, cx)
                    .id(SharedString::from(format!("shared-work-{key}")))
                    .child(crate::smoke::tracked(
                        format!("remove-shared-work-{key}"),
                        IconButton::new(SharedString::from(format!("remove-shared-work-{key}")), "x")
                            .small()
                            .tooltip("Remove task")
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.remove_shared_work(&key, cx)),
                            ),
                    ))
                    .into_any_element()
            })
            .collect();
        let note_strip = (!self.shared_notes.is_empty() || !work_chips.is_empty()).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap(px(space::XS))
                .px(px(space::SM))
                .pt(px(space::SM))
                .children(work_chips)
                .children(self.shared_notes.iter().map(|note| {
                    let key = note.key.clone();
                    div()
                        .id(SharedString::from(format!("shared-note-{key}")))
                        .max_w(px(280.))
                        .h(px(30.))
                        .pl(px(space::SM))
                        .pr(px(space::XXS))
                        .flex()
                        .items_center()
                        .gap(px(space::XS))
                        .rounded(px(radius::FULL))
                        .bg(colors.surface_strong)
                        .border_1()
                        .border_color(colors.border)
                        .child(ui::icon_sized("notebook-pen", 14., colors.text_secondary))
                        .child(div().min_w_0().footnote().truncate().child(note.label()))
                        .child(crate::smoke::tracked(
                            format!("remove-shared-note-{key}"),
                            IconButton::new(SharedString::from(format!("remove-shared-note-{key}")), "x")
                                .small()
                                .tooltip("Remove note")
                                .on_click(
                                    cx.listener(move |this, _: &ClickEvent, _, cx| this.remove_shared_note(&key, cx)),
                                ),
                        ))
                }))
        });
        if reply_strip.is_none() && note_strip.is_none() && self.attachments.is_empty() {
            self.strips_height.set(0.);
        }
        let attachment_strip = (!self.attachments.is_empty()).then(|| {
            div().flex().gap(px(space::SM)).px(px(space::SM)).pt(px(space::SM)).children(self.attachments.iter().map(
                |attachment| {
                    let local_id = attachment.local_id;
                    let picture = attachment.preview.clone();
                    div()
                        .id(SharedString::from(format!("attachment-{local_id}")))
                        .relative()
                        .w(px(92.))
                        .h(px(64.))
                        .flex_none()
                        .rounded(px(radius::MD))
                        .bg(colors.surface_strong)
                        .overflow_hidden()
                        .map(|this| match &attachment.preview {
                            Some(path) => this.child(
                                img(crate::pictures::source(path.clone())).size_full().object_fit(ObjectFit::Cover),
                            ),
                            None => this.p(px(space::SM)).child(
                                div()
                                    .micro()
                                    .text_color(colors.text_secondary)
                                    .line_clamp(3)
                                    .child(attachment.name.clone()),
                            ),
                        })
                        // A picture opens to draw on before it goes.
                        .when_some(picture, |this, path| {
                            this.cursor_pointer()
                                .tooltip(|window, cx| {
                                    gpui_component::tooltip::Tooltip::new("Draw on it").build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.edit_picture(Source::Composer { local_id, path: path.clone() }, window, cx)
                                }))
                        })
                        .when(attachment.storage_id.is_none(), |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(colors.scrim.opacity(0.3))
                                    .child(ui::spinner(18., cx)),
                            )
                        })
                        .child(
                            div()
                                .id(SharedString::from(format!("remove-attachment-{local_id}")))
                                .absolute()
                                .top(px(4.))
                                .right(px(4.))
                                .size(px(20.))
                                .rounded(px(radius::FULL))
                                .bg(colors.media.opacity(0.7))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .child(ui::icon("x").size(px(12.)).text_color(colors.on_media))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.remove_attachment(local_id, window, cx);
                                })),
                        )
                },
            ))
        });
        // + at the left; the model, the reasoning and Send (or the mic, or Stop) at the right.
        let text_row = (!recording).then(|| {
            let plus = crate::smoke::tracked(
                "composer-attach",
                IconButton::new("composer-attach", "plus")
                    .small()
                    .tooltip("Attach files")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_files(window, cx))),
            );
            // An agent answers with the model picked for it, in its own chat and in the group; in the group it also thinks
            // as hard as its own chat has it.
            let mut controls = Vec::new();
            if self.group.is_none() && !self.model_fixed {
                controls.push(model_picker.into_any_element());
            }
            // Only the levels the chat's model takes, and none for a model without any.
            if let Some(reasoning_picker) = reasoning_picker.filter(|_| self.group.is_none()) {
                controls.push(crate::smoke::tracked("composer-reasoning", reasoning_picker));
            }
            controls.push(crate::smoke::tracked("composer-action", action));
            self.render_text_row(plus, controls, window, cx)
        });
        div().w_full().flex().justify_center().pb(px(space::LG)).child(
            div()
                .relative()
                .w_full()
                .max_w(px(crate::theme::MAX_CONTENT_WIDTH))
                .px(px(crate::theme::PAGE_PADDING))
                .flex()
                .flex_col()
                .gap(px(space::SM))
                // The questions float over the bottom of the chat, on the composer's top edge: out of the flow the
                // composer is measured by, so the messages and the composer stay put as they come, change and go.
                // A soft fade behind them lets the messages under them fall away.
                .children(panel.map(|panel| {
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(relative(1.))
                        .pt(px(PANEL_FADE))
                        .pb(px(space::SM))
                        .px(px(crate::theme::PAGE_PADDING))
                        .child(div().absolute().inset_0().bg(gpui::linear_gradient(
                            180.,
                            gpui::linear_color_stop(colors.chat_background.opacity(0.), 0.),
                            gpui::linear_color_stop(colors.chat_background.opacity(0.94), 0.45),
                        )))
                        .child(panel)
                }))
                .when_some(self.hint.clone(), |this, hint| {
                    this.child(div().px(px(space::SM)).footnote().text_color(colors.danger).child(hint))
                })
                .child(
                    div()
                        .relative()
                        .key_context(if mentioning {
                            "ChatComposer mentioning"
                        } else if back_to_questions {
                            "ChatComposer answering"
                        } else {
                            "ChatComposer"
                        })
                        // ⌘V with a copied picture or Finder files attaches them, and long text goes as paste.txt;
                        // other text goes on to the input.
                        .capture_action(cx.listener(|this, _: &Paste, window, cx| {
                            let mut files = crate::pasteboard::pasted_files(cx);
                            if files.is_empty() {
                                let draft_chars = this.input.read(cx).value().chars().count();
                                files.extend(crate::pasteboard::pasted_long_text(cx, draft_chars));
                            }
                            if !files.is_empty() {
                                cx.stop_propagation();
                                this.attach_files(files, window, cx);
                                return;
                            }
                            // Text with a file's chip copied from a message being written: the file comes too.
                            let copied = cx
                                .read_from_clipboard()
                                .and_then(|item| item.text())
                                .filter(|text| !attachments::loose_chips(text, &[]).is_empty());
                            if let Some(text) = copied {
                                cx.stop_propagation();
                                this.input.update(cx, |input, cx| input.replace(text, window, cx));
                                if let Err(name) = this.restore_chips(window, cx) {
                                    this.hint = Some(format!(
                                        "{name} isn't attached here. Attach it again, or delete its name."
                                    ));
                                }
                                cx.notify();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &SendMessage, window, cx| this.send(window, cx)))
                        // ↑ on the first line goes up to the panel's answers; on a later line it moves the caret.
                        .on_action(cx.listener(|this, _: &BackToQuestions, _, cx| {
                            if !this.input.read(cx).on_first_row() {
                                cx.propagate();
                                return;
                            }
                            this.question_panel.update(cx, |panel, cx| panel.from_below(cx));
                            this.focus_panel = true;
                            cx.notify();
                        }))
                        .on_action(cx.listener(|this, _: &MentionPrev, _, cx| this.move_mention(-1, cx)))
                        .on_action(cx.listener(|this, _: &MentionNext, _, cx| this.move_mention(1, cx)))
                        .on_action(cx.listener(|this, _: &MentionPick, window, cx| this.pick_mention(None, window, cx)))
                        .on_action(cx.listener(|this, _: &MentionDismiss, _, cx| {
                            this.mention_dismissed = this.mention_query(cx).map(|(at, _)| at);
                            cx.notify();
                        }))
                        .children(mention_list)
                        // A Working row dropped here: the message points at its task (work_share.rs).
                        .on_drop(cx.listener(|this, dragged: &DraggedWork, window, cx| {
                            this.share_work(dragged.0.clone(), window, cx)
                        }))
                        .drag_over::<DraggedWork>(move |style, _, _, _| {
                            style.border_color(colors.accent).bg(colors.accent_soft.opacity(0.5))
                        })
                        .flex()
                        .flex_col()
                        .rounded(px(20.))
                        // Floats over the feed, opaque: messages scroll behind it but never show through it, and a
                        // click on it never reaches them.
                        .block_mouse_except_scroll()
                        .bg(colors.surface)
                        .border_1()
                        .border_color(colors.border)
                        .shadow_lg()
                        .when(reply_strip.is_some() || note_strip.is_some() || attachment_strip.is_some(), |this| {
                            // Measured, so sending (which clears them) can make the feed's room for the composer
                            // smaller in the same frame as the composer shrinks.
                            let strips_height = self.strips_height.clone();
                            this.child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .children(reply_strip)
                                    .children(note_strip)
                                    .children(attachment_strip)
                                    .child(
                                        gpui::canvas(
                                            move |bounds, _, _| strips_height.set(f32::from(bounds.size.height)),
                                            |_, _, _, _| {},
                                        )
                                        .absolute()
                                        .top_0()
                                        .left_0()
                                        .size_full(),
                                    ),
                            )
                        })
                        .when_some(voice_row, |this, row| this.child(row))
                        .children(text_row),
                ),
        )
    }
}

gpui::actions!(chat, [SendMessage, MentionPrev, MentionNext, MentionPick, MentionDismiss, BackToQuestions]);

/// `voiceSessions:sendMessage`'s args for the message being sent, with the reasoning level the chat doesn't have yet. A
/// retry with the same `requestId` finds the turn the first try made.
fn message_args(session_id: &str, pending: &PendingSend, reasoning: Option<&str>) -> Value {
    let mut args = json!({
        "sessionId": session_id,
        "requestId": pending.request_id,
        "message": pending.text,
    });
    if let Some(reply) = &pending.reply {
        args["replyToMessageId"] = json!(reply.message_id);
    }
    let sent: Vec<Value> = pending.attachments.iter().filter_map(PendingAttachment::sent).collect();
    if !sent.is_empty() {
        args["attachments"] = Value::Array(sent);
    }
    if !pending.notes.is_empty() {
        args["sharedNotes"] = note_share::args(&pending.notes);
    }
    if !pending.work.is_empty() {
        args["sharedWork"] = work_share::args(&pending.work);
    }
    if let Some((_, note)) = &pending.context {
        args["contextNote"] = json!(note);
    }
    if let Some(reasoning) = reasoning {
        args["reasoning"] = json!(reasoning);
    }
    args
}

/// Names the composer offers after an "@", at most this many at once.
const MAX_MENTION_ROWS: usize = 10;

/// A conversation while its messages load: bubbles at the bottom like a chat,
/// short ones of yours on the right, longer replies on the left.
pub fn messages_skeleton(composer_height: f32, cx: &App) -> gpui::Div {
    let colors = *cx.colors();
    // (yours, width of the bubble, lines)
    const BUBBLES: [(bool, f32, usize); 6] =
        [(false, 0.7, 3), (true, 0.38, 1), (false, 0.8, 4), (true, 0.5, 2), (false, 0.62, 2), (true, 0.3, 1)];
    div().flex_1().min_h_0().w_full().flex().justify_center().overflow_hidden().child(
        div()
            .w_full()
            .h_full()
            .max_w(px(crate::theme::MAX_CONTENT_WIDTH))
            .px(px(crate::theme::PAGE_PADDING))
            .pb(px(composer_height + space::XXXL))
            .flex()
            .flex_col()
            .justify_end()
            .gap(px(space::MD))
            .children(BUBBLES.iter().map(|&(yours, width, lines)| {
                let bubble = div()
                    .w(relative(width))
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(20.))
                    .border_1()
                    .border_color(colors.border.opacity(0.6))
                    .flex()
                    .flex_col()
                    .gap(px(9.))
                    .children((0..lines).map(move |line| {
                        // The last line of a longer reply runs short, like text does.
                        let last = lines > 1 && line + 1 == lines;
                        ui::skeleton(relative(if last { 0.55 } else { 1. }), 10., 5.).secondary()
                    }));
                div().w_full().flex().when(yours, |this| this.justify_end()).child(bubble)
            })),
    )
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        // Esc closes these, top first (escape.rs): the reaction picker over a reply being written; the image editor
        // is in a dialog.
        if self.reply_to.is_some() {
            crate::escape::layer(window, cx, |this, _, cx| {
                this.reply_to = None;
                cx.notify();
            });
        }
        if self.picker_for.is_some() {
            crate::escape::popup(window, cx, |this, _, cx| {
                this.picker_for = None;
                cx.notify();
            });
        }
        if self.image_editor.is_some() {
            crate::escape::dialog_layer(window, cx, |this, window, cx| this.close_editor(window, cx));
        }
        self.hold_pin();
        self.note_glance_clip();
        // A letter typed on a sound's row comes back to this box while it's drawn (audio_row.rs).
        crate::audio_row::message_box(&gpui::Focusable::focus_handle(self.input.read(cx), cx), window, cx);
        let body: AnyElement = match (&self.session_id, &self.detail) {
            (None, _) if self.pending.is_none() && !self.fixture() => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .when_some(self.draft_prompt.clone(), |this, prompt| {
                    this.child(div().title2().text_color(colors.text_secondary).child(prompt))
                })
                .into_any_element(),
            (Some(_), None) if self.known_empty => div().flex_1().into_any_element(),
            (Some(_), None) => messages_skeleton(self.composer_height, cx)
                .when(self.slow_load, |this| {
                    this.child(
                        div()
                            .pt(px(space::SM))
                            .footnote()
                            .text_color(colors.text_secondary)
                            .child("Still loading messages…"),
                    )
                })
                .into_any_element(),
            (Some(_), Some(Ok(None))) => ui::empty_state(
                "message-circle",
                "Context unavailable",
                Some("This conversation could not be loaded.".into()),
                None,
                cx,
            )
            .flex_1()
            .into_any_element(),
            (Some(_), Some(Err(error))) if self.detail().is_none() => {
                ui::empty_state("triangle-alert", "Context unavailable", Some(error.clone().into()), None, cx)
                    .flex_1()
                    .into_any_element()
            }
            // In a narrow chat the rows leave room at the right for the timeline's grab strip.
            _ => div()
                .flex_1()
                .min_h_0()
                .w_full()
                .flex()
                .pr(px(timeline::gutter(self.width)))
                .child(
                    list(
                        self.list_state.clone(),
                        cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)),
                    )
                    .flex_1()
                    .h_full(),
                )
                .into_any_element(),
        };
        let view = cx.entity().downgrade();
        // Measures the composer each frame; the feed's bottom spacer follows it.
        let measure = gpui::canvas(
            move |bounds, _, cx| {
                let (height, width) = (f32::from(bounds.size.height), f32::from(bounds.size.width));
                view.update(cx, |this, cx| {
                    // A new width places the glance cards again (glance.rs `card_place`).
                    if (width - this.width).abs() > 0.5 && this.width > 0. {
                        cx.notify();
                    }
                    this.width = width;
                    let height = height - this.composer_layout.feed_lag();
                    if (height - this.composer_height).abs() > 0.5 {
                        this.composer_height = height;
                        this.fit_spacer(cx);
                        cx.notify();
                    }
                    this.note_held();
                    this.after_layout(cx);
                })
                .ok();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.chat_background)
            .track_focus(&self.focus)
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.attach_files(paths.paths().to_vec(), window, cx)
            }))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(colors.accent_soft.opacity(0.3)))
            .child(body)
            .children(self.render_timeline(cx))
            .children(self.render_jump_pill(cx))
            // The measure also runs `after_layout`, which marks what's on screen seen: the group keeps it, in a strip
            // where the composer would be.
            .child(
                div().absolute().left_0().right_0().bottom_0().child(measure).child(self.render_composer(window, cx)),
            )
    }
}

// --- Row pieces ---

/// A message as the markdown its bubble shows, with each agent's @name a `mention:<chat>/<speaker>` link
/// (`open_mention`), which the text view draws in the accent colour and opens that agent. Names in code are left alone.
fn mention_markdown(text: &str, members: &[(String, String)], chat: gpui::EntityId) -> String {
    // The longest name first, so "@Ann Lee" isn't taken for "@Ann".
    let mut members: Vec<&(String, String)> =
        members.iter().filter(|(speaker, name)| !name.is_empty() && filo_local::store::valid_id(speaker)).collect();
    members.sort_by_key(|(_, name)| std::cmp::Reverse(name.len()));
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        let fence = line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
        if fenced || fence {
            fenced ^= fence;
            out.push_str(line);
            continue;
        }
        let mut code = false;
        let mut rest = line;
        while let Some(c) = rest.chars().next() {
            let found = (c == '@' && !code)
                .then(|| members.iter().find(|(_, name)| rest[1..].starts_with(name.as_str())))
                .flatten();
            match found {
                Some((speaker, name)) => {
                    let escaped: String = name
                        .chars()
                        .flat_map(|c| "\\`*_[]<>".contains(c).then_some('\\').into_iter().chain([c]))
                        .collect();
                    out.push_str(&format!("[@{escaped}](mention:{}/{speaker})", chat.as_u64()));
                    rest = &rest[1 + name.len()..];
                }
                None => {
                    code ^= c == '`';
                    out.push(c);
                    rest = &rest[c.len_utf8()..];
                }
            }
        }
    }
    out
}

/// The conversations, for the `mention:` links in their messages (`open_mention`).
#[derive(Default)]
struct MentionChats(Vec<gpui::WeakEntity<Conversation>>);

impl gpui::Global for MentionChats {}

/// Opens the agent a `mention:<chat>/<speaker>` link names (`mention_markdown`); commit_links.rs's link handler hands
/// links here. False for any other link.
pub fn open_mention(url: &str, cx: &mut App) -> bool {
    let Some((chat, speaker)) = url.strip_prefix("mention:").and_then(|rest| rest.split_once('/')) else {
        return false;
    };
    let view = cx
        .try_global::<MentionChats>()
        .and_then(|chats| chats.0.iter().find(|view| view.entity_id().as_u64().to_string() == chat).cloned());
    if let Some(view) = view {
        let speaker = speaker.to_string();
        view.update(cx, |_, cx| cx.emit(ConversationEvent::OpenSpeaker(speaker))).ok();
    }
    true
}

/// Whether row `index` needs its link to the message `target` it answers, above it: someone else (the user, or in the
/// group another member) wrote a message after that one, or the replier answered another message in between, so it
/// isn't clear which one it answers. Cards, glances and the replier's other messages of the same answer don't count,
/// and an answer in several messages links only its first (the nearest row above that isn't a time mark is another
/// answer to it). In the group, a coding task's result links what asked for it past anything, the replier's own "Started"
/// too, unless it's right under it or goes on from its own first message. None when that message isn't among `rows`.
fn apart(rows: &[ViewRow], index: usize, target: &str, group: bool) -> Option<bool> {
    let ViewRow::Message { message: reply, .. } = &rows[index] else { return Some(false) };
    let answered = rows[..index].iter().rposition(|row| row_message_id(row) == Some(target))?;
    let above = rows[..index].iter().rev().find(|row| {
        !matches!(
            row,
            ViewRow::Older
                | ViewRow::Gap(_)
                | ViewRow::Mark(_)
                | ViewRow::Unseen
                | ViewRow::Thinking(..)
                | ViewRow::Spacer(_)
        )
    });
    let answers_too = |message: &Message| message.reply_to_message_id.as_deref() == Some(target);
    if group && reply.coding_result.is_some() {
        let goes_on = matches!(above, Some(ViewRow::Message { message, .. }) if answers_too(message) && message.turn == reply.turn);
        return Some(!goes_on && above.and_then(row_message_id) != Some(target));
    }
    if let Some(ViewRow::Message { message, .. }) = above
        && answers_too(message)
    {
        return Some(false);
    }
    let someone_else = |message: &Message| {
        message.is_user() != reply.is_user() || (!reply.is_user() && message.speaker != reply.speaker)
    };
    let answers_another =
        |message: &Message| message.reply_to_message_id.as_deref().is_some_and(|answered| answered != target);
    Some(rows[answered + 1..index].iter().any(
        |row| matches!(row, ViewRow::Message { message, .. } if someone_else(message) || answers_another(message)),
    ))
}

/// The message a row shows, for a reply's link to find (`link_replies`, `jump_to`).
fn row_message_id(row: &ViewRow) -> Option<&str> {
    match row {
        ViewRow::Message { message, .. } => Some(&message.id),
        ViewRow::GroupLink { id, .. } | ViewRow::GroupCall { id, .. } | ViewRow::Routine { id, .. } => Some(id),
        _ => None,
    }
}

/// Selected text as a markdown quote ("> " before each line, blank lines kept as ">"), then a blank line for the
/// answer. Empty for nothing but spaces.
fn quote_block(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = text
        .lines()
        .map(|line| if line.trim().is_empty() { ">".to_string() } else { format!("> {}", line.trim_end()) })
        .collect();
    format!("{}\n\n", lines.join("\n"))
}

/// What goes between what's typed and a quote put after it: a blank line, or nothing in an empty composer.
fn quote_gap(typed: &str) -> &'static str {
    if typed.trim().is_empty() || typed.ends_with("\n\n") {
        ""
    } else if typed.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}

/// What the user sent (`attachments::with_chips`), with its mentions (`mention_words`) in the accent colour and its
/// pictures' chips where they were, as both were in the composer: the chips are the composer's own (its chip text,
/// `chip_atom`'s colours, painted by the input's `paint_chip`). A chip opens the viewer on that picture, as one in the
/// pictures above the bubble does.
fn user_text(
    key: &str,
    text: &str,
    chips: &[attachments::SentChip],
    files: &[Attachment],
    mention_words: &[String],
    editor: Option<gpui::WeakEntity<Conversation>>,
    cx: &mut App,
) -> AnyElement {
    let colors = *cx.colors();
    let mut styles: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> =
        mention_suggestions::mention_ranges(text, mention_words)
            .into_iter()
            .filter(|range| !chips.iter().any(|chip| range.start < chip.range.end && chip.range.start < range.end))
            .map(|range| (range, gpui::HighlightStyle { color: Some(colors.accent), ..Default::default() }))
            .collect();
    let atoms: Vec<InputAtom> = chips
        .iter()
        .map(|chip| {
            let file = &files[chip.file];
            let shape =
                attachments::ChipText { text: String::new(), image_len: chip.image_len, muted_from: chip.muted_from };
            let mut atom = Conversation::chip_atom(chip.file as u64, &shape, cx);
            atom.range = chip.range.clone();
            atom.image = file
                .local_file
                .as_deref()
                .filter(|_| chip.image_len > 0)
                .and_then(|path| attachments::sent_chip_picture(path, cx));
            atom
        })
        .collect();
    styles.extend(atoms.iter().map(|atom| {
        (
            atom.range.start + atom.muted_from..atom.range.end,
            gpui::HighlightStyle { color: Some(atom.muted), ..Default::default() },
        )
    }));
    styles.sort_by_key(|(range, _)| range.start);
    let styled = gpui::StyledText::new(text.to_string()).with_highlights(styles);
    if atoms.is_empty() {
        return div().callout().child(styled).into_any_element();
    }
    // What the viewer steps through: the message's pictures and videos on this Mac, as from the pictures above.
    let media: Vec<&Attachment> =
        files.iter().filter(|a| a.content_type.starts_with("image/") || a.content_type.starts_with("video/")).collect();
    let shown: Vec<crate::file_preview::Item> =
        media.iter().filter_map(|attachment| preview_item(attachment)).collect();
    let opens: Vec<Option<usize>> = chips
        .iter()
        .map(|chip| {
            preview_item(&files[chip.file]).and_then(|item| shown.iter().position(|other| other.file == item.file))
        })
        .collect();
    let layout = styled.layout().clone();
    let atoms = Rc::new(atoms);
    let pieces = {
        let (layout, text) = (layout.clone(), text.to_string());
        move |atom: &InputAtom| chip_pieces(&layout, &text, atom)
    };
    // Under the text, as the input paints them: each chip on its line (a chip wraps as one word), its picture in the
    // room its text leaves.
    let under = gpui::canvas(|_, _, _| {}, {
        let (atoms, pieces, layout) = (atoms.clone(), pieces.clone(), layout.clone());
        move |_, _, window, _| {
            for atom in atoms.iter() {
                let (rows, room) = pieces(atom);
                gpui_component::input::paint_chip(atom, &rows, room, layout.line_height(), window);
            }
        }
    })
    .absolute()
    .size_full();
    // Over it, laid out after the text: the pointer is a hand over a chip.
    let over = gpui::canvas(
        {
            let atoms = atoms.clone();
            move |_, window, _| {
                atoms
                    .iter()
                    .flat_map(|atom| pieces(atom).0)
                    .map(|row| window.insert_hitbox(row, gpui::HitboxBehavior::Normal))
                    .collect::<Vec<_>>()
            }
        },
        |_, hitboxes, window, _| {
            for hitbox in &hitboxes {
                window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
            }
        },
    )
    .absolute()
    .size_full();
    // A click on a chip opens it. Told from where the click was, so a click whose press and release come together (a
    // tap on the trackpad) counts too.
    let open = move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
        let Ok(index) = layout.index_for_position(event.position()) else { return };
        let Some(chip) = atoms.iter().position(|atom| atom.range.contains(&index)) else { return };
        if let Some(at) = opens.get(chip).copied().flatten() {
            open_media(shown.clone(), at, editor.clone(), window, cx);
        }
    };
    div()
        .id(SharedString::from(format!("text-{key}")))
        .callout()
        .relative()
        .child(under)
        .child(styled)
        .child(over)
        .on_click(open)
        .into_any_element()
}

/// Where a chip in a sent message's text sits: a piece per line it covers (a line high), and the room its text leaves
/// for its picture.
fn chip_pieces(
    layout: &gpui::TextLayout,
    text: &str,
    atom: &InputAtom,
) -> (Vec<gpui::Bounds<gpui::Pixels>>, Option<gpui::Bounds<gpui::Pixels>>) {
    let at = |index: usize| layout.position_for_index(index);
    let (Some(start), Some(end)) = (at(atom.range.start), at(atom.range.end)) else { return (Vec::new(), None) };
    let line_height = layout.line_height();
    // Where the text wraps right before the chip, its start reads as the end of the line above.
    let next = text
        .get(atom.range.start..)
        .and_then(|rest| rest.chars().next())
        .and_then(|c| at(atom.range.start + c.len_utf8()));
    let start = match next {
        Some(next) if next.y > start.y => gpui::point(layout.bounds().left(), next.y),
        _ => start,
    };
    let row = |from: gpui::Pixels, to: gpui::Pixels, y: gpui::Pixels| {
        gpui::Bounds::new(gpui::point(from, y), gpui::size(to - from, line_height))
    };
    let rows = if end.y > start.y {
        vec![row(start.x, layout.bounds().right(), start.y), row(layout.bounds().left(), end.x, end.y)]
    } else {
        vec![row(start.x, end.x, start.y)]
    };
    let room = at(atom.range.start + atom.image_len)
        .filter(|_| atom.image_len > 0)
        .map(|room_end| row(start.x, room_end.x.max(start.x), start.y));
    (rows, room)
}

/// Opens the app's viewer on a message's pictures and videos at `at`. Where Filo can be sent a picture (an agent's
/// chat, an `editor`), the viewer offers to draw on it.
fn open_media(
    shown: Vec<crate::file_preview::Item>,
    at: usize,
    editor: Option<gpui::WeakEntity<Conversation>>,
    window: &mut Window,
    cx: &mut App,
) {
    let draw = editor.map(|view| -> crate::media_viewer::Draw {
        Rc::new(move |item, window, cx| {
            view.update(cx, |view, cx| view.edit_picture(Source::Chat(item), window, cx)).ok();
        })
    });
    crate::media_viewer::open(shown, at, draw, window, cx);
}

/// The pictures among a message's media that can be copied: those on this Mac.
fn copyable_pictures(media: &[&Attachment]) -> Vec<crate::file_preview::Item> {
    media.iter().filter_map(|attachment| preview_item(attachment)).filter(crate::media_viewer::is_picture).collect()
}

/// What opens when it's clicked: its file on this Mac, none for one that's still being picked up.
fn preview_item(attachment: &Attachment) -> Option<crate::file_preview::Item> {
    let file = attachment.local_file.clone()?;
    Some(crate::file_preview::Item {
        url: String::new(),
        name: attachment.name.clone(),
        content_type: attachment.content_type.clone(),
        file: Some(file),
    })
}

/// The box a lone picture or video gets: its own shape (very tall or wide ones cropped to 2:5 / 5:2), at most
/// 320 × 340 and never larger than the picture. 4:3 (16:9 for a video) until the picture has loaded.
fn media_box(natural: Option<(f32, f32)>, video: bool) -> (f32, f32) {
    const MAX_W: f32 = 320.;
    const MAX_H: f32 = 340.;
    let (width, height) =
        natural.filter(|(w, h)| *w > 0. && *h > 0.).unwrap_or(if video { (16., 9.) } else { (4., 3.) });
    let aspect = (width / height).clamp(0.4, 2.5);
    let (w, h) = if aspect >= MAX_W / MAX_H { (MAX_W, MAX_W / aspect) } else { (MAX_H * aspect, MAX_H) };
    // A small picture keeps its size (in points on a 2× screen), but no smaller than 120 across.
    let shrink = natural.map_or(1., |(natural_w, _)| ((natural_w / 2.).max(120.) / w).min(1.));
    (w * shrink, h * shrink)
}

/// The picture's size in pixels once it has loaded (the same cache `img` loads into, so nothing loads twice).
fn loaded_size(picture: &Resource, window: &mut Window, cx: &mut App) -> Option<(f32, f32)> {
    crate::pictures::natural_size_of(picture, window, cx)
}

/// Where an attachment's picture comes from: its file on this Mac (for one still being sent, the file it was picked
/// from, already loaded as its preview was, so it shows at once). None for a video: its poster is `video_frame`.
fn picture(attachment: &Attachment) -> Option<Resource> {
    let file = attachment.local_file.as_ref().filter(|_| !attachment.content_type.starts_with("video/"))?;
    Some(Resource::Path(file.as_path().into()))
}

/// A video's poster and length, made from its file (video_player.rs).
fn video_frame(attachment: &Attachment, cx: &mut App) -> Option<crate::video_player::Poster> {
    let video = attachment.content_type.starts_with("video/");
    preview_item(attachment).filter(|_| video).and_then(|item| crate::video_player::poster(&item, cx))
}

/// Where a fixture's bubbles, pictures and links were last drawn, by name (`probe`); nothing outside `dev` builds.
#[cfg(feature = "dev")]
type Drawn = Rc<std::cell::RefCell<HashMap<String, gpui::Bounds<gpui::Pixels>>>>;
#[cfg(not(feature = "dev"))]
#[derive(Clone, Default)]
struct Drawn(());

/// Records where its parent (`relative`) was drawn, under `name` (fixture checks).
#[cfg(feature = "dev")]
fn probe(drawn: &Drawn, name: String) -> Option<AnyElement> {
    let drawn = drawn.clone();
    let probe = gpui::canvas(
        move |bounds, _, _| {
            drawn.borrow_mut().insert(name.clone(), bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    Some(probe.into_any_element())
}

#[cfg(not(feature = "dev"))]
fn probe(_: &Drawn, _: String) -> Option<AnyElement> {
    None
}

/// How far a reaction hangs below its bubble (it's 20 px tall, so half sits on the bubble).
const REACTION_HANG: f32 = 10.;

/// Rows for a mosaic of `count` pictures: two share a row, three put one across the top, then rows of two and three.
fn mosaic_rows(count: usize) -> Vec<usize> {
    match count {
        0 | 1 => vec![count],
        2 => vec![2],
        3 => vec![1, 2],
        4 => vec![2, 2],
        _ => {
            let mut rows = Vec::new();
            let mut left = count;
            while left > 0 {
                // Rows of three, but never a row of one at the end.
                let take = if left == 4 || left == 2 { 2 } else { left.min(3) };
                rows.push(take);
                left -= take;
            }
            rows
        }
    }
}

/// Each picture's size in a mosaic `width` wide: a row is one height, its pictures as wide as their shapes allow, the
/// row exactly filling the width (a very tall or wide picture is cropped a little to keep rows sensible).
fn mosaic_sizes(aspects: &[f32], width: f32, gap: f32) -> Vec<(f32, f32)> {
    let mut sizes = Vec::with_capacity(aspects.len());
    let mut start = 0;
    for count in mosaic_rows(aspects.len()) {
        let row = &aspects[start..start + count];
        let inner = width - gap * (count as f32 - 1.);
        let total: f32 = row.iter().sum();
        let height = (inner / total).clamp(if count == 1 { 140. } else { 96. }, if count == 1 { 300. } else { 220. });
        for aspect in row {
            sizes.push((inner * aspect / total, height));
        }
        start += count;
    }
    sizes
}

/// A message's pictures and videos, outside its bubble: one in its own shape, several as a mosaic of rows that keep
/// their shapes (two side by side, three as one across the top and two below, and so on). A tile opens the app's
/// viewer (media_viewer.rs) on the message's pictures and videos, where a video plays and a picture can be drawn on
/// (in the image editor, when there's an `editor`). A picture being sent shows as it will stay, with
/// no spinner while it uploads (a failed send shows its error and Retry under the message), and opens from its file.
fn render_media(
    id: &str,
    media: &[&Attachment],
    editor: Option<gpui::WeakEntity<Conversation>>,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Div {
    let colors = *cx.colors();
    let single = media.len() == 1;
    const MOSAIC_WIDTH: f32 = 440.;
    const GAP: f32 = 3.;
    const OUTER: f32 = 18.;
    const INNER: f32 = 6.;
    let posters: Vec<Option<crate::video_player::Poster>> =
        media.iter().map(|attachment| video_frame(attachment, cx)).collect();
    let sources: Vec<(bool, Option<Resource>)> = media
        .iter()
        .zip(&posters)
        .map(|(attachment, poster)| {
            let poster = poster.as_ref().map(|poster| Resource::Path(poster.picture.as_path().into()));
            (attachment.content_type.starts_with("video/"), picture(attachment).or(poster))
        })
        .collect();
    let sizes: Vec<(f32, f32)> = if single && media[0].gone {
        vec![GONE_TILE]
    } else if single {
        let (is_video, source) = &sources[0];
        vec![media_box(source.as_ref().and_then(|picture| loaded_size(picture, window, cx)), *is_video)]
    } else {
        // Until a picture has loaded, it counts as 4:3 (16:9 for a video).
        let aspects: Vec<f32> = sources
            .iter()
            .map(|(is_video, source)| {
                source
                    .as_ref()
                    .and_then(|picture| loaded_size(picture, window, cx))
                    .filter(|(w, h)| *w > 0. && *h > 0.)
                    .map_or(if *is_video { 16. / 9. } else { 4. / 3. }, |(w, h)| (w / h).clamp(0.5, 2.4))
            })
            .collect();
        mosaic_sizes(&aspects, MOSAIC_WIDTH, GAP)
    };
    // Where each tile sits, for its corners: the mosaic's outer corners are round, the inner ones small.
    let rows = mosaic_rows(media.len());
    let mut places = Vec::with_capacity(media.len());
    for (row_ix, count) in rows.iter().enumerate() {
        for col in 0..*count {
            places.push((row_ix == 0, row_ix + 1 == rows.len(), col == 0, col + 1 == *count));
        }
    }
    // What the viewer steps through: the message's pictures and videos that have uploaded, and pictures being sent
    // from this Mac.
    let mut shown: Vec<crate::file_preview::Item> = Vec::with_capacity(media.len());
    let shown_at: Vec<Option<usize>> = media
        .iter()
        .map(|attachment| {
            preview_item(attachment).map(|item| {
                shown.push(item);
                shown.len() - 1
            })
        })
        .collect();
    let pictures = copyable_pictures(media);
    let mut row_divs: Vec<gpui::Div> = Vec::new();
    let mut index = 0;
    for count in rows {
        let mut row = div().flex().gap(px(GAP));
        for _ in 0..count {
            let (is_video, source) = sources[index].clone();
            let (width, height) = sizes[index];
            let (top, bottom, left, right) = places[index];
            let corner = |outer: bool| px(if single || outer { OUTER } else { INNER });
            let shown_at = shown_at[index];
            let gone = media[index].gone;
            let tile = div()
                .id(SharedString::from(format!("media-{id}-{index}")))
                .relative()
                .w(px(width))
                .h(px(height))
                .rounded_tl(corner(top && left))
                .rounded_tr(corner(top && right))
                .rounded_bl(corner(bottom && left))
                .rounded_br(corner(bottom && right))
                .overflow_hidden()
                .bg(colors.surface_strong)
                .when(!gone, |this| this.cursor_pointer())
                .when(gone, |this| this.child(gone_tile(media[index], colors)))
                // The picture takes the corners itself: clipping to the tile is rectangular.
                .when_some(source, |this, picture| {
                    this.child(
                        img(crate::pictures::source(ImageSource::Resource(picture)))
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .rounded_tl(corner(top && left))
                            .rounded_tr(corner(top && right))
                            .rounded_bl(corner(bottom && left))
                            .rounded_br(corner(bottom && right)),
                    )
                })
                // A cropped picture rounds its own (larger) bounds, so its corners would come out square: a ring in
                // the chat's colour, its inner edge the tile's rounded outline, covers what spills past them (the
                // tile clips the rest of the ring away).
                .child(
                    div()
                        .absolute()
                        .top(-px(OUTER))
                        .left(-px(OUTER))
                        .right(-px(OUTER))
                        .bottom(-px(OUTER))
                        .rounded_tl(corner(top && left) + px(OUTER))
                        .rounded_tr(corner(top && right) + px(OUTER))
                        .rounded_bl(corner(bottom && left) + px(OUTER))
                        .rounded_br(corner(bottom && right) + px(OUTER))
                        .border(px(OUTER))
                        .border_color(colors.chat_background),
                )
                // A hairline on top, so a dark screenshot doesn't melt into a dark chat: drawn over the picture, it
                // takes no room and keeps the tile's size.
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_tl(corner(top && left))
                        .rounded_tr(corner(top && right))
                        .rounded_bl(corner(bottom && left))
                        .rounded_br(corner(bottom && right))
                        .border_1()
                        .border_color(colors.border),
                )
                .when(is_video && !gone, |this| {
                    let duration = posters[index].as_ref().and_then(|poster| poster.duration_ms);
                    this.child(crate::video_player::badge(duration, cx))
                })
                .on_click({
                    let (shown, editor) = (shown.clone(), editor.clone());
                    move |_, window, cx| {
                        let Some(at) = shown_at else { return };
                        open_media(shown.clone(), at, editor.clone(), window, cx);
                    }
                });
            // A picture's right-click menu copies it (and, in a message with several, all of them).
            let copyable =
                shown_at.and_then(|at| shown.get(at)).filter(|item| crate::media_viewer::is_picture(item)).cloned();
            let tile = match copyable {
                Some(item) => {
                    use gpui_component::menu::ContextMenuExt as _;
                    let all = pictures.clone();
                    let tile = tile.context_menu(move |menu, _, _| {
                        let item = item.clone();
                        let menu = menu.item(
                            PopupMenuItem::new("Copy image")
                                .icon(gpui_component::Icon::empty().path("icons/copy.svg"))
                                .on_click(move |_, window, cx| {
                                    crate::media_viewer::copy_pictures_and_tell(vec![item.clone()], window, cx)
                                }),
                        );
                        if all.len() < 2 {
                            return menu;
                        }
                        let all = all.clone();
                        menu.item(
                            PopupMenuItem::new(format!("Copy all {} images", all.len()))
                                .icon(gpui_component::Icon::empty().path("icons/image.svg"))
                                .on_click(move |_, window, cx| {
                                    crate::media_viewer::copy_pictures_and_tell(all.clone(), window, cx)
                                }),
                        )
                    });
                    // Its own id around the menu: gpui-component names every context menu "context-menu", so tiles
                    // side by side would share one menu (notes.rs).
                    div().id(SharedString::from(format!("media-menu-{id}-{index}"))).child(tile).into_any_element()
                }
                None => tile.into_any_element(),
            };
            row = row.child(tile);
            index += 1;
        }
        row_divs.push(row);
    }
    div().flex().flex_col().gap(px(GAP)).children(row_divs)
}

/// The size of a lone picture's or video's tile once its file is gone (`gone_tile`).
const GONE_TILE: (f32, f32) = (240., 150.);

/// What a picture's or video's tile shows once its file is no longer there (Filo's copy deleted from the Shared list, an
/// agent's file shown in place deleted since it was sent): its name, said to be gone, in place of the picture.
fn gone_tile(file: &Attachment, colors: crate::theme::Palette) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .p(px(space::MD))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(space::XS))
        .text_color(colors.text_secondary)
        .child(ui::icon("file-x").size(px(22.)).text_color(colors.text_tertiary))
        .child(div().footnote().semibold().child(file.gone_label()))
        .child(div().caption().max_w_full().truncate().child(file.name.clone()))
}

fn render_file(id: &str, index: usize, file: &Attachment, user: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let colors = *cx.colors();
    let fg = if user { colors.on_user_bubble } else { colors.on_assistant_bubble };
    // A sound that's gone shows as a file that's gone: there's nothing to play.
    if media::is_audio(&file.content_type, &file.name) && !file.gone {
        // It plays in place from this Mac's copy (audio_row.rs).
        let key = format!("{id}-{index}");
        return crate::audio_row::row(&key, file.local_file.as_deref(), &file.name, 300., fg, window, cx);
    }
    let preview = preview_item(file);
    let row = div()
        .id(SharedString::from(format!("file-{id}-{index}")))
        .flex()
        .items_center()
        .gap(px(space::SM))
        .when(!file.gone, |this| this.cursor_pointer())
        .child(
            div()
                .size(px(36.))
                .flex_none()
                .rounded(px(radius::SM))
                .bg(fg.opacity(0.1))
                .flex()
                .items_center()
                .justify_center()
                .child(ui::icon(if file.gone { "file-x" } else { "file-text" }).size(px(18.)).text_color(fg)),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().footnote().semibold().truncate().child(file.name.clone()))
                .child(div().caption().opacity(0.7).child(if file.gone {
                    file.gone_label().to_string()
                } else {
                    format::file_size(file.size)
                })),
        )
        .on_click(move |_, window, cx| {
            if let Some(item) = &preview {
                crate::file_preview::open(item.clone(), window, cx);
            }
        });
    crate::smoke::tracked(format!("chat-file-{}", file.name), row)
}

/// A voice message whose recording the user deleted from the Shared list: said to be no longer kept, as a file is,
/// with its length.
fn gone_voice(duration_ms: f64, user: bool, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let fg = if user { colors.on_user_bubble } else { colors.on_assistant_bubble };
    let length = (duration_ms > 0.).then(|| format!(" · {}", format::duration_clock(duration_ms)));
    div()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .child(
            div()
                .size(px(32.))
                .flex_none()
                .rounded(px(radius::FULL))
                .bg(fg.opacity(0.1))
                .flex()
                .items_center()
                .justify_center()
                .child(ui::icon("file-x").size(px(16.)).text_color(fg)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().footnote().semibold().child("Voice message"))
                .child(div().caption().opacity(0.7).child(format!("No longer kept{}", length.unwrap_or_default()))),
        )
        .into_any_element()
}

fn render_voice(id: &str, url: Option<String>, duration_ms: f64, label: &str, user: bool, cx: &App) -> AnyElement {
    render_voice_sized(id, url, duration_ms, label, user, 32., cx)
}

/// `render_voice` with a smaller play button, to fit the composer's row.
fn render_voice_sized(
    id: &str,
    url: Option<String>,
    duration_ms: f64,
    label: &str,
    user: bool,
    button: f32,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    let fg = if user { colors.on_user_bubble } else { colors.on_assistant_bubble };
    let audio_id = format!("voice-{id}");
    let playing = media::is_playing(&audio_id, cx);
    // A static waveform, like the app's placeholder bars.
    let bars = [6., 12., 18., 10., 22., 14., 8., 16., 20., 12., 6., 14., 18., 10., 8., 12., 16., 6.];
    div()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .child(
            div()
                .id(SharedString::from(format!("play-{audio_id}")))
                .size(px(button))
                .flex_none()
                .rounded(px(radius::FULL))
                .bg(fg.opacity(0.15))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(ui::icon(if playing { "pause" } else { "play" }).size(px(16.)).text_color(fg))
                .on_click(move |_, _, cx| {
                    if let Some(url) = &url {
                        media::toggle(audio_id.clone(), url.clone(), None, cx);
                    }
                }),
        )
        .child(div().flex().items_center().gap(px(2.)).children(bars.iter().map(|height| {
            div().w(px(3.)).h(px(*height)).rounded(px(2.)).bg(fg.opacity(if playing { 0.9 } else { 0.5 }))
        })))
        .child(div().caption().opacity(0.8).child(if duration_ms > 0. {
            format::duration_clock(duration_ms)
        } else {
            label.to_string()
        }))
        .into_any_element()
}

/// Where a Filo message's result row goes: the coding round that message itself reports (the backend sets it on the
/// message), never a nearby or the latest task's; while that task still works (`task`), its live thread instead, as
/// the round's page would only have what it said so far. The user's messages and older ones without it have none.
fn proof_route(message: &Message, task: Option<&ProofTask>) -> Option<Route> {
    let result = message.coding_result.as_ref().filter(|_| !message.is_user())?;
    Some(match task {
        Some(task) if task.live => Route::CodingTask(result.task_id.clone()),
        _ => Route::CodingResult(result.task_id.clone(), Some(result.round as u32)),
    })
}

/// The coding task a message's result row reports: its title and tool head the footer, from the chat's coding sessions
/// (already on screen, so no extra read) or, for a task the chat doesn't list (the group's members start theirs in
/// their own chats, and a chat lists its last 20), what the backend sent with the message. None for a message that
/// has neither.
fn proof_task(message: &Message, sessions: &[CodingSession]) -> Option<ProofTask> {
    use crate::screens::coding::{is_finished, round_state};
    let result = message.coding_result.as_ref().filter(|_| !message.is_user())?;
    // The task's status as its row has it (the chat's sessions are newer than the message), else as the message came.
    let session = sessions.iter().find(|session| session.task_id == result.task_id);
    let (title, tool, status, worked) = match session {
        Some(session) => {
            (session.title.clone(), session.tool.clone(), Some(session.status.as_str()), session.worked_ms)
        }
        None => (result.title.clone()?, result.tool.clone()?, result.task_status.as_deref(), result.worked_ms),
    };
    // A live one's pill weighs its open questions as its row does.
    let state = match session {
        Some(session) if session.is_live() => session.pill(),
        _ => round_state(result.status.as_deref(), status),
    };
    // A finished task's time, as its row says it.
    let label = crate::screens::coding::with_worked(state.1, status.unwrap_or_default(), worked);
    Some(ProofTask { title, tool, state, live: status.is_some_and(|status| !is_finished(status)), label })
}

/// A finished result as the foot of its message's bubble, under a faint line like the work rows': the task's tool mark
/// (Claude Code, Codex, Muse Code or OpenCode), its title and how the round ended (Done, Failed or Stopped; for a round
/// that only waited on background work, what the task came to) on one row, which opens the proof. A task still at work shows its state instead,
/// and the row opens its live thread (`proof_route`). Without the task, a file mark and "Coding result" stand in for them.
fn proof_footer(id: &str, route: Route, task: Option<&ProofTask>, divided: bool, cx: &App) -> impl IntoElement {
    use crate::screens::agent_work::state_pill;
    use crate::screens::coding::{named_tool_tile, task_heading};
    let colors = *cx.colors();
    let (mark, title) = match task {
        Some(task) => (
            named_tool_tile(SharedString::from(format!("proof-tool-{id}")), &task.tool, 16., cx).into_any_element(),
            task_heading(&task.title, &task.tool),
        ),
        None => (
            ui::icon("file-text").size(px(16.)).text_color(colors.text_secondary).into_any_element(),
            "Coding result".into(),
        ),
    };
    let (state, label) = task.map_or(("completed", "Done".to_string()), |task| (task.state.0, task.label.clone()));
    footer_row(
        format!("proof-{id}"),
        mark,
        title.into(),
        Some(state_pill(state, label, true, cx).into_any_element()),
        route,
        divided,
        cx,
    )
}

/// An agent's full report as the foot of its post's bubble, the way a coding result is: a file mark and the report's
/// title on one row, which opens it as a page (coding_result.rs `PostReport`).
fn report_footer(id: &str, title: String, route: Route, divided: bool, cx: &App) -> impl IntoElement {
    let mark = ui::icon("file-text").size(px(16.)).text_color(cx.colors().text_secondary).into_any_element();
    footer_row(format!("report-{id}"), mark, title.into(), None, route, divided, cx)
}

/// The row at the foot of a bubble that opens a result's page (`proof_footer`, `report_footer`): its mark, its title and
/// any pill, under a faint line when something is above it.
fn footer_row(
    id: String,
    mark: AnyElement,
    title: SharedString,
    pill: Option<AnyElement>,
    route: Route,
    divided: bool,
    cx: &App,
) -> impl IntoElement {
    let colors = *cx.colors();
    let row = div()
        .id(SharedString::from(id.clone()))
        .flex()
        .items_center()
        .gap(px(space::SM - 2.))
        .cursor_pointer()
        .active(|style| style.opacity(0.62))
        .child(mark)
        .child(div().flex_1().min_w_0().footnote().semibold().text_color(colors.text).line_clamp(2).child(title))
        .children(pill)
        .on_click(move |_, window, cx| nav::push(route.clone(), window, cx));
    div()
        .when(divided, |this| this.pt(px(space::SM)).border_t_1().border_color(colors.border.opacity(0.6)))
        .child(crate::smoke::tracked(id, row))
}

/// Space above a row: padding, not a margin, which the row's wrapper leaves out of its height (the row would then be
/// drawn that much over the next one).
fn gap_above(content: impl IntoElement, gap: f32) -> AnyElement {
    div().pt(px(gap)).child(content).into_any_element()
}

/// A routine's run as one line in the middle of the chat: "Routine · Check PRs · every 30 minutes", or "nothing to
/// report" in place of the schedule once it ended without sending anything. Its replies, if any, follow as messages.
fn render_routine(id: &str, mark: &RoutineMark, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let detail = if mark.quiet { "nothing to report".to_string() } else { mark.when.clone() };
    let chip = div()
        .id(SharedString::from(format!("routine-{id}")))
        .max_w(relative(0.86))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .px(px(space::MD))
        .py(px(space::XXS))
        .rounded(px(radius::FULL))
        .bg(colors.surface)
        .child(ui::icon_sized("repeat", 13., colors.text_secondary))
        .child(div().flex_none().caption().semibold().text_color(colors.text_secondary).child("Routine"))
        .child(div().min_w_0().caption().text_color(colors.text).truncate().child(mark.title.clone()))
        .when(!detail.is_empty(), |chip| {
            chip.child(div().flex_none().caption().text_color(colors.text_secondary).child(format!("· {detail}")))
        });
    let chip = crate::smoke::tracked(format!("routine-{id}"), chip);
    div().py(px(space::SM)).flex().justify_center().child(chip).into_any_element()
}

/// The typing dots while a reply is under way, and beside them what the agent is doing ("Clicking in Helium"). The
/// dots bounce on the spinners' stepped clock (ui::ticker), not a repeating animation: a reply that works on the
/// computer runs for minutes, and a repeating one redraws the window every frame all that time.
/// What the group says while the routing model picks who answers (local_backend/group.rs `group_turns`).
const ROUTING: &str = "Picking who answers";

/// The routing model at work in the group: a spinner, its tool's mark and "Picking who answers".
fn render_routing(engine: Option<filo_local::Engine>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    div()
        .id("routing")
        .pt(px(space::MD))
        .flex()
        .items_center()
        .gap(px(space::SM))
        .footnote()
        .text_color(colors.text_secondary)
        .child(ui::spinner(13., cx))
        .children(engine.map(|engine| ui::icon_sized(engine_logo(engine), 13., engine_color(engine, cx))))
        .child(ROUTING)
        .into_any_element()
}

fn render_thinking(agent_name: &str, activity: Option<&str>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let still = ui::reduce_motion();
    let now = ui::ticker::seconds();
    div()
        .pt(px(space::MD))
        .flex()
        .items_center()
        .gap(px(space::SM))
        .child(
            div()
                .id("thinking")
                .relative()
                .flex_none()
                .px(px(16.))
                .py(px(14.))
                .rounded(px(20.))
                .bg(colors.assistant_bubble)
                .border_1()
                .border_color(colors.border)
                .flex()
                .gap(px(5.))
                .tooltip({
                    let label = SharedString::from(format!("{agent_name} is thinking"));
                    move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                })
                .children((0..3).map(|index| {
                    let phase = (now / 1.2 - index as f32 * 0.18).rem_euclid(1.);
                    let lift = if still || phase >= 0.35 { 0. } else { (phase / 0.35 * std::f32::consts::PI).sin() };
                    div()
                        .size(px(7.))
                        .rounded(px(radius::FULL))
                        .bg(colors.text_secondary)
                        .mt(px(-4. * lift))
                        .opacity(0.45 + 0.55 * lift)
                }))
                .when(!still, |this| {
                    this.child(
                        gpui::canvas(|_, window, cx| ui::ticker::keep(window, cx), |_, _, _, _| {}).absolute().size_0(),
                    )
                }),
        )
        .when_some(activity, |this, activity| {
            this.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .footnote()
                    .text_color(colors.text_secondary)
                    .truncate()
                    .child(SharedString::from(activity.to_string())),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        CodingSession, Engine, Message, ModelOption, PendingSend, effort_label, message_args, model_label,
        model_row_checked, model_to_send, mosaic_rows, mosaic_sizes, proof_route, proof_task, reasoning_level,
        reasoning_to_send, reasoning_used, with_model,
    };
    use crate::nav::Route;
    use serde_json::json;

    #[test]
    fn retry_errors_belong_only_to_the_failure_that_was_retried() {
        let turn = |id: &str, error: &str| {
            serde_json::from_value(
                json!({ "turnId": id, "requestId": id, "createdAt": 0., "state": "failed", "error": error }),
            )
            .unwrap()
        };
        let earlier = turn("earlier", "Claude Code: session limit");
        let current = turn("current", "Codex: could not reply");
        let retry_error = ("earlier".to_string(), "Retry could not be confirmed.".to_string());
        assert_eq!(super::failure_message(&earlier, Some(&retry_error)), "Retry could not be confirmed.");
        assert_eq!(super::failure_message(&current, Some(&retry_error)), "Codex: could not reply");
        assert_eq!(super::failure_message(&earlier, None), "Claude Code: session limit");
    }

    /// A settled failure's line keeps the error's first paragraph and ends on what went wrong, not on the words that lead
    /// into what the agent wrote instead.
    #[test]
    fn a_settled_line_says_what_went_wrong() {
        let words = |error: Option<&str>| {
            super::settled_words(
                &serde_json::from_value(
                    json!({ "turnId": "t", "requestId": "t", "createdAt": 0., "state": "settled", "error": error }),
                )
                .unwrap(),
            )
        };
        assert_eq!(
            words(Some(
                "Claude Code finished without sending a message with Filo's send_message tool. What it wrote instead:\n\nAll done."
            )),
            "Couldn't reply then: Claude Code finished without sending a message with Filo's send_message tool."
        );
        assert_eq!(
            words(Some("You've hit your session limit · resets 2:50pm\n\nTry again after it resets.")),
            "Couldn't reply then: You've hit your session limit · resets 2:50pm"
        );
        assert_eq!(words(Some("Here's why:\n\nIt broke.")), "Couldn't reply then: Here's why");
        assert_eq!(words(None), "Couldn't reply then.");
    }

    #[test]
    fn agent_names_are_mention_links_outside_code() {
        let members = [
            ("filo".to_string(), "Filo".to_string()),
            ("ann-1".to_string(), "Ann".to_string()),
            ("ann-lee".to_string(), "Ann Lee".to_string()),
        ];
        let chat = gpui::EntityId::from(7u64);
        // The links name the chat by its id.
        let markdown = |text: &str| {
            super::mention_markdown(text, &members, chat).replace(&format!("mention:{}/", chat.as_u64()), "mention:7/")
        };
        assert_eq!(
            markdown("Thanks @Ann Lee and @Ann, ask **@Filo**."),
            "Thanks [@Ann Lee](mention:7/ann-lee) and [@Ann](mention:7/ann-1), ask **[@Filo](mention:7/filo)**."
        );
        // Names in code stay as they are; the lines and lists around them too.
        assert_eq!(
            markdown("- run `@Ann`\n- then @Ann\n```\n@Filo\n```\n@Filo"),
            "- run `@Ann`\n- then [@Ann](mention:7/ann-1)\n```\n@Filo\n```\n[@Filo](mention:7/filo)"
        );
        // A name markdown would read as formatting is escaped; a speaker that can't be an address isn't linked.
        let odd = [("odd-1".to_string(), "R_2*D".to_string()), ("not an id".to_string(), "Bob".to_string())];
        assert_eq!(
            super::mention_markdown("@R_2*D and @Bob", &odd, chat)
                .replace(&format!("mention:{}/", chat.as_u64()), "mention:7/"),
            "[@R\\_2\\*D](mention:7/odd-1) and @Bob"
        );
    }

    fn message(id: &str, role: &str, coding_result: Option<(&str, u32)>) -> Message {
        let mut value = serde_json::json!({ "_id": id, "_creationTime": 1.0, "role": role, "content": "Done." });
        if let Some((task, round)) = coding_result {
            value["codingResult"] = serde_json::json!({ "taskId": task, "round": round });
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn an_agents_chat_points_at_the_group_where_it_asked_or_was_asked() {
        use super::super::feed;
        use super::super::types::GroupLink;
        let link = |kind: &str, names: &[&str]| -> Message {
            let who: Vec<serde_json::Value> =
                names.iter().map(|name| json!({ "speaker": name.to_lowercase(), "name": name })).collect();
            serde_json::from_value(json!({
                "_id": "l1", "_creationTime": 2., "role": "assistant", "content": "Called by Context Engineer in the group: Build it.",
                "groupLink": { "kind": kind, "who": who, "messageId": "g1" },
            }))
            .unwrap()
        };
        let called = link("calledBy", &["Context Engineer"]);
        assert_eq!(called.group_link.as_ref().map(GroupLink::label).as_deref(), Some("Called by Context Engineer"));
        assert_eq!(called.group_link.as_ref().and_then(|link| link.message_id.as_deref()), Some("g1"));
        assert_eq!(
            link("asked", &["Filo", "Vibe Engineer"]).group_link.map(|link| link.label()).as_deref(),
            Some("Asked Filo, Vibe Engineer")
        );
        assert_eq!(link("posted", &[]).group_link.map(|link| link.label()).as_deref(), Some("Posted in the group"));
        // It stands apart from the agent's bubbles around it.
        let reply: Message = serde_json::from_value(
            json!({ "_id": "a1", "_creationTime": 3., "role": "assistant", "content": "On it." }),
        )
        .unwrap();
        assert!(!feed::share_group(&called, &reply));
        assert!(!feed::share_group(&reply, &called));
    }

    #[test]
    fn each_message_opens_the_proof_of_the_round_it_reports() {
        // Two finished rounds in one chat, the older one last in the list: each opens its own.
        let first = message("m1", "assistant", Some(("task-a", 1)));
        let second = message("m2", "assistant", Some(("task-b", 3)));
        assert_eq!(proof_route(&second, None), Some(Route::CodingResult("task-b".into(), Some(3))));
        assert_eq!(proof_route(&first, None), Some(Route::CodingResult("task-a".into(), Some(1))));
    }

    #[test]
    fn a_task_still_at_work_opens_its_thread_and_a_finished_one_its_round() {
        // Round 2 ended waiting on a build; the task then carried on, waits again, and finally finishes.
        let reply = message("m1", "assistant", Some(("task-a", 2)));
        let session = |status: &str| -> Vec<CodingSession> {
            serde_json::from_value(json!([
                { "taskId": "task-a", "tool": "claude-code", "title": "Agents list", "status": status, "now": "", "pending": 0.0, "createdAt": 1.0, "updatedAt": 2.0 },
            ]))
            .unwrap()
        };
        let mut reply_ended = reply.clone();
        reply_ended.coding_result.as_mut().unwrap().status = Some("monitoring".into());
        for (status, state) in [
            ("monitoring", ("monitoring", "Monitoring")),
            ("running", ("active", "Working")),
            ("queued", ("queued", "Waiting")),
        ] {
            let task = proof_task(&reply_ended, &session(status)).unwrap();
            assert_eq!((task.state, task.live), (state, true), "{status}");
            assert_eq!(proof_route(&reply_ended, Some(&task)), Some(Route::CodingTask("task-a".into())));
        }
        let task = proof_task(&reply_ended, &session("done")).unwrap();
        assert!(!task.live);
        assert_eq!(proof_route(&reply_ended, Some(&task)), Some(Route::CodingResult("task-a".into(), Some(2))));
    }

    #[test]
    fn the_proof_names_the_task_it_reports() {
        let sessions: Vec<CodingSession> = serde_json::from_value(json!([
            { "taskId": "task-a", "tool": "claude-code", "title": "Free up disk space", "status": "done", "now": "Done", "pending": 0.0, "createdAt": 1.0, "updatedAt": 2.0 },
            { "taskId": "task-b", "tool": "codex", "title": "Fix the sidebar", "status": "done", "now": "Done", "pending": 0.0, "createdAt": 1.0, "updatedAt": 2.0 },
        ]))
        .unwrap();
        let task = proof_task(&message("m1", "assistant", Some(("task-b", 2))), &sessions).unwrap();
        assert_eq!((task.title.as_str(), task.tool.as_str()), ("Fix the sidebar", "codex"));
        // A task the chat no longer lists, and the user's own message: no heading.
        assert!(proof_task(&message("m2", "assistant", Some(("task-z", 1))), &sessions).is_none());
        assert!(proof_task(&message("m3", "user", Some(("task-a", 1))), &sessions).is_none());
    }

    #[test]
    fn a_task_the_chat_doesnt_list_is_named_by_its_message() {
        // The group's: a member started it in its own chat, so the backend sends its title, tool and states.
        let sent = |status: &str, task_status: &str| {
            let mut value = message("m1", "assistant", Some(("task-g", 1)));
            value.coding_result = serde_json::from_value(json!({
                "taskId": "task-g", "round": 1.0, "title": "Pet tray: fix text overflow", "tool": "claude-code", "status": status, "taskStatus": task_status, "workedMs": 1_080_000.0,
            }))
            .unwrap();
            proof_task(&value, &[]).unwrap()
        };
        let task = sent("done", "done");
        assert_eq!(
            (task.title.as_str(), task.tool.as_str(), task.state),
            ("Pet tray: fix text overflow", "claude-code", ("completed", "Done"))
        );
        // A round that only waited on a build: Monitoring while it waits; once the task carried on and finished, what
        // it came to, like its other rounds' cards.
        assert_eq!(sent("monitoring", "monitoring").state, ("monitoring", "Monitoring"));
        assert_eq!(sent("monitoring", "done").state, ("completed", "Done"));
        assert_eq!(sent("monitoring", "running").state, ("active", "Working"));
        assert_eq!(sent("failed", "failed").state, ("failed", "Failed"));
        // Live while it works, so its row opens the thread.
        assert!(
            sent("monitoring", "monitoring").live
                && sent("done", "waiting_on_you").live
                && !sent("monitoring", "done").live
        );
        assert_eq!(sent("done", "waiting_on_you").state, ("waiting", "Needs you"));
        // A finished task says how long it worked, on a round that only waited on a build too; one at work doesn't.
        assert_eq!(sent("done", "done").label, "Done · 18m");
        assert_eq!(sent("failed", "failed").label, "Failed · 18m");
        assert_eq!(sent("monitoring", "done").label, "Done · 18m");
        assert_eq!(sent("monitoring", "monitoring").label, "Monitoring");
        assert_eq!(sent("done", "running").label, "Working");
    }

    #[test]
    fn a_message_without_a_round_has_no_proof() {
        // Older messages, plain answers and the user's own messages.
        assert_eq!(proof_route(&message("m1", "assistant", None), None), None);
        assert_eq!(proof_route(&message("m2", "user", Some(("task-a", 1))), None), None);
    }

    #[test]
    fn mosaic_rows_never_leave_one_alone_at_the_end() {
        assert_eq!(mosaic_rows(2), vec![2]);
        assert_eq!(mosaic_rows(3), vec![1, 2]);
        assert_eq!(mosaic_rows(4), vec![2, 2]);
        assert_eq!(mosaic_rows(5), vec![3, 2]);
        assert_eq!(mosaic_rows(7), vec![3, 2, 2]);
        assert_eq!(mosaic_rows(9), vec![3, 3, 3]);
    }

    #[test]
    fn mosaic_rows_fill_the_width_and_keep_shapes() {
        // Two 4:3 screenshots side by side.
        let sizes = mosaic_sizes(&[4. / 3., 4. / 3.], 440., 3.);
        assert!((sizes[0].0 + sizes[1].0 + 3. - 440.).abs() < 0.01);
        assert!((sizes[0].0 / sizes[0].1 - 4. / 3.).abs() < 0.01);
        // Three: one across the top, two below, each row as wide as the mosaic.
        let sizes = mosaic_sizes(&[16. / 9., 1., 1.], 440., 3.);
        assert!((sizes[0].0 - 440.).abs() < 0.01);
        assert!((sizes[1].0 + sizes[2].0 + 3. - 440.).abs() < 0.01);
        assert_eq!(sizes[1].1, sizes[2].1);
    }

    #[test]
    fn reasoning_levels_read_in_sentence_case() {
        let labels: Vec<String> =
            ["minimal", "low", "medium", "high", "xhigh", "max"].iter().map(|level| effort_label(level)).collect();
        assert_eq!(labels, ["Minimal", "Low", "Medium", "High", "Extra high", "Max"]);
        assert_eq!(reasoning_level("turbo"), None);
        assert_eq!(reasoning_level("xhigh"), Some("xhigh"));
    }

    #[test]
    fn the_picker_shows_the_level_the_model_actually_uses() {
        let levels = |all: &[&str]| all.iter().map(|level| level.to_string()).collect::<Vec<_>>();
        let codex = levels(&["low", "medium", "high", "xhigh"]);
        assert_eq!(reasoning_used("max", &codex), Some("xhigh"));
        assert_eq!(reasoning_used("medium", &codex), Some("medium"));
        assert_eq!(reasoning_used("high", &levels(&["minimal", "low"])), Some("low"));
        // A model without levels: no picker.
        assert_eq!(reasoning_used("high", &[]), None);
    }

    #[test]
    fn a_message_carries_the_reasoning_level_only_when_the_chat_lacks_it() {
        let pending = PendingSend::new("request-1".into(), "Hi".into(), None, Default::default());
        let args = |reasoning| message_args("chat-1", &pending, reasoning);
        // Nothing picked, or the chat has the pick already: the args stay as they were.
        assert_eq!(reasoning_to_send(None, Some("low")), None);
        assert_eq!(reasoning_to_send(Some("max"), Some("max")), None);
        let plain = args(reasoning_to_send(None, None));
        assert_eq!(plain, json!({ "sessionId": "chat-1", "requestId": "request-1", "message": "Hi" }));
        // A new chat's pick, or one the chat doesn't have yet, goes with the message.
        assert_eq!(args(reasoning_to_send(Some("xhigh"), None))["reasoning"], "xhigh");
        assert_eq!(args(reasoning_to_send(Some("low"), Some("high")))["reasoning"], "low");
        // High picked in a new chat is sent too: the chat keeps it even if the default changes.
        assert_eq!(args(reasoning_to_send(Some("high"), None))["reasoning"], "high");
    }

    #[test]
    fn a_message_carries_the_model_only_when_the_chat_lacks_it() {
        let codex = (Engine::Codex, Some("gpt-a".to_string()));
        let claude_default = (Engine::Claude, None);
        assert_eq!(model_to_send(None, Some(&codex)), None);
        assert_eq!(model_to_send(Some(&codex), Some(&codex)), None);
        assert_eq!(model_to_send(Some(&codex), Some(&claude_default)), Some(codex.clone()));
        // The default engine's own model picked in a new chat is sent too: the chat keeps it if the default changes.
        assert_eq!(model_to_send(Some(&claude_default), None), Some(claude_default.clone()));
        let args = with_model(json!({ "sessionId": "chat-1" }), &codex);
        assert_eq!(args, json!({ "sessionId": "chat-1", "engine": "codex", "model": "gpt-a" }));
        assert_eq!(with_model(json!({}), &claude_default), json!({ "engine": "claude", "model": null }));
    }

    #[test]
    fn the_picker_names_the_model_and_its_default() {
        let row = |engine, id: &str, label: &str, default, aliases: &[&str]| ModelOption {
            engine,
            id: id.into(),
            label: label.into(),
            default,
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            unavailable: None,
        };
        let rows = vec![
            row(Engine::Claude, "claude-opus-5-5", "Opus 5.5", true, &["opus"]),
            row(Engine::Claude, "sonnet", "Sonnet 5.5", false, &["claude-sonnet-5-5"]),
            row(Engine::Codex, "gpt-a", "GPT-A", false, &[]),
        ];
        let label = |engine, model: Option<&str>| {
            model_label(&(engine, model.map(str::to_string)), &rows).map(|label| label.to_string())
        };
        // A chat that never picked: the model the CLI answers with by default, when it said; else no name at all.
        assert_eq!(label(Engine::Claude, None).as_deref(), Some("Opus 5.5"));
        assert_eq!(label(Engine::Codex, None), None);
        // A pick, by any name the CLI takes for it (an alias saved before the default was known).
        assert_eq!(label(Engine::Claude, Some("opus")).as_deref(), Some("Opus 5.5"));
        assert_eq!(label(Engine::Codex, Some("gpt-a")).as_deref(), Some("GPT-A"));
        assert_eq!(label(Engine::Codex, Some("gpt-gone")).as_deref(), Some("gpt-gone"));
        assert!(model_row_checked(&rows[0], &(Engine::Claude, None)));
        assert!(model_row_checked(&rows[0], &(Engine::Claude, Some("opus".into()))));
        assert!(!model_row_checked(&rows[1], &(Engine::Claude, None)));
        assert!(!model_row_checked(&rows[2], &(Engine::Claude, Some("gpt-a".into()))));
    }
}

#[cfg(test)]
mod reply_link_tests {
    use super::{Message, ViewRow, apart};

    fn row(id: &str, role: &str, answers: Option<&str>) -> ViewRow {
        let message: Message = serde_json::from_value(serde_json::json!({ "_id": id, "_creationTime": 1.0, "role": role, "replyToMessageId": answers, "turn": id })).unwrap();
        ViewRow::Message {
            message,
            joins_prev: false,
            joins_next: false,
            late: false,
            stopped: None,
            failed: None,
            sending: None,
            glance: None,
            proof_task: None,
            reply: None,
        }
    }

    #[test]
    fn a_reply_links_what_it_answers_only_past_something_else() {
        // Right after it, or after a time mark: nothing between.
        let rows = [row("q", "user", None), ViewRow::Mark("5:40 PM".into()), row("a", "assistant", Some("q"))];
        assert_eq!(apart(&rows, 2, "q", false), Some(false));
        // An answer in two messages: neither links it.
        let rows = [
            row("q", "user", None),
            row("a1", "assistant", Some("q")),
            ViewRow::Unseen,
            row("a2", "assistant", Some("q")),
        ];
        assert_eq!(apart(&rows, 3, "q", false), Some(false));
        // Another message between them: the answer's first message links it, the rest go on from there.
        let rows = [
            row("q1", "user", None),
            row("q2", "user", None),
            row("a1", "assistant", Some("q1")),
            row("a2", "assistant", Some("q1")),
        ];
        assert_eq!((apart(&rows, 2, "q1", false), apart(&rows, 3, "q1", false)), (Some(true), Some(false)));
        // So does the next answer, to the newest message: the answer to the other one sits between them.
        let rows = [
            row("q1", "user", None),
            row("q2", "user", None),
            row("a1", "assistant", Some("q1")),
            row("b1", "assistant", Some("q2")),
        ];
        assert_eq!(apart(&rows, 3, "q2", false), Some(true));
        // Not the same answer's own glance or message, past a card.
        let rows = [
            row("q", "user", None),
            row("a1", "assistant", Some("q")),
            ViewRow::Routine {
                id: "r".into(),
                mark: super::RoutineMark { title: "Brief".into(), when: String::new(), quiet: false },
            },
            row("a2", "assistant", Some("q")),
        ];
        assert_eq!(apart(&rows, 3, "q", false), Some(false));
        // A card, or the agent's own message, between them: it's still clear what it answers.
        let rows = [
            row("q", "user", None),
            ViewRow::Routine {
                id: "r".into(),
                mark: super::RoutineMark { title: "Brief".into(), when: String::new(), quiet: false },
            },
            row("a", "assistant", Some("q")),
        ];
        assert_eq!(apart(&rows, 2, "q", false), Some(false));
        let rows = [row("q", "user", None), row("g", "assistant", None), row("a", "assistant", Some("q"))];
        assert_eq!(apart(&rows, 2, "q", false), Some(false));
        // The user's reply to the agent's message past the agent's newer one, and past only its own.
        let rows = [row("m1", "assistant", None), row("m2", "assistant", None), row("r", "user", Some("m1"))];
        assert_eq!(apart(&rows, 2, "m1", false), Some(true));
        let rows = [row("m", "assistant", None), row("u", "user", None), row("r", "user", Some("m"))];
        assert_eq!(apart(&rows, 2, "m", false), Some(false));
        // In the group, another member's message between them; the same member's own isn't.
        let said = |id: &str, speaker: &str, answers: Option<&str>| {
            let mut row = row(id, "assistant", answers);
            if let ViewRow::Message { message, .. } = &mut row {
                message.speaker = Some(speaker.into());
            }
            row
        };
        let rows = [row("q", "user", None), said("b", "bea", None), said("a", "ann", Some("q"))];
        assert_eq!(apart(&rows, 2, "q", false), Some(true));
        let rows = [row("q", "user", None), said("a0", "ann", None), said("a", "ann", Some("q"))];
        assert_eq!(apart(&rows, 2, "q", false), Some(false));
        // A coding task's result after the replier's "Started" and its task card: in a chat, as any reply; in the group,
        // it links the ask.
        let result = |id: &str, turn: &str| {
            let mut row = said(id, "ann", Some("q"));
            if let ViewRow::Message { message, .. } = &mut row {
                message.coding_result = serde_json::from_value(serde_json::json!({ "taskId": "t", "round": 1.0 })).ok();
                message.turn = Some(turn.into());
            }
            row
        };
        let card = || ViewRow::Routine {
            id: "c".into(),
            mark: super::RoutineMark { title: "Task".into(), when: String::new(), quiet: false },
        };
        let rows = [row("q", "user", None), said("a", "ann", Some("q")), card(), result("r", "news")];
        assert_eq!((apart(&rows, 3, "q", false), apart(&rows, 3, "q", true)), (Some(false), Some(true)));
        // Past the user's newer message, in a chat too.
        let rows = [row("q", "user", None), said("a", "ann", Some("q")), row("u", "user", None), result("r", "news")];
        assert_eq!(apart(&rows, 3, "q", false), Some(true));
        // In the group, not right under the ask, nor going on from its own first message.
        let rows = [row("q", "user", None), result("r", "news")];
        assert_eq!(apart(&rows, 1, "q", true), Some(false));
        let mut first = said("r1", "ann", Some("q"));
        if let ViewRow::Message { message, .. } = &mut first {
            message.turn = Some("news".into());
        }
        let rows = [row("q", "user", None), said("a", "ann", Some("q")), first, result("r2", "news")];
        assert_eq!((apart(&rows, 2, "q", true), apart(&rows, 3, "q", true)), (Some(false), Some(false)));
        // Not loaded, or only below it: nothing to link.
        let rows = [row("a", "assistant", Some("q")), row("q", "user", None)];
        assert_eq!(apart(&rows, 0, "q", false), None);
    }
}

#[cfg(test)]
mod quote_tests {
    use super::{quote_block, quote_gap};

    #[test]
    fn selected_words_quote_line_by_line() {
        assert_eq!(quote_block("  the runner  "), "> the runner\n\n");
        assert_eq!(quote_block("first line\n\nthird line"), "> first line\n>\n> third line\n\n");
        assert_eq!(quote_block(" \n "), "");
    }

    #[test]
    fn a_double_click_selects_the_word_under_it() {
        use gpui_component::text::select::word_at;
        let text = "Pushed fd806ce6, can't stop  now.\nNext";
        let word = |ix: usize| &text[word_at(text, ix)];
        assert_eq!(word(9), "fd806ce6");
        assert_eq!(word(18), "can't");
        assert_eq!(word(15), ",");
        assert_eq!(word(28), "  ");
        // Past a line's end: its last word, not the next line's.
        assert_eq!(word(33), ".");
        assert_eq!(word(text.len()), "Next");
    }

    #[test]
    fn a_selection_runs_along_the_lines_between_its_ends() {
        use gpui::{point, px};
        use gpui_component::text::select::char_selected;
        let (width, line) = (px(10.), px(20.));
        // From the middle of the first line to the middle of the third, dragged either way.
        let (a, b) = (point(px(100.), px(10.)), point(px(50.), px(50.)));
        let selected = |x: f32, y: f32| {
            char_selected(point(px(x), px(y)), width, line, a, b)
                && char_selected(point(px(x), px(y)), width, line, b, a)
        };
        assert!(selected(120., 0.) && !selected(60., 0.));
        assert!(selected(0., 20.) && selected(400., 20.));
        assert!(selected(10., 40.) && !selected(80., 40.));
        assert!(!selected(0., 60.));
    }

    #[test]
    fn punctuation_after_code_or_a_link_stays_with_it() {
        use gpui_component::text::wrap::kept_with;
        // "(`f6d565a`, `0959ce6`), see [notes](…))?\" done" as drawn: code at 1..8 and 10..17, a link at 24..29.
        let text = "(f6d565a, 0959ce6), see notes)?\" done";
        let (code, link) = ([1..8, 10..17], 24..29);
        let spans = [code[0].clone(), code[1].clone(), link];
        assert_eq!(kept_with(text, &spans, &code), vec![17, 29, 30, 31]);
        // A mark that is code itself is left alone, and so is one after nothing.
        assert!(kept_with("a)", &[0..1, 1..2], &[0..1, 1..2]).is_empty());
        assert!(kept_with("a)", &[1..1], &[]).is_empty());
    }

    #[test]
    fn underlines_and_chips_go_a_piece_per_line() {
        use gpui::{point, px};
        use gpui_component::text::wrap::pieces;
        // "the notes and more", 10 points a character, wrapped after "the notes ": gpui puts offset 10, where the
        // second line starts, at the end of the first.
        let text = "the notes and more";
        let position = |ix: usize| {
            Some(if ix <= 10 { point(px(ix as f32 * 10.), px(0.)) } else { point(px((ix - 10) as f32 * 10.), px(20.)) })
        };
        let pieces = |range| pieces(text, range, position, px(0.));
        // Across the wrap: the space it ends the first line with is left out.
        assert_eq!(pieces(4..13), vec![(px(40.), px(90.), px(0.)), (px(0.), px(30.), px(20.))]);
        // Starting where the line wraps: only on the second line, nothing at the first one's end.
        assert_eq!(pieces(10..13), vec![(px(0.), px(30.), px(20.))]);
        // Ending where it wraps: only on the first.
        assert_eq!(pieces(4..9), vec![(px(40.), px(90.), px(0.))]);
    }

    #[test]
    fn a_quote_goes_a_blank_line_after_what_is_typed() {
        assert_eq!(quote_gap(""), "");
        assert_eq!(quote_gap("  "), "");
        assert_eq!(quote_gap("about this"), "\n\n");
        assert_eq!(quote_gap("about this\n"), "\n");
        assert_eq!(quote_gap("about this\n\n"), "");
    }
}

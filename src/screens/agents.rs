// Agents (design/filo-desktop-agents.html): the agents the user made on the left, and on the right
// the open one's conversation, which is a Filo chat that answers as that agent. Each agent has one conversation. One of
// them is the main agent (an agent's menu: right-click its row; its row says Main): it leads the group, answers Ask on Feed posts and owns
// what's no agent's; deleting it asks for another main agent first. A new
// agent or an edit opens the form in the right pane; the picture picks from the animals, the helpers and the user's
// own avatars, and the model picks who answers as the agent in every chat with it. Above them is the group, where the
// agents talk among themselves and the user reads along. It's optional: off, its row says Off and its page is its
// setup (`GroupSetup`, onboarding's dots and buttons with the Edit agent page's picture, colour and name): its look
// and members, then the model that picks who answers; on, its chat, whose header and ⋯ open Group settings (the same
// two steps) and ⋯ turns it off. The list is as wide as the user
// drags its right edge (kept in desktop-agents.json).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DragMoveEvent, Entity, Focusable as _, ImageSource,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, Resource, ScrollHandle,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::Button as MenuButton;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{Sizable as _, WindowExt as _};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::avatars::{self, ANIMALS, CustomAvatar};
use crate::backend;
use crate::format;
use crate::screens::agent_picture;
use crate::screens::chat::conversation::{self, Conversation, ConversationEvent, ModelChoice};
use crate::screens::chat::drafts::{self, Drafts};
use crate::screens::chat::shared::SharedPanel;
use crate::screens::chat::{Mention, reply_status, working_panel};
use crate::screens::routine_display::{Routine, format_routine_schedule};
use crate::theme::{ActiveTheme as _, HEADER_HEIGHT, TypeVariant, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};

/// The group's place in the list and in `selected`, and what a banner for it opens.
pub const GROUP: &str = "group";
/// The group's own picture (`group_picture`) as the form and the picture sheet hold it; saved as none.
pub const GROUP_PICTURE: &str = "group";

/// The list's width until the user drags its edge, and how narrow or wide it can be dragged.
const LIST_WIDTH: f32 = 280.;
const LIST_MIN: f32 = 220.;
const LIST_MAX: f32 = 440.;
/// The pane right of the list keeps at least this much: a narrow window narrows the list (down to `LIST_MIN`) first.
const PANE_MIN: f32 = 440.;
/// The ⋯ at the end of a chat's header (a small icon button), with a little to spare.
const AGENT_MENU_WIDTH: f32 = 28.;
const ROW_PICTURE: f32 = 40.;
pub const NAME_MAX: usize = 32;
pub const INSTRUCTIONS_MAX: usize = 8_000;
/// The form's column, padding included.
const FORM_WIDTH: f32 = 620.;
/// Older shared items load once the form page is scrolled this close to its end.
const SHARED_NEAR_END: f32 = 600.;
/// Conversations kept live, the ones shown last; older ones are parked (Conversation::park: no queries, but what
/// they showed and where), and past `MAX_KEPT` close and load again when the agent is opened again.
const MAX_OPEN: usize = 6;
const MAX_KEPT: usize = 12;

/// The helpers' pictures, embedded as `workers/<name>.png`.
const HELPERS: [&str; 12] =
    ["sunny", "indi", "milo", "mint", "mochi", "peach", "poppy", "rosie", "coco", "aqua", "blu", "vivi"];

/// The colours offered behind an agent's picture, in the profile's colour popover; any other `#RRGGBB` (one picked on
/// the hue slider) shows as the custom one.
const BACKGROUNDS: [&str; 10] =
    ["#E26B9C", "#E5574F", "#EC8E45", "#E2B54A", "#5CB87A", "#4FB3B8", "#4F7FE6", "#8A6CF0", "#3A3F4B", "#26221F"];
/// A colour on the hue slider: its hue, at this saturation and lightness.
const HUE_SATURATION: f32 = 0.62;
const HUE_LIGHTNESS: f32 = 0.6;
/// The profile's picture.
const PROFILE_PICTURE: f32 = 76.;

/// Starting points for the instructions, offered while they're empty.
const TEMPLATES: [(&str, &str); 4] = [
    (
        "Tutor",
        "You are a patient tutor. Find out what I already know, explain one idea at a time with a short example, then check my understanding with a quick question. Correct mistakes kindly and say why.",
    ),
    (
        "Coach",
        "You are an encouraging coach. Help me set a small, concrete next step, ask how the last one went, and keep me honest without lecturing. Keep replies short.",
    ),
    (
        "Researcher",
        "You research questions thoroughly. Search the web, compare sources, and answer with a short summary first, then the details with links. Say when something is uncertain.",
    ),
    (
        "Editor",
        "You edit my writing. Keep my voice, cut filler, fix grammar and make it clearer. Show the edited version first, then the main changes in a few bullets.",
    ),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    pub picture: String,
    /// The colour behind the picture (`#RRGGBB`), when the user chose one.
    pub background: Option<String>,
    pub instructions: String,
    pub session_id: Option<String>,
    /// The model picked for it (`model` none: the engine's own default); no engine for an agent from before there
    /// was a pick, which answers with the app's default.
    #[serde(default)]
    pub engine: Option<filo_local::Engine>,
    #[serde(default)]
    pub model: Option<String>,
    /// The engine its conversation is tied to once it has a session: the profile offers that engine's models only, and
    /// the other engines by duplicating it.
    #[serde(default)]
    pub locked_engine: Option<filo_local::Engine>,
    pub last: Option<LastMessage>,
    pub unseen: f64,
    /// A reply in its chat failed and hasn't gone through since.
    #[serde(default)]
    pub reply_failed: bool,
    /// An unanswered question or permission card, here or on a coding task this agent started.
    #[serde(default)]
    pub needs_input: bool,
    /// The main agent: it leads the group, answers Ask on Feed posts and owns what's no agent's (`users:setMainAgent`).
    #[serde(default)]
    pub main: bool,
    /// Saved by a newer Filo on an engine this one doesn't know: listed, but not changed or talked to here.
    #[serde(default)]
    pub needs_newer_filo: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastMessage {
    pub from_user: bool,
    pub preview: String,
    pub at: f64,
}

impl AgentRow {
    pub fn image(&self) -> ImageSource {
        picture_source(&self.picture)
    }

    /// Its picture on its round tile, in its own colour.
    pub fn tile(&self, size: f32, cx: &App) -> gpui::Div {
        agent_picture(self.image(), self.background.as_deref(), size, cx)
    }
}

/// `agents:canCreate`: whether a new agent can be made. Agents answer with a coding tool, so one of them has to be
/// plugged in on the Coding page; `reason` says what to do when none is.
#[derive(Clone, Debug, Deserialize)]
pub struct CanCreate {
    pub allowed: bool,
    pub reason: Option<String>,
}

/// Why a new agent can't be made now, if it can't. Until the answer comes, it can (`agents:create` checks again).
fn creation_blocked(can_create: Option<&CanCreate>) -> Option<String> {
    let can_create = can_create.filter(|can_create| !can_create.allowed)?;
    Some(can_create.reason.clone().unwrap_or_else(|| {
        "Plug in a coding tool (Claude Code, Codex, Muse Code or OpenCode) on the Coding page to create an agent."
            .into()
    }))
}

/// Why New agent didn't open the form, with the way to the Coding page, where the tools are plugged in.
fn explain_blocked(reason: String, window: &mut Window, cx: &mut App) {
    ui::confirm(window, cx, "Plug in a coding tool first", reason, "Open Coding", "Not now", false, |window, cx| {
        crate::nav::open_tab(crate::nav::Tab::Coding, window, cx);
    });
}

/// `agents:group`: the group chat of Filo and the agents.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupRow {
    pub session_id: String,
    pub last: Option<GroupLast>,
    pub unseen: f64,
    #[serde(default)]
    pub reply_failed: bool,
    #[serde(default)]
    pub needs_input: bool,
    pub members: Vec<GroupMember>,
    /// Who leads: `filo` or the main agent's id.
    #[serde(default)]
    pub lead: Option<String>,
    /// Filo is in the group (while it's an agent, or leads).
    #[serde(default = "yes")]
    pub filo: bool,
    /// Its name, picture and colour as the user set them (`agents:updateGroup`); none: "Group", the Agents tab's faces
    /// and the theme's colour.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub picture: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
}

impl GroupRow {
    /// Its name, or "Group" while it has none.
    pub fn title(&self) -> String {
        group_title(self.name.as_deref())
    }

    /// Its picture on its round tile, in its colour.
    pub fn tile(&self, size: f32, cx: &App) -> gpui::Div {
        agent_picture(
            picture_source(self.picture.as_deref().unwrap_or(GROUP_PICTURE)),
            self.background.as_deref(),
            size,
            cx,
        )
    }
}

/// The group's name as shown: the user's, or "Group".
pub fn group_title(name: Option<&str>) -> String {
    name.map(str::trim).filter(|name| !name.is_empty()).unwrap_or(filo_local::group::DEFAULT_NAME).to_string()
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupLast {
    pub speaker: String,
    pub preview: String,
    pub at: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMember {
    pub id: String,
    pub name: String,
    pub picture: String,
    pub background: Option<String>,
}

/// `agents:groupSetup`: how the group is set (local_backend/group.rs): on or off, its look, members and routing model.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSettingsRow {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub picture: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    /// Every agent is in it, the ones made later too.
    #[serde(default = "yes")]
    pub all: bool,
    /// Its members by agent id.
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub route: Option<RouteModel>,
}

/// A coding tool's model, by the id its CLI takes: the one that picks who answers in the group, and how hard it thinks
/// (none: Low, `filo_local::group_route::DEFAULT_REASONING`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RouteModel {
    pub engine: filo_local::Engine,
    pub model: String,
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// `agents:routingModels`: whether a coding tool is signed in, and the routing model used when none is picked.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoutingModels {
    signed_in: bool,
    #[serde(default)]
    default: Option<RouteModel>,
}
/// The setup's column.
const SETUP_WIDTH: f32 = 560.;

/// The group's setup while it's off, or its Group settings once it's on: the same pieces. The setup's step 0: its
/// picture, colour and name, and who's in it; step 1: the model that picks who answers. Group settings has them all on
/// one page (`settings_page`).
struct GroupSetup {
    /// Group settings (Cancel and Save), or the setup (Turn on).
    settings: bool,
    step: usize,
    name: Entity<InputState>,
    look: ProfilePicture,
    /// Every agent, or `members`.
    all: bool,
    members: Vec<String>,
    /// The routing model picked; none: the one `RoutingModels` names.
    route: Option<RouteModel>,
    /// The reasoning levels the routing model takes (`route_levels`): for which model, and when they were asked.
    levels: Option<(String, std::time::Instant, Vec<String>)>,
    saving: bool,
    /// What the group shared, under Group settings (the setup has none), and the page's scroll, near whose end it loads
    /// older items.
    shared: Option<Entity<SharedPanel>>,
    scroll: ScrollHandle,
    _subs: [Subscription; 2],
}

/// `agents:scheduledCount`: the pending reminders and the routines an agent owns.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct ScheduledCount {
    reminders: f64,
    routines: f64,
}

/// "2 reminders and 1 routine".
fn count_text(count: ScheduledCount) -> String {
    let part = |n: f64, word: &str| format!("{n} {word}{}", if n == 1. { "" } else { "s" });
    match (count.reminders > 0., count.routines > 0.) {
        (true, true) => format!("{} and {}", part(count.reminders, "reminder"), part(count.routines, "routine")),
        (true, false) => part(count.reminders, "reminder"),
        _ => part(count.routines, "routine"),
    }
}

/// The group's picture: the three faces of the Agents tab.
pub fn group_picture() -> ImageSource {
    ImageSource::Resource(Resource::Embedded("images/tab-agents.png".into()))
}

/// An agent's picture: an animal, a helper (`helper:<name>`), an upload, one of the user's own avatars (`custom:<id>`);
/// or the group's own.
pub fn picture_source(picture: &str) -> ImageSource {
    if picture == GROUP_PICTURE {
        return group_picture();
    }
    if avatars::character_pose(picture).is_some() {
        return avatars::avatar_source(picture);
    }
    if let Some(id) = crate::local_backend::upload_id_of(picture) {
        return ImageSource::Resource(Resource::Path(crate::local_backend::uploaded_picture_path(id).into()));
    }
    if let Some(name) = picture.strip_prefix("helper:") {
        return ImageSource::Resource(Resource::Embedded(format!("workers/{name}.png").into()));
    }
    if avatars::is_avatar_id(picture) {
        return avatars::animal_image(picture);
    }
    avatars::animal_image(avatars::DEFAULT_AVATAR)
}

/// A picture on a round tile, like the avatars elsewhere.
pub fn round_picture(source: ImageSource, size: f32, cx: &App) -> gpui::Div {
    agent_picture(source, None, size, cx)
}

/// An agent's picture on its round tile, in the colour the user chose for it (`#RRGGBB`) or else the theme's.
pub fn agent_picture(source: ImageSource, background: Option<&str>, size: f32, cx: &App) -> gpui::Div {
    picture_on(source, background_color(background), size, cx)
}

/// `agent_picture` with the colour already read (None for the theme's).
pub fn picture_on(source: ImageSource, background: Option<gpui::Hsla>, size: f32, cx: &App) -> gpui::Div {
    // A picture the user uploaded is a photo already cut round (agent_picture.rs): it fills the tile.
    let uploaded = matches!(&source, ImageSource::Resource(Resource::Path(_)));
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(radius::FULL))
        .bg(background.unwrap_or(cx.colors().surface_strong))
        .flex()
        .items_center()
        .justify_center()
        .child(avatars::picture(source, if uploaded { size } else { size * 0.86 }))
}

/// The coding tool a list row's agent answers with, at the right end of its second line, under the time: the tool's
/// bare mark, no tile, in a muted grey, or on the open agent's row in the tool's own colour (as on the Coding page).
/// Hovering it names the tool. `opacity` fades it in and out as the list's ⌄ menu turns the marks on and off; at 0
/// there's no mark.
fn tool_mark(
    id: &str,
    engine: filo_local::Engine,
    opacity: f32,
    active: bool,
    cx: &App,
) -> Option<gpui::Stateful<gpui::Div>> {
    const MARK: f32 = 13.;
    let colors = cx.colors();
    let color = if active { conversation::engine_color(engine, cx) } else { colors.text_secondary };
    (opacity > 0.).then(|| {
        div()
            .id(SharedString::from(format!("agent-tool-{id}")))
            .flex_none()
            .opacity(opacity)
            .child(ui::icon_sized(conversation::engine_logo(engine), MARK, color))
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(engine.label()).build(window, cx))
    })
}

/// A list row's time: faint, or in the accent and semibold while the chat has messages unread (there's no dot).
fn list_time(time: String, unseen: bool, colors: &crate::theme::Palette) -> gpui::Div {
    let time = div().flex_none().caption().child(time);
    if unseen { time.semibold().text_color(colors.accent) } else { time.text_color(colors.text_tertiary) }
}

/// A background as saved, `#RRGGBB`, from what was typed (the `#` may be left out); None when it isn't a colour.
pub fn parse_background(value: &str) -> Option<String> {
    let hex = value.trim().trim_start_matches('#');
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", hex.to_ascii_uppercase()))
}

/// The colour of a saved background; None leaves the theme's.
pub fn background_color(background: Option<&str>) -> Option<gpui::Hsla> {
    background.and_then(parse_background).map(|hex| ui::hex_color(Some(&hex), gpui::transparent_black()))
}

/// A picked colour as saved, `#RRGGBB` (rounded, not cut down like `Colorize::to_hex`, so a saved colour picks back
/// the same; alpha is dropped).
fn background_hex(color: gpui::Hsla) -> String {
    let rgb = color.to_rgb();
    let byte = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02X}{:02X}{:02X}", byte(rgb.r), byte(rgb.g), byte(rgb.b))
}

/// The colour at `hue` (0–360) on the hue slider, as saved.
fn hue_background(hue: f32) -> String {
    background_hex(gpui::hsla(hue.rem_euclid(360.) / 360., HUE_SATURATION, HUE_LIGHTNESS, 1.))
}

/// Where the hue slider starts for a saved colour: its own hue, or a pink for none.
fn hue_of(background: Option<&str>) -> f32 {
    background_color(background).map_or(330., |color| (color.h * 360.).round())
}

/// A colour that isn't one of the offered ones: the rainbow swatch is the picked one, with the hue slider out.
fn is_custom(background: Option<&str>) -> bool {
    background.is_some_and(|hex| !BACKGROUNDS.contains(&hex))
}

/// Where the picture sheet's Done goes for a picture that isn't the form's (the welcome's first agent).
pub type PictureDone = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// How a view reaches the `ProfilePicture` it shows, for the picture's clicks and the hue slider.
pub type HoldsPicture<T> = fn(&mut T) -> Option<&mut ProfilePicture>;

/// An agent's picture and the colour behind it, as the profile edits them: the picture with its ✎, and the colour
/// popover a click on it opens (the offered colours, any other on the hue slider, and the way to another picture). The
/// Edit agent page's, and the welcome's first agent's (screens/welcome.rs).
pub struct ProfilePicture {
    pub picture: String,
    /// None for the theme's.
    pub background: Option<String>,
    /// The colour popover under the picture, whether its hue slider is out, and the slider (moving it sets
    /// `background`).
    colors_open: bool,
    hue_open: bool,
    hue: Entity<SliderState>,
    /// Where the picture is: a click on it while the popover is open closes the popover rather than reopening it.
    bounds: Rc<Cell<gpui::Bounds<gpui::Pixels>>>,
}

impl ProfilePicture {
    /// Its state, and the hue slider's subscription for the view to keep.
    pub fn new<T: 'static>(
        picture: String,
        background: Option<String>,
        holds: HoldsPicture<T>,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> (Self, Subscription) {
        let hue =
            cx.new(|_| SliderState::new().min(0.).max(360.).step(1.).default_value(hue_of(background.as_deref())));
        let sub = cx.subscribe_in(&hue, window, move |this, _, event: &SliderEvent, _, cx| {
            let SliderEvent::Change(value) = event;
            if let Some(look) = holds(this) {
                look.background = Some(hue_background(value.start()));
                cx.notify();
            }
        });
        let look = ProfilePicture {
            hue_open: is_custom(background.as_deref()),
            picture,
            background,
            colors_open: false,
            hue,
            bounds: Rc::default(),
        };
        (look, sub)
    }

    /// Back to `picture` on `background`, the popover closed.
    pub fn reset(&mut self, picture: String, background: Option<String>, window: &mut Window, cx: &mut App) {
        self.hue.update(cx, |hue, cx| hue.set_value(hue_of(background.as_deref()), window, cx));
        self.hue_open = is_custom(background.as_deref());
        self.colors_open = false;
        self.picture = picture;
        self.background = background;
    }

    pub fn colors_open(&self) -> bool {
        self.colors_open
    }

    /// Opens or closes the colour popover; whether that changed anything.
    pub fn set_colors_open(&mut self, open: bool) -> bool {
        let changed = self.colors_open != open;
        self.colors_open = open;
        changed
    }

    /// One of the offered colours, or None for the theme's (Reset to default); the hue slider goes away.
    fn pick_background(&mut self, background: Option<String>) {
        self.background = background;
        self.hue_open = false;
    }

    /// The rainbow swatch: the hue slider comes out, and its colour is the picture's.
    fn pick_custom(&mut self, cx: &App) {
        self.hue_open = true;
        self.background = Some(hue_background(self.hue.read(cx).value().start()));
    }

    /// The picture (`source`) in its colour with its ✎; a click opens the colour popover under it, whose Change
    /// picture… runs `change_picture`.
    pub fn render<T: 'static>(
        &self,
        source: ImageSource,
        holds: HoldsPicture<T>,
        change_picture: fn(&mut T, &mut Window, &mut Context<T>),
        cx: &mut Context<T>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let label = |text: &str| div().footnote().semibold().text_color(colors.text_secondary).child(text.to_string());
        let picture_bounds = self.bounds.clone();
        let picture = div()
            .id("agent-picture")
            .relative()
            .flex_none()
            .cursor_pointer()
            .child(agent_picture(source, self.background.as_deref(), PROFILE_PICTURE, cx))
            .child(
                div()
                    .absolute()
                    .right(px(-2.))
                    .bottom(px(-2.))
                    .size(px(26.))
                    .rounded(px(radius::FULL))
                    .bg(colors.raised)
                    .border_2()
                    .border_color(colors.background)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::icon("pencil").size(px(12.)).text_color(colors.text)),
            )
            .child({
                let bounds = picture_bounds.clone();
                gpui::canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(look) = holds(this) {
                    look.colors_open = !look.colors_open;
                    cx.notify();
                }
            }));
        // The colours: the offered ones, the rainbow one with its hue slider for any other, back to the theme's, and the
        // way to another picture.
        let current = self.background.clone();
        let custom = self.hue_open || is_custom(current.as_deref());
        let ring = |selected: bool| {
            div()
                .size(px(28.))
                .flex_none()
                .p(px(2.))
                .rounded(px(radius::FULL))
                .border_2()
                .border_color(if selected { colors.text } else { gpui::transparent_black() })
                .cursor_pointer()
        };
        let swatches = BACKGROUNDS.iter().map(|hex| {
            let picked = Some(hex.to_string());
            ring(!custom && current.as_deref() == Some(*hex))
                .id(SharedString::from(format!("background-{hex}")))
                .child(
                    div()
                        .size_full()
                        .rounded(px(radius::FULL))
                        .bg(background_color(Some(hex)).unwrap_or(colors.surface_strong)),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(look) = holds(this) {
                        look.pick_background(picked.clone());
                        cx.notify();
                    }
                }))
        });
        let rainbow = ring(custom)
            .id("background-custom")
            .child(rainbow_dot())
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Any colour").build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(look) = holds(this) {
                    look.pick_custom(cx);
                    cx.notify();
                }
            }));
        let hue_slider = self.hue_open.then(|| {
            let color = |degrees: f32| gpui::hsla(degrees.rem_euclid(360.) / 360., HUE_SATURATION, HUE_LIGHTNESS, 1.);
            let segment = |index: usize| {
                let (from, to) = (index as f32 * 60., (index + 1) as f32 * 60.);
                div()
                    .flex_1()
                    .h_full()
                    .bg(gpui::linear_gradient(
                        90.,
                        gpui::linear_color_stop(color(from), 0.),
                        gpui::linear_color_stop(color(to), 1.),
                    ))
                    .when(index == 0, |this| this.rounded_l(px(radius::FULL)))
                    .when(index == 5, |this| this.rounded_r(px(radius::FULL)))
            };
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h(px(24.))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .top(px(7.))
                                .h(px(10.))
                                .flex()
                                .children((0..6).map(segment)),
                        )
                        .child(Slider::new(&self.hue).bg(gpui::transparent_black()).text_color(gpui::white())),
                )
                .child(div().footnote().text_color(colors.text_secondary).child("Custom"))
        });
        let colors_popover = self.colors_open.then(|| {
            let bounds = picture_bounds.clone();
            let list = div()
                .id("agent-colors")
                .occlude()
                .mt(px(space::SM))
                .w(px(11. * 28. + 10. * space::XXS + 2. * space::MD))
                .p(px(space::MD))
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .rounded(px(radius::MD))
                .bg(colors.raised)
                .border_1()
                .border_color(colors.border)
                .shadow_lg()
                // The scroll wheel over it doesn't close it (it's one menu of escape.rs's popups).
                .relative()
                .child(gpui_component::one_menu::surface())
                .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    // The picture toggles it itself.
                    if !bounds.get().contains(&event.position)
                        && holds(this).is_some_and(|look| look.set_colors_open(false))
                    {
                        cx.notify();
                    }
                }))
                .child(div().flex().items_center().h(px(28.)).child(div().flex_1().child(label("Colour"))).when(
                    current.is_some(),
                    |this| {
                        this.child(div().mr(px(-(space::MD - 2.))).child(
                            Button::ghost("reset-background", "Reset to default").small().on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    if let Some(look) = holds(this) {
                                        look.pick_background(None);
                                        cx.notify();
                                    }
                                },
                            )),
                        ))
                    },
                ))
                .child(div().flex().gap(px(space::XXS)).children(swatches).child(rainbow))
                .children(hue_slider)
                .child(div().h(px(1.)).bg(colors.border))
                .child(div().ml(px(-(space::MD - 2.))).child(
                    Button::ghost("agent-change-picture", "Change picture…").small().icon("image").on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if let Some(look) = holds(this) {
                                look.colors_open = false;
                            }
                            change_picture(this, window, cx);
                            cx.notify();
                        }),
                    ),
                ));
            div().absolute().left_0().bottom_0().child(
                gpui::deferred(
                    gpui::anchored().anchor(gpui::Corner::TopLeft).snap_to_window_with_margin(px(8.)).child(list),
                )
                .with_priority(1),
            )
        });
        div().relative().flex_none().child(picture).children(colors_popover).into_any_element()
    }
}

/// An agent's name, as large text that edits in place.
pub fn name_field(input: &Entity<InputState>, window: &Window, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    div()
        .id("agent-name")
        .ml(px(-7.))
        .rounded(px(radius::XS))
        .border_1()
        .border_color(if focused { colors.accent.opacity(0.6) } else { gpui::transparent_black() })
        .when(focused, |this| this.bg(colors.surface))
        .when(!focused, |this| this.hover(|style| style.border_color(colors.border)))
        .child(gpui::Styled::h(
            Input::new(input)
                .appearance(false)
                .px(px(6.))
                .py_0()
                .text_size(px(22.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .line_height(px(30.)),
            px(36.),
        ))
        .into_any_element()
}

/// The name's field, empty.
pub fn name_input(window: &mut Window, cx: &mut Context<InputState>) -> InputState {
    InputState::new(window, cx).placeholder("Name your agent")
}

/// The instructions' field, empty: it grows as they're typed, and the page scrolls rather than the field.
pub fn instructions_input(window: &mut Window, cx: &mut Context<InputState>) -> InputState {
    InputState::new(window, cx).multi_line(true).auto_grow(6, 400).placeholder("What it does and how it talks to you")
}

/// Who answers as the agent, by its provider's logo and its name (the same models as the Chats composer's picker), and
/// the tool it answers on. `locked` keeps the picker to that engine's models; `after` goes right after its chip (the
/// group's routing step's reasoning picker).
pub fn model_line<T: 'static>(
    model: &ModelChoice,
    locked: Option<filo_local::Engine>,
    after: Option<AnyElement>,
    pick: fn(&mut T, ModelChoice, &mut Context<T>),
    cx: &mut Context<T>,
) -> gpui::Div {
    let colors = *cx.colors();
    let model_rows = std::sync::Arc::new(
        conversation::model_rows(cx)
            .iter()
            .filter(|row| locked.is_none_or(|engine| row.engine == engine))
            .cloned()
            .collect::<Vec<_>>(),
    );
    let model_picker = {
        let entity = cx.entity().downgrade();
        let (model, model_rows) = (model.clone(), model_rows.clone());
        let button = MenuButton::new("agent-model")
            .outline()
            .small()
            .rounded(px(radius::FULL))
            .child(ui::icon_sized(conversation::engine_logo(model.0), 13., colors.text_secondary))
            .children(conversation::model_label(&model, &conversation::all_model_rows(cx)))
            .dropdown_caret(true);
        let picker = conversation::model_picker(
            "agent-model-menu",
            button,
            gpui::Corner::TopLeft,
            model_rows,
            &model,
            move |choice, cx| {
                entity.update(cx, |this, cx| pick(this, choice, cx)).ok();
            },
        );
        crate::smoke::tracked("agent-model", picker)
    };
    let engine =
        div().footnote().text_color(colors.text_secondary).child(format!("on {}", locked.unwrap_or(model.0).label()));
    div().flex().items_center().gap(px(space::SM)).child(model_picker).children(after).child(engine)
}

/// What the form was opened with or last saved: the save bar shows while the form differs from it, and Revert goes
/// back to it.
#[derive(Clone)]
struct Saved {
    name: String,
    instructions: String,
    picture: String,
    background: Option<String>,
    model: ModelChoice,
    model_picked: bool,
}

impl Saved {
    /// Whether `other` has something to save over this (picking the model it already has isn't a change).
    fn differs(&self, other: &Saved) -> bool {
        self.name != other.name
            || self.instructions != other.instructions
            || self.picture != other.picture
            || self.background != other.background
            || self.model != other.model
    }
}

struct Form {
    /// None for a new agent.
    id: Option<String>,
    name: Entity<InputState>,
    instructions: Entity<InputState>,
    /// The picture, the colour behind it and the colour popover.
    look: ProfilePicture,
    /// Who answers as the agent, in every chat with it.
    model: ModelChoice,
    /// Whether `model` is saved: always for a new agent; for an edited one, once it's picked (one from before there
    /// was a pick keeps following the app's default until then).
    model_picked: bool,
    /// The engine an edited agent's conversation is tied to (`AgentRow::locked_engine`): the picker offers its models.
    locked: Option<filo_local::Engine>,
    saved: Saved,
    saving: bool,
    /// Saved a moment ago: the save bar says so briefly.
    just_saved: bool,
    /// The page's own focus: a click on the page takes the keyboard from a field.
    focus: gpui::FocusHandle,
    /// What the agent's conversation shared, under the form of an agent that has one.
    shared: Option<Entity<SharedPanel>>,
    /// The agent's own routines, read once when its form opens (None until they arrive).
    routines: Option<Result<Vec<Routine>, String>>,
    scroll: ScrollHandle,
    _subs: [Subscription; 3],
}

/// The picture sheet while it's open (`open_picker`): the picture its preview shows (the form's changes only on Done),
/// an upload being read or cropped, whether Done is keeping one, and the crop's zoom.
struct PictureSheet {
    picture: String,
    /// The colour the preview shows it on.
    background: Option<String>,
    /// Where Done's picture goes when it isn't the form's (`open_picker_for`).
    done: Option<PictureDone>,
    crop: Option<agent_picture::Crop>,
    reading: bool,
    keeping: bool,
    zoom: Entity<SliderState>,
    _zoom: Subscription,
}

gpui::actions!(agents, [BackAtLineStart]);

/// In an agent's profile, ⌘[ goes back from its fields too (they don't indent), and ⌘← does once the caret is at its
/// line's start (`back_at_line_start`); elsewhere on the page the app's own ⌘← and ⌘[ (chrome.rs) go back.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-[", crate::chrome::GoBack, Some("AgentProfile > Input")),
        gpui::KeyBinding::new("cmd-left", BackAtLineStart, Some("AgentProfile > Input")),
    ]);
}

pub struct AgentsScreen {
    agents: Option<Result<Vec<AgentRow>, String>>,
    selected: Option<String>,
    form: Option<Form>,
    sheet: Option<PictureSheet>,
    custom: Vec<CustomAvatar>,
    /// The pictures the user uploaded (`agentPictures:list`), newest first.
    uploads: Vec<agent_picture::Uploaded>,
    /// Open conversations by agent id, most recently shown last.
    chats: Vec<(String, Entity<Conversation>)>,
    /// The agent whose conversation is being started.
    opening: Option<String>,
    /// A note shared to an agent from the Notes tab, put above its composer once its conversation shows.
    pending_share: Option<(String, crate::screens::chat::note_share::SharedNote)>,
    /// The agent the first run made (screens/welcome.rs) and three things to say to it, offered over its chat until
    /// something is said there.
    first_chat: Option<(String, Vec<&'static str>)>,
    /// The group, once loaded (`Some(None)`: there isn't one yet), its conversation, and whether it's being made.
    group: Option<Option<GroupRow>>,
    /// How the group is set (`agents:groupSetup`), its setup or Group settings while open, the routing step's models,
    /// and whether it was just turned on (its chat shows once `agents:group` has it).
    group_settings: Option<GroupSettingsRow>,
    setup: Option<GroupSetup>,
    routing: Option<RoutingModels>,
    turning_on: bool,
    /// A new agent made from the setup (it needs two): back to the setup once it's made.
    back_to_setup: bool,
    group_chat: Option<Entity<Conversation>>,
    /// The group view's clicks on @mentions.
    _group_sub: Option<Subscription>,
    /// Who the group view last showed as speakers, so it's told only when they change.
    group_speakers: String,
    /// Something outside the tab asked for an agent or the group (a banner, the pet's tray): the tab doesn't switch
    /// to the group by itself when that loads.
    picked: bool,
    /// Whether New agent can make one (`agents:canCreate`).
    can_create: Option<CanCreate>,
    attending: bool,
    /// The list's width as the user left it (`desktop-agents.json`); `list_width` fits it to the window.
    list_width: f32,
    /// A drag changed the width and it isn't saved yet: it's saved once the drag ends.
    list_width_unsaved: bool,
    /// Whether the list's rows show their coding tool's mark (`desktop-agents.json`), and when they last began fading in
    /// or out.
    tool_marks: bool,
    tool_marks_fade: Option<std::time::Instant>,
    /// When the newest time the list and the chat show began ("Working 12s", "Working · 4m"), so the ticker redraws as
    /// they move on (None: no time shows).
    list_newest: Option<f64>,
    chat_newest: Option<f64>,
    /// The agents' and the group's chats with work running, from the companion's (`voiceSessions:working`, the pet's
    /// tray), for the list's "Working" rows; and the companion watched for them (it changes every second, the tray
    /// seldom: only a change in it redraws the list).
    tray: Vec<crate::companion::Working>,
    _tray_sub: Option<(gpui::EntityId, Subscription)>,
    /// Agents' chats (and the group's, as `GROUP`) left with something unsent, on top of the list (chat/drafts.rs), and
    /// each open conversation watched for its draft being sent or cleared.
    drafts: Drafts,
    draft_subs: HashMap<gpui::EntityId, Subscription>,
    /// Each agent's chat watched for clicks on its lines pointing at the group.
    link_subs: HashMap<gpui::EntityId, Subscription>,
    /// The message to show once its chat's conversation shows (an agent's id, or `GROUP`): the group's one such a
    /// click points at, or one ⌘P's search found, with how far back it is (0: loaded already, or not known).
    reveal: Option<(String, String, usize)>,
    /// The list's rows in the order shown, and the order they take once the pointer leaves the list (`arrange`): while
    /// it's over the list (`pointer`) rows don't move under it (they slide to a new order: sliding_rows.rs). The rows
    /// left with a draft at the last placing: one that gains or loses its draft (the user leaving its chat, or sending
    /// or clearing it) moves even under the pointer.
    shown: Vec<String>,
    target: Vec<String>,
    placed_drafts: Vec<String>,
    pointer: ListPointer,
    _subs: Vec<Task<()>>,
}

/// Where the pointer is for the list: the list's bounds as last drawn, and whether the pointer left the window.
#[derive(Clone, Default)]
struct ListPointer {
    bounds: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    away: Rc<Cell<bool>>,
    /// Whether the last drawn list held its order.
    held: Rc<Cell<bool>>,
}

impl ListPointer {
    fn over(&self, position: gpui::Point<gpui::Pixels>) -> bool {
        !self.away.get() && self.bounds.get().is_some_and(|bounds| bounds.contains(&position))
    }
}

/// The list's edge being dragged.
#[derive(Clone)]
struct ListEdge;

impl Render for ListEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// `desktop-agents.json`, beside the app's other settings files.
fn settings_path() -> std::path::PathBuf {
    crate::config::config_dir().join("desktop-agents.json")
}

/// The saved list width; anything missing or unreadable is the default, and a stored width is kept within the limits.
fn parse_list_width(text: &str) -> f32 {
    let value: Option<f64> =
        serde_json::from_str::<Value>(text).ok().and_then(|value| value.get("listWidth")?.as_f64());
    value.filter(|width| width.is_finite()).map_or(LIST_WIDTH, |width| (width as f32).clamp(LIST_MIN, LIST_MAX))
}

/// Whether the list's rows show their coding tool's mark (the list's ⌄ menu); on unless turned off.
fn parse_tool_marks(text: &str) -> bool {
    serde_json::from_str::<Value>(text).ok().and_then(|value| value.get("toolMarks")?.as_bool()).unwrap_or(true)
}

fn load_settings() -> String {
    std::fs::read_to_string(settings_path()).unwrap_or_default()
}

/// Sets one setting in the file, the others kept; one that isn't a JSON object is never saved over (config.rs).
fn save_setting(key: &str, value: Value) {
    if let Err(error) =
        crate::config::set_settings("desktop-agents.json", serde_json::Map::from_iter([(key.to_string(), value)]))
    {
        eprintln!("agents: {error}");
    }
}

fn save_list_width(width: f32) {
    save_setting("listWidth", json!(width.round()));
}

/// The list's width in a tab `content` wide: what the user chose, within the limits, narrowed so the pane beside it
/// keeps `PANE_MIN` (never under `LIST_MIN`).
fn fit_list_width(chosen: f32, content: f32) -> f32 {
    chosen.clamp(LIST_MIN, LIST_MAX).min(content - PANE_MIN).max(LIST_MIN)
}

/// What places a row in the list (`arrange`).
#[derive(Clone, Debug, Default)]
struct Place {
    key: String,
    /// The group (0) and the main agent (1) stay on top.
    pinned: Option<u8>,
    needs_input: bool,
    /// Left with a draft (chat/drafts.rs).
    draft: bool,
    /// Its last message's time, the one its row shows.
    last: f64,
}

/// The list's order: the group and the main agent, then the rows needing input, then those left with a draft, then the
/// rest, each part by last message, the latest first. Ties keep the order given.
fn arrange(places: &[Place]) -> Vec<String> {
    let rank = |place: &Place| -> (u8, f64) {
        match place {
            Place { pinned: Some(pin), .. } => (0, f64::from(*pin)),
            Place { needs_input: true, last, .. } => (1, -last),
            Place { draft: true, last, .. } => (2, -last),
            Place { last, .. } => (3, -last),
        }
    };
    let mut sorted: Vec<&Place> = places.iter().collect();
    sorted.sort_by(|a, b| {
        let (a, b) = (rank(a), rank(b));
        a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
    });
    sorted.into_iter().map(|place| place.key.clone()).collect()
}

/// `shown` held in place while the pointer is over the list: its rows keep their order, a row that's gone drops out and
/// a new one, or one in `moved` (the user changed it: its draft), goes where `target` puts it.
fn held(shown: &[String], target: &[String], moved: &[String]) -> Vec<String> {
    let mut order: Vec<String> =
        shown.iter().filter(|key| target.contains(key) && !moved.contains(key)).cloned().collect();
    for (index, key) in target.iter().enumerate() {
        if !order.contains(key) {
            order.insert(index.min(order.len()), key.clone());
        }
    }
    order
}

/// How long the tool marks take to fade in or out.
const MARKS_FADE: std::time::Duration = std::time::Duration::from_millis(180);

impl AgentsScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _subs = vec![
            working_panel::ticker(cx, |this: &Self| {
                this.list_newest.into_iter().chain(this.chat_newest).reduce(f64::max)
            }),
            backend::subscribe(
                cx,
                "agents:list",
                json!({}),
                |this: &mut Self, result: Result<Vec<AgentRow>, String>, cx| {
                    if let Ok(rows) = &result {
                        // A deleted agent's conversation closes with it.
                        this.chats.retain(|(id, _)| rows.iter().any(|row| &row.id == id));
                        if this.selected.as_ref().is_none_or(|id| id != GROUP && !rows.iter().any(|row| &row.id == id))
                        {
                            this.selected = rows.first().map(|row| row.id.clone());
                        }
                    }
                    this.agents = Some(result);
                    cx.notify();
                },
            ),
            backend::subscribe(
                cx,
                "customAvatars:list",
                json!({}),
                |this: &mut Self, result: Result<Vec<CustomAvatar>, String>, cx| {
                    this.custom = result.unwrap_or_default();
                    cx.notify();
                },
            ),
            backend::subscribe(
                cx,
                "agentPictures:list",
                json!({}),
                |this: &mut Self, result: Result<Vec<agent_picture::Uploaded>, String>, cx| {
                    this.uploads = result.unwrap_or_default();
                    cx.notify();
                },
            ),
            backend::subscribe(
                cx,
                "agents:group",
                json!({}),
                |this: &mut Self, result: Result<Option<GroupRow>, String>, cx| {
                    if let Ok(group) = result {
                        // The tab opens on the group while it's on.
                        if group.is_some()
                            && !matches!(this.group, Some(Some(_)))
                            && this.form.is_none()
                            && !this.picked
                        {
                            this.selected = Some(GROUP.to_string());
                        }
                        if group.is_some() {
                            this.turning_on = false;
                        }
                        this.group = Some(group);
                        cx.notify();
                    }
                },
            ),
            backend::subscribe(
                cx,
                "agents:groupSetup",
                json!({}),
                |this: &mut Self, result: Result<GroupSettingsRow, String>, cx| {
                    if let Ok(settings) = result {
                        this.group_settings = Some(settings);
                        cx.notify();
                    }
                },
            ),
            backend::subscribe(
                cx,
                "agents:canCreate",
                json!({}),
                |this: &mut Self, result: Result<CanCreate, String>, cx| {
                    this.can_create = result.ok();
                    cx.notify();
                },
            ),
        ];
        let settings = load_settings();
        AgentsScreen {
            agents: None,
            selected: None,
            form: None,
            sheet: None,
            custom: Vec::new(),
            uploads: Vec::new(),
            chats: Vec::new(),
            opening: None,
            pending_share: None,
            first_chat: None,
            group: None,
            group_settings: None,
            setup: None,
            routing: None,
            turning_on: false,
            back_to_setup: false,
            group_chat: None,
            _group_sub: None,
            group_speakers: String::new(),
            picked: false,
            can_create: None,
            attending: false,
            list_width: parse_list_width(&settings),
            list_width_unsaved: false,
            tool_marks: parse_tool_marks(&settings),
            tool_marks_fade: None,
            list_newest: None,
            chat_newest: None,
            tray: Vec::new(),
            _tray_sub: None,
            drafts: Drafts::default(),
            draft_subs: HashMap::new(),
            link_subs: HashMap::new(),
            reveal: None,
            shown: Vec::new(),
            target: Vec::new(),
            placed_drafts: Vec::new(),
            pointer: ListPointer::default(),
            _subs,
        }
    }

    /// Where a note can be shared (the Notes tab's right-click menu, `nav::share_targets`).
    pub fn share_targets(&self) -> Vec<crate::screens::chat::note_share::AgentTarget> {
        self.rows()
            .iter()
            .map(|row| crate::screens::chat::note_share::AgentTarget {
                id: row.id.clone(),
                name: row.name.clone(),
                picture: row.image(),
                background: background_color(row.background.as_deref()),
            })
            .collect()
    }

    fn rows(&self) -> &[AgentRow] {
        match &self.agents {
            Some(Ok(rows)) => rows,
            _ => &[],
        }
    }

    /// The list's width as drawn now.
    fn list_width(&self) -> f32 {
        fit_list_width(self.list_width, ui::content_width())
    }

    /// The list's edge follows the pointer, within the limits and the window's room.
    fn on_edge_drag(&mut self, event: &DragMoveEvent<ListEdge>, _: &mut Window, cx: &mut Context<Self>) {
        let wanted = f32::from(event.event.position.x - event.bounds.left());
        let width = fit_list_width(wanted, ui::content_width());
        if (width - self.list_width).abs() >= 0.5 {
            self.list_width = width;
            self.list_width_unsaved = true;
            cx.notify();
        }
    }

    /// A double-click on the edge puts the list back to its usual width.
    pub fn reset_list_width(&mut self, cx: &mut Context<Self>) {
        self.list_width = LIST_WIDTH;
        self.list_width_unsaved = false;
        save_list_width(LIST_WIDTH);
        cx.notify();
    }

    /// Turns the tool marks on the list's rows on or off (the list's ⌄ menu), fading them, and keeps the choice.
    pub fn set_tool_marks(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.tool_marks {
            return;
        }
        // Turned back midway, the fade goes back from where it got to.
        let shown = self
            .tool_marks_fade
            .map_or(1., |started| (started.elapsed().as_secs_f32() / MARKS_FADE.as_secs_f32()).min(1.));
        self.tool_marks = on;
        self.tool_marks_fade = Some(std::time::Instant::now() - MARKS_FADE.mul_f32(1. - shown));
        save_setting("toolMarks", json!(on));
        cx.notify();
    }

    /// Whether the tool marks are on, for the scripted checks.
    #[cfg(feature = "dev")]
    pub fn tool_marks(&self) -> bool {
        self.tool_marks
    }

    /// How much the tool marks show now (0 to 1), asking for the next frame while they fade.
    fn tool_marks_opacity(&mut self, window: &mut Window) -> f32 {
        let done = self
            .tool_marks_fade
            .map_or(1., |started| (started.elapsed().as_secs_f32() / MARKS_FADE.as_secs_f32()).min(1.));
        if done < 1. {
            window.request_animation_frame();
        } else {
            self.tool_marks_fade = None;
        }
        if self.tool_marks { done } else { 1. - done }
    }

    /// The list's width, for the scripted checks: (as drawn, as chosen).
    #[cfg(feature = "dev")]
    pub fn list_widths(&self) -> (f32, f32) {
        (self.list_width(), self.list_width)
    }

    /// The open form's model, for the scripted checks (`FILO_OPEN=agent-model-drive`).
    #[cfg(feature = "dev")]
    pub fn drive_form_model(&self) -> Option<conversation::ModelChoice> {
        self.form.as_ref().map(|form| form.model.clone())
    }

    /// The open form's (unsaved changes, instructions as shown, model), for `FILO_OPEN=agent-save-keys-drive`.
    #[cfg(feature = "dev")]
    pub fn drive_form_state(&self, cx: &App) -> Option<(bool, String, conversation::ModelChoice)> {
        let form = self.form.as_ref()?;
        Some((self.form_dirty(cx), form.instructions.read(cx).value().to_string(), form.model.clone()))
    }

    /// Where the keyboard is, as far as the open form goes (agent-save-keys-drive).
    #[cfg(feature = "dev")]
    pub fn drive_focus_place(&self, window: &Window, cx: &App) -> &'static str {
        let Some(form) = &self.form else { return "no form" };
        if form.instructions.read(cx).focus_handle(cx).is_focused(window) {
            "the instructions"
        } else if form.focus.is_focused(window) {
            "the page"
        } else if form.focus.contains_focused(window, cx) {
            "something in the page"
        } else if window.focused(cx).is_some() {
            "something outside the page"
        } else {
            "nothing"
        }
    }

    /// The keyboard in the open form's instructions (agent-save-keys-drive).
    #[cfg(feature = "dev")]
    pub fn drive_focus_instructions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &self.form {
            form.instructions.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// The open form's instructions changed to `text`, as typing would (agent-save-keys-drive).
    #[cfg(feature = "dev")]
    pub fn drive_set_instructions(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &self.form {
            form.instructions.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        }
        cx.notify();
    }

    /// Whether the open chat's composer has the keyboard (agent-back-drive).
    #[cfg(feature = "dev")]
    pub fn drive_composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.form.is_none()
            && self
                .current_conversation()
                .is_some_and(|view| view.read(cx).composer().read(cx).focus_handle(cx).is_focused(window))
    }

    /// The open form's instructions caret: its line and column (agent-back-drive).
    #[cfg(feature = "dev")]
    pub fn drive_instructions_caret(&self, cx: &App) -> Option<(u32, u32)> {
        let position = self.form.as_ref()?.instructions.read(cx).cursor_position();
        Some((position.line, position.character))
    }

    /// The chat open (an agent's id, or `GROUP`), for the scripted checks (`FILO_OPEN=search-drive`).
    #[cfg(feature = "dev")]
    pub fn drive_selected(&self) -> Option<String> {
        self.selected.clone()
    }

    /// The agent named `name`, for the scripted checks (`FILO_OPEN=agents-drive`).
    /// The agents in the list as (id, name, main) (smoke.rs `FILO_OPEN=main-agent-drive`).
    #[cfg(feature = "dev")]
    pub fn drive_rows(&self) -> Vec<(String, String, bool)> {
        self.rows().iter().map(|row| (row.id.clone(), row.name.clone(), row.main)).collect()
    }

    /// The agent shown, the things to try offered over its chat, and what its message box holds (smoke.rs's
    /// welcome-drive).
    #[cfg(feature = "dev")]
    pub fn drive_first_chat(&self, cx: &App) -> (Option<String>, Option<Vec<&'static str>>, Option<String>) {
        let tries = self
            .first_chat
            .as_ref()
            .filter(|(id, _)| self.selected.as_ref() == Some(id))
            .map(|(_, tries)| tries.clone());
        let draft = self
            .selected
            .as_deref()
            .and_then(|id| self.chat(id))
            .map(|view| view.read(cx).composer().read(cx).value().to_string());
        (self.selected.clone(), tries, draft)
    }

    /// Makes `id` the main agent, as its menu's Make main agent does.
    #[cfg(feature = "dev")]
    pub fn drive_make_main(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.make_main(id, window, cx, |_, _, _| {});
    }

    /// The group's page as the group-optional-drive reads it (smoke/drives.rs): whether its chat is on, the open setup
    /// (Group settings or not, its step, the members it puts in, All agents, its name and the routing model it has), and
    /// whether a coding tool is signed in.
    #[cfg(feature = "dev")]
    pub fn drive_group(
        &self,
        cx: &App,
    ) -> (bool, Option<(bool, usize, Vec<String>, bool, String, Option<String>)>, bool) {
        let setup = self.setup.as_ref().map(|setup| {
            let route = self.setup_route().map(|route| format!("{}:{}", route.engine.key(), route.model));
            (
                setup.settings,
                setup.step,
                self.setup_members(),
                setup.all,
                setup.name.read(cx).value().to_string(),
                route,
            )
        });
        (self.group_row().is_some(), setup, self.routing.as_ref().is_some_and(|routing| routing.signed_in))
    }

    /// The level the routing step's reasoning picker shows (group-setup-scroll-drive): the one picked, else Low, as the
    /// nearest the routing model takes; none while its levels aren't known.
    #[cfg(feature = "dev")]
    pub fn drive_route_reasoning(&mut self, cx: &App) -> Option<&'static str> {
        let levels = self.route_levels(cx);
        let route = self.setup_route()?;
        conversation::reasoning_used(
            route.reasoning.as_deref().unwrap_or(filo_local::group_route::DEFAULT_REASONING),
            &levels,
        )
    }

    /// What a click on the setup does, by name (group-optional-drive: posted clicks need a window macOS draws): "new
    /// agent", "all", "member:<id>", "next", "back", "route:<engine>:<model>", "reasoning:<level>", "commit", "cancel",
    /// "step:<n>".
    #[cfg(feature = "dev")]
    pub fn drive_setup(&mut self, action: &str, window: &mut Window, cx: &mut Context<Self>) {
        match action.split_once(':') {
            Some(("member", id)) => self.toggle_member(id.to_string(), cx),
            Some(("step", step)) => self.setup_step(step.parse().unwrap_or(0), cx),
            Some(("reasoning", level)) => {
                if let Some(level) = conversation::reasoning_level(level) {
                    self.pick_route_reasoning(level, cx);
                }
            }
            Some(("route", route)) => {
                if let Some((engine, model)) =
                    route.split_once(':').and_then(|(engine, model)| Some((filo_local::Engine::parse(engine)?, model)))
                {
                    self.pick_route((engine, Some(model.to_string())), cx);
                }
            }
            _ => match action {
                "new agent" => self.new_agent_for_setup(window, cx),
                "all" => self.toggle_all(cx),
                "next" => self.setup_step(1, cx),
                "back" => self.setup_step(0, cx),
                "commit" => self.commit_setup(window, cx),
                "cancel" => self.close_setup(cx),
                "save agent" => self.save(window, cx),
                _ => eprintln!("drive_setup: no {action}"),
            },
        }
        cx.notify();
    }

    /// The open form's name and instructions typed over (group-optional-drive's New agent).
    #[cfg(feature = "dev")]
    pub fn drive_fill_form(&mut self, name: &str, instructions: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &self.form {
            form.name.update(cx, |input, cx| input.set_value(name.to_string(), window, cx));
            form.instructions.update(cx, |input, cx| input.set_value(instructions.to_string(), window, cx));
        }
        cx.notify();
    }

    /// Group settings, as its ⋯'s item opens them; and the group turned off, as Turn off's confirmation does.
    #[cfg(feature = "dev")]
    pub fn drive_turn_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.turn_off_group(window, cx);
    }

    #[cfg(feature = "dev")]
    pub fn drive_confirm_turn_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_turn_off(window, cx);
    }

    #[cfg(feature = "dev")]
    pub fn agent_named(&self, name: &str) -> Option<String> {
        self.rows().iter().find(|row| row.name.eq_ignore_ascii_case(name)).map(|row| row.id.clone())
    }

    /// What the open agent chat's work indicator says (as `render_chat` works it out), and whether its row in the list
    /// says it's working (its reply under way), for the scripted checks.
    #[cfg(feature = "dev")]
    pub fn work_status(&self, cx: &App) -> (Option<String>, bool) {
        let Some(row) = self.selected.as_deref().and_then(|id| self.row(id)) else { return (None, false) };
        let Some(view) = self.chat(&row.id) else { return (None, false) };
        (working_panel::status_text(&working_panel::here_for(view.read(cx))), self.list_working(&row.id, cx).is_some())
    }

    /// Whether the chat behind a row of the list (an agent's id, or `GROUP`) is working, and since when
    /// (chat/reply_status.rs): the companion's tray, and the open conversation's own reply.
    fn list_working(&self, id: &str, cx: &App) -> Option<Option<f64>> {
        let view = if id == GROUP { self.group_chat.clone() } else { self.chat(id) };
        reply_status::list_working(id, &self.tray, view.and_then(|view| reply_status::reply_since(view.read(cx))))
    }

    /// Follows the companion's working chats: the list redraws only when the
    /// agents' part of them changes.
    fn watch_tray(&mut self, cx: &mut Context<Self>) {
        let Some(companion) = crate::companion::companion(cx) else {
            self._tray_sub = None;
            self.tray.clear();
            return;
        };
        if self._tray_sub.as_ref().is_some_and(|(id, _)| *id == companion.entity_id()) {
            return;
        }
        // An agent's row also works while a coding task it owns does, whichever chat the task is in.
        let agents_part = |companion: &crate::companion::Companion| -> Vec<crate::companion::Working> {
            let mut part: Vec<crate::companion::Working> = companion
                .working()
                .iter()
                .filter(|chat| matches!(chat.opens, crate::companion::Opens::Agent(_)))
                .cloned()
                .collect();
            let mut tasks: Vec<(&String, &f64)> = companion.coding_agents().iter().collect();
            tasks.sort_by(|a, b| a.0.cmp(b.0));
            part.extend(tasks.into_iter().map(|(agent, since)| crate::companion::Working {
                session_id: format!("coding-task-of:{agent}"),
                title: String::new(),
                since: Some(*since),
                opens: crate::companion::Opens::Agent(agent.clone()),
            }));
            part
        };
        self.tray = agents_part(companion.read(cx));
        let sub = cx.observe(&companion, move |this, companion, cx| {
            let tray = agents_part(companion.read(cx));
            if tray != this.tray {
                this.tray = tray;
                cx.notify();
            }
        });
        self._tray_sub = Some((companion.entity_id(), sub));
    }

    /// The list's order as last drawn, once the pointer is off it (`arrange`).
    #[cfg(feature = "dev")]
    pub fn list_order(&self) -> Vec<String> {
        self.target.clone()
    }

    /// Puts `rows` in the list in place of the agents (`FILO_OPEN=list-reorder-timing`).
    #[cfg(feature = "dev")]
    pub fn show_rows_for_check(&mut self, rows: Vec<AgentRow>, cx: &mut Context<Self>) {
        self.agents = Some(Ok(rows));
        cx.notify();
    }

    /// Gives row `id` a last message saying `preview`, from `at` (none: when its last one was), as `agents:list` does
    /// when its chat moves on.
    #[cfg(feature = "dev")]
    pub fn set_last_for_check(&mut self, id: &str, at: Option<f64>, preview: &str, cx: &mut Context<Self>) {
        if let Some(Ok(rows)) = &mut self.agents
            && let Some(row) = rows.iter_mut().find(|row| row.id == id)
        {
            let at = at.or(row.last.as_ref().map(|last| last.at)).unwrap_or_default();
            row.last = Some(LastMessage { from_user: false, preview: preview.to_string(), at });
            cx.notify();
        }
    }

    /// The agents' chats left with a draft, with their draft's line (`FILO_OPEN=drafts-drive`).
    #[cfg(feature = "dev")]
    pub fn drafts(&self) -> Vec<(String, String)> {
        self.drafts.entries().to_vec()
    }

    /// What ⌘P's search lists (screens/search.rs): the agents and the group, as the list has them.
    pub fn search_sources(&self) -> (&[AgentRow], Option<&GroupRow>) {
        (self.rows(), self.group_row())
    }

    fn row(&self, id: &str) -> Option<&AgentRow> {
        self.rows().iter().find(|row| row.id == id)
    }

    fn chat(&self, agent_id: &str) -> Option<Entity<Conversation>> {
        self.chats.iter().find(|(id, _)| id == agent_id).map(|(_, view)| view.clone())
    }

    /// The conversation on screen, when the right pane shows one.
    pub fn current_conversation(&self) -> Option<Entity<Conversation>> {
        if self.form.is_some() {
            return None;
        }
        match self.selected.as_deref()? {
            GROUP => self.group_chat.clone(),
            id => self.chat(id),
        }
    }

    /// Whether the user is looking at the Agents tab: the conversation on screen marks replies seen.
    pub fn set_attending(&mut self, attending: bool, cx: &mut Context<Self>) {
        self.attending = attending;
        let current = self.current_conversation();
        for view in self.chats.iter().map(|(_, view)| view.clone()).chain(self.group_chat.clone()).collect::<Vec<_>>() {
            let looking = attending && current.as_ref() == Some(&view);
            view.update(cx, |view, cx| view.set_attending(looking, cx));
        }
    }

    fn group_row(&self) -> Option<&GroupRow> {
        self.group.as_ref()?.as_ref()
    }

    /// The group's Filo: the Filo agent fresh data starts with, by its own name and picture (its colour is
    /// `filo_background`); while it isn't an agent, the panda named Filo.
    fn filo(&self) -> (String, ImageSource) {
        match self.row(crate::local_backend::DEFAULT_AGENT_ID) {
            Some(row) => (row.name.clone(), row.image()),
            None => ("Filo".into(), avatars::animal_image(avatars::DEFAULT_AVATAR)),
        }
    }

    fn filo_background(&self) -> Option<gpui::Hsla> {
        self.row(crate::local_backend::DEFAULT_AGENT_ID).and_then(|row| background_color(row.background.as_deref()))
    }

    /// The group's conversation.
    fn group_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Conversation>> {
        let group = self.group_row().cloned()?;
        let view = match &self.group_chat {
            Some(view) => view.clone(),
            None => {
                let view =
                    cx.new(|cx| Conversation::new(Some(group.session_id.clone()), "The group".into(), window, cx));
                // An @mention opens that agent.
                self._group_sub =
                    Some(cx.subscribe_in(&view, window, |this, _, event: &ConversationEvent, window, cx| {
                        if let ConversationEvent::OpenSpeaker(speaker) = event
                            && this.row(speaker).is_some()
                        {
                            this.open_agent(speaker.clone(), window, cx);
                        }
                    }));
                self.group_chat = Some(view.clone());
                self.watch_draft(&view, cx);
                self.set_attending(self.attending, cx);
                view
            }
        };
        let (filo_name, filo_picture) = self.filo();
        let filo_look =
            self.row(crate::local_backend::DEFAULT_AGENT_ID).map(|row| (row.picture.clone(), row.background.clone()));
        let key = std::iter::once(format!("filo={filo_name}:{}:{filo_look:?}", group.filo))
            .chain(std::iter::once(format!("everyone={:?}:{:?}", group.picture, group.background)))
            .chain(
                group
                    .members
                    .iter()
                    .map(|member| format!("{}={}:{}:{:?}", member.id, member.name, member.picture, member.background)),
            )
            .chain(
                self.rows().iter().map(|row| format!("{}={}:{}:{:?}", row.id, row.name, row.picture, row.background)),
            )
            .collect::<Vec<_>>()
            .join("|");
        if key != self.group_speakers {
            self.group_speakers = key;
            let mut speakers = HashMap::new();
            speakers.insert(
                crate::local_backend::DEFAULT_AGENT_ID.to_string(),
                (filo_name, filo_picture, self.filo_background()),
            );
            // Every agent, members or not: one that left the group still shows on what it said there.
            for row in self.rows() {
                speakers.insert(
                    row.id.clone(),
                    (row.name.clone(), row.image(), background_color(row.background.as_deref())),
                );
            }
            for member in &group.members {
                let picture = picture_source(&member.picture);
                speakers.insert(
                    member.id.clone(),
                    (member.name.clone(), picture, background_color(member.background.as_deref())),
                );
            }
            // An "@" in the group names Filo (while it's in it), any agent, or everyone (crates/filo-local
            // `group::names_everyone`), shown with the group's picture.
            let everyone = picture_source(group.picture.as_deref().unwrap_or(GROUP_PICTURE));
            let mentions: Vec<Mention> = group
                .filo
                .then(|| self.filo())
                .into_iter()
                .map(|(name, picture)| (name, picture, self.filo_background()))
                .chain(group.members.iter().map(|member| {
                    (
                        member.name.clone(),
                        picture_source(&member.picture),
                        background_color(member.background.as_deref()),
                    )
                }))
                .chain(std::iter::once((
                    "everyone".to_string(),
                    everyone,
                    background_color(group.background.as_deref()),
                )))
                .collect();
            view.update(cx, |view, cx| {
                view.set_group(speakers, cx);
                view.set_mentions(mentions, cx);
            });
        }
        self.reveal_in(GROUP, &view, cx);
        Some(view)
    }

    /// A line in an agent's chat pointing at the group was clicked: the group, at that message when it's known.
    fn open_group_at(&mut self, message: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal = message.map(|message| (GROUP.to_string(), message, 0));
        self.open_agent(GROUP.to_string(), window, cx);
    }

    /// An agent's chat or the group's (`GROUP`), scrolled to `message`, `depth` messages from the newest, flashing it
    /// (⌘P's search, screens/search.rs).
    pub fn open_at(
        &mut self,
        chat: String,
        message: String,
        depth: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal = Some((chat.clone(), message, depth));
        self.open_agent(chat, window, cx);
    }

    /// Shows the message waiting for `chat`'s conversation (`reveal`), now that it shows.
    fn reveal_in(&mut self, chat: &str, view: &Entity<Conversation>, cx: &mut Context<Self>) {
        if self.reveal.as_ref().is_none_or(|(waiting, _, _)| waiting != chat) {
            return;
        }
        let Some((_, message, depth)) = self.reveal.take() else { return };
        view.update(cx, |view, cx| {
            if depth > 0 { view.reveal_message_at(&message, depth, cx) } else { view.reveal_message(&message, cx) }
        });
    }

    /// Watches an agent's chat for clicks on its lines pointing at the group.
    fn watch_links(&mut self, view: &Entity<Conversation>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(view, window, |this, _, event: &ConversationEvent, window, cx| match event {
            ConversationEvent::OpenGroup(message) => this.open_group_at(message.clone(), window, cx),
            // An @mention opens that agent, as in the group.
            ConversationEvent::OpenSpeaker(agent) if this.row(agent).is_some() => {
                this.open_agent(agent.clone(), window, cx)
            }
            // An ask between agents while the group is off: the other agent's chat, at the ask or its answer.
            ConversationEvent::OpenAgent(agent, message) if this.row(agent).is_some() => match message {
                Some(message) => this.open_at(agent.clone(), message.clone(), 0, window, cx),
                None => this.open_agent(agent.clone(), window, cx),
            },
            _ => {}
        });
        self.link_subs.insert(view.entity_id(), sub);
    }

    /// Opens the agent's conversation with the note above its composer, what's typed there kept (its conversation may
    /// have to be started first, so the note goes on when it shows: `render_chat`).
    pub fn share_note(
        &mut self,
        id: String,
        note: crate::screens::chat::note_share::SharedNote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_share = Some((id.clone(), note));
        self.open_agent(id, window, cx);
    }

    /// The chat on screen is being left for `next` (None: the agent form): with something typed and not sent, it goes on
    /// top of the list; without, off it.
    fn leave_current(&mut self, next: Option<&str>, cx: &mut Context<Self>) {
        let Some(current) = self.selected.clone().filter(|current| Some(current.as_str()) != next) else { return };
        let Some(view) = self.current_conversation() else { return };
        let preview = view.read(cx).draft_preview(cx);
        self.drafts.left(&current, preview);
    }

    /// The drafts as the chats hold them now (one sent or cleared drops out); whether they changed.
    fn sync_drafts(&mut self, cx: &App) -> bool {
        let (chats, group) = (&self.chats, &self.group_chat);
        self.drafts.refresh(|key| {
            let view = if key == GROUP {
                group.as_ref()
            } else {
                chats.iter().find(|(id, _)| id == key).map(|(_, view)| view)
            };
            view?.read(cx).draft_preview(cx)
        })
    }

    /// Redraws the list when `view`'s draft is sent or cleared, or typed on while its row shows it; nothing else it does.
    fn watch_draft(&mut self, view: &Entity<Conversation>, cx: &mut Context<Self>) {
        let sub = cx.observe(view, |this, _, cx| {
            if this.sync_drafts(cx) {
                cx.notify();
            }
        });
        self.draft_subs.insert(view.entity_id(), sub);
    }

    pub fn open_agent(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_current(Some(&id), cx);
        if self.pending_share.as_ref().is_some_and(|(agent, _)| *agent != id) {
            self.pending_share = None;
        }
        self.selected = Some(id.clone());
        self.picked = true;
        self.form = None;
        // The group while it's off: its setup, filled in as it was.
        if id == GROUP
            && self.setup.is_none()
            && matches!(self.group, Some(None))
            && self.group_settings.is_some()
            && !self.turning_on
        {
            self.open_setup(false, window, cx);
        }
        if let Some(view) = self.chat(&id) {
            view.update(cx, |view, cx| view.focus_composer(window, cx));
        }
        self.set_attending(self.attending, cx);
        cx.notify();
    }

    /// The agent the first run made: its chat, with `tries` offered over it while nothing has been said there.
    pub fn open_first_chat(
        &mut self,
        id: String,
        tries: Vec<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.first_chat = Some((id.clone(), tries));
        self.open_agent(id, window, cx);
    }

    /// A thing to try, in the selected agent's message box to send or change.
    fn try_saying(&mut self, text: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.selected.clone().and_then(|id| self.chat(&id)) {
            view.update(cx, |view, cx| view.type_draft(text.to_string(), window, cx));
        }
    }

    /// The selected agent's conversation, opened (or started) when first shown.
    fn conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Conversation>> {
        let row = self.row(self.selected.as_deref()?)?.clone();
        if let Some(index) = self.chats.iter().position(|(id, _)| *id == row.id) {
            let switched = index + 1 != self.chats.len();
            let entry = self.chats.remove(index);
            self.chats.push(entry);
            let view = self.chats.last()?.1.clone();
            let mentions = self.mentions(&row.id);
            let (faces_key, faces) = self.faces();
            view.update(cx, |view, cx| {
                view.set_agent_name(row.name.clone(), cx);
                view.set_mentions(mentions, cx);
                view.set_faces(faces_key, faces, cx);
                // Nothing said yet: the agent's picture shows (`render_chat`), not a skeleton under it.
                view.set_known_empty(row.last.is_none(), cx);
                if switched {
                    view.unpark(cx);
                }
            });
            if switched {
                self.park_old_chats(cx);
            }
            return Some(view);
        }
        let Some(session_id) = row.session_id.clone() else {
            if self.opening.as_deref() != Some(row.id.as_str()) {
                self.opening = Some(row.id.clone());
                backend::call_in(
                    window,
                    cx,
                    "agents:openChat",
                    json!({ "id": row.id }),
                    |this: &mut Self, result: Result<Value, String>, window, cx| {
                        this.opening = None;
                        if let Err(error) = result {
                            ui::toast_error(window, cx, "Couldn’t open the agent", error);
                        }
                        cx.notify();
                    },
                );
            }
            return None;
        };
        let view = cx.new(|cx| Conversation::new(Some(session_id), row.name.clone(), window, cx));
        let mentions = self.mentions(&row.id);
        let (faces_key, faces) = self.faces();
        view.update(cx, |view, cx| {
            // It answers with the model picked for the agent: no model picker in its composer.
            view.fix_model(cx);
            view.set_mentions(mentions, cx);
            view.set_faces(faces_key, faces, cx);
            view.set_known_empty(row.last.is_none(), cx);
        });
        self.chats.push((row.id.clone(), view.clone()));
        self.watch_draft(&view, cx);
        self.watch_links(&view, window, cx);
        self.park_old_chats(cx);
        view.update(cx, |view, cx| view.focus_composer(window, cx));
        self.set_attending(self.attending, cx);
        Some(view)
    }

    /// Past `MAX_OPEN`, parks the chats shown longest ago (not ones with unsent work: a message on its way needs the
    /// chat's answers to hand over to), and past `MAX_KEPT` closes them.
    fn park_old_chats(&mut self, cx: &mut Context<Self>) {
        let parked = self.chats.len().saturating_sub(MAX_OPEN);
        for (_, view) in &self.chats[..parked] {
            if !view.read(cx).has_unsent_work(cx) {
                view.update(cx, |view, _| view.park());
            }
        }
        // Past `MAX_KEPT`, the chats shown longest ago close, but not ones holding a draft or a send.
        let mut index = 0;
        while self.chats.len() > MAX_KEPT && index + 1 < self.chats.len() {
            if self.chats[index].1.read(cx).has_unsent_work(cx) {
                index += 1;
            } else {
                let (_, view) = self.chats.remove(index);
                self.draft_subs.remove(&view.entity_id());
                self.link_subs.remove(&view.entity_id());
            }
        }
    }

    /// Every agent's name, picture and colour by id, for the chips between agents asking each other, with a key that
    /// changes when one of them does.
    fn faces(&self) -> (String, HashMap<String, (String, ImageSource, Option<gpui::Hsla>)>) {
        let key = self
            .rows()
            .iter()
            .map(|row| format!("{}={}:{}:{:?}", row.id, row.name, row.picture, row.background))
            .collect::<Vec<_>>()
            .join("|");
        let faces = self
            .rows()
            .iter()
            .map(|row| (row.id.clone(), (row.name.clone(), row.image(), background_color(row.background.as_deref()))))
            .collect();
        (key, faces)
    }

    /// Who an "@" in an agent's chat offers: Filo, then the other agents.
    fn mentions(&self, agent_id: &str) -> Vec<Mention> {
        let (filo_name, filo_picture) = self.filo();
        std::iter::once((filo_name, filo_picture, None))
            .chain(
                self.rows()
                    .iter()
                    .filter(|row| row.id != agent_id)
                    .map(|row| (row.name.clone(), row.image(), background_color(row.background.as_deref()))),
            )
            .collect()
    }

    pub fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Clicked before `agents:canCreate` answered (just after launch): ask it once, then go on. If it can't be
        // asked, the form opens and `agents:create` checks again.
        if self.can_create.is_none() {
            backend::call_in(
                window,
                cx,
                "agents:canCreate",
                json!({}),
                |this: &mut Self, result: Result<CanCreate, String>, window, cx| {
                    match creation_blocked(result.as_ref().ok()) {
                        Some(reason) => explain_blocked(reason, window, cx),
                        None => this.open_new_form(window, cx),
                    }
                    if let Ok(can_create) = result {
                        this.can_create.get_or_insert(can_create);
                    }
                },
            );
            return;
        }
        if let Some(reason) = creation_blocked(self.can_create.as_ref()) {
            explain_blocked(reason, window, cx);
            return;
        }
        self.open_new_form(window, cx);
    }

    fn open_new_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let animal = &ANIMALS[(format::now_ms() as usize / 7) % ANIMALS.len()];
        let model = (crate::local_backend::default_engine(), None);
        self.open_form(None, String::new(), String::new(), animal.avatar_id("waving"), None, (model, true), window, cx);
    }

    pub fn edit_agent(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.row(&id).cloned() else { return };
        let model = match row.engine {
            Some(engine) => ((engine, row.model), true),
            None => ((row.locked_engine.unwrap_or_else(crate::local_backend::default_engine), None), false),
        };
        self.open_form(Some(id), row.name, row.instructions, row.picture, row.background, model, window, cx);
        if let Some(form) = &mut self.form {
            form.locked = row.locked_engine;
        }
    }

    /// Group settings: the setup's two steps, with Cancel and Save.
    pub fn edit_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_setup(true, window, cx);
    }

    // --- The group's setup and Group settings ---

    /// The group's setup (`settings` false: its page while it's off, ending in Turn on) or its Group settings (Cancel
    /// and Save): the same two steps, from how it's set now (an off group's earlier settings filled in).
    fn open_setup(&mut self, settings: bool, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.group_settings.clone().unwrap_or_default();
        let name = saved.name.clone().unwrap_or_else(|| filo_local::group::DEFAULT_NAME.to_string());
        let name_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder(filo_local::group::DEFAULT_NAME);
            input.set_value(name, window, cx);
            input
        });
        let name_sub = cx.subscribe_in(&name_input, window, |_, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                ui::cap_length(input, NAME_MAX, window, cx);
                cx.notify();
            }
        });
        let picture = saved.picture.clone().unwrap_or_else(|| GROUP_PICTURE.to_string());
        let (look, hue_sub) = ProfilePicture::new(
            picture,
            saved.background.clone(),
            |this: &mut Self| this.setup.as_mut().map(|setup| &mut setup.look),
            window,
            cx,
        );
        let members =
            if saved.all { self.rows().iter().map(|row| row.id.clone()).collect() } else { saved.members.clone() };
        let session = self.group.clone().flatten().map(|group| group.session_id).filter(|_| settings);
        let width = setup_column_width(window, self.list_width());
        let shared = session.map(|session| cx.new(|cx| SharedPanel::new(session, "Members".into(), width, cx)));
        self.setup = Some(GroupSetup {
            settings,
            step: 0,
            name: name_input,
            look,
            all: saved.all,
            members,
            route: saved.route.clone(),
            levels: None,
            saving: false,
            shared,
            scroll: ScrollHandle::new(),
            _subs: [name_sub, hue_sub],
        });
        self.form = None;
        self.selected = Some(GROUP.to_string());
        self.refresh_routing(cx);
        conversation::refresh_models(cx);
        cx.notify();
    }

    /// Whether a tool is signed in and the default routing model, asked again (a tool signed in since counts).
    fn refresh_routing(&mut self, cx: &mut Context<Self>) {
        backend::call(
            cx,
            "agents:routingModels",
            json!({}),
            |this: &mut Self, result: Result<RoutingModels, String>, cx| {
                match result {
                    Ok(models) => this.routing = Some(models),
                    Err(error) => eprintln!("agents:routingModels failed: {error}"),
                }
                cx.notify();
            },
        );
    }

    fn close_setup(&mut self, cx: &mut Context<Self>) {
        self.setup = None;
        cx.notify();
    }

    /// Who the setup puts in the group: every agent while there are fewer than two or all are picked, else the ones
    /// ticked, in the list's order.
    fn setup_members(&self) -> Vec<String> {
        let rows = self.rows();
        match &self.setup {
            Some(setup) if rows.len() >= 2 && !setup.all => {
                rows.iter().filter(|row| setup.members.contains(&row.id)).map(|row| row.id.clone()).collect()
            }
            _ => rows.iter().map(|row| row.id.clone()).collect(),
        }
    }

    /// The routing model the setup has: the one picked, else the cheapest fast one of the tools signed in.
    fn setup_route(&self) -> Option<RouteModel> {
        let routing = self.routing.as_ref()?;
        self.setup.as_ref().and_then(|setup| setup.route.clone()).or_else(|| routing.default.clone())
    }

    /// The setup's step (Group settings is one page: none).
    fn setup_step(&mut self, step: usize, cx: &mut Context<Self>) {
        let enough = self.setup_members().len() >= 2;
        if let Some(setup) = self.setup.as_mut().filter(|setup| !setup.settings)
            && (step == 0 || enough)
        {
            setup.step = step;
            setup.look.set_colors_open(false);
            cx.notify();
        }
    }

    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.rows().iter().map(|row| row.id.clone()).collect();
        if let Some(setup) = &mut self.setup {
            setup.all = !setup.all;
            // Ticking them one by one starts from everyone.
            if !setup.all {
                setup.members = ids;
            }
            cx.notify();
        }
    }

    fn toggle_member(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(setup) = &mut self.setup {
            match setup.members.iter().position(|member| *member == id) {
                Some(index) => {
                    setup.members.remove(index);
                }
                None => setup.members.push(id),
            }
            cx.notify();
        }
    }

    /// A model picked in the routing step's picker (`model_line`): the level picked stays, as the nearest it takes.
    fn pick_route(&mut self, (engine, model): ModelChoice, cx: &mut Context<Self>) {
        let reasoning = self.setup_route().and_then(|route| route.reasoning);
        if let (Some(setup), Some(model)) = (&mut self.setup, model) {
            setup.route = Some(RouteModel { engine, model, reasoning });
            cx.notify();
        }
    }

    /// A level picked in the routing step's reasoning picker, for the routing model it has.
    fn pick_route_reasoning(&mut self, level: &'static str, cx: &mut Context<Self>) {
        let route = self.setup_route();
        if let (Some(setup), Some(route)) = (&mut self.setup, route) {
            setup.route = Some(RouteModel { reasoning: Some(level.to_string()), ..route });
            cx.notify();
        }
    }

    /// The reasoning levels the setup's routing model takes, least first, as the composer asks them for a model
    /// (`Backend::reasoning_levels`): again after a while, sooner while none are known, rather than on every frame.
    fn route_levels(&mut self, cx: &App) -> Vec<String> {
        let Some(route) = self.setup_route() else { return Vec::new() };
        let key = format!("{}:{}", route.engine.key(), route.model);
        let Some(setup) = &mut self.setup else { return Vec::new() };
        let fresh = |(kept, at, levels): &(String, std::time::Instant, Vec<String>)| {
            *kept == key
                && at.elapsed()
                    < if levels.is_empty() {
                        conversation::REASONING_LEVELS_FRESH / 12
                    } else {
                        conversation::REASONING_LEVELS_FRESH
                    }
        };
        if !setup.levels.as_ref().is_some_and(fresh) {
            let levels = match cx.try_global::<backend::BackendGlobal>() {
                Some(backend) => backend.0.reasoning_levels(route.engine, Some(&route.model)),
                None => crate::local_backend::CLAUDE_EFFORTS.iter().map(|level| level.to_string()).collect(),
            };
            setup.levels = Some((key, std::time::Instant::now(), levels));
        }
        setup.levels.as_ref().map(|(_, _, levels)| levels.clone()).unwrap_or_default()
    }

    /// The picture sheet for the setup's picture.
    fn open_setup_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &self.setup else { return };
        let (picture, background) = (setup.look.picture.clone(), setup.look.background.clone());
        self.open_sheet(picture, background, None, window, cx);
    }

    /// A new agent from the setup, which needs two: the form, then back here.
    fn new_agent_for_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.back_to_setup = true;
        self.new_agent(window, cx);
    }

    /// Turn on (the setup) or Save (Group settings): one call either way (`agents:setUpGroup`).
    fn commit_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let members = self.setup_members();
        let route = self.setup_route();
        let few = self.rows().len() < 2;
        let Some(setup) = &mut self.setup else { return };
        if setup.saving || members.len() < 2 {
            return;
        }
        setup.saving = true;
        let settings = setup.settings;
        let picture = (setup.look.picture != GROUP_PICTURE).then(|| setup.look.picture.clone());
        let mut args = json!({
            "name": setup.name.read(cx).value().trim(),
            "picture": picture,
            "background": setup.look.background,
            "all": setup.all || few,
            "members": members,
            "turnOn": !settings,
        });
        if let Some(route) = route {
            args["engine"] = json!(route.engine);
            args["model"] = json!(route.model);
            args["reasoning"] = json!(route.reasoning.as_deref().unwrap_or(filo_local::group_route::DEFAULT_REASONING));
        }
        cx.notify();
        backend::call_in(
            window,
            cx,
            "agents:setUpGroup",
            args,
            move |this: &mut Self, result: Result<Value, String>, window, cx| {
                match result {
                    Ok(_) => {
                        this.setup = None;
                        if settings {
                            ui::toast(window, cx, "Saved");
                        } else {
                            // Its chat shows once `agents:group` has it.
                            this.turning_on = this.group_row().is_none();
                        }
                    }
                    Err(error) => {
                        if let Some(setup) = &mut this.setup {
                            setup.saving = false;
                        }
                        ui::toast_error(window, cx, "Couldn’t save the group", error);
                    }
                }
                cx.notify();
            },
        );
    }

    /// Turn off group, asked first: its page is the setup again, filled in as it was.
    fn confirm_turn_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::button::ButtonVariant;
        use gpui_component::dialog::DialogButtonProps;
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title(div().headline().child("Turn off the group?"))
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Turn off")
                        .ok_variant(ButtonVariant::Primary)
                        .cancel_text("Cancel"),
                )
                .on_ok(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.turn_off_group(window, cx)).ok();
                    true
                })
        });
    }

    fn turn_off_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        backend::call_in(
            window,
            cx,
            "agents:turnOffGroup",
            json!({}),
            |this: &mut Self, result: Result<Value, String>, window, cx| match result {
                Ok(_) => {
                    this.group = Some(None);
                    this.open_setup(false, window, cx);
                }
                Err(error) => ui::toast_error(window, cx, "Couldn’t turn off the group", error),
            },
        );
    }

    /// The agent again on another engine (`agents:duplicate`), on that engine's default model, then its chat.
    fn duplicate_with(&mut self, id: String, engine: filo_local::Engine, window: &mut Window, cx: &mut Context<Self>) {
        let (engine, model) = saved_model(&(engine, None), &conversation::model_rows(cx));
        backend::call_in(
            window,
            cx,
            "agents:duplicate",
            json!({ "id": id, "engine": engine, "model": model }),
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                match result {
                    Ok(value) => {
                        this.form = None;
                        if let Some(copy) = value.as_str() {
                            this.open_agent(copy.to_string(), window, cx);
                        }
                    }
                    Err(error) => ui::toast_error(window, cx, "Couldn’t duplicate the agent", error),
                }
                cx.notify();
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_form(
        &mut self,
        id: Option<String>,
        name: String,
        instructions: String,
        picture: String,
        background: Option<String>,
        (model, model_picked): (ModelChoice, bool),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        conversation::refresh_models(cx);
        let saved = Saved {
            name: name.clone(),
            instructions: instructions.clone(),
            picture: picture.clone(),
            background: background.clone(),
            model: model.clone(),
            model_picked,
        };
        let name_input = cx.new(|cx| {
            let mut input = name_input(window, cx);
            input.set_value(name, window, cx);
            input
        });
        let instructions_input = cx.new(|cx| {
            let mut input = instructions_input(window, cx);
            input.set_value(instructions, window, cx);
            input
        });
        let (look, hue_sub) = ProfilePicture::new(
            picture,
            background,
            |this: &mut Self| this.form.as_mut().map(|form| &mut form.look),
            window,
            cx,
        );
        let _subs = [
            cx.subscribe_in(&name_input, window, |_, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    ui::cap_length(input, NAME_MAX, window, cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&instructions_input, window, |_, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    ui::cap_length(input, INSTRUCTIONS_MAX, window, cx);
                    cx.notify();
                }
            }),
            hue_sub,
        ];
        // A new agent starts at its name; an edited one at the page, no field taking the keys.
        let focus_name = id.is_none().then(|| name_input.clone());
        let page_focus = cx.focus_handle();
        let width = form_column_width(window, self.list_width());
        let shared = id.as_deref().and_then(|id| self.row(id)).and_then(|row| {
            let (session_id, name) = (row.session_id.clone()?, row.name.clone());
            Some(cx.new(|cx| SharedPanel::new(session_id, name, width, cx)))
        });
        let edited = id.clone();
        self.leave_current(None, cx);
        self.form = Some(Form {
            id,
            name: name_input,
            instructions: instructions_input,
            look,
            model,
            model_picked,
            locked: None,
            saved,
            saving: false,
            just_saved: false,
            focus: page_focus.clone(),
            shared,
            routines: None,
            scroll: ScrollHandle::new(),
            _subs,
        });
        // A one-off read: the profile shows the routines as they were when it opened.
        if let Some(agent) = edited {
            backend::call(
                cx,
                "routines:agentRoutines",
                json!({ "agentId": agent.clone() }),
                move |this: &mut Self, result: Result<Vec<Routine>, String>, cx| {
                    if let Some(form) = this.form.as_mut().filter(|form| form.id.as_deref() == Some(agent.as_str())) {
                        form.routines = Some(result);
                        cx.notify();
                    }
                },
            );
        }
        match focus_name {
            Some(name) => name.update(cx, |input, cx| input.focus(window, cx)),
            None => window.focus(&page_focus),
        }
        self.set_attending(self.attending, cx);
        cx.notify();
    }

    fn close_form(&mut self, cx: &mut Context<Self>) {
        self.form = None;
        self.back_to_setup = false;
        self.set_attending(self.attending, cx);
        cx.notify();
    }

    fn use_template(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let (name, text) = TEMPLATES[index];
        form.instructions.update(cx, |input, cx| input.set_value(text, window, cx));
        if form.name.read(cx).value().trim().is_empty() {
            form.name.update(cx, |input, cx| input.set_value(name, window, cx));
        }
        cx.notify();
    }

    fn pick_model(&mut self, choice: ModelChoice, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else { return };
        form.model = choice;
        form.model_picked = true;
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else { return };
        let name = form.name.read(cx).value().trim().to_string();
        let instructions = form.instructions.read(cx).value().trim().to_string();
        if name.is_empty() || instructions.is_empty() || form.saving {
            return;
        }
        let sent = form.current(cx);
        if form.id.is_some() && !form.saved.differs(&sent) {
            return;
        }
        // Both tools unplugged while the form was open: say so, and keep what was typed.
        if form.id.is_none()
            && let Some(reason) = creation_blocked(self.can_create.as_ref())
        {
            explain_blocked(reason, window, cx);
            return;
        }
        form.saving = true;
        let mut fields = json!({ "name": name, "instructions": instructions, "picture": form.look.picture });
        if let Some(background) = &form.look.background {
            fields["background"] = json!(background);
        }
        let (path, mut args) = match &form.id {
            // `null` resets an edited agent's colour to the theme's.
            Some(id) => (
                "agents:update",
                json!({ "id": id, "name": name, "instructions": instructions, "picture": form.look.picture, "background": form.look.background }),
            ),
            None => ("agents:create", fields),
        };
        if form.model_picked {
            let (engine, model) = saved_model(&form.model, &conversation::model_rows(cx));
            args["engine"] = json!(engine);
            args["model"] = json!(model);
        }
        let edited = form.id.clone();
        cx.notify();
        backend::call_in(window, cx, path, args, move |this: &mut Self, result: Result<Value, String>, window, cx| {
            match result {
                // An edit stays on the profile, now saved: the bar says so for a moment, then goes.
                Ok(_) if edited.is_some() => {
                    if let Some(form) = this.form.as_mut().filter(|form| form.id == edited) {
                        form.saving = false;
                        form.saved = sent.clone();
                        form.just_saved = true;
                        Self::fade_saved(cx);
                    }
                }
                // Made from the group's setup (it needs two agents): back to the setup, the new agent in it.
                Ok(_) if std::mem::take(&mut this.back_to_setup) => this.close_form(cx),
                Ok(value) => {
                    this.form = None;
                    if let Some(id) = value.as_str() {
                        this.open_agent(id.to_string(), window, cx);
                    }
                }
                Err(error) => {
                    if let Some(form) = &mut this.form {
                        form.saving = false;
                    }
                    ui::toast_error(window, cx, "Couldn’t save the agent", error);
                }
            }
            cx.notify();
        });
    }

    /// The save bar's "Saved", gone after a moment.
    fn fade_saved(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1600)).await;
            this.update(cx, |this, cx| {
                if let Some(form) = &mut this.form {
                    form.just_saved = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// An edited agent's form differs from what's saved.
    fn form_dirty(&self, cx: &App) -> bool {
        self.form.as_ref().is_some_and(|form| form.id.is_some() && form.saved.differs(&form.current(cx)))
    }

    /// Back to what's saved (the save bar's Revert, Esc).
    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.form else { return };
        let saved = form.saved.clone();
        form.name.update(cx, |input, cx| input.set_value(saved.name.clone(), window, cx));
        form.instructions.update(cx, |input, cx| input.set_value(saved.instructions.clone(), window, cx));
        form.look.reset(saved.picture, saved.background, window, cx);
        form.model = saved.model;
        form.model_picked = saved.model_picked;
        cx.notify();
    }

    /// Esc on the form: back to what's saved when something changed, else away from the form, as Cancel was.
    fn escape_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form_dirty(cx) {
            self.revert(window, cx);
        } else if !self.form.as_ref().is_some_and(|form| form.saving) {
            self.close_form(cx);
        }
    }

    /// Back from an agent's profile to the chat it was opened over (its chevron, ⌘← and ⌘[), the composer taking the
    /// keys; what changed and isn't saved is left, as a click on another agent in the list leaves it.
    fn back_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected.clone() {
            Some(id) => self.open_agent(id, window, cx),
            None => self.close_form(cx),
        }
    }

    /// ⌘← in a profile's field: back once the caret is at its line's start with nothing selected, else the field's own
    /// move to the line's start.
    fn back_at_line_start(&mut self, _: &BackAtLineStart, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return cx.propagate() };
        let Some(input) = [&form.name, &form.instructions]
            .into_iter()
            .find(|input| input.read(cx).focus_handle(cx).is_focused(window))
            .cloned()
        else {
            return cx.propagate();
        };
        let at_start = input.update(cx, |input, cx| {
            let selected = gpui::EntityInputHandler::selected_text_range(input, true, window, cx)
                .is_some_and(|selection| !selection.range.is_empty());
            input.cursor_position().character == 0 && !selected
        });
        if at_start {
            self.back_from_form(window, cx);
        } else {
            cx.propagate();
        }
    }

    /// Asks first; when the agent owns reminders or routines, also whether they go to the main agent or are deleted
    /// with it.
    /// The main agent: it leads the group and answers Ask on Feed posts.
    fn main_row(&self) -> Option<&AgentRow> {
        self.rows().iter().find(|row| row.main)
    }

    /// Makes `id` the main agent (`users:setMainAgent`), then `then`.
    fn make_main(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        backend::call_in(
            window,
            cx,
            "users:setMainAgent",
            json!({ "agentId": id }),
            move |this: &mut Self, result: Result<Value, String>, window, cx| match result {
                Ok(_) => then(this, window, cx),
                Err(error) => ui::toast_error(window, cx, "Couldn’t change the main agent", error),
            },
        );
    }

    /// Deleting the main agent: another becomes main first (picked here), then it's deleted as any other.
    fn confirm_delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.row(&id).is_some_and(|row| row.main) {
            return self.confirm_delete_agent(id, window, cx);
        }
        let name = self.row(&id).map(|row| row.name.clone()).unwrap_or_else(|| "This agent".into());
        let others: Vec<AgentRow> = self.rows().iter().filter(|row| row.id != id).cloned().collect();
        if others.is_empty() {
            ui::toast_error(
                window,
                cx,
                format!("{name} is your main agent"),
                "Make another agent first: it becomes your main agent, then you can delete this one.",
            );
            return;
        }
        let this = cx.entity().downgrade();
        let options: Vec<(String, SharedString, Option<&'static str>)> =
            others.iter().map(|row| (row.id.clone(), SharedString::from(row.name.clone()), None)).collect();
        let picked = Rc::new(std::cell::RefCell::new(others[0].id.clone()));
        window.open_dialog(cx, move |dialog, _, cx| {
            use gpui_component::dialog::DialogButtonProps;
            let (this, id, chosen, pick) = (this.clone(), id.clone(), picked.clone(), picked.clone());
            let current = picked.borrow().clone();
            dialog
                .title(div().headline().child("Pick another main agent first"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(space::MD))
                        .child(div().callout().text_color(cx.colors().text_secondary).child(format!(
                            "{name} is your main agent: it leads the group, {}has what’s no other agent’s. Who takes over?",
                            // Feed's Ask only while Feed is on (pages.rs).
                            if crate::pages::get(cx).feed { "answers Ask on Feed posts and " } else { "and " }
                        )))
                        .child(ui::segmented(
                            "delete-main-heir",
                            options.clone(),
                            &current,
                            move |value, window, _| {
                                *pick.borrow_mut() = value;
                                window.refresh();
                            },
                            cx,
                        )),
                )
                .confirm()
                .button_props(DialogButtonProps::default().ok_text("Continue").cancel_text("Cancel"))
                .on_ok(move |_, window, cx| {
                    let (heir, id) = (chosen.borrow().clone(), id.clone());
                    this.update(cx, |this, cx| this.make_main(heir, window, cx, move |this, window, cx| this.confirm_delete_agent(id, window, cx))).ok();
                    true
                })
        });
    }

    fn confirm_delete_agent(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.row(&id).map(|row| row.name.clone()).unwrap_or_else(|| "this agent".into());
        // What it has goes to the main agent.
        let filo =
            self.main_row().filter(|row| row.id != id).map(|row| row.name.clone()).unwrap_or_else(|| self.filo().0);
        let this = cx.entity().downgrade();
        backend::call_in(
            window,
            cx,
            "agents:scheduledCount",
            json!({ "id": id }),
            move |_: &mut Self, result: Result<ScheduledCount, String>, window, cx| {
                let count = result.unwrap_or_default();
                let title = format!("Delete {name}?");
                if count.reminders + count.routines == 0. {
                    ui::confirm(
                        window,
                        cx,
                        title,
                        "Its conversation goes too.",
                        "Delete",
                        "Cancel",
                        true,
                        move |window, cx| {
                            this.update(cx, |this, cx| this.delete(id.clone(), None, window, cx)).ok();
                        },
                    );
                    return;
                }
                use gpui_component::button::ButtonVariant;
                use gpui_component::dialog::DialogButtonProps;
                let message = format!("Its conversation goes too. {name} has {}.", count_text(count));
                let it = if count.reminders + count.routines == 1. { "it" } else { "them" };
                let options: Vec<(bool, SharedString, Option<&'static str>)> = vec![
                    (false, format!("Give {it} to {filo}").into(), None),
                    (true, format!("Delete {it} too").into(), None),
                ];
                let delete_too = Rc::new(Cell::new(false));
                window.open_dialog(cx, move |dialog, _, cx| {
                    let (this, id, chosen) = (this.clone(), id.clone(), delete_too.clone());
                    let pick = delete_too.clone();
                    dialog
                        .title(div().headline().child(title.clone()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(space::MD))
                                .child(div().callout().text_color(cx.colors().text_secondary).child(message.clone()))
                                .child(ui::segmented(
                                    "delete-scheduled",
                                    options.clone(),
                                    &delete_too.get(),
                                    move |value, window, _| {
                                        pick.set(value);
                                        window.refresh();
                                    },
                                    cx,
                                )),
                        )
                        .confirm()
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text("Delete")
                                .ok_variant(ButtonVariant::Danger)
                                .cancel_text("Cancel"),
                        )
                        .on_ok(move |_, window, cx| {
                            let scheduled = if chosen.get() { "delete" } else { "filo" };
                            this.update(cx, |this, cx| this.delete(id.clone(), Some(scheduled), window, cx)).ok();
                            true
                        })
                });
            },
        );
    }

    /// `scheduled`: "filo" (to the main agent) or "delete" for the agent's reminders and routines; none leaves it to the
    /// server (the main agent).
    fn delete(&mut self, id: String, scheduled: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        self.form = None;
        self.chats.retain(|(agent, _)| *agent != id);
        if self.selected.as_deref() == Some(id.as_str()) {
            self.selected = None;
        }
        cx.notify();
        let mut args = json!({ "id": id });
        if let Some(scheduled) = scheduled {
            args["scheduled"] = json!(scheduled);
        }
        backend::call_in(
            window,
            cx,
            "agents:remove",
            args,
            |_: &mut Self, result: Result<Value, String>, window, cx| {
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t delete the agent", error);
                }
            },
        );
    }

    /// The picture sheet: a pick shows in its preview first and goes to the form on Done; an upload is cropped there.
    pub fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (picture, background) = self
            .form
            .as_ref()
            .map(|form| (form.look.picture.clone(), form.look.background.clone()))
            .unwrap_or_default();
        self.open_sheet(picture, background, None, window, cx);
    }

    /// The picture sheet for a picture that isn't the form's, on `background`: Done gives `done` the one picked.
    pub fn open_picker_for(
        &mut self,
        picture: String,
        background: Option<String>,
        done: PictureDone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_sheet(picture, background, Some(done), window, cx);
    }

    fn open_sheet(
        &mut self,
        picture: String,
        background: Option<String>,
        done: Option<PictureDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let zoom = cx.new(|_| SliderState::new().min(1.).max(agent_picture::MAX_ZOOM).step(0.01).default_value(1.));
        let zoom_sub = cx.subscribe(&zoom, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(value) = event;
            if let Some(crop) = this.sheet.as_mut().and_then(|sheet| sheet.crop.as_mut()) {
                crop.set_zoom(value.start());
                cx.notify();
            }
        });
        self.sheet = Some(PictureSheet {
            picture,
            background,
            done,
            crop: None,
            reading: false,
            keeping: false,
            zoom,
            _zoom: zoom_sub,
        });
        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let Some(screen) = this.upgrade() else { return dialog };
            dialog
                // Eight tiles a row: all the animals, or all of a panda's poses.
                .w(px(690.))
                .overlay_closable(true)
                .title(div().headline().child("Picture"))
                .child(picture_sheet(&screen, cx))
        });
    }

    /// A picture in the sheet: its preview shows it (an upload being cropped is let go).
    fn sheet_pick(&mut self, picture: String, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.picture = picture;
            sheet.crop = None;
        }
        cx.notify();
    }

    /// Upload from your Mac: the file picker, then the picture in the preview to crop.
    fn upload_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = agent_picture::choose_file();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(path)) = chosen.await else { return };
            this.update(cx, |this, cx| {
                if let Some(sheet) = &mut this.sheet {
                    sheet.reading = true;
                }
                cx.notify();
            })
            .ok();
            let read = cx.background_executor().spawn(async move { agent_picture::Crop::load(&path) }).await;
            this.update_in(cx, |this, window, cx| {
                let Some(sheet) = &mut this.sheet else { return };
                sheet.reading = false;
                match read {
                    Ok(crop) => {
                        sheet.crop = Some(crop);
                        sheet.zoom.update(cx, |zoom, cx| zoom.set_value(1., window, cx));
                    }
                    Err(error) => ui::toast_error(window, cx, "Couldn’t open the picture", error.to_string()),
                }
                cx.notify();
                window.refresh();
            })
            .ok();
        })
        .detach();
    }

    /// Takes an upload off "Yours"; a form or sheet showing it goes back to the agent's saved picture.
    fn remove_upload(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let picture = crate::local_backend::upload_ref(&id);
        let saved = self.form.as_ref().map(|form| form.saved.picture.clone()).unwrap_or_default();
        if let Some(form) = self.form.as_mut().filter(|form| form.look.picture == picture) {
            form.look.picture = saved.clone();
        }
        if let Some(sheet) = self.sheet.as_mut().filter(|sheet| sheet.picture == picture) {
            sheet.picture = saved;
        }
        self.uploads.retain(|upload| upload.id != id);
        cx.notify();
        backend::call_in(
            window,
            cx,
            "agentPictures:remove",
            json!({ "id": id }),
            |_: &mut Self, result: Result<Value, String>, window, cx| {
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t remove the picture", error);
                }
            },
        );
    }

    /// Removing an avatar an agent made: asked first, as it can't be uploaded again. Agents and the pet showing it go back
    /// to the panda.
    fn confirm_remove_custom(&mut self, avatar: CustomAvatar, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        ui::confirm(
            window,
            cx,
            format!("Remove {}?", avatar.name),
            "Agents and the pet showing it go back to the panda.",
            "Remove",
            "Cancel",
            true,
            move |window, cx| {
                this.update(cx, |this, cx| this.remove_custom(avatar.id.clone(), window, cx)).ok();
            },
        );
    }

    fn remove_custom(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let picture = avatars::custom_ref(&id);
        let saved = self
            .form
            .as_ref()
            .map(|form| form.saved.picture.clone())
            .filter(|saved| *saved != picture)
            .unwrap_or_else(|| avatars::DEFAULT_AVATAR.into());
        if let Some(form) = self.form.as_mut().filter(|form| form.look.picture == picture) {
            form.look.picture = saved.clone();
        }
        if let Some(sheet) = self.sheet.as_mut().filter(|sheet| sheet.picture == picture) {
            sheet.picture = saved;
        }
        self.custom.retain(|avatar| avatar.id != id);
        cx.notify();
        backend::call_in(
            window,
            cx,
            "customAvatars:remove",
            json!({ "id": id }),
            |_: &mut Self, result: Result<Value, String>, window, cx| {
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t remove the avatar", error);
                }
            },
        );
    }

    /// Cancel: the sheet closes and the form keeps its picture.
    fn cancel_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sheet = None;
        window.close_dialog(cx);
        cx.notify();
    }

    /// Done: the sheet's picture goes to the form (saved with the agent, from the save bar). A cropped upload is kept
    /// first, and joins "Yours".
    fn finish_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.sheet else { return };
        let Some(crop) = sheet.crop.clone() else {
            let picture = sheet.picture.clone();
            let done = self.sheet.take().and_then(|sheet| sheet.done);
            self.sheet_done(done, picture, window, cx);
            window.close_dialog(cx);
            return;
        };
        if sheet.keeping || sheet.reading {
            return;
        }
        sheet.keeping = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let png = cx.background_executor().spawn(async move { crop.png() }).await;
            this.update_in(cx, |this, window, cx| {
                match png {
                    Ok(png) => {
                        let args =
                            json!({ "png": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png) });
                        backend::call_in(
                            window,
                            cx,
                            "agentPictures:add",
                            args,
                            |this: &mut Self, result: Result<String, String>, window, cx| {
                                match result {
                                    Ok(id) if this.sheet.is_some() => {
                                        let done = this.sheet.take().and_then(|sheet| sheet.done);
                                        this.sheet_done(done, crate::local_backend::upload_ref(&id), window, cx);
                                        window.close_dialog(cx);
                                    }
                                    Ok(_) => {}
                                    Err(error) => {
                                        if let Some(sheet) = &mut this.sheet {
                                            sheet.keeping = false;
                                        }
                                        ui::toast_error(window, cx, "Couldn’t keep the picture", error);
                                    }
                                }
                                cx.notify();
                            },
                        );
                    }
                    Err(error) => {
                        if let Some(sheet) = &mut this.sheet {
                            sheet.keeping = false;
                        }
                        ui::toast_error(window, cx, "Couldn’t crop the picture", error.to_string());
                    }
                }
                cx.notify();
                window.refresh();
            })
            .ok();
        })
        .detach();
    }

    /// Near the end of the form page, the shared list loads older items.
    fn on_form_scroll(&mut self, _: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.form else { return };
        let Some(shared) = form.shared.clone() else { return };
        let scrolled = -form.scroll.offset().y;
        if form.scroll.max_offset().height - scrolled < px(SHARED_NEAR_END) {
            shared.update(cx, |shared, cx| shared.load_older(cx));
        }
    }

    /// The sheet's Done: its picture to `done`, or else to the form.
    fn sheet_done(&mut self, done: Option<PictureDone>, picture: String, window: &mut Window, cx: &mut Context<Self>) {
        match done {
            Some(done) => done(picture, window, cx),
            None => {
                if let Some(form) = &mut self.form {
                    form.look.picture = picture;
                } else if let Some(setup) = &mut self.setup {
                    setup.look.picture = picture;
                }
            }
        }
        cx.notify();
    }

    fn set_colors_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let look = match (&mut self.form, &mut self.setup) {
            (Some(form), _) => Some(&mut form.look),
            (None, Some(setup)) => Some(&mut setup.look),
            _ => None,
        };
        if look.is_some_and(|look| look.set_colors_open(open)) {
            cx.notify();
        }
    }
}

impl Form {
    fn current(&self, cx: &App) -> Saved {
        Saved {
            name: self.name.read(cx).value().to_string(),
            instructions: self.instructions.read(cx).value().to_string(),
            picture: self.look.picture.clone(),
            background: self.look.background.clone(),
            model: self.model.clone(),
            model_picked: self.model_picked,
        }
    }
}

/// The width inside the form's column: the pane right of the sidebar and the list, capped, less the column's padding.
/// The model an agent keeps: its engine's default saved as the model it is now, so the agent stays on it when the CLI's
/// default changes (as the engine's own default while the CLI hasn't said which model that is).
/// An agent's menu (its row's right click, its chat header's "…"): make it the main agent, edit it, delete it.
fn agent_menu(
    menu: gpui_component::menu::PopupMenu,
    screen: WeakEntity<AgentsScreen>,
    row: &AgentRow,
) -> gpui_component::menu::PopupMenu {
    use gpui_component::menu::PopupMenuItem;
    let (main, edit, delete) = (screen.clone(), screen.clone(), screen);
    let (main_id, edit_id, delete_id) = (row.id.clone(), row.id.clone(), row.id.clone());
    menu.item(
        PopupMenuItem::new(if row.main { "Main agent" } else { "Make main agent" })
            .checked(row.main)
            .disabled(row.main)
            .on_click(move |_, window, cx| {
                let id = main_id.clone();
                main.update(cx, |this, cx| this.make_main(id, window, cx, |_, _, _| {})).ok();
            }),
    )
    .separator()
    .item(PopupMenuItem::new("Edit agent").on_click(move |_, window, cx| {
        edit.update(cx, |this, cx| this.edit_agent(edit_id.clone(), window, cx)).ok();
    }))
    .item(PopupMenuItem::new("Delete agent").on_click(move |_, window, cx| {
        delete.update(cx, |this, cx| this.confirm_delete(delete_id.clone(), window, cx)).ok();
    }))
}

pub fn saved_model((engine, model): &ModelChoice, rows: &[filo_local::engine::ModelOption]) -> ModelChoice {
    let model = model
        .clone()
        .or_else(|| rows.iter().find(|row| row.engine == *engine && row.default).map(|row| row.id.clone()));
    (*engine, model)
}

fn form_column_width(window: &Window, list_width: f32) -> f32 {
    let pane = f32::from(window.viewport_size().width) - crate::app::SIDEBAR_WIDTH - list_width;
    pane.min(FORM_WIDTH) - 2. * space::XXXL
}

/// The width of Group settings' column: the pane right of the sidebar and the list less the page's padding, capped.
fn setup_column_width(window: &Window, list_width: f32) -> f32 {
    let pane = f32::from(window.viewport_size().width) - crate::app::SIDEBAR_WIDTH - list_width;
    (pane - 2. * space::XXXL).min(SETUP_WIDTH)
}

/// The custom colour's swatch: a few hues blended on a dot.
fn rainbow_dot() -> gpui::Div {
    let hue = |degrees: f32, alpha: f32| gpui::hsla(degrees / 360., 0.8, 0.6, alpha);
    let layer = |angle: f32, degrees: f32| {
        div().absolute().inset_0().rounded(px(radius::FULL)).bg(gpui::linear_gradient(
            angle,
            gpui::linear_color_stop(hue(degrees, 1.), 0.),
            gpui::linear_color_stop(hue(degrees, 0.), 0.7),
        ))
    };
    div()
        .relative()
        .size_full()
        .rounded(px(radius::FULL))
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(hue(330., 1.), 0.),
            gpui::linear_color_stop(hue(215., 1.), 1.),
        ))
        .child(layer(90., 48.))
        .child(layer(270., 140.))
}

/// The picture sheet: the preview on top (the picked picture on the agent's colour, or an upload to crop), Upload from
/// your Mac, then the pictures: the user's own, the animals, the picked animal's poses, the helpers; Cancel and Done.
fn picture_sheet(screen: &Entity<AgentsScreen>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let state = screen.read(cx);
    let Some(sheet) = &state.sheet else { return div().into_any_element() };
    let weak = screen.downgrade();
    let current = sheet.picture.as_str();
    let fill = background_color(sheet.background.as_deref()).unwrap_or(colors.surface_strong);
    let preview = match &sheet.crop {
        Some(crop) => {
            crop_view(weak.clone(), crop, fill, gpui_component::ActiveTheme::theme(cx).background).into_any_element()
        }
        None => picture_on(picture_source(current), Some(fill), agent_picture::PREVIEW, cx).into_any_element(),
    };
    let controls = {
        let upload = weak.clone();
        let button = Button::secondary("picture-upload", "Upload from your Mac")
            .small()
            .icon("image-plus")
            .loading(sheet.reading)
            .on_click(move |_: &ClickEvent, window, cx| {
                upload.update(cx, |this, cx| this.upload_picture(window, cx)).ok();
            });
        let zoom = sheet.crop.is_some().then(|| {
            div()
                .w(px(240.))
                .flex()
                .flex_col()
                .gap(px(space::XS))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(ui::icon_sized("minus", 13., colors.text_secondary))
                        .child(div().flex_1().child(Slider::new(&sheet.zoom)))
                        .child(ui::icon_sized("plus", 13., colors.text_secondary)),
                )
                .child(
                    div()
                        .footnote()
                        .text_color(colors.text_secondary)
                        .child("Drag the picture to move it; the slider zooms."),
                )
        });
        div().flex().flex_col().items_start().gap(px(space::MD)).child(button).children(zoom)
    };
    let tile = |key: String, source: ImageSource, picture: String, selected: bool| {
        let screen = weak.clone();
        div()
            .id(SharedString::from(format!("pick-{key}")))
            .relative()
            .size(px(72.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(radius::MD))
            .border_2()
            .border_color(if selected { colors.accent } else { gpui::transparent_black() })
            .bg(if selected { colors.accent_soft } else { colors.surface })
            .cursor_pointer()
            .when(!selected, |this| this.hover(|style| style.bg(colors.selected_fill)))
            .child(avatars::picture(source, 56.))
            .on_click(move |_: &ClickEvent, window, cx| {
                screen.update(cx, |screen, cx| screen.sheet_pick(picture.clone(), cx)).ok();
                window.refresh();
            })
    };
    let section = |title: &str, tiles: Vec<gpui::Stateful<gpui::Div>>| {
        div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(div().footnote().semibold().text_color(colors.text_secondary).child(title.to_string()))
            .child(div().flex().flex_wrap().gap(px(space::SM)).children(tiles))
    };
    let cropping = sheet.crop.is_some();
    let chosen = |picture: &str| !cropping && picture == current;
    // A tile of Yours with its × (on hover).
    let removable =
        |key: String,
         source: ImageSource,
         picture: String,
         remove: Box<dyn Fn(&mut AgentsScreen, &mut Window, &mut Context<AgentsScreen>)>| {
            let (group, screen) = (SharedString::from(key.clone()), weak.clone());
            tile(key.clone(), source, picture.clone(), chosen(&picture)).group(group.clone()).child(
                div()
                    .id(SharedString::from(format!("remove-{key}")))
                    .absolute()
                    .top(px(-6.))
                    .right(px(-6.))
                    .size(px(20.))
                    .rounded(px(radius::FULL))
                    .bg(colors.raised)
                    .border_1()
                    .border_color(colors.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Remove").build(window, cx))
                    .child(ui::icon_sized("x", 11., colors.text))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        screen.update(cx, |this, cx| remove(this, window, cx)).ok();
                        window.refresh();
                    }),
            )
        };
    // Yours: the pictures uploaded here, newest first, then the avatars agents made for the user.
    let mut yours: Vec<gpui::Stateful<gpui::Div>> = state
        .uploads
        .iter()
        .map(|upload| {
            let (picture, id) = (crate::local_backend::upload_ref(&upload.id), upload.id.clone());
            removable(
                format!("upload-{}", upload.id),
                picture_source(&picture),
                picture,
                Box::new(move |this, window, cx| this.remove_upload(id.clone(), window, cx)),
            )
        })
        .collect();
    yours.extend(state.custom.iter().map(|avatar| {
        let removed = avatar.clone();
        let picture = avatars::custom_ref(&avatar.id);
        removable(
            format!("custom-{}", avatar.id),
            avatar.poses.idle.image(),
            picture,
            Box::new(move |this, window, cx| this.confirm_remove_custom(removed.clone(), window, cx)),
        )
    }));
    let mut sections = Vec::new();
    // The group's setup: its own picture, to go back to.
    if sheet.done.is_none() && state.form.is_none() && state.setup.is_some() {
        sections.push(section(
            "Group",
            vec![tile(GROUP_PICTURE.to_string(), group_picture(), GROUP_PICTURE.to_string(), chosen(GROUP_PICTURE))],
        ));
    }
    if !yours.is_empty() {
        sections.push(section("Yours", yours));
    }
    let animal = avatars::animal_of(current).filter(|_| avatars::is_avatar_id(current));
    let animals = ANIMALS
        .iter()
        .map(|item| {
            let id = if avatars::is_avatar_id(current) {
                avatars::avatar_for_animal(item, current)
            } else {
                item.avatar_id("waving")
            };
            tile(
                format!("animal-{}", item.id),
                avatars::animal_image(&item.avatar_id("waving")),
                id,
                !cropping && animal.is_some_and(|animal| animal.id == item.id),
            )
        })
        .collect();
    sections.push(section("Avatars", animals));
    if let Some(animal) = animal {
        let poses = animal
            .poses
            .iter()
            .map(|(pose, _)| animal.avatar_id(pose))
            .map(|id| tile(id.clone(), avatars::animal_image(&id), id.clone(), chosen(&id)))
            .collect();
        sections.push(section("Poses", poses));
    }
    let helpers = HELPERS
        .iter()
        .map(|name| {
            let picture = format!("helper:{name}");
            tile(picture.clone(), picture_source(&picture), picture.clone(), chosen(&picture))
        })
        .collect();
    sections.push(section("Helpers", helpers));
    let (cancel, done) = (weak.clone(), weak);
    div()
        .flex()
        .flex_col()
        .gap(px(space::LG))
        .child(div().flex().items_center().gap(px(space::XL)).child(preview).child(controls))
        .child(div().h(px(1.)).bg(colors.border))
        .child(
            div()
                .id("agent-picker")
                .max_h(px(400.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(space::LG))
                .pt(px(space::XS))
                .pb(px(space::SM))
                .children(sections),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(space::SM))
                .child(Button::ghost("picture-cancel", "Cancel").on_click(move |_: &ClickEvent, window, cx| {
                    cancel.update(cx, |this, cx| this.cancel_sheet(window, cx)).ok();
                }))
                .child(
                    Button::primary("picture-done", "Done").disabled(sheet.reading).loading(sheet.keeping).on_click(
                        move |_: &ClickEvent, window, cx| {
                            done.update(cx, |this, cx| this.finish_sheet(window, cx)).ok();
                        },
                    ),
                ),
        )
        .into_any_element()
}

/// An upload under the preview circle: drawn at its zoom and place, the square around the circle masked in the
/// sheet's colour; dragging moves it.
fn crop_view(
    screen: WeakEntity<AgentsScreen>,
    crop: &agent_picture::Crop,
    fill: gpui::Hsla,
    mask: gpui::Hsla,
) -> impl IntoElement {
    let size = agent_picture::PREVIEW;
    let (left, top) = crop.offset();
    let (width, height) = crop.shown_size();
    // A ring round the circle as wide as the square's corners stick out past it ((√2 − 1) × the radius, and some).
    let ring = size / 2. * 0.5;
    let mut cover = div()
        .absolute()
        .left(px(-ring))
        .top(px(-ring))
        .size(px(size + 2. * ring))
        .rounded(px(radius::FULL))
        .border_color(mask);
    {
        let widths = &mut gpui::Styled::style(&mut cover).border_widths;
        let width = Some(px(ring).into());
        (widths.top, widths.right, widths.bottom, widths.left) = (width, width, width, width);
    }
    let (start, drag) = (screen.clone(), screen);
    div()
        .id("picture-crop")
        .relative()
        .size(px(size))
        .flex_none()
        .overflow_hidden()
        .bg(fill)
        .cursor(gpui::CursorStyle::OpenHand)
        .child(
            gpui::img(ImageSource::Render(crop.shown.clone()))
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height)),
        )
        .child(cover)
        .on_mouse_down(MouseButton::Left, move |event: &gpui::MouseDownEvent, _, cx| {
            let at = (f32::from(event.position.x), f32::from(event.position.y));
            start
                .update(cx, |this, _| {
                    this.sheet.as_mut().and_then(|sheet| sheet.crop.as_mut()).map(|crop| crop.start_drag(at))
                })
                .ok();
        })
        .on_drag(agent_picture::CropDrag, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_drag_move(move |event: &DragMoveEvent<agent_picture::CropDrag>, window, cx| {
            let at = (f32::from(event.event.position.x), f32::from(event.event.position.y));
            drag.update(cx, |this, cx| {
                if let Some(crop) = this.sheet.as_mut().and_then(|sheet| sheet.crop.as_mut()) {
                    crop.drag_to(at);
                    cx.notify();
                }
            })
            .ok();
            window.refresh();
        })
}

impl AgentsScreen {
    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let now = format::now_ms();
        // The open chat's row is highlighted, or with a profile open, its agent's (none for a new agent).
        let (selected, reading) = match &self.form {
            Some(form) => (form.id.clone(), None),
            None => (self.selected.clone(), self.selected.clone()),
        };
        // The open agent still answers the pointer, a shade stronger than its highlight.
        let mut selected_hover = colors.selected_fill;
        selected_hover.a = (selected_hover.a * 1.6).min(1.);
        // A chat with work running says "Working 12s" in place of its last message, as the Chats list does.
        let mut working_since = Vec::new();
        // Each row's question and last message's time, for its place in the list.
        let mut lasts: Vec<(String, bool, f64)> = Vec::new();
        let marks = self.tool_marks_opacity(window);
        // Chats left with a draft go on top, under those needing input, the group's too, and say so under the name
        // (chat/drafts.rs).
        self.sync_drafts(cx);
        let group_row = self.group_row().cloned().map(|group| {
            let active = selected.as_deref() == Some(GROUP);
            let unseen = group.unseen > 0. && !(reading.as_deref() == Some(GROUP) && self.attending);
            let busy = self.list_working(GROUP, cx);
            lasts.push((GROUP.to_string(), group.needs_input, group.last.as_ref().map_or(0., |last| last.at)));
            let working = if group.needs_input { None } else { busy };
            working_since.extend(working);
            let preview = match working {
                Some(since) => Some(reply_status::list_line("agent-group", since, cx)),
                None if group.needs_input => Some(reply_status::needs_input_line(cx)),
                None if group.reply_failed => Some(reply_status::failed_line(cx)),
                None => group
                    .last
                    .as_ref()
                    .map(|last| self.preview_line(format!("{}: {}", last.speaker, last.preview), unseen, cx)),
            };
            let draft = self.drafts.preview(GROUP);
            let preview =
                drafts::with_line(draft, group.needs_input || working.is_some() || group.reply_failed, preview, cx);
            self.list_row(
                "agent-group",
                active,
                unseen,
                group.tile(ROW_PICTURE, cx).into_any_element(),
                group.title().into(),
                group.last.as_ref().map(|last| format::list_time(last.at, now)),
                preview,
                selected_hover,
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_agent(GROUP.to_string(), window, cx)))
            .into_any_element()
        });
        // Off, it's still on top: its picture faded, and Off under its name; it opens its setup.
        let group_row = group_row.or_else(|| {
            let settings = self.group_settings.clone()?;
            let active = selected.as_deref() == Some(GROUP);
            lasts.push((GROUP.to_string(), false, 0.));
            let tile = agent_picture(
                picture_source(settings.picture.as_deref().unwrap_or(GROUP_PICTURE)),
                settings.background.as_deref(),
                ROW_PICTURE,
                cx,
            )
            .opacity(0.5);
            let chip = div()
                .flex()
                .child(
                    div()
                        .px(px(space::XS + 2.))
                        .rounded(px(radius::FULL))
                        .bg(colors.surface_strong)
                        .caption()
                        .semibold()
                        .text_color(colors.text_tertiary)
                        .child("Off"),
                )
                .into_any_element();
            let row = self.list_row(
                "agent-group",
                active,
                false,
                tile.into_any_element(),
                group_title(settings.name.as_deref()).into(),
                None,
                Some(chip),
                selected_hover,
                cx,
            );
            Some(
                crate::smoke::tracked(
                    "agent-group",
                    row.on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.open_agent(GROUP.to_string(), window, cx)),
                    ),
                )
                .into_any_element(),
            )
        });
        let rows: Vec<(String, AnyElement)> = self
            .rows()
            .iter()
            .map(|row| {
                let id = row.id.clone();
                let active = selected.as_deref() == Some(row.id.as_str());
                let unseen = row.unseen > 0. && !(reading.as_deref() == Some(row.id.as_str()) && self.attending);
                let busy = self.list_working(&row.id, cx);
                lasts.push((row.id.clone(), row.needs_input, row.last.as_ref().map_or(0., |last| last.at)));
                let working = if row.needs_input { None } else { busy };
                working_since.extend(working);
                let preview = match working {
                    Some(since) => Some(reply_status::list_line(&format!("agent-{}", row.id), since, cx)),
                    None if row.needs_input => Some(reply_status::needs_input_line(cx)),
                    None if row.reply_failed => Some(reply_status::failed_line(cx)),
                    None if row.needs_newer_filo => Some(self.preview_line("Needs a newer Filo".into(), false, cx)),
                    None => row.last.as_ref().map(|last| {
                        self.preview_line(
                            if last.from_user { format!("You: {}", last.preview) } else { last.preview.clone() },
                            unseen,
                            cx,
                        )
                    }),
                };
                let draft = self.drafts.preview(&row.id);
                let preview =
                    drafts::with_line(draft, row.needs_input || working.is_some() || row.reply_failed, preview, cx);
                let engine = row.engine.or(row.locked_engine).unwrap_or_else(crate::local_backend::default_engine);
                let mark = tool_mark(&row.id, engine, marks, active, cx);
                let open_id = id.clone();
                let element = div()
                    .id(SharedString::from(format!("agent-{}", row.id)))
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .px(px(space::MD))
                    .py(px(space::SM))
                    .rounded(px(radius::MD))
                    .cursor_pointer()
                    .when(active, |this| this.bg(colors.selected_fill).hover(move |style| style.bg(selected_hover)))
                    .when(!active, |this| this.hover(|style| style.bg(colors.selected_fill.opacity(0.5))))
                    .child(row.tile(ROW_PICTURE, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(space::SM))
                                    .child(div().min_w_0().callout().semibold().truncate().child(row.name.clone()))
                                    // The main agent (its menu's Make main agent): it leads the group and answers Ask.
                                    .when(row.main, |this| {
                                        this.child(
                                            div()
                                                .id(SharedString::from(format!("agent-main-{}", row.id)))
                                                .flex_none()
                                                .px(px(space::XS + 2.))
                                                .rounded(px(radius::FULL))
                                                .bg(colors.accent_soft)
                                                .caption()
                                                .semibold()
                                                .text_color(colors.on_accent_soft)
                                                .child("Main"),
                                        )
                                    })
                                    .child(div().flex_1())
                                    .when_some(row.last.as_ref(), |this, last| {
                                        this.child(list_time(format::list_time(last.at, now), unseen, &colors))
                                    }),
                            )
                            .when(preview.is_some() || mark.is_some(), |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(space::SM))
                                        .child(div().flex_1().min_w_0().overflow_hidden().children(preview))
                                        .children(mark),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_agent(open_id.clone(), window, cx)
                    }));
                let element = {
                    use gpui_component::menu::ContextMenuExt as _;
                    let (weak, row) = (cx.entity().downgrade(), row.clone());
                    crate::smoke::tracked(
                        format!("agent-row-{}", row.id),
                        element.context_menu(move |menu, _, _| agent_menu(menu, weak.clone(), &row)),
                    )
                };
                (id, element)
            })
            .collect();
        let mut items: HashMap<String, AnyElement> =
            group_row.map(|row| (GROUP.to_string(), row)).into_iter().chain(rows).collect();
        // Each row slides from where it showed to its new place.
        let rows = self.place_rows(lasts, window).into_iter().filter_map(|key| {
            let row = items.remove(&key)?;
            Some((SharedString::from(key), row))
        });
        // The list's background, which the rows on their way are drawn on.
        let background = colors.background;
        let list = crate::sliding_rows::sliding_rows("agents-rows", background, rows);
        self.list_newest = working_panel::newest_time(working_since);
        div()
            .relative()
            .w(px(self.list_width()))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(background)
            .border_r_1()
            .border_color(colors.surface_strong)
            .child(
                crate::chrome::drag_region(div().id("agents-list-header"))
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .pl(px(space::LG))
                    .pr(px(space::SM))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::XS))
                            .child(div().title3().bold().child("Agents"))
                            .child(self.render_view_menu(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::XXS))
                            // ⌘P's search box, through the same action as the key (chrome.rs).
                            .child(crate::smoke::tracked(
                                "agents-search",
                                IconButton::new("agents-search", "search").small().tooltip("Search ⌘P").on_click(
                                    |_, window, cx| window.dispatch_action(Box::new(crate::chrome::Search), cx),
                                ),
                            ))
                            .child(
                                IconButton::new("new-agent", "square-pen").small().tooltip("New agent").on_click(
                                    cx.listener(|this, _: &ClickEvent, window, cx| this.new_agent(window, cx)),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .id("agents-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(space::SM))
                    .pb(px(space::MD))
                    .child(list),
            )
            .child(self.watch_pointer(cx))
            .child(self.render_edge(cx))
    }

    /// The ⌄ after the list's title: what the list shows. The list keeps its own order (`arrange`), so there's no
    /// sorting here.
    fn render_view_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::button::ButtonVariants as _;
        use gpui_component::menu::PopupMenuItem;
        let colors = *cx.colors();
        let weak = cx.entity().downgrade();
        let on = self.tool_marks;
        // The header drags the window: a press here is the menu's.
        div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(crate::smoke::tracked(
            "agents-view-menu",
            MenuButton::new("agents-view-menu")
                .ghost()
                .xsmall()
                .icon(gpui_component::Icon::empty().path("icons/chevron-down.svg").text_color(colors.text_secondary))
                .dropdown_menu_with_anchor(gpui::Corner::TopLeft, move |menu, _, _| {
                    let weak = weak.clone();
                    menu.min_w(px(200.)).item(PopupMenuItem::new("Show tool logos").checked(on).on_click(
                        move |_, _, cx| {
                            weak.update(cx, |this, cx| this.set_tool_marks(!on, cx)).ok();
                        },
                    ))
                }),
        ))
    }

    /// Puts the rows in their order (`arrange`), or holds the one shown while the pointer is over the list (all but the
    /// rows whose draft came or went: the user's doing): the rows' keys in the order they show.
    fn place_rows(&mut self, lasts: Vec<(String, bool, f64)>, window: &mut Window) -> Vec<String> {
        let main = self.main_row().map(|row| row.id.clone());
        let drafted: Vec<&str> = self.drafts.keys().collect();
        let moved: Vec<String> = drafted
            .iter()
            .filter(|key| !self.placed_drafts.iter().any(|placed| placed == *key))
            .map(|key| key.to_string())
            .chain(self.placed_drafts.iter().filter(|placed| !drafted.contains(&placed.as_str())).cloned())
            .collect();
        let places: Vec<Place> = lasts
            .into_iter()
            .map(|(key, needs_input, last)| Place {
                pinned: if key == GROUP {
                    Some(0)
                } else if main.as_ref() == Some(&key) {
                    Some(1)
                } else {
                    None
                },
                needs_input,
                draft: drafted.contains(&key.as_str()),
                last,
                key,
            })
            .collect();
        self.target = arrange(&places);
        let hold = self.pointer.over(window.mouse_position());
        self.pointer.held.set(hold);
        let shown = if hold { held(&self.shown, &self.target, &moved) } else { self.target.clone() };
        self.placed_drafts = self.drafts.keys().map(str::to_string).collect();
        self.shown = shown;
        self.shown.clone()
    }

    /// Follows the pointer for the list (`ListPointer`): leaving it, or the window, while the list holds its order
    /// redraws the list in its new one.
    fn watch_pointer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pointer = self.pointer.clone();
        let screen = cx.entity().downgrade();
        gpui::canvas(
            |bounds, _, _| bounds,
            move |_, bounds, window, _| {
                pointer.bounds.set(Some(bounds));
                let release = {
                    let pointer = pointer.clone();
                    move |cx: &mut App| {
                        if pointer.held.replace(false) {
                            screen.update(cx, |_, cx| cx.notify()).ok();
                        }
                    }
                };
                window.on_mouse_event({
                    let (pointer, release) = (pointer.clone(), release.clone());
                    move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Capture {
                            pointer.away.set(false);
                            if !pointer.over(event.position) {
                                release(cx);
                            }
                        }
                    }
                });
                window.on_mouse_event(move |_: &gpui::MouseExitEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        pointer.away.set(true);
                        release(cx);
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    /// The list's right edge: drag it to resize the list, double-click it for the usual width. A strip a few points
    /// wide over the border, above the header's window drag, with the resize cursor; the line shows while hovered or
    /// dragged.
    fn render_edge(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let dragging = self.list_width_unsaved && cx.has_active_drag();
        let edge = div()
            .id("agents-list-edge")
            .absolute()
            .inset_0()
            .occlude()
            .cursor_col_resize()
            .flex()
            .justify_center()
            .group("agents-list-edge")
            .child(
                div()
                    .w(px(2.))
                    .h_full()
                    .when(dragging, |this| this.bg(colors.accent))
                    .when(!dragging, |this| this.group_hover("agents-list-edge", |style| style.bg(colors.border))),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    this.reset_list_width(cx);
                }
            }))
            .on_drag(ListEdge, |edge, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| edge.clone())
            });
        // Absolute children sit in their parent: the strip fills this one, with or without the checks' wrapper.
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(-4.))
            .w(px(8.))
            .flex()
            .child(crate::smoke::tracked_fill("agents-list-edge", edge))
    }

    /// A row of the list: picture, name and time, and the last message under them; `unseen` marks the time (`list_time`).
    #[allow(clippy::too_many_arguments)]
    fn list_row(
        &self,
        id: &str,
        active: bool,
        unseen: bool,
        picture: AnyElement,
        name: SharedString,
        time: Option<String>,
        preview: Option<AnyElement>,
        selected_hover: gpui::Hsla,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = *cx.colors();
        div()
            .id(SharedString::from(id.to_string()))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .px(px(space::MD))
            .py(px(space::SM))
            .rounded(px(radius::MD))
            .cursor_pointer()
            .when(active, |this| this.bg(colors.selected_fill).hover(move |style| style.bg(selected_hover)))
            .when(!active, |this| this.hover(|style| style.bg(colors.selected_fill.opacity(0.5))))
            .child(picture)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().flex_1().min_w_0().callout().semibold().truncate().child(name))
                            .when_some(time, |this, time| this.child(list_time(time, unseen, &colors))),
                    )
                    .children(preview),
            )
    }

    /// A row's last message, under its name: in the text's own colour while it's unread, softer once read. Clamped to a
    /// line rather than kept on one (`truncate`): gpui then measures it again at the width it gets, so it ends in "…"
    /// where it's cut instead of mid-letter.
    fn preview_line(&self, preview: String, unseen: bool, cx: &App) -> AnyElement {
        let colors = cx.colors();
        let color = if unseen { colors.text } else { colors.text_secondary };
        div().footnote().text_color(color).line_clamp(1).text_ellipsis().child(preview).into_any_element()
    }

    /// The group: Filo and the agents talking, for the user to read.
    fn render_group(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let view = self.group_conversation(window, cx);
        let empty = self.group_row().is_none_or(|group| group.last.is_none());
        let (_, filo_picture) = self.filo();
        let group = self.group_row().cloned();
        // Its name and picture as the user set them, in the header and over an empty group.
        let title = group_title(group.as_ref().and_then(|group| group.name.as_deref()));
        let tile = |size: f32| match &group {
            Some(group) => group.tile(size, cx),
            None => round_picture(group_picture(), size, cx),
        };
        let (header_tile, intro_tile) = (tile(32.), tile(112.));
        // The one leading first, then Filo (while it's in the group), then the others.
        let lead = group.as_ref().and_then(|group| group.lead.clone()).filter(|lead| lead != "filo");
        let members = group.as_ref().map(|group| group.members.clone()).unwrap_or_default();
        let face = |member: &GroupMember| (picture_source(&member.picture), member.background.clone());
        let faces: Vec<(ImageSource, Option<String>)> = members
            .iter()
            .filter(|member| Some(&member.id) == lead.as_ref())
            .map(face)
            .chain(group.as_ref().is_none_or(|group| group.filo).then_some((filo_picture, None)))
            .chain(members.iter().filter(|member| Some(&member.id) != lead.as_ref()).map(face))
            .take(7)
            .collect();
        // The picture and name open Group settings; so does its ⋯, which also turns the group off.
        let header = crate::chrome::drag_region(div().id("group-header"))
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(space::SM))
            .pr(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(crate::smoke::tracked(
                "edit-group",
                div()
                    .id("edit-group")
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .py(px(space::XS))
                    .pl(px(space::XS))
                    .pr(px(space::MD))
                    .rounded(px(radius::FULL))
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.surface))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(header_tile)
                    .child(div().min_w_0().headline().truncate().child(title.clone()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.edit_group(window, cx))),
            ))
            .child(div().flex_1())
            .child(div().flex().children(faces.into_iter().enumerate().map(|(index, (face, background))| {
                div()
                    .when(index > 0, |this| this.ml(px(-8.)))
                    .rounded(px(radius::FULL))
                    .border_2()
                    .border_color(colors.chat_background)
                    .child(agent_picture(face, background.as_deref(), 24., cx))
            })))
            .child({
                use gpui_component::button::ButtonVariants as _;
                use gpui_component::menu::PopupMenuItem;
                let weak = cx.entity().downgrade();
                crate::smoke::tracked(
                    "group-menu",
                    div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                        MenuButton::new("group-menu")
                            .ghost()
                            .small()
                            .icon(
                                gpui_component::Icon::empty()
                                    .path("icons/ellipsis.svg")
                                    .text_color(colors.text_secondary),
                            )
                            .dropdown_menu_with_anchor(gpui::Corner::TopRight, move |menu, _, _| {
                                let (edit, off) = (weak.clone(), weak.clone());
                                menu.item(PopupMenuItem::new("Group settings").on_click(move |_, window, cx| {
                                    edit.update(cx, |this, cx| this.edit_group(window, cx)).ok();
                                }))
                                .separator()
                                .item(PopupMenuItem::new("Turn off group").on_click(
                                    move |_, window, cx| {
                                        off.update(cx, |this, cx| this.confirm_turn_off(window, cx)).ok();
                                    },
                                ))
                            }),
                    ),
                )
            });
        let intro = empty.then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::MD))
                .child(intro_tile)
                .child(div().title2().child(title.clone()))
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(colors.chat_background)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(match view {
                        Some(view) => view.into_any_element(),
                        None => div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::spinner(20., cx))
                            .into_any_element(),
                    })
                    .children(intro),
            )
            .into_any_element()
    }

    /// The group's setup or Group settings (`GroupSetup`). The setup: the step in a column in the middle, onboarding's
    /// footer under it (its dots, Back, Continue then Turn on). Group settings: one page from the top (`settings_page`),
    /// Cancel and Save under it. Either scrolls as one area when its members don't fit.
    fn render_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let Some(setup) = &self.setup else { return div().flex_1().into_any_element() };
        let (step, settings, saving) = (setup.step, setup.settings, setup.saving);
        let scroll = setup.scroll.clone();
        crate::smoke::keep_page_scroll("group-settings", &scroll);
        if setup.look.colors_open() {
            crate::escape::popup(window, cx, |this, _, cx| this.set_colors_open(false, cx));
        }
        // Esc leaves Group settings as Cancel does.
        if settings {
            crate::escape::layer(window, cx, |this, _, cx| this.close_setup(cx));
        }
        let enough = self.setup_members().len() >= 2;
        let pieces = if settings {
            let levels = self.route_levels(cx);
            self.settings_page(levels, window, cx)
        } else if step == 0 {
            self.setup_identity(window, cx)
        } else {
            let levels = self.route_levels(cx);
            self.setup_routing(levels, cx)
        };
        let dots = (!settings).then(|| {
            crate::screens::welcome::step_dots(2, step, cx).into_iter().enumerate().map(|(index, dot)| {
                crate::smoke::tracked(format!("group-setup-dot-{index}"), div().p(px(3.)).child(dot))
            })
        });
        let back = match (step, settings) {
            (_, true) => Some(
                Button::ghost("group-setup-cancel", "Cancel")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_setup(cx))),
            ),
            (1, false) => Some(
                Button::ghost("group-setup-back", "Back")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.setup_step(0, cx))),
            ),
            _ => None,
        };
        let primary = if step == 0 && !settings {
            Button::primary("group-setup-next", "Continue")
                .disabled(!enough)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.setup_step(1, cx)))
        } else {
            let label = match (settings, saving) {
                (true, true) => "Saving…",
                (true, false) => "Save",
                (false, true) => "Turning on…",
                (false, false) => "Turn on",
            };
            Button::primary("group-setup-commit", label)
                .disabled(saving || !enough)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.commit_setup(window, cx)))
        };
        let footer = div()
            .flex_none()
            .w_full()
            .px(px(space::XXXL))
            .py(px(space::LG))
            .flex()
            .justify_center()
            .border_t_1()
            .border_color(colors.border.opacity(0.5))
            .child(
                div()
                    .w_full()
                    .max_w(px(SETUP_WIDTH))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(div().flex_1().flex().items_center().gap(px(2.)).children(dots.into_iter().flatten()))
                    .children(back.map(|back| {
                        crate::smoke::tracked(if settings { "group-setup-cancel" } else { "group-setup-back" }, back)
                    }))
                    .child(crate::smoke::tracked(
                        if step == 0 && !settings { "group-setup-next" } else { "group-setup-commit" },
                        primary,
                    )),
            );
        let column = div().w_full().max_w(px(SETUP_WIDTH)).flex().flex_col().gap(px(space::XXL)).children(pieces);
        div()
            .id("group-setup")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .child(crate::chrome::drag_region(div().id("group-setup-header")).h(px(HEADER_HEIGHT)).flex_none())
            .child(
                div()
                    .id("group-setup-scroll")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .on_scroll_wheel(cx.listener(Self::on_setup_scroll))
                    .child(
                        // At least the area's height, so a short step stays centred; a long one (many members) grows
                        // past it and the area scrolls from the top rather than squeezing it to spill over both edges.
                        // Group settings starts at the top, as the Edit agent page does.
                        div()
                            .w_full()
                            .flex_none()
                            .min_h_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .when(!settings, |this| this.justify_center())
                            .px(px(space::XXXL))
                            .pb(px(space::XXL))
                            .child(column),
                    ),
            )
            .child(footer)
            .into_any_element()
    }

    /// The setup's first step: the group's picture and name, and who's in it.
    fn setup_identity(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![self.setup_header(None, window, cx), self.setup_members_section(cx)]
    }

    /// Group settings, one page as the Edit agent page is: the picture and name with the routing model's line under
    /// the name, as an agent's model line is, and who's in it below.
    fn settings_page(&self, levels: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let line = self.route_line(levels, cx);
        let mut pieces = vec![self.setup_header(Some(line), window, cx), self.setup_members_section(cx)];
        // What the group shared, as an agent's profile shows its chat's.
        if let Some(shared) = self.setup.as_ref().and_then(|setup| setup.shared.clone()) {
            let width = setup_column_width(window, self.list_width());
            shared.update(cx, |shared, cx| shared.set_width(width, cx));
            let colors = *cx.colors();
            pieces.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::SM))
                    .child(
                        div().px(px(space::SM)).footnote().semibold().text_color(colors.text_secondary).child("Shared"),
                    )
                    .child(shared)
                    .into_any_element(),
            );
        }
        pieces
    }

    /// Near the end of Group settings, the shared list loads older items.
    fn on_setup_scroll(&mut self, _: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &self.setup else { return };
        let Some(shared) = setup.shared.clone() else { return };
        let scrolled = -setup.scroll.offset().y;
        if setup.scroll.max_offset().height - scrolled < px(SHARED_NEAR_END) {
            shared.update(cx, |shared, cx| shared.load_older(cx));
        }
    }

    /// The group's picture (its ✎ and colour popover) and name, as the Edit agent page has them; `under` goes under the
    /// name, where that page has the agent's model line.
    fn setup_header(&self, under: Option<AnyElement>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(setup) = &self.setup else { return div().into_any_element() };
        let picture = setup.look.render(
            picture_source(&setup.look.picture),
            |this: &mut Self| this.setup.as_mut().map(|setup| &mut setup.look),
            Self::open_setup_picker,
            cx,
        );
        div()
            .flex()
            .items_center()
            .gap(px(space::LG + 2.))
            .child(picture)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(name_field(&setup.name, window, cx))
                    .children(under),
            )
            .into_any_element()
    }

    /// Who's in the group: All agents, or each ticked. With one agent, New agent makes the second.
    fn setup_members_section(&self, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::switch::Switch;
        let colors = *cx.colors();
        let Some(setup) = &self.setup else { return div().into_any_element() };
        let rows = self.rows();
        let few = rows.len() < 2;
        let members = self.setup_members();
        let count =
            if setup.all || few { members.len().to_string() } else { format!("{} of {}", members.len(), rows.len()) };
        let faces = |ids: &[String]| {
            div().flex().children(rows.iter().filter(|row| ids.contains(&row.id)).take(7).enumerate().map(
                |(index, row)| {
                    div()
                        .when(index > 0, |this| this.ml(px(-6.)))
                        .rounded(px(radius::FULL))
                        .border_2()
                        .border_color(colors.surface)
                        .child(row.tile(20., cx))
                },
            ))
        };
        let mut card: Vec<AnyElement> = Vec::new();
        if !few {
            let all = setup.all;
            card.push(
                ui::ListRow::new("group-setup-all", "All agents")
                    .trailing(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::MD))
                            .when(all, |this| this.child(faces(&members)))
                            .child(crate::smoke::tracked(
                                "group-setup-all",
                                Switch::new("group-setup-all-switch")
                                    .checked(all)
                                    .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_all(cx))),
                            )),
                    )
                    .into_any_element(),
            );
        }
        if few || !setup.all {
            for row in rows {
                let on = members.contains(&row.id);
                let id = row.id.clone();
                let tick = div()
                    .size(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(on, |this| this.child(ui::icon_sized("check", 16., colors.accent)));
                let item =
                    ui::ListRow::new(SharedString::from(format!("group-setup-member-{}", row.id)), row.name.clone())
                        .leading(row.tile(28., cx))
                        .trailing(tick);
                let item = if few {
                    item
                } else {
                    item.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_member(id.clone(), cx)))
                };
                card.push(crate::smoke::tracked(format!("group-setup-member-{}", row.id), item).into_any_element());
            }
        }
        if few {
            let dash = div()
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::FULL))
                .border_1()
                .border_dashed()
                .border_color(colors.border_strong)
                .child(ui::icon_sized("plus", 14., colors.text_tertiary));
            let item = ui::ListRow::new("group-setup-new-agent", "New agent")
                .leading(dash)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_agent_for_setup(window, cx)));
            card.push(crate::smoke::tracked("group-setup-new-agent", item).into_any_element());
        }
        let label = div()
            .px(px(space::SM))
            .flex()
            .items_center()
            .justify_between()
            .footnote()
            .semibold()
            .text_color(colors.text_secondary)
            .child("Members")
            .child(div().font_weight(gpui::FontWeight::NORMAL).text_color(colors.text_tertiary).child(count));
        div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(label)
            .child(ui::section(None, None, card, cx))
            .into_any_element()
    }

    /// The setup's second step: the model that picks who answers.
    fn setup_routing(&self, levels: Vec<String>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let title = div().title3().bold().child("Model that picks who answers");
        vec![
            div()
                .flex()
                .flex_col()
                .gap(px(space::MD))
                .child(title)
                .child(self.route_line(levels, cx))
                .into_any_element(),
        ]
    }

    /// The model that picks who answers, picked as the Edit agent page picks an agent's, and right after it how hard it
    /// thinks, picked as the composer picks a chat's, from `levels`, the ones it takes. "No coding tool is signed in."
    /// while none is; a skeleton until that's known.
    fn route_line(&self, levels: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let Some(routing) = &self.routing else { return ui::skeleton_rows(1, None, cx).into_any_element() };
        if !routing.signed_in {
            let open = Button::secondary("group-setup-coding", "Open Coding")
                .small()
                .on_click(|_, window, cx| crate::nav::open_tab(crate::nav::Tab::Coding, window, cx));
            return div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(space::MD))
                .callout()
                .text_color(colors.text_secondary)
                .child("No coding tool is signed in.")
                .child(open)
                .into_any_element();
        }
        let Some(route) = self.setup_route() else { return div().into_any_element() };
        let reasoning = levels.iter().any(|level| conversation::reasoning_level(level).is_some()).then(|| {
            let level = route.reasoning.as_deref().unwrap_or(filo_local::group_route::DEFAULT_REASONING);
            let current = conversation::reasoning_used(level, &levels);
            let entity = cx.entity().downgrade();
            let trigger = MenuButton::new("route-reasoning")
                .outline()
                .small()
                .rounded(px(radius::FULL))
                .tooltip("How hard it thinks");
            let picker = conversation::reasoning_picker(
                trigger,
                gpui::Corner::TopLeft,
                levels,
                current,
                colors.text_secondary,
                move |level, cx| {
                    entity.update(cx, |this, cx| this.pick_route_reasoning(level, cx)).ok();
                },
            );
            crate::smoke::tracked("route-reasoning", picker)
        });
        model_line(&(route.engine, Some(route.model)), None, reasoning, Self::pick_route, cx).into_any_element()
    }

    fn render_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        if self.selected.as_deref() == Some(GROUP) {
            // On: its chat (Group settings over it while open). Off: its setup, opened with what it had before.
            if self.setup.is_some() {
                return self.render_setup(window, cx);
            }
            if self.group_row().is_some() {
                return self.render_group(window, cx);
            }
            if self.turning_on || self.group.is_none() || self.group_settings.is_none() {
                return div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::spinner(20., cx))
                    .into_any_element();
            }
            self.open_setup(false, window, cx);
            return self.render_setup(window, cx);
        }
        let Some(row) = self.selected.as_deref().and_then(|id| self.row(id)).cloned() else {
            return div().flex_1().into_any_element();
        };
        let view = self.conversation(window, cx);
        if let (Some(view), Some(id)) = (&view, self.selected.clone()) {
            self.reveal_in(&id, view, cx);
        }
        if let Some(view) = &view
            && self.pending_share.as_ref().is_some_and(|(agent, _)| *agent == row.id)
            && let Some((_, note)) = self.pending_share.take()
        {
            view.update(cx, |view, cx| view.share_note(note, window, cx));
        }
        let id = row.id.clone();
        // What's working, as in Filo's chat (screens/chat/working_panel.rs): the indicator in the middle of the header,
        // or a panel on the right when the user chose it.
        let room = ui::content_width() - self.list_width();
        let here = view.as_ref().map(|view| working_panel::here_for(view.read(cx))).unwrap_or_default();
        // The agent's own reply under way isn't this chat's task: its row in the list says it's working
        // (chat/reply_status.rs).
        // Only this agent's chat's tasks: the other agents' work isn't this chat's (the pet's tray shows all of it).
        let layout = working_panel::layout(room, !here.is_empty(), cx);
        self.chat_newest = working_panel::newest_time(here.iter().map(|row| row.since));
        let opener = working_panel::opener(view.clone());
        let details_open = layout.indicator && cx.default_global::<working_panel::WorkPlacement>().details_open;
        // The name and the indicator share the bar without overlapping (`fit_header`): the indicator in the middle while
        // that's clear of the name, else just after it, the name cut short only when both won't fit. The browser's
        // badge before the ⋯ takes what the name's least leaves it (`side_badge_room`), its words shortened to fit.
        let main_label = row.main.then(|| {
            working_panel::text_width("Main agent", TypeVariant::Caption.size(), gpui::FontWeight::NORMAL, cx)
                + space::MD
        });
        let lead = space::SM + space::XS + 32. + space::MD + main_label.unwrap_or(0.) + space::MD;
        let name_width =
            working_panel::text_width(&row.name, TypeVariant::Headline.size(), gpui::FontWeight::SEMIBOLD, cx);
        let header_width = room - layout.docked.unwrap_or(0.);
        let browser = browser_badge(
            &row.name,
            working_panel::side_badge_room(header_width, lead, name_width, AGENT_MENU_WIDTH + space::SM),
            cx,
        );
        let trailing = browser.as_ref().map_or(0., |(_, width)| *width) + AGENT_MENU_WIDTH + space::SM;
        let pill = if layout.indicator {
            working_panel::indicator_width(here.len(), &working_panel::status_text(&here).unwrap_or_default(), cx)
        } else {
            0.
        };
        let fit = working_panel::fit_header(header_width, lead, name_width, trailing, pill);
        let indicator = layout.indicator.then(|| {
            let text = working_panel::status_text(&here).unwrap_or_default();
            let needs_you = here.iter().any(|row| row.state == "waiting");
            let toggle: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, cx| {
                working_panel::activate(
                    room,
                    |cx| {
                        let state = cx.default_global::<working_panel::WorkPlacement>();
                        state.details_open = !state.details_open;
                    },
                    cx,
                )
            });
            working_panel::indicator(
                "agent-work-badge",
                working_panel::faces(&here, cx),
                text,
                fit.pill,
                needs_you,
                details_open,
                toggle,
                window,
                cx,
            )
        });
        let details = details_open.then(|| working_panel::details(&here, opener.clone(), cx));
        let right = layout.docked.map(|width| working_panel::render(&here, width, opener.clone(), cx));
        working_panel::escape_layers(&layout, details_open, window, cx);
        // The picture and name open the agent's settings; the rest of the bar drags the window.
        let header = crate::chrome::drag_region(div().id("agent-header"))
            .relative()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(space::SM))
            .pr(px(space::SM))
            .flex()
            .items_center()
            .child(
                div()
                    .id("edit-agent")
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .py(px(space::XS))
                    .pl(px(space::XS))
                    .pr(px(space::MD))
                    .rounded(px(radius::FULL))
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.surface))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(row.tile(32., cx))
                    .child(working_panel::header_name(&row.name, fit.name, cx))
                    .when(row.main, |this| {
                        this.child(div().flex_none().caption().text_color(colors.text_secondary).child("Main agent"))
                    })
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| this.edit_agent(id.clone(), window, cx)),
                    ),
            )
            .child(div().flex_1())
            .children(browser.map(|(badge, _)| badge))
            .child({
                use gpui_component::button::ButtonVariants as _;
                let (weak, menu_row) = (cx.entity().downgrade(), row.clone());
                div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                    MenuButton::new("agent-menu")
                        .ghost()
                        .small()
                        .icon(
                            gpui_component::Icon::empty().path("icons/ellipsis.svg").text_color(colors.text_secondary),
                        )
                        .dropdown_menu_with_anchor(gpui::Corner::TopRight, move |menu, _, _| {
                            agent_menu(menu, weak.clone(), &menu_row)
                        }),
                )
            })
            .children(indicator.map(|indicator| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(fit.left))
                    .w(px(fit.pill))
                    .flex()
                    .items_center()
                    .child(indicator)
            }));
        // A conversation with nothing in it yet shows who it's with; the first run's agent, also what it runs on and
        // three things to try.
        let tries = self.first_chat.as_ref().filter(|(agent, _)| *agent == row.id).map(|(_, tries)| tries.clone());
        let intro = row.last.is_none().then(|| match tries {
            Some(tries) => {
                let engine = row.locked_engine.or(row.engine).unwrap_or_else(crate::local_backend::default_engine);
                let tries = tries.into_iter().enumerate().map(|(index, text)| {
                    let row = div()
                        .id(SharedString::from(format!("first-try-{index}")))
                        .w_full()
                        .px(px(space::LG - 2.))
                        .py(px(space::MD - 2.))
                        .flex()
                        .items_center()
                        .gap(px(space::MD - 2.))
                        .rounded(px(radius::MD))
                        .bg(colors.surface)
                        .border_1()
                        .border_color(colors.border)
                        .cursor_pointer()
                        .hover(|style| style.bg(colors.surface_strong))
                        .child(div().flex_1().min_w_0().callout().child(text))
                        .child(ui::icon_sized("arrow-up", 14., colors.text_tertiary))
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| this.try_saying(text, window, cx)),
                        );
                    crate::smoke::tracked_fill(format!("first-try-{index}"), row)
                });
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom(px(120.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(space::XXL))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(460.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(space::MD + 2.))
                            .text_center()
                            .child(row.tile(80., cx))
                            .child(div().title2().child(format!("{} is ready", row.name)))
                            .child(div().callout().text_color(colors.text_secondary).child(format!(
                                "It runs on {} on this Mac and answers here. Try one of these, or say anything.",
                                engine.label()
                            )))
                            .child(
                                div()
                                    .w_full()
                                    .mt(px(space::XS))
                                    .flex()
                                    .flex_col()
                                    .gap(px(space::SM))
                                    .text_left()
                                    .children(tries),
                            )
                            .child(
                                div()
                                    .footnote()
                                    .text_color(colors.text_tertiary)
                                    .child("Its picture, name and instructions are on its profile."),
                            ),
                    )
            }
            None => div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom(px(120.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::MD))
                .child(row.tile(112., cx))
                .child(div().title2().child(row.name.clone())),
        });
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(colors.chat_background)
                    .child(header)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(match view {
                                Some(view) => view.into_any_element(),
                                None => div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(ui::spinner(20., cx))
                                    .into_any_element(),
                            })
                            .children(intro),
                    )
                    .children(details),
            )
            .children(right)
            .into_any_element()
    }

    fn render_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let Some(form) = &self.form else { return div().into_any_element() };
        let shared = form.shared.clone();
        // The list is hidden while there are no agents yet, and the form has its room too.
        let width = form_column_width(window, if self.rows().is_empty() { 0. } else { self.list_width() });
        if let Some(shared) = &shared {
            let name = form.id.as_deref().and_then(|id| self.row(id)).map(|row| row.name.clone()).unwrap_or_default();
            shared.update(cx, |shared, cx| {
                shared.set_agent_name(name, cx);
                shared.set_width(width, cx);
            });
        }
        let editing = form.id.clone();
        let blank =
            form.name.read(cx).value().trim().is_empty() || form.instructions.read(cx).value().trim().is_empty();
        let no_instructions = form.instructions.read(cx).value().is_empty();
        let saving = form.saving;
        // The agent's own routines, read-only: title, schedule and whether each runs.
        let routines = match &form.routines {
            Some(Ok(routines)) if !routines.is_empty() => {
                let rows = routines
                    .iter()
                    .map(|routine| {
                        ui::ListRow::new(
                            SharedString::from(format!("agent-routine-{}", routine.id)),
                            routine.title.clone(),
                        )
                        .icon("repeat")
                        .subtitle(format_routine_schedule(routine))
                        .trailing(div().footnote().text_color(colors.text_secondary).child(if routine.active() {
                            "Active"
                        } else {
                            "Paused"
                        }))
                        .into_any_element()
                    })
                    .collect();
                Some(ui::section(Some("Routines"), None, rows, cx))
            }
            Some(Err(error)) => {
                let row = ui::ListRow::new("agent-routines-error", "Couldn’t load its routines")
                    .subtitle(error.clone())
                    .into_any_element();
                Some(ui::section(Some("Routines"), None, vec![row], cx))
            }
            _ => None,
        };
        let dirty = form.id.is_some() && form.saved.differs(&form.current(cx));
        let just_saved = form.just_saved;
        let label = |text: &str| div().footnote().semibold().text_color(colors.text_secondary).child(text.to_string());
        // The picture in its colour, at the head of the profile; a click opens the colour popover under it.
        let picture = form.look.render(
            picture_source(&form.look.picture),
            |this: &mut Self| this.form.as_mut().map(|form| &mut form.look),
            Self::open_picker,
            cx,
        );
        let name = name_field(&form.name, window, cx);
        let templates = no_instructions.then(|| {
            div().flex().flex_wrap().gap(px(space::SM)).children(TEMPLATES.iter().enumerate().map(
                |(index, (name, _))| {
                    div()
                        .id(SharedString::from(format!("template-{name}")))
                        .h(px(30.))
                        .px(px(space::MD))
                        .flex()
                        .items_center()
                        .rounded(px(radius::FULL))
                        .border_1()
                        .border_color(colors.border)
                        .footnote()
                        .text_color(colors.text_secondary)
                        .cursor_pointer()
                        .hover(|style| style.bg(colors.surface))
                        .child(name.to_string())
                        .on_click(
                            cx.listener(move |this, _: &ClickEvent, window, cx| this.use_template(index, window, cx)),
                        )
                },
            ))
        });
        // An agent whose conversation started keeps its engine: only that engine's models.
        let locked = form.locked;
        let model_line =
            model_line(&form.model, locked, None, |this: &mut Self, choice, cx| this.pick_model(choice, cx), cx);
        // An edited agent's ⋯: the way to another engine for one tied to an engine (a copy that starts there and can
        // look back at this chat), one item per engine, and Delete.
        let more = editing.clone().map(|id| {
            use gpui_component::button::ButtonVariants as _;
            use gpui_component::menu::PopupMenuItem;
            let weak = cx.entity().downgrade();
            let others: Vec<filo_local::Engine> = locked
                .map(|locked| filo_local::Engine::ALL.into_iter().filter(|engine| *engine != locked).collect())
                .unwrap_or_default();
            let button = MenuButton::new("agent-form-menu")
                .ghost()
                .small()
                .icon(gpui_component::Icon::empty().path("icons/ellipsis.svg").text_color(colors.text_secondary))
                .dropdown_menu_with_anchor(gpui::Corner::TopRight, move |menu, _, _| {
                    let delete = weak.clone();
                    let delete_id = id.clone();
                    let mut menu = menu;
                    for engine in others.iter().copied() {
                        let (duplicate, duplicate_id) = (weak.clone(), id.clone());
                        menu = menu.item(
                            PopupMenuItem::new(format!("Duplicate with {}", engine.label()))
                                .icon(gpui_component::Icon::empty().path("icons/copy.svg"))
                                .on_click(move |_, window, cx| {
                                    duplicate
                                        .update(cx, |this, cx| {
                                            this.duplicate_with(duplicate_id.clone(), engine, window, cx)
                                        })
                                        .ok();
                                }),
                        );
                    }
                    let menu = if others.is_empty() { menu } else { menu.separator() };
                    menu.item(
                        PopupMenuItem::element(|_, cx| div().text_color(cx.colors().danger).child("Delete agent…"))
                            .on_click(move |_, window, cx| {
                                delete.update(cx, |this, cx| this.confirm_delete(delete_id.clone(), window, cx)).ok();
                            }),
                    )
                });
            div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(button)
        });
        // An edited agent's chevron, back to the chat (as ⌘← and ⌘[ are).
        let back = editing.is_some().then(|| {
            div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(crate::smoke::tracked(
                "agent-form-back",
                IconButton::new("agent-form-back", "chevron-left")
                    .small()
                    .tooltip("Back")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back_from_form(window, cx))),
            ))
        });
        let header = div()
            .flex()
            .items_center()
            .gap(px(space::LG + 2.))
            .child(picture)
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(6.)).child(name).child(model_line));
        // The save bar floats up from the bottom while there's something to save (a new agent: always, to create it).
        let creating = editing.is_none();
        // ⌘S saves while the bar offers it, wherever the keyboard is: in a field, after a menu closed, in the list.
        if creating || dirty {
            crate::escape::save(window, cx, |this, window, cx| this.save(window, cx));
        }
        let bar = (creating || dirty || just_saved).then(|| {
            let keycap = |key: &str, on: gpui::Hsla| {
                div()
                    .px(px(4.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(on.opacity(0.3))
                    .caption()
                    .text_color(on.opacity(0.75))
                    .child(key.to_string())
            };
            let bar_button = |id: &'static str, text: &str, key: &str, primary: bool, enabled: bool| {
                let fg = if primary { colors.on_action } else { colors.text };
                div()
                    .id(id)
                    .h(px(28.))
                    .px(px(space::MD))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(radius::FULL))
                    .footnote()
                    .semibold()
                    .text_color(fg)
                    .when(primary, |this| this.bg(colors.action))
                    .when(!enabled, |this| this.opacity(0.42))
                    .when(enabled, |this| {
                        this.cursor_pointer().hover(move |style| {
                            if primary { style.opacity(0.9) } else { style.bg(colors.selected_fill) }
                        })
                    })
                    .child(text.to_string())
                    .child(keycap(key, fg))
            };
            let content = if !creating && !dirty {
                div()
                    .h(px(28.))
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child(ui::icon_sized("check", 14., colors.success))
                    .child("Saved")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::XS))
                    .child(
                        div()
                            .pl(px(space::SM + 2.))
                            .pr(px(space::SM))
                            .footnote()
                            .text_color(colors.text_secondary)
                            .child(if creating { "New agent" } else { "Unsaved changes" }),
                    )
                    .child(
                        bar_button("revert-agent", if creating { "Cancel" } else { "Revert" }, "esc", false, !saving)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                if creating {
                                    this.close_form(cx);
                                } else {
                                    this.revert(window, cx);
                                }
                            })),
                    )
                    .child(crate::smoke::tracked(
                        "save-agent",
                        bar_button(
                            "save-agent",
                            if saving {
                                "Saving…"
                            } else if creating {
                                "Create"
                            } else {
                                "Save"
                            },
                            "⌘S",
                            true,
                            !blank && !saving,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx))),
                    ))
                    .into_any_element()
            };
            div().absolute().left_0().right_0().bottom(px(space::XL)).flex().justify_center().child(
                div()
                    .id("agent-save-bar")
                    .occlude()
                    .p(px(6.))
                    .rounded(px(radius::FULL))
                    .bg(colors.raised)
                    .border_1()
                    .border_color(colors.border)
                    .shadow_lg()
                    .child(content),
            )
        });
        crate::smoke::keep_page_scroll("agent-profile", &form.scroll);
        // The bar on top stays put while the page scrolls under it; the rest of it drags the window.
        let top = crate::chrome::drag_region(div().id("agent-form-header"))
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .px(px(space::SM))
            .flex()
            .items_center()
            .justify_between()
            .child(div().children(back))
            .children(more);
        let page = div()
            .id("agent-form")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&form.scroll)
            .on_scroll_wheel(cx.listener(Self::on_form_scroll))
            .bg(colors.background)
            .child(crate::smoke::tracked(
                "agent-profile-end",
                // A set width, not just a cap: the shared panel is as wide as the column, which made the column's
                // min-content width its real one, and Taffy then reused that measurement's layout, leaving gaps.
                div()
                    .w(px(width + 2. * space::XXXL))
                    .mx_auto()
                    .px(px(space::XXXL))
                    // Room under the end of the page for the save bar.
                    .pb(px(space::HUGE * 2.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .child(header)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(space::SM))
                            .child(label("Instructions"))
                            .child(Input::new(&form.instructions))
                            .children(templates)
                            .child(
                                div()
                                    .footnote()
                                    .text_color(colors.text_secondary)
                                    .child("What it does and how it talks to you. Every chat with it follows these."),
                            ),
                    )
                    .when_some(routines, |this, section| this.child(section))
                    .when_some(shared, |this, shared| {
                        this.child(
                            div()
                                .pt(px(space::XL))
                                .border_t_1()
                                .border_color(colors.border.opacity(0.6))
                                .flex()
                                .flex_col()
                                .gap(px(space::SM))
                                .child(label("Shared"))
                                .child(shared),
                        )
                    })
                    .children(crate::smoke::page_end("agent-profile")),
            ));
        div()
            .id("agent-form-page")
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .track_focus(&form.focus)
            // ⌘← and ⌘[ go back from an edited agent's profile, its fields too (`bind_keys`).
            .when(editing.is_some(), |this| {
                this.key_context("AgentProfile")
                    .on_action(
                        cx.listener(|this, _: &crate::chrome::GoBack, window, cx| this.back_from_form(window, cx)),
                    )
                    .on_action(cx.listener(Self::back_at_line_start))
            })
            .child(top)
            .child(page)
            .children(bar)
            .into_any_element()
    }

    fn render_empty(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let pictures = ["owl-laptop", "bear-waving", "fox-laptop", "rabbit-heart"];
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(space::XL))
            .bg(colors.background)
            .child(div().flex().children(pictures.iter().enumerate().map(|(index, id)| {
                div()
                    .when(index > 0, |this| this.ml(px(-14.)))
                    .rounded(px(radius::FULL))
                    .border_3()
                    .border_color(colors.background)
                    .child(round_picture(avatars::animal_image(id), 76., cx))
            })))
            .child(div().title2().child("Make your own agents"))
            .child(
                div().max_w(px(420.)).text_center().callout().text_color(colors.text_secondary).child(
                    creation_blocked(self.can_create.as_ref())
                        .unwrap_or_else(|| "Each with its own instructions and its own conversation.".into()),
                ),
            )
            .child(crate::smoke::tracked(
                "agents-new-first",
                Button::primary("first-agent", "New agent")
                    .icon("plus")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_agent(window, cx))),
            ))
            .child(
                div()
                    .footnote()
                    .text_color(colors.text_tertiary)
                    .child("The welcome setup is in Settings › Welcome tour."),
            )
            .into_any_element()
    }
}

impl Render for AgentsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        self.watch_tray(cx);
        // A drag of the list's edge has ended: keep the width for the next launch.
        if self.list_width_unsaved && !cx.has_active_drag() {
            self.list_width_unsaved = false;
            save_list_width(self.list_width);
        }
        let body = match &self.agents {
            None => div()
                .size_full()
                .flex()
                .child(div().w(px(self.list_width())).p(px(space::MD)).child(ui::skeleton_rows(
                    4,
                    Some(ROW_PICTURE),
                    cx,
                )))
                .into_any_element(),
            Some(Err(error)) if self.form.is_none() => {
                ui::empty_state("triangle-alert", "Couldn’t load your agents", Some(error.clone().into()), None, cx)
                    .into_any_element()
            }
            Some(Ok(rows)) if rows.is_empty() && self.form.is_none() => self.render_empty(cx),
            _ => {
                // Esc closes the colour popover, then reverts what changed, then leaves the form.
                if let Some(form) = &self.form {
                    if form.look.colors_open() {
                        crate::escape::popup(window, cx, |this, _, cx| this.set_colors_open(false, cx));
                    }
                    crate::escape::layer(window, cx, |this, window, cx| this.escape_form(window, cx));
                }
                let right =
                    if self.form.is_some() { self.render_form(window, cx) } else { self.render_chat(window, cx) };
                let has_rows = !self.rows().is_empty();
                div()
                    .size_full()
                    .flex()
                    .on_drag_move(cx.listener(Self::on_edge_drag))
                    .when(has_rows, |this| this.child(self.render_list(window, cx)))
                    .child(right)
                    .into_any_element()
            }
        };
        div().size_full().bg(colors.background).child(body)
    }
}

#[cfg(test)]
mod list_width_tests {
    use super::*;

    #[test]
    fn the_saved_width_is_read_within_the_limits() {
        // No file, a broken one, a missing or odd value: the usual width.
        for text in ["", "{}", "not json", r#"{"listWidth":"wide"}"#, r#"{"listWidth":null}"#] {
            assert_eq!(parse_list_width(text), LIST_WIDTH, "{text}");
        }
        assert_eq!(parse_list_width(r#"{"listWidth":333}"#), 333.);
        // A width from a file edited by hand, or an older limit, comes back within the limits.
        assert_eq!(parse_list_width(r#"{"listWidth":40}"#), LIST_MIN);
        assert_eq!(parse_list_width(r#"{"listWidth":9000}"#), LIST_MAX);
    }

    #[test]
    fn the_tool_marks_are_on_unless_turned_off() {
        for text in ["", "{}", "not json", r#"{"toolMarks":"no"}"#, r#"{"listWidth":300}"#] {
            assert!(parse_tool_marks(text), "{text}");
        }
        let both = r#"{"listWidth":333,"toolMarks":false}"#;
        assert!(!parse_tool_marks(both));
        assert_eq!(parse_list_width(both), 333.);
    }

    #[test]
    fn a_narrow_window_narrows_the_list_before_the_chat() {
        // Room for both: the chosen width.
        assert_eq!(fit_list_width(360., 1400.), 360.);
        // Dragged past the limits.
        assert_eq!(fit_list_width(100., 1400.), LIST_MIN);
        assert_eq!(fit_list_width(900., 1400.), LIST_MAX);
        // A narrow window: the chat keeps PANE_MIN, the list gives way...
        assert_eq!(fit_list_width(400., 760.), 760. - PANE_MIN);
        // ...but never below its minimum.
        assert_eq!(fit_list_width(400., 500.), LIST_MIN);
    }
}

/// "Scout is working in the browser" in the agent's header while it works a page in Filo's browser (or the user paused
/// it or took the page over there): a click opens the page in the browser window, to watch, pause or take it over. At
/// most `room` wide (`working_panel::side_badge_room`), margin included. With its width.
fn browser_badge(agent: &str, room: f32, cx: &App) -> Option<(gpui::AnyElement, f32)> {
    let (tab, control) = crate::browser::working_for(agent)?;
    let open: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| crate::browser::window::open(Some(tab.clone()), cx));
    Some(browser_pill(agent, control, room, open, cx))
}

/// `browser_badge`'s pill for `agent` and who has its page: its words whole, else without the agent's name ("Working in
/// the browser"), else that cut short with "…", so they never run past its edge.
pub(crate) fn browser_pill(
    agent: &str,
    control: crate::browser::Control,
    room: f32,
    on_click: Rc<dyn Fn(&mut App)>,
    cx: &App,
) -> (gpui::AnyElement, f32) {
    let colors = *cx.colors();
    let (labels, bg, fg) = match control {
        crate::browser::Control::Agent => (
            [format!("{agent} is working in the browser"), "Working in the browser".to_string()],
            colors.accent_soft,
            colors.on_accent_soft,
        ),
        crate::browser::Control::Paused => (
            [format!("{agent} is paused in the browser"), "Paused in the browser".to_string()],
            colors.warning_soft,
            colors.on_warning_soft,
        ),
        crate::browser::Control::User => (
            [format!("You have {agent}'s browser page"), "You have its browser page".to_string()],
            colors.info_soft,
            colors.on_info_soft,
        ),
    };
    // The globe, the gap after it and the padding; the margin after the pill.
    let chrome = 13. + space::XS + 2. * space::MD;
    let text_room = (room.min(working_panel::SIDE_BADGE_MAX) - space::XS - chrome).max(0.);
    let text = working_panel::fit_label(&labels, text_room, cx);
    let text_width =
        working_panel::text_width(&text, TypeVariant::Footnote.size(), gpui::FontWeight::SEMIBOLD, cx).min(text_room);
    let badge = div()
        .id("agent-browser-badge")
        .flex_none()
        .mr(px(space::XS))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .px(px(space::MD))
        .py(px(space::XXS))
        .rounded(px(radius::FULL))
        .bg(bg)
        .text_color(fg)
        .footnote()
        .semibold()
        .cursor_pointer()
        .child(ui::icon_sized("globe", 13., fg))
        .child(working_panel::one_line(div(), text, text_width))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| on_click(cx));
    (crate::smoke::tracked("agent-browser-badge", badge).into_any_element(), chrome + text_width + space::XS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(key: &str) -> Place {
        Place { key: key.into(), ..Place::default() }
    }

    #[test]
    fn pinned_then_needs_input_then_drafts_then_the_rest_each_latest_message_first() {
        let places = vec![
            Place { last: 50., ..place("old") },
            Place { last: 90., ..place("recent") },
            Place { pinned: Some(1), last: 1., ..place("main") },
            Place { draft: true, last: 10., ..place("drafted-early") },
            Place { last: 70., ..place("middle") },
            Place { needs_input: true, last: 20., ..place("asking-early") },
            Place { pinned: Some(0), needs_input: true, ..place(GROUP) },
            Place { draft: true, last: 30., ..place("drafted-late") },
            Place { needs_input: true, draft: true, last: 40., ..place("asking-late") },
            Place { ..place("no-messages") },
        ];
        assert_eq!(
            arrange(&places),
            [
                GROUP,
                "main",
                "asking-late",
                "asking-early",
                "drafted-late",
                "drafted-early",
                "recent",
                "middle",
                "old",
                "no-messages"
            ]
        );
        // Answered: back among the drafts, or the rest.
        let answered: Vec<Place> = places.iter().cloned().map(|place| Place { needs_input: false, ..place }).collect();
        assert_eq!(
            arrange(&answered),
            [
                GROUP,
                "main",
                "asking-late",
                "drafted-late",
                "drafted-early",
                "recent",
                "middle",
                "old",
                "asking-early",
                "no-messages"
            ]
        );
        // None asking or drafted: the latest message first.
        let places: Vec<Place> =
            places.into_iter().filter(|place| place.pinned.is_some() || !place.needs_input && !place.draft).collect();
        assert_eq!(arrange(&places), [GROUP, "main", "recent", "middle", "old", "no-messages"]);
        // A new message moves its row up.
        let replied: Vec<Place> = places
            .iter()
            .cloned()
            .map(|place| if place.key == "old" { Place { last: 95., ..place } } else { place })
            .collect();
        assert_eq!(arrange(&replied), [GROUP, "main", "old", "recent", "middle", "no-messages"]);
        // Ties keep the order given (the agents as made).
        assert_eq!(arrange(&[place("a"), place("b"), place("c")]), ["a", "b", "c"]);
    }

    #[test]
    fn the_list_holds_its_order_under_the_pointer() {
        let keys = |list: &[&str]| list.iter().map(|key| key.to_string()).collect::<Vec<_>>();
        let shown = keys(&[GROUP, "a", "b", "c"]);
        // "c" starts working: held, nothing moves; a deleted row goes, a new one shows where it belongs.
        assert_eq!(held(&shown, &keys(&[GROUP, "c", "a", "b"]), &[]), shown);
        assert_eq!(held(&shown, &keys(&[GROUP, "c", "a"]), &[]), keys(&[GROUP, "a", "c"]));
        assert_eq!(held(&shown, &keys(&[GROUP, "new", "c", "a", "b"]), &[]), keys(&[GROUP, "new", "a", "b", "c"]));
        assert_eq!(held(&[], &keys(&["b", "a"]), &[]), keys(&["b", "a"]));
    }

    #[test]
    fn a_row_whose_draft_came_or_went_moves_under_the_pointer() {
        let keys = |list: &[&str]| list.iter().map(|key| key.to_string()).collect::<Vec<_>>();
        let shown = keys(&[GROUP, "a", "b", "c"]);
        // "c" left with a draft goes up to the drafts at once; "b" got a reply meanwhile and stays held.
        assert_eq!(held(&shown, &keys(&[GROUP, "c", "b", "a"]), &keys(&["c"])), keys(&[GROUP, "c", "a", "b"]));
        // "a"'s draft sent or cleared: back down among the rest.
        assert_eq!(held(&shown, &keys(&[GROUP, "b", "c", "a"]), &keys(&["a"])), keys(&[GROUP, "b", "c", "a"]));
    }

    #[test]
    fn new_agents_wait_for_a_coding_tool() {
        let can_create = |value: Value| serde_json::from_value::<CanCreate>(value).unwrap();
        assert_eq!(creation_blocked(None), None);
        assert_eq!(creation_blocked(Some(&can_create(json!({ "allowed": true, "reason": null })))), None);
        let reason = "Plug one in on the Coding page.";
        assert_eq!(
            creation_blocked(Some(&can_create(json!({ "allowed": false, "reason": reason })))).as_deref(),
            Some(reason)
        );
        assert!(creation_blocked(Some(&can_create(json!({ "allowed": false })))).unwrap().contains("Coding page"));
    }

    #[test]
    fn backgrounds_are_saved_as_hex() {
        assert_eq!(parse_background("cfe0ff").as_deref(), Some("#CFE0FF"));
        assert_eq!(parse_background(" #3f7df0 ").as_deref(), Some("#3F7DF0"));
        assert_eq!(parse_background("#3f7df"), None);
        assert_eq!(parse_background("blue"), None);
        assert!(background_color(None).is_none());
        assert!(BACKGROUNDS.iter().all(|hex| parse_background(hex).as_deref() == Some(*hex)));
        // A colour picked from the palette saves as the same hex it was shown from.
        assert!(
            BACKGROUNDS
                .iter()
                .chain(["#A1B2C3", "#000000", "#FFFFFF"].iter())
                .all(|hex| background_hex(background_color(Some(hex)).unwrap()) == *hex)
        );
    }

    #[test]
    fn a_hue_colour_opens_at_its_own_hue_as_a_custom_one() {
        for hue in [0., 45., 200., 330.] {
            let hex = hue_background(hue);
            assert!(is_custom(Some(&hex)));
            assert!((hue_of(Some(&hex)) - hue).abs() <= 1., "{hue} came back as {}", hue_of(Some(&hex)));
        }
        assert_eq!(hue_background(360.), hue_background(0.));
        assert!(!is_custom(None) && !is_custom(Some(BACKGROUNDS[0])));
    }

    #[test]
    fn only_a_real_change_shows_the_save_bar() {
        let engine = crate::local_backend::default_engine();
        let saved = Saved {
            name: "Coach".into(),
            instructions: "Be kind".into(),
            picture: "panda-waving".into(),
            background: None,
            model: (engine, None),
            model_picked: false,
        };
        assert!(!saved.differs(&Saved { model_picked: true, ..saved.clone() }));
        assert!(saved.differs(&Saved { background: Some(BACKGROUNDS[0].into()), ..saved.clone() }));
        assert!(saved.differs(&Saved { name: "Coach!".into(), ..saved.clone() }));
        assert!(saved.differs(&Saved { model: (engine, Some("other".into())), ..saved.clone() }));
    }

    #[test]
    fn an_agent_keeps_the_model_its_engines_default_is_now() {
        use filo_local::{Engine, engine::ModelOption};
        let row = |engine, id: &str, default| ModelOption {
            engine,
            id: id.into(),
            label: id.into(),
            default,
            aliases: Vec::new(),
        };
        let rows = [
            row(Engine::Claude, "sonnet", false),
            row(Engine::Claude, "claude-opus-5-5", true),
            row(Engine::Codex, "gpt-a", true),
        ];
        assert_eq!(saved_model(&(Engine::Claude, None), &rows), (Engine::Claude, Some("claude-opus-5-5".into())));
        assert_eq!(saved_model(&(Engine::Codex, None), &rows), (Engine::Codex, Some("gpt-a".into())));
        assert_eq!(
            saved_model(&(Engine::Claude, Some("sonnet".into())), &rows),
            (Engine::Claude, Some("sonnet".into()))
        );
        // While the CLI hasn't said which model its default is: the default itself.
        assert_eq!(saved_model(&(Engine::Codex, None), &rows[..2]), (Engine::Codex, None));
    }

    #[test]
    fn an_agents_picture_is_its_own() {
        let embedded = |source: gpui::ImageSource| match source {
            gpui::ImageSource::Resource(gpui::Resource::Embedded(path)) => path.to_string(),
            _ => "other".into(),
        };
        assert_eq!(embedded(picture_source("fox-laptop")), "avatars/fox-laptop.webp");
        assert_eq!(embedded(picture_source("helper:sunny")), "workers/sunny.png");
    }
}

// What's working in this chat, for Filo's chat and each agent's chat: only the coding tasks the chat on screen started
// (the live `coding:sessionsForChat` sessions, asked for by this chat's id). Never the other chats' tasks (the pet's
// tray and the menu bar show all of them), and never this chat's own reply, which shows in its header
// (reply_status.rs). By default it's the indicator in the middle of the chat's header (the work badge); its details are
// a popover of compact rows, the docked panel's (the task with its tool's mark, its state and how long; what it's
// doing), a row opening its task. Only a right-click on the indicator (or the docked panel's title) moves it: to a
// panel on the right, or back at the top. The choice is kept in desktop-working.json (an older app's "left" reads as
// the top), and nothing docks unless the user chose the right (`layout_for`). Nothing here subscribes to anything new.
// A row opens the coding task's thread, or the question's card in this chat. A row dragged onto the composer points
// the next message at its task (work_share.rs).

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Global, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::coding_sessions::CodingSession;
use crate::screens::coding::{named_tool_tile, task_heading, tool_name, tool_tile};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// The panel's width, when there's room for it.
pub const WIDTH: f32 = 264.;
/// The narrowest the docked panel gets before the chat would have to give up readable room.
pub const MIN_WIDTH: f32 = 220.;
/// What the chat beside the docked panel always keeps: messages and the composer stay readable. With less room than
/// this plus `MIN_WIDTH`, the panel doesn't dock (and never covers the chat): the indicator stands in at the top.
pub const CHAT_MIN: f32 = 460.;
/// The details under the indicator.
pub const DETAILS_WIDTH: f32 = 420.;
/// A row in the popover: its card less its padding and border.
const POPOVER_ROW_WIDTH: f32 = DETAILS_WIDTH - 2. * space::XS - 2.;
/// A row's tool mark.
const FACE: f32 = 22.;

/// What a row opens. None of them starts, stops or answers anything.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A coding task's thread.
    CodingTask(String),
    /// A coding task's open question: its card in this chat (the thread when the card isn't loaded).
    Question(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelRow {
    pub key: String,
    /// The coding tool that works on it ("claude-code", "codex"…).
    pub tool: String,
    /// The tool's name.
    pub name: String,
    /// On what: the task.
    pub title: String,
    /// What it's doing now (its latest step or its question), when it says something.
    pub detail: Option<String>,
    /// When the coding task's current round started running (`CodingSession::since`, ms). Rows show how long it has run
    /// beside the state ("Working · 4m"); None when not known or not running, and then no time.
    pub since: Option<f64>,
    /// `agent_work::state_pill`'s look and its words.
    pub state: &'static str,
    pub label: &'static str,
    pub target: Target,
    /// The task's project, computer, branch and model, with their icons, for its title's hover details
    /// (`CodingSession::facts`).
    pub facts: Vec<(&'static str, String)>,
}

impl PanelRow {
    fn needs_you(&self) -> bool {
        self.state == "waiting"
    }
}

/// The work in this chat, the badge's lists: needing you first, then working, then waiting.
pub fn here_rows(coding: &[CodingSession]) -> Vec<PanelRow> {
    let now = crate::format::now_ms();
    let mut rows: Vec<PanelRow> = Vec::new();
    for session in coding {
        let (state, label) = session.pill();
        let target = if session.needs_you() {
            Target::Question(session.task_id.clone())
        } else {
            Target::CodingTask(session.task_id.clone())
        };
        rows.push(PanelRow {
            key: format!("coding-{}", session.task_id),
            tool: session.tool.clone(),
            name: tool_name(&session.tool),
            title: session.title.clone(),
            detail: session.detail(now),
            state,
            label,
            since: session.since(),
            target,
            facts: session.facts(),
        });
    }
    let rank = |row: &PanelRow| match row.state {
        "waiting" => 0,
        "active" | "monitoring" => 1,
        _ => 2,
    };
    rows.sort_by_key(rank);
    rows
}

/// `here_rows` for a conversation on screen (an agent's chat).
pub fn here_for(conversation: &super::conversation::Conversation) -> Vec<PanelRow> {
    here_rows(&super::coding_sessions::live(&conversation.coding_sessions))
}

/// How long something has run, short enough for a row's top line: "<1m", "4m", "1h 5m" (`format::hours_minutes`),
/// "2d".
pub fn short_age(since: f64, now: f64) -> String {
    let minutes = ((now - since) / 60_000.).floor().max(0.) as i64;
    match minutes {
        0 => "<1m".into(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => crate::format::hours_minutes(minutes),
        _ => format!("{}d", minutes / 1440),
    }
}

/// Redraws `T` while it shows how long something has run ("Working · 4m", a list row's "Working 12s"), so the times
/// move on with nothing else changing: every second while the newest is under a minute old, as the chat list counts,
/// else every half minute. `newest` is when the newest time on screen began (`newest_time`), None with none showing.
/// No query: the times come from rows the screen already has.
pub fn ticker<T: 'static>(cx: &mut gpui::Context<T>, newest: fn(&T) -> Option<f64>) -> gpui::Task<()> {
    cx.spawn(async move |this, cx| {
        let mut wait = tick_after(None, 0.);
        loop {
            cx.background_executor().timer(wait).await;
            let next = this.update(cx, |this, cx| {
                let newest = newest(this);
                if newest.is_some() {
                    cx.notify();
                }
                tick_after(newest, crate::format::now_ms())
            });
            match next {
                Ok(next) => wait = next,
                Err(_) => break,
            }
        }
    })
}

/// How long the ticker waits: a second while the newest time shown is under a minute old (its seconds count), else
/// half a minute.
pub fn tick_after(newest: Option<f64>, now: f64) -> std::time::Duration {
    std::time::Duration::from_secs(if newest.is_some_and(|at| now - at < 60_000.) { 1 } else { 30 })
}

/// When the newest of the times shown began, for `ticker`: 0 (long ago) when none of their starts is known, None when
/// nothing shows a time.
pub fn newest_time(times: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    times.into_iter().fold(None, |newest, at| Some(newest.unwrap_or(0_f64).max(at.unwrap_or(0.))))
}

/// A state with how long it has run: "Working · 4m", "Needs you · 1h 5m"; the state alone when the start isn't known.
pub fn status_time(label: &str, since: Option<f64>, now: f64) -> String {
    match since {
        Some(since) => format!("{label} · {}", short_age(since, now)),
        None => label.to_string(),
    }
}

// ---- Where it shows ----

/// Where the work shows: the indicator at the top of the chat (the default), or a panel docked on the right. Only the
/// user's choice docks it, from the right-click menu on the indicator or the panel's title.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    #[default]
    Top,
    Right,
}

impl Placement {
    pub const ALL: [Placement; 2] = [Placement::Top, Placement::Right];

    /// The right-click menu's words.
    pub fn label(self) -> &'static str {
        match self {
            Placement::Top => "Top center (default)",
            Placement::Right => "Right sidebar",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Placement::Top => "top",
            Placement::Right => "right",
        }
    }
}

/// `desktop-working.json`, beside the app's other settings files.
fn settings_path() -> std::path::PathBuf {
    crate::config::config_dir().join("desktop-working.json")
}

fn saved_key(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value.get("placement")?.as_str().map(str::to_string)
}

/// What the settings file says: anything missing, unreadable or unknown is the default, the top of the chat (so an
/// older app's files and a first launch never dock the panel). The left sidebar is gone: a saved "left" is the top.
pub fn parse_placement(text: &str) -> Placement {
    match saved_key(text).as_deref() {
        Some("right") => Placement::Right,
        _ => Placement::Top,
    }
}

/// An older app's "left", which `load_placement` rewrites as the top.
pub fn is_legacy_left(text: &str) -> bool {
    saved_key(text).as_deref() == Some("left")
}

/// The saved placement; a saved "left" is moved to the top in the file too, so it reads the same everywhere after.
fn load_placement() -> Placement {
    let Ok(text) = std::fs::read_to_string(settings_path()) else { return Placement::Top };
    if is_legacy_left(&text) {
        save_placement(Placement::Top);
        eprintln!("working: the left sidebar is gone; the work shows at the top");
    }
    parse_placement(&text)
}

/// Saves the placement, the file's other settings kept; one that isn't a JSON object is never saved over (config.rs).
fn save_placement(placement: Placement) {
    let values = serde_json::Map::from_iter([("placement".to_string(), serde_json::json!(placement.key()))]);
    if let Err(error) = crate::config::set_settings("desktop-working.json", values) {
        eprintln!("working: {error}");
    }
}

/// The placement, read once from its file, and the docked panel's open or hidden state (until the user opens or hides
/// it: open when there's room). Keyboard focus for the indicators, by id.
pub struct WorkPlacement {
    pub placement: Placement,
    dock_open: Option<bool>,
    /// The details under the indicator.
    pub details_open: bool,
    focus: std::collections::HashMap<String, gpui::FocusHandle>,
    /// The row whose title the pointer is on, by key: its details show beside it (`title_tip`).
    tip: Option<String>,
}

impl Default for WorkPlacement {
    fn default() -> Self {
        WorkPlacement {
            placement: load_placement(),
            dock_open: None,
            details_open: false,
            focus: Default::default(),
            tip: None,
        }
    }
}

/// The pointer came onto a row's title, or left it.
fn set_tip(key: &str, on: bool, cx: &mut App) {
    let state = cx.default_global::<WorkPlacement>();
    if on {
        state.tip = Some(key.to_string());
    } else if state.tip.as_deref() == Some(key) {
        state.tip = None;
    }
}

fn tip_shown(key: &str, cx: &App) -> bool {
    cx.try_global::<WorkPlacement>().and_then(|state| state.tip.as_deref()) == Some(key)
}

/// The row whose details show beside it, if any (the fixture logs it).
#[cfg(feature = "dev")]
pub fn tip(cx: &App) -> Option<String> {
    cx.try_global::<WorkPlacement>().and_then(|state| state.tip.clone())
}

impl Global for WorkPlacement {}

pub fn placement(cx: &mut App) -> Placement {
    cx.default_global::<WorkPlacement>().placement
}

/// The user's choice: kept for every chat and the next launch. The right shows its panel straight away.
pub fn set_placement(placement: Placement, cx: &mut App) {
    let state = cx.default_global::<WorkPlacement>();
    state.placement = placement;
    state.dock_open = None;
    state.details_open = false;
    save_placement(placement);
}

/// Hides the docked panel (its ×) or opens it again (the indicator, while it's hidden).
pub fn set_dock_open(open: bool, cx: &mut App) {
    cx.default_global::<WorkPlacement>().dock_open = Some(open);
}

fn focus_for(id: &str, cx: &mut App) -> gpui::FocusHandle {
    if let Some(focus) = cx.default_global::<WorkPlacement>().focus.get(id) {
        return focus.clone();
    }
    let focus = cx.focus_handle().tab_stop(true);
    cx.default_global::<WorkPlacement>().focus.insert(id.to_string(), focus.clone());
    focus
}

/// How the work shows in a chat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The indicator at the top of the chat.
    pub indicator: bool,
    /// The panel docked on the right, this wide, taking its room from the chat (never over it).
    pub docked: Option<f32>,
}

/// The docked panel's width in `room` (the chat and the panel together): its full width when the chat keeps
/// `CHAT_MIN`, narrower down to `MIN_WIDTH` as the window narrows, and None when even that would squeeze the chat.
pub fn dock_width(room: f32) -> Option<f32> {
    let width = (room - CHAT_MIN).min(WIDTH);
    (width >= MIN_WIDTH).then_some(width)
}

/// The rule: nothing while nothing works; the indicator at the top unless the user docked the panel on the right; the
/// docked panel sits beside the chat, taking its room, whenever `room` fits it beside a readable chat. Hidden with ×,
/// or on a window too narrow for both, the indicator stands in (its details open under it, as at the top), so the
/// panel never covers the messages or the composer.
pub fn layout_for(placement: Placement, dock_open: Option<bool>, room: f32, working: bool) -> Layout {
    let none = Layout { indicator: false, docked: None };
    if !working {
        return none;
    }
    let top = Layout { indicator: true, docked: None };
    if placement == Placement::Top || dock_open == Some(false) {
        return top;
    }
    match dock_width(room) {
        Some(width) => Layout { indicator: false, docked: Some(width) },
        None => top,
    }
}

pub fn layout(room: f32, working: bool, cx: &mut App) -> Layout {
    let state = cx.default_global::<WorkPlacement>();
    layout_for(state.placement, state.dock_open, room, working)
}

/// Esc closes the details under the indicator, then the docked panel (as its ×), escape.rs.
pub fn escape_layers<V: 'static>(layout: &Layout, details_open: bool, window: &mut Window, cx: &mut gpui::Context<V>) {
    if layout.docked.is_some() {
        crate::escape::layer(window, cx, |_, _, cx| set_dock_open(false, cx));
    }
    if details_open {
        crate::escape::popup(window, cx, |_, _, cx| cx.default_global::<WorkPlacement>().details_open = false);
    }
}

/// What the indicator does when clicked (or Return): the details under it at the top (and on a window too narrow to
/// dock), or the hidden docked panel back.
pub fn activate(room: f32, toggle_details: impl FnOnce(&mut App), cx: &mut App) {
    if placement(cx) == Placement::Top || dock_width(room).is_none() {
        toggle_details(cx);
    } else {
        set_dock_open(true, cx);
    }
}

/// What a row's state says it's doing, in the indicator's words.
fn doing(row: &PanelRow) -> &'static str {
    match row.state {
        "queued" => "waiting",
        // A coding task whose turn ended with background work still going (coding_sessions.rs `pill`).
        "monitoring" => "monitoring",
        _ => "working",
    }
}

/// The indicator's words for this chat's tasks, counting exactly the rows it lists: what needs the user first ("Claude
/// Code needs you", "2 tasks need you"), then what they're doing: "Scout is working", "Claude Code is waiting", "2 tasks
/// are working", or each kind counted when they differ ("1 working, 1 waiting"), so a task that hasn't started never
/// counts as working. None with nothing here, so the indicator goes.
pub fn status_text(here: &[PanelRow]) -> Option<String> {
    let first = here.first()?;
    let waiting: Vec<&PanelRow> = here.iter().filter(|row| row.needs_you()).collect();
    Some(match (waiting.as_slice(), here.len()) {
        ([one], _) => format!("{} needs you", one.name),
        ([_, ..], _) => format!("{} tasks need you", waiting.len()),
        ([], 1) => format!("{} is {}", first.name, doing(first)),
        ([], count) if here.iter().all(|row| doing(row) == doing(first)) => {
            format!("{count} tasks are {}", doing(first))
        }
        ([], _) => ["working", "waiting"]
            .into_iter()
            .map(|kind| (here.iter().filter(|row| doing(row) == kind).count(), kind))
            .filter(|(count, _)| *count > 0)
            .map(|(count, kind)| format!("{count} {kind}"))
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// The faces the indicator shows: up to three of this chat's tools' marks, each drawn on nothing (its own transparent
/// shape, no dark square or disc behind it on the badge).
pub fn faces(here: &[PanelRow], cx: &App) -> Vec<AnyElement> {
    here.iter()
        .take(3)
        .map(|row| div().rounded(px(radius::FULL)).child(tool_tile(&row.tool, 20., cx)).into_any_element())
        .collect()
}

/// How wide `text` is in the theme's font at `size` and `weight`, rounded up: to lay out a line before it's drawn.
pub fn text_width(text: &str, size: f32, weight: gpui::FontWeight, cx: &App) -> f32 {
    let system = cx.text_system();
    let font = system.resolve_font(&gpui::Font { weight, ..gpui::font(cx.theme().font_family()) });
    let width: f32 =
        text.chars().map(|ch| system.advance(font, px(size), ch).map_or(0., |advance| f32::from(advance.width))).sum();
    width.ceil() + 1.
}

/// How wide `indicator` draws with `faces` faces and `text`, before anything cuts it short.
pub fn indicator_width(faces: usize, text: &str, cx: &App) -> f32 {
    let text = text_width(text, crate::theme::TypeVariant::Footnote.size(), gpui::FontWeight::SEMIBOLD, cx);
    (indicator_chrome(faces) + text).min(INDICATOR_MAX)
}

/// What `indicator` takes besides its words, with `faces` faces.
fn indicator_chrome(faces: usize) -> f32 {
    let faces = faces.min(3) as f32;
    let start = if faces > 0. { space::XS + 20. * faces - 6. * (faces - 1.) } else { space::MD };
    // The faces' row is there even with none, so the gap after it is too; the border's 1 on each side.
    start + space::XS + space::MD + 2.
}

/// A pill's words `width` wide, in the footnote's semibold: the first of `labels` that fits whole, else the last cut
/// short with "…", or nothing when not even the "…" fits. Cut here, not by GPUI's `truncate`: text that doesn't wrap is
/// laid out once, at the first width asked (`one_line`), so a pill narrower than its words sliced them at its edge.
pub fn fit_label(labels: &[String], width: f32, cx: &App) -> String {
    let (size, weight) = (crate::theme::TypeVariant::Footnote.size(), gpui::FontWeight::SEMIBOLD);
    if let Some(label) = labels.iter().find(|label| text_width(label, size, weight, cx) <= width) {
        return label.clone();
    }
    match labels.last() {
        Some(last) if text_width("…", size, weight, cx) <= width => {
            let font = gpui::Font { weight, ..gpui::font(cx.theme().font_family()) };
            up_to_lines(last, 1, width, font, size, cx).pop().unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// A chat header's name, `width` wide (`fit_header`), cut with "…" when it doesn't fit.
pub fn header_name(name: &str, width: f32, cx: &App) -> gpui::Div {
    let font = gpui::Font { weight: gpui::FontWeight::SEMIBOLD, ..gpui::font(cx.theme().font_family()) };
    let line =
        up_to_lines(name, 1, width, font, crate::theme::TypeVariant::Headline.size(), cx).pop().unwrap_or_default();
    one_line(div().headline(), line, width)
}

/// The indicator's widest.
const INDICATOR_MAX: f32 = 360.;
/// The least of a name the header keeps beside the indicator before cutting the indicator short too.
const NAME_MIN: f32 = 96.;
/// The widest a badge at the header's end is (agents.rs `browser_badge`).
pub const SIDE_BADGE_MAX: f32 = 300.;

/// How wide a badge just before the header's ⋯ may be (agents.rs `browser_badge`), in a header `width` wide: what's
/// left after `lead`, the name's least (`NAME_MIN`, or all of a shorter name) and `trailing` (the ⋯), up to
/// `SIDE_BADGE_MAX`. The indicator then fits between the name and it (`fit_header`, with the badge in its `trailing`).
pub fn side_badge_room(width: f32, lead: f32, name: f32, trailing: f32) -> f32 {
    // The gaps `fit_header` keeps around the indicator.
    (width - lead - name.min(NAME_MIN) - trailing - 2. * space::SM).clamp(0., SIDE_BADGE_MAX)
}

/// Where the name and the indicator go in a chat's header `width` wide, with `lead` before the name (its padding and
/// picture), `name` its text's width, `trailing` after it all (the ⋯), and `pill` the indicator's width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderFit {
    /// The name's width: all of it, or less, cut with "…".
    pub name: f32,
    /// The indicator's left edge and width.
    pub left: f32,
    pub pill: f32,
}

/// The indicator sits in the middle of the header while that's clear of the name and the ⋯; with a longer name it sits
/// just after the name, and only when even that won't fit is the name cut short with "…" (down to `NAME_MIN`, then the
/// indicator is cut too). The two never overlap.
pub fn fit_header(width: f32, lead: f32, name: f32, trailing: f32, pill: f32) -> HeaderFit {
    let gap = space::SM;
    let room = (width - lead - trailing - 2. * gap).max(0.);
    let pill = pill.min((room - name.min(NAME_MIN)).max(0.));
    let name = name.min(room - pill).max(0.);
    let left = ((width - pill) / 2.).max(lead + name + gap).min(width - trailing - gap - pill);
    HeaderFit { name, left, pill }
}

/// The indicator at the top of the chat, in the header's middle: the work badge, `width` wide (`fit_header`), its words
/// cut short with "…" when they need more. Click or Return opens its details (or the docked panel); right-click chooses
/// where the work shows.
#[allow(clippy::too_many_arguments)]
pub fn indicator(
    id: &'static str,
    faces: Vec<AnyElement>,
    text: String,
    width: f32,
    needs_you: bool,
    open: bool,
    on_activate: Rc<dyn Fn(&mut Window, &mut App)>,
    window: &Window,
    cx: &mut App,
) -> AnyElement {
    use gpui_component::menu::ContextMenuExt as _;
    let colors = *cx.colors();
    let text_room = (width.min(INDICATOR_MAX) - indicator_chrome(faces.len())).max(0.);
    let text = fit_label(&[text], text_room, cx);
    let focus = focus_for(id, cx);
    let focused = focus.is_focused(window);
    let (bg, fg) = if needs_you {
        (colors.warning_soft, colors.on_warning_soft)
    } else {
        (colors.accent_soft, colors.on_accent_soft)
    };
    let key_activate = on_activate.clone();
    let badge = div()
        .id(id)
        .track_focus(&focus)
        .flex()
        .items_center()
        .gap(px(space::XS))
        .pl(px(if faces.is_empty() { space::MD } else { space::XS }))
        .pr(px(space::MD))
        .py(px(space::XXS))
        // As wide as the header gives it (`fit_header`): its words get what's left of that.
        .min_w_0()
        .max_w(px(INDICATOR_MAX))
        .rounded(px(radius::FULL))
        .bg(bg)
        .text_color(fg)
        .border_1()
        .border_color(if focused { colors.accent } else { gpui::transparent_black() })
        .when(open, |this| this.shadow_xs())
        .footnote()
        .semibold()
        .cursor_pointer()
        .child(
            div().flex_none().flex().children(
                faces
                    .into_iter()
                    .enumerate()
                    .map(|(index, face)| div().when(index > 0, |this| this.ml(px(-6.))).child(face)),
            ),
        )
        .child(one_line(div(), text, text_room))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event: &ClickEvent, window, cx| {
            // A right-click opens the placement menu (`context_menu`), not the details.
            if !event.standard_click() {
                return;
            }
            cx.stop_propagation();
            on_activate(window, cx);
            window.refresh();
        })
        .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                key_activate(window, cx);
                window.refresh();
            }
        })
        .context_menu(placement_menu);
    crate::smoke::tracked(id, badge)
}

/// Whether an indicator has the keyboard (the fixture's checks).
#[cfg(feature = "dev")]
pub fn is_focused(id: &str, window: &Window, cx: &App) -> bool {
    cx.try_global::<WorkPlacement>().and_then(|state| state.focus.get(id)).is_some_and(|focus| focus.is_focused(window))
}

/// Top center and Right sidebar, the one in use checked: the only way to move the work (the right-click menu).
pub fn placement_menu(
    menu: gpui_component::menu::PopupMenu,
    _: &mut Window,
    cx: &mut gpui::Context<gpui_component::menu::PopupMenu>,
) -> gpui_component::menu::PopupMenu {
    use gpui_component::menu::PopupMenuItem;
    let current = placement(cx);
    Placement::ALL.into_iter().fold(menu, |menu, choice| {
        menu.item(PopupMenuItem::new(choice.label()).checked(choice == current).on_click(move |_, window, cx| {
            set_placement(choice, cx);
            window.refresh();
        }))
    })
}

pub type OnOpen = Rc<dyn Fn(&Target, &mut Window, &mut App)>;

/// What a row does in the app: pages open over the chat (Back returns to it, draft and place kept), a question
/// scrolls to its card.
pub fn opener(conversation: Option<gpui::Entity<super::conversation::Conversation>>) -> OnOpen {
    Rc::new(move |target, window, cx| match target {
        Target::CodingTask(id) => crate::nav::push(crate::nav::Route::CodingTask(id.clone()), window, cx),
        Target::Question(id) => {
            let shown = conversation.as_ref().is_some_and(|conversation| {
                conversation.update(cx, |conversation, cx| conversation.reveal_question(id, cx))
            });
            if !shown {
                crate::nav::push(crate::nav::Route::CodingTask(id.clone()), window, cx);
            }
        }
    })
}

// ---- The popover ----

/// The indicator's details at the top, in Filo's chat and in an agent's alike: this chat's tasks as the docked panel's
/// compact rows, in a popover under the header (`top` points down). A row opens its task.
pub fn popover(id: &'static str, top: f32, here: &[PanelRow], on_open: OnOpen, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let rows: Vec<AnyElement> =
        here.iter().map(|row| compact_row(row, POPOVER_ROW_WIDTH, on_open.clone(), cx)).collect();
    let card = div()
        .id(SharedString::from(format!("{id}-card")))
        .w(px(DETAILS_WIDTH))
        .max_h(px(480.))
        .overflow_y_scroll()
        .p(px(space::XS))
        .rounded(px(radius::LG))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(2.))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        // Over the chat: clicks and the scroll wheel are the card's (a long list scrolls here, not the chat).
        .occlude()
        .children(rows);
    let card = div().mt(px(space::XS)).child(card);
    // The scroll wheel over it scrolls its list, not closing it (it's one menu of escape.rs's popups). First, so it keeps
    // the pointer from what's under the card, not from the card's rows.
    let card = div().relative().child(gpui_component::one_menu::surface()).child(card);
    div()
        .id(id)
        .absolute()
        .top(px(top))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(crate::smoke::tracked(id, card))
        .into_any_element()
}

/// The indicator's details in an agent's chat: the popover, closing behind what a row opens.
pub fn details(here: &[PanelRow], on_open: OnOpen, cx: &App) -> AnyElement {
    let close: OnOpen = Rc::new(move |target, window, cx| {
        cx.default_global::<WorkPlacement>().details_open = false;
        on_open(target, window, cx);
    });
    popover("working-details", 44., here, close, cx)
}

/// The panel docked on the right, `width` wide, beside the chat (`layout_for`): the sunken side-panel colour of the
/// chat history, a plain title, and compact rows. Its title's right-click moves it back to the top; × hides it (the
/// indicator brings it back).
pub fn render(here: &[PanelRow], width: f32, on_open: OnOpen, cx: &App) -> AnyElement {
    use gpui_component::menu::ContextMenuExt as _;
    let colors = *cx.colors();
    let rows: Vec<AnyElement> =
        here.iter().map(|row| compact_row(row, width - 2. * space::XS, on_open.clone(), cx)).collect();
    let body = div()
        .id("working-panel-rows")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(space::XS))
        .pb(px(space::MD))
        .children(rows);
    let title = div()
        .id("working-panel-title")
        .flex_none()
        .h(px(44.))
        .pl(px(space::MD))
        .pr(px(space::XS))
        .flex()
        .items_center()
        .child(div().flex_1().subhead().semibold().child("Working"))
        .child(crate::smoke::tracked(
            "working-panel-close",
            ui::IconButton::new("working-panel-close", "x").small().tooltip("Hide").on_click(|_, window, cx| {
                set_dock_open(false, cx);
                window.refresh();
            }),
        ))
        .context_menu(placement_menu);
    let panel = div()
        .id("working-panel")
        .w(px(width))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(colors.sunken)
        .border_l_1()
        .border_color(colors.border.opacity(0.6))
        .child(crate::smoke::tracked("working-panel-title", title))
        .child(body);
    crate::smoke::tracked("working-panel", panel).into_any_element()
}

/// One row, in the docked panel and in the popover at the top: the tool's mark (which names the tool on hover), the
/// task in up to two lines with its state and how long it has run ("Working · 4m", "Needs you · 1h 5m") under it, then
/// what it's doing now. The whole row opens it. `width` is the row's: its text gets set widths from it (`one_line`).
fn compact_row(row: &PanelRow, width: f32, on_open: OnOpen, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    // The text's room: the row less its padding, the mark and the gap after it.
    let room = width - 2. * space::SM - FACE - space::SM;
    let target = row.target.clone();
    let status_color = match row.state {
        "waiting" => colors.warning,
        _ => colors.text_secondary,
    };
    let status_id = format!("working-row-status-{}", row.key);
    let status = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(3.))
        .caption()
        .text_color(status_color)
        .when(row.state == "monitoring", |this| this.child(ui::icon_sized("eye", 12., status_color)))
        .child(status_time(row.label, row.since, crate::format::now_ms()));
    let status =
        div().flex_none().flex().items_center().gap(px(space::XS)).child(crate::smoke::tracked(status_id, status));
    let line = |text: String, color: gpui::Hsla, width: f32| one_line(div().caption().text_color(color), text, width);
    // Up to two lines, ending in "…" when it's longer; the whole of it and where it runs show beside the row on hover.
    let top = div().w_full().flex().items_center().gap(px(space::XS)).child(title_block(row, &row.tool, room, cx));
    let id = format!("working-row-{}", row.key);
    let tip = tip_shown(&row.key, cx).then(|| title_tip(row, &row.tool, cx));
    let key = row.key.clone();
    let element = div()
        .id(SharedString::from(id.clone()))
        .relative()
        .w_full()
        .flex()
        .items_start()
        .gap(px(space::SM))
        .px(px(space::SM))
        .py(px(6.))
        .rounded(px(radius::MD))
        .cursor_pointer()
        .hover(|style| style.bg(colors.selected_fill.opacity(0.5)))
        .when(row.needs_you(), |this| this.bg(colors.warning_soft.opacity(0.35)))
        // The row doesn't write the tool's name: its mark names it on hover.
        .child(div().flex_none().pt(px(1.)).child(named_tool_tile(
            format!("working-row-tool-{}", row.key),
            &row.tool,
            FACE,
            cx,
        )))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(top)
                .child(div().w_full().flex().child(status))
                .when_some(row.detail.clone().filter(|detail| !detail.trim().is_empty()), |this, detail| {
                    this.child(line(detail, colors.text_tertiary, room))
                }),
        )
        .children(tip)
        // Dragged onto the composer, the message points at it (work_share.rs).
        .on_drag(super::work_share::DraggedWork(super::work_share::SharedWork::from_row(row)), |dragged, _, _, cx| {
            set_tip(&dragged.0.key(), false, cx);
            cx.new(|_| dragged.clone())
        })
        .on_click(move |event: &ClickEvent, window, cx| {
            if event.standard_click() {
                // What it opens covers the row, and the pointer leaving it then goes unseen.
                set_tip(&key, false, cx);
                on_open(&target, window, cx);
                window.refresh();
            }
        });
    crate::smoke::tracked(id, element)
}

/// One line of `text`, `width` wide, cut with "…" at its edge; `style` sets the type.
/// The width is set rather than left to the flex row: GPUI lays out text that doesn't wrap once a frame, at the first
/// width layout asks it about, and a pass that sizes a box by its content can ask with that content's width (a title in
/// a wide panel came out "Make Fe…", as wide as the "Working · 1m" under it). A set width is the same in every pass.
pub(crate) fn one_line(style: gpui::Div, text: String, width: f32) -> gpui::Div {
    style.flex_none().w(px(width.max(0.))).truncate().child(text)
}

/// `text` in lines `width` wide, broken where GPUI would wrap it in `font` at `size`: at most two, the second cut with
/// "…" when the rest won't fit on it.
pub(crate) fn two_lines(text: &str, width: f32, font: gpui::Font, size: f32, cx: &App) -> Vec<String> {
    up_to_lines(text, 2, width, font, size, cx)
}

/// `text` in lines `width` wide, broken where GPUI would wrap it in `font` at `size`: at most `max`, the last cut with
/// "…" when the rest won't fit on it. Line breaks in `text` become spaces.
pub(crate) fn up_to_lines(text: &str, max: usize, width: f32, font: gpui::Font, size: f32, cx: &App) -> Vec<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let max = max.max(1);
    let mut wrapper = cx.text_system().line_wrapper(font.clone(), px(size));
    let breaks: Vec<usize> = wrapper
        .wrap_line(&[gpui::LineFragment::text(&text)], px(width))
        .map(|boundary| boundary.ix)
        .take(max)
        .collect();
    let mut lines = Vec::with_capacity(max);
    let mut start = 0;
    for &end in breaks.iter().take(max - 1) {
        lines.push(text[start..end].trim_end().to_string());
        start = end;
    }
    let rest = &text[start..];
    let last = if breaks.len() < max {
        rest.to_string()
    } else {
        let run = gpui::TextRun {
            len: rest.len(),
            font,
            color: gpui::Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let cut = wrapper.truncate_line(rest.to_string().into(), px(width), "…", &mut vec![run]);
        // No space before the "…".
        match cut.strip_suffix('…') {
            Some(kept) => format!("{}…", kept.trim_end()),
            None => cut.to_string(),
        }
    };
    lines.push(last);
    lines
}

/// A coding row's task, `width` wide, in up to two lines (`two_lines`). The pointer on it, anywhere on either line,
/// shows `title_tip`; off it, the tip goes. Whether it's on is read again each frame the title is drawn, from where the
/// pointer is, so the tip can't stick on or stay off after a missed hover event (GPUI's `on_hover` only reports a
/// change it saw, and a click, a press or a row that moved under a still pointer left it out of step); `on_hover` just
/// asks for that frame. Only where the title shows: not under a menu (gpui-component's one_menu.rs) or anything else
/// drawn over it, as GPUI's hover goes (the hit test, which is this frame's once it paints).
fn title_block(row: &PanelRow, tool: &str, width: f32, cx: &App) -> AnyElement {
    let key = row.key.clone();
    let font = gpui::Font { weight: gpui::FontWeight::SEMIBOLD, ..gpui::font(cx.theme().font_family()) };
    let lines = two_lines(&task_heading(&row.title, tool), width, font, crate::theme::TypeVariant::Footnote.size(), cx);
    let follow = gpui::canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_, hitbox, window, cx| {
            let on = hitbox.is_hovered(window);
            if on != tip_shown(&key, cx) {
                set_tip(&key, on, cx);
                window.refresh();
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let id = format!("working-row-title-{}", row.key);
    let title = div()
        .id(SharedString::from(id.clone()))
        .relative()
        .flex_none()
        .w(px(width.max(0.)))
        .flex()
        .flex_col()
        .footnote()
        .semibold()
        .children(lines.into_iter().map(|line| one_line(div(), line, width)))
        .child(follow)
        .on_hover(|_, window, _| window.refresh());
    crate::smoke::tracked(id, title).into_any_element()
}

/// The tip's width at most; the title wraps inside it.
const TIP_MAX_WIDTH: f32 = 280.;

/// A coding row's details while its title is hovered: the whole title, then its project, computer, branch and model
/// (only those the backend sent). Not its state, time or what it's doing now: the row says those. The app's tooltip look (`gpui_component`'s
/// Tooltip). It sits left of the row, its top at the row's (the docked panel is at the window's right, the chat to its
/// left); pushed back inside the window near an edge. Drawn above everything, but nothing in it takes the pointer, so
/// it never takes a click or covers a button from it, and it takes no room from the rows.
fn title_tip(row: &PanelRow, tool: &str, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let fact = |icon: &str, text: &str| {
        let mark = match icon {
            "model" => tool_tile(tool, 14., cx).into_any_element(),
            icon => ui::icon_sized(icon, 14., colors.text_secondary).into_any_element(),
        };
        div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(div().flex_none().child(mark))
            .child(div().min_w_0().child(text.to_string()))
    };
    let card = div()
        .max_w(px(TIP_MAX_WIDTH))
        .flex()
        .flex_col()
        .gap(px(space::XS))
        .px(px(space::SM + 2.))
        .py(px(space::SM))
        .rounded(px(radius::XS))
        .bg(colors.raised)
        .text_color(colors.text)
        .border_1()
        .border_color(colors.border)
        .shadow_md()
        .child(div().footnote().semibold().child(task_heading(&row.title, tool)))
        .children((!row.facts.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .caption()
                .children(row.facts.iter().map(|(icon, text)| fact(icon, text)))
        }));
    let tip = gpui::anchored()
        .anchor(gpui::Corner::TopRight)
        .offset(gpui::point(px(-space::SM), px(0.)))
        .snap_to_window_with_margin(px(8.))
        .child(card);
    div().absolute().top_0().left_0().child(gpui::deferred(tip).with_priority(2)).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, status: &str, pending: f64) -> CodingSession {
        serde_json::from_value(serde_json::json!({
            "taskId": id, "tool": "claude-code", "title": format!("Coding {id}"), "status": status, "now": "Running tests",
            "pending": pending, "createdAt": 1.0, "updatedAt": 1.0,
        }))
        .unwrap()
    }

    #[test]
    fn this_chat_lists_what_the_badge_counts_needing_you_first() {
        let rows = here_rows(&[session("s", "running", 0.), session("q", "running", 1.)]);
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        // The question comes first.
        assert_eq!(keys, ["coding-q", "coding-s"]);
        assert_eq!(rows[0].target, Target::Question("q".into()));
        assert_eq!(rows[1].target, Target::CodingTask("s".into()));
        // A coding card says what the tool is doing now, as in Filo's chat.
        assert_eq!(rows[1].detail.as_deref(), Some("Running tests"));
    }

    #[test]
    fn the_indicator_never_covers_the_name() {
        // A short name: the indicator in the middle, the name whole.
        assert_eq!(fit_header(630., 60., 40., 40., 180.), HeaderFit { name: 40., left: 225., pill: 180. });
        // "Desktop App Developer": too long for the middle, so the indicator follows it, and the name stays whole.
        let fit = fit_header(630., 60., 190., 40., 180.);
        assert_eq!((fit.name, fit.left, fit.pill), (190., 258., 180.));
        // Longer still: the name is cut short just before the indicator, which ends before the ⋯.
        let fit = fit_header(630., 60., 400., 40., 180.);
        assert_eq!(fit.pill, 180.);
        assert_eq!(fit.left, 60. + fit.name + space::SM);
        assert_eq!(fit.left + fit.pill, 630. - 40. - space::SM);
        // A narrow header: the name keeps its least, the indicator is cut short beside it, and nothing is negative.
        let fit = fit_header(320., 60., 400., 40., 180.);
        assert_eq!(fit.name, NAME_MIN);
        assert!(fit.left >= 60. + fit.name + space::SM && fit.left + fit.pill <= 320. - 40. - space::SM);
        let fit = fit_header(100., 60., 400., 40., 180.);
        assert!(fit.name >= 0. && fit.pill >= 0.);
    }

    #[test]
    fn the_badge_before_the_menu_leaves_the_name_its_least() {
        // A wide header: the badge gets its widest.
        assert_eq!(side_badge_room(1000., 68., 120., 36.), SIDE_BADGE_MAX);
        // The narrowest chat (440): what's left after the name's least, which the name then keeps beside it.
        let room = side_badge_room(440., 68., 120., 36.);
        assert_eq!(room, 440. - 68. - NAME_MIN - 36. - 2. * space::SM);
        let fit = fit_header(440., 68., 120., 36. + room, 0.);
        assert_eq!(fit.name, NAME_MIN);
        // A short name leaves the badge more; no room is never negative.
        assert_eq!(side_badge_room(440., 68., 40., 36.), 440. - 68. - 40. - 36. - 2. * space::SM);
        assert_eq!(side_badge_room(150., 68., 120., 36.), 0.);
    }

    #[test]
    fn the_top_is_the_default_and_only_a_choice_docks_it() {
        // No file, an old or broken one, an unknown value: the top.
        for text in ["", "{}", "not json", r#"{"placement":"middle"}"#, r#"{"placement":3}"#] {
            assert_eq!(parse_placement(text), Placement::Top, "{text}");
            assert!(!is_legacy_left(text), "{text}");
        }
        assert_eq!(parse_placement(r#"{"placement":"right"}"#), Placement::Right);
        assert_eq!(parse_placement(r#"{"placement":"top"}"#), Placement::Top);
        // The left sidebar is gone: an older app's "left" is the top, and is rewritten as such.
        assert_eq!(parse_placement(r#"{"placement":"left"}"#), Placement::Top);
        assert!(is_legacy_left("{\"placement\":\"left\"}\n"));
        assert!(!is_legacy_left(r#"{"placement":"right"}"#));
        // Only the top and the right, in sentence case, in the right-click menu.
        assert_eq!(Placement::ALL.map(Placement::label), ["Top center (default)", "Right sidebar"]);
    }

    #[test]
    fn where_the_work_shows() {
        let top = Layout { indicator: true, docked: None };
        let nothing = Layout { indicator: false, docked: None };
        let wide = 1200.;
        // Work arriving never docks a panel by itself: at the top, wide or narrow.
        assert_eq!(layout_for(Placement::Top, None, wide, true), top);
        assert_eq!(layout_for(Placement::Top, Some(true), wide, true), top);
        assert_eq!(layout_for(Placement::Top, None, 500., true), top);
        // Nothing working: nothing shows.
        assert_eq!(layout_for(Placement::Right, None, wide, false), nothing);
        assert_eq!(layout_for(Placement::Top, None, wide, false), nothing);
        // Docked, with room: beside the chat on the right, at its full width.
        assert_eq!(layout_for(Placement::Right, None, wide, true), Layout { indicator: false, docked: Some(WIDTH) });
        // Hidden with ×: the indicator stands in, and brings it back.
        assert_eq!(layout_for(Placement::Right, Some(false), wide, true), top);
        // A window that used to put the panel over the chat (under 900 points for both) still docks it beside.
        assert_eq!(layout_for(Placement::Right, None, 860., true), Layout { indicator: false, docked: Some(WIDTH) });
        // Tighter: the panel narrows so the chat keeps its readable width.
        assert_eq!(
            layout_for(Placement::Right, Some(true), CHAT_MIN + 240., true),
            Layout { indicator: false, docked: Some(240.) }
        );
        // Too narrow for both: never over the chat; the indicator at the top, opened or not.
        assert_eq!(layout_for(Placement::Right, None, CHAT_MIN + MIN_WIDTH - 1., true), top);
        assert_eq!(layout_for(Placement::Right, Some(true), 500., true), top);
    }

    #[test]
    fn the_docked_panel_leaves_the_chat_readable() {
        assert_eq!(dock_width(2000.), Some(WIDTH));
        assert_eq!(dock_width(CHAT_MIN + WIDTH), Some(WIDTH));
        assert_eq!(dock_width(CHAT_MIN + MIN_WIDTH), Some(MIN_WIDTH));
        assert_eq!(dock_width(CHAT_MIN + MIN_WIDTH - 1.), None);
        for room in [CHAT_MIN + MIN_WIDTH, 700., 860., 1000., 1600.] {
            let width = dock_width(room).unwrap();
            assert!(room - width >= CHAT_MIN, "{room}");
            assert!((MIN_WIDTH..=WIDTH).contains(&width), "{room}");
        }
    }

    #[test]
    fn every_row_says_its_state_and_for_how_long() {
        let minute = 60_000.;
        assert_eq!(short_age(0., 20_000.), "<1m");
        assert_eq!(short_age(0., 4. * minute), "4m");
        assert_eq!(short_age(0., 60. * minute), "1h");
        assert_eq!(short_age(0., 65. * minute), "1h 5m");
        assert_eq!(short_age(0., 49. * 60. * minute), "2d");
        // A clock a little behind the backend's: not a negative time.
        assert_eq!(short_age(10_000., 0.), "<1m");
        assert_eq!(status_time("Working", Some(0.), 4. * minute), "Working · 4m");
        assert_eq!(status_time("Needs you", Some(0.), 65. * minute), "Needs you · 1h 5m");
        assert_eq!(status_time("Paused", None, 4. * minute), "Paused");
        // Each row carries when its work started: the coding task's current round's start.
        let running = CodingSession { round_started_at: Some(5_000.), ..session("s", "running", 0.) };
        assert_eq!(here_rows(&[running]).iter().map(|row| row.since).collect::<Vec<_>>(), [Some(5_000.)]);
    }

    #[test]
    fn the_badge_counts_the_tasks_it_lists() {
        // Two Claude Code tasks this chat started: two tasks, not "agents", and no other chat's work (the rows come only
        // from this chat's `sessionsForChat`, asked for by its id).
        let two = here_rows(&[session("a", "running", 0.), session("b", "running", 0.)]);
        assert_eq!(two.len(), 2);
        assert_eq!(status_text(&two).as_deref(), Some("2 tasks are working"));
        assert_eq!(status_text(&here_rows(&[session("a", "running", 0.)])).as_deref(), Some("Claude Code is working"));
        // What needs the user leads, by its own count.
        let one_waiting = here_rows(&[session("s", "running", 0.), session("q", "running", 1.)]);
        assert_eq!(status_text(&one_waiting).as_deref(), Some("Claude Code needs you"));
        let two_waiting =
            here_rows(&[session("q", "running", 1.), session("r", "running", 1.), session("s", "running", 0.)]);
        assert_eq!(status_text(&two_waiting).as_deref(), Some("2 tasks need you"));
        // Nothing here: no words, so no indicator, whatever runs in other chats.
        assert_eq!(status_text(&[]), None);
        assert_eq!(layout_for(Placement::Top, None, 1200., false), Layout { indicator: false, docked: None });
    }

    #[test]
    fn switching_chats_switches_the_tasks() {
        // Each chat's rows are its own: the tasks asked for by its id. Chat A has two Claude Code tasks; chat B, opened
        // after it, only its own coding task; chat C nothing, so no indicator there.
        let a = here_rows(&[session("c1", "running", 0.), session("c2", "running", 0.)]);
        let b = here_rows(&[session("b1", "running", 1.)]);
        let c = here_rows(&[]);
        let keys = |rows: &[PanelRow]| rows.iter().map(|row| row.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&a), ["coding-c1", "coding-c2"]);
        assert_eq!(keys(&b), ["coding-b1"]);
        assert_eq!(status_text(&a).as_deref(), Some("2 tasks are working"));
        assert_eq!(status_text(&b).as_deref(), Some("Claude Code needs you"));
        assert_eq!(status_text(&c), None);
        assert_eq!(layout_for(Placement::Right, None, 1200., !c.is_empty()), Layout { indicator: false, docked: None });
    }

    #[test]
    fn times_move_on_as_often_as_they_show() {
        // Nothing shows a time: the ticker doesn't redraw, and looks again in half a minute.
        assert_eq!(newest_time(std::iter::empty()), None);
        assert_eq!(tick_after(None, 0.), std::time::Duration::from_secs(30));
        // Starts not known: half a minute is plenty.
        assert_eq!(newest_time([None, None]), Some(0.));
        assert_eq!(tick_after(newest_time([None]), 600_000.), std::time::Duration::from_secs(30));
        // The newest under a minute old counts seconds ("Working 12s"); older, minutes.
        assert_eq!(newest_time([Some(1_000.), None, Some(500_000.)]), Some(500_000.));
        assert_eq!(tick_after(Some(500_000.), 512_000.), std::time::Duration::from_secs(1));
        assert_eq!(tick_after(Some(500_000.), 600_000.), std::time::Duration::from_secs(30));
    }

    fn coding(status: &str, now: &str, pending: f64) -> CodingSession {
        CodingSession {
            status: status.into(),
            now: now.into(),
            pending,
            created_at: 1_000.,
            round_started_at: Some(1_000.),
            ..session("t", status, 0.)
        }
    }

    /// The one row a session makes: (its state's words, the line under it, its time).
    fn shown(session: CodingSession, now: f64) -> (&'static str, Option<String>, String) {
        let row = here_rows(&[session]).remove(0);
        (row.label, row.detail.clone(), status_time(row.label, row.since, now))
    }

    #[test]
    fn a_queued_task_waits_then_works() {
        let minute = 60_000.;
        // Queued: waiting, why underneath, and no time (it hasn't run, and the wait's start isn't known).
        let queued = shown(coding("queued", "Waiting for the computer", 0.), 5. * minute);
        assert_eq!(queued, ("Waiting", Some("Waiting for the computer".into()), "Waiting".into()));
        // Claimed, no step logged yet: working, once, with no "Starting" under it.
        let started = shown(coding("running", "Starting", 0.), 2. * minute);
        assert_eq!(started, ("Working", None, "Working · 1m".into()));
        // Its first step: what it's doing.
        let stepping = shown(coding("running", "Running npm test", 0.), 2. * minute);
        assert_eq!(stepping, ("Working", Some("Running npm test".into()), "Working · 1m".into()));
        assert_eq!(
            status_text(&here_rows(&[coding("queued", "Waiting for the computer", 0.)])).as_deref(),
            Some("Claude Code is waiting")
        );
        assert_eq!(
            status_text(&here_rows(&[coding("running", "Starting", 0.)])).as_deref(),
            Some("Claude Code is working")
        );
    }

    #[test]
    fn a_task_behind_another_says_which() {
        // The backend's reason (queuedBehind): another task in the folder it shares, or a full computer.
        for reason in [
            "Waiting for \"Review it\" to finish",
            "Waiting for a free slot on Alice's Mac (3 of 3 running)",
            "Waiting for disk space on Alice's Mac",
        ] {
            assert_eq!(
                shown(coding("queued", reason, 0.), 1e6),
                ("Waiting", Some(reason.to_string()), "Waiting".into()),
                "{reason}"
            );
        }
        // Its question with its agent: waiting, not working, and why.
        assert_eq!(shown(coding("waiting_on_agent", "Has a question", 0.), 1e6).0, "Waiting");
        assert_eq!(shown(coding("waiting_on_agent", "Has a question", 0.), 1e6).1.as_deref(), Some("Has a question"));
        // One working and one waiting behind it: each counted as what it is.
        let both = here_rows(&[
            coding("running", "Editing main.rs", 0.),
            CodingSession { task_id: "u".into(), ..coding("queued", "Waiting for \"Fix it\" to finish", 0.) },
        ]);
        assert_eq!(status_text(&both).as_deref(), Some("1 working, 1 waiting"));
    }

    #[test]
    fn a_task_whose_computer_is_off_waits_for_it() {
        let offline = CodingSession {
            computer: Some(super::super::coding_sessions::Computer { name: "Sam's MacBook".into(), last_seen_at: 0. }),
            ..coding("queued", "Waiting for the computer", 0.)
        };
        let row = here_rows(&[offline]).remove(0);
        assert_eq!(row.label, "Waiting");
        // here_rows reads the real clock: a computer last seen at 0 is long gone.
        assert_eq!(row.detail.as_deref(), Some("Waiting for Sam's MacBook to come back online"));
        assert_eq!(row.since, None);
    }

    #[test]
    fn a_question_for_the_user_needs_them() {
        // Waiting on the user: "Needs you" alone, not the backend's "Waiting on you" again under it.
        let asked = here_rows(&[coding("waiting_on_you", "Waiting on you", 0.)]).remove(0);
        assert_eq!(
            (asked.label, asked.detail.clone(), asked.target.clone()),
            ("Needs you", None, Target::Question("t".into()))
        );
        // A permission request mid-run: needs the user, with the step it asked about.
        let permission = here_rows(&[coding("running", "Running git push", 1.)]).remove(0);
        assert_eq!((permission.label, permission.detail.as_deref()), ("Needs you", Some("Running git push")));
        assert_eq!(status_text(&[asked]).as_deref(), Some("Claude Code needs you"));
    }

    #[test]
    fn no_line_repeats_the_state() {
        // Nothing logged, or only a status: the state alone.
        for (status, now) in
            [("running", ""), ("running", "  "), ("running", "Starting"), ("running", "Working"), ("queued", "Queued")]
        {
            assert_eq!(here_rows(&[coding(status, now, 0.)])[0].detail, None, "{status} {now}");
        }
    }

    #[test]
    fn the_time_is_the_rounds_running_time() {
        let minute = 60_000.;
        let hour = 60. * minute;
        let at = |status: &str, round: Option<f64>| CodingSession {
            created_at: 0.,
            round_started_at: round,
            ..coding(status, "Running npm test", 0.)
        };
        // A new task: queued five minutes with no time, then claimed at 5m; at 7m it has worked 2m, not 7m.
        assert_eq!(shown(at("queued", None), 5. * minute).2, "Waiting");
        assert_eq!(shown(at("running", Some(5. * minute)), 7. * minute).2, "Working · 2m");
        // Its second round the next morning: 3 minutes in, not the 14 hours since the task began.
        assert_eq!(shown(at("queued", None), 14. * hour).2, "Waiting");
        assert_eq!(shown(at("running", Some(14. * hour)), 14. * hour + 3. * minute).2, "Working · 3m");
        // Paused (its question with its agent, or its computer gone): "Waiting", no time, even if a start came along.
        assert_eq!(shown(at("waiting_on_agent", Some(minute)), 9. * minute).2, "Waiting");
        assert_eq!(shown(at("waiting_on_you", Some(minute)), 9. * minute).2, "Needs you");
        // Resumed: the backend moves the start on by the pause (2m run, 3m paused, then 1m more reads 3m).
        assert_eq!(shown(at("running", Some(9. * minute - 3. * minute)), 9. * minute).2, "Working · 3m");
        // A backend or round without the start: "Working" alone, never the task's age.
        assert_eq!(shown(at("running", None), 14. * hour).2, "Working");
        assert_eq!(shown(at("running", Some(0.)), 14. * hour).2, "Working");
        // The same row drawn again later counts on from the same start (nothing kept on the client to reset).
        let row = || here_rows(&[at("running", Some(5. * minute))]).remove(0).since;
        assert_eq!(row(), row());
    }

    #[test]
    fn a_coding_rows_details_are_only_what_the_server_sent() {
        let full: CodingSession = serde_json::from_value(serde_json::json!({
            "taskId": "f", "tool": "claude-code", "title": "Add weekly and monthly usage limits", "status": "running", "now": "Running tests",
            "pending": 0.0, "createdAt": 1.0, "updatedAt": 1.0, "repoName": "notes-app", "branch": "main",
            "model": "claude-opus-5-5", "roundStartedAt": 1.0, "computer": { "name": "Sam's MacBook", "lastSeenAt": 1.0 },
        }))
        .unwrap();
        let row = here_rows(&[full]).remove(0);
        let facts = |row: &PanelRow| row.facts.clone();
        assert_eq!(
            facts(&row),
            [
                ("folder-git-2", "notes-app".to_string()),
                ("laptop", "Sam's MacBook".into()),
                ("git-branch", "main".into()),
                ("model", "claude-opus-5-5".into())
            ]
        );
        // The full title stays on the row (only drawing cuts it); the tip shows it whole.
        assert_eq!(row.title, "Add weekly and monthly usage limits");
        // An older backend sends none of them: no details made up, the title alone.
        assert!(facts(&here_rows(&[session("o", "running", 0.)]).remove(0)).is_empty());
        // Some, or blank: only those with words, in their order.
        let some = CodingSession {
            repo_name: Some("notes-app".into()),
            branch: Some("  ".into()),
            model: Some("gpt-5.5".into()),
            ..session("p", "running", 0.)
        };
        assert_eq!(
            facts(&here_rows(&[some]).remove(0)),
            [("folder-git-2", "notes-app".to_string()), ("model", "gpt-5.5".into())]
        );
        // A folder with no git history shows as a plain folder.
        let plain =
            CodingSession { repo_name: Some("filo-desktop".into()), plain_folder: true, ..session("d", "running", 0.) };
        assert_eq!(facts(&here_rows(&[plain]).remove(0)), [("folder", "filo-desktop".to_string())]);
    }

    #[test]
    fn nothing_working_means_no_rows() {
        assert!(
            here_rows(
                &[session("done", "done", 0.)].into_iter().filter(|session| !session.is_finished()).collect::<Vec<_>>()
            )
            .is_empty()
        );
    }
}

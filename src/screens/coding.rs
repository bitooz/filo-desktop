// The Coding page: Claude Code, Codex, Muse Code and OpenCode, run on this Mac with the user's own sign-in. Computers
// (this Mac, always on; "Keep coding after Filo quits"), Tools (plug in or out, each one's mode, model and
// reasoning level, and how much of its plan's 5-hour window and week is left), where tasks work, Tasks, and Projects (the repos found under the home folder and the folders the
// user added, git or not); a task opens its thread (coding_thread.rs). All of it is in Filo's database on this Mac
// (local_backend/coding.rs). The tool names, labels and proof pieces here are shared with the thread and the chat's
// coding rows (chat/coding_sessions.rs).

use std::collections::HashMap;

use gpui::Corner;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::Disableable as _;
use gpui_component::Sizable as _;
use gpui_component::button::Button as MenuButton;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{Icon, IconName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backend;
use crate::config;
use crate::file_preview;
use crate::format;
use crate::nav::{self, Route};
use crate::screens::search_menu;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton, Tone};
use filo_local::coding::effort_label;

/// How many projects the list shows before asking for a search.
const PROJECTS_SHOWN: usize = 50;
/// Projects shown before "Show all" or a search.
const PROJECTS_PREVIEW: usize = 6;
/// Finished tasks shown under the ones still going, before "Show all".
const FINISHED_SHOWN: usize = 5;
/// A task's or a project's line.
const LINE_HEIGHT: f32 = 34.;
/// More models than this (with "Default") and the model picker is searched rather than a menu.
const MODELS_IN_MENU: usize = 16;
/// The searched model picker's width.
const MODEL_LIST_WIDTH: f32 = 340.;
/// How often the tools' plan usage is asked again while the page is open.
const USAGE_EVERY: std::time::Duration = std::time::Duration::from_secs(2 * 60);
/// How long the backend keeps its answer (local_backend/coding.rs `USAGE_FRESH`): an ask within it comes back the same
/// at once, so the pills aren't dimmed for it.
const USAGE_KEPT: std::time::Duration = std::time::Duration::from_secs(30);
/// The last `coding:usage` answer, kept beside `local/` so the pills show it from the first frame after a restart.
const USAGE_SAVED: &str = "desktop-plan-usage.json";
/// The tools whose accounts report plan windows (local_backend/coding.rs `plan_usage`): before any answer, their pills'
/// room is held by placeholders.
const USAGE_TOOLS: [&str; 2] = ["claude-code", "codex"];

/// A tool's name ("Claude Code" for "claude-code"); an id no tool has, as it is.
pub(crate) fn tool_name(id: &str) -> String {
    filo_local::coding::engine_of(id).map_or_else(|| id.to_string(), |engine| engine.label().to_string())
}

/// How a task's status reads.
pub(crate) fn status_label(status: &str) -> &'static str {
    match status {
        "queued" => "Queued",
        "running" => "Working",
        "monitoring" => "Monitoring",
        "waiting_on_agent" => "Has a question",
        "waiting_on_you" => "Waiting on you",
        "done" => "Done",
        "failed" => "Failed",
        "stopped" => "Stopped",
        _ => "Unknown",
    }
}

/// A task's one state, read from its status the same way everywhere it shows (the chat's task card, rows, badge and
/// panel, the Coding page, a round's result page): `agent_work::state_pill`'s look and its words. `status` is the
/// task's as the backend sends it (a running one asking the user is "waiting_on_you", one whose question is with its
/// agent "waiting_on_agent"); `needs_you`: a question for the user is open.
pub(crate) fn task_state(status: &str, needs_you: bool) -> (&'static str, &'static str) {
    match status {
        "done" => ("completed", "Done"),
        "failed" => ("failed", "Failed"),
        "stopped" => ("canceled", "Stopped"),
        _ if needs_you || status == "waiting_on_you" => ("waiting", "Needs you"),
        // Not started (its folder's other task, a full computer, or the computer itself), or its question is with its agent.
        "queued" | "waiting_on_agent" => ("queued", "Waiting"),
        // Its turn ended with background work still going (a build, a Monitor): it picks the task back up after.
        "monitoring" => ("monitoring", "Monitoring"),
        _ => ("active", "Working"),
    }
}

/// What a round's card says: while its task still works (a later round runs, or it waits on background work), the
/// task's state, the same as its row; after, how the round ended. A round that only waited on background work reported
/// nothing of its own, so it says what its task came to ("Done"), or Monitoring when the task's state isn't known.
pub(crate) fn round_state(round_status: Option<&str>, task_status: Option<&str>) -> (&'static str, &'static str) {
    match (round_status, task_status) {
        (_, Some(task)) if !is_finished(task) => task_state(task, false),
        (Some("monitoring"), Some(task)) => task_state(task, false),
        (Some("monitoring"), None) => ("monitoring", "Monitoring"),
        (Some("failed"), _) => ("failed", "Failed"),
        (Some("stopped"), _) => ("canceled", "Stopped"),
        _ => ("completed", "Done"),
    }
}

/// What a round's own page says: as its card (`round_state`), but a round that only waited on background work says
/// Monitoring only while it still waits (it's the task's latest round and the task monitors); once the task moved on
/// (it carried on in a later round, or finished), "Waited", with no mark.
pub(crate) fn round_page_state(
    round_status: &str,
    task_status: Option<&str>,
    latest: bool,
) -> (&'static str, &'static str) {
    match round_status {
        "monitoring" if latest && task_status == Some("monitoring") => ("monitoring", "Monitoring"),
        "monitoring" => ("waited", "Waited"),
        _ => round_state(Some(round_status), task_status),
    }
}

/// The chip on a tool's row: "Ready", or why not in a word or two.
fn tool_chip(tool: &ToolSummary, plugged: bool) -> (&'static str, Tone) {
    if !plugged {
        ("Unplugged", Tone::Neutral)
    } else if tool.ready {
        ("Ready", Tone::Success)
    } else if !tool.signed_in_on.is_empty() {
        ("Computer offline", Tone::Warning)
    } else if !tool.installed_on.is_empty() {
        ("Not signed in", Tone::Warning)
    } else {
        ("Not installed", Tone::Warning)
    }
}

/// When a plan window resets, as its meter says: "4:10 PM" within a day, "Mon" within the week, else "Oct 30".
fn resets_label(at: f64, now: f64) -> String {
    const DAY: f64 = 24. * 3_600_000.;
    if at - now < DAY {
        format::clock(at)
    } else if at - now < 7. * DAY {
        format::local(at).format("%a").to_string()
    } else {
        format::short_date(at)
    }
}

/// A plan window's pill on its tool's name line: "5h 96%", the reset added once it runs low ("5h 20% · 4:10 PM"), its
/// tooltip saying both ("5-hour limit: 20% left, resets 4:10 PM"). None once its reset has passed (its numbers are old
/// until the next ask).
#[derive(Debug, PartialEq)]
struct Meter {
    /// What's left, 0–100: how far its fill reaches.
    left: f64,
    text: String,
    tip: String,
    /// Neutral, Warning at 25% left or less, Danger at 10% or less.
    tone: Tone,
}

fn meter(name: &str, long_name: &str, window: &UsageWindow, now: f64) -> Option<Meter> {
    let left = (100. - window.used_percent).clamp(0., 100.).round();
    let resets = match window.resets_at {
        Some(at) if at <= now => return None,
        Some(at) => Some(resets_label(at, now)),
        None => None,
    };
    let tone = if left <= 10. {
        Tone::Danger
    } else if left <= 25. {
        Tone::Warning
    } else {
        Tone::Neutral
    };
    let text = match (&resets, tone) {
        (Some(resets), Tone::Warning | Tone::Danger) => format!("{name} {left}% · {resets}"),
        _ => format!("{name} {left}%"),
    };
    let tip = match &resets {
        Some(resets) => format!("{long_name}: {left}% left, resets {resets}"),
        None => format!("{long_name}: {left}% left"),
    };
    Some(Meter { left, text, tip, tone })
}

/// A usage pill's outline, its tint and text to be added (`CodingScreen::render_usage`).
fn usage_pill(id: SharedString) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .relative()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(space::XS))
        .min_h(px(18.))
        .px(px(space::SM - 2.))
        .rounded(px(radius::FULL))
        .border_1()
        .caption()
}

/// "~/Desktop/app" for "/Users/me/Desktop/app" (or /home/me/…), on whichever computer it is.
pub(crate) fn short_path(path: &str) -> String {
    for home in ["/Users/", "/home/"] {
        if let Some(rest) = path.strip_prefix(home) {
            return match rest.split_once('/') {
                Some((_, inside)) => format!("~/{inside}"),
                None => "~".into(),
            };
        }
    }
    path.to_string()
}

/// Whether a task with this status is over.
pub(crate) fn is_finished(status: &str) -> bool {
    matches!(status, "done" | "failed" | "stopped")
}

/// A finished task's state with how long it worked (`coding_handoff::worked_ms`), the same on its chat card and rows
/// and the Coding page: "Done · 18m". A task still at work, or one without its time, has its state alone.
pub(crate) fn with_worked(label: &str, status: &str, worked_ms: Option<f64>) -> String {
    match worked_ms.filter(|_| is_finished(status)) {
        Some(worked) => format!("{label} · {}", format::worked_short(worked)),
        None => label.to_string(),
    }
}

/// "41.3 GB", "820 MB".
fn disk_size(bytes: f64) -> String {
    let gb = bytes / 1_000_000_000.;
    if gb >= 100. {
        format!("{gb:.0} GB")
    } else if gb >= 1. {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes / 1_000_000.)
    }
}

fn plural(count: f64, one: &str, many: &str) -> String {
    if count == 1. { format!("1 {one}") } else { format!("{count} {many}") }
}

/// How a kind of proof reads.
fn proof_kind_label(kind: &str) -> &'static str {
    match kind {
        "image" => "Screenshot",
        "video" => "Recording",
        _ => "Output",
    }
}

/// An approval's answer as it reads ("allow_always" → "Allowed always"); a question's answer as given.
pub(crate) fn answer_label(kind: &str, answer: &str) -> String {
    match (kind, answer) {
        ("approval", "allow") => "Allowed".into(),
        ("approval", "allow_always") => "Allowed always".into(),
        ("approval", "deny") => "Denied".into(),
        _ => answer.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Replies (local_backend/coding.rs; numbers arrive as floats)

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    tools: Vec<ToolSummary>,
    /// Where new tasks work (`WORKSPACES`).
    #[serde(default = "default_workspace")]
    workspace: String,
    /// "Keep coding after Filo quits" (on by default): what quitting does to running tasks and turns.
    #[serde(default = "keep_by_default")]
    keep_on_quit: bool,
    /// "Keep Mac awake while tasks run" (off by default): no idle sleep while a coding task or a routine runs.
    #[serde(default)]
    keep_awake: bool,
    /// "Keep running with the lid closed (plugged in only)" (off by default): no sleep with the lid closed either, while
    /// tasks keep the Mac awake (local_backend/lid.rs).
    #[serde(default)]
    keep_awake_lid: bool,
    /// Access left installed, including after a cancelled removal or incomplete setup.
    #[serde(default)]
    keep_awake_lid_access: bool,
    /// What's wrong with lid mode now (a failed `pmset`, the 12-hour cap, a backstop that doesn't run), if anything.
    #[serde(default)]
    keep_awake_lid_problem: Option<String>,
    runners: Vec<RunnerRow>,
}

impl Overview {
    fn plugged(&self) -> Vec<String> {
        self.tools.iter().filter(|tool| tool.plugged).map(|tool| tool.id.clone()).collect()
    }
}

fn default_workspace() -> String {
    "worktree".into()
}

fn keep_by_default() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolSummary {
    id: String,
    plugged: bool,
    installed_on: Vec<String>,
    signed_in_on: Vec<String>,
    account: Option<String>,
    plan: Option<String>,
    version: Option<String>,
    ready: bool,
    /// Its default mode for new tasks (`MODES`).
    #[serde(default = "default_mode")]
    mode: String,
    /// Its default model and reasoning level for new tasks; none means the tool's own.
    model: Option<String>,
    effort: Option<String>,
    /// What it offers, as its computer reports it.
    #[serde(default)]
    models: Vec<ToolModel>,
}

/// `coding:usage`: what a tool's account says is left of its plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanUsage {
    five_hour: Option<UsageWindow>,
    week: Option<UsageWindow>,
}

/// `USAGE_SAVED`: the last answer, by tool.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedUsage {
    usage: Option<HashMap<String, PlanUsage>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageWindow {
    used_percent: f64,
    /// When it resets (ms).
    resets_at: Option<f64>,
}

/// `CodingModel`: a model a tool offers, with the reasoning levels it takes.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolModel {
    id: String,
    name: String,
    #[serde(default)]
    efforts: Vec<String>,
    default_effort: Option<String>,
    #[serde(default)]
    is_default: bool,
}

/// What the model picker offers: "Default" (the tool's own pick, named when it says which) first, then its models, as
/// (id, label); "" is the default's id.
fn model_choices(tool: &ToolSummary) -> Vec<(String, String)> {
    let default = tool
        .models
        .iter()
        .find(|item| item.is_default)
        .map(|item| format!("Default ({})", item.name))
        .unwrap_or_else(|| "Default".into());
    std::iter::once((String::new(), default))
        .chain(tool.models.iter().map(|item| (item.id.clone(), item.name.clone())))
        .collect()
}

/// The model picker's button, as a select field: it fills the width it's given, its name cut short when it doesn't
/// fit (whole in its tooltip), the caret at its end. Drawn as the outline buttons beside it are.
#[derive(IntoElement)]
struct ModelField {
    base: gpui::Stateful<gpui::Div>,
    label: SharedString,
    selected: bool,
}

impl ModelField {
    fn new(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>) -> Self {
        ModelField { base: div().id(id.into()), label: label.into(), selected: false }
    }
}

impl gpui_component::Selectable for ModelField {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl gpui::Styled for ModelField {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.base.style()
    }
}

impl gpui::InteractiveElement for ModelField {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

impl gpui_component::menu::DropdownMenu for ModelField {}

impl gpui::RenderOnce for ModelField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = gpui_component::ActiveTheme::theme(cx);
        self.base
            .h_6()
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(if self.selected { theme.secondary_active } else { theme.background })
            .when(theme.shadow, |this| this.shadow_xs())
            .text_sm()
            .text_color(theme.secondary_foreground)
            .when(!self.selected, |this| this.hover(|this| this.bg(theme.secondary_hover)))
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
            // The whole name when it's cut short.
            .when(!self.selected, |this| {
                let label = self.label.clone();
                this.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
            })
            .child(div().flex_1().min_w_0().truncate().line_height(gpui::relative(1.)).child(self.label))
            .child(Icon::new(IconName::ChevronDown).small())
    }
}

fn default_mode() -> String {
    "auto".into()
}

/// The modes and their names: how freely a tool works on new tasks.
const MODES: [(&str, &str); 2] = [("auto", "Auto"), ("full_access", "Full access")];

/// "Own worktree" first: the welcome's "Give each task its own branch" is it on, "Your checkout" off.
const WORKSPACES: [(&str, &str, &str); 2] = [
    (
        "worktree",
        "Own worktree",
        "Each task in a git project gets its own branch and folder (a git worktree); your checkout stays as it is.",
    ),
    ("checkout", "Your checkout", "Every task works in the project folder as it is, side by side with you."),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunnerRow {
    name: String,
    repo_count: f64,
    free_disk_bytes: Option<f64>,
    tools: Vec<ToolState>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolState {
    id: String,
    installed: bool,
    signed_in: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    name: String,
    path: String,
    branch: Option<String>,
    /// Still there (a folder can move or go after it was added).
    online: bool,
    /// No git history: tasks work in it as it is.
    #[serde(default)]
    plain_folder: bool,
    /// The user added it, so it can be taken off the list.
    #[serde(default)]
    added: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskRow {
    task_id: String,
    title: String,
    tool: String,
    status: String,
    owner: String,
    repo_name: String,
    pending: f64,
    updated_at: f64,
    /// How long it has worked, in milliseconds (`coding_handoff::worked_ms`).
    worked_ms: Option<f64>,
}

/// A screenshot, recording or output file a tool sent back with a round's report.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Proof {
    proof_id: String,
    /// "image", "video" or "file".
    pub kind: String,
    pub name: String,
    pub url: String,
    content_type: String,
    size: f64,
}

impl Proof {
    pub(crate) fn preview_item(&self) -> file_preview::Item {
        file_preview::Item {
            url: self.url.clone(),
            name: self.name.clone(),
            content_type: self.content_type.clone(),
            file: self.local_file(),
        }
    }

    /// The file on this Mac when the proof is one (a coding task handed off from a chat saves its proof here, and its
    /// `url` is the file's path), not an address.
    pub(crate) fn local_file(&self) -> Option<std::path::PathBuf> {
        let path = self.url.strip_prefix("file://").unwrap_or(&self.url);
        path.starts_with('/').then(|| std::path::PathBuf::from(path))
    }

    /// Where its picture loads from: its file on this Mac, else its URL.
    pub(crate) fn picture(&self) -> gpui::Resource {
        match self.local_file() {
            Some(path) => gpui::Resource::Path(path.into()),
            None => gpui::Resource::Uri(gpui::SharedUri::from(self.url.clone())),
        }
    }
}

/// Runs a Coding mutation; a failure shows as a toast.
fn mutate<V: 'static>(
    window: &Window,
    cx: &mut Context<V>,
    path: &str,
    args: Value,
    failed: &'static str,
    done: impl FnOnce(&mut V, bool, &mut Context<V>) + 'static,
) {
    backend::call_in(window, cx, path, args, move |view, result: Result<Value, String>, window, cx| {
        if let Err(error) = &result {
            ui::toast_error(window, cx, failed, error.clone());
        }
        done(view, result.is_ok(), cx);
    });
}

pub(crate) fn mono(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div().font_family(theme.mono_family()).text_size(px(theme.mono_size() - 1.)).child(text.into())
}

/// A command to type in Terminal, drawn as code.
fn command_pill(command: &str, cx: &App) -> gpui::Div {
    let colors = cx.colors();
    mono(command.to_string(), cx)
        .px(px(space::SM))
        .py(px(2.))
        .rounded(px(radius::XS))
        .bg(colors.surface_strong)
        .text_color(colors.text)
}

/// A coding tool's picture: its maker's mark on its own, no tile (36 in lists, 28
/// beside a message, 20 in a badge), Claude's in its orange for Claude Code, OpenAI's for Codex, Meta's in its blue for
/// Muse Code and OpenCode's for OpenCode.
pub(crate) fn tool_tile(tool: &str, size: f32, cx: &App) -> gpui::Div {
    use crate::screens::chat::conversation::{engine_color, engine_logo};
    let engine = filo_local::coding::engine_of(tool).unwrap_or(filo_local::Engine::Claude);
    let (mark, color) = (engine_logo(engine), engine_color(engine, cx));
    div().size(px(size)).flex_none().flex().items_center().justify_center().child(ui::icon_sized(
        mark,
        (size * 0.8).round(),
        color,
    ))
}

/// A coding task's row leads with its task; the tool's name only when the task has no title yet.
pub(crate) fn task_heading(title: &str, tool: &str) -> String {
    if title.trim().is_empty() { tool_name(tool) } else { title.to_string() }
}

/// `tool_tile` on a row that doesn't write the tool's name (a coding task's row leads with the task): the mark alone
/// says which tool, and hovering it names it ("Claude Code", "Codex").
pub(crate) fn named_tool_tile(
    id: impl Into<SharedString>,
    tool: &str,
    size: f32,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let name = SharedString::from(tool_name(tool));
    tool_tile(tool, size, cx)
        .id(id.into())
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name.clone()).build(window, cx))
}

fn icon_tile(icon: &str, cx: &App) -> gpui::Div {
    let colors = cx.colors();
    div()
        .size(px(36.))
        .flex_none()
        .rounded(px(10.))
        .bg(colors.surface_strong)
        .flex()
        .items_center()
        .justify_center()
        .child(ui::icon_sized(icon, 18., colors.text))
}

fn note(text: impl Into<SharedString>, cx: &App) -> gpui::Div {
    div().px(px(space::LG)).py(px(space::LG)).subhead().text_color(cx.colors().text_secondary).child(text.into())
}

/// A screenshot as a thumbnail with its name under it, on `behind`; it opens larger in the app's viewer.
pub(crate) fn proof_thumbnail(proof: &Proof, behind: gpui::Hsla, cx: &App) -> AnyElement {
    const WIDTH: f32 = 168.;
    let colors = cx.colors();
    let item = proof.preview_item();
    div()
        .id(SharedString::from(format!("coding-proof-{}", proof.proof_id)))
        .w(px(WIDTH))
        .flex()
        .flex_col()
        .gap(px(space::XXS))
        .cursor_pointer()
        .child(
            div()
                .relative()
                .w(px(WIDTH))
                .h(px(WIDTH * 2. / 3.))
                .rounded(px(radius::SM))
                .bg(colors.surface_strong)
                .hover(|style| style.opacity(0.85))
                .child(ui::rounded_cover(proof.url.clone(), radius::SM, behind)),
        )
        .child(div().caption().text_color(colors.text_secondary).truncate().child(proof.name.clone()))
        .on_click(move |_, window, cx| file_preview::open(item.clone(), window, cx))
        .into_any_element()
}

/// A recording or an output file: name, kind and size; it opens in the app's viewer (file_preview::open), or in its
/// own app when the viewer can't show it.
pub(crate) fn proof_row(proof: &Proof, cx: &App) -> AnyElement {
    let colors = cx.colors();
    let item = proof.preview_item();
    let row = div()
        .id(SharedString::from(format!("coding-proof-{}", proof.proof_id)))
        .flex()
        .items_center()
        .gap(px(space::MD))
        .cursor_pointer()
        .child(icon_tile(if proof.kind == "video" { "video" } else { "file-text" }, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().callout().medium().truncate().child(proof.name.clone()))
                .child(div().footnote().text_color(colors.text_secondary).child(format!(
                    "{} · {}",
                    proof_kind_label(&proof.kind),
                    format::file_size(proof.size)
                ))),
        )
        .child(ui::icon_sized("eye", 16., colors.text_tertiary))
        .on_click(move |_, window, cx| file_preview::open(item.clone(), window, cx));
    crate::smoke::tracked(format!("coding-file-{}", proof.name), row)
}

// ---------------------------------------------------------------------------------------------------------------
// The page

pub struct CodingScreen {
    overview: Option<Result<Option<Overview>, String>>,
    tasks: Option<Result<Vec<TaskRow>, String>>,
    projects: Option<Result<Vec<Project>, String>>,
    search: Entity<InputState>,
    query: String,
    /// Switches flipped and waiting for the backend, by tool or runner id.
    plugging: HashMap<String, bool>,
    /// A mode change on its way, by tool.
    moding: HashMap<String, String>,
    /// A model or reasoning level change on its way, by tool ("" for the tool's own).
    modeling: HashMap<String, String>,
    efforting: HashMap<String, String>,
    /// A workspace change on its way.
    workspacing: Option<String>,
    /// A "Keep coding after Filo quits" change on its way.
    keeping: Option<bool>,
    /// A "Keep Mac awake while tasks run" change on its way.
    waking: Option<bool>,
    /// A "Keep running with the lid closed" change on its way (it may wait on the password prompt).
    lidding: Option<bool>,
    /// Every finished task and every project, not just the first few.
    all_tasks: bool,
    all_projects: bool,
    /// What's left of each tool's plan, by tool (`coding:usage`), asked again every `USAGE_EVERY`: the last answer,
    /// saved (`USAGE_SAVED`), None before the first.
    usage: Option<HashMap<String, PlanUsage>>,
    /// The page opened and its ask hasn't come back: the pills shown are dimmed, with a spinner after them.
    usage_loading: bool,
    /// When the last answer came, this run.
    usage_at: Option<std::time::Instant>,
    _usage: Task<()>,
    _overview_sub: Task<()>,
    _tasks_sub: Task<()>,
    _projects_sub: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl CodingScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let overview_sub = backend::subscribe(
            cx,
            "coding:overview",
            json!({}),
            |this: &mut Self, result: Result<Option<Overview>, String>, cx| {
                this.overview = Some(result);
                cx.notify();
            },
        );
        let tasks_sub =
            backend::subscribe(cx, "coding:tasks", json!({ "limit": 30 }), |this: &mut Self, result, cx| {
                this.tasks = Some(result);
                cx.notify();
            });
        let usage = cx.spawn(async move |this, cx| {
            while this.update(cx, |this, cx| this.ask_usage(cx)).is_ok() {
                cx.background_executor().timer(USAGE_EVERY).await;
            }
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects"));
        let subscriptions = vec![cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let query = state.read(cx).value().trim().to_string();
                if query != this.query {
                    this.query = query;
                    this.subscribe_projects(cx);
                }
            }
        })];
        let mut this = CodingScreen {
            overview: None,
            tasks: None,
            projects: None,
            search,
            query: String::new(),
            plugging: HashMap::new(),
            moding: HashMap::new(),
            modeling: HashMap::new(),
            efforting: HashMap::new(),
            workspacing: None,
            keeping: None,
            waking: None,
            lidding: None,
            all_tasks: false,
            all_projects: false,
            usage: config::load_settings::<SavedUsage>(USAGE_SAVED).usage,
            usage_loading: true,
            usage_at: None,
            _usage: usage,
            _overview_sub: overview_sub,
            _tasks_sub: tasks_sub,
            _projects_sub: None,
            _subscriptions: subscriptions,
        };
        this.subscribe_projects(cx);
        this
    }

    /// Every task and project shown, as "Show N more" does (`pages-bottom-drive`): whether either had more to show.
    #[cfg(feature = "dev")]
    pub fn drive_show_all(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self
            .tasks
            .as_ref()
            .and_then(|tasks| tasks.as_ref().ok())
            .map_or(0, |tasks| tasks.iter().filter(|task| is_finished(&task.status)).count());
        let projects = self.projects.as_ref().and_then(|projects| projects.as_ref().ok()).map_or(0, Vec::len);
        self.all_tasks = true;
        self.all_projects = true;
        cx.notify();
        finished > FINISHED_SHOWN || projects > PROJECTS_PREVIEW
    }

    /// Asks how much of each tool's plan is left as the page opens again (app.rs `select_tab`), the last numbers
    /// dimmed meanwhile unless the backend still keeps them.
    pub(crate) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        if self.usage_at.is_none_or(|at| at.elapsed() >= USAGE_KEPT) {
            self.usage_loading = true;
            cx.notify();
        }
        self.ask_usage(cx);
    }

    /// Asks how much of each tool's plan is left: when the page is made, as it opens again and every `USAGE_EVERY`.
    /// A new answer is saved for the next start.
    fn ask_usage(&mut self, cx: &mut Context<Self>) {
        backend::call(
            cx,
            "coding:usage",
            json!({}),
            |this: &mut Self, result: Result<HashMap<String, PlanUsage>, String>, cx| {
                this.usage_loading = false;
                if let Ok(usage) = result {
                    this.usage_at = Some(std::time::Instant::now());
                    if this.usage.as_ref() != Some(&usage) {
                        if let Err(error) =
                            config::save_settings(USAGE_SAVED, &SavedUsage { usage: Some(usage.clone()) })
                        {
                            eprintln!("plan usage: {error}");
                        }
                        this.usage = Some(usage);
                    }
                }
                cx.notify();
            },
        );
    }

    /// A tool's 5-hour and weekly pills, when its account reports them: bordered, the fill behind the text as wide as
    /// what's left, tinted warning or danger when it runs low. While the page's ask is on its way, the last numbers
    /// dimmed with a spinner after them, and a shimmering placeholder of a pill's size for a window with none to show
    /// (nothing saved yet, or its reset has passed).
    fn render_usage(&self, tool: &str, cx: &App) -> Vec<AnyElement> {
        let colors = *cx.colors();
        let loading = self.usage_loading;
        let windows = match &self.usage {
            Some(usage) => match usage.get(tool) {
                Some(usage) => [usage.five_hour.as_ref().map(Some), usage.week.as_ref().map(Some)],
                None => return Vec::new(),
            },
            None if loading && USAGE_TOOLS.contains(&tool) => [Some(None), Some(None)],
            None => return Vec::new(),
        };
        let now = format::now_ms();
        let mut dimmed = false;
        let mut pills: Vec<AnyElement> = [("5h", "5-hour limit"), ("Week", "Weekly limit")]
            .into_iter()
            .zip(windows)
            .filter_map(|((name, long_name), window)| {
                let id = SharedString::from(format!("coding-usage-{tool}-{name}"));
                let Some(meter) = window?.and_then(|window| meter(name, long_name, window, now)) else {
                    // Its text, unseen, sizes it as a pill like "5h 74%".
                    return loading.then(|| {
                        usage_pill(id)
                            .border_color(gpui::transparent_black())
                            // Over its unseen border too: as wide and tall as a pill.
                            .child(
                                div()
                                    .absolute()
                                    .inset(px(-1.))
                                    .child(ui::skeleton(gpui::relative(1.), 18., radius::FULL).h_full()),
                            )
                            .child(div().text_color(gpui::transparent_black()).child(name))
                            .child(div().text_color(gpui::transparent_black()).child("00%"))
                            .into_any_element()
                    });
                };
                dimmed |= loading;
                let (fill, border) = match meter.tone {
                    Tone::Danger => (colors.danger.opacity(0.18), colors.danger.opacity(0.45)),
                    Tone::Warning => (colors.warning.opacity(0.18), colors.warning.opacity(0.45)),
                    _ => (colors.text.opacity(0.07), colors.border),
                };
                let (label, value) = meter.text.split_once(' ').unwrap_or((meter.text.as_str(), ""));
                let tip = SharedString::from(meter.tip);
                Some(
                    usage_pill(id)
                        .border_color(border)
                        .when(loading, |this| this.opacity(0.5))
                        // The tint is the pill's own rounded shape, clipped square on the right as wide as what's left:
                        // gpui clips to rectangles only, so a square-cornered fill would poke out of the rounded ends.
                        .when(meter.left > 0., |this| {
                            let share = meter.left as f32 / 100.;
                            this.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left_0()
                                    .w(gpui::relative(share))
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left_0()
                                            .w(gpui::relative(1. / share))
                                            .rounded(px(radius::FULL))
                                            .bg(fill),
                                    ),
                            )
                        })
                        .child(div().relative().text_color(colors.text_secondary).child(label.to_string()))
                        .child(div().relative().text_color(colors.text).child(value.to_string()))
                        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                        .into_any_element(),
                )
            })
            .collect();
        if dimmed {
            pills.push(ui::spinner(12., cx).into_any_element());
        }
        pills
    }

    fn subscribe_projects(&mut self, cx: &mut Context<Self>) {
        let args = if self.query.is_empty() { json!({}) } else { json!({ "search": self.query }) };
        self._projects_sub = Some(backend::subscribe(cx, "coding:projects", args, |this: &mut Self, result, cx| {
            this.projects = Some(result);
            cx.notify();
        }));
    }

    /// Adds a folder the user picks (git or not) to the projects.
    fn add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = paths.await.ok().and_then(Result::ok).flatten().and_then(|paths| paths.into_iter().next())
            else {
                return;
            };
            this.update_in(cx, |_, window, cx| {
                let args = json!({ "path": path.display().to_string() });
                mutate(window, cx, "coding:addProject", args, "Couldn't add the folder", |_: &mut Self, _, _| {});
            })
            .ok();
        })
        .detach();
    }

    fn set_mode(&mut self, tool: String, mode: String, window: &mut Window, cx: &mut Context<Self>) {
        self.moding.insert(tool.clone(), mode.clone());
        cx.notify();
        mutate(
            window,
            cx,
            "coding:setToolMode",
            json!({ "tool": tool, "mode": mode }),
            "Couldn't change the mode",
            move |this: &mut Self, _, cx| {
                this.moding.remove(&tool);
                cx.notify();
            },
        );
    }

    fn set_model(&mut self, tool: String, model: String, window: &mut Window, cx: &mut Context<Self>) {
        self.modeling.insert(tool.clone(), model.clone());
        cx.notify();
        mutate(
            window,
            cx,
            "coding:setToolModel",
            json!({ "tool": tool, "model": model }),
            "Couldn't change the model",
            move |this: &mut Self, _, cx| {
                this.modeling.remove(&tool);
                cx.notify();
            },
        );
    }

    fn set_effort(&mut self, tool: String, effort: String, window: &mut Window, cx: &mut Context<Self>) {
        self.efforting.insert(tool.clone(), effort.clone());
        cx.notify();
        mutate(
            window,
            cx,
            "coding:setToolEffort",
            json!({ "tool": tool, "effort": effort }),
            "Couldn't change the reasoning level",
            move |this: &mut Self, _, cx| {
                this.efforting.remove(&tool);
                cx.notify();
            },
        );
    }

    fn set_workspace(&mut self, workspace: String, window: &mut Window, cx: &mut Context<Self>) {
        self.workspacing = Some(workspace.clone());
        cx.notify();
        mutate(
            window,
            cx,
            "coding:setWorkspace",
            json!({ "workspace": workspace }),
            "Couldn't change where tasks work",
            move |this: &mut Self, _, cx| {
                this.workspacing = None;
                cx.notify();
            },
        );
    }

    fn set_keep_on_quit(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.keeping = Some(keep);
        cx.notify();
        mutate(
            window,
            cx,
            "coding:setKeepOnQuit",
            json!({ "keep": keep }),
            "Couldn't change what quitting does",
            move |this: &mut Self, _, cx| {
                this.keeping = None;
                cx.notify();
            },
        );
    }

    /// A setting as a row with its switch, as this page draws them: `on` as saved or on its way (`busy`: the switch waits
    /// for the backend), a spinner while it isn't known yet, and `toggle` given the new value. This page's rows, and the
    /// welcome's "How it runs" (titles only).
    fn switch_row(
        id: &str,
        title: &str,
        subtitle: Option<&str>,
        on: Option<bool>,
        busy: bool,
        toggle: fn(&mut Self, bool, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let trailing = match on {
            Some(on) => crate::smoke::tracked(
                format!("{id}-switch"),
                Switch::new(SharedString::from(format!("{id}-switch")))
                    .checked(on)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, checked: &bool, window, cx| toggle(this, *checked, window, cx))),
            ),
            None => ui::spinner(16., cx).into_any_element(),
        };
        ui::ListRow::new(SharedString::from(id.to_string()), title.to_string())
            .subtitle_opt(subtitle.map(str::to_string))
            .trailing(trailing)
            .into_any_element()
    }

    /// The settings as loaded, while they are.
    fn loaded(&self) -> Option<&Overview> {
        self.overview.as_ref()?.as_ref().ok()?.as_ref()
    }

    /// "Keep coding after Filo quits" as a row titled `title` (the welcome's words), nothing under it: this page's switch,
    /// state and call (a spinner until the settings have loaded: they come with the tools', which takes a few seconds the
    /// first time).
    pub(crate) fn keep_on_quit_switch(&self, id: &str, title: &str, cx: &mut Context<Self>) -> AnyElement {
        let on = self.keeping.or_else(|| self.loaded().map(|overview| overview.keep_on_quit));
        Self::switch_row(id, title, None, on, self.keeping.is_some(), Self::set_keep_on_quit, cx)
    }

    /// Where tasks work, as a switch titled `title` (the welcome's words), nothing under it: on is "Own worktree", off
    /// "Your checkout", with this page's state and call (a spinner until the settings have loaded).
    pub(crate) fn own_branch_switch(&self, id: &str, title: &str, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspacing.clone().or_else(|| self.loaded().map(|overview| overview.workspace.clone()));
        let own = |this: &mut Self, on: bool, window: &mut Window, cx: &mut Context<Self>| {
            this.set_workspace(if on { WORKSPACES[0].0 } else { WORKSPACES[1].0 }.to_string(), window, cx)
        };
        Self::switch_row(
            id,
            title,
            None,
            workspace.map(|workspace| workspace == WORKSPACES[0].0),
            self.workspacing.is_some(),
            own,
            cx,
        )
    }

    /// "Keep coding after Filo quits": what quitting does to the coding tasks and conversation turns running on this Mac.
    fn render_keep_on_quit(&self, overview: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let on = self.keeping.unwrap_or(overview.keep_on_quit);
        let subtitle = if on {
            "Quitting Filo leaves this Mac's coding tasks and chat replies running while the Mac is awake; Filo picks them up when it opens again."
        } else {
            "Quitting Filo stops this Mac's coding tasks and chat replies, as if you pressed Stop on each. Their branches and changes stay, and they don't start again on their own."
        };
        Self::switch_row(
            "coding-keep-on-quit",
            "Keep coding after Filo quits",
            Some(subtitle),
            Some(on),
            self.keeping.is_some(),
            Self::set_keep_on_quit,
            cx,
        )
    }

    fn set_keep_awake(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.waking = Some(keep);
        cx.notify();
        mutate(
            window,
            cx,
            "coding:setKeepAwake",
            json!({ "keep": keep }),
            "Couldn't change whether the Mac stays awake",
            move |this: &mut Self, _, cx| {
                this.waking = None;
                cx.notify();
            },
        );
    }

    /// "Keep Mac awake while tasks run": no idle sleep while this Mac's coding tasks and routines run.
    fn render_keep_awake(&self, overview: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let on = self.waking.unwrap_or(overview.keep_awake);
        let subtitle = if on {
            "This Mac doesn't go to sleep on its own while a coding task, a routine or a chat reply runs, and can again once none is running. The display can still sleep, and closing the lid still sleeps the Mac unless the option below is on."
        } else {
            "This Mac sleeps when it's idle, as usual, even while a coding task, a routine or a chat reply runs; the work pauses until it wakes."
        };
        Self::switch_row(
            "coding-keep-awake",
            "Keep Mac awake while tasks run",
            Some(subtitle),
            Some(on),
            self.waking.is_some(),
            Self::set_keep_awake,
            cx,
        )
    }

    fn set_keep_awake_lid(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.lidding = Some(keep);
        cx.notify();
        let failed = if keep {
            "Couldn't keep tasks running with the lid closed"
        } else {
            "Couldn't fully turn off keeping tasks running with the lid closed"
        };
        mutate(window, cx, "coding:setKeepAwakeLid", json!({ "keep": keep }), failed, move |this: &mut Self, _, cx| {
            this.lidding = None;
            cx.notify();
        });
    }

    /// Lid mode needs keep-awake to operate, but can always be switched off and its access removed.
    fn render_keep_awake_lid(&self, overview: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let awake = self.waking.unwrap_or(overview.keep_awake);
        let on = self.lidding.unwrap_or(overview.keep_awake_lid);
        let problem = overview
            .keep_awake_lid_problem
            .clone()
            .filter(|_| self.lidding.is_none() && (on || overview.keep_awake_lid_access));
        let subtitle: SharedString = match problem {
            Some(problem) => problem.into(),
            None => match (awake, on, self.lidding) {
                (_, _, Some(false)) if overview.keep_awake_lid_access => "Turning off and removing access; macOS may ask for your password.",
                (_, _, Some(false)) => "Turning off…",
                (_, _, Some(true)) => "Waiting for your password: macOS asks once, to let Filo switch sleep off and back on.",
                (false, true, _) => "Paused while “Keep Mac awake while tasks run” is off. Turn this off to remove its installed access.",
                (_, false, _) if overview.keep_awake_lid_access => "Off, but access is still installed. Remove access to finish turning this off; macOS asks for your password.",
                (false, false, _) => "Needs “Keep Mac awake while tasks run” on.",
                (_, true, _) => "While tasks run and the Mac is plugged in, closing the lid doesn't sleep it. Sleep comes back on when they end, on battery, after 12 hours, or if the Mac gets too hot.",
                (_, false, _) => "Closing the lid sleeps the Mac and pauses tasks. Turn on to keep them going while it's plugged in; macOS asks for your password once.",
            }
            .into(),
        };
        ui::ListRow::new("coding-keep-awake-lid", "Keep running with the lid closed (plugged in only)")
            .subtitle(subtitle)
            .trailing(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .when(!on && overview.keep_awake_lid_access, |row| {
                        row.child(
                            Button::secondary("coding-remove-lid-access", "Remove access")
                                .small()
                                .disabled(self.lidding.is_some())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.set_keep_awake_lid(false, window, cx)),
                                ),
                        )
                    })
                    .child(
                        Switch::new("coding-keep-awake-lid-switch")
                            .checked(on)
                            .disabled((!awake && !on) || self.lidding.is_some())
                            .on_click(cx.listener(|this, checked: &bool, window, cx| {
                                this.set_keep_awake_lid(*checked, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Where new tasks work: a select-style button, and what the choice means beside it.
    fn render_workspace(&self, overview: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let workspace = self.workspacing.clone().unwrap_or_else(|| overview.workspace.clone());
        let (_, label, description) =
            WORKSPACES.iter().find(|(value, _, _)| *value == workspace).copied().unwrap_or(WORKSPACES[0]);
        let entity = cx.entity().downgrade();
        let picker = MenuButton::new("coding-workspace")
            .outline()
            .small()
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                WORKSPACES.iter().fold(menu, |menu, (value, label, _)| {
                    let (entity, value) = (entity.clone(), value.to_string());
                    menu.item(PopupMenuItem::new(*label).checked(value == workspace).on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.set_workspace(value.clone(), window, cx)).ok();
                    }))
                })
            });
        div()
            .px(px(space::LG))
            .py(px(space::MD))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(space::SM))
            .child(picker)
            .child(div().min_w_0().footnote().text_color(colors.text_secondary).child(description))
            .into_any_element()
    }

    /// How the tool works on new tasks, in one line: its model and reasoning level (what it offers, "Default" being its
    /// own pick) and its mode. The model's field takes the free space and cuts a long name short, so the line never
    /// wraps and the reasoning and mode pickers stay put whichever model is picked.
    fn render_pickers(&self, tool: &ToolSummary, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let label = |text: &'static str| div().flex_none().footnote().text_color(colors.text_secondary).child(text);
        let group = |text: &'static str, picker: AnyElement| {
            let id = format!("coding-{}-{}", text.to_lowercase(), tool.id);
            div()
                .flex_none()
                .child(crate::smoke::tracked(
                    id,
                    div().flex().items_center().gap(px(space::SM)).child(label(text)).child(picker),
                ))
                .into_any_element()
        };
        let (model_picker, effort_picker) = self.render_model_pickers(tool, cx);
        let mode = self.moding.get(&tool.id).cloned().unwrap_or_else(|| tool.mode.clone());
        let (_, mode_label) = MODES.iter().find(|(value, _)| *value == mode).copied().unwrap_or(MODES[0]);
        let entity = cx.entity().downgrade();
        let (tool_id, selected) = (tool.id.clone(), mode.clone());
        let mode_picker = MenuButton::new(SharedString::from(format!("coding-mode-{}", tool.id)))
            .outline()
            .small()
            .label(mode_label)
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                MODES.iter().fold(menu, |menu, (value, label)| {
                    let (entity, tool_id, value) = (entity.clone(), tool_id.clone(), value.to_string());
                    menu.item(PopupMenuItem::new(*label).checked(value == selected).on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.set_mode(tool_id.clone(), value.clone(), window, cx)).ok();
                    }))
                })
            });
        div()
            .pt(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .map(|row| match model_picker {
                Some(picker) => row.child(
                    div().flex_1().min_w_0().flex().items_center().gap(px(space::SM)).child(label("Model")).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(crate::smoke::tracked(format!("coding-model-{}", tool.id), picker)),
                    ),
                ),
                // Nothing to pick a model from: the mode on its own, where it starts.
                None => row,
            })
            .children(effort_picker.map(|picker| group("Reasoning", picker)))
            .child(group("Mode", mode_picker.into_any_element()))
            .into_any_element()
    }

    /// The model and reasoning pickers, when the tool's computer said what it offers. A long list (OpenCode knows
    /// hundreds) is searched rather than scrolled through.
    fn render_model_pickers(
        &self,
        tool: &ToolSummary,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, Option<AnyElement>) {
        if tool.models.is_empty() {
            return (None, None);
        }
        let model =
            self.modeling.get(&tool.id).cloned().or_else(|| tool.model.clone()).filter(|model| !model.is_empty());
        let effort =
            self.efforting.get(&tool.id).cloned().or_else(|| tool.effort.clone()).filter(|effort| !effort.is_empty());
        let fallback = tool.models.iter().find(|item| item.is_default);
        let chosen = match &model {
            Some(id) => tool.models.iter().find(|item| &item.id == id),
            None => fallback,
        };
        let choices = model_choices(tool);
        let model_label = match (&model, chosen) {
            (Some(_), Some(item)) => item.name.clone(),
            (Some(id), None) => id.clone(),
            (None, _) => choices[0].1.clone(),
        };
        let default_effort = chosen
            .and_then(|item| item.default_effort.as_deref())
            .map(|level| format!("Default ({})", effort_label(level)))
            .unwrap_or_else(|| "Default".into());
        let efforts = chosen.map(|item| item.efforts.clone()).unwrap_or_default();
        let entity = cx.entity().downgrade();
        let field = ModelField::new(SharedString::from(format!("coding-model-{}", tool.id)), model_label).w_full();
        let selected = model.clone().unwrap_or_default();
        let model_picker = if choices.len() > MODELS_IN_MENU {
            self.render_model_search(tool, field, choices, selected, cx)
        } else {
            let (entity, tool_id) = (entity.clone(), tool.id.clone());
            field
                .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                    choices.iter().fold(menu, |menu, (value, label)| {
                        let (entity, tool_id, value) = (entity.clone(), tool_id.clone(), value.clone());
                        menu.item(PopupMenuItem::new(label.clone()).checked(value == selected).on_click(
                            move |_, window, cx| {
                                entity
                                    .update(cx, |this, cx| this.set_model(tool_id.clone(), value.clone(), window, cx))
                                    .ok();
                            },
                        ))
                    })
                })
                .into_any_element()
        };
        let effort_picker = (!efforts.is_empty()).then(|| {
            let (tool_id, selected) = (tool.id.clone(), effort.clone());
            MenuButton::new(SharedString::from(format!("coding-effort-{}", tool.id)))
                .outline()
                .small()
                .label(effort.as_deref().map(effort_label).unwrap_or_else(|| default_effort.clone()))
                .dropdown_caret(true)
                .dropdown_menu_with_anchor(Corner::TopLeft, move |menu, _, _| {
                    let choices = std::iter::once((String::new(), default_effort.clone()))
                        .chain(efforts.iter().map(|level| (level.clone(), effort_label(level))));
                    choices.fold(menu, |menu, (value, label)| {
                        let checked = selected.as_deref().unwrap_or_default() == value;
                        let (entity, tool_id) = (entity.clone(), tool_id.clone());
                        menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, window, cx| {
                            entity
                                .update(cx, |this, cx| this.set_effort(tool_id.clone(), value.clone(), window, cx))
                                .ok();
                        }))
                    })
                })
                .into_any_element()
        });
        (Some(model_picker), effort_picker)
    }

    /// The model picker for a long list: searched by name or id (search_menu.rs), the one picked ticked.
    fn render_model_search(
        &self,
        tool: &ToolSummary,
        field: ModelField,
        choices: Vec<(String, String)>,
        selected: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        let tool_id = tool.id.clone();
        let rows: std::rc::Rc<Vec<search_menu::Choice>> = std::rc::Rc::new(
            choices
                .iter()
                .map(|(value, label)| search_menu::Choice {
                    checked: *value == selected,
                    also: value.clone(),
                    ..search_menu::Choice::new(label.clone())
                })
                .collect(),
        );
        let id = format!("coding-model-search-{}", tool.id);
        search_menu::search_menu(
            id,
            field,
            Corner::TopLeft,
            MODEL_LIST_WIDTH,
            "Search models",
            move || rows.clone(),
            move |index, window, cx| {
                let value = choices[index].0.clone();
                entity.update(cx, |this, cx| this.set_model(tool_id.clone(), value, window, cx)).ok();
            },
        )
        .into_any_element()
    }

    fn set_plugged(&mut self, tool: String, plugged: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.plugging.contains_key(&tool) {
            return;
        }
        self.plugging.insert(tool.clone(), plugged);
        cx.notify();
        let failed = if plugged { "Couldn't plug it in" } else { "Couldn't unplug it" };
        mutate(
            window,
            cx,
            "coding:setPlugged",
            json!({ "tool": tool, "plugged": plugged }),
            failed,
            move |this: &mut Self, _, cx| {
                this.plugging.remove(&tool);
                cx.notify();
            },
        );
    }

    fn render_tool(&self, tool: &ToolSummary, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let engine = filo_local::coding::engine_of(&tool.id);
        let name = engine.map_or(tool.id.as_str(), |engine| engine.label());
        let sign_in = engine.map(|engine| engine.sign_in_command()).unwrap_or_default();
        let plugged = self.plugging.get(&tool.id).copied().unwrap_or(tool.plugged);
        let (chip, tone) = tool_chip(tool, plugged);
        let mut lines: Vec<AnyElement> = Vec::new();
        let line = |text: String| div().footnote().text_color(colors.text_secondary).child(text).into_any_element();
        if !tool.signed_in_on.is_empty() {
            let mut parts = Vec::new();
            parts.extend(tool.plan.clone());
            parts.extend(tool.version.as_ref().map(|version| {
                if version.starts_with(|c: char| c.is_ascii_digit()) { format!("v{version}") } else { version.clone() }
            }));
            let rest = parts.iter().map(|part| format!("\u{a0}· {part}")).collect::<String>();
            lines.push(match &tool.account {
                // The email stays blurred until clicked.
                Some(account) => div()
                    .flex()
                    .items_center()
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child("Signed in as\u{a0}")
                    .child(ui::PrivateText::new(
                        SharedString::from(format!("coding-account-{}", tool.id)),
                        account.clone(),
                    ))
                    .child(rest)
                    .into_any_element(),
                None => line(format!("Signed in{rest}")),
            });
        } else if !tool.installed_on.is_empty() {
            lines.push(line(format!("Installed on {}, not signed in.", tool.installed_on.join(", "))));
        } else {
            lines.push(line("Not installed on your computers.".into()));
        }
        if tool.signed_in_on.is_empty() && !tool.installed_on.is_empty() && !sign_in.is_empty() {
            lines.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(space::XS))
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child("To sign in, run")
                    .child(command_pill(&sign_in, cx))
                    .child("in Terminal.")
                    .into_any_element(),
            );
        }
        if plugged {
            lines.push(self.render_pickers(tool, cx));
        }
        let id = tool.id.clone();
        div()
            .px(px(space::LG))
            .py(px(space::MD))
            .flex()
            .items_start()
            .gap(px(space::MD))
            .child(tool_tile(&tool.id, 36., cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().callout().medium().child(name.to_string()))
                            .child(ui::chip(chip, tone, true, cx))
                            .children(if tool.signed_in_on.is_empty() {
                                Vec::new()
                            } else {
                                self.render_usage(&tool.id, cx)
                            }),
                    )
                    .children(lines),
            )
            .child(
                Switch::new(SharedString::from(format!("coding-plug-{}", tool.id)))
                    .checked(plugged)
                    .disabled(self.plugging.contains_key(&tool.id))
                    .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                        this.set_plugged(id.clone(), *checked, window, cx)
                    })),
            )
            .into_any_element()
    }

    /// This Mac: what it holds and the tools in use it lacks. Filo always codes on it.
    fn render_runner(&self, runner: &RunnerRow, plugged: &[String], cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let mut parts = vec![plural(runner.repo_count, "project", "projects")];
        parts.extend(runner.free_disk_bytes.map(|free| format!("{} free", disk_size(free))));
        let status = parts.join(" · ");
        let missing = runner
            .tools
            .iter()
            .filter(|tool| !tool.signed_in && plugged.contains(&tool.id))
            .map(|tool| {
                format!("{} {}", tool_name(&tool.id), if tool.installed { "not signed in" } else { "not installed" })
            })
            .collect::<Vec<_>>()
            .join(" · ");
        div()
            .px(px(space::LG))
            .py(px(space::MD))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(icon_tile("laptop", cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .child(div().callout().medium().truncate().child(runner.name.clone()))
                    .child(div().footnote().text_color(colors.text_secondary).child(status))
                    .when(!missing.is_empty(), |this| {
                        this.child(div().footnote().text_color(colors.warning).child(missing))
                    }),
            )
            .into_any_element()
    }

    /// "Show 12 more" under a list, or "Show less" once it's open.
    fn show_more(
        &self,
        id: &'static str,
        hidden: usize,
        open: bool,
        toggle: fn(&mut Self),
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        (hidden > 0 || open).then(|| {
            div()
                .px(px(space::XS))
                .pt(px(space::XXS))
                .child(
                    ui::Button::ghost(id, if open { "Show less".to_string() } else { format!("Show {hidden} more") })
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            toggle(this);
                            cx.notify();
                        })),
                )
                .into_any_element()
        })
    }

    /// One line a project: its name, where it is and its branch, and "Not found" for a folder that went; a folder the
    /// user added can be taken off the list.
    fn render_project(&self, index: usize, project: &Project, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let computer = (!project.online).then(|| "Not found".to_string());
        let remove_path = project.path.clone();
        div()
            .id(SharedString::from(format!("coding-project-{index}")))
            .h(px(LINE_HEIGHT))
            .px(px(space::SM + 2.))
            .flex()
            .items_center()
            .gap(px(space::SM + 2.))
            .child(ui::icon_sized(
                if project.plain_folder { "folder" } else { "folder-git-2" },
                15.,
                colors.text_secondary,
            ))
            .child(div().flex_none().max_w(px(240.)).callout().medium().truncate().child(project.name.clone()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .footnote()
                    .text_color(colors.text_tertiary)
                    .truncate()
                    .child(short_path(&project.path)),
            )
            .when_some(project.branch.clone(), |this, branch| {
                this.child(
                    div()
                        .flex_none()
                        .max_w(px(180.))
                        .flex()
                        .items_center()
                        .gap(px(space::XS))
                        .child(ui::icon_sized("git-branch", 13., colors.text_tertiary))
                        .child(div().min_w_0().footnote().text_color(colors.text_secondary).truncate().child(branch)),
                )
            })
            .when_some(computer, |this, computer| {
                this.child(div().flex_none().footnote().text_color(colors.text_tertiary).child(computer))
            })
            .when(project.added, |this| {
                this.child(
                    IconButton::new(SharedString::from(format!("coding-project-remove-{index}")), "x")
                        .small()
                        .tooltip("Take off the list")
                        .on_click(cx.listener(move |_, _, window, cx| {
                            mutate(
                                window,
                                cx,
                                "coding:removeProject",
                                json!({ "path": remove_path }),
                                "Couldn't take it off the list",
                                |_: &mut Self, _, _| {},
                            );
                        })),
                )
            })
            .into_any_element()
    }

    fn render_projects(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let mut footer: Option<SharedString> = None;
        let body: AnyElement = match &self.projects {
            None => ui::skeleton_rows(3, None, cx).into_any_element(),
            Some(Err(error)) => note(error.clone(), cx).into_any_element(),
            Some(Ok(projects)) if projects.is_empty() => note(
                if self.query.is_empty() {
                    "No projects yet. Filo looks for git repos in your home folder; add any folder with Add folder."
                        .to_string()
                } else {
                    format!("No projects match “{}”.", self.query)
                },
                cx,
            )
            .into_any_element(),
            Some(Ok(projects)) => {
                // A search shows what it found; otherwise the first few, the rest behind "Show all".
                let open = self.all_projects || !self.query.is_empty();
                let shown = projects.len().min(if open { PROJECTS_SHOWN } else { PROJECTS_PREVIEW });
                if open && projects.len() > PROJECTS_SHOWN {
                    footer = Some(
                        format!("Showing {PROJECTS_SHOWN} of {}. Search to find the others.", projects.len()).into(),
                    );
                }
                let toggle = if self.query.is_empty() {
                    self.show_more(
                        "coding-projects-more",
                        projects.len() - shown,
                        self.all_projects,
                        |this| this.all_projects = !this.all_projects,
                        cx,
                    )
                } else {
                    None
                };
                let rows: Vec<AnyElement> = projects
                    .iter()
                    .take(shown)
                    .enumerate()
                    .map(|(index, project)| self.render_project(index, project, cx))
                    .collect();
                div()
                    .p(px(space::XS))
                    .flex()
                    .flex_col()
                    .children(rows)
                    .children(toggle.map(|toggle| crate::smoke::tracked("coding-projects-more", toggle)))
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(
                div()
                    .px(px(space::SM))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(div().flex_1().footnote().semibold().text_color(colors.text_secondary).child("Projects"))
                    .child(
                        IconButton::new("coding-projects-refresh", "refresh-cw")
                            .small()
                            .tooltip("Look for repos again")
                            .on_click(cx.listener(|_, _, window, cx| {
                                mutate(
                                    window,
                                    cx,
                                    "coding:refreshProjects",
                                    json!({}),
                                    "Couldn't look for projects",
                                    |_: &mut Self, _, _| {},
                                );
                            })),
                    )
                    .child(
                        Button::secondary("coding-add-folder", "Add folder")
                            .small()
                            .icon("folder")
                            .on_click(cx.listener(|this, _, window, cx| this.add_folder(window, cx))),
                    ),
            )
            .child(Input::new(&self.search).cleanable(true).prefix(ui::icon_sized(
                "search",
                16.,
                colors.text_secondary,
            )))
            .child(crate::smoke::tracked("coding-projects-card", ui::section(None, footer, vec![body], cx)))
            .into_any_element()
    }

    /// One line a task: a turning ring while it works, an eye while it monitors (a dot in its colour otherwise), its title, where and with what,
    /// its questions, its state and when it last changed. It opens the task's thread.
    fn render_task(&self, task: &TaskRow, now: f64, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let (state, label) = task_state(&task.status, false);
        let color = super::agent_work::state_tone(state).color(cx);
        let mut meta = vec![task.repo_name.clone(), tool_name(&task.tool)];
        // Whose it is: the agent's name (the main agent's for one of no agent's), or You.
        meta.push(task.owner.clone());
        let task_id = task.task_id.clone();
        div()
            .id(SharedString::from(format!("coding-task-{}", task.task_id)))
            .h(px(LINE_HEIGHT))
            .px(px(space::SM + 2.))
            .flex()
            .items_center()
            .gap(px(space::SM + 2.))
            .rounded(px(radius::SM))
            .cursor_pointer()
            .hover(|style| style.bg(colors.text.opacity(0.06)))
            .child(div().size(px(14.)).flex_none().flex().items_center().justify_center().map(|this| {
                if state == "monitoring" {
                    this.child(ui::icon_sized("eye", 14., color))
                } else if state == "active" {
                    this.child(ui::spinner(14., cx))
                } else {
                    this.child(div().size(px(8.)).rounded_full().bg(color))
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .callout()
                    .truncate()
                    .text_color(if is_finished(&task.status) { colors.text_secondary } else { colors.text })
                    .child(task.title.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(240.))
                    .footnote()
                    .text_color(colors.text_tertiary)
                    .truncate()
                    .child(meta.join(" · ")),
            )
            .when(task.pending > 0., |this| {
                this.child(ui::chip(plural(task.pending, "question", "questions"), Tone::Warning, true, cx))
            })
            .child(
                div()
                    .flex_none()
                    .w(px(120.))
                    .flex()
                    .justify_end()
                    .footnote()
                    .medium()
                    .text_color(color)
                    .child(with_worked(label, &task.status, task.worked_ms)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(68.))
                    .flex()
                    .justify_end()
                    .footnote()
                    .text_color(colors.text_tertiary)
                    .child(format::age(task.updated_at, now)),
            )
            .on_click(move |_, window, cx| nav::push(Route::CodingTask(task_id.clone()), window, cx))
            .into_any_element()
    }

    /// Every task still going, then the last few finished; the rest behind "Show all".
    fn render_tasks(&self, now: f64, cx: &mut Context<Self>) -> AnyElement {
        let body: AnyElement = match &self.tasks {
            None => ui::skeleton_rows(3, None, cx).into_any_element(),
            Some(Err(error)) => note(error.clone(), cx).into_any_element(),
            Some(Ok(tasks)) if tasks.is_empty() => note("No tasks yet.", cx).into_any_element(),
            Some(Ok(tasks)) => {
                let (going, finished): (Vec<&TaskRow>, Vec<&TaskRow>) =
                    tasks.iter().partition(|task| !is_finished(&task.status));
                let shown = if self.all_tasks { finished.len() } else { finished.len().min(FINISHED_SHOWN) };
                let toggle = self.show_more(
                    "coding-tasks-more",
                    finished.len() - shown,
                    self.all_tasks,
                    |this| this.all_tasks = !this.all_tasks,
                    cx,
                );
                div()
                    .p(px(space::XS))
                    .flex()
                    .flex_col()
                    .children(
                        going
                            .into_iter()
                            .chain(finished.into_iter().take(shown))
                            .map(|task| self.render_task(task, now, cx)),
                    )
                    .children(toggle)
                    .into_any_element()
            }
        };
        ui::section(Some("Tasks"), None, vec![body], cx).into_any_element()
    }
}

impl Render for CodingScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = format::now_ms();
        let header = ui::page_header("Coding", vec![]);
        let body: AnyElement = match &self.overview {
            None => ui::skeleton_page(&[2, 2, 3], Some(36.), cx).into_any_element(),
            Some(Err(error)) => ui::banner(error.clone(), Tone::Danger, cx).into_any_element(),
            Some(Ok(overview)) => {
                let overview = overview.clone().unwrap_or_else(|| Overview {
                    tools: Vec::new(),
                    workspace: default_workspace(),
                    keep_on_quit: true,
                    keep_awake: false,
                    keep_awake_lid: false,
                    keep_awake_lid_access: false,
                    keep_awake_lid_problem: None,
                    runners: Vec::new(),
                });
                let plugged = overview.plugged();
                // The computers first, then the tools they run.
                let tool_rows: Vec<AnyElement> = filo_local::coding::TOOLS
                    .iter()
                    .map(|id| {
                        let tool =
                            overview.tools.iter().find(|tool| tool.id == *id).cloned().unwrap_or_else(|| ToolSummary {
                                id: id.to_string(),
                                plugged: false,
                                installed_on: Vec::new(),
                                signed_in_on: Vec::new(),
                                account: None,
                                plan: None,
                                version: None,
                                ready: false,
                                mode: default_mode(),
                                model: None,
                                effort: None,
                                models: Vec::new(),
                            });
                        self.render_tool(&tool, cx)
                    })
                    .collect();
                let mut computer_rows: Vec<AnyElement> =
                    overview.runners.iter().map(|runner| self.render_runner(runner, &plugged, cx)).collect();
                if !computer_rows.is_empty() {
                    computer_rows.push(self.render_keep_on_quit(&overview, cx));
                    computer_rows.push(self.render_keep_awake(&overview, cx));
                    computer_rows.push(self.render_keep_awake_lid(&overview, cx));
                }
                const UNPLUG: &str = "Unplugging a tool stops its running tasks.";
                let mut sections = Vec::new();
                if !computer_rows.is_empty() {
                    sections.push(ui::section(Some("Computers"), None, computer_rows, cx).into_any_element());
                }
                sections.push(ui::section(Some("Tools"), Some(UNPLUG.into()), tool_rows, cx).into_any_element());
                sections.push(
                    ui::section(
                        Some("Where tasks work by default"),
                        None,
                        vec![self.render_workspace(&overview, cx)],
                        cx,
                    )
                    .into_any_element(),
                );
                sections.push(self.render_tasks(now, cx));
                sections.push(self.render_projects(cx));
                div().flex().flex_col().gap(px(space::XXL)).children(sections).into_any_element()
            }
        };
        div()
            .id("coding-scroll")
            .size_full()
            .overflow_y_scroll()
            .when_some(crate::smoke::page_scroll("coding"), |this, handle| this.track_scroll(&handle))
            .child(crate::smoke::tracked(
                "coding-end",
                ui::page_column()
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XL))
                    .child(header)
                    .child(body)
                    .children(crate::smoke::page_end("coding")),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coding_tasks_row_leads_with_the_task() {
        assert_eq!(task_heading("Make the sidebar rows compact", "claude-code"), "Make the sidebar rows compact");
        // Its mark names the tool; the name is written only while the task has no title.
        assert_eq!(task_heading("  ", "codex"), "Codex");
        assert_eq!(task_heading("", "claude-code"), "Claude Code");
    }

    #[test]
    fn a_round_that_only_waited_says_monitoring_only_while_it_still_waits() {
        // Its card: the task's state, Monitoring while it waits, Working once it carried on, Done once it finished.
        assert_eq!(round_state(Some("monitoring"), Some("monitoring")), ("monitoring", "Monitoring"));
        assert_eq!(round_state(Some("monitoring"), Some("running")), ("active", "Working"));
        assert_eq!(round_state(Some("monitoring"), Some("done")), ("completed", "Done"));
        assert_eq!(round_state(Some("monitoring"), Some("failed")), ("failed", "Failed"));
        assert_eq!(round_state(Some("monitoring"), None), ("monitoring", "Monitoring"));
        // A round that ended on its own keeps how it ended.
        assert_eq!(round_state(Some("failed"), Some("done")), ("failed", "Failed"));
        // Its page: Monitoring while it's the latest round and the task waits, else Waited.
        assert_eq!(round_page_state("monitoring", Some("monitoring"), true), ("monitoring", "Monitoring"));
        assert_eq!(round_page_state("monitoring", Some("done"), true), ("waited", "Waited"));
        assert_eq!(round_page_state("monitoring", Some("done"), false), ("waited", "Waited"));
        assert_eq!(round_page_state("monitoring", Some("running"), false), ("waited", "Waited"));
        assert_eq!(round_page_state("monitoring", Some("monitoring"), false), ("waited", "Waited"));
        assert_eq!(round_page_state("done", Some("done"), true), ("completed", "Done"));
        assert_eq!(round_page_state("done", Some("running"), false), ("active", "Working"));
    }

    #[test]
    fn labels_match_the_shared_protocol() {
        assert_eq!(status_label("waiting_on_you"), "Waiting on you");
        assert_eq!(status_label("queued"), "Queued");
        assert!(is_finished("stopped") && !is_finished("running"));
        assert_eq!(tool_name("claude-code"), "Claude Code");
        assert_eq!(answer_label("approval", "allow_always"), "Allowed always");
        assert_eq!(answer_label("question", "Use main"), "Use main");
        assert_eq!(proof_kind_label("image"), "Screenshot");
        assert_eq!(proof_kind_label("video"), "Recording");
        assert_eq!(proof_kind_label("file"), "Output");
    }

    #[test]
    fn a_proofs_size_reads_as_in_the_chat() {
        let mb = 1024. * 1024.;
        assert_eq!(format::file_size(12.4 * mb), "12.4 MB");
        assert_eq!(format::file_size(820. * 1024.), "820 KB");
        assert_eq!(format::file_size(100.), "100 B");
    }

    #[test]
    fn a_tool_that_cant_take_tasks_says_why() {
        let tool = |signed_in: &[&str], installed: &[&str], ready: bool| ToolSummary {
            id: "codex".into(),
            plugged: true,
            installed_on: installed.iter().map(|name| name.to_string()).collect(),
            signed_in_on: signed_in.iter().map(|name| name.to_string()).collect(),
            account: None,
            plan: None,
            version: None,
            ready,
            mode: default_mode(),
            model: None,
            effort: None,
            models: Vec::new(),
        };
        let signed_in = tool(&["Mac"], &["Mac"], false);
        assert_eq!(tool_chip(&tool(&["Mac"], &["Mac"], true), true).0, "Ready");
        assert_eq!(tool_chip(&signed_in, false).0, "Unplugged");
        assert_eq!(tool_chip(&signed_in, true).0, "Computer offline");
        assert_eq!(tool_chip(&tool(&[], &["Mac"], false), true).0, "Not signed in");
        assert_eq!(tool_chip(&tool(&[], &[], false), true).0, "Not installed");
    }

    /// A plan pill says what's left, rounded; a low one adds when it resets (the time within a day, the weekday within
    /// the week) and turns warning, then danger; its tooltip always says both. One whose reset passed says nothing until
    /// it's asked again.
    #[test]
    fn a_plan_pill_says_whats_left_and_when_it_resets() {
        let now = 1_792_100_000_000.;
        let window = |used_percent: f64, resets_at: Option<f64>| UsageWindow { used_percent, resets_at };
        let in_hours = |hours: f64| Some(now + hours * 3_600_000.);
        let clock = format::clock(now + 2. * 3_600_000.);
        let plenty = meter("5h", "5-hour limit", &window(38.4, in_hours(2.)), now).unwrap();
        assert_eq!((plenty.text.as_str(), plenty.tone), ("5h 62%", Tone::Neutral));
        assert_eq!(plenty.tip, format!("5-hour limit: 62% left, resets {clock}"));
        assert_eq!(plenty.left, 62.);
        let monday = format::local(now + 72. * 3_600_000.).format("%a").to_string();
        let low = meter("Week", "Weekly limit", &window(75., in_hours(72.)), now).unwrap();
        assert_eq!((low.text, low.tone), (format!("Week 25% · {monday}"), Tone::Warning));
        let out = meter("Week", "Weekly limit", &window(104., in_hours(200.)), now).unwrap();
        let date = format::short_date(now + 200. * 3_600_000.);
        assert_eq!((out.text, out.tone, out.left), (format!("Week 0% · {date}"), Tone::Danger, 0.));
        assert_eq!(meter("5h", "5-hour limit", &window(26., in_hours(2.)), now).unwrap().tone, Tone::Neutral);
        assert_eq!(meter("5h", "5-hour limit", &window(90., in_hours(2.)), now).unwrap().tone, Tone::Danger);
        let unknown = meter("5h", "5-hour limit", &window(0., None), now).unwrap();
        assert_eq!((unknown.text.as_str(), unknown.tip.as_str()), ("5h 100%", "5-hour limit: 100% left"));
        assert_eq!(meter("5h", "5-hour limit", &window(20., in_hours(-0.1)), now), None);
    }

    /// The last answer saved for the next start reads back as `coding:usage` gave it.
    #[test]
    fn saved_plan_usage_reads_back() {
        let answer: HashMap<String, PlanUsage> =
            serde_json::from_value(json!({ "claude-code": { "fiveHour": { "usedPercent": 26.0, "resetsAt": 1_792_100_000_000.0 }, "week": { "usedPercent": 16.0, "resetsAt": null } } })).unwrap();
        let saved = serde_json::to_value(SavedUsage { usage: Some(answer.clone()) }).unwrap();
        assert_eq!(saved["usage"]["claude-code"]["fiveHour"]["usedPercent"], 26.0);
        assert_eq!(serde_json::from_value::<SavedUsage>(saved).unwrap().usage, Some(answer));
        assert_eq!(serde_json::from_value::<SavedUsage>(json!({})).unwrap().usage, None);
    }

    #[test]
    fn project_paths_start_at_home() {
        assert_eq!(short_path("/Users/me/Desktop/app"), "~/Desktop/app");
        assert_eq!(short_path("/home/me/src/app"), "~/src/app");
        assert_eq!(short_path("/opt/app"), "/opt/app");
    }

    #[test]
    fn disk_sizes_read_plainly() {
        assert_eq!(disk_size(41_300_000_000.), "41.3 GB");
        assert_eq!(disk_size(820_000_000.), "820 MB");
        assert_eq!(disk_size(250_000_000_000.), "250 GB");
    }

    #[test]
    fn reads_the_overview() {
        let overview: Overview = serde_json::from_value(json!({
            "plugged": ["codex"],
            "tools": [{ "id": "codex", "plugged": true, "installedOn": ["Mac"], "signedInOn": ["Mac"], "account": "a@b.c", "ready": true }],
            "runners": [{
                "name": "Mac", "platform": "darwin", "version": "1",
                "lastSeenAt": 1.0, "repoCount": 3.0, "freeDiskBytes": 1.0e9,
                "tools": [{ "id": "codex", "installed": true, "signedIn": true }],
            }],
        }))
        .unwrap();
        assert_eq!(overview.runners[0].repo_count, 3.);
        assert!(overview.tools[0].ready);
        // "Keep coding after Filo quits" is on unless the backend says it's off.
        assert!(overview.keep_on_quit);
        let off: Overview = serde_json::from_value(json!({ "tools": [], "runners": [], "keepOnQuit": false })).unwrap();
        assert!(!off.keep_on_quit);
        // "Keep Mac awake while tasks run" is off unless the backend says it's on.
        assert!(!overview.keep_awake);
        let on: Overview = serde_json::from_value(json!({ "tools": [], "runners": [], "keepAwake": true })).unwrap();
        assert!(on.keep_awake);
        // "Keep running with the lid closed" is off unless the backend says it's on.
        assert!(!overview.keep_awake_lid);
    }
}

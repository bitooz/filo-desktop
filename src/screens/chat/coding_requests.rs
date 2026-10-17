// Coding questions handed to the user in a chat: when Filo or an agent can't answer what a coding tool asked,
// the question waits in the panel above the composer (question_panel.rs), read from `coding:requestsForSession` and
// answered with `coding:answerRequest`.
//
// The chat keeps them where they were asked (`place`, by `askedAt`, then `createdAt`, then its id; never by their state
// or when they were answered): a task's questions waiting together share one line ("Claude Code asked 2 questions ·
// answer below"); each answered or closed one is a card of its own, folded to what was asked and the answer, which
// opens to the question and its command. The backend lists the 20 newest answered and 10 newest closed ones per chat.

use std::collections::HashMap;

use gpui::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::text::TextView;
use serde::Deserialize;

use super::feed::FeedItem;
use crate::screens::coding::{answer_label, tool_tile};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub request_id: String,
    pub task_id: String,
    /// "question" or "approval".
    pub kind: String,
    /// In the agent's words.
    pub question: String,
    /// The tool's own wording (a command, a path), shown as it is; a question's short header ("Main default").
    pub detail: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    /// What each option means, by its place in `options` (empty for none); older backends leave it out.
    #[serde(default)]
    pub option_notes: Vec<String>,
    /// The tool's name ("Claude Code", "Codex").
    pub tool: String,
    pub project: String,
    #[serde(default)]
    pub task_title: String,
    /// "pending", "answered" or "expired" (its task finished first).
    pub state: String,
    pub answer: Option<String>,
    pub decided_by: Option<String>,
    /// When it was handed to the user: where its line sits in the chat.
    #[serde(default)]
    pub asked_at: Option<f64>,
    /// When the tool asked; older backends leave it out.
    #[serde(default)]
    pub created_at: Option<f64>,
}

impl ChatRequest {
    /// Its place in the chat's timeline.
    pub fn at(&self) -> f64 {
        self.asked_at.or(self.created_at).unwrap_or(0.)
    }

    /// The tool's id on the Coding page, from the name the request carries ("Muse Code" → `muse`; Claude Code's for
    /// one it doesn't name).
    pub(super) fn tool_id(&self) -> &'static str {
        filo_local::coding::tool_of(filo_local::Engine::parse(&self.tool).unwrap_or(filo_local::Engine::Claude))
    }

    /// The panel's header: tool and task (the project when the task has no title).
    pub(super) fn context(&self) -> String {
        let what = if self.task_title.is_empty() { &self.project } else { &self.task_title };
        if what.is_empty() { self.tool.clone() } else { format!("{} · {what}", self.tool) }
    }

    /// The question's short name, for its tab and its answer's record: a question's own header when it has a short
    /// one, else "Permission" or "Question <n>".
    pub(super) fn label(&self, number: usize) -> String {
        if self.kind == "approval" {
            return "Permission".into();
        }
        match self.header() {
            Some(header) => header.to_string(),
            None => format!("Question {number}"),
        }
    }

    /// A question's header (Claude Code's "header", up to a few words), which names its tab instead of showing below it.
    fn header(&self) -> Option<&str> {
        let detail = self.detail.as_deref()?.trim();
        (self.kind != "approval" && !detail.is_empty() && !detail.contains('\n') && detail.chars().count() <= 28)
            .then_some(detail)
    }

    /// The tool's wording shown under the question: an approval's command or path, a question's longer detail.
    pub(super) fn shown_detail(&self) -> Option<&str> {
        let detail = self.detail.as_deref().filter(|detail| !detail.trim().is_empty())?;
        self.header().is_none().then_some(detail)
    }

    /// An approval's command, when it's one (`Run \`npm install\``): shown as code.
    /// A long one is stored cut (filo-local `store::cut`, at 1,000 characters): it ends in "…", not its backquote.
    pub(super) fn command(&self) -> Option<&str> {
        if self.kind != "approval" {
            return None;
        }
        let rest = self.question.trim().strip_prefix("Run `")?;
        let command = rest.strip_suffix('`').or_else(|| rest.strip_suffix('…'))?;
        Some(command.trim()).filter(|command| !command.is_empty())
    }

    /// A command's description, when it reads as one: a short line, not more of the command.
    fn description(&self) -> Option<&str> {
        self.command()?;
        self.detail
            .as_deref()
            .map(str::trim)
            .filter(|detail| !detail.is_empty() && !detail.contains('\n') && detail.chars().count() <= 120)
    }

    /// What was asked, in a few words, for its line in the chat: a command's description ("Rerun the layout proof"),
    /// else the command; otherwise the words it was asked in.
    pub(super) fn title(&self) -> String {
        let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(command) = self.command() {
            return match self.description() {
                Some(description) => one_line(description),
                None => format!("Run {}", one_line(command)),
            };
        }
        one_line(&super::feed::strip_markdown_for_quote(&self.question))
    }

    /// The question and the tool's wording, for the clipboard.
    pub(super) fn copy_text(&self) -> String {
        let mut text = self.question.trim().to_string();
        if let Some(detail) = self.detail.as_deref().filter(|detail| !detail.trim().is_empty()) {
            text.push_str("\n\n");
            text.push_str(detail.trim());
        }
        text
    }
}

/// One answer to pick: what it says, what it means, whether the asker recommends it, and what goes back to the tool.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    pub note: String,
    pub recommended: bool,
    pub answer: String,
}

/// A label without the "(Recommended)" the tools mark their suggestion with, and whether it had one.
fn without_recommended(option: &str) -> (String, bool) {
    let lower = option.to_lowercase();
    match lower.find("(recommended)") {
        Some(at) => {
            let label = format!("{}{}", &option[..at], &option[at + "(recommended)".len()..]);
            (label.split_whitespace().collect::<Vec<_>>().join(" "), true)
        }
        None => (option.trim().to_string(), false),
    }
}

/// The answers a request offers: Allow, Allow always and Deny for an approval; the options for a question.
pub fn choices(request: &ChatRequest) -> Vec<Choice> {
    if request.kind == "approval" {
        let choice = |label: &str, note: String, answer: &str| Choice {
            label: label.into(),
            note,
            recommended: false,
            answer: answer.into(),
        };
        return vec![
            choice("Allow", "Just this once".into(), "allow"),
            choice("Allow always", format!("For the rest of {}'s session", request.tool), "allow_always"),
            choice("Deny", format!("{} doesn't do it", request.tool), "deny"),
        ];
    }
    request
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let (label, recommended) = without_recommended(option);
            Choice {
                label,
                note: request.option_notes.get(index).map(|note| note.trim().to_string()).unwrap_or_default(),
                recommended,
                answer: option.clone(),
            }
        })
        .collect()
}

/// How an answer reads: an approval's in words, an option without its "(Recommended)", a skipped question as so.
pub fn shown_answer(request: &ChatRequest, answer: &str) -> String {
    if answer == SKIPPED {
        return "Skipped".into();
    }
    if request.kind == "approval" {
        return answer_label(&request.kind, answer);
    }
    if request.options.iter().any(|option| option == answer) {
        without_recommended(answer).0
    } else {
        answer.to_string()
    }
}

/// What a question the user skipped tells the tool.
pub const SKIPPED: &str = filo_local::coding_handoff::SKIPPED;

/// The chat's requests in the order they sit in, each once (a later copy of the same request wins): asked first, then
/// asked of the tool first, then by id. Answering or closing one never changes where it is.
pub(super) fn ordered(requests: Vec<ChatRequest>) -> Vec<ChatRequest> {
    let mut by_id: HashMap<String, ChatRequest> = HashMap::new();
    for request in requests {
        by_id.insert(request.request_id.clone(), request);
    }
    let mut out: Vec<ChatRequest> = by_id.into_values().collect();
    out.sort_by(|a, b| {
        a.at()
            .total_cmp(&b.at())
            .then_with(|| a.created_at.unwrap_or(a.at()).total_cmp(&b.created_at.unwrap_or(b.at())))
            .then_with(|| a.request_id.cmp(&b.request_id))
    });
    out
}

/// The feed with the requests in it, each after everything up to the moment it was asked (an item from the same
/// millisecond comes first). `requests` are `ordered`. With older history still unloaded (`loaded_from`, the oldest
/// loaded message), those from before it wait until that page loads, rather than sitting above it.
pub(super) fn place(items: Vec<FeedItem>, requests: &[ChatRequest], loaded_from: Option<f64>) -> Vec<FeedItem> {
    let mut requests =
        requests.iter().filter(|request| loaded_from.is_none_or(|from| request.at() >= from)).cloned().peekable();
    if requests.peek().is_none() {
        return items;
    }
    let mut placed = Vec::with_capacity(items.len() + 4);
    for item in items {
        while let Some(request) = requests.next_if(|request| request.at() < item.timestamp()) {
            placed.push(FeedItem::CodingRequest(request));
        }
        placed.push(item);
    }
    placed.extend(requests.map(FeedItem::CodingRequest));
    placed
}

/// Plain text for the markdown view, shown as written: punctuation escaped (no emphasis, links or HTML out of a path or
/// a command), each line its own paragraph (the text view drops a hard break inside a paragraph, which ran two paths
/// together; `detail_style` keeps the paragraphs as tight as lines).
pub(super) fn plain_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for (index, line) in text.trim_end().lines().filter(|line| !line.trim().is_empty()).enumerate() {
        if index > 0 {
            out.push_str("\n\n");
        }
        let line = line.trim_end();
        // Leading spaces would make a code block; keep their width as non-breaking spaces.
        let indent = line.len() - line.trim_start().len();
        out.extend(std::iter::repeat_n('\u{a0}', indent));
        for character in line.trim_start().chars() {
            if character.is_ascii_punctuation() {
                out.push('\\');
            }
            out.push(character);
        }
    }
    out
}

/// The tool's own wording: one line a paragraph, with no room between them.
pub(super) fn detail_style() -> gpui_component::text::TextViewStyle {
    ui::markdown_style().paragraph_gap(gpui::rems(0.1))
}

/// How an answer goes out: to the backend, or (`FILO_OPEN=coding-cards`) to a script that fails the first send and
/// takes the next, so the error and retry show without answering anything real.
#[derive(Clone)]
pub enum Answerer {
    Backend,
    #[cfg(feature = "dev")]
    Fixture,
}

/// A request as its line in the chat tells it: still waiting, or answered (by whom, and what) or closed. `sent` are the
/// answers the panel sent that the backend's copy doesn't say yet.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Outcome {
    Waiting,
    Answered { by_user: bool, answer: String },
    Closed,
}

pub(super) fn outcome(request: &ChatRequest, sent: &[(String, String)]) -> Outcome {
    match request.state.as_str() {
        "expired" => Outcome::Closed,
        "pending" => match sent.iter().find(|(id, _)| *id == request.request_id) {
            Some((_, answer)) => Outcome::Answered { by_user: true, answer: shown_answer(request, answer) },
            None => Outcome::Waiting,
        },
        _ => Outcome::Answered {
            by_user: request.decided_by.as_deref() != Some("agent"),
            answer: shown_answer(request, request.answer.as_deref().unwrap_or("")),
        },
    }
}

/// What the line says first: who answered, or that they wait or closed.
pub(super) fn record_lead(requests: &[ChatRequest], sent: &[(String, String)], agent_name: &str) -> String {
    let outcomes: Vec<Outcome> = requests.iter().map(|request| outcome(request, sent)).collect();
    let tool = requests.first().map_or("The coding tool", |request| request.tool.as_str());
    if outcomes.contains(&Outcome::Waiting) {
        return match requests {
            [one] if one.kind == "approval" => format!("{tool} asks for permission"),
            [_] => format!("{tool} asked a question"),
            _ => format!("{tool} asked {} questions", requests.len()),
        };
    }
    if outcomes.iter().all(|outcome| *outcome == Outcome::Closed) {
        return format!("Closed without an answer · {tool}");
    }
    let by_user = |want: bool| {
        outcomes.iter().all(|outcome| {
            matches!(outcome, Outcome::Answered { by_user, .. } if *by_user == want) || *outcome == Outcome::Closed
        })
    };
    if by_user(true) {
        format!("You answered {tool}")
    } else if by_user(false) {
        format!("{agent_name} answered {tool}")
    } else {
        format!("Answered {tool}")
    }
}

/// A task's questions waiting on the user, as one line in the chat where they were asked: "Claude Code asked 2
/// questions · answer below". A click takes the keyboard to the panel above the composer (`on_click`).
pub(super) fn render_waiting(
    requests: &[ChatRequest],
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui::AnyElement {
    let colors = *cx.colors();
    let Some(first) = requests.first() else { return div().into_any_element() };
    let id = SharedString::from(format!("coding-ask-{}-line", first.request_id));
    let line = div()
        .id(id.clone())
        .max_w_full()
        .px(px(space::SM + 3.))
        .py(px(space::XS + 1.))
        .flex()
        .items_center()
        .gap(px(space::SM - 2.))
        .rounded(px(radius::MD))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .cursor_pointer()
        .hover(|style| style.bg(colors.surface_strong))
        .on_click(move |event: &ClickEvent, window, cx| {
            if event.standard_click() {
                on_click(window, cx);
            }
        })
        .child(tool_tile(first.tool_id(), 13., cx))
        .child(div().min_w_0().footnote().text_color(colors.text_secondary).truncate().child(record_lead(
            requests,
            &[],
            "",
        )))
        .child(div().flex_none().footnote().text_color(colors.text_tertiary).child("· answer below"));
    crate::smoke::tracked_fill(id.to_string(), div().w_full().flex().justify_center().child(line)).into_any_element()
}

/// `text` on one line, cut with "…" to about what `lines` lines of monospace at `size` hold in `room` px (a little
/// less, as lines break between words).
fn clip_lines(text: &str, room: f32, size: f32, lines: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let per_line = (room / (size * 0.62)).floor().max(10.) as usize;
    let fits = per_line * lines * 9 / 10;
    if text.chars().count() <= fits {
        return text;
    }
    let mut cut: String = text.chars().take(fits.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// One answered (or closed) request in the chat, folded to a line: what was asked in a few words and the answer, and
/// who gave it. Open (`expanded`), it shows the question, the command in small monospace (two lines at most), a long
/// answer in full, and Copy question and Open the task. `sent` is the answer the panel sent that the backend's copy
/// doesn't say yet. A click on the line opens and folds it (`on_toggle`).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_answered(
    request: &ChatRequest,
    sent: Option<&str>,
    agent_name: &str,
    expanded: bool,
    width: f32,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let colors = *cx.colors();
    let sent: Vec<(String, String)> =
        sent.map(|answer| (request.request_id.clone(), answer.to_string())).into_iter().collect();
    let (answer, by) = match outcome(request, &sent) {
        Outcome::Answered { by_user, answer } => {
            (answer, Some(if by_user { "you".to_string() } else { agent_name.to_string() }))
        }
        _ => ("Closed without an answer".to_string(), None),
    };
    let closed = by.is_none();
    let id = |part: &str| SharedString::from(format!("coding-ask-{}-{part}", request.request_id));
    let fold = div()
        .id(id("fold"))
        .w_full()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .cursor_pointer()
        .on_click(move |event: &ClickEvent, window, cx| {
            if event.standard_click() {
                on_toggle(window, cx);
            }
        })
        .child(ui::icon_sized(
            if closed { "ban" } else { "circle-check" },
            14.,
            if closed { colors.text_tertiary } else { colors.success },
        ))
        // One line ending in "…" when cut: a clamp, not `truncate`, which keeps the uncut text it measured first.
        .child(
            div()
                .flex_1()
                .min_w_0()
                .footnote()
                .text_color(colors.text)
                .line_clamp(1)
                .text_ellipsis()
                .child(request.title()),
        )
        .child(
            div()
                .flex_none()
                .max_w(px((width * 0.4).max(80.)))
                .min_w_0()
                .footnote()
                .semibold()
                .text_color(if closed { colors.text_secondary } else { colors.text })
                .line_clamp(1)
                .text_ellipsis()
                .child(answer.clone()),
        )
        .children(by.map(|by| div().flex_none().caption().text_color(colors.text_tertiary).child(format!("by {by}"))))
        .child(ui::icon_sized(if expanded { "chevron-up" } else { "chevron-down" }, 14., colors.text_tertiary));
    let details = expanded.then(|| {
        let question: Option<gpui::AnyElement> = match request.command() {
            // Its command says it: shown below as code, not as words; a long note from the tool, as words.
            Some(_) => request
                .detail
                .as_deref()
                .map(str::trim)
                .filter(|detail| !detail.is_empty() && request.description().is_none())
                .map(|detail| {
                    div()
                        .line_clamp(3)
                        .text_color(colors.text_secondary)
                        .child(detail.split_whitespace().collect::<Vec<_>>().join(" "))
                        .into_any_element()
                }),
            None => Some(
                TextView::markdown(id("question"), request.question.clone(), window, cx)
                    .style(ui::markdown_style())
                    .selectable(true)
                    .into_any_element(),
            ),
        };
        // The command (or the tool's wording), as code, cut to what two lines hold.
        let size = cx.theme().mono_size() * 0.85;
        let room = width - 2. * space::MD - 2. * space::SM - 4.;
        let code = request.command().or_else(|| request.shown_detail()).map(|code| clip_lines(code, room, size, 2));
        let task_id = request.task_id.clone();
        let copy = request.copy_text();
        let link = |part: &str, icon: &'static str, label: &'static str| {
            div()
                .id(id(part))
                .flex()
                .items_center()
                .gap(px(space::XS))
                .px(px(space::XS))
                .rounded(px(radius::XS))
                .caption()
                .text_color(colors.text_secondary)
                .cursor_pointer()
                .hover(|style| style.text_color(colors.text))
                .child(ui::icon_sized(icon, 12., colors.text_tertiary))
                .child(label)
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(space::SM - 2.))
                    .child(tool_tile(request.tool_id(), 14., cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .caption()
                            .text_color(colors.text_secondary)
                            .truncate()
                            .child(request.context()),
                    ),
            )
            .children(question.map(|question| div().w_full().footnote().text_color(colors.text).child(question)))
            .children(code.map(|code| {
                div()
                    .w_full()
                    .px(px(space::SM))
                    .py(px(space::XS + 1.))
                    .rounded(px(radius::XS))
                    .bg(colors.chat_background)
                    .overflow_hidden()
                    .font_family(cx.theme().mono_family())
                    .text_size(px(size))
                    .line_height(px((size * 1.45).round()))
                    .text_color(colors.text_secondary)
                    .line_clamp(2)
                    .child(code)
            }))
            .when(answer.chars().count() > 60 && !closed, |this| {
                let view = TextView::markdown(id("answer"), plain_markdown(&answer), window, cx)
                    .style(ui::markdown_style())
                    .selectable(true);
                this.child(div().w_full().footnote().text_color(colors.text).child(view))
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .child(link("copy", "copy", "Copy question").on_click(move |_, window, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                        ui::toast(window, cx, "Question copied");
                    }))
                    .child(link("task", "arrow-up-right", "Open the task").on_click(move |_, window, cx| {
                        crate::nav::push(crate::nav::Route::CodingTask(task_id.clone()), window, cx)
                    })),
            )
    });
    let card = div()
        .id(id("card"))
        .w(px(width))
        .px(px(space::MD))
        .py(px(space::SM))
        .rounded(px(radius::MD))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .child(crate::smoke::tracked(id("fold").to_string(), fold))
        .children(details);
    card.into_any_element()
}

/// The fixture `FILO_OPEN=coding-cards` shows (a reported layout): two disk-space questions answered long ago, a
/// later reply below them, then two questions Claude Code asked together (headers, descriptions, a recommended one)
/// and, from another task, a permission request that waits its turn.
#[cfg(feature = "dev")]
pub fn fixture_requests(now: f64) -> Vec<ChatRequest> {
    let minute = 60_000.;
    let request = |id: &str,
                   task: &str,
                   at: f64,
                   state: &str,
                   question: &str,
                   detail: Option<&str>,
                   options: &[(&str, &str)],
                   answer: Option<&str>| ChatRequest {
        request_id: id.into(),
        task_id: task.into(),
        kind: "question".into(),
        question: question.into(),
        detail: detail.map(str::to_string),
        options: options.iter().map(|(option, _)| option.to_string()).collect(),
        option_notes: options.iter().map(|(_, note)| note.to_string()).collect(),
        tool: "Claude Code".into(),
        project: "notes-app".into(),
        task_title: if task == "task-disk" {
            "Free up disk space".into()
        } else {
            "Restore docked Working sidebar".into()
        },
        state: state.into(),
        answer: answer.map(str::to_string),
        decided_by: answer.map(|_| "user".into()),
        asked_at: Some(at),
        created_at: Some(at - 2_000.),
    };
    vec![
        request(
            "disk-1",
            "task-disk",
            now - 50. * minute,
            "answered",
            "The disk is **99% full** (3.3 GB free). Can Claude Code delete the Rust build cache in `apps/desktop/target/debug/incremental` (about 4.1 GB)? It rebuilds on the next build.",
            Some("Rust cache"),
            &[("Yes, delete it", ""), ("No, keep it", "")],
            Some("Yes, delete it"),
        ),
        request(
            "disk-2",
            "task-disk",
            now - 50. * minute + 400.,
            "answered",
            "Also clear the Xcode simulator caches under `~/Library/Developer/CoreSimulator/Caches`? About 6 GB.",
            Some("Simulators"),
            &[("Clear them", ""), ("Leave them", "")],
            Some("Leave them, I still need the iOS 18 runtime for the widget work this week"),
        ),
        request(
            "disk-3",
            "task-disk",
            now - 4. * minute,
            "pending",
            "Claude Code found old `node_modules` folders in other projects. Which ones should go?",
            Some("Old folders"),
            &[
                ("Only old-experiments", "landing-page-v1, untouched since March (2.4 GB)"),
                ("Both folders (Recommended)", "old-experiments and scratch (4.2 GB)"),
                ("Neither", "Keep everything as it is for now"),
            ],
            None,
        ),
        request(
            "disk-3b",
            "task-disk",
            now - 4. * minute + 300.,
            "pending",
            "After that, should it empty the npm and pnpm caches too?",
            Some("Package caches"),
            &[
                ("Empty them (Recommended)", "About 3 GB; they refill as you install"),
                ("Keep them", "Installs stay fast offline"),
            ],
            None,
        ),
        // Two permissions the agent decided on its own (a reported layout): long commands, short descriptions.
        ChatRequest {
            kind: "approval".into(),
            options: Vec::new(),
            option_notes: Vec::new(),
            decided_by: Some("agent".into()),
            ..request(
                "perm-1",
                "task-disk",
                now - 8. * minute,
                "answered",
                "Run `cd /Users/me/Projects/filo-desktop; python3 /tmp/patch.py src/screens/chat/question_panel.rs --footer-height 28 && cargo build 2>&1 | grep -E \"^error\" -A10 | head -20`",
                Some("Fix footer height and rerun layout proof"),
                &[],
                Some("allow"),
            )
        },
        ChatRequest {
            kind: "approval".into(),
            options: Vec::new(),
            option_notes: Vec::new(),
            decided_by: Some("agent".into()),
            ..request(
                "perm-2",
                "task-disk",
                now - 8. * minute + 500.,
                "answered",
                // As the backend keeps a long one: cut at 1,000 characters, so it ends in "…", not its backquote.
                &filo_local::store::cut(
                    &format!(
                        "Run `cd /Users/me/Projects/filo-desktop; P=/Users/me/.config/filo/local/coding/proof/306da29d/round-2; rm -f $P/*.png; env -u FILO_OPEN FILO_CONFIG_DIR=/tmp/filo-qp-config FILO_WINDOW=1004x760 FILO_OPEN=coding-cards:agent FILO_FIXTURE_PLAN=\"{}\" ./target/debug/filo 2>&1 | grep fixture:`",
                        "replied,wait:1500,layout:a3,key:enter,wait:300,".repeat(24)
                    ),
                    1_000,
                ),
                Some("Rerun the layout proof"),
                &[],
                Some("allow_always"),
            )
        },
        ChatRequest {
            kind: "approval".into(),
            options: Vec::new(),
            option_notes: Vec::new(),
            ..request(
                "disk-4",
                "task-sidebar",
                now - 2. * minute,
                "pending",
                "Claude Code wants to delete Xcode's derived data outside the project. Allow it?",
                Some(
                    "rm -rf ~/Library/Developer/Xcode/DerivedData/FiloWidgetExtension-bkqzjxhcyrqwmvdhgafuoeptlsnz/Build/Intermediates.noindex",
                ),
                &[],
                None,
            )
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: &str, at: f64, state: &str) -> ChatRequest {
        ChatRequest {
            request_id: id.into(),
            task_id: "t1".into(),
            kind: "question".into(),
            question: format!("Question {id}?"),
            detail: None,
            options: vec!["Yes".into(), "No".into()],
            option_notes: Vec::new(),
            tool: "Claude Code".into(),
            project: "filo".into(),
            task_title: "Free disk space".into(),
            state: state.into(),
            answer: (state == "answered").then(|| "Yes".into()),
            decided_by: (state == "answered").then(|| "user".into()),
            asked_at: Some(at),
            created_at: Some(at),
        }
    }

    fn message(id: &str, at: f64) -> FeedItem {
        FeedItem::Message {
            message: serde_json::from_value(
                json!({ "_id": id, "_creationTime": at, "role": "assistant", "content": id }),
            )
            .unwrap(),
        }
    }

    /// The feed as ids: messages by id, cards as "?id".
    fn shape(items: &[FeedItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                FeedItem::Message { message, .. } => message.id.clone(),
                FeedItem::CodingRequest(request) => format!("?{}", request.request_id),
                other => other.timestamp().to_string(),
            })
            .collect()
    }

    #[test]
    fn reads_requests_for_a_chat() {
        let requests: Vec<ChatRequest> = serde_json::from_value(json!([{
            "requestId": "q1", "taskId": "t1", "kind": "approval", "question": "Can Codex push the branch?",
            "detail": "git push origin filo/x", "options": [], "tool": "Codex", "project": "filo", "taskTitle": "Fix it",
            "state": "answered", "answer": "allow_always", "decidedBy": "agent", "askedAt": 1.0, "createdAt": 0.5,
        }]))
        .unwrap();
        assert_eq!(requests[0].decided_by.as_deref(), Some("agent"));
        assert_eq!(requests[0].at(), 1.);
        assert_eq!(requests[0].tool_id(), "codex");
        assert_eq!(requests[0].option_notes, Vec::<String>::new());
        assert_eq!(record_lead(&requests, &[], "Filo"), "Filo answered Codex");
        assert_eq!(outcome(&requests[0], &[]), Outcome::Answered { by_user: false, answer: "Allowed always".into() });
        // Its tab says what it is; the command shows under the question.
        assert_eq!(
            (requests[0].label(1), requests[0].shown_detail()),
            ("Permission".to_string(), Some("git push origin filo/x"))
        );
    }

    #[test]
    fn options_read_with_what_they_mean_and_the_recommended_one() {
        let mut asked = request("q", 10., "pending");
        asked.detail = Some("Main default".into());
        asked.options =
            vec!["Oldest agent".into(), "Bring Filo back (Recommended)".into(), "Nobody until I pick".into()];
        asked.option_notes = vec!["The first agent you made".into(), "".into()];
        let picks = choices(&asked);
        assert_eq!(
            picks.iter().map(|choice| choice.label.as_str()).collect::<Vec<_>>(),
            ["Oldest agent", "Bring Filo back", "Nobody until I pick"]
        );
        assert_eq!(picks.iter().map(|choice| choice.recommended).collect::<Vec<_>>(), [false, true, false]);
        assert_eq!(
            (picks[0].note.as_str(), picks[1].note.as_str(), picks[2].note.as_str()),
            ("The first agent you made", "", "")
        );
        // What goes back is the tool's own label; it reads without the mark.
        assert_eq!(picks[1].answer, "Bring Filo back (Recommended)");
        assert_eq!(shown_answer(&asked, "Bring Filo back (Recommended)"), "Bring Filo back");
        assert_eq!(shown_answer(&asked, SKIPPED), "Skipped");
        // A short header names its tab and isn't repeated below; a long one shows.
        assert_eq!((asked.label(1), asked.shown_detail()), ("Main default".to_string(), None));
        asked.detail = Some("A much longer explanation than any tab could hold".into());
        assert_eq!(asked.label(2), "Question 2");
        assert!(asked.shown_detail().is_some());
        let approval = ChatRequest { kind: "approval".into(), options: Vec::new(), ..request("a", 10., "pending") };
        assert_eq!(
            choices(&approval).iter().map(|choice| choice.answer.as_str()).collect::<Vec<_>>(),
            ["allow", "allow_always", "deny"]
        );
    }

    #[test]
    fn an_answered_card_leads_with_a_short_title() {
        let mut permission =
            ChatRequest { kind: "approval".into(), options: Vec::new(), ..request("p", 10., "answered") };
        permission.question = "Run `cd /a/b; python3 x.py\nprint(1)`".into();
        permission.detail = Some("Rerun the  layout proof".into());
        assert_eq!(permission.title(), "Rerun the layout proof");
        assert_eq!(permission.command(), Some("cd /a/b; python3 x.py\nprint(1)"));
        // A long one is stored cut: it ends in "…" and is still its command.
        let mut cut = permission.clone();
        cut.question = filo_local::store::cut(&format!("Run `{}`", "make proof ".repeat(150)), 1_000);
        assert!(cut.question.ends_with('…'));
        assert!(cut.command().is_some_and(|command| command.starts_with("make proof make proof")));
        assert_eq!(cut.title(), "Rerun the layout proof");
        // A long note from the tool isn't a title.
        cut.detail = Some("because ".repeat(40));
        assert!(cut.title().starts_with("Run make proof"));
        // No description: its command, on one line.
        permission.detail = None;
        assert_eq!(permission.title(), "Run cd /a/b; python3 x.py print(1)");
        // Another kind of permission, or one put to the user in the agent's words, reads as asked.
        permission.question = "Change /etc/hosts".into();
        assert_eq!((permission.title(), permission.command()), ("Change /etc/hosts".to_string(), None));
        permission.question = "Let Claude Code delete Xcode's derived data?".into();
        permission.detail = Some("rm -rf ~/Library/Developer/Xcode/DerivedData".into());
        assert_eq!(permission.title(), "Let Claude Code delete Xcode's derived data?");
        assert_eq!(clip_lines("a  b\nc", 400., 12., 2), "a b c");
        let long = clip_lines(&"x".repeat(500), 400., 12., 2);
        assert!(long.ends_with('…') && long.chars().count() < 120);
        let mut question = request("q", 10., "answered");
        question.question = "Which **port** should it use?".into();
        assert_eq!(question.title(), "Which port should it use?");
    }

    #[test]
    fn the_line_says_they_wait_then_what_was_answered() {
        let (one, two) = (request("q1", 10., "pending"), request("q2", 10., "pending"));
        let both = vec![one.clone(), two.clone()];
        assert_eq!(record_lead(&both, &[], "Filo"), "Claude Code asked 2 questions");
        assert_eq!(record_lead(&both[..1], &[], "Filo"), "Claude Code asked a question");
        // The panel sent one: still waiting on the other.
        let sent = vec![("q1".to_string(), "Yes".to_string())];
        assert_eq!(record_lead(&both, &sent, "Filo"), "Claude Code asked 2 questions");
        let sent = vec![("q1".to_string(), "Yes".to_string()), ("q2".to_string(), SKIPPED.to_string())];
        assert_eq!(record_lead(&both, &sent, "Filo"), "You answered Claude Code");
        assert_eq!(outcome(&two, &sent), Outcome::Answered { by_user: true, answer: "Skipped".into() });
        let closed = vec![request("q3", 10., "expired")];
        assert_eq!(record_lead(&closed, &[], "Filo"), "Closed without an answer · Claude Code");
    }

    #[test]
    fn an_older_backend_without_times_still_places_cards() {
        let old: ChatRequest = serde_json::from_value(json!({
            "requestId": "q1", "taskId": "t1", "kind": "question", "question": "Keep it?", "tool": "Codex", "project": "filo",
            "state": "pending", "createdAt": 7.0,
        }))
        .unwrap();
        assert_eq!(old.at(), 7.);
        let none: ChatRequest = serde_json::from_value(json!({
            "requestId": "q2", "taskId": "t1", "kind": "question", "question": "Keep it?", "tool": "Codex", "project": "filo", "state": "pending",
        }))
        .unwrap();
        assert_eq!(none.at(), 0.);
        assert_eq!(none.options, Vec::<String>::new());
    }

    #[test]
    fn old_answered_cards_stay_above_newer_messages() {
        // A reported layout: two cards answered long ago, then Filo's newer reply, which belongs below them.
        let requests = ordered(vec![request("disk-2", 20., "answered"), request("disk-1", 10., "answered")]);
        let feed = place(vec![message("m-old", 5.), message("m-new", 100.)], &requests, None);
        assert_eq!(shape(&feed), ["m-old", "?disk-1", "?disk-2", "m-new"]);
    }

    #[test]
    fn answering_changes_a_card_where_it_is() {
        let before =
            place(vec![message("a", 5.), message("b", 30.)], &ordered(vec![request("q", 10., "pending")]), None);
        // Answered at 40 (after "b"): the backend's askedAt doesn't change, nor does its place.
        let after =
            place(vec![message("a", 5.), message("b", 30.)], &ordered(vec![request("q", 10., "answered")]), None);
        assert_eq!(shape(&before), shape(&after));
        assert_eq!(shape(&after), ["a", "?q", "b"]);
    }

    #[test]
    fn ties_break_by_when_the_tool_asked_then_id_never_by_state() {
        let mut late = request("b-late", 10., "pending");
        late.created_at = Some(9.);
        let mut early = request("c-early", 10., "answered");
        early.created_at = Some(8.);
        let same = request("a-same", 10., "answered");
        let mut same2 = request("a-same2", 10., "pending");
        same2.created_at = Some(10.);
        let order: Vec<String> =
            ordered(vec![late, same2, same, early]).into_iter().map(|request| request.request_id).collect();
        assert_eq!(order, ["c-early", "b-late", "a-same", "a-same2"]);
        // A message from the same millisecond comes first.
        let feed = place(vec![message("m", 10.)], &ordered(vec![request("q", 10., "pending")]), None);
        assert_eq!(shape(&feed), ["m", "?q"]);
    }

    #[test]
    fn a_replayed_or_reconnected_list_shows_each_card_once() {
        let stale = request("q", 10., "pending");
        let fresh = request("q", 10., "answered");
        let requests = ordered(vec![stale, request("r", 20., "pending"), fresh]);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].state, "answered");
        let feed = place(vec![message("m", 15.)], &requests, None);
        assert_eq!(shape(&feed), ["?q", "m", "?r"]);
    }

    #[test]
    fn cards_wait_for_their_page_of_history() {
        let requests = ordered(vec![request("ancient", 1., "answered"), request("recent", 60., "pending")]);
        // Only messages from 50 on are loaded: the card from 1 would sit above them, out of place.
        let feed = place(vec![message("m50", 50.), message("m70", 70.)], &requests, Some(50.));
        assert_eq!(shape(&feed), ["m50", "?recent", "m70"]);
        // The older page loads: it joins in its place.
        let feed = place(vec![message("m0", 0.), message("m50", 50.), message("m70", 70.)], &requests, None);
        assert_eq!(shape(&feed), ["m0", "?ancient", "m50", "?recent", "m70"]);
    }

    #[test]
    fn requests_arriving_before_the_messages_still_land_in_order() {
        // The requests come first; the chat is still empty.
        let requests = ordered(vec![request("q", 10., "pending")]);
        assert_eq!(shape(&place(Vec::new(), &requests, None)), ["?q"]);
        // Then the messages load around it.
        assert_eq!(shape(&place(vec![message("a", 5.), message("b", 15.)], &requests, None)), ["a", "?q", "b"]);
    }

    #[test]
    fn plain_text_shows_as_written() {
        assert_eq!(plain_markdown("rm -rf __pycache__/*.pyc"), "rm \\-rf \\_\\_pycache\\_\\_\\/\\*\\.pyc");
        assert_eq!(plain_markdown("<b>hi</b>\n  two"), "\\<b\\>hi\\<\\/b\\>\n\n\u{a0}\u{a0}two");
        // Two paths on two lines stay two lines (one paragraph each), blank lines dropped.
        assert_eq!(plain_markdown("/a/b\n\n/c"), "\\/a\\/b\n\n\\/c");
        assert_eq!(plain_markdown("# not a heading"), "\\# not a heading");
    }

    #[test]
    fn the_fixture_mirrors_the_screenshot() {
        let requests = ordered(fixture_requests(1_000_000_000.));
        let states: Vec<&str> = requests.iter().map(|request| request.state.as_str()).collect();
        assert_eq!(states, ["answered", "answered", "answered", "answered", "pending", "pending", "pending"]);
    }
}

// A coding session's thread (design/filo-coding-progress.html, design "B + C", `sessionThread`; `coding:thread`): the
// conversation between its owner (one of the user's agents, the main agent for one of no agent's) and Claude Code or
// Codex. The owner's brief, follow-ups and answers sit on the right (the side the user's own messages take in a chat),
// the tool's replies with the proof it sent (screenshots open in Quick Look) on the left; a run of bubbles from one side
// shows its picture once, at its foot. What the tool ran, changed and searched shows as quiet rows on its side, and
// markers (a note, an error, "Finished") in the middle. A task the user started from the Coding page ("You") takes a follow-up once a round is
// over, which resumes the tool's session; anyone else's is read-only (to change course the user tells the owner in
// the chat). It updates live while the tool works. Opened from the Coding page's tasks and from a chat's coding bar
// and rows (chat/coding_sessions.rs).

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Hsla, InteractiveElement as _, IntoElement, ListAlignment,
    ListState, ParentElement as _, Render, SharedString, Styled as _, Task, Window, div, list,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::text::TextView;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::avatars;
use crate::backend;
use crate::screens::agents::AgentRow;
use crate::screens::coding::{Proof, is_finished, mono, proof_row, proof_thumbnail, tool_name, tool_tile};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, Tone};

/// The pictures beside the bubbles, and the room they take.
const AVATAR: f32 = 28.;

#[derive(Clone, Debug, Deserialize)]
struct Thread {
    task: ThreadTask,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadTask {
    title: String,
    tool: String,
    status: String,
    /// Its agent's name (the main agent's for a task of no agent's), or "You".
    owner: String,
    repo_name: String,
    /// The computer it runs on.
    runner: String,
    branch: Option<String>,
    /// What it runs with: the model's name and the reasoning level's label ("High", or "Default").
    model: Option<String>,
    reasoning: Option<String>,
    /// The folder it works in on this Mac (the project, or its worktree).
    #[serde(default)]
    work_path: Option<String>,
}

/// Who a task started from the Coding page belongs to.
const YOU: &str = "You";

#[derive(Clone, Debug, Deserialize)]
struct Entry {
    key: String,
    /// "owner", "tool", "activity" or "marker".
    from: String,
    /// An activity's kind (command, files, plan, tool, search) or a marker's (note, error, status, monitoring).
    kind: Option<String>,
    text: String,
    /// A round's screenshots, recordings and output, on the tool's report.
    #[serde(default)]
    proof: Vec<Proof>,
}

pub struct CodingThreadScreen {
    task_id: String,
    thread: Option<Result<Option<Thread>, String>>,
    /// For the owner's picture: the agent's own.
    agents: Vec<AgentRow>,
    /// The entries, measured one by one like the chat's rows (a markdown bubble's height depends on its width), held to
    /// the bottom so the newest shows.
    list: ListState,
    /// What the list was last laid out for: entry keys and text lengths.
    laid_out: String,
    stopping: bool,
    /// The next message for the tool, on a task the user started, and whether it's on its way.
    follow_up: Entity<InputState>,
    sending: bool,
    _subs: Vec<Task<()>>,
    _inputs: Vec<gpui::Subscription>,
}

impl CodingThreadScreen {
    pub fn new(task_id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            backend::subscribe(
                cx,
                "coding:thread",
                json!({ "taskId": task_id }),
                |this: &mut Self, result: Result<Option<Thread>, String>, cx| {
                    let entries = result
                        .as_ref()
                        .ok()
                        .and_then(|thread| thread.as_ref())
                        .map_or(&[][..], |thread| &thread.entries[..]);
                    let shape: String = entries
                        .iter()
                        .map(|entry| format!("{}:{}:{};", entry.key, entry.text.len(), entry.proof.len()))
                        .collect();
                    if shape != this.laid_out {
                        let (old, new) = (this.list.item_count(), entries.len());
                        // New entries at the end keep the rest measured; anything else lays the whole thread out again.
                        if new > old && this.laid_out.len() <= shape.len() && shape.starts_with(&this.laid_out) {
                            this.list.splice(old..old, new - old);
                        } else {
                            this.list.reset(new);
                        }
                        this.laid_out = shape;
                    }
                    this.thread = Some(result);
                    cx.notify();
                },
            ),
            backend::subscribe(
                cx,
                "agents:list",
                json!({}),
                |this: &mut Self, result: Result<Vec<AgentRow>, String>, cx| {
                    if let Ok(agents) = result {
                        this.agents = agents;
                        cx.notify();
                    }
                },
            ),
        ];
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        let follow_up = cx.new(|cx| {
            InputState::new(window, cx).multi_line(true).auto_grow(1, 8).placeholder("Tell it what to do next")
        });
        let inputs = vec![cx.subscribe_in(&follow_up, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        CodingThreadScreen {
            task_id,
            thread: None,
            agents: Vec::new(),
            list,
            laid_out: String::new(),
            stopping: false,
            follow_up,
            sending: false,
            _subs: subs,
            _inputs: inputs,
        }
    }

    /// Sends the follow-up: the tool picks the task up again in its session.
    fn send_follow_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.follow_up.read(cx).value().trim().to_string();
        if message.is_empty() || self.sending {
            return;
        }
        self.sending = true;
        cx.notify();
        let args = json!({ "taskId": self.task_id, "message": message });
        backend::call_in(
            window,
            cx,
            "coding:continueTask",
            args,
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.sending = false;
                match result {
                    Ok(_) => this.follow_up.update(cx, |input, cx| input.set_value("", window, cx)),
                    Err(error) => ui::toast_error(window, cx, "Couldn't send it", error),
                }
                cx.notify();
            },
        );
    }

    /// Scrolls up `distance` from where the thread is (the memory soak in smoke.rs, to bring a report's proof into view).
    #[cfg(feature = "dev")]
    pub fn scroll_up(&self, distance: f32, cx: &mut Context<Self>) {
        self.list.scroll_by(px(-distance));
        cx.notify();
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        // A task in a plain folder (no git history) has no branch to keep.
        let branch = matches!(&self.thread, Some(Ok(Some(thread))) if thread.task.branch.is_some());
        let detail = if branch {
            "The tool stops where it is. Its branch and changes stay."
        } else {
            "The tool stops where it is. Its changes stay."
        };
        ui::confirm(window, cx, "Stop this task?", detail, "Stop", "Cancel", true, move |window, cx| {
            entity
                .update(cx, |this, cx| {
                    this.stopping = true;
                    cx.notify();
                    let args = json!({ "taskId": this.task_id });
                    backend::call_in(
                        window,
                        cx,
                        "coding:stopTask",
                        args,
                        |this: &mut Self, result: Result<Value, String>, window, cx| {
                            if let Err(error) = result {
                                ui::toast_error(window, cx, "Couldn't stop it", error);
                            }
                            this.stopping = false;
                            cx.notify();
                        },
                    );
                })
                .ok();
        });
    }

    /// One entry of the thread, padded into the page's column: a bubble (with its picture when it ends a run), a step or
    /// a marker.
    fn render_entry(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(Ok(Some(thread))) = &self.thread else { return div().into_any_element() };
        let Some(entry) = thread.entries.get(index) else { return div().into_any_element() };
        // A bubble shows its sender's picture when the next speaker differs (the tool's steps count as the tool's).
        let speaker = |entry: &Entry| match entry.from.as_str() {
            "owner" => Some("owner"),
            "tool" | "activity" => Some("tool"),
            _ => None,
        };
        let last_of_run = thread.entries.get(index + 1).and_then(speaker) != speaker(entry);
        let element = match entry.from.as_str() {
            "owner" => from_row(
                last_of_run.then(|| self.owner_face(&thread.task.owner, cx)),
                true,
                None,
                Some(bubble(&entry.key, &entry.text, true, window, cx)),
            ),
            "tool" => {
                let proof = (!entry.proof.is_empty()).then(|| proof_block(&entry.proof, cx));
                let text = (!entry.text.trim().is_empty()).then(|| bubble(&entry.key, &entry.text, false, window, cx));
                from_row(
                    last_of_run.then(|| tool_tile(&thread.task.tool, AVATAR, cx).into_any_element()),
                    false,
                    proof,
                    text,
                )
            }
            "activity" => activity_row(entry, cx),
            _ => marker_row(entry, &self.task_id, cx),
        };
        let last = index + 1 == thread.entries.len();
        // A list item's own margins don't centre it, so a full-width row centres the column.
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(
                ui::page_column()
                    .pt(px(if index == 0 { space::MD } else { 0. }))
                    .pb(px(if last { space::XL } else { space::SM + 2. }))
                    .child(element),
            )
            .into_any_element()
    }

    /// The owner's face: its picture, on a disc of its colour when it's an agent given one; the user's own tasks show a
    /// person.
    fn owner_face(&self, owner: &str, cx: &App) -> AnyElement {
        if owner == YOU {
            let colors = cx.colors();
            return div()
                .size(px(AVATAR))
                .rounded_full()
                .bg(colors.user_bubble)
                .flex()
                .items_center()
                .justify_center()
                .child(ui::icon_sized("user", 16., colors.on_user_bubble))
                .into_any_element();
        }
        let row = self.owner_row(owner);
        let picture = row.map_or_else(|| avatars::animal_image(avatars::DEFAULT_AVATAR), AgentRow::image);
        match row.and_then(|row| row.background.as_deref()) {
            Some(background) => {
                crate::screens::agents::agent_picture(picture, Some(background), AVATAR, cx).into_any_element()
            }
            None => avatars::picture(picture, AVATAR).into_any_element(),
        }
    }

    /// The owner: the agent by its name, else the main agent (a task of no agent's, or of one that's gone).
    fn owner_row(&self, owner: &str) -> Option<&AgentRow> {
        self.agents.iter().find(|agent| agent.name == owner).or_else(|| self.agents.iter().find(|agent| agent.main))
    }
}

/// "Opus 5.5 · High reasoning": the model and reasoning level the task runs with, whichever the backend sent.
fn runs_with(task: &ThreadTask) -> Option<String> {
    let reasoning = task.reasoning.as_ref().map(|level| format!("{level} reasoning"));
    let parts: Vec<String> = [task.model.clone(), reasoning].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A bubble with its sender's picture at its foot (the last of a run; the others keep its room): the tool's on the
/// left, the owner's on the right. Proof sits above the bubble, on its own.
fn from_row(
    avatar: Option<AnyElement>,
    mine: bool,
    proof: Option<AnyElement>,
    bubble: Option<AnyElement>,
) -> AnyElement {
    let avatar = div().flex_none().w(px(AVATAR)).mb(px(2.)).children(avatar);
    let content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .when(mine, |this| this.items_end())
        .when(!mine, |this| this.items_start())
        .gap(px(space::XS + 2.))
        .children(proof)
        .children(bubble);
    let row = div().flex().items_end().gap(px(space::SM));
    if mine { row.child(content).child(avatar) } else { row.child(avatar).child(content) }.into_any_element()
}

fn bubble(id: &str, text: &str, mine: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let colors = *cx.colors();
    let (fill, border, ink) = if mine {
        (colors.user_bubble, colors.user_bubble_border, colors.on_user_bubble)
    } else {
        (colors.assistant_bubble, colors.border, colors.on_assistant_bubble)
    };
    div()
        .max_w(relative(0.8))
        .min_w_0()
        .overflow_hidden()
        .px(px(12.))
        .py(px(7.))
        .rounded(px(16.))
        .bg(fill)
        .border_1()
        .border_color(border)
        .text_color(ink)
        .child(
            div().callout().min_w_0().child(
                TextView::markdown(SharedString::from(format!("coding-md-{id}")), text.to_string(), window, cx)
                    .style(ui::markdown_style())
                    .selectable(true),
            ),
        )
        .into_any_element()
}

/// A report's proof: screenshots as a small grid, then recordings and output as rows.
fn proof_block(proof: &[Proof], cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let (images, others): (Vec<&Proof>, Vec<&Proof>) = proof.iter().partition(|item| item.kind == "image");
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .when(!images.is_empty(), |this| {
            this.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(space::SM))
                    .children(images.into_iter().map(|item| proof_thumbnail(item, colors.chat_background, cx))),
            )
        })
        .children(others.into_iter().map(|item| {
            div()
                .w(px(320.))
                .p(px(space::SM))
                .rounded(px(radius::LG))
                .bg(colors.surface)
                .border_1()
                .border_color(colors.border)
                .child(proof_row(item, cx))
        }))
        .into_any_element()
}

/// What the tool did, as one quiet line under the bubbles' edge: a command, files, its plan, a search or a tool.
fn activity_row(entry: &Entry, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let kind = entry.kind.as_deref().unwrap_or("");
    let icon = match kind {
        "command" => "square-terminal",
        "files" => "pencil",
        "plan" => "list-todo",
        "search" => "search",
        _ => "workflow",
    };
    let line = entry.text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim().to_string();
    let text = if kind == "command" { mono(line, cx) } else { div().subhead().child(line) };
    div()
        .id(SharedString::from(format!("coding-activity-{}", entry.key)))
        .ml(px(AVATAR + space::SM))
        .max_w(relative(0.8))
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(space::SM + 2.))
        .child(ui::icon_sized(icon, 16., colors.text_tertiary))
        .child(text.flex_1().min_w_0().truncate().text_color(colors.text_secondary))
        .into_any_element()
}

/// A marker in the middle: a note, an error, or where the task ended up. A failure has Report
/// beside it (Report a problem, screens/report.rs).
fn marker_row(entry: &Entry, task_id: &str, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let failed = entry.kind.as_deref() == Some("error")
        || (entry.kind.as_deref() == Some("status") && entry.text.starts_with("Failed"));
    let icon = match entry.kind.as_deref() {
        _ if failed => "triangle-alert",
        // "Stopped" (crates coding.rs `finish_round`) has the Stop button's square, not Finished's check.
        Some("status") if entry.text.starts_with("Stopped") => "square",
        Some("status") => "circle-check",
        Some("monitoring") => "eye",
        _ => "info",
    };
    let color: Hsla = if failed { colors.danger } else { colors.text_tertiary };
    div()
        .py(px(space::XS))
        .flex()
        .justify_center()
        .child(
            div()
                .max_w(relative(0.86))
                .flex()
                .items_center()
                .gap(px(6.))
                .footnote()
                .medium()
                .text_color(color)
                .child(ui::icon_sized(icon, 14., color))
                .child(div().min_w_0().truncate().child(entry.text.clone()))
                .when(failed, |this| {
                    let origin =
                        crate::report::Origin::Task { task_id: task_id.to_string(), error: Some(entry.text.clone()) };
                    this.child(
                        Button::ghost(SharedString::from(format!("report-{}", entry.key)), "Report").small().on_click(
                            move |_, window, cx| {
                                crate::nav::push(crate::nav::Route::Report(origin.clone()), window, cx)
                            },
                        ),
                    )
                }),
        )
        .into_any_element()
}

impl Render for CodingThreadScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let thread = match &self.thread {
            Some(Ok(Some(thread))) => Some(thread.clone()),
            _ => None,
        };
        let Some(thread) = thread else {
            let body = match &self.thread {
                None => ui::skeleton_page(&[3, 2], None, cx).into_any_element(),
                Some(Err(error)) => ui::banner(error.clone(), Tone::Danger, cx).into_any_element(),
                Some(Ok(_)) => ui::empty_state("square-terminal", "This task isn't here anymore", None, None, cx)
                    .into_any_element(),
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(ui::sub_page_header("coding-thread", "Coding", Vec::new(), cx))
                .child(div().flex_1().min_h_0().child(ui::page_column().pt(px(space::LG)).child(body)));
        };
        let task = &thread.task;
        let tool = tool_name(&task.tool);

        let mut trailing = Vec::new();
        if let Some(path) = task.work_path.clone().filter(|path| std::path::Path::new(path).exists()) {
            trailing.push(
                Button::secondary("coding-thread-folder", "Show folder")
                    .small()
                    .icon("folder")
                    .on_click(move |_, _, cx| cx.reveal_path(std::path::Path::new(&path)))
                    .into_any_element(),
            );
        }
        if !is_finished(&task.status) {
            trailing.push(
                Button::danger("coding-thread-stop", "Stop")
                    .small()
                    .loading(self.stopping)
                    .on_click(cx.listener(|this, _, window, cx| this.stop(window, cx)))
                    .into_any_element(),
            );
        }
        let subtitle =
            if task.owner == YOU { task.title.clone() } else { format!("{}, with {}", task.title, task.owner) };
        let header = ui::sub_page_header_with("coding-thread", tool.clone(), Some(subtitle.into()), trailing, cx);
        let info = [Some(task.repo_name.clone()), task.branch.clone(), Some(task.runner.clone()), runs_with(task)]
            .into_iter()
            .flatten()
            .map(|part| div().min_w_0().truncate().child(part));

        // The repo, branch, Mac and model sit under the bar, above where the conversation fades out.
        let header = div().flex().flex_col().child(header).child(
            div()
                .flex_none()
                .px(px(space::LG))
                .py(px(space::XS))
                .flex()
                .justify_center()
                .gap(px(space::MD))
                .footnote()
                .text_color(colors.text_tertiary)
                .children(info),
        );
        let conversation = list(
            self.list.clone(),
            cx.processor(|this, index: usize, window, cx| this.render_entry(index, window, cx)),
        )
        .size_full();
        let footer =
            div().flex_none().px(px(space::XL)).py(px(space::MD)).border_t_1().border_color(colors.border.opacity(0.5));
        let footer = match (task.owner == YOU, is_finished(&task.status)) {
            // The user's own task, between rounds: what to do next.
            (true, true) => {
                let empty = self.follow_up.read(cx).value().trim().is_empty();
                footer.child(
                    ui::page_column()
                        .flex()
                        .items_end()
                        .gap(px(space::SM))
                        .child(div().flex_1().min_w_0().child(Input::new(&self.follow_up)))
                        .child(
                            Button::primary("coding-thread-send", "Send")
                                .small()
                                .loading(self.sending)
                                .disabled(empty)
                                .on_click(cx.listener(|this, _, window, cx| this.send_follow_up(window, cx))),
                        ),
                )
            }
            (true, false) => footer.footnote().text_color(colors.text_tertiary).text_center().child(format!(
                "{tool} is working on this Mac. Stop ends it where it is; you can send it more once it's done."
            )),
            (false, _) => footer.footnote().text_color(colors.text_tertiary).text_center().child(format!(
                "{owner}'s conversation with {tool}. To change course, tell {owner} in the chat.",
                owner = task.owner
            )),
        };
        ui::sub_page_on(header, conversation, colors.chat_background).child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_thread() {
        let thread: Thread = serde_json::from_value(json!({
            "task": {
                "taskId": "t1", "title": "Add a plus one button", "tool": "claude-code", "status": "done", "owner": "Filo",
                "sessionId": "s1", "repoName": "filo-proof-test", "repoPath": "/x", "runner": "Mac", "worktree": true,
                "branch": "filo/plus-one", "rounds": 1.0, "pending": 0.0, "createdAt": 1.0, "updatedAt": 2.0,
            },
            "now": "Done",
            "entries": [
                { "key": "brief", "at": 1.0, "from": "owner", "text": "Add a button." },
                { "key": "a1", "at": 2.0, "from": "activity", "kind": "command", "text": "ls -la" },
                { "key": "r1", "at": 3.0, "from": "tool", "text": "Done.", "proof": [
                    { "proofId": "p1", "round": 1.0, "kind": "image", "name": "after.png", "url": "https://x/y", "contentType": "image/png", "size": 2048.0 },
                ] },
                { "key": "m1", "at": 4.0, "from": "marker", "kind": "status", "text": "Finished" },
            ],
        }))
        .unwrap();
        assert_eq!(thread.task.branch.as_deref(), Some("filo/plus-one"));
        assert_eq!(runs_with(&thread.task), None);
        let mut task = thread.task.clone();
        task.model = Some("Opus 5.5".into());
        task.reasoning = Some("Extra high".into());
        assert_eq!(runs_with(&task).as_deref(), Some("Opus 5.5 · Extra high reasoning"));
        task.model = None;
        assert_eq!(runs_with(&task).as_deref(), Some("Extra high reasoning"));
        assert_eq!(thread.entries[2].proof[0].kind, "image");
        assert!(thread.entries[0].proof.is_empty());
    }
}

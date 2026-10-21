// Coding sessions in a chat (design/filo-coding-progress.html, design "B + C"; `coding:sessionsForChat`): each Claude
// Code or Codex session Filo or an agent started from this chat. The live ones join the header's work badge beside
// the user's agents (mod.rs), with a card each in its popover saying what the tool is doing now. Every session also
// sits in the timeline as a row where it started, with its state in two words. Rows and cards open the session's
// thread (screens/coding_thread.rs). Filo's reports stay in the chat as messages.

use std::time::Duration;

use gpui::{
    AnyElement, App, ClickEvent, Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, div, prelude::FluentBuilder as _, px, relative,
};
use serde::Deserialize;

use super::feed::FeedItem;
use crate::format;
use crate::nav::{self, Route};
use crate::screens::coding::{is_finished, named_tool_tile, status_label, task_heading, task_state, with_worked};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingSession {
    pub task_id: String,
    /// "claude-code" or "codex".
    pub tool: String,
    pub title: String,
    pub status: String,
    /// What it's doing now in a few words ("Running npm test"), or its status when it isn't running.
    pub now: String,
    /// Open questions.
    pub pending: f64,
    pub created_at: f64,
    /// A live task's computer: a queued one waits for it once it stops checking in, and the row names it on hover.
    pub computer: Option<Computer>,
    /// Where and with what it works, for the row's hover details; None from a backend that doesn't send them.
    pub repo_name: Option<String>,
    /// Its folder has no git history: a plain folder's icon, and no branch.
    #[serde(default)]
    pub plain_folder: bool,
    pub branch: Option<String>,
    pub model: Option<String>,
    /// While the tool runs: when its current request started (`coding_handoff::shown_request_start`). None while it
    /// doesn't run, or from a backend or round without it.
    pub request_started_at: Option<f64>,
    /// How long it has worked, in milliseconds (`coding_handoff::worked_ms`): "Done · 18m" once it's finished.
    pub worked_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Computer {
    pub name: String,
    pub last_seen_at: f64,
}

/// `RUNNER_ONLINE_MS`: a computer that hasn't checked in for this long is offline.
const RUNNER_ONLINE_MS: f64 = 2. * 60. * 1000.;

impl CodingSession {
    /// `codingSessionNow`: what it's doing, or that its computer has to come back online first.
    pub fn now_line(&self, now: f64) -> String {
        match &self.computer {
            Some(computer) if self.status == "queued" && now - computer.last_seen_at >= RUNNER_ONLINE_MS => {
                format!("Waiting for {} to come back online", computer.name)
            }
            _ => self.now.clone(),
        }
    }

    pub fn is_finished(&self) -> bool {
        is_finished(&self.status)
    }

    /// A question for the user is open.
    pub fn needs_you(&self) -> bool {
        self.status == "waiting_on_you" || self.pending > 0.
    }

    /// Its one state, the same on the badge, its popover, the docked panel, the timeline, its result cards and the
    /// Coding page (`task_state`): needing the user first; waiting while queued or while the tool's question is with
    /// its agent; monitoring while it waits on background work; working only while the tool runs.
    pub fn pill(&self) -> (&'static str, &'static str) {
        task_state(&self.status, self.needs_you())
    }

    /// The line under its state: why it waits, or what the tool is doing now, as the backend (or, for a computer gone
    /// quiet, the viewer's clock) says. None when the line would only repeat a state ("Queued", "Working", "Waiting on
    /// you") or is the backend's "Starting" for a running task that has logged no step yet: the state alone says that.
    pub fn detail(&self, now: f64) -> Option<String> {
        let line = self.now_line(now);
        let line = line.trim();
        let generic = line.is_empty()
            || line == "Starting"
            || line == self.pill().1
            // "Has a question" (waiting_on_agent's) stays: it's why the task waits.
            || ["queued", "running", "monitoring", "waiting_on_you", "done", "failed", "stopped"].iter().any(|status| line == status_label(status));
        (!generic).then(|| line.to_string())
    }

    /// When "Working · 4m" counts from: the start of the task's current request, from the backend
    /// (`coding_handoff::working_since`, as the Agents list and the pet's tray), so a follow-up the next morning starts
    /// at 0 rather than counting from the task's creation, a round carrying on after background work keeps counting,
    /// and redrawing or opening the chat again changes nothing. Only while it works: waiting, needing the user or being
    /// checked show no time, and a running task without the field (an older backend, or a round begun before it) shows
    /// "Working" alone, never a made-up time.
    pub fn since(&self) -> Option<f64> {
        if self.pill().1 != "Working" {
            return None;
        }
        self.request_started_at.filter(|at| *at > 0.)
    }

    /// What its row's hover details list under the full title, each with its icon: the project, the computer, the
    /// branch and the model, whichever the backend sent (nothing is guessed).
    pub fn facts(&self) -> Vec<(&'static str, String)> {
        let computer = self.computer.as_ref().map(|computer| computer.name.clone());
        let project = if self.plain_folder { "folder" } else { "folder-git-2" };
        [
            (project, self.repo_name.clone()),
            ("laptop", computer),
            ("git-branch", self.branch.clone()),
            ("model", self.model.clone()),
        ]
        .into_iter()
        .filter_map(|(icon, text)| {
            text.map(|text| (icon, text.trim().to_string())).filter(|(_, text)| !text.is_empty())
        })
        .collect()
    }

    /// Working or waiting on someone.
    pub fn is_live(&self) -> bool {
        !self.is_finished()
    }

    /// The state in two words at most, and its colour: a finished one's end, else `pill`'s words.
    fn state(&self, cx: &App) -> (&'static str, Hsla) {
        let colors = cx.colors();
        match self.status.as_str() {
            "done" => ("Done", colors.success),
            "failed" => ("Failed", colors.danger),
            "stopped" => ("Stopped", colors.text_secondary),
            _ if self.needs_you() => ("Needs you", colors.warning),
            _ => (self.pill().1, colors.text_secondary),
        }
    }
}

/// The feed with the sessions in it: each after the items before its start, sessions with nothing between them in one
/// group. `sessions` are oldest first. Sessions that started before `loaded_from` (the oldest loaded message, while
/// older ones are left to load) wait for their page: placed now, every one of them would pile up above the first
/// loaded message, whenever it ran.
pub(super) fn place(items: Vec<FeedItem>, sessions: &[CodingSession], loaded_from: Option<f64>) -> Vec<FeedItem> {
    let mut sessions =
        sessions.iter().filter(|session| loaded_from.is_none_or(|from| session.created_at >= from)).cloned().peekable();
    if sessions.peek().is_none() {
        return items;
    }
    let mut placed = Vec::with_capacity(items.len() + 1);
    for item in items {
        let mut group = Vec::new();
        while let Some(session) = sessions.next_if(|session| session.created_at < item.timestamp()) {
            group.push(session);
        }
        if !group.is_empty() {
            placed.push(FeedItem::Coding(group));
        }
        placed.push(item);
    }
    let rest: Vec<CodingSession> = sessions.collect();
    if !rest.is_empty() {
        placed.push(FeedItem::Coding(rest));
    }
    placed
}

/// Wakes the view every half minute while a session works, so the bar's "6 min" keeps up.
pub(super) fn ticker<V: 'static>(cx: &mut Context<V>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(30)).await;
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
}

fn open(task_id: String) -> impl Fn(&ClickEvent, &mut gpui::Window, &mut App) + 'static {
    move |_, window, cx| nav::push(Route::CodingTask(task_id.clone()), window, cx)
}

/// The sessions still live, for the header's work badge and its popover (mod.rs).
pub(super) fn live(sessions: &[CodingSession]) -> Vec<CodingSession> {
    sessions.iter().filter(|session| session.is_live()).cloned().collect()
}

/// Sessions in the timeline, one row each: the tool's mark (its name on hover), the task, and under it its state with
/// how long a live one has run ("Working · 4m"). A row opens its thread.
pub(super) fn render_rows(sessions: &[CodingSession], cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let now = format::now_ms();
    div()
        .pt(px(space::MD))
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .children(sessions.iter().map(|session| {
            let (state, color) = session.state(cx);
            let state = match session.is_live() {
                true => super::working_panel::status_time(state, session.since(), now),
                false => with_worked(state, &session.status, session.worked_ms),
            };
            div()
                .id(SharedString::from(format!("coding-row-{}", session.task_id)))
                .w(relative(0.86))
                .flex()
                .items_center()
                .gap(px(space::MD))
                .px(px(space::MD + 2.))
                .py(px(space::MD))
                .rounded(px(radius::LG))
                .bg(colors.surface)
                .border_1()
                .border_color(colors.border)
                .cursor_pointer()
                .hover(|style| style.bg(colors.surface_strong))
                .child(named_tool_tile(format!("coding-row-tool-{}", session.task_id), &session.tool, 32., cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().callout().semibold().truncate().child(task_heading(&session.title, &session.tool)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(space::XS))
                                .when(session.status == "monitoring", |this| {
                                    this.child(ui::icon_sized("eye", 13., color))
                                })
                                .child(div().min_w_0().footnote().medium().text_color(color).truncate().child(state)),
                        ),
                )
                .child(ui::icon_sized("chevron-right", 16., colors.text_tertiary))
                .on_click(open(session.task_id.clone()))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(task_id: &str, created_at: f64) -> CodingSession {
        CodingSession {
            task_id: task_id.into(),
            tool: "codex".into(),
            title: "Fix it".into(),
            status: "running".into(),
            now: "Running npm test".into(),
            pending: 0.,
            created_at,
            computer: None,
            repo_name: None,
            plain_folder: false,
            branch: None,
            model: None,
            request_started_at: None,
            worked_ms: None,
        }
    }

    #[test]
    fn a_queued_session_waits_for_its_computer_once_it_goes_quiet() {
        let queued = CodingSession {
            status: "queued".into(),
            now: "Waiting for the computer".into(),
            computer: Some(Computer { name: "Sam's MacBook".into(), last_seen_at: 1_000. }),
            ..session("t1", 0.)
        };
        assert_eq!(queued.now_line(1_000. + 60_000.), "Waiting for the computer");
        assert_eq!(queued.now_line(1_000. + 3. * 60_000.), "Waiting for Sam's MacBook to come back online");
        assert_eq!(session("t2", 0.).now_line(1e12), "Running npm test");
    }

    #[test]
    fn reads_sessions_for_a_chat() {
        let sessions: Vec<CodingSession> = serde_json::from_value(json!([{
            "taskId": "t1", "tool": "claude-code", "title": "Add a plus one button", "status": "stopped", "now": "Stopped",
            "pending": 0.0, "createdAt": 1.0, "updatedAt": 2.0,
        }]))
        .unwrap();
        assert!(sessions[0].is_finished());
    }

    #[test]
    fn places_sessions_after_what_came_before_them() {
        let call = |at: f64| FeedItem::Message {
            message: serde_json::from_value(json!({ "_id": format!("m{at}"), "_creationTime": at, "role": "user" }))
                .unwrap(),
        };
        let placed =
            place(vec![call(10.), call(30.)], &[session("a", 20.), session("b", 25.), session("c", 40.)], None);
        assert_eq!(shape(&placed), ["10", "ab", "30", "c"]);
    }

    #[test]
    fn sessions_from_before_the_loaded_page_wait_for_it() {
        let call = |at: f64| FeedItem::Message {
            message: serde_json::from_value(json!({ "_id": format!("m{at}"), "_creationTime": at, "role": "user" }))
                .unwrap(),
        };
        let sessions = [session("a", 2.), session("b", 5.), session("c", 20.), session("d", 40.)];
        let placed = place(vec![call(10.), call(30.)], &sessions, Some(10.));
        assert_eq!(shape(&placed), ["10", "c", "30", "d"]);
        let older = place(vec![call(1.), call(10.), call(30.)], &sessions, Some(1.));
        assert_eq!(shape(&older), ["1", "ab", "10", "c", "30", "d"]);
        assert_eq!(shape(&place(vec![call(10.)], &sessions[..2], Some(10.))), ["10"]);
    }

    fn shape(placed: &[FeedItem]) -> Vec<String> {
        placed
            .iter()
            .map(|item| match item {
                FeedItem::Coding(group) => {
                    group.iter().map(|session| session.task_id.clone()).collect::<Vec<_>>().join("")
                }
                other => other.timestamp().to_string(),
            })
            .collect()
    }
}

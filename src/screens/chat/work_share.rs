// A running coding task a message points at, dragged from the Working panel or popover (working_panel.rs) onto the
// composer, or picked after "@" (mention_suggestions.rs). Like a shared note (note_share.rs) it waits above the
// composer as a chip with an ×, and goes with the next message as its kind and id (`sharedWork` on
// `voiceSessions:sendMessage` and the voice-message mutations; local_backend.rs `message_context`):
// Filo reads the task's name and id, the bubble doesn't show them. Pointing sends nothing by itself, and what's typed
// stays.

use gpui::{App, Context, IntoElement, ParentElement as _, Render, Styled as _, Window, div, px};
use serde_json::{Value, json};

use super::working_panel::{PanelRow, Target};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};

/// Tasks one message points at (the backend's limit).
pub const MAX_WORK: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct SharedWork {
    /// The coding task's id.
    pub id: String,
    /// The task, which the chip says.
    pub title: String,
    /// Which tool works on it and its state, under the title in the "@" list.
    pub by: String,
    pub status: String,
    /// The tool ("claude-code", "codex"…), whose mark the chip shows.
    pub tool: String,
}

impl SharedWork {
    /// A Working row's task; any row is one (a question is its coding task's).
    pub fn from_row(row: &PanelRow) -> Self {
        let (Target::CodingTask(id) | Target::Question(id)) = &row.target;
        SharedWork {
            id: id.clone(),
            title: row.title.clone(),
            by: row.name.clone(),
            status: row.label.to_string(),
            tool: row.tool.clone(),
        }
    }

    pub fn key(&self) -> String {
        format!("coding-{}", self.id)
    }

    /// What the chip says: the task, else who works on it.
    pub fn label(&self) -> String {
        let title = self.title.trim();
        if title.is_empty() { self.by.clone() } else { title.to_string() }
    }

    /// Whether `typed` (after "@", lowercased) names it: the start of a word of its title, or of who works on it.
    pub fn matches(&self, typed: &str) -> bool {
        typed.is_empty()
            || [&self.title, &self.by]
                .iter()
                .any(|text| text.to_lowercase().split_whitespace().any(|word| word.starts_with(typed)))
    }

    fn json(&self) -> Value {
        json!({ "kind": "coding", "id": self.id })
    }
}

/// The tasks as the send mutations take them.
pub fn args(work: &[SharedWork]) -> Value {
    Value::Array(work.iter().map(SharedWork::json).collect())
}

/// `work` with `task` added (or brought up to date, when it's there already), or why it can't go with one message.
pub fn add(work: &[SharedWork], task: SharedWork) -> Result<Vec<SharedWork>, &'static str> {
    let key = task.key();
    let mut next: Vec<SharedWork> = work.iter().filter(|shared| shared.key() != key).cloned().collect();
    next.push(task);
    if next.len() > MAX_WORK {
        return Err("Up to 5 tasks go with one message.");
    }
    Ok(next)
}

/// A chip's face and words, without its ×: the task's tool and its title.
pub fn chip(work: &SharedWork, cx: &App) -> gpui::Div {
    let colors = *cx.colors();
    div()
        .max_w(px(280.))
        .h(px(30.))
        .pl(px(space::XS + 2.))
        .pr(px(space::XXS))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .rounded(px(radius::FULL))
        .bg(colors.surface_strong)
        .border_1()
        .border_color(colors.border)
        .child(div().flex_none().child(crate::screens::coding::tool_tile(&work.tool, 18., cx)))
        .child(div().min_w_0().footnote().truncate().child(work.label()))
}

/// What a Working row carries while it's dragged, drawn under the pointer as its chip.
#[derive(Clone, Debug)]
pub struct DraggedWork(pub SharedWork);

impl Render for DraggedWork {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        chip(&self.0, cx).pr(px(space::SM)).shadow_lg().bg(colors.raised)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(id: &str, title: &str) -> SharedWork {
        SharedWork {
            id: id.into(),
            title: title.into(),
            by: "Claude Code".into(),
            status: "Working".into(),
            tool: "claude-code".into(),
        }
    }

    #[test]
    fn a_task_pointed_at_again_is_not_doubled() {
        let one = add(&[], work("a", "Old")).unwrap();
        let both = add(&one, work("b", "Other")).unwrap();
        assert_eq!(both.len(), 2);
        let again = add(&both, work("a", "New")).unwrap();
        assert_eq!(again.iter().map(|w| w.title.as_str()).collect::<Vec<_>>(), ["Other", "New"]);
    }

    #[test]
    fn at_most_five_tasks() {
        let five = ["a", "b", "c", "d", "e"].into_iter().fold(Vec::new(), |all, id| add(&all, work(id, id)).unwrap());
        assert!(add(&five, work("f", "f")).is_err());
        assert!(add(&five, work("a", "a")).is_ok());
    }

    #[test]
    fn goes_as_its_kind_and_id() {
        assert_eq!(
            args(&[work("j1", "Fix"), work("k2", "Flights")]),
            json!([{ "kind": "coding", "id": "j1" }, { "kind": "coding", "id": "k2" }])
        );
    }

    #[test]
    fn at_finds_it_by_a_word_of_its_title_or_who_works() {
        let task = work("a", "Fix the login page");
        assert!(task.matches(""));
        assert!(task.matches("log"));
        assert!(task.matches("claude"));
        assert!(!task.matches("ogin"));
    }

    #[test]
    fn a_row_becomes_its_task() {
        let row = |target: Target| PanelRow {
            key: "k".into(),
            tool: "codex".into(),
            name: "Codex".into(),
            title: "Fix the login page".into(),
            detail: None,
            since: None,
            state: "active",
            label: "Working",
            target,
            facts: Vec::new(),
        };
        let task = SharedWork::from_row(&row(Target::CodingTask("c1".into())));
        assert_eq!((task.id.as_str(), task.tool.as_str(), task.key()), ("c1", "codex", "coding-c1".to_string()));
        let question = SharedWork::from_row(&row(Target::Question("c2".into())));
        assert_eq!(question.id, "c2");
    }
}

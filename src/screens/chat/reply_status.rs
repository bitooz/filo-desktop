// A chat's own reply under way, Filo's or an agent's. It's never a task in the chat's work badge, its popover or the
// docked panel (those are the tasks the chat started), and not a status in the chat's header: it shows where the chat
// list shows work, on the chat's row in the list beside it: the Agents list says "Working 12s" on an agent's row and
// the group's, worked out here. Nothing here
// subscribes to anything new: the rows come from the companion's working chats (`voiceSessions:working`, the pet's
// tray), with the open conversation's own turns for the chat on screen.

use gpui::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div, px};

use super::conversation::Conversation;
use crate::companion::{Opens, Working};
use crate::theme::{ActiveTheme as _, Typography as _, space};

/// The reply under way in `conversation`: None while nothing is being written; else since when (ms), when known.
pub fn reply_since(conversation: &Conversation) -> Option<Option<f64>> {
    conversation.replying().then(|| conversation.reply_started_at())
}

/// Whether the chat behind an Agents list row is working, and since when: the agent's reply
/// or work the chat started, from the earliest. `agent` is the row's agent id (or `agents::GROUP`); `tray` the
/// companion's working chats, matched by that id, never by a name; `reply` the open conversation's own reply
/// (`reply_since`), which shows as soon as it starts. None while nothing works there.
pub fn list_working(agent: &str, tray: &[Working], reply: Option<Option<f64>>) -> Option<Option<f64>> {
    // An agent can work in more than one chat (its own, and the group for a task from before): from the earliest.
    let tray = tray
        .iter()
        .filter(|chat| matches!(&chat.opens, Opens::Agent(id) if id == agent))
        .map(|chat| chat.since)
        .reduce(|a, b| match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (known, None) | (None, known) => known,
        });
    match (tray, reply) {
        (None, None) => None,
        (Some(since), None) | (None, Some(since)) => Some(since),
        (Some(a), Some(b)) => Some(a.into_iter().chain(b).reduce(f64::min)),
    }
}

/// A working row's words: "Working 12s", "Working 4m"; "Working" when the start isn't known.
pub fn list_text(since: Option<f64>, now: f64) -> String {
    match crate::format::working_for(since, now) {
        elapsed if elapsed.is_empty() => "Working".to_string(),
        elapsed => format!("Working {elapsed}"),
    }
}

/// The line under a working row's name: the turning ring and "Working 3m", in the link colour.
pub fn list_line(id: &str, since: Option<f64>, cx: &App) -> AnyElement {
    let colors = cx.colors();
    let line = div()
        .flex()
        .items_center()
        .gap(px(space::XS))
        .child(crate::ui::working_ring(13., colors.link))
        .child(div().caption().semibold().text_color(colors.link).child(list_text(since, crate::format::now_ms())));
    crate::smoke::tracked(format!("list-working-{id}"), line).into_any_element()
}

/// The line under a row whose last reply failed (the chat's red "Please retry" row): a warning and
/// "Reply failed" in the danger colour, until a reply there goes through (`replyFailed`). A retry under way shows as
/// working instead; stopped, the row says it failed again.
pub fn failed_line(cx: &App) -> AnyElement {
    let colors = cx.colors();
    div()
        .flex()
        .items_center()
        .gap(px(space::XS))
        .child(crate::ui::icon_sized("triangle-alert", 13., colors.danger))
        .child(div().caption().semibold().text_color(colors.danger).child("Reply failed"))
        .into_any_element()
}

/// An unanswered question or permission card takes priority over work and drafts.
pub fn needs_input_line(cx: &App) -> AnyElement {
    let colors = cx.colors();
    div()
        .flex()
        .items_center()
        .gap(px(space::XS))
        .child(crate::ui::icon_sized("message-circle", 13., colors.warning))
        .child(div().caption().semibold().text_color(colors.warning).child("Needs input"))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: f64 = 60_000.;

    fn tray(session: &str, agent: &str, since: f64) -> Working {
        Working {
            session_id: session.into(),
            title: "Desktop App Developer".into(),
            since: Some(since),
            opens: Opens::Agent(agent.into()),
        }
    }

    #[test]
    fn an_agents_row_works_while_its_chat_does() {
        let others = vec![
            tray("s-context", "agent-context", 9. * MINUTE),
            Working {
                session_id: "s-chat".into(),
                title: "Desktop App Developer".into(),
                since: Some(MINUTE),
                opens: Opens::Chat,
            },
        ];
        // Idle: nothing in the tray for this agent (a chat of Filo's named like it doesn't count), no reply here.
        assert_eq!(list_working("agent-dev", &others, None), None);
        // The reply starts in the open chat: the row says so before the tray hears of it.
        assert_eq!(list_working("agent-dev", &others, Some(Some(5. * MINUTE))), Some(Some(5. * MINUTE)));
        // Then the tray has it too: from the earliest.
        let mut working = others.clone();
        working.push(tray("s-dev", "agent-dev", 4. * MINUTE));
        assert_eq!(list_working("agent-dev", &working, Some(Some(5. * MINUTE))), Some(Some(4. * MINUTE)));
        // A row that isn't open still works from the tray alone; the other agent's row is its own.
        assert_eq!(list_working("agent-dev", &working, None), Some(Some(4. * MINUTE)));
        assert_eq!(list_working("agent-context", &working, None), Some(Some(9. * MINUTE)));
        // The reply's start not known yet: working, with no time rather than a wrong one.
        assert_eq!(list_working("agent-dev", &others, Some(None)), Some(None));
        // Done: the reply ended and the tray dropped the chat, so the row goes back to its last message.
        assert_eq!(list_working("agent-dev", &others, None), None);
        // The group's row by its id.
        assert_eq!(
            list_working(
                crate::screens::agents::GROUP,
                &[tray("s-group", crate::screens::agents::GROUP, MINUTE)],
                None
            ),
            Some(Some(MINUTE))
        );
    }

    #[test]
    fn an_agents_row_works_for_its_task_in_another_chat() {
        // Its coding task works in the group (started there before tasks went to its own chat), and its own chat
        // replies meanwhile: the row works, from the earlier of the two.
        let tray =
            [tray("coding-task-of:agent-dev", "agent-dev", 3. * MINUTE), tray("s-dev", "agent-dev", 6. * MINUTE)];
        assert_eq!(list_working("agent-dev", &tray, None), Some(Some(3. * MINUTE)));
        let unknown = Working { since: None, ..tray[1].clone() };
        assert_eq!(list_working("agent-dev", &[unknown.clone(), tray[0].clone()], None), Some(Some(3. * MINUTE)));
        assert_eq!(list_working("agent-dev", &[unknown], None), Some(None));
    }

    #[test]
    fn a_reply_is_the_lists_not_the_badges() {
        // An agent replying with no task of its own: its row in the list works, and its chat has no indicator (its
        // badge, popover and panel come from the chat's tasks only, never its turns).
        let tray = [tray("s-dev", "agent-dev", 2. * MINUTE)];
        assert_eq!(list_working("agent-dev", &tray, Some(Some(2. * MINUTE))), Some(Some(2. * MINUTE)));
        let here = super::super::working_panel::here_rows(&[]);
        assert_eq!(super::super::working_panel::status_text(&here), None);
    }

    #[test]
    fn a_working_row_says_it_as_the_chat_list_does() {
        assert_eq!(list_text(Some(0.), 12_000.), "Working 12s");
        assert_eq!(list_text(Some(0.), 4. * MINUTE), "Working 4m");
        assert_eq!(list_text(Some(0.), 65. * MINUTE), "Working 1h 5m");
        assert_eq!(list_text(None, 4. * MINUTE), "Working");
    }
}

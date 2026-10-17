// Reply shapes of the chat functions (local_backend.rs, local_backend/group.rs). Numbers are f64, times in
// milliseconds.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub session: Session,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    /// `_creationTime` of the newest message the user has seen.
    pub seen_until: Option<f64>,
    /// The chat's reasoning level (local_backend `EFFORT_ORDER`); none means the default, High.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// The levels its next turn's model takes, least first (local_backend `chat_efforts`); none when they aren't known.
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    /// The engine the user picked for this chat ("claude", "codex"); none follows the app's default engine.
    #[serde(default)]
    pub engine: Option<String>,
    /// The engine's model picked with it; none is the engine's own default.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_creationTime")]
    pub creation_time: f64,
    pub role: String,
    #[serde(default)]
    pub content: String,
    pub voice_recording: Option<VoiceRecording>,
    pub reaction: Option<String>,
    pub reply_to_message_id: Option<String>,
    /// The turn it's of (an agent's), or that it started (the user's).
    #[serde(default)]
    pub turn: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Who said it in the group chat of Filo and the agents: the agent's id.
    #[serde(default)]
    pub speaker: Option<String>,
    /// The coding round this message reports: it shows Open proof (coding_result.rs).
    #[serde(default)]
    pub coding_result: Option<CodingResult>,
    /// A glance Filo showed: the card's head; its page is fetched when the card shows (glance.rs).
    #[serde(default)]
    pub glance: Option<GlanceRef>,
    /// In an agent's own chat: a line pointing at the group, where it asked others or another member asked it, or
    /// what it posted there (`posted_in_group`).
    #[serde(default)]
    pub group_link: Option<GroupLink>,
    /// In an agent's own chat: a turn the group gave it, run there (local_backend/group.rs): a card with the ask.
    #[serde(default)]
    pub group_call: Option<GroupCall>,
    /// A routine's run coming due: one line in the chat instead of its instructions.
    #[serde(default)]
    pub routine: Option<RoutineMark>,
    /// In an agent's own chat: another agent asked it this from its chat (crates/filo-local `agent_asks`), shown with
    /// a chip opening that chat.
    #[serde(default)]
    pub asked_by: Option<AskedBy>,
    /// In the group: where it went off or on, or who joined or left, a line in the chat at that time ("You turned the
    /// group off"); not a message.
    #[serde(default)]
    pub mark: Option<String>,
    /// In the group: an agent's full report under its short post, opened as a page from its row (coding_result.rs).
    #[serde(default)]
    pub report: Option<String>,
}

/// Who asked an agent from its own chat (crates/filo-local store.rs `AskedBy`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AskedBy {
    /// The asking agent's id and name, and the line in its chat that says it asked.
    pub speaker: String,
    pub name: String,
    #[serde(default)]
    pub message_id: Option<String>,
}

/// Which routine a run's message is (local_backend.rs `routine_run`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RoutineMark {
    pub title: String,
    /// Its schedule in words ("every 30 minutes").
    #[serde(default)]
    pub when: String,
    /// It ended without sending anything: there was nothing to report.
    #[serde(default)]
    pub quiet: bool,
}

/// Where an agent's chat points at the group: it asked members there ("asked"), a member
/// asked it ("calledBy"), or it posted there in a turn the group gave it ("posted"), and the group message that was.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupLink {
    pub kind: String,
    #[serde(default)]
    pub who: Vec<GroupMember>,
    #[serde(default)]
    pub message_id: Option<String>,
    /// While the group is off, where the line points instead: the agent it asked ("asked") or that answered
    /// ("answered"), whose chat it opens at `message_id`; the ask's state ("working", "done", "failed") and how long
    /// it took.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub worked_ms: Option<f64>,
}

/// A turn the group gave an agent, run in its own chat (crates/filo-local store.rs `GroupCall`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroupCall {
    /// Who asked; none: the user.
    #[serde(default)]
    pub from: Option<GroupMember>,
    /// The group message that asked.
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub ask: String,
    /// The user waits for its answer there; not for a message of theirs to everyone, which it answers only if it's
    /// for it (crates/filo-local `group::checking`).
    #[serde(default)]
    pub answers_user: bool,
    /// The user wrote right after its message there, naming no one: it answers if it's for it.
    #[serde(default)]
    pub after_question: bool,
}

impl GroupCall {
    /// Its words: "Called by Release Manager in the group", "You asked in the group", "You wrote after its message in
    /// the group", or "You wrote to everyone in the group".
    pub fn label(&self) -> String {
        match &self.from {
            Some(from) => format!("Called by {} in the group", from.name),
            None if self.answers_user => "You asked in the group".into(),
            None if self.after_question => "You wrote after its message in the group".into(),
            None => "You wrote to everyone in the group".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GroupMember {
    pub speaker: String,
    pub name: String,
}

impl GroupLink {
    /// Its words: "Asked Filo", "Called by Context Engineer", "Posted in the group".
    pub fn label(&self) -> String {
        let names = self.who.iter().map(|member| member.name.as_str()).collect::<Vec<_>>().join(", ");
        match self.kind.as_str() {
            "asked" => format!("Asked {names}"),
            "answered" => format!("{names} answered"),
            "posted" => "Posted in the group".into(),
            _ => format!("Called by {names}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GlanceRef {
    pub glance_id: String,
    pub title: String,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// Asked to be as wide as the chat allows (`show_glance`'s `wide`).
    #[serde(default)]
    pub wide: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodingResult {
    pub task_id: String,
    pub round: f64,
    /// The task's title and tool ("claude-code", "codex"…), for a chat that doesn't list the task (the group).
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    /// How the round ended: "done", "failed", "stopped", or "monitoring" (its turn ended waiting on background work).
    #[serde(default)]
    pub status: Option<String>,
    /// The task's status now (coding.rs): whether it still works.
    #[serde(default)]
    pub task_status: Option<String>,
    /// How long the task has worked, in milliseconds (`coding_handoff::worked_ms`).
    #[serde(default)]
    pub worked_ms: Option<f64>,
}

impl Message {
    pub fn is_user(&self) -> bool {
        self.role == "user"
    }

    /// In an agent's own chat: the group message this is, when it's what the agent posted there (local_backend.rs
    /// `detail` sends the post itself). It reads in full here, but notifies and counts as unseen in the group only.
    pub fn posted_in_group(&self) -> Option<&GroupLink> {
        self.group_link.as_ref().filter(|link| link.kind == "posted")
    }

    /// Its words: a user's reply saved by this Mac's backend has what it answers quoted at its top
    /// (filo_local::store::with_quote), which the bubble's quote shows instead.
    pub fn own_content(&self) -> &str {
        self.reply_quote().map_or(&self.content, |(_, rest)| rest)
    }

    /// The quote at the top of a user's reply, as saved with it, and the words after it.
    pub fn reply_quote(&self) -> Option<(String, &str)> {
        self.reply_to_message_id.as_ref().filter(|_| self.is_user())?;
        filo_local::store::split_quote(&self.content)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VoiceRecording {
    pub duration_ms: f64,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub storage_id: String,
    pub name: String,
    pub content_type: String,
    pub size: f64,
    /// Where the file is on the computer it was sent from: the message's text holds this path where the chip was.
    #[serde(default)]
    pub local_path: Option<String>,
    /// This Mac's copy of the file (the local backend's saved one; a message still being sent, the file it was picked
    /// from): drawn and opened from disk.
    #[serde(default)]
    pub local_file: Option<std::path::PathBuf>,
}

/// An artifact a reply made in this chat (local_backend/artifacts.rs `chat_tasks`): its card, which opens it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub artifact: Option<ArtifactRef>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ArtifactRef {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub turn_id: String,
    /// The `requestId` of the send that made it (the message being sent finds its turn by it).
    pub request_id: String,
    /// When the triggering message was written, including a hidden handoff message.
    pub created_at: f64,
    pub user_message_id: Option<String>,
    /// "pending", "completed", "failed", or "settled": it failed, and a turn in the chat went through after, so its
    /// error is a quiet line with no Retry (crates/filo-local `turn_state`).
    pub state: String,
    pub error: Option<String>,
    /// A failure on a usage limit: when it's tried again on its own (ms; local_backend/limit_retry.rs).
    #[serde(default)]
    pub retry_at: Option<f64>,
    /// What the agent is doing right now while the turn runs ("Clicking in Helium"; crates/filo-local `chat_activity`).
    #[serde(default)]
    pub activity: Option<String>,
    /// The CLI compacted its session during this turn: from here it has a summary of the earlier conversation.
    #[serde(default)]
    pub compacted: bool,
    /// This turn started a fresh session, the one before having gone cold: from here it has the newest messages.
    #[serde(default)]
    pub reseeded: bool,
    /// In the group: the member answering in its own chat (its agent id), which the typing row opens.
    #[serde(default)]
    pub speaker: Option<String>,
    /// In the group: the post waits for the routing model, of this tool, to pick who answers.
    #[serde(default)]
    pub routing: Option<filo_local::Engine>,
}

impl ChatTurn {
    pub fn is_active(&self) -> bool {
        self.state == "pending"
    }
}

/// What the composer quotes when replying.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplyTarget {
    pub message_id: String,
    pub is_user: bool,
    /// Who wrote it, when not the user: Filo, the agent or a member of the group.
    pub author: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_call_says_how_the_user_reached_the_agent() {
        let label = |json: serde_json::Value| serde_json::from_value::<GroupCall>(json).unwrap().label();
        assert_eq!(label(serde_json::json!({ "ask": "Dinner?", "answersUser": true })), "You asked in the group");
        assert_eq!(
            label(serde_json::json!({ "ask": "yes", "afterQuestion": true })),
            "You wrote after its message in the group"
        );
        assert_eq!(label(serde_json::json!({ "ask": "standup?" })), "You wrote to everyone in the group");
        assert_eq!(
            label(serde_json::json!({ "ask": "?", "from": { "speaker": "chef", "name": "Chef" } })),
            "Called by Chef in the group"
        );
    }
}

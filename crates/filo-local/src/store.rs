// Local chats on disk.
//
//   <root>/chats/<chat id>/chat.json       title, engine and model, the engine's session (written by the app, replaced whole)
//   <root>/chats/<chat id>/messages.jsonl  one message per line, only ever appended
//   <root>/chats/<chat id>/reactions.json  the user's reaction to a message, by its id (the app's, replaced whole)
//   <root>/chats/<chat id>/moved.json      where an agent's files were found after they moved (replaced whole, by the app
//                                          or a turn's worker; one losing the other's write only means a lookup again)
//   <root>/workspace/                      the folder the CLI runs in (its sessions are kept per folder)
//   <root>/workspace/.claude/skills/<name>/           copies of the user's skill folders (skills.rs) for Claude Code, made
//                                                     by `write_skills`, with Filo's own (desktop-pet, and artifacts with
//                                                     its template)
//   <root>/workspace/.agents/skills/<name>/           the same for Codex, with Filo's dataviz
//   <root>/workspace/pages/<name>.html                pages agents build and publish as artifacts (artifacts.rs)
//
// Two processes write a chat: the app (the user's messages, errors, chat.json) and the MCP server the CLI started
// (replies). Neither rewrites the other's data: messages are appended with O_APPEND, one write per line, and only the
// app replaces chat.json (through a temporary file and a rename). A torn or unreadable line is skipped on read.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::now_ms;
use crate::skills::{self, Skill};

/// A chat title is the first message, cut to this many characters.
const TITLE_CHARS: usize = 48;
/// The chat list's preview of the last message.
const PREVIEW_CHARS: usize = 120;
/// How much of the message a reply answers its quote keeps.
const REPLY_QUOTE_CHARS: usize = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// Something went wrong with a turn (the CLI missing, signed out, it stopped without replying), kept in the chat
    /// so the history explains itself after a restart.
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub text: String,
    /// Milliseconds since the epoch.
    pub at: u64,
    /// The turn it belongs to: a user message starts one, replies sent during it carry its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    /// In an agent's own chat: a line pointing at the group chat, where the agent asked others or was asked (group.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_link: Option<GroupLink>,
    /// Files the user sent with it (attachments.rs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// For the engine only, not shown in the chat: what Filo's app tells the conversation's coding tool on its
    /// own (a coding task it handed off asks something or finished a round, coding_handoff.rs). A turn it starts may
    /// end without a reply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// A routine's run coming due (local_backend/scheduled.rs): a turn that may end without a reply when the routine
    /// says to stay quiet, and a one-line marker in the chat instead of the routine's instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<RoutineRun>,
    /// A check-in an agent planned for itself coming due (local_backend/scheduled.rs `run_wake`): shown in the chat
    /// like the user's message, but Filo's, so its turn may end without a reply when there's nothing new.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub check_in: bool,
    /// The message it answers: for the user's, the one picked with the chat's Reply, its text quoted at the top of this
    /// one's (`with_quote`); for an agent's, the message its turn answers (`Store::agent_message`), not quoted; for one
    /// from Filo's app (`hidden`), the message its turn's replies answer in its place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// In an agent's own chat: a turn the agents' group chat gave it, run here (group.rs): who asked what, and the
    /// group's latest messages it read. Its send_message posts in the group, leaving a line here (`in_group`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_call: Option<GroupCall>,
    /// What the agent posted in the group during such a turn: the group message's id. The chat shows it as one line
    /// linking to the post, not as a reply; its text is kept for the turn's count and the engine's context (turn.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_group: Option<String>,
    /// A voice message (the chat's mic): its recording, kept on this Mac. Its text is what was said, as transcribed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Voice>,
    /// In an agent's own chat: another agent asked it this from its chat with the user (agent_asks.rs), while the group
    /// is off. The chat shows the ask with a chip opening the asker's chat; the agent's answer goes back there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by: Option<AskedBy>,
}

/// Who asked an agent from its own chat (`Message::asked_by`), and what.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedBy {
    /// The asking agent's id and name.
    pub speaker: String,
    pub name: String,
    /// The asker's chat, and the line there that says it asked.
    pub chat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// What it asked, as the chat shows it (the message's text is what the engine reads).
    pub ask: String,
}

/// A voice message's recording: saved under the store's root like a sent file (attachments.rs, `storage_id` its name
/// there), and how long it is. The app writes the file just after the message (local_backend/voice.rs), so for a
/// moment it may not be there yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Voice {
    pub storage_id: String,
    pub duration_ms: u64,
}

/// How a check-in's message starts (local_backend/scheduled.rs `wake_message`).
pub const CHECK_IN: &str = "⏰ Check-in you planned for yourself: ";

/// Which routine a run's message is, for its line in the chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutineRun {
    pub title: String,
    /// Its schedule in words ("every 30 minutes").
    #[serde(default)]
    pub when: String,
}

impl Message {
    pub fn new(role: Role, text: impl Into<String>, turn: Option<String>) -> Self {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            role,
            text: text.into(),
            at: now_ms(),
            turn,
            group_link: None,
            attachments: Vec::new(),
            hidden: false,
            routine: None,
            check_in: false,
            reply_to: None,
            group_call: None,
            in_group: None,
            voice: None,
            asked_by: None,
        }
    }

    /// A message Filo's app sent on its own (a coding task's news: `hidden`; a routine's run; a check-in, also one saved
    /// before check-ins had their mark, read from the words `CHECK_IN` starts it with), not the user.
    pub fn from_app(&self) -> bool {
        self.hidden
            || self.routine.is_some()
            || self.check_in
            || (self.role == Role::User && self.text.starts_with(CHECK_IN))
    }

    /// Its own words: a reply's of the user's without the quote at its top.
    pub fn own_text(&self) -> &str {
        match &self.reply_to {
            Some(_) if self.role == Role::User => split_quote(&self.text).map_or(self.text.as_str(), |(_, rest)| rest),
            _ => &self.text,
        }
    }

    /// What a reply to it quotes: its own words, or the names of its files when it's only those.
    fn quotable(&self) -> String {
        let own = if self.role == Role::User { self.own_text() } else { self.text.as_str() };
        if own.trim().is_empty() { self.summary() } else { own.trim().to_string() }
    }

    /// Its text, or the names of its files when it's only those, or a routine run's routine (a chat's title and preview).
    pub fn summary(&self) -> String {
        if let Some(routine) = &self.routine {
            return format!("Routine · {}", routine.title);
        }
        if let Some(call) = &self.group_call {
            return call.ask.clone();
        }
        if let Some(asked) = &self.asked_by {
            return asked.ask.clone();
        }
        if let Some(link) = self.group_link.as_ref().filter(|link| link.kind == "answered") {
            let name = link.who.first().map_or("An agent", |who| who.name.as_str());
            return format!("{name} answered");
        }
        if self.own_text().trim().is_empty() && !self.attachments.is_empty() {
            return self.attachments.iter().map(|file| file.name.as_str()).collect::<Vec<_>>().join(", ");
        }
        self.own_text().to_string()
    }
}

/// A file sent with a message. The user's, as the composer uploaded it: a copy saved under the store's root
/// (attachments.rs), which the chat shows and the turn hands to the engine. An agent's (mcp.rs `send_message`'s
/// `files`): a picture or video as Filo's frozen copy (attachments.rs `sent`, `local_path` where the agent had it), any
/// other file where it is on this Mac, no copy (`storage_id` empty), with its `bookmark` and `inode`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub storage_id: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    /// The saved copy; an agent's file itself when it isn't copied.
    pub file: PathBuf,
    /// Where the user keeps it: the message's text has this path where the file's chip was. For an agent's copy, where
    /// the agent had the file when it sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// An agent's file's macOS bookmark (bookmark.rs, base64), which finds it when it's renamed or moved
    /// (attachments.rs `moved_to`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
    /// That file's inode when it was sent: a file its bookmark finds counts as it only with the same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
}

/// Where an agent's chat points at the group: it asked members there (`asked`), or a member asked it (`calledBy`),
/// and the group message that was. The app shows it as a link to the group (`groupLink`). With `agent`, it points at
/// that agent's own chat instead (agent_asks.rs, while the group is off): where this one asked it (`asked`), or what it
/// answered (`answered`), `message_id` being that chat's message, and `ask` the ask it's about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupLink {
    pub kind: String,
    pub who: Vec<LinkedMember>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<String>,
}

/// A turn the group gave an agent, run in its own chat (`Message::group_call`): the user message that starts it there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCall {
    /// Who the agent is in the group (its id): send_message posts in its name.
    #[serde(deserialize_with = "speaker")]
    pub speaker: String,
    /// Who asked: a member, or none for the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<LinkedMember>,
    /// The group message that asked, which the chat's card opens the group at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// What was asked, as the group has it.
    pub ask: String,
    /// The group's latest messages as the agent read them, one per line.
    #[serde(default)]
    pub context: String,
    /// The user is waiting for its answer in the group: ending without one is a failure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answers_user: bool,
    /// The user wrote right after the agent's question in the group, naming no one (group.rs `Turn::passes_on`): it
    /// answers if it's for it, else the message goes on to the others.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub after_question: bool,
    /// It only checks whether the message is for it and may stay quiet (group.rs `checking`): the chat shows it once
    /// its turn left something else here (`shown`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checking: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkedMember {
    #[serde(deserialize_with = "speaker")]
    pub speaker: String,
    pub name: String,
}

/// A group speaker as a chat file has it: the Filo agent's id, which older builds wrote as `group::OLD_FILO`.
fn speaker<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let speaker = String::deserialize(deserializer)?;
    Ok(if speaker == crate::group::OLD_FILO { crate::apps::FILO_AGENT.to_string() } else { speaker })
}

/// What the chat shows of `messages`: not the errors (its turns show them), not what's for the engine only (`hidden`),
/// and not a group call its agent only checks (`GroupCall::checking`) while its turn has left nothing else here (a post's
/// line, an error): one that stayed quiet leaves no card, nor moves its row in the Agents list.
pub fn shown(messages: Vec<Message>) -> Vec<Message> {
    let keep: Vec<bool> = {
        let shown = shows(&messages);
        messages.iter().map(shown).collect()
    };
    messages.into_iter().zip(keep).filter_map(|(message, keep)| keep.then_some(message)).collect()
}

/// `shown`, of messages read without copying them (`Store::shared_messages`).
pub fn shown_of(messages: &[Message]) -> Vec<&Message> {
    let shown = shows(messages);
    messages.iter().filter(|message| shown(message)).collect()
}

/// Whether the chat shows a message of `messages` (`shown`).
fn shows(messages: &[Message]) -> impl Fn(&Message) -> bool {
    let quiet: Vec<String> = messages
        .iter()
        .filter(|call| call.group_call.as_ref().is_some_and(|call| call.checking))
        .filter(|call| {
            !messages.iter().any(|other| other.id != call.id && other.turn.is_some() && other.turn == call.turn)
        })
        .map(|call| call.id.clone())
        .collect();
    move |message: &Message| message.role != Role::Error && !message.hidden && !quiet.contains(&message.id)
}

/// The CLI conversation a chat continues: the next turn resumes it, so the engine keeps its own context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub engine: Engine,
    pub id: String,
    /// The instructions the session has, as `engine::fingerprint` marks them: a resumed Codex session whose chat's
    /// instructions have changed since gets them again (none: from before this was kept, so it gets them once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// The model its last turn asked for ("" for the CLI's default; none: from before this was kept), so a turn on
    /// another knows the chat has switched (turn.rs `turn_window`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    /// Answers the next message. Changing it starts a new CLI session, which gets the chat so far as context.
    pub engine: Engine,
    /// The engine's model for this chat (`--model`, `-m`); none means the CLI's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The user picked this chat's engine and model (the composer's model picker). Otherwise each turn takes the
    /// engine the app answers with by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub engine_chosen: bool,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Session>,
    /// Who the engine is in this chat (an agent's persona and its instructions), in place of `engine::INSTRUCTIONS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// The turns during which the CLI compacted its session (summarized what came before to make room): the chat marks
    /// where, since from there the engine has a summary of the earlier conversation, not all of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compacted_turns: Vec<String>,
    /// The turns that started a fresh session because the one before went cold (turn.rs, reseed.rs): from there the
    /// engine has the chat's newest messages, not all of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reseeded_turns: Vec<String>,
    /// How hard the engine thinks here, as the user picked in the composer ("low", "medium", "high", "xhigh" or "max");
    /// none: as its tool does by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// The reasoning level the next turn's engine gets for `reasoning`: one its model takes, set by the app as the turn
    /// starts (turn.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// When a turn here last ended without an error (turn.rs): a failure from before no longer stands (turn_state.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<u64>,
}

/// A row of the chat list.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatSummary {
    pub chat: Chat,
    pub preview: Option<String>,
    /// The newest of the chat's own time and its last message's.
    pub active_at: u64,
}

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    /// Each chat's messages as last read (`messages`), shared by the store's clones.
    read: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<ReadSoFar>>>>>,
}

/// A messages file as far as it was read: its whole lines, parsed, up to `upto` bytes, and the bytes just before that
/// point, which tell a file that was only appended to (the same bytes there still) from one written anew.
#[derive(Debug, Default)]
struct ReadSoFar {
    upto: u64,
    tail: Vec<u8>,
    messages: Arc<Vec<Message>>,
}

/// How many bytes before where a file was read up to are kept to recognise it (`ReadSoFar::tail`).
const READ_TAIL: u64 = 64;

/// The messages in `text`, one per line; a torn or unreadable line is skipped.
fn parse_lines(text: &str) -> impl Iterator<Item = Message> + '_ {
    text.lines().filter(|line| !line.trim().is_empty()).filter_map(|line| serde_json::from_str(line).ok())
}

/// Ids name folders: only what `uuid` makes (letters, digits, dashes), so nothing can point outside the store.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn invalid(id: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("not a chat id: {id:?}"))
}

/// ~/.config/filo/local, or `FILO_LOCAL_DIR` in `dev` builds (the `filo-local` command's checks).
pub fn default_root() -> PathBuf {
    #[cfg(feature = "dev")]
    if let Some(dir) = std::env::var_os("FILO_LOCAL_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/filo/local")
}

impl Store {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into(), read: Arc::default() }
    }

    pub fn open_default() -> Self {
        Store::at(default_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the CLI runs, created if missing. Kept the same for every turn: both CLIs file sessions by folder.
    pub fn workspace(&self) -> io::Result<PathBuf> {
        let dir = self.root.join("workspace");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// The folder of Filo's skill `name` as Claude Code finds it (`write_skills`), with its files: what a coding task is
    /// pointed to for the artifacts skill's template (coding_handoff.rs).
    pub fn skill_folder(&self, name: &str) -> PathBuf {
        self.root.join("workspace").join(".claude/skills").join(name)
    }

    /// The user's skills (their folders, skills.rs) as real skills of the CLIs that run in the workspace: a copy of each
    /// whole folder in Claude Code's `.claude/skills` and Codex's `.agents/skills`, with Filo's own (`skills::FILO_SKILLS`,
    /// the artifacts one with its template beside it), plus Filo's dataviz for Codex (Claude Code has its own). Those
    /// folders are Filo's: whatever else is in them goes, so a skill removed goes from both. The user's folders are only
    /// read. Only a file that changed is written (through a temporary file and a rename, so a CLI starting meanwhile never
    /// reads half of one).
    pub fn write_skills(&self, skills: &[Skill]) -> io::Result<()> {
        let workspace = self.workspace()?;
        let mut folders: Vec<(String, SkillSource)> = skills
            .iter()
            .filter(|skill| !skill.name.is_empty() && skills::clean_name(&skill.name) == skill.name)
            .map(|skill| (skill.name.clone(), SkillSource::Folder(skill.folder.clone())))
            .collect();
        for (name, files) in skills::FILO_SKILLS {
            if !folders.iter().any(|(own, _)| own == name) {
                folders.push((
                    name.into(),
                    SkillSource::Files(files.iter().map(|(file, text)| (file.to_string(), text.to_string())).collect()),
                ));
            }
        }
        let mut codex = folders.clone();
        if !codex.iter().any(|(name, _)| name == "dataviz") {
            codex.push(("dataviz".into(), SkillSource::Files(vec![("SKILL.md".into(), skills::DATAVIZ.into())])));
        }
        write_skill_folders(&workspace.join(".claude/skills"), &folders)?;
        write_skill_folders(&workspace.join(".agents/skills"), &codex)
    }

    /// A chat's folder (its messages, its turns' locks and what they share), for an id that can be one.
    pub(crate) fn chat_dir(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(invalid(id));
        }
        Ok(self.root.join("chats").join(id))
    }

    pub fn create_chat(&self, engine: Engine) -> io::Result<Chat> {
        let now = now_ms();
        let chat = Chat {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            engine,
            model: None,
            engine_chosen: false,
            created_at: now,
            updated_at: now,
            session: None,
            instructions: None,
            compacted_turns: Vec::new(),
            reseeded_turns: Vec::new(),
            reasoning: None,
            effort: None,
            last_ok_at: None,
        };
        fs::create_dir_all(self.chat_dir(&chat.id)?)?;
        self.save_chat(&chat)?;
        Ok(chat)
    }

    pub fn chat(&self, id: &str) -> io::Result<Option<Chat>> {
        let path = self.chat_dir(id)?.join("chat.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn save_chat(&self, chat: &Chat) -> io::Result<()> {
        let dir = self.chat_dir(&chat.id)?;
        crate::files::write_atomic(&dir.join("chat.json"), &serde_json::to_vec_pretty(chat).map_err(io::Error::other)?)
    }

    /// Changes a chat read fresh from disk, so two writers don't undo each other's fields.
    pub fn update_chat(&self, id: &str, change: impl FnOnce(&mut Chat)) -> io::Result<Chat> {
        let mut chat =
            self.chat(id)?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no chat {id}")))?;
        change(&mut chat);
        chat.updated_at = now_ms();
        self.save_chat(&chat)?;
        Ok(chat)
    }

    pub fn delete_chat(&self, id: &str) -> io::Result<()> {
        match fs::remove_dir_all(self.chat_dir(id)?) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Every chat, the most recently active first. Unreadable chats are left out.
    pub fn list_chats(&self) -> Vec<ChatSummary> {
        let Ok(entries) = fs::read_dir(self.root.join("chats")) else { return Vec::new() };
        let mut rows: Vec<ChatSummary> = entries
            .flatten()
            .filter_map(|entry| {
                let id = entry.file_name().into_string().ok()?;
                let chat = self.chat(&id).ok()??;
                let last = self
                    .shared_messages(&id)
                    .ok()
                    .and_then(|messages| messages.iter().filter(|message| !message.hidden).next_back().cloned());
                let active_at = chat.updated_at.max(last.as_ref().map_or(0, |message| message.at));
                let preview = last.map(|message| preview(&message.summary(), PREVIEW_CHARS));
                Some(ChatSummary { chat, preview, active_at })
            })
            .collect();
        rows.sort_by(|a, b| b.active_at.cmp(&a.active_at).then_with(|| a.chat.id.cmp(&b.chat.id)));
        rows
    }

    /// The chat's messages file: its whole history, word for word, which its turns are told they can read (turn.rs).
    pub fn messages_file(&self, id: &str) -> io::Result<PathBuf> {
        Ok(self.chat_dir(id)?.join("messages.jsonl"))
    }

    pub fn messages(&self, id: &str) -> io::Result<Vec<Message>> {
        self.shared_messages(id).map(|messages| messages.to_vec())
    }

    /// `messages`, without copying them: the chat's queries read them many times a second while a turn runs. A file
    /// is parsed once; after that only the lines added to it since are (messages are only ever appended), and a file
    /// written anew (shorter, or other bytes where it was read up to) is read whole again.
    pub fn shared_messages(&self, id: &str) -> io::Result<Arc<Vec<Message>>> {
        let path = self.messages_file(id)?;
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.read.lock().unwrap().remove(&path);
                return Ok(Arc::default());
            }
            Err(error) => return Err(error),
        };
        let read = self.read.lock().unwrap().entry(path).or_default().clone();
        let mut read = read.lock().unwrap();
        let len = file.metadata()?.len();
        // Still the file that was read: the bytes before where it was read up to are the same.
        let kept = read.upto <= len && {
            let from = read.upto - read.tail.len() as u64;
            let mut tail = vec![0; read.tail.len()];
            file.seek(SeekFrom::Start(from)).and_then(|_| file.read_exact(&mut tail)).is_ok() && tail == read.tail
        };
        if !kept {
            *read = ReadSoFar::default();
        }
        let mut bytes = Vec::with_capacity((len - read.upto) as usize);
        file.seek(SeekFrom::Start(read.upto))?;
        file.read_to_end(&mut bytes)?;
        // Whole lines are kept; a line still being written is read again next time.
        let whole = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |end| end + 1);
        let (lines, rest) = bytes.split_at(whole);
        let lines = std::str::from_utf8(lines).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let rest = std::str::from_utf8(rest).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !lines.is_empty() {
            Arc::make_mut(&mut read.messages).extend(parse_lines(lines));
            read.upto += whole as u64;
            let mut tail = std::mem::take(&mut read.tail);
            tail.extend_from_slice(lines.as_bytes());
            read.tail = tail.split_off(tail.len().saturating_sub(READ_TAIL as usize));
        }
        if rest.trim().is_empty() {
            return Ok(read.messages.clone());
        }
        let mut messages = read.messages.to_vec();
        messages.extend(parse_lines(rest));
        Ok(Arc::new(messages))
    }

    /// The user's reactions to the chat's messages (the chat's React), by message id. Messages are only ever appended, so
    /// these are kept beside them; only the app writes them.
    pub fn reactions(&self, id: &str) -> HashMap<String, String> {
        let Ok(dir) = self.chat_dir(id) else { return HashMap::new() };
        fs::read(dir.join("reactions.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The user's reaction to message `message_id` of chat `id`, or none to take it back.
    pub fn set_reaction(&self, id: &str, message_id: &str, reaction: Option<&str>) -> io::Result<()> {
        let dir = self.chat_dir(id)?;
        let mut reactions = self.reactions(id);
        match reaction {
            Some(reaction) => reactions.insert(message_id.to_string(), reaction.to_string()),
            None => reactions.remove(message_id),
        };
        crate::files::write_atomic(
            &dir.join("reactions.json"),
            &serde_json::to_vec_pretty(&reactions).map_err(io::Error::other)?,
        )
    }

    /// Where an agent's files in chat `id` were found after they moved (`find_moved_files`): by message id, then the
    /// file's place in the message.
    fn moved_files(&self, id: &str) -> HashMap<String, BTreeMap<usize, PathBuf>> {
        let Ok(dir) = self.chat_dir(id) else { return HashMap::new() };
        fs::read(dir.join("moved.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    /// `messages` of chat `id` with each agent's file where it is now: where it was last found, else, once it's no
    /// longer there, where its bookmark finds it (attachments.rs `moved_to`), which is kept for next time. Messages are
    /// only ever appended, so that's kept beside them, as the reactions are.
    pub fn find_moved_files(&self, id: &str, messages: &mut [Message]) {
        if !messages.iter().flat_map(|message| &message.attachments).any(|file| file.bookmark.is_some()) {
            return;
        }
        let mut moved = self.moved_files(id);
        let mut found = false;
        for message in messages.iter_mut() {
            for (index, file) in message.attachments.iter_mut().enumerate() {
                if let Some(path) = moved.get(&message.id).and_then(|files| files.get(&index)) {
                    *file = crate::attachments::moved(file, path.clone());
                }
            }
            for index in crate::attachments::find_moved(&mut message.attachments) {
                moved.entry(message.id.clone()).or_default().insert(index, message.attachments[index].file.clone());
                found = true;
            }
        }
        if found
            && let Ok(dir) = self.chat_dir(id)
            && let Ok(bytes) = serde_json::to_vec_pretty(&moved)
        {
            let _ = crate::files::write_atomic(&dir.join("moved.json"), &bytes);
        }
    }

    /// Changes whenever a message is added (the messages file's size), for cheap polling while a turn runs.
    pub fn messages_version(&self, id: &str) -> u64 {
        self.chat_dir(id)
            .ok()
            .and_then(|dir| fs::metadata(dir.join("messages.jsonl")).ok())
            .map_or(0, |meta| meta.len())
    }

    /// Adds a message to a chat that exists. The first user message also names the chat.
    pub fn append(&self, chat_id: &str, message: &Message) -> io::Result<()> {
        let dir = self.chat_dir(chat_id)?;
        if !dir.join("chat.json").is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no chat {chat_id}")));
        }
        let path = dir.join("messages.jsonl");
        let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
        line.push('\n');
        // After a torn line (a writer killed mid-write), start a new one rather than joining it.
        if !ends_with_newline(&path) {
            line.insert(0, '\n');
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// The user's message, which names an untitled chat.
    pub fn add_user_message(&self, chat_id: &str, text: &str, turn: &str) -> io::Result<Message> {
        self.add_user_message_with(chat_id, text, turn, Vec::new())
    }

    /// A message from Filo's app for the engine only (`Message::hidden`), starting turn `turn`; the chat doesn't show it.
    /// The turn's replies answer `answers` instead (the message that asked for the coding task it tells of), if any.
    pub fn add_hidden_message(
        &self,
        chat_id: &str,
        text: &str,
        turn: &str,
        answers: Option<String>,
    ) -> io::Result<Message> {
        let mut message = Message::new(Role::User, text, Some(turn.to_string()));
        message.hidden = true;
        message.reply_to = answers;
        self.append(chat_id, &message)?;
        Ok(message)
    }

    /// An agent's message in turn `turn`, answering the message the turn answers (`Message::reply_to`): its latest from
    /// the user (or from Filo's app, `hidden`, the chat doesn't show; for one telling how a coding task's round ended,
    /// the message that asked the task for work, `shown_ask`, when it has one). A message the user sent while the turn
    /// ran, which its CLI took (steer.rs), is its latest. Appended by whoever made it.
    pub fn agent_message(&self, chat_id: &str, text: impl Into<String>, turn: Option<String>) -> Message {
        let mut message = Message::new(Role::Assistant, text, turn);
        if let Some(turn) = message.turn.as_deref() {
            let messages = self.messages(chat_id).unwrap_or_default();
            let steered = crate::steer::seen_into(&crate::steer::all(self, chat_id), turn);
            let of_turn = |asked: &Message| {
                asked
                    .turn
                    .as_deref()
                    .is_some_and(|asked| asked == turn || steered.iter().any(|steered| steered == asked))
            };
            let asked = messages.iter().rev().find(|asked| asked.role == Role::User && of_turn(asked));
            message.reply_to =
                asked.map(|asked| asked.reply_to.clone().filter(|_| asked.hidden).unwrap_or_else(|| asked.id.clone()));
        }
        message
    }

    /// Of the turns that asked a coding task for work (`turns`, latest first: coding_handoff.rs `asking_turns`), the
    /// latest the chat shows the asking message of: its id.
    pub fn shown_ask(&self, chat_id: &str, turns: &[String]) -> Option<String> {
        let shown = shown(self.messages(chat_id).unwrap_or_default());
        turns.iter().find_map(|turn| {
            shown
                .iter()
                .find(|asked| asked.role == Role::User && asked.turn.as_deref() == Some(turn.as_str()))
                .map(|asked| asked.id.clone())
        })
    }

    /// A check-in an agent planned coming due (`Message::check_in`), starting turn `turn`.
    pub fn add_check_in_message(&self, chat_id: &str, text: &str, turn: &str) -> io::Result<Message> {
        let mut message = Message::new(Role::User, text, Some(turn.to_string()));
        message.check_in = true;
        self.append(chat_id, &message)?;
        Ok(message)
    }

    /// A routine's run coming due (`Message::routine`), starting turn `turn`.
    pub fn add_routine_message(
        &self,
        chat_id: &str,
        text: &str,
        turn: &str,
        routine: RoutineRun,
    ) -> io::Result<Message> {
        let mut message = Message::new(Role::User, text, Some(turn.to_string()));
        message.routine = Some(routine);
        self.append(chat_id, &message)?;
        Ok(message)
    }

    /// The user's message with the files they sent; one of only files names the chat after them.
    pub fn add_user_message_with(
        &self,
        chat_id: &str,
        text: &str,
        turn: &str,
        attachments: Vec<Attachment>,
    ) -> io::Result<Message> {
        self.add_user_reply(chat_id, text, turn, attachments, None)
    }

    /// `add_user_message_with`, answering `reply_to` (a message of this chat): its text quoted at the top
    /// (`with_quote`), which is how the engine reads it.
    pub fn add_user_reply(
        &self,
        chat_id: &str,
        text: &str,
        turn: &str,
        attachments: Vec<Attachment>,
        reply_to: Option<&Message>,
    ) -> io::Result<Message> {
        self.add_user_voice(chat_id, text, turn, attachments, reply_to, None)
    }

    /// `add_user_reply` for a voice message: `text` is what was said, and `voice` its recording.
    pub fn add_user_voice(
        &self,
        chat_id: &str,
        text: &str,
        turn: &str,
        attachments: Vec<Attachment>,
        reply_to: Option<&Message>,
        voice: Option<Voice>,
    ) -> io::Result<Message> {
        let text = reply_to.map_or_else(|| text.to_string(), |target| with_quote(&target.quotable(), text));
        let mut message = Message::new(Role::User, text, Some(turn.to_string()));
        message.attachments = attachments;
        message.reply_to = reply_to.map(|target| target.id.clone());
        message.voice = voice;
        self.append(chat_id, &message)?;
        self.update_chat(chat_id, |chat| {
            if chat.title.trim().is_empty() {
                chat.title = title_from(&message.summary());
            }
        })?;
        Ok(message)
    }
}

/// Where a skill's copy comes from: the user's folder, or the files of one of Filo's own.
#[derive(Clone, Debug)]
enum SkillSource {
    Folder(PathBuf),
    Files(Vec<(String, String)>),
}

/// `dir` holding exactly `skills`, each as `<name>/` with what its source has, and nothing else.
fn write_skill_folders(dir: &Path, skills: &[(String, SkillSource)]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(skills::TEMP) && !skills.iter().any(|(own, _)| name == own.as_str()) {
            skills::remove_path(&entry.path());
        }
    }
    // One that can't be copied (its folder went meanwhile) doesn't keep the others from theirs.
    let mut failed = None;
    for (name, source) in skills {
        if let Err(error) = write_skill_folder(&dir.join(name), source) {
            failed.get_or_insert(error);
        }
    }
    failed.map_or(Ok(()), Err)
}

fn write_skill_folder(folder: &Path, source: &SkillSource) -> io::Result<()> {
    let files = match source {
        SkillSource::Folder(from) => return skills::mirror(from, folder),
        SkillSource::Files(files) => files,
    };
    fs::create_dir_all(folder)?;
    // A file the skill no longer has goes; Filo's half-written ones are another write's, about to be renamed.
    for entry in fs::read_dir(folder)?.flatten() {
        let file = entry.file_name();
        if !file.to_string_lossy().starts_with(skills::TEMP) && !files.iter().any(|(own, _)| file == own.as_str()) {
            skills::remove_path(&entry.path());
        }
    }
    for (file, text) in files {
        if !fs::read_to_string(folder.join(file)).is_ok_and(|old| old == *text) {
            skills::write_atomic(folder, file, text)?;
        }
    }
    Ok(())
}

/// An empty or missing file counts as ending with one.
fn ends_with_newline(path: &Path) -> bool {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = fs::File::open(path) else { return true };
    if file.seek(SeekFrom::End(-1)).is_err() {
        return true;
    }
    let mut last = [0u8];
    file.read_exact(&mut last).map_or(true, |_| last[0] == b'\n')
}

/// The first line of a message, cut to fit a chat title.
fn title_from(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("New chat");
    cut(line, TITLE_CHARS)
}

/// A message in one line (the chat list, notifications): its Markdown as plain text, `cut` to `max` characters.
pub fn preview(text: &str, max: usize) -> String {
    cut(&crate::markdown::plain_text(text), max)
}

/// A reply as saved and as the engine reads it: what it answers as a Markdown quote ("> " before each line, its
/// blank lines as ">", at most `REPLY_QUOTE_CHARS`), a blank line, then the user's words.
pub fn with_quote(quoted: &str, text: &str) -> String {
    let quoted = quoted.trim();
    let mut kept: String = quoted.chars().take(REPLY_QUOTE_CHARS).collect();
    if kept.len() < quoted.len() {
        kept = format!("{}…", kept.trim_end());
    }
    let lines: Vec<String> = kept
        .lines()
        .map(|line| if line.trim().is_empty() { ">".to_string() } else { format!("> {}", line.trim_end()) })
        .collect();
    let text = text.trim();
    if text.is_empty() { lines.join("\n") } else { format!("{}\n\n{text}", lines.join("\n")) }
}

/// A text that starts with a quote (`with_quote`), apart: the quote's lines without their "> ", and the rest.
pub fn split_quote(text: &str) -> Option<(String, &str)> {
    let mut quote = Vec::new();
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        let Some(quoted) = bare.strip_prefix('>') else { break };
        quote.push(quoted.strip_prefix(' ').unwrap_or(quoted));
        end += line.len();
    }
    if quote.is_empty() {
        return None;
    }
    let rest = &text[end..];
    // The quote ends at a blank line, or the text with it.
    let rest = match rest.split_once('\n') {
        _ if rest.is_empty() => rest,
        Some((blank, after)) if blank.trim().is_empty() => after,
        _ => return None,
    };
    Some((quote.join("\n"), rest))
}

/// At most `max` characters, on one line, with an ellipsis when cut.
pub fn cut(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("filo-local-test-{name}-{}", uuid::Uuid::new_v4()));
        Store::at(dir)
    }

    #[test]
    fn reactions_are_kept_beside_the_messages() {
        let store = temp_store("reactions");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let reply = Message::new(Role::Assistant, "Done", Some("t1".into()));
        store.append(&chat.id, &reply).unwrap();
        assert!(store.reactions(&chat.id).is_empty());
        store.set_reaction(&chat.id, &reply.id, Some("👍")).unwrap();
        store.set_reaction(&chat.id, &reply.id, Some("🔥")).unwrap();
        assert_eq!(store.reactions(&chat.id), HashMap::from([(reply.id.clone(), "🔥".to_string())]));
        // The messages stay as they were appended.
        assert_eq!(store.messages(&chat.id).unwrap(), vec![reply.clone()]);
        store.set_reaction(&chat.id, &reply.id, None).unwrap();
        assert!(store.reactions(&chat.id).is_empty());
        assert!(store.set_reaction("../x", &reply.id, Some("👍")).is_err());
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_reply_quotes_what_it_answers_on_top() {
        let store = temp_store("reply");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let answer = Message::new(Role::Assistant, "Two ways:\n\n- the bus\n- the train", Some("t1".into()));
        store.append(&chat.id, &answer).unwrap();
        let reply = store.add_user_reply(&chat.id, "the train, please", "t2", Vec::new(), Some(&answer)).unwrap();
        assert_eq!(reply.text, "> Two ways:\n>\n> - the bus\n> - the train\n\nthe train, please");
        assert_eq!(reply.reply_to.as_deref(), Some(answer.id.as_str()));
        assert_eq!(reply.own_text(), "the train, please");
        assert_eq!(reply.summary(), "the train, please");
        assert_eq!(store.messages(&chat.id).unwrap().last(), Some(&reply));
        // A reply to a reply quotes only its words.
        let again = store.add_user_reply(&chat.id, "or the bus", "t3", Vec::new(), Some(&reply)).unwrap();
        assert_eq!(again.text, "> the train, please\n\nor the bus");
        // Words the user quoted themselves stay theirs.
        let quoted = store.add_user_reply(&chat.id, "> the bus\n\nwhy?", "t4", Vec::new(), Some(&answer)).unwrap();
        assert_eq!(quoted.own_text(), "> the bus\n\nwhy?");
        assert_eq!(split_quote(&quoted.text).unwrap().0, "Two ways:\n\n- the bus\n- the train");
        // A long one is cut.
        let long = with_quote(&"word ".repeat(400), "ok");
        assert!(
            long.starts_with("> word") && long.ends_with("…\n\nok") && long.chars().count() < REPLY_QUOTE_CHARS + 10
        );
        assert_eq!(split_quote("> not a reply\nstraight on"), None);
        assert_eq!(split_quote("plain"), None);
    }

    #[test]
    fn an_agents_message_answers_its_turns_message() {
        let store = temp_store("agent-reply");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let asked = store.add_user_message(&chat.id, "Bus or train?", "t1").unwrap();
        let later = store.add_user_message(&chat.id, "And back?", "t2").unwrap();
        let answer = store.agent_message(&chat.id, "> The train\n\nis faster.", Some("t1".into()));
        assert_eq!(answer.reply_to.as_deref(), Some(asked.id.as_str()));
        // Its words are its own, a quote at their top included: only the user's replies have one put there.
        assert_eq!(
            (answer.own_text(), answer.summary().as_str()),
            ("> The train\n\nis faster.", "> The train\n\nis faster.")
        );
        assert_eq!(
            store.agent_message(&chat.id, "Same way.", Some("t2".into())).reply_to.as_deref(),
            Some(later.id.as_str())
        );
        // Filo's app starting the turn on its own (a coding task's news): that's what it answers.
        let news = store.add_hidden_message(&chat.id, "The task finished.", "t3", None).unwrap();
        assert_eq!(
            store.agent_message(&chat.id, "Done.", Some("t3".into())).reply_to.as_deref(),
            Some(news.id.as_str())
        );
        // A coding task's round ended: the message that asked for the task, the latest the chat shows (not the news).
        let turns = ["t9", "t3", "t1"].map(String::from);
        assert_eq!(store.shown_ask(&chat.id, &turns).as_deref(), Some(asked.id.as_str()));
        store.add_hidden_message(&chat.id, "The task finished.", "t4", store.shown_ask(&chat.id, &turns)).unwrap();
        assert_eq!(
            store.agent_message(&chat.id, "Done.", Some("t4".into())).reply_to.as_deref(),
            Some(asked.id.as_str())
        );
        assert_eq!(store.agent_message(&chat.id, "Hi.", None).reply_to, None);
        assert_eq!(store.agent_message(&chat.id, "Hi.", Some("t9".into())).reply_to, None);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_message_taken_into_a_running_turn_is_what_its_replies_answer_once_its_cli_has_it() {
        let store = temp_store("agent-reply-steered");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let asked = store.add_user_message(&chat.id, "Fill in the shoutouts", "t1").unwrap();
        let steered = store.add_user_message(&chat.id, "Don't send without my approval", "t2").unwrap();
        // Claimed by the running turn, not yet in its CLI: a reply still answers what the turn was for.
        assert!(crate::steer::claim(&store, &chat.id, "t2", "t1"));
        assert_eq!(
            store.agent_message(&chat.id, "Filled in two.", Some("t1".into())).reply_to.as_deref(),
            Some(asked.id.as_str())
        );
        crate::steer::seen(&store, &chat.id, "t2");
        assert_eq!(
            store.agent_message(&chat.id, "Holding them for you.", Some("t1".into())).reply_to.as_deref(),
            Some(steered.id.as_str())
        );
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_group_call_the_agent_only_checks_shows_once_its_turn_left_something() {
        let call = |turn: &str, checking: bool| {
            let mut message = Message::new(Role::User, "[From the group chat…]", Some(turn.into()));
            message.group_call = Some(GroupCall {
                speaker: "chef".into(),
                from: None,
                message_id: None,
                ask: "@everyone standup?".into(),
                context: String::new(),
                answers_user: false,
                after_question: false,
                checking,
            });
            message
        };
        let ids = |messages: Vec<Message>| {
            shown(messages).into_iter().map(|message| message.turn.unwrap_or_default()).collect::<Vec<_>>()
        };
        let said = Message::new(Role::User, "hi", Some("t0".into()));
        // Checking and quiet: nothing; still checking: nothing yet.
        assert_eq!(ids(vec![said.clone(), call("t1", true)]), ["t0"]);
        // It posted (a line linking to the post), or failed: the card shows, the error doesn't.
        let posted = Message::new(Role::Assistant, "That's me.", Some("t2".into()));
        let failed = Message::new(Role::Error, "Limit.", Some("t3".into()));
        assert_eq!(ids(vec![call("t2", true), posted, call("t3", true), failed]), ["t2", "t2", "t3"]);
        // One it doesn't only check (or kept before there was the mark) shows as it is.
        assert_eq!(ids(vec![call("t4", false)]), ["t4"]);
    }

    #[test]
    fn chats_round_trip_and_sort_by_activity() {
        let store = temp_store("round-trip");
        let first = store.create_chat(Engine::Claude).unwrap();
        let second = store.create_chat(Engine::Codex).unwrap();
        // The first chat's messages come at least a millisecond after the second chat, so it's the more recently
        // active one: in the same millisecond the two would tie, and the order fall to their random ids.
        while now_ms() <= second.updated_at {
            std::thread::yield_now();
        }
        store
            .add_user_message(
                &first.id,
                "  \nHello there, this is a long first message that names the chat\nsecond line",
                "t1",
            )
            .unwrap();
        store.append(&first.id, &Message::new(Role::Assistant, "Hi!", Some("t1".into()))).unwrap();

        let chat = store.chat(&first.id).unwrap().unwrap();
        assert_eq!(chat.title, "Hello there, this is a long first message that…");
        assert_eq!(chat.engine, Engine::Claude);
        let messages = store.messages(&first.id).unwrap();
        assert_eq!(
            messages.iter().map(|m| (m.role, m.text.as_str())).collect::<Vec<_>>(),
            [
                (Role::User, "  \nHello there, this is a long first message that names the chat\nsecond line"),
                (Role::Assistant, "Hi!")
            ]
        );

        let rows = store.list_chats();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].chat.id, first.id);
        assert_eq!(rows[0].preview.as_deref(), Some("Hi!"));
        assert_eq!(rows[1].chat.id, second.id);
        assert_eq!(rows[1].preview, None);

        // A fresh Store over the same folder (the app after a restart) reads the same thing.
        let reopened = Store::at(store.root());
        assert_eq!(reopened.messages(&first.id).unwrap(), messages);
        store.delete_chat(&first.id).unwrap();
        assert!(store.chat(&first.id).unwrap().is_none());
        assert_eq!(store.list_chats().len(), 1);
        fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn torn_lines_are_skipped_and_ids_are_checked() {
        let store = temp_store("torn");
        let chat = store.create_chat(Engine::Claude).unwrap();
        store.append(&chat.id, &Message::new(Role::User, "one", None)).unwrap();
        let path = store.root().join("chats").join(&chat.id).join("messages.jsonl");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"id\":\"half").unwrap();
        drop(file);
        assert_eq!(store.messages(&chat.id).unwrap().len(), 1);

        assert!(store.append("../escape", &Message::new(Role::User, "x", None)).is_err());
        assert!(store.chat("a/b").is_err());
        assert!(store.append("missing-chat", &Message::new(Role::User, "x", None)).is_err());
        let before = store.messages_version(&chat.id);
        store.append(&chat.id, &Message::new(Role::Assistant, "two", None)).unwrap();
        assert!(store.messages_version(&chat.id) > before);
        let texts: Vec<String> = store.messages(&chat.id).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(texts, ["one", "two"]);
        fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn messages_read_again_only_where_the_file_grew_match_a_whole_read() {
        let store = temp_store("read-so-far");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let path = store.messages_file(&chat.id).unwrap();
        // What a store that never read the file before finds: the whole file, parsed.
        let whole = |store: &Store| -> Vec<String> {
            Store::at(store.root()).messages(&chat.id).unwrap().into_iter().map(|m| m.text).collect()
        };
        let texts =
            |store: &Store| -> Vec<String> { store.messages(&chat.id).unwrap().into_iter().map(|m| m.text).collect() };
        assert!(texts(&store).is_empty());
        store.append(&chat.id, &Message::new(Role::User, "one", None)).unwrap();
        assert_eq!(texts(&store), ["one"]);
        // Another process (the MCP server) appends a reply, in two writes: half a line shows nothing until it's whole.
        let line = serde_json::to_string(&Message::new(Role::Assistant, "two", None)).unwrap() + "\n";
        let (first, second) = line.split_at(10);
        OpenOptions::new().append(true).open(&path).unwrap().write_all(first.as_bytes()).unwrap();
        assert_eq!(texts(&store), ["one"]);
        OpenOptions::new().append(true).open(&path).unwrap().write_all(second.as_bytes()).unwrap();
        assert_eq!(texts(&store), ["one", "two"]);
        assert_eq!(texts(&store), whole(&store));
        // A last line with no newline yet is read like a whole read reads it.
        let unended = serde_json::to_string(&Message::new(Role::Assistant, "three", None)).unwrap();
        OpenOptions::new().append(true).open(&path).unwrap().write_all(unended.as_bytes()).unwrap();
        assert_eq!(texts(&store), ["one", "two", "three"]);
        store.append(&chat.id, &Message::new(Role::Assistant, "four", None)).unwrap();
        assert_eq!(texts(&store), ["one", "two", "three", "four"]);
        assert_eq!(texts(&store), whole(&store));
        // Written anew, longer or shorter: read whole again.
        let rewrite = |texts: &[&str]| {
            let lines: Vec<String> = texts
                .iter()
                .map(|text| serde_json::to_string(&Message::new(Role::User, *text, None)).unwrap())
                .collect();
            fs::write(&path, lines.join("\n") + "\n").unwrap();
        };
        rewrite(&["a much longer first message than any before", "b", "c", "d", "e"]);
        assert_eq!(texts(&store), ["a much longer first message than any before", "b", "c", "d", "e"]);
        rewrite(&["x"]);
        assert_eq!(texts(&store), ["x"]);
        assert_eq!(*store.shared_messages(&chat.id).unwrap(), Store::at(store.root()).messages(&chat.id).unwrap());
        // Gone: none.
        fs::remove_file(&path).unwrap();
        assert!(texts(&store).is_empty());
        fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn skill_folders_are_copied_whole_for_both_clis_and_removed_with_them() {
        use std::os::unix::fs::PermissionsExt as _;
        let store = temp_store("skills");
        let dir = store.root().join(skills::DIR);
        skills::save(
            &dir,
            &skills::Fields {
                name: "weekly-report".into(),
                description: "When asked".into(),
                body: "1. Group.".into(),
            },
        )
        .unwrap();
        skills::save(
            &dir,
            &skills::Fields { name: "tone".into(), description: "When asked".into(), body: "Short.".into() },
        )
        .unwrap();
        // One installed from a repository, with its scripts, references and a binary file.
        skills::tests::repo_skill(&dir.join("web-demo-video"), "web-demo-video");
        let workspace = store.workspace().unwrap();
        let claude = workspace.join(".claude/skills");
        let codex = workspace.join(".agents/skills");
        let names = |dir: &Path| {
            let mut names: Vec<String> =
                fs::read_dir(dir).unwrap().flatten().map(|entry| entry.file_name().into_string().unwrap()).collect();
            names.sort();
            names
        };
        // Something that isn't Filo's in its folders goes.
        fs::create_dir_all(claude.join("stray")).unwrap();

        store.write_skills(&skills::list(&dir)).unwrap();
        assert_eq!(names(&claude), ["artifacts", "desktop-pet", "tone", "web-demo-video", "weekly-report"]);
        assert_eq!(names(&codex), ["artifacts", "dataviz", "desktop-pet", "tone", "web-demo-video", "weekly-report"]);
        // Filo's artifacts skill has its template and its check beside it, for both; the user's have all their files.
        for dir in [&claude, &codex] {
            assert_eq!(names(&dir.join("artifacts")), ["SKILL.md", "check_page.py", "template.html"]);
            assert_eq!(fs::read_to_string(dir.join("artifacts/template.html")).unwrap(), skills::ARTIFACTS_TEMPLATE);
            assert_eq!(names(&dir.join("web-demo-video")), ["SKILL.md", "references", "scripts"]);
            assert_eq!(names(&dir.join("web-demo-video/scripts")), ["parts.tar.gz", "record.sh"]);
            assert_eq!(fs::read_to_string(dir.join("web-demo-video/references/deep/notes.md")).unwrap(), "Notes.");
            assert_eq!(
                fs::metadata(dir.join("web-demo-video/scripts/record.sh")).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        assert!(
            skills::ARTIFACTS.starts_with("---\nname: artifacts\ndescription: ")
                && skills::ARTIFACTS_TEMPLATE.contains("<!--filo:head-->")
        );
        let file = fs::read_to_string(claude.join("weekly-report/SKILL.md")).unwrap();
        assert_eq!(file, "---\nname: weekly-report\ndescription: \"When asked\"\n---\n\n1. Group.\n");
        assert_eq!(fs::read_to_string(codex.join("weekly-report/SKILL.md")).unwrap(), file);
        assert_eq!(fs::read_to_string(codex.join("dataviz/SKILL.md")).unwrap(), skills::DATAVIZ);
        assert_eq!(fs::read_to_string(claude.join("desktop-pet/SKILL.md")).unwrap(), skills::DESKTOP_PET);

        // Edited: the copy changes and the folder's other files stay, through every sync; removed: its copies go from
        // both. A file put in a copy goes, as the folder hasn't it.
        skills::save(
            &dir,
            &skills::Fields {
                name: "web-demo-video".into(),
                description: "When recording".into(),
                body: "Record.".into(),
            },
        )
        .unwrap();
        skills::remove(&dir, "tone").unwrap();
        fs::write(claude.join("web-demo-video/scripts/extra.sh"), "x").unwrap();
        for _ in 0..2 {
            store.write_skills(&skills::list(&dir)).unwrap();
        }
        assert!(
            fs::read_to_string(codex.join("web-demo-video/SKILL.md"))
                .unwrap()
                .ends_with("license: MIT\nmetadata:\n  author: bitooz\n  version: \"1.2\"\n---\n\nRecord.\n")
        );
        assert_eq!(
            skills::read(&dir, "web-demo-video").unwrap().files,
            ["references/deep/notes.md", "scripts/parts.tar.gz", "scripts/record.sh"]
        );
        assert_eq!(names(&claude.join("web-demo-video/scripts")), ["parts.tar.gz", "record.sh"]);
        assert_eq!(names(&claude), ["artifacts", "desktop-pet", "web-demo-video", "weekly-report"]);
        assert_eq!(names(&codex), ["artifacts", "dataviz", "desktop-pet", "web-demo-video", "weekly-report"]);
        // A skill of the user's by the name of Filo's own takes its place.
        skills::save(
            &dir,
            &skills::Fields { name: "artifacts".into(), description: "Mine".into(), body: "My way.".into() },
        )
        .unwrap();
        store.write_skills(&skills::list(&dir)).unwrap();
        assert_eq!(names(&claude.join("artifacts")), ["SKILL.md"]);
        skills::remove(&dir, "artifacts").unwrap();
        // A file Filo's skill no longer has goes from its folder.
        fs::write(claude.join("artifacts/old-template.html"), "x").unwrap();
        store.write_skills(&[]).unwrap();
        assert_eq!(names(&claude), ["artifacts", "desktop-pet"]);
        assert_eq!(names(&codex), ["artifacts", "dataviz", "desktop-pet"]);
        assert_eq!(names(&claude.join("artifacts")), ["SKILL.md", "check_page.py", "template.html"]);
        // The user's folders are only read.
        assert_eq!(skills::list(&dir).len(), 2);
        fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_message_keeps_its_files() {
        let store = temp_store("files");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let file = Attachment {
            storage_id: "id.png".into(),
            name: "image1.png".into(),
            content_type: "image/png".into(),
            size: 74_752,
            file: store.root().join("attachments/id.png"),
            local_path: Some("/Users/me/shot.png".into()),
            bookmark: None,
            inode: None,
        };
        store.add_user_message_with(&chat.id, "", "t1", vec![file.clone()]).unwrap();
        let messages = Store::at(store.root()).messages(&chat.id).unwrap();
        assert_eq!(messages[0].attachments, vec![file]);
        assert_eq!(store.chat(&chat.id).unwrap().unwrap().title, "image1.png");
        assert_eq!(store.list_chats()[0].preview.as_deref(), Some("image1.png"));
        // A message from before files went with them reads as one without.
        let old: Message = serde_json::from_str(r#"{"id":"m","role":"user","text":"hi","at":1}"#).unwrap();
        assert!(old.attachments.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("attachments"));
        fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn an_agents_file_that_moved_is_found_and_kept_found() {
        let store = temp_store("moved-files");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let work = store.root().canonicalize().unwrap().join("work");
        fs::create_dir_all(&work).unwrap();
        let (chart, notes) = (work.join("chart.png"), work.join("notes.txt"));
        fs::write(&chart, b"\x89PNG\r\n\x1a\n").unwrap();
        fs::write(&notes, b"notes").unwrap();
        let mut sent = store.agent_message(&chat.id, "Here", None);
        sent.attachments =
            vec![crate::attachments::in_place(&chart).unwrap(), crate::attachments::in_place(&notes).unwrap()];
        store.append(&chat.id, &sent).unwrap();
        let shown = |store: &Store| {
            let mut messages = store.messages(&chat.id).unwrap();
            store.find_moved_files(&chat.id, &mut messages);
            messages.pop().unwrap().attachments.into_iter().map(|file| (file.name, file.file)).collect::<Vec<_>>()
        };
        // Renamed: shown under its new name and path, and kept there (moved.json), not found again each time.
        let renamed = work.join("chart v2.png");
        fs::rename(&chart, &renamed).unwrap();
        let expected = vec![("chart v2.png".to_string(), renamed.clone()), ("notes.txt".to_string(), notes.clone())];
        assert_eq!(shown(&store), expected);
        assert!(store.root().join("chats").join(&chat.id).join("moved.json").is_file());
        assert_eq!(shown(&Store::at(store.root())), expected);
        // Moved after that: found from where it was last.
        fs::create_dir_all(work.join("done")).unwrap();
        let moved = work.join("done/chart v2.png");
        fs::rename(&renamed, &moved).unwrap();
        assert_eq!(shown(&store)[0], ("chart v2.png".to_string(), moved.clone()));
        // The message itself is as it was sent.
        assert_eq!(store.messages(&chat.id).unwrap().pop().unwrap().attachments[0].file, chart);
        fs::remove_dir_all(store.root()).ok();
    }
}

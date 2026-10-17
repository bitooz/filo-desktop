// The group chat of Filo and the user's agents, in Filo's database (db.rs), and its turns.
//
// One group per Mac (`group_chat`), its messages (`group_messages`) and a queue of turns (`group_turns`). It's optional
// (`Settings`): off until the user sets it up in the Agents tab, with the agents they pick as members and the model that
// picks who answers (`switch`, `set_settings`); a group an older Filo made stays on with every agent, as it was. Where it
// went off and on, and who joined or left, are marks in its chat (`marks`). Only members get its posts (`members`).
//
// The user writes in it: whoever they name with @Name answers, a Reply goes to the member whose post it answers, and
// "@everyone" reaches every member at the same time (`Turn::at_once`), each answering only if it's for them and free to
// say nothing. A post naming no one goes to one quick call to the routing model (group_route.rs, `needs_route`), which
// reads it, the recent messages and what each member is busy with (`work`), and picks who answers, or nobody (`routed`);
// when that call can't be made, the user's main agent (main_agent.rs, who leads the group) takes it (`route`). A member
// asks the next speakers by name in its post's ask list (send_message's `ask`, `Asks`), so agents ask each other
// things; an @Name in a member's text is only a name and wakes no one. Each one asked takes a turn too, until nobody is
// asked or a thread reaches MAX_CHAIN turns. The app runs the queue one turn at a time, so each speaker reads what the
// others just said, except the turns of one message that asks several (the user's @Names or "@everyone", a member's
// ask list, the routing call's picks), which run together (local_backend/group.rs): each one asked by name is told who
// else was, to take its own part, and that the first one asked makes any change they'd share (`together`).
//
// A member's turn runs in its own chat (local_backend/group.rs): a message there with the ask and the group's latest
// messages (`own_chat_prompt`, `call_of`), answered on the chat's session as any message is. It posts with Filo's
// `send_message`, which the MCP server (mcp.rs, `--group-speaker`, `--own-chat`) adds here in the speaker's name,
// leaving a line in its chat that links to it; one that has nothing to add posts nothing. A speaker without a chat of its own (Filo without the
// Filo agent) runs on a fresh session instead (`run`), told who it is and who's in the group.
//
// From one of the user's other chats, Filo or an agent brings the others in with the MCP server's `ask_agents`
// (`ask`; while the group is off, or for an agent not in it, each one asked works in its own chat instead,
// agent_asks.rs): its message goes to the group in its name, whoever its ask list names answers there, and when nobody is left to speak
// the app gives the asker a turn back in the chat it asked from to pass the answer on (`group_asks`).
//
// The user can name the group and give it a picture (`look`, kept in `group_chat`); speakers are told its name.
//
// The user's message can come with files and pictures, saved as an agent's chat saves them (attachments.rs): the
// group shows them, and every member answering in its thread gets them as they would in their own chat
// (`thread_files`), and reads where they're saved in the group's latest messages (`transcript`).
//
// Members (Filo and the agents) are the app's agents (app_data.rs; `roster` reads them), so they come in as arguments.

use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use rusqlite::{OptionalExtension as _, Row, params};

use crate::apps::FILO_AGENT;
use crate::db::Db;
use crate::engine::{self, Engine, McpLaunch, Stopper, TurnSpec};
use crate::store::{Role, Store, cut};
use crate::turn::{Report, Runtime};

/// What older builds called the Filo agent in the group; it speaks by its own id now (`apps::FILO_AGENT`), as every agent
/// does (`move_old_speakers`, store.rs `speaker`).
pub(crate) const OLD_FILO: &str = "filo";
/// Turns one thread may take, so the group can't talk forever.
const MAX_CHAIN: i64 = 8;
/// What each speaker reads of the group before its turn.
const TRANSCRIPT_MESSAGES: usize = 30;
const TRANSCRIPT_CHARS: usize = 16_000;
/// One message's part of the transcript, at most.
const LINE_CHARS: usize = 1_500;
/// How much of the user's message a turn's reason quotes.
const QUOTE_CHARS: usize = 1_500;
/// A turn's reason, at most (coding_handoff.rs sizes a coding task's news in the group to fit).
pub const REASON_CHARS: usize = 4_000;
/// A message in the group, at most (a runaway tool call shouldn't fill the database).
const MAX_MESSAGE_CHARS: usize = 64 * 1024;

/// The tables (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE group_chat (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    session_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE group_messages (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    id      TEXT NOT NULL UNIQUE,
    role    TEXT NOT NULL,
    speaker TEXT,
    text    TEXT NOT NULL,
    at      INTEGER NOT NULL,
    thread  TEXT,
    turn_id TEXT
);
CREATE INDEX group_messages_by_thread ON group_messages (thread);
CREATE INDEX group_messages_by_turn ON group_messages (turn_id);
CREATE TABLE group_turns (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    id           TEXT NOT NULL UNIQUE,
    thread       TEXT NOT NULL,
    speaker      TEXT NOT NULL,
    chain        INTEGER NOT NULL,
    reason       TEXT NOT NULL,
    answers_user INTEGER NOT NULL DEFAULT 0,
    state        TEXT NOT NULL,
    error        TEXT,
    created_at   INTEGER NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER
);
CREATE INDEX group_turns_by_state ON group_turns (state, seq);
CREATE INDEX group_turns_by_thread ON group_turns (thread);
";

/// Asks from the user's other chats (`ask`), each a thread of its own: where it came from, so the asker hears back
/// there once the thread is over (db.rs `MIGRATIONS`; `ensure_asks` too, in case a migration of another page took its
/// slot on a development Mac).
pub const ASKS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS group_asks (
    thread      TEXT PRIMARY KEY NOT NULL,
    origin_chat TEXT NOT NULL,
    asker       TEXT NOT NULL,
    message_id  TEXT NOT NULL,
    named       TEXT NOT NULL DEFAULT '[]',
    created_at  INTEGER NOT NULL,
    linked_at   INTEGER,
    reported_at INTEGER
);
CREATE INDEX IF NOT EXISTS group_asks_open ON group_asks (reported_at);
";

/// Moves what older builds kept under the Filo agent's old speaker name (`OLD_FILO`) to its id, once (the `app_state`
/// mark), as the database opens (db.rs). Chat files keep theirs: store.rs reads the old name as the id.
pub(crate) fn move_old_speakers(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    const MOVED: &str = "filo_speaker_moved";
    let done: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM app_state WHERE key = ?1)", [MOVED], |row| row.get(0))?;
    if done {
        return Ok(());
    }
    for (table, column) in [
        ("group_messages", "speaker"),
        ("group_turns", "speaker"),
        ("group_asks", "asker"),
        ("coding_handoffs", "speaker"),
        ("coding_wakes", "speaker"),
    ] {
        // A table a development Mac's migrations never made has nothing to move.
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            [table, column],
            |row| row.get(0),
        )?;
        if has {
            conn.execute(&format!("UPDATE {table} SET {column} = ?1 WHERE {column} = ?2"), [FILO_AGENT, OLD_FILO])?;
        }
    }
    conn.execute("INSERT INTO app_state (key, value) VALUES (?1, '1')", [MOVED]).map(drop)
}

/// Columns added beside the migrations, so a Filo that doesn't know them still opens the file (db.rs): the message a
/// reply answers (`replies_to`), the user's reaction to a member's message, a voice message's recording (`set_voice`), the
/// files the user's message came with (`GroupMessage::attachments`), whom a member's message asks (`GroupMessage::asks`), the group's name, picture and colour, what a turn
/// passes on when it ends quietly (`Turn::passes_on`), and the turns that run together (`Turn::at_once`).
pub fn ensure_columns(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    for (table, column) in [
        ("group_messages", "reply_to"),
        ("group_messages", "reaction"),
        ("group_messages", "voice"),
        ("group_messages", "attachments"),
        ("group_messages", "asks"),
        ("group_messages", "report"),
        ("group_chat", "name"),
        ("group_chat", "picture"),
        ("group_chat", "background"),
        ("group_chat", "state"),
        ("group_chat", "members"),
        ("group_chat", "route_engine"),
        ("group_chat", "route_model"),
        ("group_chat", "route_reasoning"),
        ("group_turns", "passes_on"),
        ("group_turns", "at_once"),
    ] {
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            [table, column],
            |row| row.get(0),
        )?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"))?;
        }
    }
    conn.execute_batch(MARKS_SCHEMA)?;
    conn.execute_batch(crate::group_route::SCHEMA)?;
    conn.execute_batch(crate::agent_asks::SCHEMA)?;
    Ok(())
}

/// Where the group went off or on, and who joined or left in its settings: marks in the chat at that time (`marks`).
const MARKS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS group_marks (
    seq    INTEGER PRIMARY KEY AUTOINCREMENT,
    at     INTEGER NOT NULL,
    kind   TEXT NOT NULL,
    joined TEXT NOT NULL DEFAULT '[]',
    left_  TEXT NOT NULL DEFAULT '[]'
);
";

// --- On or off, who's in it, and the model that picks who answers ---

/// The group's settings (`group_chat`'s `state`, `members`, `route_engine`, `route_model` and `route_reasoning`). A group an older Filo
/// made has no state: it was always on, so it's on, with every agent in it, as it was.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// Whether the group is on: the user's posts go to its members, and agents ask each other there.
    pub on: bool,
    /// Whether it was ever set up (the group's row is there): what an off group's setup starts from.
    pub made: bool,
    /// The members by agent id; none: every agent, the ones made later too.
    pub members: Option<Vec<String>>,
    /// The model that picks who answers a post naming no one (group_route.rs); none: the cheapest fast one there is.
    pub route: Option<RouteModel>,
}

/// A coding tool's model, by the id its CLI takes, and how hard it thinks (a level of `coding::EFFORT_ORDER`; none:
/// `group_route::DEFAULT_REASONING`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RouteModel {
    pub engine: Engine,
    pub model: String,
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// The group's settings; off with nobody picked before it's made.
pub fn settings(db: &Db) -> Result<Settings, String> {
    let row = db.with(|conn| {
        conn.query_row(
            "SELECT state, members, route_engine, route_model, route_reasoning FROM group_chat WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
    })?;
    let Some((state, members, engine, model, reasoning)) = row else { return Ok(Settings::default()) };
    let route = engine.as_deref().and_then(Engine::parse).zip(model.filter(|model| !model.trim().is_empty())).map(
        |(engine, model)| RouteModel {
            engine,
            model,
            reasoning: reasoning.filter(|level| crate::coding::EFFORT_ORDER.contains(&level.as_str())),
        },
    );
    Ok(Settings {
        on: state.as_deref() != Some("off"),
        made: true,
        members: members.and_then(|json| serde_json::from_str(&json).ok()),
        route,
    })
}

/// Whether the group is on.
pub fn is_on(db: &Db) -> bool {
    settings(db).is_ok_and(|settings| settings.on)
}

/// Keeps who's in the group (none: every agent) and the model that picks who answers, the group made if it isn't yet
/// (off). Who joined or left since leaves a mark in the chat while the group is on.
pub fn set_settings(db: &Db, members: Option<&[String]>, route: Option<&RouteModel>, now: i64) -> Result<(), String> {
    let before = self::members(db);
    let members_json = members.map(|members| serde_json::to_string(members).unwrap_or_else(|_| "[]".into()));
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO group_chat (id, session_id, created_at, state) VALUES (1, ?1, ?2, 'off')",
            params![uuid::Uuid::new_v4().to_string(), now],
        )?;
        tx.execute(
            "UPDATE group_chat SET members = ?1, route_engine = ?2, route_model = ?3, route_reasoning = ?4 WHERE id = 1",
            params![members_json, route.map(|route| route.engine.key()), route.map(|route| route.model.as_str()), route.and_then(|route| route.reasoning.as_deref())],
        )
    })?;
    if is_on(db) {
        let after = self::members(db);
        let joined: Vec<&str> = after
            .iter()
            .filter(|member| !before.iter().any(|old| old.id == member.id))
            .map(|member| member.id.as_str())
            .collect();
        let left: Vec<&str> = before
            .iter()
            .filter(|member| !after.iter().any(|new| new.id == member.id))
            .map(|member| member.id.as_str())
            .collect();
        if !joined.is_empty() || !left.is_empty() {
            let (joined, left) =
                (serde_json::to_string(&joined).unwrap_or_default(), serde_json::to_string(&left).unwrap_or_default());
            db.write(|tx| {
                tx.execute(
                    "INSERT INTO group_marks (at, kind, joined, left_) VALUES (?1, 'members', ?2, ?3)",
                    params![now, joined, left],
                )
            })?;
        }
    }
    Ok(())
}

/// Turns the group on or off (made the first time). A group that has posts gets a mark where it went off or on; turned
/// off, nothing waiting in it speaks. Whether it changed.
pub fn switch(db: &Db, on: bool, now: i64) -> Result<bool, String> {
    let was = settings(db)?;
    if was.made && was.on == on {
        return Ok(false);
    }
    let state = if on { "on" } else { "off" };
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO group_chat (id, session_id, created_at, state) VALUES (1, ?1, ?2, ?3)",
            params![uuid::Uuid::new_v4().to_string(), now, state],
        )?;
        tx.execute("UPDATE group_chat SET state = ?1 WHERE id = 1", params![state])?;
        let posts: i64 = tx.query_row("SELECT COUNT(*) FROM group_messages", [], |row| row.get(0))?;
        if posts > 0 {
            tx.execute("INSERT INTO group_marks (at, kind) VALUES (?1, ?2)", params![now, state])?;
        }
        if !on {
            tx.execute(
                "UPDATE group_turns SET state = 'stopped', finished_at = ?1 WHERE state = 'queued'",
                params![now],
            )?;
            tx.execute(
                "UPDATE group_routes SET state = 'done', picked = '[]' WHERE state IN ('queued', 'running')",
                [],
            )?;
        }
        Ok(())
    })?;
    Ok(true)
}

/// A mark in the group's chat: where it went off or on, or who joined or left it (by agent id).
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub seq: i64,
    pub at: i64,
    pub kind: String,
    pub joined: Vec<String>,
    pub left: Vec<String>,
}

/// The marks since `after` (ms), oldest first.
pub fn marks(db: &Db, after: i64) -> Result<Vec<Mark>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare("SELECT seq, at, kind, joined, left_ FROM group_marks WHERE at >= ?1 ORDER BY seq")?;
        let rows = statement.query_map(params![after], |row| {
            let list = |json: String| serde_json::from_str::<Vec<String>>(&json).unwrap_or_default();
            Ok(Mark {
                seq: row.get(0)?,
                at: row.get(1)?,
                kind: row.get(2)?,
                joined: list(row.get(3)?),
                left: list(row.get(4)?),
            })
        })?;
        rows.collect()
    })
}

/// Who's in the group: `roster`'s agents the user picked, the main agent first; every agent when they picked all.
pub fn members(db: &Db) -> Vec<Member> {
    let all = roster(db);
    match settings(db).ok().and_then(|settings| settings.members) {
        Some(picked) => all.into_iter().filter(|member| picked.contains(&member.id)).collect(),
        None => all,
    }
}

/// What the group is called until the user names it.
pub const DEFAULT_NAME: &str = "Group";
/// The longest name the group keeps.
const MAX_NAME_CHARS: usize = 32;

/// The group's name, picture (an agent's kind: an avatar, a helper or an uploaded one) and the colour behind it, as the
/// user set them; none of each is the default ("Group", the Agents tab's faces, the theme's colour).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    pub name: Option<String>,
    pub picture: Option<String>,
    pub background: Option<String>,
}

impl Look {
    /// Its name, or "Group".
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(DEFAULT_NAME)
    }
}

/// The group's look (the default before the group is made).
pub fn look(db: &Db) -> Result<Look, String> {
    let look = db.with(|conn| {
        conn.query_row("SELECT name, picture, background FROM group_chat WHERE id = 1", [], |row| {
            Ok(Look { name: row.get(0)?, picture: row.get(1)?, background: row.get(2)? })
        })
        .optional()
    })?;
    Ok(look.unwrap_or_default())
}

/// Keeps the group's look: a blank name or picture goes back to the default, a long name is cut.
pub fn set_look(db: &Db, look: &Look) -> Result<(), String> {
    let kept =
        |value: &Option<String>| value.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let name =
        kept(&look.name).map(|name| name.chars().take(MAX_NAME_CHARS).collect::<String>().trim_end().to_string());
    let changed = db.write(|tx| {
        tx.execute(
            "UPDATE group_chat SET name = ?1, picture = ?2, background = ?3 WHERE id = 1",
            params![name, kept(&look.picture), kept(&look.background)],
        )
    })?;
    if changed == 0 {
        return Err("There's no group yet.".into());
    }
    Ok(())
}

/// The group as speakers are told of it: "the group chat", or "\"Team\", the group chat" once the user named it.
fn group_chat(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("\"{name}\", the group chat"),
        None => "the group chat".into(),
    }
}

/// A message in the group: the user's (no speaker) or a member's.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupMessage {
    pub id: String,
    pub role: Role,
    /// The member's agent id (Filo's is `FILO_AGENT`); none for the user.
    pub speaker: Option<String>,
    pub text: String,
    /// Milliseconds since the epoch.
    pub at: i64,
    /// The user's message (its request id) that started the thread it's in.
    pub thread: Option<String>,
    /// The turn that posted it.
    pub turn: Option<String>,
    /// The files the user sent with it (store.rs `Attachment`), kept as JSON beside it.
    pub attachments: Vec<crate::store::Attachment>,
    /// Whom a member's message asks (none for the user's, whose @Names do), kept as JSON beside it.
    pub asks: Asks,
}

/// Whom a member's message asks, from the ask list it was sent with (`asks`): the members by id, in order, and
/// everyone or not. Only these get a turn from it; an @Name in its text is just a name.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Asks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub everyone: bool,
}

impl Asks {
    pub fn is_empty(&self) -> bool {
        self.members.is_empty() && !self.everyone
    }
}

impl GroupMessage {
    /// Its text (`text`, cut or not), with the files it came with named after it and where each is saved: how the
    /// members read it.
    pub fn with_files(&self, text: &str) -> String {
        if self.attachments.is_empty() {
            return text.to_string();
        }
        let files: Vec<String> = self
            .attachments
            .iter()
            .map(|file| format!("{} ({}), saved at {}", file.name, file.content_type, file.file.display()))
            .collect();
        [text.trim_end(), &format!("[Attached: {}]", files.join("; "))]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Its text, or its files' names for a message of files alone: what a preview or a card shows.
    pub fn summary(&self) -> String {
        if self.text.trim().is_empty() && !self.attachments.is_empty() {
            return self.attachments.iter().map(|file| file.name.as_str()).collect::<Vec<_>>().join(", ");
        }
        self.text.clone()
    }
}

/// Where a turn is: `queued`, `running`, then `completed`, `failed` or `stopped`.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub id: String,
    pub thread: String,
    pub speaker: String,
    /// How many turns into its thread (the first is 1).
    pub chain: i64,
    /// Why it's this speaker's turn, as the speaker is told.
    pub reason: String,
    /// The user wrote to this speaker: it has to answer, so saying nothing is a failure.
    pub answers_user: bool,
    pub state: String,
    pub error: Option<String>,
    pub created_at: i64,
    /// The turn of the member whose message the user's came right after (`start_reply_thread`): ended without posting, it
    /// passes the message on to the one leading (`pass_on`), with this note for the speakers (empty for none). None for
    /// every other turn.
    pub passes_on: Option<String>,
    /// A mark the turns of one message asking several members (@Names, "@everyone", an ask list) share: they run at the
    /// same time (`claim_alongside`), each in its member's own chat. None for a turn that runs on its own, in order.
    pub at_once: Option<String>,
}

/// Someone in the group: Filo (`FILO_AGENT`) or another agent, by its agent id.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub id: String,
    pub name: String,
    /// Who they are, in the user's words (an agent's instructions).
    pub about: String,
}

fn role_key(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Error => "error",
    }
}

fn message_from(row: &Row) -> rusqlite::Result<GroupMessage> {
    let role: String = row.get(1)?;
    Ok(GroupMessage {
        id: row.get(0)?,
        role: if role == "user" {
            Role::User
        } else if role == "error" {
            Role::Error
        } else {
            Role::Assistant
        },
        speaker: row.get(2)?,
        text: row.get(3)?,
        at: row.get(4)?,
        thread: row.get(5)?,
        turn: row.get(6)?,
        attachments: row
            .get::<_, Option<String>>(7)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default(),
        asks: row.get::<_, Option<String>>(8)?.and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default(),
    })
}

const MESSAGE_COLUMNS: &str = "id, role, speaker, text, at, thread, turn_id, attachments, asks";

fn turn_from(row: &Row) -> rusqlite::Result<Turn> {
    Ok(Turn {
        id: row.get(0)?,
        thread: row.get(1)?,
        speaker: row.get(2)?,
        chain: row.get(3)?,
        reason: row.get(4)?,
        answers_user: row.get::<_, i64>(5)? != 0,
        state: row.get(6)?,
        error: row.get(7)?,
        created_at: row.get(8)?,
        passes_on: row.get(9)?,
        at_once: row.get(10)?,
    })
}

const TURN_COLUMNS: &str =
    "id, thread, speaker, chain, reason, answers_user, state, error, created_at, passes_on, at_once";

// --- The group and its messages ---

/// The group's conversation id, once the group is made.
pub fn session(db: &Db) -> Result<Option<String>, String> {
    db.with(|conn| conn.query_row("SELECT session_id FROM group_chat WHERE id = 1", [], |row| row.get(0)).optional())
}

/// The group's conversation id, the group made the first time.
pub fn ensure(db: &Db, now: i64) -> Result<String, String> {
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO group_chat (id, session_id, created_at) VALUES (1, ?1, ?2)",
            params![uuid::Uuid::new_v4().to_string(), now],
        )?;
        tx.query_row("SELECT session_id FROM group_chat WHERE id = 1", [], |row| row.get(0))
    })
}

/// The latest `limit` messages, oldest first, and whether there are earlier ones.
pub fn messages(db: &Db, limit: usize) -> Result<(Vec<GroupMessage>, bool), String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {MESSAGE_COLUMNS} FROM group_messages ORDER BY seq DESC LIMIT ?1"))?;
        let mut rows =
            statement.query_map(params![limit as i64 + 1], message_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        rows.reverse();
        Ok((rows, more))
    })
}

/// The replies among `ids`: each one's id and the id of the message it answers (the user's pick with Reply, a member's
/// call, `post_with`).
pub fn replies_to(db: &Db, ids: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare("SELECT id, reply_to FROM group_messages WHERE reply_to IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        Ok(rows.filter_map(Result::ok).filter(|(id, _)| ids.contains(&id.as_str())).collect())
    })
}

/// A member's full report under its short post `id` (send_message's `report`), opened as a page from the post.
pub fn set_report(db: &Db, id: &str, report: &str) -> Result<(), String> {
    let report: String = report.chars().take(MAX_MESSAGE_CHARS).collect();
    db.write(|tx| tx.execute("UPDATE group_messages SET report = ?2 WHERE id = ?1", params![id, report])).map(drop)
}

/// The reports among `ids` (`set_report`): each post's id and its report.
pub fn reports(db: &Db, ids: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare("SELECT id, report FROM group_messages WHERE report IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        Ok(rows.filter_map(Result::ok).filter(|(id, _)| ids.contains(&id.as_str())).collect())
    })
}

/// The reactions among `ids`: each reacted message's id and its emoji.
pub fn reactions(db: &Db, ids: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare("SELECT id, reaction FROM group_messages WHERE reaction IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        Ok(rows.filter_map(Result::ok).filter(|(id, _)| ids.contains(&id.as_str())).collect())
    })
}

/// The user's reaction to message `id` (the chat's React), or none to take it back. False when there's no such
/// message.
pub fn react(db: &Db, id: &str, reaction: Option<&str>) -> Result<bool, String> {
    db.write(|tx| tx.execute("UPDATE group_messages SET reaction = ?2 WHERE id = ?1", params![id, reaction]))
        .map(|changed| changed > 0)
}

/// The user's message `id` was a voice message: its recording (store.rs `Voice`), kept as JSON beside it.
pub fn set_voice(db: &Db, id: &str, voice: &crate::store::Voice) -> Result<(), String> {
    let json = serde_json::to_string(voice).map_err(|error| error.to_string())?;
    db.write(|tx| tx.execute("UPDATE group_messages SET voice = ?2 WHERE id = ?1 AND role = 'user'", params![id, json]))
        .map(|_| ())
}

/// The voice messages among `ids`: each one's recording.
pub fn voices(db: &Db, ids: &[&str]) -> Result<std::collections::HashMap<String, crate::store::Voice>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare("SELECT id, voice FROM group_messages WHERE voice IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        Ok(rows
            .filter_map(Result::ok)
            .filter(|(id, _)| ids.contains(&id.as_str()))
            .filter_map(|(id, json)| Some((id, serde_json::from_str(&json).ok()?)))
            .collect())
    })
}

pub fn message(db: &Db, id: &str) -> Result<Option<GroupMessage>, String> {
    db.with(|conn| {
        conn.query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM group_messages WHERE id = ?1"),
            params![id],
            message_from,
        )
        .optional()
    })
}

/// The members' messages after `after` (ms): how many, and the newest.
pub fn unseen(db: &Db, after: i64) -> Result<(usize, Option<GroupMessage>), String> {
    db.with(|conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM group_messages WHERE role = 'assistant' AND at > ?1", params![after], |row| row.get(0))?;
        let latest = conn
            .query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM group_messages WHERE role = 'assistant' AND at > ?1 ORDER BY seq DESC LIMIT 1"), params![after], message_from)
            .optional()?;
        Ok((count as usize, latest))
    })
}

/// The user's message, starting thread `thread` (the app's request id); the one already there when it's sent again.
/// True when it's new. Tests; the app sends `add_user_reply`.
#[cfg(any(test, feature = "dev"))]
pub fn add_user_message(db: &Db, thread: &str, text: &str, now: i64) -> Result<(GroupMessage, bool), String> {
    add_user_reply(db, thread, text, Vec::new(), None, now)
}

/// `add_user_message` with the files sent with it (`attachments`), answering `reply_to` (a group message): its text
/// quoted at the top (store.rs `with_quote`), which is how the members read it.
pub fn add_user_reply(
    db: &Db,
    thread: &str,
    text: &str,
    attachments: Vec<crate::store::Attachment>,
    reply_to: Option<&GroupMessage>,
    now: i64,
) -> Result<(GroupMessage, bool), String> {
    let text = match reply_to {
        // A reply to the user's own reply quotes only its words.
        Some(target) if target.role == Role::User => crate::store::with_quote(
            crate::store::split_quote(&target.text).map_or(&target.text, |(_, rest)| rest),
            text,
        ),
        Some(target) => crate::store::with_quote(&target.text, text),
        None => text.to_string(),
    };
    let text = text.as_str();
    db.write(|tx| {
        let known = tx
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM group_messages WHERE role = 'user' AND thread = ?1"),
                params![thread],
                message_from,
            )
            .optional()?;
        if let Some(message) = known {
            return Ok((message, false));
        }
        let message = GroupMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            speaker: None,
            text: text.to_string(),
            at: now,
            thread: Some(thread.to_string()),
            turn: None,
            attachments,
            asks: Asks::default(),
        };
        insert_message(tx, &message, reply_to.map(|target| target.id.as_str()))?;
        Ok((message, true))
    })
}

/// `message`, answering `reply_to` (`replies_to`).
fn insert_message(tx: &rusqlite::Transaction, message: &GroupMessage, reply_to: Option<&str>) -> rusqlite::Result<()> {
    let attachments =
        if message.attachments.is_empty() { None } else { serde_json::to_string(&message.attachments).ok() };
    let asks = if message.asks.is_empty() { None } else { serde_json::to_string(&message.asks).ok() };
    tx.execute(
        "INSERT INTO group_messages (id, role, speaker, text, at, thread, turn_id, reply_to, attachments, asks) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![message.id, role_key(message.role), message.speaker, message.text, message.at, message.thread, message.turn, reply_to, attachments, asks],
    )?;
    Ok(())
}

/// A member's message (send_message during its turn `turn_id`, in that turn's thread), answering what gave it the turn
/// (`call_of`) and asking `asks` (its ask list, read by `asks`), who speak next (`follow`).
pub fn post(
    db: &Db,
    speaker: &str,
    text: &str,
    asks: Asks,
    turn_id: Option<&str>,
    now: i64,
) -> Result<GroupMessage, String> {
    post_with(db, speaker, text, asks, turn_id, now, |_, _| Ok(()))
}

/// `post`, with `also` written in the same transaction (a glance's card for the message, glances_mcp.rs).
pub fn post_with(
    db: &Db,
    speaker: &str,
    text: &str,
    asks: Asks,
    turn_id: Option<&str>,
    now: i64,
    also: impl FnOnce(&rusqlite::Transaction, &GroupMessage) -> rusqlite::Result<()>,
) -> Result<GroupMessage, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The message is empty.".into());
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!("The message is too long (at most {MAX_MESSAGE_CHARS} characters). Make it shorter."));
    }
    // What it answers: the message that gave the turn to its speaker, by the members (none for a turn nobody asked
    // for). A coding task's news answers what asked the task for work in the group, the latest ask (none when it was
    // asked in a member's chat).
    let answers = match turn_id.map(|id| turn(db, id)).transpose()?.flatten() {
        Some(turn) => match crate::coding_handoff::asking_turns(db, &turn.id)? {
            Some(asks) => asks
                .iter()
                .filter_map(|ask| self::turn(db, ask).ok().flatten())
                .find_map(|ask| call_of(db, &ask).ok().flatten())
                .map(|call| call.id),
            None => call_of(db, &turn)?.map(|call| call.id),
        },
        None => None,
    };
    db.write(|tx| {
        let thread: Option<String> = match turn_id {
            Some(turn) => tx
                .query_row("SELECT thread FROM group_turns WHERE id = ?1", params![turn], |row| row.get(0))
                .optional()?,
            None => None,
        };
        let message = member_message(speaker, text, asks, thread, turn_id, now);
        insert_message(tx, &message, answers.as_deref())?;
        also(tx, &message)?;
        Ok(message)
    })
}

fn member_message(
    speaker: &str,
    text: &str,
    asks: Asks,
    thread: Option<String>,
    turn_id: Option<&str>,
    now: i64,
) -> GroupMessage {
    GroupMessage {
        id: uuid::Uuid::new_v4().to_string(),
        role: Role::Assistant,
        speaker: Some(speaker.to_string()),
        text: text.to_string(),
        at: now,
        thread,
        turn: turn_id.map(str::to_string),
        attachments: Vec::new(),
        asks,
    }
}

/// Every message in `thread`, oldest first.
pub fn thread_messages(db: &Db, thread: &str) -> Result<Vec<GroupMessage>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {MESSAGE_COLUMNS} FROM group_messages WHERE thread = ?1 ORDER BY seq"))?;
        let rows = statement.query_map(params![thread], message_from)?;
        rows.collect()
    })
}

/// The files the user's message that started `thread` came with: every member answering in it gets them.
pub fn thread_files(db: &Db, thread: &str) -> Result<Vec<crate::store::Attachment>, String> {
    let messages = thread_messages(db, thread)?;
    Ok(messages
        .into_iter()
        .find(|message| message.role == Role::User)
        .map(|message| message.attachments)
        .unwrap_or_default())
}

/// What turn `turn_id` posted.
pub fn replies_of(db: &Db, turn_id: &str) -> Result<Vec<GroupMessage>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM group_messages WHERE turn_id = ?1 AND role = 'assistant' ORDER BY seq"
        ))?;
        let rows = statement.query_map(params![turn_id], message_from)?;
        rows.collect()
    })
}

/// The first member message in `thread`.
pub fn first_reply(db: &Db, thread: &str) -> Result<Option<GroupMessage>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM group_messages WHERE thread = ?1 AND role = 'assistant' ORDER BY seq LIMIT 1"), params![thread], message_from)
            .optional()
    })
}

// --- Turns ---

/// Queues `speaker`'s turn in `thread`, unless it already has one waiting there. Its id, when queued.
pub fn enqueue(
    db: &Db,
    thread: &str,
    speaker: &str,
    chain: i64,
    reason: &str,
    answers_user: bool,
    now: i64,
) -> Result<Option<String>, String> {
    db.write(|tx| enqueue_in(tx, thread, speaker, chain, reason, answers_user, None, now))
}

/// `enqueue`, in `tx`, running together with the other turns marked `at_once` (`Turn::at_once`).
#[allow(clippy::too_many_arguments)]
fn enqueue_in(
    tx: &rusqlite::Transaction,
    thread: &str,
    speaker: &str,
    chain: i64,
    reason: &str,
    answers_user: bool,
    at_once: Option<&str>,
    now: i64,
) -> rusqlite::Result<Option<String>> {
    let waiting: i64 = tx.query_row(
        "SELECT COUNT(*) FROM group_turns WHERE thread = ?1 AND speaker = ?2 AND state = 'queued'",
        params![thread, speaker],
        |row| row.get(0),
    )?;
    if waiting > 0 {
        return Ok(None);
    }
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO group_turns (id, thread, speaker, chain, reason, answers_user, state, created_at, at_once) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8)",
        params![id, thread, speaker, chain, cut_keep_lines(reason, REASON_CHARS), answers_user as i64, now, at_once],
    )?;
    Ok(Some(id))
}

/// The oldest queued turn, now running; none when nothing waits. One statement, so two runners never take the same.
pub fn claim_next(db: &Db, now: i64) -> Result<Option<Turn>, String> {
    db.write(|tx| {
        tx.query_row(
            &format!(
                "UPDATE group_turns SET state = 'running', started_at = ?1
                 WHERE seq = (SELECT seq FROM group_turns WHERE state = 'queued' ORDER BY seq LIMIT 1)
                 RETURNING {TURN_COLUMNS}"
            ),
            params![now],
            turn_from,
        )
        .optional()
    })
}

/// The other queued turns that run together with `turn`, just claimed (`Turn::at_once`), now running too; none for a
/// turn that runs on its own.
pub fn claim_alongside(db: &Db, turn: &Turn, now: i64) -> Result<Vec<Turn>, String> {
    let Some(at_once) = &turn.at_once else { return Ok(Vec::new()) };
    db.write(|tx| {
        let mut statement = tx.prepare(&format!("UPDATE group_turns SET state = 'running', started_at = ?1 WHERE state = 'queued' AND at_once = ?2 RETURNING {TURN_COLUMNS}"))?;
        statement.query_map(params![now, at_once], turn_from)?.collect()
    })
}

/// How a turn ended: `completed`, `failed` (with why) or `stopped`.
pub fn finish(db: &Db, turn_id: &str, state: &str, error: Option<&str>, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE group_turns SET state = ?2, error = ?3, finished_at = ?4 WHERE id = ?1",
            params![turn_id, state, error, now],
        )
    })
    .map(|_| ())
}

pub fn turns_of(db: &Db, thread: &str) -> Result<Vec<Turn>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {TURN_COLUMNS} FROM group_turns WHERE thread = ?1 ORDER BY seq"))?;
        let rows = statement.query_map(params![thread], turn_from)?;
        rows.collect()
    })
}

/// Turns queued or running, oldest first (tests).
#[cfg(any(test, feature = "dev"))]
pub fn active(db: &Db) -> Result<Vec<Turn>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {TURN_COLUMNS} FROM group_turns WHERE state IN ('queued', 'running') ORDER BY seq"
        ))?;
        let rows = statement.query_map([], turn_from)?;
        rows.collect()
    })
}

/// A failed turn the group still shows (`failures`): its speaker hasn't posted in the group since it failed, nor had a
/// turn there go through (one that may end quietly).
const SHOWN_FAILURE: &str = "t.state = 'failed'
    AND NOT EXISTS (SELECT 1 FROM group_messages m WHERE m.role = 'assistant' AND m.speaker = t.speaker AND m.at > COALESCE(t.finished_at, t.created_at))
    AND NOT EXISTS (SELECT 1 FROM group_turns o WHERE o.speaker = t.speaker AND o.state = 'completed' AND o.finished_at > COALESCE(t.finished_at, t.created_at))";

/// The failed turns the group shows, each under the message that asked for it, oldest first: until retried, or until
/// its speaker posts in the group after all, or a later turn of its there goes through.
pub fn failures(db: &Db) -> Result<Vec<Turn>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {TURN_COLUMNS} FROM group_turns t WHERE {SHOWN_FAILURE} ORDER BY t.seq"))?;
        let rows = statement.query_map([], turn_from)?;
        rows.collect()
    })
}

/// Whether the group shows a failed answer (`failures`), with nothing waiting: its row in the list says so.
pub fn reply_failed(db: &Db) -> Result<bool, String> {
    db.with(|conn| {
        let active: i64 =
            conn.query_row("SELECT COUNT(*) FROM group_turns WHERE state IN ('queued', 'running')", [], |row| {
                row.get(0)
            })?;
        if active > 0 {
            return Ok(false);
        }
        conn.query_row(&format!("SELECT EXISTS (SELECT 1 FROM group_turns t WHERE {SHOWN_FAILURE})"), [], |row| {
            row.get(0)
        })
    })
}

/// Turns left running when the app that ran them closed: they failed (retrying runs them again). How many.
#[cfg(test)]
fn interrupted(db: &Db, now: i64) -> Result<usize, String> {
    interrupted_except(db, now, &[])
}

/// `interrupted`, leaving alone the turns in `still_running` (run in their speaker's own chat by a worker that carried
/// on after Filo quit).
pub fn interrupted_except(db: &Db, now: i64, still_running: &[String]) -> Result<usize, String> {
    let ids: Vec<String> = db.with(|conn| {
        conn.prepare("SELECT id FROM group_turns WHERE state = 'running'")?.query_map([], |row| row.get(0))?.collect()
    })?;
    let ids: Vec<String> = ids.into_iter().filter(|id| !still_running.contains(id)).collect();
    db.write(|tx| {
        for id in &ids {
            tx.execute(
                "UPDATE group_turns SET state = 'failed', error = 'Filo closed before this answer was finished.', finished_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
        }
        Ok(ids.len())
    })
}

/// A turn by its id.
pub fn turn(db: &Db, id: &str) -> Result<Option<Turn>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {TURN_COLUMNS} FROM group_turns WHERE id = ?1"), params![id], turn_from)
            .optional()
    })
}

/// Stop: every waiting turn is dropped (the running one is stopped by whoever runs it). How many.
pub fn stop_queued(db: &Db, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        tx.execute("UPDATE group_turns SET state = 'stopped', finished_at = ?1 WHERE state = 'queued'", params![now])
    })
}

/// A removed agent's waiting turns go with it.
pub fn drop_speaker(db: &Db, speaker: &str, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE group_turns SET state = 'stopped', finished_at = ?2 WHERE speaker = ?1 AND state = 'queued'",
            params![speaker, now],
        )
    })
}

/// Runs a thread's failed turns again. How many were queued.
pub fn retry(db: &Db, thread: &str, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE group_turns SET state = 'queued', error = NULL, started_at = NULL, finished_at = NULL, created_at = ?2 WHERE thread = ?1 AND state = 'failed'",
            params![thread, now],
        )
    })
}

/// Runs the failed turns `ids` again (the ones the group shows under one message). How many were queued.
pub fn retry_turns(db: &Db, ids: &[String], now: i64) -> Result<usize, String> {
    db.write(|tx| {
        let mut queued = 0;
        for id in ids {
            queued += tx.execute(
                "UPDATE group_turns SET state = 'queued', error = NULL, started_at = NULL, finished_at = NULL, created_at = ?2 WHERE id = ?1 AND state = 'failed'",
                params![id, now],
            )?;
        }
        Ok(queued)
    })
}

/// The user wrote in the group (`message`, already added for `thread`): whoever they named answers; naming no one, the
/// latest message's poster checks it first, then the one leading takes it. `note`: what the speakers are told besides
/// (where the message came from). The speakers queued, in order. Tests; the app starts `start_reply_thread`.
#[cfg(any(test, feature = "dev"))]
pub fn start_thread(
    db: &Db,
    thread: &str,
    message: &str,
    note: Option<&str>,
    members: &[Member],
    now: i64,
) -> Result<Vec<String>, String> {
    start_reply_thread(db, thread, message, None, note, members, now)
}

/// What a member reached by a message to everyone is told to do with it.
const CHECK: &str = "Answer only if it's for you: about what you do or what you're working on. If it isn't, end your turn without sending \
anything: that's fine, nobody waits on you. Every member gets it at the same time, so answer only for yourself.";

/// `start_thread` for a reply to `reply_to` (`message` with its quote on top, `add_user_reply`): only the user's own
/// words name anyone. Those named answer, all at once when they're several, "@everyone" reaches every member at once, a reply answers the member whose
/// post it answers, and naming no one, the one leading takes it (`route`). The app sends a post naming no one to the
/// routing call first (`needs_route`, group_route.rs), which picks who answers (`routed`); this is where it goes when
/// that can't be asked.
pub fn start_reply_thread(
    db: &Db,
    thread: &str,
    message: &str,
    reply_to: Option<&GroupMessage>,
    note: Option<&str>,
    members: &[Member],
    now: i64,
) -> Result<Vec<String>, String> {
    route(db, thread, message, reply_to, note, members, now)
}

/// Whether the user's post (`message`, a reply to `reply_to` or not) goes to the routing call (group_route.rs): there's
/// more than one member, and it names none of them (@Name, @everyone) and doesn't answer a member's post with Reply,
/// which say who answers themselves.
pub fn needs_route(message: &str, reply_to: Option<&GroupMessage>, members: &[Member]) -> bool {
    let own = reply_to.and_then(|_| crate::store::split_quote(message)).map_or(message, |(_, rest)| rest);
    let replied = reply_to
        .and_then(|target| target.speaker.as_deref())
        .is_some_and(|speaker| members.iter().any(|member| member.id == speaker));
    members.len() > 1 && mentioned(own, members).is_empty() && !names_everyone(own) && !replied
}

/// The routing call for the user's post in `thread` (`message`) picked `picked`: each answers it, all at once, told it's
/// theirs and who else answers (`together`); nobody picked, nobody answers. None (the call failed or its answer couldn't be read): the one leading takes
/// it, as when there's no call (`route`). The speakers queued.
pub fn routed(
    db: &Db,
    thread: &str,
    message: &str,
    picked: Option<&[String]>,
    note: Option<&str>,
    members: &[Member],
    now: i64,
) -> Result<Vec<String>, String> {
    let picked: Option<Vec<String>> = picked
        .map(|picked| picked.iter().filter(|id| members.iter().any(|member| member.id == **id)).cloned().collect());
    crate::group_route::finish(db, thread, picked.as_deref().unwrap_or_default())?;
    let Some(picked) = picked else { return route(db, thread, message, None, note, members, now) };
    let quoted = cut_keep_lines(message.trim(), QUOTE_CHARS);
    let at_once = (picked.len() > 1).then(|| uuid::Uuid::new_v4().to_string());
    db.write(|tx| {
        for speaker in &picked {
            let reason = with_together(
                format!("The user wrote in the group, naming no one, and it's for you:\n\n\"{quoted}\""),
                speaker,
                &picked,
                members,
            );
            enqueue_in(tx, thread, speaker, 1, &with_note(reason, note), true, at_once.as_deref(), now)?;
        }
        Ok(())
    })?;
    Ok(picked)
}

/// What each of several members asked by one message is told (`with_together`): who else was asked, all starting at
/// once (`Turn::at_once`), to take its own part, and that the first one asked owns any change they'd share. None for
/// one asked alone, or not by name.
fn together(speaker: &str, asked: &[String], members: &[Member]) -> Option<String> {
    let owner = asked.first().filter(|_| asked.len() > 1 && asked.iter().any(|id| id == speaker))?;
    let others: Vec<String> = asked.iter().filter(|id| *id != speaker).map(|id| speaker_name(id, members)).collect();
    let (last, rest) = others.split_last()?;
    let (others, were) =
        if rest.is_empty() { (last.clone(), "was") } else { (format!("{} and {last}", rest.join(", ")), "were") };
    let shared = if owner == speaker {
        "is yours to make: they leave it to you.".to_string()
    } else {
        let owner = speaker_name(owner, members);
        format!(
            "is {owner}'s to make: don't make it yourself, say in your post what you'd change and leave it to {owner}."
        )
    };
    Some(format!(
        "{others} {were} asked too, and you all start now, at the same time, without seeing each other's answers first. \
Take your own part of it (what you do, under Members) and leave theirs to them, rather than all doing the same thing. \
A change to the same files or git history (edits, commits, branches, merges) {shared}"
    ))
}

/// `reason`, with what `speaker` is told when others were asked with it (`together`).
fn with_together(reason: String, speaker: &str, asked: &[String], members: &[Member]) -> String {
    match together(speaker, asked, members) {
        Some(together) => format!("{reason}\n\n{together}"),
        None => reason,
    }
}

/// A turn's reason, with what the speakers are told besides (`note`).
fn with_note(reason: String, note: Option<&str>) -> String {
    match note {
        Some(note) => format!("{reason}\n\n{note}"),
        None => reason,
    }
}

/// Who takes the user's message `message` in `thread` (`start_reply_thread`, once it isn't the latest poster's to check
/// first): those it names, else the member whose post it answers, else the one leading; with "@everyone", every member
/// besides. Several start at once.
fn route(
    db: &Db,
    thread: &str,
    message: &str,
    reply_to: Option<&GroupMessage>,
    note: Option<&str>,
    members: &[Member],
    now: i64,
) -> Result<Vec<String>, String> {
    let own = reply_to.and_then(|_| crate::store::split_quote(message)).map_or(message, |(_, rest)| rest);
    let named = mentioned(own, members);
    // Alone in the group, the one member just answers.
    let everyone = names_everyone(own) && members.len() > 1;
    let replied = reply_to
        .and_then(|target| target.speaker.as_deref())
        .filter(|speaker| members.iter().any(|member| member.id == *speaker));
    // Who speaks, and whether the user waits on them (the ones named, or the one it falls to) or they only check.
    let mut speakers: Vec<(String, bool)> = named.iter().map(|id| (id.clone(), true)).collect();
    if everyone {
        speakers.extend(
            members.iter().filter(|member| !named.contains(&member.id)).map(|member| (member.id.clone(), false)),
        );
    } else if speakers.is_empty() {
        speakers.push((replied.unwrap_or_else(|| leader(members)).to_string(), true));
    }
    let quoted = cut_keep_lines(message.trim(), QUOTE_CHARS);
    let at_once = (speakers.len() > 1).then(|| uuid::Uuid::new_v4().to_string());
    db.write(|tx| {
        for (speaker, answers) in &speakers {
            let reason = if !answers {
                format!("The user wrote to everyone in the group (@everyone):\n\n\"{quoted}\"\n\n{CHECK}")
            } else if named.is_empty() && replied.is_some() {
                format!("The user answered your message in the group:\n\n\"{quoted}\"")
            } else if named.is_empty() && members.len() > 1 {
                format!(
                    "The user wrote in the group, naming no one, so it's yours to take:\n\n\"{quoted}\"\n\n\
If it's for you, answer it. If it's for other members (what they do or what they're working on, under Members), ask them in send_message's \
`ask` so they answer it themselves, and don't answer for them; a short word to the user with it is fine. If it's for all of them, ask everyone."
                )
            } else if named.is_empty() {
                format!("The user wrote in the group, naming no one, so it is yours to answer:\n\n\"{quoted}\"")
            } else {
                with_together(format!("The user wrote to you in the group:\n\n\"{quoted}\""), speaker, &named, members)
            };
            enqueue_in(tx, thread, speaker, 1, &with_note(reason, note), *answers, at_once.as_deref(), now)?;
        }
        Ok(())
    })?;
    Ok(speakers.into_iter().map(|(speaker, _)| speaker).collect())
}

/// Who a member's message asking `asks` reaches besides `from`: those it asks, then, asking everyone, every other
/// member.
fn reached(asks: &Asks, from: &str, members: &[Member]) -> Vec<String> {
    let mut reached: Vec<String> = asks
        .members
        .iter()
        .filter(|id| *id != from && members.iter().any(|member| member.id == **id))
        .cloned()
        .collect();
    if asks.everyone {
        let rest: Vec<String> =
            members.iter().map(|member| member.id.clone()).filter(|id| id != from && !reached.contains(id)).collect();
        reached.extend(rest);
    }
    reached
}

/// After a turn that posted: whoever its messages asked speaks next, in the same thread, while the thread has turns
/// left, all at once when it asks several; asking everyone has every other member check whether it's for them. An @Name
/// in the text wakes no one.
pub fn follow(db: &Db, turn: &Turn, members: &[Member], now: i64) -> Result<(), String> {
    if turn.chain >= MAX_CHAIN {
        return Ok(());
    }
    let name = speaker_name(&turn.speaker, members);
    for message in replies_of(db, &turn.id)? {
        let named = reached(&message.asks, &turn.speaker, members);
        if named.is_empty() {
            continue;
        }
        let by_name: Vec<String> = named.iter().filter(|id| message.asks.members.contains(id)).cloned().collect();
        let at_once = (named.len() > 1).then(|| uuid::Uuid::new_v4().to_string());
        db.write(|tx| {
            for next in &named {
                let reason = if by_name.contains(next) {
                    with_together(format!("{name} asked you something in the group."), next, &by_name, members)
                } else {
                    format!("{name} asked everyone in the group. {CHECK}")
                };
                enqueue_in(tx, &turn.thread, next, turn.chain + 1, &reason, false, at_once.as_deref(), now)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Whom a member's ask list (send_message's or ask_agents' `ask`) asks, `from` asking: each entry a member's name as
/// under Members (any case, an "@" in front or not; "Filo" names Filo while it's in the group), or "everyone". Itself,
/// and blank entries, ask no one. A name nobody has is an error that says who can be asked, so nothing is posted.
pub fn asks(list: &[&str], from: &str, members: &[Member]) -> Result<Asks, String> {
    let mut asks = Asks::default();
    for entry in list {
        let name = entry.trim().trim_start_matches('@').trim();
        if name.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("everyone") {
            asks.everyone = true;
            continue;
        }
        let found =
            members.iter().find(|member| member.name.trim().to_lowercase() == name.to_lowercase()).or_else(|| {
                name.eq_ignore_ascii_case("filo")
                    .then(|| members.iter().find(|member| member.id == FILO_AGENT))
                    .flatten()
            });
        let Some(member) = found else {
            let others: Vec<String> = members
                .iter()
                .filter(|member| member.id != from)
                .map(|member| format!("\"{}\"", member.name))
                .collect();
            return Err(format!(
                "Nobody in the group is called \"{name}\": ask by name, one of {}, or \"everyone\".",
                others.join(", ")
            ));
        };
        if member.id != from && !asks.members.contains(&member.id) {
            asks.members.push(member.id.clone());
        }
    }
    Ok(asks)
}

/// Whom `asks` reaches besides `from`, as its poster is told: "everyone", or their names; none when it asks no one.
pub fn asked_names(asks: &Asks, from: &str, members: &[Member]) -> Option<String> {
    let reached = reached(asks, from, members);
    if asks.everyone && !reached.is_empty() {
        return Some("everyone".into());
    }
    let names: Vec<String> = reached.iter().map(|id| speaker_name(id, members)).collect();
    (!names.is_empty()).then(|| names.join(", "))
}

// --- Who's who ---

/// A speaker's name: Filo's, an agent's, or "An agent" for one that's gone.
pub fn speaker_name(speaker: &str, members: &[Member]) -> String {
    match members.iter().find(|member| member.id == speaker) {
        Some(member) => member.name.clone(),
        None if speaker == FILO_AGENT => "Filo".into(),
        None => "An agent".into(),
    }
}

/// Who leads the group: the first of `roster`'s members, the user's main agent.
pub fn leader(members: &[Member]) -> &str {
    members.first().map_or(FILO_AGENT, |member| member.id.as_str())
}

/// Who the user's message names with @Name, in order, each once, by agent id. Longer names win ("@Gym Coach Pro"
/// over "@Gym Coach"), case doesn't matter, and the name has to end there ("@Filo's" counts, "@Filofax" doesn't).
/// "@Filo" always names Filo while it's in the group, whatever the user calls it.
fn mentioned(text: &str, members: &[Member]) -> Vec<String> {
    let mut names: Vec<(String, Vec<char>)> =
        members.iter().map(|member| (member.id.clone(), member.name.trim().chars().collect())).collect();
    if members.iter().any(|member| member.id == FILO_AGENT) {
        names.push((FILO_AGENT.to_string(), "Filo".chars().collect()));
    }
    names.retain(|(_, name)| !name.is_empty());
    names.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let chars: Vec<char> = text.chars().collect();
    let mut found: Vec<String> = Vec::new();
    for (index, _) in chars.iter().enumerate().filter(|(_, c)| **c == '@') {
        let rest = &chars[index + 1..];
        let hit = names.iter().find(|(_, name)| {
            rest.len() >= name.len()
                && rest[..name.len()].iter().zip(name).all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
                && rest.get(name.len()).is_none_or(|next| !next.is_alphanumeric())
        });
        if let Some((id, _)) = hit
            && !found.contains(id)
        {
            found.push(id.clone());
        }
    }
    found
}

/// Whether the user's message writes "@everyone", which reaches every member at once: any case, as a word of its own ("@everyone," counts, "team@everyone" and "@everyones" don't).
pub fn names_everyone(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let word: Vec<char> = "everyone".chars().collect();
    chars.iter().enumerate().filter(|(_, c)| **c == '@').any(|(index, _)| {
        let rest = &chars[index + 1..];
        (index == 0 || !chars[index - 1].is_alphanumeric())
            && rest.len() >= word.len()
            && rest[..word.len()].iter().zip(&word).all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
            && rest.get(word.len()).is_none_or(|next| !next.is_alphanumeric())
    })
}

/// Who's in the group, from the agents in the database: the user's main agent first, who leads it (main_agent.rs), then
/// the others, oldest first. Filo (`FILO_AGENT`, with the Filo agent's own name and instructions: the Filo agent is Filo
/// itself, not a second member) is in it while it's an agent, and leads while it's the main agent, as it does when no
/// other was picked.
///
/// An agent a newer Filo saved on an engine this one doesn't know isn't in it: this build can't run its turns.
pub fn roster(db: &Db) -> Vec<Member> {
    let agents: Vec<crate::app_data::Agent> = crate::app_data::agents(db)
        .unwrap_or_default()
        .into_iter()
        .filter(|agent| agent.newer_engine().is_none())
        .collect();
    let filo_agent = agents.iter().find(|agent| agent.id == FILO_AGENT);
    let name = filo_agent.map(|agent| agent.name.trim().to_string()).filter(|name| !name.is_empty());
    let filo = Member {
        id: FILO_AGENT.into(),
        name: name.unwrap_or_else(|| "Filo".into()),
        about: filo_agent.map(|agent| agent.instructions.clone()).unwrap_or_default(),
    };
    let mut others: Vec<Member> = agents
        .iter()
        .filter(|agent| agent.id != FILO_AGENT)
        .map(|agent| Member { id: agent.id.clone(), name: agent.name.clone(), about: agent.instructions.clone() })
        .collect();
    let lead = crate::main_agent::id(db);
    let mut members: Vec<Member> =
        others.iter().position(|member| member.id == lead).map(|index| others.remove(index)).into_iter().collect();
    if filo_agent.is_some() || members.is_empty() {
        members.push(filo);
    }
    members.extend(others);
    members
}

/// Who speaks from conversation `chat_id`: the agent whose conversation it is, Filo for the Filo agent's and for Filo's
/// own chats.
pub fn speaker_of_chat(db: &Db, chat_id: &str) -> String {
    match crate::app_data::agent_of_chat(db, chat_id).ok().flatten() {
        Some(agent) => agent.id,
        None => FILO_AGENT.to_string(),
    }
}

// --- Asks from the user's other chats ---

/// An ask's outcome: who it asked by name, or nobody (`asked` empty) with whom it could have.
#[derive(Clone, Debug, PartialEq)]
pub struct AskOutcome {
    pub asked: Vec<String>,
    pub could_ask: Vec<String>,
}

/// An ask in the database (`group_asks`): its thread, and the chat it's reported back to.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub thread: String,
    pub origin_chat: String,
}

/// The longest message an ask posts.
const MAX_ASK_CHARS: usize = 4_000;

/// `asker` (Filo or an agent) asks from its conversation `origin_chat`: `text` goes to the group in its name, as a
/// thread of its own, and whoever its ask list `ask` names (`asks`; not itself) answers there, all at once; asking
/// everyone has every other member check whether it's for them. Asking nobody posts nothing.
pub fn ask(
    db: &Db,
    members: &[Member],
    origin_chat: &str,
    asker: &str,
    text: &str,
    ask: &[&str],
    now: i64,
) -> Result<AskOutcome, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Write the message for the ones you ask.".into());
    }
    if text.chars().count() > MAX_ASK_CHARS {
        return Err(format!("The message is too long (at most {MAX_ASK_CHARS} characters). Make it shorter."));
    }
    let could_ask: Vec<String> =
        members.iter().filter(|member| member.id != asker).map(|member| member.name.clone()).collect();
    let asks = asks(ask, asker, members)?;
    let named = reached(&asks, asker, members);
    if named.is_empty() {
        return Ok(AskOutcome { asked: Vec::new(), could_ask });
    }
    let thread = uuid::Uuid::new_v4().to_string();
    let name = speaker_name(asker, members);
    let by_name: Vec<String> = named.iter().filter(|id| asks.members.contains(id)).cloned().collect();
    let reason = |speaker: &String| {
        if by_name.contains(speaker) {
            with_together(
                format!("{name} asked you something in the group, from their chat with the user."),
                speaker,
                &by_name,
                members,
            )
        } else {
            format!("{name} asked everyone in the group, from their chat with the user. {CHECK}")
        }
    };
    let named_json = serde_json::to_string(&named).unwrap_or_else(|_| "[]".into());
    let at_once = (named.len() > 1).then(|| uuid::Uuid::new_v4().to_string());
    ensure_asks(db)?;
    db.write(|tx| {
        tx.execute("INSERT OR IGNORE INTO group_chat (id, session_id, created_at) VALUES (1, ?1, ?2)", params![uuid::Uuid::new_v4().to_string(), now])?;
        // It answers nothing in the group: it starts a thread of its own.
        let message = member_message(asker, text, asks.clone(), Some(thread.clone()), None, now);
        insert_message(tx, &message, None)?;
        for speaker in &named {
            enqueue_in(tx, &thread, speaker, 1, &reason(speaker), false, at_once.as_deref(), now)?;
        }
        tx.execute(
            "INSERT INTO group_asks (thread, origin_chat, asker, message_id, named, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![thread, origin_chat, asker, message.id, named_json, now],
        )?;
        Ok(())
    })?;
    Ok(AskOutcome { asked: named.iter().map(|id| speaker_name(id, members)).collect(), could_ask })
}

/// Makes the asks' table if it isn't there.
pub fn ensure_asks(db: &Db) -> Result<(), String> {
    db.with(|conn| conn.execute_batch(ASKS_SCHEMA))
}

/// Asks not reported back yet whose thread is over (nobody queued or running in it), oldest first.
pub fn asks_answered(db: &Db) -> Result<Vec<Ask>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(
            "SELECT a.thread, a.origin_chat FROM group_asks a
             WHERE a.reported_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM group_turns t WHERE t.thread = a.thread AND t.state IN ('queued', 'running'))
             ORDER BY a.created_at",
        )?;
        let rows = statement.query_map([], |row| Ok(Ask { thread: row.get(0)?, origin_chat: row.get(1)? }))?;
        rows.collect()
    })
}

/// Takes an answered ask to report (true once: whoever claims it reports it).
pub fn claim_report(db: &Db, thread: &str, now: i64) -> Result<bool, String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE group_asks SET reported_at = ?2 WHERE thread = ?1 AND reported_at IS NULL",
            params![thread, now],
        )
    })
    .map(|changed| changed > 0)
}

/// What the asker is sent back in the chat it asked from: the thread as it went, and to tell the user.
pub fn report_message<Tz: TimeZone>(
    messages: &[GroupMessage],
    members: &[Member],
    name: Option<&str>,
    tz: &Tz,
    now: i64,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let answered = messages.iter().skip(1).any(|message| message.role == Role::Assistant);
    let went = if answered {
        transcript(messages, members, tz, now)
    } else {
        format!("{}\n(Nobody answered.)", transcript(messages, members, tz, now))
    };
    format!(
        "↩ What came of what you asked in {} (the user can read it in the Agents tab):\n\n{went}\n\n\
(This is the group answering you, not the user typing. Tell the user what came of it, short and in your own words, with send_message.)",
        group_chat(name)
    )
}

// --- What a speaker is told ---

use crate::schedule::ago;

/// The group's latest messages as "[Sat, Oct 10, 07:12, 5 min ago] Name: text" lines, oldest first. The time tells an
/// old thread from the one going on.
pub fn transcript<Tz: TimeZone>(messages: &[GroupMessage], members: &[Member], tz: &Tz, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for message in messages.iter().rev().filter(|message| message.role != Role::Error).take(TRANSCRIPT_MESSAGES) {
        let who = match &message.speaker {
            None => "User".to_string(),
            Some(speaker) => speaker_name(speaker, members),
        };
        let when = tz
            .timestamp_millis_opt(message.at)
            .single()
            .map(|at| at.format("%a, %b %-d, %H:%M").to_string())
            .unwrap_or_default();
        let line = format!(
            "[{when}, {}] {who}: {}",
            ago(now - message.at),
            message.with_files(&cut_keep_lines(message.text.trim(), LINE_CHARS))
        );
        used += line.len();
        if used > TRANSCRIPT_CHARS && !lines.is_empty() {
            break;
        }
        lines.push(line);
    }
    lines.reverse();
    if lines.is_empty() { "(nothing yet)".into() } else { lines.join("\n") }
}

/// At most `max` characters, line breaks kept, with an ellipsis when cut.
fn cut_keep_lines(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Who the speaker is, for the CLI's system prompt: the one leading the group (the first of `members`), or an agent in
/// it.
pub fn instructions(speaker: &Member, members: &[Member]) -> String {
    let who = if speaker.id == leader(members) {
        let what = if speaker.id == FILO_AGENT { "main assistant" } else { "main agent" };
        format!(
            "You are {}, the user's {what} in the Filo desktop app on their Mac. You lead the group chat of the agents the user made.",
            speaker.name
        )
    } else {
        format!(
            "You are {}, one of the agents the user made in the Filo desktop app on their Mac, speaking in the group chat of Filo and the user's agents.",
            speaker.name
        )
    };
    let about = if speaker.about.trim().is_empty() {
        String::new()
    } else {
        format!("\n\nWho you are and how to work, in the user's words:\n\n{}", speaker.about.trim())
    };
    format!(
        "{who} Everyone in the group, the user included, only sees what you post with the `send_message` tool (from the `filo` MCP server); \
your plain text output is never shown. If something needs a tool you don't have, say so in your message.{about}"
    )
}

/// The group message that gave `turn` to its speaker: the latest in its thread by the user, or by another member
/// asking it (or everyone). None for a turn nobody asked for there (a coding task's news, a check-in it planned).
pub fn call_of(db: &Db, turn: &Turn) -> Result<Option<GroupMessage>, String> {
    let messages = thread_messages(db, &turn.thread)?;
    Ok(messages
        .into_iter()
        .rev()
        .filter(|message| message.role != Role::Error && message.speaker.as_deref() != Some(turn.speaker.as_str()))
        .find(|message| {
            message.role == Role::User || message.asks.everyone || message.asks.members.contains(&turn.speaker)
        }))
}

/// Whether `turn` only checks the message that gave it the turn (`call`, `call_of`'s): the user's "@everyone" or their
/// message right after the speaker's (`Turn::passes_on`), or a member's asking everyone that doesn't ask its speaker by
/// name. Its speaker answers only if it's for them, and may stay quiet.
pub fn checking(turn: &Turn, call: Option<&GroupMessage>) -> bool {
    !turn.answers_user
        && call.is_some_and(|call| {
            call.role == Role::User || (call.asks.everyone && !call.asks.members.contains(&turn.speaker))
        })
}

/// What a turn run in the speaker's own chat sends it (local_backend/group.rs): `prompt`'s, told it works there as on
/// any message and that send_message posts in the group (`name`, when the user named it). The time and its memories
/// come with every turn of the chat (turn.rs), so they're not here.
pub fn own_chat_prompt<Tz: TimeZone>(
    turn: &Turn,
    members: &[Member],
    work: &Work,
    messages: &[GroupMessage],
    name: Option<&str>,
    tz: &Tz,
    now: i64,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (roster, answer) = roster_and_answer(turn, members, work);
    [
        format!(
            "[From {} of Filo and the agents the user made (the user reads it in the Agents tab and writes in it too; their messages show as \"User\"). \
You do this here, in your own chat, as you would any message: with your tools, what this chat knows, and coding tasks, which report back here. \
For this turn send_message posts in the group in your name, and this chat shows it too; nothing else you write is shown.]",
            group_chat(name)
        ),
        format!("Members:\n{}", roster.join("\n")),
        format!("Latest messages in the group:\n{}", transcript(messages, members, tz, now)),
        format!("Why you're speaking now: {}", turn.reason),
        format!(
            "{answer} To ask members something, name them in send_message's `ask` (\"everyone\" for all of them): only they answer next, in the group. \
An @Name in your text is just a name and wakes no one. Don't ask someone just to thank them or say goodbye. Earlier threads in the latest messages are over: pick one up again only if this one needs it."
        ),
    ]
    .join("\n\n")
}

/// The members as a speaker reads them, and what it's told to post. The one leading reads more of what each does, and
/// under each what it's busy with outside the group (`work`), to tell who a message is for.
fn roster_and_answer(turn: &Turn, members: &[Member], work: &Work) -> (Vec<String>, &'static str) {
    let about_chars = if turn.speaker == leader(members) { 300 } else { 160 };
    let roster: Vec<String> = members
        .iter()
        .map(|member| {
            let you = if member.id == turn.speaker { " (you)" } else { "" };
            let about = match (member.id == leader(members), member.id == FILO_AGENT) {
                (true, true) => "the user's main assistant, who leads the group".to_string(),
                (true, false) => {
                    format!("the user's main agent, who leads the group. {}", cut(&member.about, about_chars))
                }
                (false, _) => cut(&member.about, about_chars),
            };
            let doing: String =
                work.get(&member.id).into_iter().flatten().map(|line| format!("\n  - {line}")).collect();
            format!("- @{}{you}: {about}{doing}", member.name)
        })
        .collect();
    let answer = if turn.answers_user {
        "The user is waiting for your answer: post it with send_message, one or two lines in your own voice, with the full report (what you did, checked or found) in its `report`, which the group opens as a page from your message."
    } else {
        "Post your message for the group with send_message: one or two lines in your own voice, with any longer report in its `report`. If you have nothing useful to add, don't send anything."
    };
    (roster, answer)
}

/// What a turn sends the speaker: who's here, the group (`name`, when the user named it) so far, and why it's their turn.
fn prompt<Tz: TimeZone>(
    turn: &Turn,
    members: &[Member],
    work: &Work,
    messages: &[GroupMessage],
    name: Option<&str>,
    tz: &Tz,
    now: i64,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (roster, answer) = roster_and_answer(turn, members, work);
    [
        format!(
            "[{} of Filo and the agents the user made. The user reads it in the Agents tab of Filo and writes in it too; their messages show as \"User\".]",
            name.map_or_else(|| "Group chat".to_string(), |name| format!("\"{name}\", the group chat"))
        ),
        crate::schedule_mcp::now_note(tz, now),
        format!("Members:\n{}", roster.join("\n")),
        format!("Latest messages:\n{}", transcript(messages, members, tz, now)),
        format!("Why you're speaking now: {}", turn.reason),
        format!(
            "{answer} To ask members something, name them in send_message's `ask` (\"everyone\" for all of them): only they answer next, in this group. \
An @Name in your text is just a name and wakes no one. Don't ask someone just to thank them or say goodbye. Earlier threads in the latest messages are over: pick one up again only if this one needs it."
        ),
    ]
    .join("\n\n")
}

/// What members are busy with outside the group, by member id, as the one leading it reads them under Members: a line
/// each (`work`).
pub type Work = std::collections::HashMap<String, Vec<String>>;

/// How many of a member's coding tasks the one leading reads, and how long a finished one still counts.
const WORK_TASKS: usize = 3;
const WORK_RECENT_MS: i64 = 3 * 24 * 3_600_000;
/// How much of what the user last wrote to a member the one leading reads.
const WORK_ASK_CHARS: usize = 200;

/// What each member but `speaker` is busy with outside the group, so the one leading can tell who a message is for:
/// its coding tasks going on, then the ones finished in the last days (coding_handoff.rs: a task is the member's that
/// started it in the group, else the agent's whose chat it came from, else Filo's), and what the user last wrote to it
/// in its own chat (`chats`: a member's id and its chat in `store`).
pub fn work(db: &Db, store: &Store, members: &[Member], chats: &[(String, String)], speaker: &str, now: i64) -> Work {
    let mut work = Work::new();
    let mut tasks = crate::coding_handoff::unfinished(db).unwrap_or_default();
    tasks.reverse();
    tasks.extend(crate::coding_handoff::finished_since(db, now - WORK_RECENT_MS).unwrap_or_default());
    for (task, handoff) in tasks {
        let owner = handoff.speaker.or(handoff.agent_id).unwrap_or_else(|| FILO_AGENT.to_string());
        if owner == speaker || !members.iter().any(|member| member.id == owner) {
            continue;
        }
        let lines = work.entry(owner).or_default();
        if lines.len() < WORK_TASKS {
            let state = match task.status.as_str() {
                "queued" => "about to start".to_string(),
                "running" | "monitoring" => format!("running, started {}", ago(now - task.created_at)),
                status => format!("{status} {}", ago(now - task.updated_at)),
            };
            lines.push(format!("Coding task \"{}\" in {}: {state}", task.title, task.project_name));
        }
    }
    for (member, chat) in chats.iter().filter(|(member, _)| member != speaker) {
        let messages = store.messages(chat).unwrap_or_default();
        let last = messages.iter().rev().find(|message| {
            message.role == Role::User && !message.hidden && message.routine.is_none() && message.group_call.is_none()
        });
        if let Some(message) = last {
            let wrote = cut(message.own_text(), WORK_ASK_CHARS);
            work.entry(member.clone()).or_default().push(format!(
                "The user last wrote to it in its own chat {}: \"{wrote}\"",
                ago(now - message.at as i64)
            ));
        }
    }
    work
}

// --- Running a turn ---

/// How a member's turn runs: its engine, model and system prompt, where the CLI works, and which of its own
/// conversations the MCP server's other tools (reminders, check-ins, memories' source) belong to.
#[derive(Clone, Debug)]
pub struct Setup {
    pub engine: Engine,
    pub model: Option<String>,
    pub instructions: String,
    pub prompt: String,
    pub workspace: PathBuf,
    /// The speaker's own conversation (or the group's, for a speaker without one) and the folder of its store.
    pub chat_id: String,
    pub store_root: PathBuf,
}

/// The MCP server's arguments for a group turn (after the program's own, like `--local-mcp`).
fn mcp_args(setup: &Setup, turn: &Turn, db: &Path) -> Vec<String> {
    vec![
        "--chat".into(),
        setup.chat_id.clone(),
        "--turn".into(),
        turn.id.clone(),
        "--dir".into(),
        setup.store_root.display().to_string(),
        "--db".into(),
        db.display().to_string(),
        "--group-speaker".into(),
        turn.speaker.clone(),
    ]
}

/// Runs the speaker's CLI for `turn` (already claimed) on a fresh session. `on_start` gets a Stopper as soon as it runs.
/// Blocks until the turn ends. The caller records how it ended (`finish`).
pub fn run(
    db: &Db,
    db_path: &Path,
    runtime: &Runtime,
    setup: &Setup,
    turn: &Turn,
    on_start: impl Fn(Stopper),
) -> Report {
    let failed = |error: String| Report { error: Some(error), ..Report::default() };
    let engine = setup.engine;
    let Some(program) = engine::find_program(engine, runtime.path_env.as_deref()) else {
        return failed(format!(
            "{} isn't installed on this Mac (no `{}` on PATH or in ~/.local/bin, /opt/homebrew/bin, /usr/local/bin). Install it, or pick another engine in this agent's own chat.",
            engine.label(),
            engine.program()
        ));
    };
    if let Err(error) = std::fs::create_dir_all(&setup.workspace) {
        return failed(format!("Couldn't create the agents' folder: {error}"));
    }
    // The user's skills as the CLI's own, copied into the workspace (store.rs `write_skills`).
    let skills_dir = crate::skills::folder_for(db_path);
    let skills = crate::skills::list(&skills_dir);
    if let Err(error) = crate::store::Store::at(&setup.store_root).write_skills(&skills) {
        eprintln!("filo skills: couldn't write them for the group turn: {error}");
    }
    let mut args = runtime.mcp_args.clone();
    args.extend(mcp_args(setup, turn, db_path));
    // The user's apps that are on for the speaker's engine (apps.rs).
    let mut servers = crate::apps::servers_for(db_path, engine, &turn.speaker, &runtime.mcp_program, &runtime.mcp_args);
    // And the apps on this Mac (`use_computer`, computer.rs), only while "Your computer" is on.
    let computer_root = crate::computer::root_of_db(db_path);
    // How freely its shell works: as its engine's tool does on the Coding page.
    let mode = crate::coding::turn_mode(db_path, engine);
    // Feed and Notes, if they're on: their tools and what the instructions say of them.
    let pages = crate::mcp::pages(db_path);
    // A thread the user started; a check-in's isn't (computer.rs `server_if_on`).
    let attended = thread_messages(db, &turn.thread)
        .is_ok_and(|messages| messages.iter().any(|message| message.role == Role::User));
    let instructions = engine::with_capabilities(
        &setup.instructions,
        engine,
        &mode,
        &servers,
        Some(crate::computer::capability(&computer_root, attended)),
        pages,
    );
    let instructions = format!("{instructions} {}", crate::skills::install_line(&skills_dir));
    // Claude Code's Artifact tool, only while "Share to claude.ai" is on (artifact_share.rs); Codex never.
    let share = engine == Engine::Claude && crate::artifact_share::is_on(&computer_root);
    if share {
        crate::artifact_share::refresh_copies(&crate::artifacts::folder_for(db_path));
    }
    let instructions = if share {
        format!("{instructions} {}", crate::artifact_share::capability(&crate::artifacts::folder_for(db_path)))
    } else {
        instructions
    };
    servers.extend(crate::computer::server_if_on(&computer_root, &runtime.mcp_program, &runtime.mcp_args, attended));
    let (tool_model, effort) = crate::coding::turn_model(db_path, engine);
    let spec = TurnSpec {
        engine,
        program,
        prompt: setup.prompt.clone(),
        resume: None,
        new_session_id: None,
        workspace: setup.workspace.clone(),
        mcp: McpLaunch { program: runtime.mcp_program.clone(), args },
        path_env: runtime.path_env.clone(),
        model: setup.model.clone().or(tool_model),
        // What the speaker can use goes after who they are, so the group's instructions match the turn's tools.
        instructions: Some(instructions),
        inputs: Vec::new(),
        servers,
        // Every group turn is a fresh session.
        one_off: true,
        skills: skills.into_iter().map(|skill| skill.name).collect(),
        writable: vec![skills_dir],
        readable: crate::turn::readable(&setup.store_root, Some(db_path)),
        mode,
        effort,
        pages,
    };
    let mut call = engine::invocation(&spec);
    if share {
        crate::artifact_share::enable(&mut call);
    }
    let running = match engine::start_turn(&spec, &call, runtime.hung_after, None) {
        Ok(running) => running,
        Err(error) => return failed(error),
    };
    on_start(running.stopper());
    // A page it shared to claude.ai: an artifact's keeps the link.
    let mut shares = crate::artifact_share::Watcher::default();
    let outcome = running.wait_lines(|line| {
        for (file, url) in shares.line(line).into_iter().filter(|_| share) {
            crate::artifact_share::record(db_path, &file, &url);
        }
    });
    let replies = replies_of(db, &turn.id).map(|replies| replies.len()).unwrap_or(0);
    if outcome.stopped {
        return Report { replies, stopped: true, ..Report::default() };
    }
    let error = match (outcome.error, replies) {
        (Some(error), _) => Some(error),
        // Saying nothing is fine when only asked to chime in, not when the user is waiting.
        (None, 0) if turn.answers_user => Some(match outcome.final_text.filter(|text| !text.trim().is_empty()) {
            Some(text) => format!(
                "{} finished without posting with Filo's send_message tool. What it wrote instead:\n\n{text}",
                engine.label()
            ),
            None => format!("{} finished without answering.", engine.label()),
        }),
        _ => None,
    };
    Report { replies, stopped: false, error, stderr: outcome.stderr }
}

/// The group's latest messages, as a speaker reads them before its turn.
pub fn latest(db: &Db) -> Result<Vec<GroupMessage>, String> {
    Ok(messages(db, TRANSCRIPT_MESSAGES)?.0)
}

/// `run`'s prompt with the speaker's memories of the user in front, as every turn has (turn.rs).
pub fn setup_prompt(
    turn: &Turn,
    members: &[Member],
    work: &Work,
    db: &Db,
    memories: &str,
    now: i64,
) -> Result<String, String> {
    let (messages, _) = messages(db, TRANSCRIPT_MESSAGES)?;
    let look = look(db)?;
    Ok(crate::turn::with_memories(memories, &prompt(turn, members, work, &messages, look.name.as_deref(), &Local, now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<Member> {
        vec![
            Member { id: FILO_AGENT.into(), name: "Filo".into(), about: String::new() },
            Member { id: "coach".into(), name: "Gym Coach".into(), about: "Plans workouts.".into() },
            Member { id: "pro".into(), name: "Gym Coach Pro".into(), about: "Plans harder workouts.".into() },
            Member { id: "chef".into(), name: "Chef".into(), about: "Cooks.".into() },
        ]
    }

    /// What an ask list of `members`' names asks (`asks`).
    fn asking(names: &[&str]) -> Asks {
        asks(names, "", &members()).unwrap()
    }

    #[test]
    fn the_main_agent_answers_what_names_no_one() {
        let db = Db::in_memory().unwrap();
        let members = members();
        let chef_leads = vec![members[3].clone(), members[0].clone()];
        assert_eq!(leader(&chef_leads), "chef");
        assert_eq!(start_thread(&db, "t", "Dinner?", None, &chef_leads, 1).unwrap(), ["chef"]);
        assert_eq!(start_thread(&db, "u", "@Filo, dinner?", None, &chef_leads, 1).unwrap(), [FILO_AGENT]);
    }

    #[test]
    fn mentions_find_each_member_once_longest_name_first() {
        let members = members();
        assert_eq!(mentioned("@gym coach pro and @Chef, then @Chef again", &members), ["pro", "chef"]);
        assert_eq!(mentioned("@Gym Coach: what about legs? @filo's take?", &members), ["coach", FILO_AGENT]);
        assert!(mentioned("email me at chef@Chefs.com or @Filofax", &members).is_empty());
        assert!(mentioned("no names here", &members).is_empty());
        // Filo renamed: both its name and "@Filo" name it.
        let renamed = vec![Member { id: FILO_AGENT.into(), name: "Jarvis".into(), about: String::new() }];
        assert_eq!(mentioned("@Jarvis", &renamed), [FILO_AGENT]);
        assert_eq!(mentioned("@Filo", &renamed), [FILO_AGENT]);
    }

    #[test]
    fn a_thread_runs_named_speakers_in_turn_and_stops_at_the_chain_limit() {
        let db = Db::in_memory().unwrap();
        let members = members();
        let session = ensure(&db, 1).unwrap();
        assert_eq!(ensure(&db, 2).unwrap(), session);
        assert_eq!(super::session(&db).unwrap(), Some(session));

        // Naming no one: Filo answers.
        let (message, new) = add_user_message(&db, "t1", "Plan my week", 10).unwrap();
        assert!(new);
        assert_eq!(add_user_message(&db, "t1", "Plan my week", 11).unwrap(), (message.clone(), false));
        assert_eq!(start_thread(&db, "t1", "Plan my week", None, &members, 10).unwrap(), [FILO_AGENT]);
        let turn = claim_next(&db, 12).unwrap().unwrap();
        assert_eq!(
            (turn.speaker.as_str(), turn.chain, turn.answers_user, turn.state.as_str()),
            (FILO_AGENT, 1, true, "running")
        );
        assert!(turn.reason.contains("naming no one") && turn.reason.contains("Plan my week"));
        assert!(claim_next(&db, 12).unwrap().is_none());

        // Filo asks two agents; each is queued once, with a pointer to make.
        let posted = post(
            &db,
            FILO_AGENT,
            "Chef, dinners? Gym Coach, workouts?",
            asking(&["Chef", "@gym coach", "chef"]),
            Some(&turn.id),
            13,
        )
        .unwrap();
        assert_eq!(posted.thread.as_deref(), Some("t1"));
        follow(&db, &turn, &members, 14).unwrap();
        assert_eq!(
            active(&db).unwrap().iter().map(|turn| turn.speaker.as_str()).collect::<Vec<_>>(),
            [FILO_AGENT, "chef", "coach"]
        );
        finish(&db, &turn.id, "completed", None, 15).unwrap();
        let chef = claim_next(&db, 16).unwrap().unwrap();
        assert_eq!((chef.speaker.as_str(), chef.chain, chef.answers_user), ("chef", 2, false));
        assert!(
            chef.reason.starts_with("Filo asked you something in the group.\n\nGym Coach was asked too"),
            "{}",
            chef.reason
        );
        // Asked again while waiting: still one turn.
        assert_eq!(enqueue(&db, "t1", "coach", 2, "again", false, 16).unwrap(), None);

        // Turns stop at the chain limit.
        let last = Turn { chain: MAX_CHAIN, ..chef.clone() };
        post(&db, "chef", "@Filo enough?", asking(&["Filo"]), Some(&chef.id), 17).unwrap();
        follow(&db, &last, &members, 18).unwrap();
        assert_eq!(active(&db).unwrap().iter().filter(|turn| turn.speaker == FILO_AGENT).count(), 0);

        // Stop drops what waits; retry runs the failed again.
        finish(&db, &chef.id, "failed", Some("boom"), 19).unwrap();
        assert_eq!(stop_queued(&db, 20).unwrap(), 1);
        assert!(active(&db).unwrap().is_empty());
        assert_eq!(retry(&db, "t1", 21).unwrap(), 1);
        assert_eq!(claim_next(&db, 22).unwrap().unwrap().id, chef.id);
        // A turn left running when the app closed failed.
        assert_eq!(interrupted(&db, 23).unwrap(), 1);
        assert_eq!(turns_of(&db, "t1").unwrap().iter().find(|turn| turn.id == chef.id).unwrap().state, "failed");
        assert!(reply_failed(&db).unwrap());

        let (all, more) = messages(&db, 1).unwrap();
        assert!(more);
        assert_eq!(all[0].speaker.as_deref(), Some("chef"));
        assert_eq!(first_reply(&db, "t1").unwrap().unwrap().id, posted.id);
        let (count, latest) = unseen(&db, 13).unwrap();
        assert_eq!((count, latest.map(|message| message.speaker)), (1, Some(Some("chef".into()))));
        assert!(post(&db, FILO_AGENT, "   ", Asks::default(), None, 24).is_err());
    }

    #[test]
    fn only_a_posts_ask_list_wakes_members_never_an_at_name_in_its_text() {
        let db = Db::in_memory().unwrap();
        let members = members();
        add_user_message(&db, "t", "Plan the launch", 1).unwrap();
        start_thread(&db, "t", "Plan the launch", None, &members, 1).unwrap();
        let filo = claim_next(&db, 2).unwrap().unwrap();
        // A passing @reference wakes no one.
        post(
            &db,
            FILO_AGENT,
            "We still need the copyright holder that @Gym Coach asked about. @everyone, more soon.",
            Asks::default(),
            Some(&filo.id),
            3,
        )
        .unwrap();
        follow(&db, &filo, &members, 4).unwrap();
        assert_eq!(active(&db).unwrap().iter().map(|turn| turn.speaker.as_str()).collect::<Vec<_>>(), [FILO_AGENT]);
        // An ask without @ in the text wakes the member it asks, and is what gave it the turn.
        let asked =
            post(&db, FILO_AGENT, "Chef, can you draft the launch dinner?", asking(&["Chef"]), Some(&filo.id), 5)
                .unwrap();
        assert_eq!(
            message(&db, &asked.id).unwrap().unwrap().asks,
            Asks { members: vec!["chef".into()], everyone: false }
        );
        follow(&db, &filo, &members, 6).unwrap();
        finish(&db, &filo.id, "completed", None, 6).unwrap();
        let chef = claim_next(&db, 7).unwrap().unwrap();
        assert_eq!(
            (chef.speaker.as_str(), chef.chain, chef.reason.as_str()),
            ("chef", 2, "Filo asked you something in the group.")
        );
        assert_eq!(call_of(&db, &chef).unwrap().map(|call| call.id), Some(asked.id));
        assert!(!checking(&chef, call_of(&db, &chef).unwrap().as_ref()));
        assert!(claim_next(&db, 8).unwrap().is_none());
    }

    #[test]
    fn an_ask_list_names_members_as_under_members_or_everyone() {
        let members = members();
        let ids = |asks: Asks| (asks.members, asks.everyone);
        assert_eq!(
            ids(asks(&["@gym coach pro", " Chef ", "chef", "", "Filo"], "coach", &members).unwrap()),
            (vec!["pro".into(), "chef".into(), FILO_AGENT.into()], false)
        );
        // Asking itself asks no one.
        assert_eq!(ids(asks(&["Everyone", "Gym Coach"], "coach", &members).unwrap()), (Vec::new(), true));
        assert!(asks(&[], FILO_AGENT, &members).unwrap().is_empty());
        // A name nobody has says who can be asked.
        let error = asks(&["Baker"], FILO_AGENT, &members).unwrap_err();
        assert!(
            error.contains("\"Baker\"")
                && error.contains("\"Chef\"")
                && !error.contains("\"Filo\"")
                && error.contains("\"everyone\""),
            "{error}"
        );
        // Filo renamed: its name and "Filo" both ask it.
        let renamed =
            vec![Member { id: FILO_AGENT.into(), name: "Jarvis".into(), about: String::new() }, members[3].clone()];
        assert_eq!(asks(&["jarvis"], "chef", &renamed).unwrap().members, [FILO_AGENT]);
        assert_eq!(asks(&["Filo"], "chef", &renamed).unwrap().members, [FILO_AGENT]);
    }

    #[test]
    fn a_failed_answer_shows_until_retried_or_its_speaker_posts_after_all() {
        let db = Db::in_memory().unwrap();
        let members = vec![
            Member { id: FILO_AGENT.into(), name: "Filo".into(), about: String::new() },
            Member { id: "chef".into(), name: "Chef".into(), about: String::new() },
        ];
        // Filo asks Chef from its chat: a thread with no message of the user's. Chef's answer fails.
        ask(&db, &members, "filo-chat", FILO_AGENT, "Dinner?", &["Chef"], 1).unwrap();
        let chef = claim_next(&db, 2).unwrap().unwrap();
        finish(&db, &chef.id, "failed", Some("limit"), 3).unwrap();
        assert_eq!(failures(&db).unwrap().iter().map(|turn| turn.id.as_str()).collect::<Vec<_>>(), [chef.id.as_str()]);
        assert!(reply_failed(&db).unwrap());
        // Retried, it waits again; failed again, it shows again.
        assert_eq!(retry_turns(&db, std::slice::from_ref(&chef.id), 4).unwrap(), 1);
        assert!(failures(&db).unwrap().is_empty() && !reply_failed(&db).unwrap());
        claim_next(&db, 5).unwrap().unwrap();
        finish(&db, &chef.id, "failed", Some("limit"), 6).unwrap();
        assert_eq!(failures(&db).unwrap().len(), 1);
        // Someone else posting doesn't answer for Chef; Chef posting later does.
        post(&db, FILO_AGENT, "Still there?", Asks::default(), None, 7).unwrap();
        assert!(reply_failed(&db).unwrap());
        post(&db, "chef", "Pasta.", Asks::default(), None, 8).unwrap();
        assert!(failures(&db).unwrap().is_empty() && !reply_failed(&db).unwrap());
        // So does a later turn of Chef's that went through with nothing to post; another member's doesn't.
        ask(&db, &members, "filo-chat", FILO_AGENT, "Dessert?", &["Chef"], 9).unwrap();
        let again = claim_next(&db, 10).unwrap().unwrap();
        finish(&db, &again.id, "failed", Some("limit"), 11).unwrap();
        let filo = enqueue(&db, "t-filo", FILO_AGENT, 0, "check-in", false, 12).unwrap().unwrap();
        claim_next(&db, 12).unwrap();
        finish(&db, &filo, "completed", None, 13).unwrap();
        assert!(reply_failed(&db).unwrap());
        let quiet = enqueue(&db, "t-chef", "chef", 0, "check-in", false, 14).unwrap().unwrap();
        claim_next(&db, 14).unwrap();
        finish(&db, &quiet, "completed", None, 15).unwrap();
        assert!(failures(&db).unwrap().is_empty() && !reply_failed(&db).unwrap());
    }

    #[test]
    fn a_coding_tasks_news_answers_what_asked_for_the_task() {
        use crate::coding::{NewTask, Project};
        use crate::coding_handoff::{self, NewHandoff};
        let db = Db::in_memory().unwrap();
        let task = |asked_in: &str| {
            let project = Project {
                name: "oven".into(),
                path: "/p/oven".into(),
                branch: None,
                remote: None,
                plain_folder: true,
                added: true,
            };
            let made = crate::coding::create_task(
                &db,
                &NewTask {
                    brief: "Fix the oven".into(),
                    tool: "claude-code".into(),
                    project,
                    worktree: false,
                    owner: "Chef".into(),
                },
                1,
            )
            .unwrap();
            let new = NewHandoff {
                chat_id: "chef-chat".into(),
                agent_id: Some("chef".into()),
                speaker: Some("chef".into()),
                title: "Fix the oven".into(),
                goal: "Fix the oven".into(),
                done_when: "It heats".into(),
                notes: None,
                look_only: false,
                model: None,
                effort: None,
            };
            let handoff = coding_handoff::create(&db, &made.id, &new, 1)
                .map(|_| coding_handoff::get(&db, &made.id).unwrap().unwrap())
                .unwrap();
            coding_handoff::add_ask(&db, &made.id, asked_in, 2).unwrap();
            handoff
        };
        // The user asks Chef in the group; Chef starts the task in that turn and says so. Others talk meanwhile.
        let (asked, _) = add_user_message(&db, "t1", "@Chef can you fix the oven?", 1).unwrap();
        let chef = enqueue(&db, "t1", "chef", 1, "asked", true, 1).unwrap().unwrap();
        let oven = task(&chef);
        post(&db, "chef", "On it: I've started Claude Code.", Asks::default(), Some(&chef), 3).unwrap();
        add_user_message(&db, "t2", "@Filo and dinner?", 4).unwrap();
        // The round finished: Chef's news turn posts the result, answering the ask.
        let news = |handoff: &coding_handoff::Handoff, at: i64| {
            let wake = coding_handoff::add_wake(&db, handoff, "round", 1, "It finished.", at).unwrap();
            let turn = enqueue(&db, &format!("coding-{wake}"), "chef", 1, "It finished.", false, at).unwrap().unwrap();
            coding_handoff::mark_sent(&db, &[wake], &turn).unwrap();
            turn
        };
        let result = post(&db, "chef", "The oven heats again.", Asks::default(), Some(&news(&oven, 5)), 6).unwrap();
        // One asked for in Chef's own chat (a turn the group never had): nothing in the group to answer.
        let elsewhere = task("own-chat-turn");
        let quiet = post(&db, "chef", "The grill works too.", Asks::default(), Some(&news(&elsewhere, 7)), 8).unwrap();
        let replies = replies_to(&db, &[result.id.as_str(), quiet.id.as_str()]).unwrap();
        assert_eq!(replies, std::collections::HashMap::from([(result.id.clone(), asked.id.clone())]));
    }

    #[test]
    fn a_reply_quotes_the_post_it_answers_and_remembers_it() {
        let db = Db::in_memory().unwrap();
        let posted = post(&db, FILO_AGENT, "Pasta or rice?", Asks::default(), None, 1).unwrap();
        let (reply, new) = add_user_reply(&db, "t1", "pasta", Vec::new(), Some(&posted), 2).unwrap();
        assert!(new);
        assert_eq!(reply.text, "> Pasta or rice?\n\npasta");
        assert_eq!(message(&db, &reply.id).unwrap().unwrap().text, reply.text);
        // Sent again: the same message.
        assert_eq!(add_user_reply(&db, "t1", "pasta", Vec::new(), Some(&posted), 3).unwrap(), (reply.clone(), false));
        let (plain, _) = add_user_message(&db, "t2", "thanks", 4).unwrap();
        let replies = replies_to(&db, &[reply.id.as_str(), plain.id.as_str()]).unwrap();
        assert_eq!(replies, std::collections::HashMap::from([(reply.id.clone(), posted.id.clone())]));
        // A reply to that reply quotes only its words.
        let (again, _) = add_user_reply(&db, "t3", "with cheese", Vec::new(), Some(&reply), 5).unwrap();
        assert_eq!(again.text, "> pasta\n\nwith cheese");

        // Who posted it answers; a name in the quote doesn't count, one in the user's words does.
        let members = members();
        let asked = post(&db, "chef", "@Gym Coach, protein?", Asks::default(), None, 6).unwrap();
        let (reply, _) = add_user_reply(&db, "t4", "yes", Vec::new(), Some(&asked), 7).unwrap();
        assert_eq!(start_reply_thread(&db, "t4", &reply.text, Some(&asked), None, &members, 7).unwrap(), ["chef"]);
        assert!(
            claim_next(&db, 8)
                .unwrap()
                .unwrap()
                .reason
                .starts_with("The user answered your message in the group:\n\n\"> @Gym Coach, protein?")
        );
        let (named, _) = add_user_reply(&db, "t5", "@Filo too", Vec::new(), Some(&asked), 9).unwrap();
        assert_eq!(start_reply_thread(&db, "t5", &named.text, Some(&asked), None, &members, 9).unwrap(), [FILO_AGENT]);
        // The user's own message answered: the one leading.
        assert_eq!(start_reply_thread(&db, "t6", &again.text, Some(&reply), None, &members, 10).unwrap(), [FILO_AGENT]);
    }

    #[test]
    fn the_filo_agent_speaks_by_its_id_in_what_older_builds_kept() {
        // An older build's rows, with Filo as "filo": moved to its id the next time the file opens, once.
        let path = crate::db::tests::temp_path("group-old-filo");
        let db = Db::open(&path).unwrap();
        let turn = enqueue(&db, "t1", OLD_FILO, 1, "yours", true, 1).unwrap().unwrap();
        post(&db, OLD_FILO, "Pasta?", Asks::default(), Some(&turn), 2).unwrap();
        db.with(|conn| conn.execute("DELETE FROM app_state WHERE key = 'filo_speaker_moved'", [])).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let (messages, _) = messages(&db, 10).unwrap();
        assert_eq!(
            (messages[0].speaker.as_deref(), active(&db).unwrap()[0].speaker.as_str()),
            (Some(FILO_AGENT), FILO_AGENT)
        );
        // A chat file's group call names it as the id too.
        let call: crate::store::GroupCall = serde_json::from_value(
            serde_json::json!({ "speaker": "filo", "from": { "speaker": "filo", "name": "Filo" }, "ask": "Pasta?" }),
        )
        .unwrap();
        assert_eq!((call.speaker.as_str(), call.from.unwrap().speaker.as_str()), (FILO_AGENT, FILO_AGENT));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_members_post_answers_what_gave_it_the_turn() {
        let path = crate::db::tests::temp_path("group-answers");
        let db = Db::open(&path).unwrap();
        crate::app_data::add_agent(&db, &crate::app_data::tests::agent("chef", "Chef", None, 1.)).unwrap();
        // The user asks; the one leading answers them and names Chef, whose post answers that.
        let (asked, _) = add_user_message(&db, "t1", "Dinner?", 1).unwrap();
        let filo = enqueue(&db, "t1", FILO_AGENT, 1, "yours", true, 1).unwrap().unwrap();
        let handed = post(&db, FILO_AGENT, "@Chef what's for dinner?", asking(&["Chef"]), Some(&filo), 2).unwrap();
        let chef = enqueue(&db, "t1", "chef", 2, "asked", false, 3).unwrap().unwrap();
        let answer = post(&db, "chef", "Pasta.", Asks::default(), Some(&chef), 4).unwrap();
        // A check-in nobody asked for, and a post with no turn, answer nothing.
        let check_in = enqueue(&db, "t2", "chef", 1, "check-in", false, 5).unwrap().unwrap();
        let quiet = [
            post(&db, "chef", "Shopping done.", Asks::default(), Some(&check_in), 6).unwrap(),
            post(&db, "chef", "Hi.", Asks::default(), None, 7).unwrap(),
        ];
        let ids = [&asked, &handed, &answer, &quiet[0], &quiet[1]].map(|message| message.id.as_str());
        let replies = replies_to(&db, &ids).unwrap();
        assert_eq!(
            replies,
            std::collections::HashMap::from([
                (handed.id.clone(), asked.id.clone()),
                (answer.id.clone(), handed.id.clone())
            ])
        );
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn naming_agents_queues_each_and_tells_them_who_else_answers() {
        let db = Db::in_memory().unwrap();
        let members = members();
        add_user_message(&db, "t", "@Chef and @Gym Coach: ideas?", 1).unwrap();
        assert_eq!(
            start_thread(&db, "t", "@Chef and @Gym Coach: ideas?", Some("Sent from a page."), &members, 1).unwrap(),
            ["chef", "coach"]
        );
        let first = active(&db).unwrap().remove(0);
        assert!(
            first.reason.contains("Gym Coach was asked too, and you all start now, at the same time"),
            "{}",
            first.reason
        );
        assert!(first.reason.ends_with("\n\nSent from a page."), "{}", first.reason);
        assert!(drop_speaker(&db, "coach", 3).unwrap() == 1 && active(&db).unwrap().len() == 1);
    }

    #[test]
    fn several_asked_by_name_start_together_each_taking_its_own_part() {
        let members = members();
        let db = Db::in_memory().unwrap();
        ensure(&db, 1).unwrap();
        // The user names three: all queued together, all claimed at once, none waiting on another.
        let text = "@Chef @Gym Coach Pro @Filo ship the release";
        add_user_message(&db, "t", text, 1).unwrap();
        assert_eq!(start_thread(&db, "t", text, None, &members, 1).unwrap(), ["chef", "pro", FILO_AGENT]);
        assert_eq!(queued(&db), pairs(&[("chef", true), ("pro", true), (FILO_AGENT, true)]));
        let turns = active(&db).unwrap();
        assert!(turns.iter().all(|turn| turn.at_once.is_some() && turn.at_once == turns[0].at_once));
        // Each is told who else was asked, to take its own part, and that the first one named owns a shared change.
        let reason = |speaker: &str| turns.iter().find(|turn| turn.speaker == speaker).unwrap().reason.clone();
        let chef = reason("chef");
        assert!(chef.starts_with(&format!("The user wrote to you in the group:\n\n\"{text}\"\n\n")), "{chef}");
        assert!(
            chef.contains("Gym Coach Pro and Filo were asked too, and you all start now, at the same time"),
            "{chef}"
        );
        assert!(
            chef.contains("Take your own part of it") && chef.contains("rather than all doing the same thing"),
            "{chef}"
        );
        assert!(chef.ends_with("A change to the same files or git history (edits, commits, branches, merges) is yours to make: they leave it to you."), "{chef}");
        let pro = reason("pro");
        assert!(pro.contains("Chef and Filo were asked too"), "{pro}");
        assert!(
            pro.ends_with(
                "is Chef's to make: don't make it yourself, say in your post what you'd change and leave it to Chef."
            ),
            "{pro}"
        );
        assert!(reason(FILO_AGENT).contains("Chef and Gym Coach Pro were asked too"));
        assert_eq!(claimed_together(&db, 2), sorted(&["chef", "pro", FILO_AGENT]));
        assert!(claim_next(&db, 2).unwrap().is_none());
        let filo = active(&db).unwrap().into_iter().find(|turn| turn.speaker == FILO_AGENT).unwrap();
        for turn in active(&db).unwrap().into_iter().filter(|turn| turn.speaker != FILO_AGENT) {
            finish(&db, &turn.id, "completed", None, 3).unwrap();
        }

        // A member's ask list naming two: both next, at once, Gym Coach (asked first) owning a shared change.
        post(&db, FILO_AGENT, "Workouts and dinners for the week?", asking(&["Gym Coach", "Chef"]), Some(&filo.id), 4)
            .unwrap();
        follow(&db, &filo, &members, 5).unwrap();
        finish(&db, &filo.id, "completed", None, 5).unwrap();
        let turns = active(&db).unwrap();
        assert_eq!(
            turns.iter().map(|turn| (turn.speaker.as_str(), turn.chain)).collect::<Vec<_>>(),
            [("coach", 2), ("chef", 2)]
        );
        assert!(
            turns[0].reason.starts_with("Filo asked you something in the group.\n\nChef was asked too")
                && turns[0].reason.ends_with("is yours to make: they leave it to you."),
            "{}",
            turns[0].reason
        );
        assert!(
            turns[1].reason.contains("Gym Coach was asked too") && turns[1].reason.contains("is Gym Coach's to make"),
            "{}",
            turns[1].reason
        );
        assert_eq!(claimed_together(&db, 6), sorted(&["coach", "chef"]));
        stop_queued(&db, 6).unwrap();
        for turn in active(&db).unwrap() {
            finish(&db, &turn.id, "completed", None, 6).unwrap();
        }

        // Asked from another chat by name, two at once; one asked alone runs on its own, told nothing more.
        let outcome =
            ask(&db, &members, "chef-chat", "chef", "Who has the shopping list?", &["Filo", "Gym Coach"], 7).unwrap();
        assert_eq!(outcome.asked, ["Filo", "Gym Coach"]);
        let turns = active(&db).unwrap();
        assert!(turns[0].reason.contains("Gym Coach was asked too") && turns[1].reason.contains("Filo was asked too"));
        assert_eq!(claimed_together(&db, 8), sorted(&[FILO_AGENT, "coach"]));
        for turn in active(&db).unwrap() {
            finish(&db, &turn.id, "completed", None, 8).unwrap();
        }
        ask(&db, &members, "chef-chat", "chef", "Protein?", &["Gym Coach"], 9).unwrap();
        let turn = active(&db).unwrap().remove(0);
        assert_eq!(
            (turn.at_once, turn.reason.as_str()),
            (None, "Chef asked you something in the group, from their chat with the user.")
        );
    }

    /// Who `start_thread` queued, in order, with whether the user waits on each.
    fn queued(db: &Db) -> Vec<(String, bool)> {
        active(db).unwrap().into_iter().map(|turn| (turn.speaker, turn.answers_user)).collect()
    }

    fn pairs(list: &[(&str, bool)]) -> Vec<(String, bool)> {
        list.iter().map(|(speaker, answers)| (speaker.to_string(), *answers)).collect()
    }

    #[test]
    fn filo_takes_what_names_no_one_and_is_told_to_name_who_its_for() {
        let db = Db::in_memory().unwrap();
        let members = members();
        ensure(&db, 1).unwrap();
        let ask = "which one of you is working on voice messages?";
        add_user_message(&db, "t", ask, 1).unwrap();
        assert_eq!(start_thread(&db, "t", ask, None, &members, 1).unwrap(), [FILO_AGENT]);
        let turn = claim_next(&db, 2).unwrap().unwrap();
        assert!(turn.answers_user);
        assert!(turn.reason.contains("naming no one") && turn.reason.contains(ask), "{}", turn.reason);
        assert!(
            turn.reason.contains("ask them in send_message's `ask`")
                && turn.reason.contains("don't answer for them")
                && turn.reason.contains("ask everyone"),
            "{}",
            turn.reason
        );
        assert!(!checking(&turn, call_of(&db, &turn).unwrap().as_ref()));
        // Filo names Chef, who answers next, asked by Filo.
        post(&db, FILO_AGENT, "Chef, that's yours, isn't it?", asking(&["Chef"]), Some(&turn.id), 3).unwrap();
        follow(&db, &turn, &members, 4).unwrap();
        finish(&db, &turn.id, "completed", None, 4).unwrap();
        let chef = claim_next(&db, 5).unwrap().unwrap();
        assert_eq!(
            (chef.speaker.as_str(), chef.answers_user, chef.reason.as_str()),
            ("chef", false, "Filo asked you something in the group.")
        );
        assert!(!checking(&chef, call_of(&db, &chef).unwrap().as_ref()));
    }

    #[test]
    fn what_names_no_one_never_reaches_every_member() {
        let db = Db::in_memory().unwrap();
        let members = members();
        ensure(&db, 1).unwrap();
        // Only the one leading takes it, and has to answer.
        let ask = "which one of you is working on voice messages?";
        add_user_message(&db, "t", ask, 1).unwrap();
        assert_eq!(start_thread(&db, "t", ask, None, &members, 1).unwrap(), [FILO_AGENT]);
        assert_eq!(queued(&db), pairs(&[(FILO_AGENT, true)]));
        stop_queued(&db, 2).unwrap();

        // Named, only they answer, and have to; a reply naming no one is for who posted what it answers.
        add_user_message(&db, "u", "@Chef dinner?", 3).unwrap();
        assert_eq!(start_thread(&db, "u", "@Chef dinner?", None, &members, 3).unwrap(), ["chef"]);
        assert_eq!(queued(&db), pairs(&[("chef", true)]));
        stop_queued(&db, 4).unwrap();
        let posted = post(&db, "coach", "Legs today?", Asks::default(), None, 5).unwrap();
        let (reply, _) = add_user_reply(&db, "v", "yes", Vec::new(), Some(&posted), 6).unwrap();
        assert_eq!(start_reply_thread(&db, "v", &reply.text, Some(&posted), None, &members, 6).unwrap(), ["coach"]);
        stop_queued(&db, 7).unwrap();

        // Alone in the group, Filo just answers, "@everyone" or not.
        let alone = &members[..1];
        add_user_message(&db, "w", "hi", 8).unwrap();
        assert_eq!(start_thread(&db, "w", "hi", None, alone, 8).unwrap(), [FILO_AGENT]);
        assert_eq!(queued(&db), pairs(&[(FILO_AGENT, true)]));
        stop_queued(&db, 9).unwrap();
        add_user_message(&db, "x", "@everyone hi", 10).unwrap();
        assert_eq!(start_thread(&db, "x", "@everyone hi", None, alone, 10).unwrap(), [FILO_AGENT]);
        assert_eq!(queued(&db), pairs(&[(FILO_AGENT, true)]));
        assert!(active(&db).unwrap()[0].at_once.is_none());
    }

    /// The speakers of the turns claimed together with the oldest queued one (`claim_alongside`), sorted.
    fn claimed_together(db: &Db, now: i64) -> Vec<String> {
        let first = claim_next(db, now).unwrap().unwrap();
        let mut speakers: Vec<String> =
            claim_alongside(db, &first, now).unwrap().into_iter().map(|turn| turn.speaker).collect();
        speakers.push(first.speaker);
        speakers.sort();
        speakers
    }

    fn sorted(ids: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn at_everyone_reaches_every_member_at_once() {
        assert!(
            names_everyone("@everyone standup?")
                && names_everyone("Hi @Everyone, standup")
                && names_everyone("(@everyone)")
        );
        assert!(
            !names_everyone("mail team@everyone.com")
                && !names_everyone("@everyones")
                && !names_everyone("everyone, standup")
        );
        let members = members();
        let db = Db::in_memory().unwrap();
        ensure(&db, 1).unwrap();
        add_user_message(&db, "t", "@everyone standup?", 1).unwrap();
        assert_eq!(
            start_thread(&db, "t", "@everyone standup?", None, &members, 1).unwrap(),
            [FILO_AGENT, "coach", "pro", "chef"]
        );
        assert_eq!(queued(&db), pairs(&[(FILO_AGENT, false), ("coach", false), ("pro", false), ("chef", false)]));
        let turns = active(&db).unwrap();
        assert!(turns.iter().all(|turn| turn.at_once.is_some() && turn.at_once == turns[0].at_once));
        for turn in &turns {
            assert!(
                turn.reason.starts_with("The user wrote to everyone in the group (@everyone):")
                    && turn.reason.contains("at the same time"),
                "{}",
                turn.reason
            );
            assert!(turn.reason.contains("without sending anything"), "{}", turn.reason);
            assert!(checking(turn, call_of(&db, turn).unwrap().as_ref()));
        }
        // They all run at the same time: claiming the first takes the others with it.
        assert_eq!(claimed_together(&db, 2), sorted(&[FILO_AGENT, "coach", "pro", "chef"]));
        assert!(active(&db).unwrap().iter().all(|turn| turn.state == "running"));
        assert!(claim_next(&db, 2).unwrap().is_none());
        for turn in active(&db).unwrap() {
            finish(&db, &turn.id, "completed", None, 3).unwrap();
        }

        // Named as well, Chef has to answer; the rest check; all at once.
        add_user_message(&db, "u", "@Chef and @everyone: dinner?", 4).unwrap();
        assert_eq!(
            start_thread(&db, "u", "@Chef and @everyone: dinner?", None, &members, 4).unwrap(),
            ["chef", FILO_AGENT, "coach", "pro"]
        );
        assert_eq!(queued(&db), pairs(&[("chef", true), (FILO_AGENT, false), ("coach", false), ("pro", false)]));
        let turns = active(&db).unwrap();
        assert_eq!(turns[0].reason, "The user wrote to you in the group:\n\n\"@Chef and @everyone: dinner?\"");
        let checks: Vec<bool> = turns.iter().map(|turn| checking(turn, call_of(&db, turn).unwrap().as_ref())).collect();
        assert_eq!(checks, [false, true, true, true]);
        assert_eq!(claimed_together(&db, 5), sorted(&[FILO_AGENT, "coach", "pro", "chef"]));
        stop_queued(&db, 5).unwrap();

        // Named without "@everyone", several start at once too; one named alone runs on its own.
        let db = Db::in_memory().unwrap();
        add_user_message(&db, "t", "@Chef and @Gym Coach: ideas?", 1).unwrap();
        start_thread(&db, "t", "@Chef and @Gym Coach: ideas?", None, &members, 1).unwrap();
        assert_eq!(claimed_together(&db, 2), sorted(&["chef", "coach"]));
        add_user_message(&db, "u", "@Chef dinner?", 3).unwrap();
        start_thread(&db, "u", "@Chef dinner?", None, &members, 3).unwrap();
        assert!(active(&db).unwrap().iter().filter(|turn| turn.state == "queued").all(|turn| turn.at_once.is_none()));

        // A member's @everyone: every other member checks it at once next (and Chef, named, is asked).
        let db = Db::in_memory().unwrap();
        add_user_message(&db, "u", "plan the week", 3).unwrap();
        start_thread(&db, "u", "plan the week", None, &members, 3).unwrap();
        let filo = claim_next(&db, 4).unwrap().unwrap();
        post(
            &db,
            FILO_AGENT,
            "Anything for this week? Chef, dinners too",
            asking(&["everyone", "Chef"]),
            Some(&filo.id),
            5,
        )
        .unwrap();
        follow(&db, &filo, &members, 6).unwrap();
        finish(&db, &filo.id, "completed", None, 6).unwrap();
        let turns = active(&db).unwrap();
        assert_eq!(
            turns.iter().map(|turn| (turn.speaker.as_str(), turn.chain, turn.answers_user)).collect::<Vec<_>>(),
            [("chef", 2, false), ("coach", 2, false), ("pro", 2, false)]
        );
        assert_eq!(turns[0].reason, "Filo asked you something in the group.");
        assert!(
            turns[1].reason.starts_with("Filo asked everyone in the group. Answer only if it's for you"),
            "{}",
            turns[1].reason
        );
        let checks: Vec<bool> = turns.iter().map(|turn| checking(turn, call_of(&db, turn).unwrap().as_ref())).collect();
        assert_eq!(checks, [false, true, true]);
        assert_eq!(claimed_together(&db, 7), sorted(&["chef", "coach", "pro"]));
        stop_queued(&db, 7).unwrap();

        // From another chat: everyone but the asker, at once.
        let outcome = ask(&db, &members, "chef-chat", "chef", "Who has the shopping list?", &["everyone"], 8).unwrap();
        assert_eq!(outcome.asked, ["Filo", "Gym Coach", "Gym Coach Pro"]);
        assert!(
            active(&db)
                .unwrap()
                .iter()
                .filter(|turn| turn.state == "queued")
                .all(|turn| !turn.answers_user && turn.reason.contains("asked everyone"))
        );
        assert_eq!(claimed_together(&db, 9), sorted(&[FILO_AGENT, "coach", "pro"]));
    }

    #[test]
    fn a_post_naming_no_one_goes_to_the_routing_call_and_whoever_it_picks_answers() {
        let db = Db::in_memory().unwrap();
        let members = members();
        ensure(&db, 1).unwrap();
        // Naming no one, a Reply to the user's own post included: the call picks. Names, @everyone and a Reply to a
        // member's post say who answers themselves; alone in the group, the one member just answers.
        assert!(needs_route("Can build 18 go out tonight?", None, &members));
        assert!(!needs_route("@Chef dinner?", None, &members));
        assert!(!needs_route("@everyone dinner?", None, &members));
        let chefs = post(&db, "chef", "Pasta tonight.", Asks::default(), None, 1).unwrap();
        assert!(!needs_route(&crate::store::with_quote("Pasta tonight.", "Again?"), Some(&chefs), &members));
        assert!(!needs_route("Dinner?", None, &members[..1]));
        // It picks Chef and Gym Coach: both answer, at once, each told it's theirs and who else answers.
        add_user_message(&db, "t", "Dinner and a workout?", 2).unwrap();
        crate::group_route::queue(&db, "t", Some("From the Coding page."), 2).unwrap();
        assert!(crate::group_route::routing(&db, "t"));
        let picked = ["chef".to_string(), "coach".to_string(), "nobody".to_string()];
        assert_eq!(
            routed(&db, "t", "Dinner and a workout?", Some(&picked), Some("From the Coding page."), &members, 3)
                .unwrap(),
            ["chef", "coach"]
        );
        assert!(!crate::group_route::routing(&db, "t"));
        assert_eq!(queued(&db), pairs(&[("chef", true), ("coach", true)]));
        let chef = claim_next(&db, 4).unwrap().unwrap();
        assert!(
            chef.reason.contains("it's for you:")
                && chef.reason.contains("Gym Coach was asked too")
                && chef.reason.ends_with("From the Coding page."),
            "{}",
            chef.reason
        );
        assert!(chef.passes_on.is_none());
        let coach = claim_alongside(&db, &chef, 4).unwrap();
        assert_eq!(coach.iter().map(|turn| turn.speaker.as_str()).collect::<Vec<_>>(), ["coach"]);
        finish(&db, &chef.id, "completed", None, 4).unwrap();
        finish(&db, &coach[0].id, "completed", None, 4).unwrap();
        // Nobody picked ("thanks"): nobody answers.
        add_user_message(&db, "u", "thanks", 5).unwrap();
        assert!(routed(&db, "u", "thanks", Some(&[]), None, &members, 5).unwrap().is_empty());
        assert!(queued(&db).is_empty());
        // The call failed: the one leading takes it, as before there was routing.
        add_user_message(&db, "v", "What's on today?", 6).unwrap();
        assert_eq!(routed(&db, "v", "What's on today?", None, None, &members, 6).unwrap(), [FILO_AGENT]);
        assert!(active(&db).unwrap()[0].reason.contains("naming no one, so it's yours to take"));
    }

    #[test]
    fn the_group_is_off_until_its_set_up_and_an_older_ones_stays_on_with_everyone() {
        let db = Db::in_memory().unwrap();
        assert_eq!(settings(&db).unwrap(), Settings::default());
        assert!(!is_on(&db));
        // A group an older Filo made (no state): on, every agent in it.
        ensure(&db, 1).unwrap();
        let older = settings(&db).unwrap();
        assert!(older.on && older.made && older.members.is_none() && older.route.is_none());

        let db = Db::in_memory().unwrap();
        // Set up: picked members and a routing model, still off until it's turned on.
        let route = RouteModel { engine: Engine::Claude, model: "haiku".into(), reasoning: Some("medium".into()) };
        set_settings(&db, Some(&["chef".to_string(), FILO_AGENT.to_string()]), Some(&route), 1).unwrap();
        let set = settings(&db).unwrap();
        assert!(!set.on && set.made);
        assert_eq!(
            (set.members.as_deref(), set.route.as_ref()),
            (Some(&["chef".to_string(), FILO_AGENT.to_string()][..]), Some(&route))
        );
        assert!(switch(&db, true, 2).unwrap());
        assert!(!switch(&db, true, 3).unwrap());
        // Nothing posted yet: no mark.
        assert!(marks(&db, 0).unwrap().is_empty());
        post(&db, "chef", "Pasta tonight.", Asks::default(), None, 4).unwrap();
        enqueue(&db, "t", "chef", 1, "asked", true, 4).unwrap();
        crate::group_route::queue(&db, "r", None, 4).unwrap();
        // Off: what waits doesn't speak, and the chat has a mark; on again, another.
        assert!(switch(&db, false, 5).unwrap());
        assert!(active(&db).unwrap().is_empty() && !crate::group_route::routing(&db, "r"));
        assert!(!is_on(&db));
        assert!(switch(&db, true, 6).unwrap());
        let kinds: Vec<(i64, String)> = marks(&db, 0).unwrap().into_iter().map(|mark| (mark.at, mark.kind)).collect();
        assert_eq!(kinds, [(5, "off".to_string()), (6, "on".to_string())]);
        // Nobody picked: whoever was in it left, a mark too.
        set_settings(&db, Some(&[]), Some(&route), 7).unwrap();
        let last = marks(&db, 7).unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].kind, "members");
    }

    #[test]
    fn after_a_members_message_mentions_replies_and_the_leads_own_go_as_they_say() {
        let db = Db::in_memory().unwrap();
        let members = members();
        ensure(&db, 1).unwrap();
        let mut at = 0;
        let mut send = |reply_to: Option<&GroupMessage>, text: &str| {
            at += 1;
            let thread = format!("t{at}");
            let (message, _) = add_user_reply(&db, &thread, text, Vec::new(), reply_to, at).unwrap();
            let speakers = start_reply_thread(&db, &thread, &message.text, reply_to, None, &members, at).unwrap();
            let queued = queued(&db);
            stop_queued(&db, at).unwrap();
            (speakers, queued)
        };
        let question = post(&db, "chef", "Should I go ahead…?", Asks::default(), None, 0).unwrap();
        // Naming a member: only they answer, and have to.
        assert_eq!(send(None, "@Gym Coach legs today?"), (vec!["coach".to_string()], pairs(&[("coach", true)])));
        post(&db, "chef", "Should I go ahead…?", Asks::default(), None, 0).unwrap();
        assert_eq!(send(None, "@Chef yes"), (vec!["chef".to_string()], pairs(&[("chef", true)])));
        // "@everyone": every member checks, Chef too.
        post(&db, "chef", "Should I go ahead…?", Asks::default(), None, 0).unwrap();
        assert_eq!(
            send(None, "@everyone standup?").1,
            pairs(&[(FILO_AGENT, false), ("coach", false), ("pro", false), ("chef", false)])
        );
        // A reply: its poster answers, the question's or another's.
        let other = post(&db, "coach", "Legs today?", Asks::default(), None, 0).unwrap();
        post(&db, "chef", "Should I go ahead…?", Asks::default(), None, 0).unwrap();
        assert_eq!(send(Some(&other), "sure").1, pairs(&[("coach", true)]));
        post(&db, "chef", "Should I go ahead…?", Asks::default(), None, 0).unwrap();
        assert_eq!(send(Some(&question), "yes").1, pairs(&[("chef", true)]));
        // Without the routing call, what names no one is the one leading's, whoever posted last.
        post(&db, "chef", "Dinner's at eight.", Asks::default(), None, 0).unwrap();
        assert_eq!(send(None, "ok").1, pairs(&[(FILO_AGENT, true)]));
        add_user_message(&db, "solo", "hi", 90).unwrap();
        assert_eq!(send(None, "anyone around?").1, pairs(&[(FILO_AGENT, true)]));
    }

    #[test]
    fn the_one_leading_reads_what_each_member_does_and_is_busy_with() {
        use crate::coding::{NewTask, Project};
        use crate::coding_handoff::{self, NewHandoff};
        let db = Db::in_memory().unwrap();
        let store_dir = crate::test_support::temp_dir("group-work");
        let store = Store::at(store_dir.to_path_buf());
        let members = members();
        let day = 24 * 3_600_000;
        let now = 10 * day;
        // Chef's task running (started in its chat), Coach's done an hour ago (started in the group), Filo's own (its
        // chats are Filo's), and one of Chef's finished a week ago, too old to count.
        let task = |title: &str, agent: Option<&str>, speaker: Option<&str>, at: i64| {
            let project = Project {
                name: "filo-desktop".into(),
                path: "/p/filo".into(),
                branch: None,
                remote: None,
                plain_folder: true,
                added: true,
            };
            let made = crate::coding::create_task(
                &db,
                &NewTask {
                    brief: title.into(),
                    tool: "claude-code".into(),
                    project,
                    worktree: false,
                    owner: "Chef".into(),
                },
                at,
            )
            .unwrap();
            let new = NewHandoff {
                chat_id: "a-chat".into(),
                agent_id: agent.map(str::to_string),
                speaker: speaker.map(str::to_string),
                title: title.into(),
                goal: title.into(),
                done_when: "done".into(),
                notes: None,
                look_only: false,
                model: None,
                effort: None,
            };
            coding_handoff::create(&db, &made.id, &new, at).unwrap().id
        };
        let voice = task("Voice messages play inline", Some("chef"), None, now - 40 * 60_000);
        crate::coding::set_running(&db, &voice, now - 39 * 60_000).unwrap();
        let legs = task("Leg day plan", None, Some("coach"), now - 2 * 3_600_000);
        crate::coding::finish_round(&db, &legs, "done", None, None, now - 3_600_000).unwrap();
        task("Filo's own work", Some(FILO_AGENT), None, now - 60_000);
        let old = task("Old menu", Some("chef"), None, now - 8 * day);
        crate::coding::finish_round(&db, &old, "done", None, None, now - 7 * day).unwrap();
        // What the user last wrote in Chef's own chat (the group's asks and the app's own messages aside).
        let chef_chat = store.create_chat(Engine::Claude).unwrap().id;
        let mut typed = crate::store::Message::new(Role::User, "make  voice\nmessages play inline", Some("1".into()));
        typed.at = (now - 2 * 3_600_000) as u64;
        store.append(&chef_chat, &typed).unwrap();
        let mut call = crate::store::Message::new(Role::User, "group ask", Some("2".into()));
        call.group_call = Some(crate::store::GroupCall {
            speaker: "chef".into(),
            from: None,
            message_id: None,
            ask: "Dinner?".into(),
            context: String::new(),
            answers_user: true,
            after_question: false,
            checking: false,
        });
        store.append(&chef_chat, &call).unwrap();
        store.add_hidden_message(&chef_chat, "Coding task finished", "3", None).unwrap();

        let chats = vec![("chef".to_string(), chef_chat.clone()), (FILO_AGENT.to_string(), "filo-chat".to_string())];
        let work = work(&db, &store, &members, &chats, FILO_AGENT, now);
        assert_eq!(
            work["chef"],
            [
                "Coding task \"Voice messages play inline\" in filo-desktop: running, started 40 min ago",
                "The user last wrote to it in its own chat 2 hr ago: \"make voice messages play inline\"",
            ]
        );
        assert_eq!(work["coach"], ["Coding task \"Leg day plan\" in filo-desktop: done 1 hr ago"]);
        // Filo leads: its own isn't there.
        assert!(!work.contains_key(FILO_AGENT) && !work.contains_key("pro"));
        assert!(
            super::work(&db, &store, &members, &chats, "chef", now)[FILO_AGENT][0]
                .starts_with("Coding task \"Filo's own work\"")
        );

        // Filo's turn lists them under Members, with more of what each does.
        let turn = Turn {
            id: "x".into(),
            thread: "t".into(),
            speaker: FILO_AGENT.into(),
            chain: 1,
            reason: "r".into(),
            answers_user: true,
            state: "running".into(),
            error: None,
            created_at: 0,
            passes_on: None,
            at_once: None,
        };
        let text = own_chat_prompt(&turn, &members, &work, &[], None, &chrono::Utc, now);
        assert!(text.contains("- @Chef: Cooks.\n  - Coding task \"Voice messages play inline\" in filo-desktop: running, started 40 min ago\n  - The user last wrote"), "{text}");
        assert!(text.contains("- @Gym Coach: Plans workouts.\n  - Coding task \"Leg day plan\""), "{text}");
        assert!(text.contains("name them in send_message's `ask`") && text.contains("wakes no one"), "{text}");
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn an_ask_from_a_chat_posts_queues_the_named_and_waits_for_the_thread_to_end() {
        let db = Db::in_memory().unwrap();
        let members = members();
        // Asking nobody else posts nothing, whoever its text @-names, and says whom it could ask.
        let none = ask(&db, &members, "chef-chat", "chef", "@Filo just me", &["Chef"], 1).unwrap();
        assert!(none.asked.is_empty() && none.could_ask == ["Filo", "Gym Coach", "Gym Coach Pro"]);
        assert!(messages(&db, 10).unwrap().0.is_empty() && session(&db).unwrap().is_none());
        assert!(ask(&db, &members, "chef-chat", "chef", "  ", &["Filo"], 1).is_err());

        // Chef asks Filo and Gym Coach from its own chat: the group is made, the message is Chef's, both are queued.
        let outcome = ask(
            &db,
            &members,
            "chef-chat",
            "chef",
            "@Filo @Gym Coach what should they eat after legs?",
            &["Filo", "Gym Coach"],
            2,
        )
        .unwrap();
        assert_eq!(outcome.asked, ["Filo", "Gym Coach"]);
        assert!(session(&db).unwrap().is_some());
        let (posted, _) = messages(&db, 10).unwrap();
        assert_eq!((posted[0].speaker.as_deref(), posted[0].role), (Some("chef"), Role::Assistant));
        let thread = posted[0].thread.clone().unwrap();
        assert_eq!(
            active(&db).unwrap().iter().map(|turn| turn.speaker.as_str()).collect::<Vec<_>>(),
            [FILO_AGENT, "coach"]
        );

        // Not answered while anyone in the thread is still to speak.
        assert!(asks_answered(&db).unwrap().is_empty());
        let filo = claim_next(&db, 4).unwrap().unwrap();
        assert!(filo.reason.starts_with("Chef asked you something in the group"));
        post(&db, FILO_AGENT, "Protein and carbs.", Asks::default(), Some(&filo.id), 5).unwrap();
        finish(&db, &filo.id, "completed", None, 6).unwrap();
        assert!(asks_answered(&db).unwrap().is_empty());
        let coach = claim_next(&db, 7).unwrap().unwrap();
        finish(&db, &coach.id, "completed", None, 8).unwrap();
        let answered = asks_answered(&db).unwrap();
        assert_eq!(answered, [Ask { thread: thread.clone(), origin_chat: "chef-chat".into() }]);
        assert!(claim_report(&db, &thread, 9).unwrap());
        assert!(!claim_report(&db, &thread, 9).unwrap());
        assert!(asks_answered(&db).unwrap().is_empty());

        let report = report_message(&thread_messages(&db, &thread).unwrap(), &members, None, &chrono::Utc, 10);
        assert!(
            report.contains("Chef: @Filo @Gym Coach what should")
                && report.contains("Filo: Protein and carbs.")
                && report.contains("send_message"),
            "{report}"
        );
        assert!(!report.contains("Nobody answered"));
        let silent =
            report_message(&thread_messages(&db, &thread).unwrap()[..1], &members, Some("Team"), &chrono::Utc, 10);
        assert!(
            silent.contains("(Nobody answered.)") && silent.contains("asked in \"Team\", the group chat"),
            "{silent}"
        );
        assert!(report.contains("asked in the group chat"));
    }

    #[test]
    fn the_group_keeps_its_name_and_picture_and_a_blank_name_is_group() {
        let db = Db::in_memory().unwrap();
        assert!(set_look(&db, &Look { name: Some("Team".into()), ..Look::default() }).is_err());
        assert_eq!(look(&db).unwrap().title(), DEFAULT_NAME);
        ensure(&db, 1).unwrap();
        let team = Look {
            name: Some("  The Team  ".into()),
            picture: Some("helper:mint".into()),
            background: Some("#4FB3B8".into()),
        };
        set_look(&db, &team).unwrap();
        assert_eq!(
            look(&db).unwrap(),
            Look {
                name: Some("The Team".into()),
                picture: Some("helper:mint".into()),
                background: Some("#4FB3B8".into())
            }
        );
        set_look(&db, &Look { name: Some("   ".into()), picture: Some(String::new()), background: None }).unwrap();
        assert_eq!(look(&db).unwrap(), Look::default());
        assert_eq!(look(&db).unwrap().title(), "Group");
        set_look(&db, &Look { name: Some("x".repeat(40)), ..Look::default() }).unwrap();
        assert_eq!(look(&db).unwrap().title().chars().count(), MAX_NAME_CHARS);
        // Opened again, the columns are there already.
        db.with(ensure_columns).unwrap();
    }

    #[test]
    fn the_roster_and_who_speaks_from_a_chat_come_from_the_agents() {
        use crate::app_data::{self, tests::agent};
        let db = Db::in_memory().unwrap();
        assert_eq!(roster(&db), [Member { id: FILO_AGENT.into(), name: "Filo".into(), about: String::new() }]);
        let filo = app_data::Agent {
            name: " Jarvis ".into(),
            instructions: "General help.".into(),
            ..agent(FILO_AGENT, "", Some("filo-chat"), 1.)
        };
        let agents = [
            app_data::Agent { instructions: "Cooks.".into(), ..agent("chef", "Chef", Some("chef-chat"), 3.) },
            filo.clone(),
            app_data::Agent { instructions: "Trains.".into(), ..agent("coach", "Coach", None, 2.) },
        ];
        for one in &agents {
            app_data::add_agent(&db, one).unwrap();
        }
        // The Filo agent, renamed, is Filo by its own name.
        let members = roster(&db);
        let who: Vec<(&str, &str, &str)> =
            members.iter().map(|member| (member.id.as_str(), member.name.as_str(), member.about.as_str())).collect();
        assert_eq!(
            who,
            [(FILO_AGENT, "Jarvis", "General help."), ("coach", "Coach", "Trains."), ("chef", "Chef", "Cooks.")]
        );
        // Chef picked as the main agent leads; without the Filo agent, Filo isn't in the group.
        app_data::set_profile(&db, app_data::MAIN_AGENT, Some(&"chef".into())).unwrap();
        let ids = |db: &Db| roster(db).into_iter().map(|member| member.id).collect::<Vec<_>>();
        assert_eq!(ids(&db), ["chef", FILO_AGENT, "coach"]);
        app_data::remove_agent(&db, FILO_AGENT).unwrap();
        assert_eq!(ids(&db), ["chef", "coach"]);
        assert!(mentioned("@Filo?", &roster(&db)).is_empty());
        app_data::add_agent(&db, &filo).unwrap();
        assert_eq!(speaker_of_chat(&db, "chef-chat"), "chef");
        assert_eq!(speaker_of_chat(&db, "filo-chat"), FILO_AGENT);
        assert_eq!(speaker_of_chat(&db, "some-filo-chat"), FILO_AGENT);
    }

    #[test]
    fn the_prompt_has_the_roster_the_transcript_and_the_reason() {
        let members = members();
        let messages = vec![
            GroupMessage {
                id: "1".into(),
                role: Role::User,
                speaker: None,
                text: "Hi all".into(),
                at: 0,
                thread: Some("t".into()),
                turn: None,
                attachments: Vec::new(),
                asks: Asks::default(),
            },
            GroupMessage {
                id: "2".into(),
                role: Role::Assistant,
                speaker: Some(FILO_AGENT.into()),
                text: "@Chef over to you".into(),
                at: 60_000,
                thread: Some("t".into()),
                turn: None,
                attachments: Vec::new(),
                asks: Asks::default(),
            },
        ];
        let turn = Turn {
            id: "x".into(),
            thread: "t".into(),
            speaker: "chef".into(),
            chain: 2,
            reason: "Filo asked you something in the group.".into(),
            answers_user: false,
            state: "running".into(),
            error: None,
            created_at: 0,
            passes_on: None,
            at_once: None,
        };
        let text = prompt(&turn, &members, &Work::new(), &messages, None, &chrono::Utc, 3 * 3_600_000);
        assert!(text.starts_with("[Group chat of Filo"), "{text}");
        assert!(
            prompt(&turn, &members, &Work::new(), &messages, Some("Team"), &chrono::Utc, 0)
                .starts_with("[\"Team\", the group chat of Filo")
        );
        assert!(
            text.contains("- @Chef (you): Cooks.") && text.contains("- @Filo: the user's main assistant"),
            "{text}"
        );
        assert!(
            text.contains(
                "[Thu, Jan 1, 00:00, 3 hr ago] User: Hi all\n[Thu, Jan 1, 00:01, 2 hr ago] Filo: @Chef over to you"
            ),
            "{text}"
        );
        assert!(
            text.contains("Why you're speaking now: Filo asked you something") && text.contains("don't send anything")
        );
        let lead = instructions(&members[0], &members);
        assert!(lead.starts_with("You are Filo, the user's main assistant") && lead.contains("send_message"));
        assert!(instructions(&members[3], &members).ends_with("in the user's words:\n\nCooks."));
        // Chef leading: it answers what names no one, and Filo is one of the agents.
        let chef_leads: Vec<Member> =
            [members[3].clone(), members[0].clone()].into_iter().chain(members[1..3].iter().cloned()).collect();
        assert!(instructions(&chef_leads[0], &chef_leads).starts_with("You are Chef, the user's main agent"));
        assert!(instructions(&chef_leads[1], &chef_leads).starts_with("You are Filo, one of the agents"));
        let text = prompt(&turn, &chef_leads, &Work::new(), &messages, None, &chrono::Utc, 3 * 3_600_000);
        assert!(
            text.contains("- @Chef (you): the user's main agent, who leads the group. Cooks.")
                && text.contains("- @Filo: "),
            "{text}"
        );
        assert_eq!(transcript(&[], &members, &chrono::Utc, 0), "(nothing yet)");
        assert_eq!(ago(3 * 24 * 3_600_000), "3 days ago");
    }
}

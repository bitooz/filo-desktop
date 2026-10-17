// An agent asking another from its own chat with the user while the group is off (or asking one that isn't in it): the
// MCP server's `ask_agents` (mcp.rs). Each one asked works on it in its own chat, as on any message from the user,
// with a chip there opening the asker's chat (store.rs `Message::asked_by`); once its turn ends, what it said goes back
// to the asker's chat, the way a coding task's report does, and the asker passes it on to the user. The asker's chat
// shows "Asked Developer" where it asked and "Developer answered" where the answer came back, each opening the other
// chat (store.rs `GroupLink` with an `agent`).
//
// The asks are rows in `agent_asks` (`ask`, written by the MCP server); the app takes them up, runs the turns and
// reports back (local_backend/group.rs `run_agent_asks`): `queued`, `running`, then `done` or `failed`, and
// `reported_at` once the asker heard.

use rusqlite::{OptionalExtension as _, Row, params};

use crate::db::Db;
use crate::group::Member;
use crate::store::{GroupLink, LinkedMember, Message, Role, Store};

/// The table (group.rs `ensure_columns` makes it).
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agent_asks (
    id          TEXT PRIMARY KEY NOT NULL,
    origin_chat TEXT NOT NULL,
    asker       TEXT NOT NULL,
    agent       TEXT NOT NULL,
    text        TEXT NOT NULL,
    link_id     TEXT,
    chat        TEXT,
    message_id  TEXT,
    turn        TEXT,
    state       TEXT NOT NULL,
    result_id   TEXT,
    error       TEXT,
    created_at  INTEGER NOT NULL,
    started_at  INTEGER,
    finished_at INTEGER,
    reported_at INTEGER
);
CREATE INDEX IF NOT EXISTS agent_asks_by_state ON agent_asks (state, created_at);
";

/// The longest ask.
const MAX_ASK_CHARS: usize = 4_000;
/// How much of the answer goes back to the asker.
const MAX_ANSWER_CHARS: usize = 12_000;

/// One agent asked by another.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub id: String,
    /// The asker's chat, and the asker's agent id.
    pub origin_chat: String,
    pub asker: String,
    /// Who was asked (an agent id), and what.
    pub agent: String,
    pub text: String,
    /// The "Asked …" line in the asker's chat.
    pub link_id: Option<String>,
    /// The asked agent's chat, the ask's message there and its turn, once taken up.
    pub chat: Option<String>,
    pub message_id: Option<String>,
    pub turn: Option<String>,
    pub state: String,
    /// The asked agent's last message in that turn, and why it failed.
    pub result_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

const COLUMNS: &str = "id, origin_chat, asker, agent, text, link_id, chat, message_id, turn, state, result_id, error, created_at, started_at, finished_at";

fn ask_from(row: &Row) -> rusqlite::Result<Ask> {
    Ok(Ask {
        id: row.get(0)?,
        origin_chat: row.get(1)?,
        asker: row.get(2)?,
        agent: row.get(3)?,
        text: row.get(4)?,
        link_id: row.get(5)?,
        chat: row.get(6)?,
        message_id: row.get(7)?,
        turn: row.get(8)?,
        state: row.get(9)?,
        result_id: row.get(10)?,
        error: row.get(11)?,
        created_at: row.get(12)?,
        started_at: row.get(13)?,
        finished_at: row.get(14)?,
    })
}

/// `asker` asks `asked` (agents other than itself) `text` from its chat `origin_chat` in `store`, during its turn
/// `turn`: an ask each, and a line in its chat for each saying so. The names asked.
pub fn ask(
    db: &Db,
    store: &Store,
    origin_chat: &str,
    asker: &str,
    asked: &[Member],
    text: &str,
    turn: Option<&str>,
    now: i64,
) -> Result<Vec<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Write the message for the ones you ask.".into());
    }
    if text.chars().count() > MAX_ASK_CHARS {
        return Err(format!("The message is too long (at most {MAX_ASK_CHARS} characters). Make it shorter."));
    }
    let mut names = Vec::new();
    for member in asked.iter().filter(|member| member.id != asker) {
        let id = uuid::Uuid::new_v4().to_string();
        let mut line =
            Message::new(Role::Assistant, format!("Asked {}: {text}", member.name), turn.map(str::to_string));
        line.group_link = Some(GroupLink {
            kind: "asked".into(),
            who: vec![LinkedMember { speaker: member.id.clone(), name: member.name.clone() }],
            message_id: None,
            agent: Some(member.id.clone()),
            ask: Some(id.clone()),
        });
        store.append(origin_chat, &line).map_err(|error| format!("Couldn't write to this chat: {error}"))?;
        db.write(|tx| {
            tx.execute(
                "INSERT INTO agent_asks (id, origin_chat, asker, agent, text, link_id, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7)",
                params![id, origin_chat, asker, member.id, text, line.id, now],
            )
        })?;
        names.push(member.name.clone());
    }
    Ok(names)
}

/// An ask by its id.
pub fn get(db: &Db, id: &str) -> Result<Option<Ask>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {COLUMNS} FROM agent_asks WHERE id = ?1"), params![id], ask_from).optional()
    })
}

/// The asks in `state`, oldest first.
pub fn in_state(db: &Db, state: &str) -> Result<Vec<Ask>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {COLUMNS} FROM agent_asks WHERE state = ?1 ORDER BY created_at"))?;
        let rows = statement.query_map(params![state], ask_from)?;
        rows.collect()
    })
}

/// Takes a queued ask up (once: whoever claims it runs it).
pub fn claim(db: &Db, id: &str, now: i64) -> Result<bool, String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE agent_asks SET state = 'running', started_at = ?2 WHERE id = ?1 AND state = 'queued'",
            params![id, now],
        )
    })
    .map(|changed| changed > 0)
}

/// The ask's message `message_id` in the asked agent's chat `chat`, starting its turn `turn` there.
pub fn started(db: &Db, id: &str, chat: &str, message_id: &str, turn: &str) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE agent_asks SET chat = ?2, message_id = ?3, turn = ?4 WHERE id = ?1",
            params![id, chat, message_id, turn],
        )
    })
    .map(drop)
}

/// How the asked agent's turn ended: `done`, with its last message, or `failed` with why.
pub fn finish(
    db: &Db,
    id: &str,
    state: &str,
    result_id: Option<&str>,
    error: Option<&str>,
    now: i64,
) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE agent_asks SET state = ?2, result_id = ?3, error = ?4, finished_at = ?5 WHERE id = ?1",
            params![id, state, result_id, error, now],
        )
    })
    .map(drop)
}

/// Asks that ended and whose asker hasn't heard yet, oldest first.
pub fn to_report(db: &Db) -> Result<Vec<Ask>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(&format!("SELECT {COLUMNS} FROM agent_asks WHERE state IN ('done', 'failed') AND reported_at IS NULL ORDER BY finished_at"))?;
        let rows = statement.query_map([], ask_from)?;
        rows.collect()
    })
}

/// Takes an ended ask to report (true once).
pub fn claim_report(db: &Db, id: &str, now: i64) -> Result<bool, String> {
    db.write(|tx| {
        tx.execute("UPDATE agent_asks SET reported_at = ?2 WHERE id = ?1 AND reported_at IS NULL", params![id, now])
    })
    .map(|changed| changed > 0)
}

/// A removed agent's asks go with it: nobody runs them, and nobody waits on them.
pub fn drop_agent(db: &Db, agent: &str, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE agent_asks SET state = 'failed', error = 'The agent was deleted.', finished_at = ?2, reported_at = ?2 WHERE (agent = ?1 OR asker = ?1) AND state IN ('queued', 'running')",
            params![agent, now],
        )
    })
}

/// What the asked agent's engine reads: who asked, from where, and that its answer goes back there.
pub fn call_text(asker: &str, text: &str) -> String {
    format!(
        "[{asker} asked you this from their own chat with the user, in Filo. Work on it here as you would on any message from the user, with your tools and \
coding tasks. Answer with send_message, here in your chat: when your turn ends, what you sent goes back to {asker}, who passes it on to the user.]\n\n{text}"
    )
}

/// What the asker is sent back in its chat: what `name` answered (`replies`, its messages in that turn), or why it couldn't.
pub fn report_text(name: &str, asked: &str, replies: &[String], error: Option<&str>) -> String {
    let answer = replies.join("\n\n");
    let answer = crate::store::cut(answer.trim(), MAX_ANSWER_CHARS);
    let went = match (error, answer.is_empty()) {
        (Some(error), _) => format!("{name} couldn't answer: {error}"),
        (None, true) => format!("{name} finished without answering."),
        (None, false) => format!("{name} answered:\n\n{answer}"),
    };
    format!(
        "↩ What came of what you asked {name} (\"{}\"; the user can read it in {name}'s chat):\n\n{went}\n\n\
(This is {name} answering you, not the user typing. Tell the user what came of it, short and in your own words, with send_message.)",
        crate::store::cut(asked.trim(), 300)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ask_is_a_line_in_the_askers_chat_and_runs_once() {
        let dir = std::env::temp_dir().join(format!("filo-asks-{}", uuid::Uuid::new_v4()));
        let store = Store::at(&dir);
        let chat = store.create_chat(crate::engine::Engine::Claude).unwrap();
        let db = Db::in_memory().unwrap();
        let members = vec![
            Member { id: "designer".into(), name: "App Designer".into(), about: String::new() },
            Member { id: "dev".into(), name: "Developer".into(), about: String::new() },
        ];
        assert!(ask(&db, &store, &chat.id, "designer", &members, "  ", None, 1).is_err());
        // Itself is never asked.
        assert_eq!(
            ask(&db, &store, &chat.id, "designer", &members, "Check the picture at 24 px.", Some("t1"), 2).unwrap(),
            ["Developer"]
        );
        let queued = in_state(&db, "queued").unwrap();
        assert_eq!(queued.len(), 1);
        let asked = &queued[0];
        assert_eq!(
            (asked.agent.as_str(), asked.asker.as_str(), asked.origin_chat.as_str()),
            ("dev", "designer", chat.id.as_str())
        );
        let line = store
            .messages(&chat.id)
            .unwrap()
            .into_iter()
            .find(|message| Some(&message.id) == asked.link_id.as_ref())
            .unwrap();
        let link = line.group_link.unwrap();
        assert_eq!(
            (link.kind.as_str(), link.agent.as_deref(), link.ask.as_deref(), line.turn.as_deref()),
            ("asked", Some("dev"), Some(asked.id.as_str()), Some("t1"))
        );

        assert!(claim(&db, &asked.id, 3).unwrap());
        assert!(!claim(&db, &asked.id, 3).unwrap());
        started(&db, &asked.id, "dev-chat", "m1", "turn-1").unwrap();
        finish(&db, &asked.id, "done", Some("r1"), None, 4).unwrap();
        let ended = to_report(&db).unwrap();
        assert_eq!(
            (ended.len(), ended[0].result_id.as_deref(), ended[0].turn.as_deref()),
            (1, Some("r1"), Some("turn-1"))
        );
        assert!(claim_report(&db, &asked.id, 5).unwrap());
        assert!(!claim_report(&db, &asked.id, 5).unwrap());
        assert!(to_report(&db).unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_asker_hears_the_answer_or_why_there_was_none() {
        let text = report_text("Developer", "Check the picture.", &["At 24 px the faces blur.".into()], None);
        assert!(
            text.contains("Developer answered:\n\nAt 24 px the faces blur.") && text.contains("Check the picture.")
        );
        assert!(
            report_text("Developer", "Check it.", &[], Some("Claude Code: session limit"))
                .contains("Developer couldn't answer: Claude Code: session limit")
        );
        assert!(report_text("Developer", "Check it.", &[], None).contains("finished without answering"));
        assert!(call_text("App Designer", "Check it.").ends_with("Check it."));
    }
}

// The app's own small data, in Filo's database: the agents, the profile (the user's name, the main agent), how far each
// conversation has been read, and the pictures the user uploaded for agents. Each edit
// changes its own row, so nothing reads it all and writes it back, and a Filo that can't read a row (an engine it
// doesn't know) never saves over it.
//
// They were JSON files beside the database (agents.json, profile.json, seen.json, pictures.json), each
// rewritten whole on every edit. On 2026-10-14 an older Filo that didn't know OpenCode read agents.json as empty and
// saved just Filo over the user's 24 agents. `import` copies the files in once, in one transaction, renames each to
// `<name>.migrated` and records that it's done in the database, so an agents.json an older build makes afterwards is
// never read. A file that's there but can't be read stops the import: it stays as it is, and the next open tries again.
//
// Conversations stay files (store.rs): they're appended to, watched as they grow and searched with grep.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Map, Value};

use crate::db::Db;
use crate::engine::Engine;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agents (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    instructions TEXT NOT NULL,
    picture      TEXT NOT NULL,
    background   TEXT,
    session_id   TEXT,
    engine       TEXT,
    model        TEXT,
    earlier_chat TEXT,
    created_at   REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS agents_by_session ON agents (session_id);
CREATE TABLE IF NOT EXISTS profile (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS seen (
    chat_id TEXT PRIMARY KEY NOT NULL,
    until   REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_pictures (
    id       TEXT PRIMARY KEY NOT NULL,
    added_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS app_state (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
";

/// The files `import` copies in, in the order it reads them.
/// (sessions.json, Filo's own chats pinned or archived, is left as it is: nothing shows those chats.)
pub(crate) const FILES: [&str; 4] = ["agents.json", "profile.json", "seen.json", "pictures.json"];
/// `app_state`: when the JSON files were imported (ms). Once it's there they're never read again.
const IMPORTED: &str = "json_imported";
/// `app_state`: the agents are the user's (imported from a file that was there, or fresh data was given Filo): nothing
/// is added for them again, so a removed Filo stays removed.
const AGENTS_STARTED: &str = "agents_started";
/// The profile's main agent (main_agent.rs).
pub const MAIN_AGENT: &str = "mainAgentId";

/// An agent as it's kept. The engine is text, so one a newer Filo saved (on an engine this build doesn't know) is kept
/// and listed as it is (`known_engine`, `newer_engine`).
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub picture: String,
    pub background: Option<String>,
    /// Its conversation, once it has one (agents/chats/<id>).
    pub session_id: Option<String>,
    pub engine: Option<String>,
    pub model: Option<String>,
    /// The conversation of the agent it was duplicated from, which its instructions point to.
    pub earlier_chat: Option<String>,
    pub created_at: f64,
}

impl Agent {
    /// The engine it was picked to answer with, while this build knows it; none for one from before there was a pick
    /// (the app's default engine) and for one a newer Filo saved (`newer_engine`).
    pub fn known_engine(&self) -> Option<Engine> {
        self.engine.as_deref().and_then(Engine::parse)
    }

    /// The engine a newer Filo saved it with, which this build doesn't know: it's listed, marked as needing a newer
    /// Filo, and this build neither changes it nor runs its turns (`refuse_newer`).
    pub fn newer_engine(&self) -> Option<&str> {
        self.engine.as_deref().filter(|engine| Engine::parse(engine).is_none())
    }

    /// An agent a newer Filo saved isn't changed or run here.
    pub fn refuse_newer(&self) -> Result<(), String> {
        match self.newer_engine() {
            Some(_) => Err(format!(
                "{} was made with a newer Filo, on an engine this one doesn't know. Update Filo to change it or talk to it.",
                self.name
            )),
            None => Ok(()),
        }
    }
}

/// What `update_agent` changes; none leaves a field as it is.
#[derive(Clone, Debug, Default)]
pub struct AgentChange {
    pub name: Option<String>,
    pub instructions: Option<String>,
    pub picture: Option<String>,
    /// `Some(None)` resets the colour to the theme's.
    pub background: Option<Option<String>>,
    /// The engine and model, together.
    pub model: Option<(String, Option<String>)>,
}

const AGENT_COLUMNS: &str =
    "id, name, instructions, picture, background, session_id, engine, model, earlier_chat, created_at";

fn agent_row(row: &Row) -> rusqlite::Result<Agent> {
    Ok(Agent {
        id: row.get(0)?,
        name: row.get(1)?,
        instructions: row.get(2)?,
        picture: row.get(3)?,
        background: row.get(4)?,
        session_id: row.get(5)?,
        engine: row.get(6)?,
        model: row.get(7)?,
        earlier_chat: row.get(8)?,
        created_at: row.get(9)?,
    })
}

// --- Agents ---

/// Every agent, oldest first (as the Agents tab lists them).
pub fn agents(db: &Db) -> Result<Vec<Agent>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(&format!("SELECT {AGENT_COLUMNS} FROM agents ORDER BY created_at, rowid"))?;
        statement.query_map([], agent_row)?.collect()
    })
}

pub fn agent(db: &Db, id: &str) -> Result<Option<Agent>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {AGENT_COLUMNS} FROM agents WHERE id = ?1"), [id], agent_row).optional()
    })
}

/// The agent whose conversation `chat_id` is.
pub fn agent_of_chat(db: &Db, chat_id: &str) -> Result<Option<Agent>, String> {
    db.with(|conn| {
        conn.query_row(
            &format!("SELECT {AGENT_COLUMNS} FROM agents WHERE session_id = ?1 ORDER BY created_at LIMIT 1"),
            [chat_id],
            agent_row,
        )
        .optional()
    })
}

fn insert_agent(conn: &Connection, agent: &Agent) -> rusqlite::Result<usize> {
    conn.execute(
        &format!("INSERT OR IGNORE INTO agents ({AGENT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"),
        params![
            agent.id,
            agent.name,
            agent.instructions,
            agent.picture,
            agent.background,
            agent.session_id,
            agent.engine,
            agent.model,
            agent.earlier_chat,
            agent.created_at
        ],
    )
}

/// A new agent (one with its id there already stays as it was).
pub fn add_agent(db: &Db, agent: &Agent) -> Result<(), String> {
    db.write(|tx| insert_agent(tx, agent).map(drop))
}

/// The agent fresh data starts with, unless the agents are the user's already (`AGENTS_STARTED`): added once, so the
/// app and the background scheduler seeding at once still make one, and a removed one stays removed. Whether it was.
pub fn start_agents(db: &Db, first: &Agent) -> Result<bool, String> {
    db.write(|tx| {
        if state(tx, AGENTS_STARTED)?.is_some() || state(tx, IMPORTED)?.is_none() {
            return Ok(false);
        }
        insert_agent(tx, first)?;
        set_state(tx, AGENTS_STARTED, "1")?;
        Ok(true)
    })
}

/// Changes agent `id`'s fields that `change` names, and nothing else; the agent as it is now (none if it's gone).
pub fn update_agent(db: &Db, id: &str, change: &AgentChange) -> Result<Option<Agent>, String> {
    db.write(|tx| {
        let set = |column: &str, value: &dyn rusqlite::ToSql| {
            tx.execute(&format!("UPDATE agents SET {column} = ?1 WHERE id = ?2"), params![value, id])
        };
        if let Some(name) = &change.name {
            set("name", name)?;
        }
        if let Some(instructions) = &change.instructions {
            set("instructions", instructions)?;
        }
        if let Some(picture) = &change.picture {
            set("picture", picture)?;
        }
        if let Some(background) = &change.background {
            set("background", background)?;
        }
        if let Some((engine, model)) = &change.model {
            tx.execute("UPDATE agents SET engine = ?1, model = ?2 WHERE id = ?3", params![engine, model, id])?;
        }
        tx.query_row(&format!("SELECT {AGENT_COLUMNS} FROM agents WHERE id = ?1"), [id], agent_row).optional()
    })
}

/// Agent `id`'s conversation, the first time it's opened; false when the agent is gone.
pub fn set_agent_session(db: &Db, id: &str, session_id: &str) -> Result<bool, String> {
    db.write(|tx| Ok(tx.execute("UPDATE agents SET session_id = ?1 WHERE id = ?2", params![session_id, id])? > 0))
}

/// Removes agent `id`; the agent it was, if it was there.
pub fn remove_agent(db: &Db, id: &str) -> Result<Option<Agent>, String> {
    db.write(|tx| {
        let agent =
            tx.query_row(&format!("SELECT {AGENT_COLUMNS} FROM agents WHERE id = ?1"), [id], agent_row).optional()?;
        tx.execute("DELETE FROM agents WHERE id = ?1", [id])?;
        Ok(agent)
    })
}

/// Agent `id`, removed only if `remove_if` says so of it as it is now; whether it went.
pub fn remove_agent_if(db: &Db, id: &str, remove_if: impl FnOnce(&Agent) -> bool) -> Result<bool, String> {
    db.write(|tx| {
        let Some(agent) =
            tx.query_row(&format!("SELECT {AGENT_COLUMNS} FROM agents WHERE id = ?1"), [id], agent_row).optional()?
        else {
            return Ok(false);
        };
        Ok(remove_if(&agent) && tx.execute("DELETE FROM agents WHERE id = ?1", [id])? > 0)
    })
}

// --- The profile ---

/// The profile's fields as the app keeps them (`userName`, `mainAgentId`, …), each a JSON value.
pub fn profile(db: &Db) -> Result<Map<String, Value>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare("SELECT key, value FROM profile")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        rows.map(|row| row.map(|(key, value)| (key, serde_json::from_str(&value).unwrap_or(Value::Null)))).collect()
    })
}

/// One field of the profile (none takes it away).
pub fn set_profile(db: &Db, key: &str, value: Option<&Value>) -> Result<(), String> {
    db.write(|tx| {
        match value {
            Some(value) => tx.execute("INSERT INTO profile (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value", params![key, value.to_string()])?,
            None => tx.execute("DELETE FROM profile WHERE key = ?1", [key])?,
        };
        Ok(())
    })
}

/// A profile field that's text.
pub fn profile_text(db: &Db, key: &str) -> Result<Option<String>, String> {
    let value: Option<String> =
        db.with(|conn| conn.query_row("SELECT value FROM profile WHERE key = ?1", [key], |row| row.get(0)).optional())?;
    Ok(value
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .and_then(|value| value.as_str().map(str::to_string)))
}

// --- Read markers ---

/// How far each conversation has been read (ms).
pub fn seen(db: &Db) -> Result<HashMap<String, f64>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare("SELECT chat_id, until FROM seen")?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
    })
}

pub fn seen_until(db: &Db, chat_id: &str) -> Result<Option<f64>, String> {
    db.with(|conn| conn.query_row("SELECT until FROM seen WHERE chat_id = ?1", [chat_id], |row| row.get(0)).optional())
}

/// Read up to `until` (ms); never moves back.
pub fn mark_seen(db: &Db, chat_id: &str, until: f64) -> Result<(), String> {
    db.write(|tx| mark_seen_in(tx, chat_id, until).map(drop))
}

fn mark_seen_in(conn: &Connection, chat_id: &str, until: f64) -> rusqlite::Result<usize> {
    conn.execute("INSERT INTO seen (chat_id, until) VALUES (?1, ?2) ON CONFLICT (chat_id) DO UPDATE SET until = max(until, excluded.until)", params![chat_id, until])
}

// --- Uploaded pictures ---

/// The pictures the user uploaded for agents, newest first: (id, added at ms).
pub fn pictures(db: &Db) -> Result<Vec<(String, f64)>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare("SELECT id, added_at FROM agent_pictures ORDER BY added_at DESC, rowid DESC")?;
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
    })
}

pub fn add_picture(db: &Db, id: &str, added_at: f64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("INSERT OR REPLACE INTO agent_pictures (id, added_at) VALUES (?1, ?2)", params![id, added_at])
            .map(drop)
    })
}

pub fn remove_picture(db: &Db, id: &str) -> Result<(), String> {
    db.write(|tx| tx.execute("DELETE FROM agent_pictures WHERE id = ?1", [id]).map(drop))
}

// --- The one-time import ---

pub(crate) fn state(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| row.get(0)).optional()
}

pub(crate) fn set_state(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(drop)
}

/// Whether the JSON files were imported already (then they're never read again).
pub(crate) fn imported(db: &Db) -> Result<bool, String> {
    db.with(|conn| state(conn, IMPORTED)).map(|done| done.is_some())
}

/// One of `FILES` as it is in `dir`: none when it isn't there (or is empty), an error when it's there but can't be read.
fn read_file(dir: &Path, name: &str) -> Result<Option<Value>, String> {
    crate::files::read_json(&dir.join(name))
}

/// agents.json's entries, each kept whatever its engine: an entry without an id makes the file unreadable.
fn agents_of(file: Value) -> Result<Vec<Agent>, String> {
    let Value::Array(entries) = file else { return Err("Couldn't read agents.json: it isn't a list".into()) };
    let text = |entry: &Value, key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string);
    entries
        .iter()
        .map(|entry| {
            let id =
                text(entry, "id").filter(|id| !id.is_empty()).ok_or("Couldn't read agents.json: an agent has no id")?;
            Ok(Agent {
                id,
                name: text(entry, "name").unwrap_or_default(),
                instructions: text(entry, "instructions").unwrap_or_default(),
                picture: text(entry, "picture").unwrap_or_default(),
                background: text(entry, "background"),
                session_id: text(entry, "sessionId"),
                engine: text(entry, "engine"),
                model: text(entry, "model"),
                earlier_chat: text(entry, "earlierChat"),
                created_at: entry.get("createdAt").and_then(Value::as_f64).unwrap_or(0.),
            })
        })
        .collect()
}

fn object_of(name: &str, file: Value) -> Result<Map<String, Value>, String> {
    match file {
        Value::Object(map) => Ok(map),
        _ => Err(format!("Couldn't read {name}: it isn't an object")),
    }
}

/// What `import` did.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Imported {
    /// The files it copied in (and renamed); none when it had run before.
    pub files: Vec<String>,
    pub agents: usize,
}

/// Copies the JSON files in `dir` into their tables, once: all of them in one transaction, then each is renamed to
/// `<name>.migrated` and the database records that it's done. A missing file has nothing to copy. A file that can't be
/// read stops it, every file staying as it is, until the next open tries again.
pub(crate) fn import(db: &Db, dir: &Path) -> Result<Imported, String> {
    if imported(db)? {
        return Ok(Imported::default());
    }
    // Read them all before touching anything.
    let mut files = Vec::new();
    for name in FILES {
        if let Some(value) = read_file(dir, name)? {
            files.push((name, value));
        }
    }
    let mut agents = Vec::new();
    let (mut profile, mut seen, mut pictures) = (Map::new(), Map::new(), Vec::new());
    for (name, value) in &files {
        match *name {
            "agents.json" => agents = agents_of(value.clone())?,
            "profile.json" => profile = object_of(name, value.clone())?,
            "seen.json" => seen = object_of(name, value.clone())?,

            "pictures.json" => {
                let Value::Array(list) = value else {
                    return Err("Couldn't read pictures.json: it isn't a list".into());
                };
                pictures = list.clone();
            }
            _ => {}
        }
    }
    let had_agents = files.iter().any(|(name, _)| *name == "agents.json");
    let copied = db.with(|conn| {
        // Immediate, so two processes opening the database at once don't both copy.
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        if state(&tx, IMPORTED)?.is_some() {
            return Ok(false);
        }
        for agent in &agents {
            insert_agent(&tx, agent)?;
        }
        for (key, value) in &profile {
            tx.execute("INSERT OR IGNORE INTO profile (key, value) VALUES (?1, ?2)", params![key, value.to_string()])?;
        }
        for (chat, until) in &seen {
            if let Some(until) = until.as_f64() {
                mark_seen_in(&tx, chat, until)?;
            }
        }

        // Newest first in the file: the first gets the newest time, should two have the same.
        for (index, picture) in pictures.iter().enumerate() {
            let Some(id) = picture.get("id").and_then(Value::as_str) else { continue };
            let at =
                picture.get("addedAt").and_then(Value::as_f64).unwrap_or(0.) + (pictures.len() - index) as f64 * 1e-3;
            tx.execute("INSERT OR IGNORE INTO agent_pictures (id, added_at) VALUES (?1, ?2)", params![id, at])?;
        }
        if had_agents {
            set_state(&tx, AGENTS_STARTED, "1")?;
        }
        set_state(&tx, IMPORTED, &crate::now_ms().to_string())?;
        tx.commit()?;
        Ok(true)
    })?;
    if !copied {
        return Ok(Imported::default());
    }
    for (name, _) in &files {
        let file = dir.join(name);
        if let Err(error) = std::fs::rename(&file, dir.join(format!("{name}.migrated"))) {
            eprintln!("filo: couldn't rename {} after copying it into the database: {error}", file.display());
        }
    }
    Ok(Imported { files: files.iter().map(|(name, _)| name.to_string()).collect(), agents: agents.len() })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A database in a folder of its own, with the JSON files `files` names written there first.
    pub(crate) fn db_with_files(files: &[(&str, &str)]) -> (Db, std::path::PathBuf) {
        let path = crate::db::tests::temp_path("app-data");
        let dir = path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        (Db::open(&path).unwrap(), dir)
    }

    /// Agents written as agents.json had them (`[{ "id", "name", "sessionId", "createdAt", … }]`), as rows.
    pub(crate) fn add_agents(db: &Db, list: Value) {
        for agent in agents_of(list).unwrap() {
            add_agent(db, &agent).unwrap();
        }
    }

    pub(crate) fn agent(id: &str, name: &str, session: Option<&str>, created_at: f64) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            instructions: format!("You are {name}."),
            picture: "mint".into(),
            background: None,
            session_id: session.map(str::to_string),
            engine: None,
            model: None,
            earlier_chat: None,
            created_at,
        }
    }

    const AGENTS: &str = r##"[
        {"id":"b","name":"Coach","instructions":"Coaches.","picture":"sunny","createdAt":2.0,"sessionId":"chat-b","engine":"codex","model":"gpt-5"},
        {"id":"a","name":"Chef","instructions":"Cooks.","picture":"mint","background":"#112233","createdAt":1.0,"engine":"opencode"},
        {"id":"z","name":"Future","instructions":"From later.","picture":"mint","createdAt":3.0,"engine":"warp-drive","model":"w1","earlierChat":"old"}
    ]"##;

    #[test]
    fn the_files_are_copied_in_once_and_renamed() {
        let (db, dir) = db_with_files(&[
            ("agents.json", AGENTS),
            ("profile.json", r#"{"userName":"Bek","mainAgentId":"b","agentAvatar":"panda"}"#),
            ("seen.json", r#"{"chat-b":5.0,"chat-x":7.5}"#),
            ("pictures.json", r#"[{"id":"p2","addedAt":20.0},{"id":"p1","addedAt":10.0}]"#),
        ]);
        // Opening imported them: every agent, the unknown engine as it was, oldest first.
        let rows = agents(&db).unwrap();
        assert_eq!(rows.iter().map(|agent| agent.id.as_str()).collect::<Vec<_>>(), ["a", "b", "z"]);
        assert_eq!(rows[0].background.as_deref(), Some("#112233"));
        assert_eq!(
            (rows[2].engine.as_deref(), rows[2].model.as_deref(), rows[2].earlier_chat.as_deref()),
            (Some("warp-drive"), Some("w1"), Some("old"))
        );
        assert_eq!(agent_of_chat(&db, "chat-b").unwrap().map(|agent| agent.name), Some("Coach".into()));
        let profile = profile(&db).unwrap();
        assert_eq!((profile["userName"].as_str(), profile["agentAvatar"].as_str()), (Some("Bek"), Some("panda")));
        assert_eq!(profile_text(&db, MAIN_AGENT).unwrap().as_deref(), Some("b"));
        assert_eq!(seen(&db).unwrap(), HashMap::from([("chat-b".to_string(), 5.0), ("chat-x".to_string(), 7.5)]));
        assert_eq!(pictures(&db).unwrap().into_iter().map(|(id, _)| id).collect::<Vec<_>>(), ["p2", "p1"]);
        for name in ["agents.json", "profile.json", "seen.json", "pictures.json"] {
            assert!(!dir.join(name).exists() && dir.join(format!("{name}.migrated")).exists(), "{name}");
        }
        assert!(imported(&db).unwrap());

        // An older Filo makes a fresh agents.json afterwards: it's never read.
        std::fs::write(
            dir.join("agents.json"),
            r#"[{"id":"f","name":"Filo","instructions":"","picture":"","createdAt":9.0}]"#,
        )
        .unwrap();
        drop(db);
        let db = Db::open(&dir.join(crate::db::FILE)).unwrap();
        assert_eq!(import(&db, &dir).unwrap(), Imported::default());
        assert_eq!(agents(&db).unwrap().len(), 3);
        assert!(dir.join("agents.json").exists(), "left where it is");
        // And the fresh data's Filo isn't added: the agents are the user's.
        assert!(!start_agents(&db, &agent("filo", "Filo", None, 0.)).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_file_stops_the_import_and_leaves_every_file_as_it_was() {
        let broken = r#"[{"id":"a","name":"Chef""#;
        let (db, dir) = db_with_files(&[("agents.json", broken), ("seen.json", r#"{"chat-a":1.0}"#)]);
        assert!(!imported(&db).unwrap());
        assert!(agents(&db).unwrap().is_empty() && seen(&db).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("agents.json")).unwrap(), broken);
        assert!(dir.join("seen.json").exists() && !dir.join("seen.json.migrated").exists());
        assert!(import(&db, &dir).unwrap_err().starts_with("Couldn't read agents.json"));
        // Fresh data's Filo isn't added over agents that couldn't be read.
        assert!(!start_agents(&db, &agent("filo", "Filo", None, 0.)).unwrap());
        assert!(agents(&db).unwrap().is_empty());
        // An entry without an id makes it unreadable too.
        std::fs::write(dir.join("agents.json"), r#"[{"name":"Nobody"}]"#).unwrap();
        assert!(import(&db, &dir).is_err());

        // Mended, the next open imports it.
        std::fs::write(dir.join("agents.json"), r#"[{"id":"a","name":"Chef","createdAt":1.0}]"#).unwrap();
        drop(db);
        let db = Db::open(&dir.join(crate::db::FILE)).unwrap();
        assert!(imported(&db).unwrap());
        assert_eq!(agents(&db).unwrap()[0].name, "Chef");
        assert_eq!(seen(&db).unwrap()["chat-a"], 1.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fresh_data_starts_with_one_agent_once() {
        let (db, dir) = db_with_files(&[]);
        assert!(imported(&db).unwrap());
        assert!(start_agents(&db, &agent("filo", "Filo", None, 1.)).unwrap());
        assert!(!start_agents(&db, &agent("filo", "Filo", None, 1.)).unwrap());
        remove_agent(&db, "filo").unwrap();
        assert!(!start_agents(&db, &agent("filo", "Filo", None, 1.)).unwrap());
        assert!(agents(&db).unwrap().is_empty());
        // In memory there's no folder and nothing imported: nothing is added.
        let memory = Db::in_memory().unwrap();
        assert!(!start_agents(&memory, &agent("filo", "Filo", None, 1.)).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_edit_changes_its_own_row() {
        let (db, dir) = db_with_files(&[("agents.json", AGENTS)]);
        // Another process changes a row the way an older or newer Filo's edit might: edits elsewhere leave it be.
        let snapshot = |db: &Db| {
            db.with(|conn| {
                conn.query_row("SELECT name, engine, model, created_at FROM agents WHERE id = 'z'", [], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                })
            })
            .unwrap()
        };
        let before = snapshot(&db);
        add_agent(&db, &agent("c", "Coder", None, 4.)).unwrap();
        let changed = update_agent(
            &db,
            "a",
            &AgentChange {
                name: Some("Chef 2".into()),
                background: Some(None),
                model: Some(("claude".into(), Some("opus".into()))),
                ..AgentChange::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (
                changed.name.as_str(),
                changed.background,
                changed.engine.as_deref(),
                changed.model.as_deref(),
                changed.instructions.as_str()
            ),
            ("Chef 2", None, Some("claude"), Some("opus"), "Cooks.")
        );
        assert!(set_agent_session(&db, "c", "chat-c").unwrap());
        assert!(!set_agent_session(&db, "gone", "chat-c").unwrap());
        assert_eq!(remove_agent(&db, "b").unwrap().map(|agent| agent.name), Some("Coach".into()));
        assert_eq!(remove_agent(&db, "b").unwrap(), None);
        assert!(!remove_agent_if(&db, "a", |agent| agent.session_id.is_some()).unwrap());
        mark_seen(&db, "chat-c", 10.).unwrap();
        mark_seen(&db, "chat-c", 4.).unwrap();
        assert_eq!(seen_until(&db, "chat-c").unwrap(), Some(10.));

        set_profile(&db, MAIN_AGENT, Some(&Value::String("c".into()))).unwrap();
        set_profile(&db, "userName", Some(&Value::String("Bek".into()))).unwrap();
        assert_eq!(profile_text(&db, MAIN_AGENT).unwrap().as_deref(), Some("c"));
        add_picture(&db, "p1", 1.).unwrap();
        add_picture(&db, "p2", 2.).unwrap();
        remove_picture(&db, "p1").unwrap();
        assert_eq!(pictures(&db).unwrap(), [("p2".to_string(), 2.)]);
        // The agent from a newer Filo is as it was, and still listed.
        assert_eq!(snapshot(&db), before);
        assert_eq!(agents(&db).unwrap().iter().map(|agent| agent.id.as_str()).collect::<Vec<_>>(), ["a", "z", "c"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

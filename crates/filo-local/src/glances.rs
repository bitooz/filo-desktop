// Glances in Filo's database (db.rs): small visuals Filo shows above its reply in a conversation (design/
// filo-desktop-glances.html), made with the `show_glance` tool (glances_mcp.rs). Each one goes with an assistant message
// whose text says it in words (`plain_text`), so the engine's history, previews and notifications read it as text; the
// conversation's `voiceSessions:detail` gives that message the card's head (`Head`), and the app fetches the page
// (`glances:get`, built by glance_document.rs) once the card shows. The HTML stays here, off the chat's messages file
// every read of the chat goes through. A glance never changes once made; saving it to Artifacts only notes the artifact.

use std::collections::HashMap;

use rusqlite::{OptionalExtension as _, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::glance_document::{self, MAX_HTML, MAX_TEXT};

/// The table (db.rs `MIGRATIONS`). `IF NOT EXISTS`: `ensure_columns` makes it too, as the file opens.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS glances (
    id           TEXT PRIMARY KEY NOT NULL,
    chat_id      TEXT NOT NULL,
    message_id   TEXT NOT NULL,
    turn_id      TEXT,
    title        TEXT NOT NULL,
    as_of        TEXT,
    icon         TEXT,
    source       TEXT,
    visual_label TEXT,
    html         TEXT NOT NULL,
    text         TEXT NOT NULL,
    notes        TEXT,
    table_json   TEXT,
    artifact_id  TEXT,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS glances_by_chat ON glances (chat_id, created_at);
";

/// Icons the app draws in the card's head (assets/icons); anything else shows the chart icon.
pub const ICONS: &[&str] = &[
    "calendar-clock",
    "layout-grid",
    "trending-up",
    "chart-line",
    "grid-3x3",
    "chart-pie",
    "chart-column-stacked",
    "chart-gantt",
];

const MAX_TITLE: usize = 120;
const MAX_AS_OF: usize = 60;
const MAX_SOURCE: usize = 120;
const MAX_VISUAL_LABEL: usize = 24;
const MAX_NOTES: usize = 1_200;
const MAX_TABLE_COLUMNS: usize = 8;
const MAX_TABLE_ROWS: usize = 60;
const MAX_CELL: usize = 200;

/// The table, made here too in case another page's migration took its slot on a development Mac (see `SCHEMA`), and the
/// columns added beside it (db.rs runs this on every open), so a Filo from before them still opens the file.
pub fn ensure_columns(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    let has: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM pragma_table_info('glances') WHERE name = 'wide')", [], |row| {
            row.get(0)
        })?;
    if !has {
        conn.execute_batch("ALTER TABLE glances ADD COLUMN wide INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// The data behind a chart (the sheet's Table view).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// A glance as Filo made it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct New {
    pub title: String,
    pub html: String,
    pub text: String,
    pub as_of: Option<String>,
    pub icon: Option<String>,
    pub source: Option<String>,
    pub visual_label: Option<String>,
    pub notes: Option<String>,
    pub table: Option<Table>,
    /// Asked to be as wide as the chat allows, rather than as wide as its content.
    pub wide: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Glance {
    pub id: String,
    /// The conversation it shows in (the group's, for a group turn).
    pub chat_id: String,
    /// The assistant message that says it in words, and carries its card.
    pub message_id: String,
    pub turn_id: Option<String>,
    pub title: String,
    pub as_of: Option<String>,
    pub icon: Option<String>,
    pub source: Option<String>,
    pub visual_label: Option<String>,
    pub html: String,
    pub text: String,
    pub notes: Option<String>,
    pub table: Option<Table>,
    pub wide: bool,
    /// Saved to Artifacts as this artifact.
    pub artifact_id: Option<String>,
    pub created_at: i64,
}

/// What a message carries for the card's head (`glance` on a message, src/screens/chat/types.rs `GlanceRef`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Head {
    pub glance_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wide: bool,
}

impl Glance {
    fn page(&self, standalone: bool) -> String {
        glance_document::document(&glance_document::Input {
            title: &self.title,
            as_of: self.as_of.as_deref(),
            html: &self.html,
            notes: self.notes.as_deref(),
            standalone,
        })
    }

    /// The page the card and the sheet show.
    pub fn document(&self) -> String {
        self.page(false)
    }

    /// A page of its own, for Artifacts: its title on top, light or dark with the system.
    pub fn standalone_document(&self) -> String {
        self.page(true)
    }
}

/// What its message says: the title (and when it's from), then the text. The sheet's Copy takes the same.
pub fn plain_text(title: &str, as_of: Option<&str>, text: &str) -> String {
    match as_of {
        Some(as_of) => format!("{title} ({as_of})\n{text}"),
        None => format!("{title}\n{text}"),
    }
}

/// Trimmed and cut to `max` characters; none when that leaves nothing.
fn clip(value: Option<&str>, max: usize) -> Option<String> {
    let value: String = value?.trim().chars().take(max).collect();
    let value = value.trim_end().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalized_table(table: &Table) -> Option<Table> {
    let cell = |value: &str| value.trim().chars().take(MAX_CELL).collect::<String>();
    let columns: Vec<String> = table.columns.iter().take(MAX_TABLE_COLUMNS).map(|column| cell(column)).collect();
    if columns.is_empty() {
        return None;
    }
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .take(MAX_TABLE_ROWS)
        .map(|row| {
            (0..columns.len()).map(|index| row.get(index).map(|value| cell(value)).unwrap_or_default()).collect()
        })
        .collect();
    (!rows.is_empty()).then_some(Table { columns, rows })
}

/// `new` checked and tidied as it's kept: limits, a known icon or none, a table with rows or none.
pub fn checked(new: &New) -> Result<New, String> {
    let title = clip(Some(&new.title), MAX_TITLE).ok_or("Give the glance a `title`.")?;
    let text = new.text.trim().to_string();
    if text.is_empty() || text.chars().count() > MAX_TEXT {
        return Err(format!("`text` must be 1 to {MAX_TEXT} characters: the glance in plain sentences."));
    }
    let html = new.html.trim().to_string();
    if html.is_empty() || html.chars().count() > MAX_HTML {
        return Err(format!("`html` must be 1 to {MAX_HTML} characters."));
    }
    Ok(New {
        title,
        html,
        text,
        as_of: clip(new.as_of.as_deref(), MAX_AS_OF),
        icon: new.icon.as_deref().map(str::trim).filter(|icon| ICONS.contains(icon)).map(str::to_string),
        source: clip(new.source.as_deref(), MAX_SOURCE),
        visual_label: clip(new.visual_label.as_deref(), MAX_VISUAL_LABEL),
        notes: clip(new.notes.as_deref(), MAX_NOTES),
        table: new.table.as_ref().and_then(normalized_table),
        wide: new.wide,
    })
}

/// Keeps a glance `checked` already, for the message `message_id` in `chat_id`, in a transaction the caller also
/// writes its message in (the group's) or writes it right after (a chat's file).
pub fn insert(
    tx: &Transaction,
    new: &New,
    chat_id: &str,
    message_id: &str,
    turn_id: Option<&str>,
    now: i64,
) -> rusqlite::Result<Glance> {
    let glance = Glance {
        id: uuid::Uuid::new_v4().to_string(),
        chat_id: chat_id.to_string(),
        message_id: message_id.to_string(),
        turn_id: turn_id.map(str::to_string),
        title: new.title.clone(),
        as_of: new.as_of.clone(),
        icon: new.icon.clone(),
        source: new.source.clone(),
        visual_label: new.visual_label.clone(),
        html: new.html.clone(),
        text: new.text.clone(),
        notes: new.notes.clone(),
        table: new.table.clone(),
        wide: new.wide,
        artifact_id: None,
        created_at: now,
    };
    let table = glance.table.as_ref().and_then(|table| serde_json::to_string(table).ok());
    tx.execute(
        &format!("INSERT INTO glances ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15)"),
        params![
            glance.id, glance.chat_id, glance.message_id, glance.turn_id, glance.title, glance.as_of, glance.icon, glance.source,
            glance.visual_label, glance.html, glance.text, glance.notes, table, glance.created_at, glance.wide
        ],
    )?;
    Ok(glance)
}

/// Takes back a glance whose message couldn't be written.
pub fn remove(db: &Db, id: &str) -> Result<(), String> {
    db.write(|tx| tx.execute("DELETE FROM glances WHERE id = ?1", params![id])).map(|_| ())
}

const COLUMNS: &str = "id, chat_id, message_id, turn_id, title, as_of, icon, source, visual_label, html, text, notes, table_json, artifact_id, created_at, wide";

fn from_row(row: &Row) -> rusqlite::Result<Glance> {
    let table: Option<String> = row.get(12)?;
    Ok(Glance {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        message_id: row.get(2)?,
        turn_id: row.get(3)?,
        title: row.get(4)?,
        as_of: row.get(5)?,
        icon: row.get(6)?,
        source: row.get(7)?,
        visual_label: row.get(8)?,
        html: row.get(9)?,
        text: row.get(10)?,
        notes: row.get(11)?,
        table: table.and_then(|table| serde_json::from_str(&table).ok()),
        artifact_id: row.get(13)?,
        created_at: row.get(14)?,
        wide: row.get(15)?,
    })
}

pub fn get(db: &Db, id: &str) -> Result<Option<Glance>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {COLUMNS} FROM glances WHERE id = ?1"), params![id], from_row).optional()
    })
}

/// The cards' heads in a conversation, by the message each goes with.
pub fn heads(db: &Db, chat_id: &str) -> Result<HashMap<String, Head>, String> {
    db.with(|conn| {
        let mut statement =
            conn.prepare("SELECT id, message_id, title, as_of, icon, wide FROM glances WHERE chat_id = ?1")?;
        let rows = statement.query_map(params![chat_id], |row| {
            Ok((
                row.get::<_, String>(1)?,
                Head {
                    glance_id: row.get(0)?,
                    title: row.get(2)?,
                    as_of: row.get(3)?,
                    icon: row.get(4)?,
                    wide: row.get(5)?,
                },
            ))
        })?;
        rows.collect()
    })
}

/// Notes the artifact it was saved as.
pub fn set_artifact(db: &Db, id: &str, artifact_id: &str) -> Result<(), String> {
    db.write(|tx| tx.execute("UPDATE glances SET artifact_id = ?2 WHERE id = ?1", params![id, artifact_id])).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new() -> New {
        New {
            title: "  Your week  ".into(),
            html: "<div>chart</div>".into(),
            text: "Mon: 3 h".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_glance_is_checked_kept_and_found_by_its_message() {
        let db = Db::in_memory().unwrap();
        let mut made = new();
        made.as_of = Some(" ".into());
        made.icon = Some("rocket".into());
        made.visual_label = Some("A label far longer than twenty-four characters".into());
        made.table = Some(Table {
            columns: vec!["Day".into(), "Hours".into()],
            rows: vec![vec!["Mon".into()], vec!["Tue".into(), "2".into(), "extra".into()]],
        });
        let checked = checked(&made).unwrap();
        assert_eq!(
            (checked.title.as_str(), checked.as_of.as_deref(), checked.icon.as_deref()),
            ("Your week", None, None)
        );
        assert_eq!(checked.visual_label.as_deref(), Some("A label far longer than"));
        assert_eq!(
            checked.table.as_ref().unwrap().rows,
            vec![vec!["Mon".to_string(), String::new()], vec!["Tue".into(), "2".into()]]
        );

        let kept = db.write(|tx| insert(tx, &checked, "chat-1", "message-1", Some("turn-1"), 5)).unwrap();
        let found = get(&db, &kept.id).unwrap().unwrap();
        assert_eq!(found, kept);
        assert!(found.document().contains("<div id=\"filo-glance\"><div>chart</div></div>"));
        assert!(found.standalone_document().contains("<h1 class=\"filo-title\">Your week</h1>"));
        let heads = heads(&db, "chat-1").unwrap();
        assert_eq!(
            heads["message-1"],
            Head { glance_id: kept.id.clone(), title: "Your week".into(), as_of: None, icon: None, wide: false }
        );
        let wide =
            db.write(|tx| insert(tx, &New { wide: true, ..checked.clone() }, "chat-1", "message-2", None, 6)).unwrap();
        assert!(get(&db, &wide.id).unwrap().unwrap().wide && super::heads(&db, "chat-1").unwrap()["message-2"].wide);
        assert!(super::heads(&db, "chat-2").unwrap().is_empty());

        set_artifact(&db, &kept.id, "artifact-1").unwrap();
        assert_eq!(get(&db, &kept.id).unwrap().unwrap().artifact_id.as_deref(), Some("artifact-1"));
        remove(&db, &kept.id).unwrap();
        assert!(get(&db, &kept.id).unwrap().is_none());
    }

    #[test]
    fn a_table_from_before_wide_gets_the_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch("INSERT INTO glances (id, chat_id, message_id, title, html, text, created_at) VALUES ('g', 'c', 'm', 'T', 'h', 't', 1)").unwrap();
        ensure_columns(&conn).unwrap();
        ensure_columns(&conn).unwrap();
        let wide: bool = conn.query_row("SELECT wide FROM glances WHERE id = 'g'", [], |row| row.get(0)).unwrap();
        assert!(!wide);
    }

    #[test]
    fn what_a_glance_needs() {
        assert!(checked(&New { title: " ".into(), ..new() }).is_err());
        assert!(checked(&New { text: "".into(), ..new() }).is_err());
        assert!(checked(&New { html: "x".repeat(MAX_HTML + 1), ..new() }).is_err());
        let no_rows = checked(&New {
            table: Some(Table { columns: vec!["A".into()], rows: vec![] }),
            icon: Some("chart-pie".into()),
            ..new()
        })
        .unwrap();
        assert_eq!((no_rows.table, no_rows.icon.as_deref()), (None, Some("chart-pie")));
        assert_eq!(plain_text("Week", Some("Oct 4"), "Mon: 3 h"), "Week (Oct 4)\nMon: 3 h");
        assert_eq!(plain_text("Week", None, "Mon: 3 h"), "Week\nMon: 3 h");
    }
}

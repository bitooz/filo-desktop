// What Filo remembers about the user, in Filo's database (db.rs): the Memories page through the app's `memories:*`
// functions, and every local turn (turn.rs puts them in the prompt; the MCP server's memory tools save, change and
// forget them).
//
// A memory is one short statement ("Prefers metric units"), with what kind it is, where it came from (`provenance`)
// and whether the user confirmed it, in the shape the Memories page reads (docs/spec-scheduled-notes-you.md §4).
// Forgetting one deletes it outright: nothing is kept on this Mac after the user says forget.

use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};

use crate::db::Db;

/// The memories table (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE memories (
    id           TEXT PRIMARY KEY NOT NULL,
    type         TEXT NOT NULL,
    content      TEXT NOT NULL,
    source       TEXT NOT NULL,
    source_id    TEXT,
    explanation  TEXT NOT NULL DEFAULT '',
    confirmation TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX memories_by_update ON memories (updated_at DESC, id DESC);
";

/// One memory at most this long (characters).
const MAX_CONTENT: usize = 500;
const MAX_EXPLANATION: usize = 200;
/// A page of `memories:list` at most this long.
const MAX_PAGE: usize = 200;

pub const TYPES: [&str; 7] = ["preference", "fact", "goal", "relationship", "life_event", "ongoing_situation", "other"];
const CONFIRMATIONS: [&str; 3] = ["inferred", "confirmed", "corrected"];

/// A memory as the pages read it (`Memory` in the spec).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_creationTime")]
    pub creation_time: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
    pub provenance: Provenance,
    pub confirmation: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    /// `user_explicit` (added on the Memories page), `conversation` (saved in a chat), `correction`, `import`.
    pub source: String,
    /// The conversation it was saved in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub explanation: String,
}

/// A page of `memories:list`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub page: Vec<Memory>,
    pub is_done: bool,
    /// Where the next page starts (`<updatedAt>:<id>` of this page's last memory); empty when done.
    pub continue_cursor: String,
}

/// A memory to save.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct New {
    pub content: String,
    /// One of `TYPES`; anything else is `other`.
    pub kind: Option<String>,
    pub source: String,
    pub source_id: Option<String>,
    pub explanation: String,
    /// One of `CONFIRMATIONS`; anything else is `inferred`.
    pub confirmation: String,
}

/// What to change in a memory; `None` leaves it as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub content: Option<String>,
    pub kind: Option<String>,
    pub explanation: Option<String>,
    pub confirmation: Option<String>,
}

const COLUMNS: &str = "id, type, content, source, source_id, explanation, confirmation, created_at, updated_at";

fn memory(row: &Row) -> rusqlite::Result<Memory> {
    let created_at: i64 = row.get(7)?;
    Ok(Memory {
        id: row.get(0)?,
        creation_time: created_at,
        kind: row.get(1)?,
        content: row.get(2)?,
        provenance: Provenance { source: row.get(3)?, source_id: row.get(4)?, explanation: row.get(5)? },
        confirmation: row.get(6)?,
        created_at,
        updated_at: row.get(8)?,
    })
}

fn now() -> i64 {
    crate::now_ms() as i64
}

/// Up to `count` memories, the most recently changed first, after `cursor` (a page's `continue_cursor`; none or empty
/// for the first page).
pub fn page(db: &Db, count: usize, cursor: Option<&str>) -> Result<Page, String> {
    let count = count.clamp(1, MAX_PAGE);
    let after = cursor.filter(|cursor| !cursor.is_empty()).map(parse_cursor).transpose()?;
    // One more than asked says whether there's another page.
    let mut rows: Vec<Memory> = db.with(|conn| match &after {
        Some((at, id)) => conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM memories WHERE (updated_at, id) < (?1, ?2) ORDER BY updated_at DESC, id DESC LIMIT ?3"))?
            .query_map(params![at, id, count as i64 + 1], memory)?
            .collect(),
        None => conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM memories ORDER BY updated_at DESC, id DESC LIMIT ?1"))?
            .query_map([count as i64 + 1], memory)?
            .collect(),
    })?;
    let is_done = rows.len() <= count;
    rows.truncate(count);
    let continue_cursor = match rows.last() {
        Some(last) if !is_done => format!("{}:{}", last.updated_at, last.id),
        _ => String::new(),
    };
    Ok(Page { page: rows, is_done, continue_cursor })
}

fn parse_cursor(cursor: &str) -> Result<(i64, String), String> {
    cursor
        .split_once(':')
        .and_then(|(at, id)| Some((at.parse().ok()?, id.to_string())))
        .ok_or_else(|| format!("Not a memories cursor: {cursor}"))
}

/// Every memory, the most recently changed first.
pub fn all(db: &Db) -> Result<Vec<Memory>, String> {
    db.with(|conn| {
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM memories ORDER BY updated_at DESC, id DESC"))?
            .query_map([], memory)?
            .collect()
    })
}

/// One memory by id (tests).
#[cfg(any(test, feature = "dev"))]
pub fn get(db: &Db, id: &str) -> Result<Option<Memory>, String> {
    use rusqlite::OptionalExtension as _;
    db.with(|conn| {
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM memories WHERE id = ?1"))?
            .query_row([id], memory)
            .optional()
    })
}

/// Saves a memory; its id. The same statement saved again (case and spacing aside) isn't a second memory: the one
/// there is brought to the top and keeps the stronger confirmation.
pub fn create(db: &Db, new: &New) -> Result<String, String> {
    let content = clean_content(&new.content)?;
    let kind = clean_kind(new.kind.as_deref());
    let confirmation = clean_confirmation(&new.confirmation);
    let explanation = clean_line(&new.explanation, MAX_EXPLANATION);
    let now = now();
    db.write(|tx| {
        let same = normalize(&content);
        let existing: Option<(String, String)> = tx
            .prepare_cached("SELECT id, content, confirmation FROM memories")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
            .filter_map(Result::ok)
            .find(|(_, content, _)| normalize(content) == same)
            .map(|(id, _, confirmation)| (id, confirmation));
        if let Some((id, was)) = existing {
            let confirmation = if was == "inferred" { confirmation } else { was.as_str() };
            tx.execute("UPDATE memories SET confirmation = ?2, updated_at = ?3 WHERE id = ?1", params![id, confirmation, now])?;
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO memories (id, type, content, source, source_id, explanation, confirmation, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![id, kind, content, new.source, new.source_id, explanation, confirmation, now],
        )?;
        Ok(id)
    })
}

/// Changes a memory and brings it to the top. False when there's no such memory.
pub fn update(db: &Db, id: &str, change: &Change) -> Result<bool, String> {
    let content = change.content.as_deref().map(clean_content).transpose()?;
    let kind = change.kind.as_deref().map(|kind| clean_kind(Some(kind)));
    let explanation = change.explanation.as_deref().map(|text| clean_line(text, MAX_EXPLANATION));
    let confirmation = change.confirmation.as_deref().map(clean_confirmation);
    let changed = db.write(|tx| {
        tx.execute(
            "UPDATE memories SET content = COALESCE(?2, content), type = COALESCE(?3, type), explanation = COALESCE(?4, explanation),
             confirmation = COALESCE(?5, confirmation), updated_at = ?6 WHERE id = ?1",
            params![id, content, kind, explanation, confirmation, now()],
        )
    })?;
    Ok(changed > 0)
}

/// Forgets a memory for good. False when there's no such memory.
pub fn remove(db: &Db, id: &str) -> Result<bool, String> {
    Ok(db.write(|tx| tx.execute("DELETE FROM memories WHERE id = ?1", [id]))? > 0)
}

/// The memories that contain every word of `query` (case doesn't matter), in `all`'s order.
pub fn search(db: &Db, query: &str) -> Result<Vec<Memory>, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    Ok(all(db)?
        .into_iter()
        .filter(|memory| {
            let haystack =
                format!("{}\n{}\n{}", memory.content, memory.kind, memory.provenance.explanation).to_lowercase();
            words.iter().all(|word| haystack.contains(word.as_str()))
        })
        .collect())
}

/// A turn's prompt lists memories up to this many characters; older ones are left to `search_memories`.
const PROMPT_CHARS: usize = 8_000;

/// What a turn is told about the user: the memories, newest first, with their ids for the memory tools, and when to
/// save, change or forget one.
pub fn prompt_block(memories: &[Memory]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    for memory in memories {
        let unconfirmed = if memory.confirmation == "inferred" { ", not yet confirmed" } else { "" };
        let line = format!("- [{}] {} ({}{unconfirmed})", memory.id, memory.content, memory.kind.replace('_', " "));
        used += line.len() + 1;
        if used > PROMPT_CHARS {
            break;
        }
        lines.push(line);
    }
    let mut block = if memories.is_empty() {
        "You don't remember anything about the user yet (your memories, which they see and edit on the Memories page)."
            .to_string()
    } else {
        format!(
            "What you remember about the user (your memories; they see, edit and delete them on the Memories page):\n{}",
            lines.join("\n")
        )
    };
    if lines.len() < memories.len() {
        block.push_str(&format!(
            "\n({} older memories aren't listed here: look them up with `search_memories`.)",
            memories.len() - lines.len()
        ));
    }
    block.push_str(
        "\n\nUse them where they help, without reciting them. When the user tells you something lasting about themselves (a preference, \
fact, goal, relationship, life event or ongoing situation), save it with Filo's `save_memory` tool, one short statement each; \
if it changes one you have, use `update_memory` with its id instead. When they ask you to forget something, use `forget_memory`. \
Don't save passing details, secrets, or what they asked you not to keep.",
    );
    block
}

/// One line, at most 500 characters, not empty.
fn clean_content(content: &str) -> Result<String, String> {
    let content = clean_line(content, MAX_CONTENT);
    if content.is_empty() { Err("A memory can't be empty.".into()) } else { Ok(content) }
}

fn clean_line(text: &str, max: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

fn clean_kind(kind: Option<&str>) -> &'static str {
    let kind = kind.map(|kind| kind.trim().to_lowercase().replace([' ', '-'], "_")).unwrap_or_default();
    TYPES.into_iter().find(|known| *known == kind).unwrap_or("other")
}

fn clean_confirmation(confirmation: &str) -> &'static str {
    CONFIRMATIONS.into_iter().find(|known| *known == confirmation.trim()).unwrap_or("inferred")
}

fn normalize(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::temp_path;

    fn new(content: &str) -> New {
        New {
            content: content.into(),
            kind: Some("preference".into()),
            source: "user_explicit".into(),
            confirmation: "confirmed".into(),
            ..New::default()
        }
    }

    fn pause() {
        std::thread::sleep(std::time::Duration::from_millis(3));
    }

    #[test]
    fn memories_are_added_changed_and_forgotten() {
        let db = Db::in_memory().unwrap();
        let tea = create(&db, &new("  Likes   green tea ")).unwrap();
        pause();
        let run = create(
            &db,
            &New {
                kind: Some("Ongoing Situation".into()),
                source: "conversation".into(),
                source_id: Some("chat-1".into()),
                explanation: "Said so in a chat".into(),
                ..new("Training for a half marathon")
            },
        )
        .unwrap();
        let ids = |db: &Db| all(db).unwrap().into_iter().map(|memory| memory.id).collect::<Vec<_>>();
        assert_eq!(ids(&db), [run.clone(), tea.clone()]);

        let memory = get(&db, &run).unwrap().unwrap();
        assert_eq!((memory.kind.as_str(), memory.confirmation.as_str()), ("ongoing_situation", "confirmed"));
        assert_eq!(
            memory.provenance,
            Provenance {
                source: "conversation".into(),
                source_id: Some("chat-1".into()),
                explanation: "Said so in a chat".into()
            }
        );
        assert_eq!(get(&db, &tea).unwrap().unwrap().content, "Likes green tea");

        // An edit changes only what's given and brings it to the top.
        pause();
        assert!(
            update(
                &db,
                &tea,
                &Change {
                    content: Some("Likes oolong tea".into()),
                    confirmation: Some("corrected".into()),
                    ..Change::default()
                }
            )
            .unwrap()
        );
        let memory = get(&db, &tea).unwrap().unwrap();
        assert_eq!(
            (memory.content.as_str(), memory.kind.as_str(), memory.confirmation.as_str()),
            ("Likes oolong tea", "preference", "corrected")
        );
        assert!(memory.updated_at > memory.created_at);
        assert_eq!(ids(&db), [tea.clone(), run.clone()]);
        assert!(update(&db, &tea, &Change { content: Some("   ".into()), ..Change::default() }).is_err());
        assert!(!update(&db, "nope", &Change::default()).unwrap());

        assert!(remove(&db, &run).unwrap());
        assert!(!remove(&db, &run).unwrap());
        assert_eq!(get(&db, &run).unwrap(), None);
        assert_eq!(ids(&db), [tea]);
        assert!(create(&db, &new(" \n ")).is_err());
    }

    #[test]
    fn the_same_memory_twice_is_one() {
        let db = Db::in_memory().unwrap();
        let first = create(&db, &New { confirmation: "inferred".into(), ..new("Has a dog named Rex") }).unwrap();
        create(&db, &new("Lives in Tashkent")).unwrap();
        pause();
        let again = create(&db, &new("has a  DOG named rex")).unwrap();
        assert_eq!(again, first);
        let memories = all(&db).unwrap();
        assert_eq!(memories.len(), 2);
        assert_eq!((memories[0].id.as_str(), memories[0].confirmation.as_str()), (first.as_str(), "confirmed"));
        // Saved again as a guess, a confirmed memory stays confirmed.
        create(&db, &New { confirmation: "inferred".into(), ..new("Has a dog named Rex") }).unwrap();
        assert_eq!(get(&db, &first).unwrap().unwrap().confirmation, "confirmed");
    }

    #[test]
    fn pages_go_newest_first_until_done() {
        let db = Db::in_memory().unwrap();
        for index in 0..7 {
            create(&db, &new(&format!("memory {index}"))).unwrap();
            pause();
        }
        let first = page(&db, 3, None).unwrap();
        assert_eq!(
            first.page.iter().map(|memory| memory.content.as_str()).collect::<Vec<_>>(),
            ["memory 6", "memory 5", "memory 4"]
        );
        assert!(!first.is_done);
        let second = page(&db, 3, Some(&first.continue_cursor)).unwrap();
        assert_eq!(
            second.page.iter().map(|memory| memory.content.as_str()).collect::<Vec<_>>(),
            ["memory 3", "memory 2", "memory 1"]
        );
        let third = page(&db, 3, Some(&second.continue_cursor)).unwrap();
        assert_eq!(third.page.len(), 1);
        assert!(third.is_done && third.continue_cursor.is_empty());
        // Exactly a page's worth is one page, done.
        let exact = page(&db, 7, Some("")).unwrap();
        assert!(exact.is_done && exact.page.len() == 7);
        assert!(page(&db, 3, Some("garbage")).is_err());
        assert!(page(&Db::in_memory().unwrap(), 30, None).unwrap().is_done);
    }

    #[test]
    fn search_needs_every_word() {
        let db = Db::in_memory().unwrap();
        create(&db, &new("Allergic to peanuts")).unwrap();
        create(&db, &New { kind: Some("goal".into()), ..new("Wants to learn Japanese") }).unwrap();
        let found =
            |query: &str| search(&db, query).unwrap().into_iter().map(|memory| memory.content).collect::<Vec<_>>();
        assert_eq!(found("PEANUTS allergic"), ["Allergic to peanuts"]);
        assert_eq!(found("goal"), ["Wants to learn Japanese"]);
        assert_eq!(found("").len(), 2);
        assert!(found("peanuts japanese").is_empty());
    }

    #[test]
    fn memories_are_kept_across_restarts() {
        let path = temp_path("memories");
        let id = {
            let db = Db::open(&path).unwrap();
            let id = create(&db, &new("Prefers mornings for deep work")).unwrap();
            update(
                &db,
                &id,
                &Change { content: Some("Prefers early mornings for deep work".into()), ..Change::default() },
            )
            .unwrap();
            id
        };
        let db = Db::open(&path).unwrap();
        assert_eq!(get(&db, &id).unwrap().unwrap().content, "Prefers early mornings for deep work");
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_prompt_lists_memories_with_their_ids() {
        assert!(prompt_block(&[]).starts_with("You don't remember anything about the user yet (your memories, which they see and edit on the Memories page)."));
        let db = Db::in_memory().unwrap();
        let id = create(
            &db,
            &New { kind: Some("life_event".into()), confirmation: "inferred".into(), ..new("Moved to Berlin in May") },
        )
        .unwrap();
        let block = prompt_block(&all(&db).unwrap());
        assert!(block.starts_with("What you remember about the user (your memories; they see, edit and delete them on the Memories page):\n"), "{block}");
        assert!(block.contains(&format!("- [{id}] Moved to Berlin in May (life event, not yet confirmed)")), "{block}");
        assert!(block.contains("save_memory") && !block.contains("older memories"));
        for index in 0..200 {
            create(&db, &new(&format!("A fairly long memory number {index} that takes up room in the prompt")))
                .unwrap();
        }
        let block = prompt_block(&all(&db).unwrap());
        assert!(block.len() < PROMPT_CHARS + 1_000);
        assert!(block.contains("older memories aren't listed"));
    }

    #[test]
    fn memories_are_cleaned_up() {
        assert_eq!(clean_content("  a\n b\tc ").unwrap(), "a b c");
        assert_eq!(clean_content(&"x".repeat(900)).unwrap().len(), MAX_CONTENT);
        assert_eq!(
            (clean_kind(Some("Life Event")), clean_kind(Some("bogus")), clean_kind(None)),
            ("life_event", "other", "other")
        );
        assert_eq!((clean_confirmation("confirmed"), clean_confirmation("sure")), ("confirmed", "inferred"));
    }
}

// Notes in Filo's database (db.rs): the Notes tab and the note page, through the app's `notes:*` functions.
//
// A note is cleaned up when it's saved (docs/spec-scheduled-notes-you.md §3): the title on one line and at most 120
// characters, the body without `\r` and at most 8000 (UTF-16 units, as the editor counts), at most 6 tags of 24
// characters, lowercased. Tags are a JSON array in their column. Deleting a note marks it deleted, and it's dropped
// for good 30 days later (`purge_deleted`).

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::{Deserialize, Serialize};

use crate::db::Db;

/// The notes table (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE notes (
    id         TEXT PRIMARY KEY NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    tags       TEXT NOT NULL DEFAULT '[]',
    pinned     INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX notes_by_update ON notes (deleted_at, updated_at DESC);
";

const MAX_TITLE: usize = 120;
/// In UTF-16 units, like the note page's limit.
const MAX_BODY: usize = 8_000;
const MAX_TAG: usize = 24;
const MAX_TAGS: usize = 6;
/// `notes:mine` lists this many, the most recently changed.
const LIST_LIMIT: usize = 500;
/// How long a deleted note is kept before it's dropped, in ms.
const KEEP_DELETED_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// A note as the pages read it (`Note` in the spec).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What `notes:createMine` and `notes:updateMine` are given.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fields {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

const COLUMNS: &str = "id, title, body, tags, pinned, created_at, updated_at";

fn note(row: &Row) -> rusqlite::Result<Note> {
    let tags: String = row.get(3)?;
    Ok(Note {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        pinned: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn now() -> i64 {
    crate::now_ms() as i64
}

/// The notes not deleted, the most recently changed first.
pub fn list(db: &Db) -> Result<Vec<Note>, String> {
    db.with(|conn| {
        conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM notes WHERE deleted_at IS NULL ORDER BY updated_at DESC, id LIMIT ?1"
        ))?
        .query_map([LIST_LIMIT as i64], note)?
        .collect()
    })
}

/// The note, unless it's deleted or never was.
pub fn get(db: &Db, id: &str) -> Result<Option<Note>, String> {
    db.with(|conn| find(conn, id))
}

fn find(conn: &Connection, id: &str) -> rusqlite::Result<Option<Note>> {
    conn.prepare_cached(&format!("SELECT {COLUMNS} FROM notes WHERE id = ?1 AND deleted_at IS NULL"))?
        .query_row([id], note)
        .optional()
}

/// Makes a note; its id.
pub fn create(db: &Db, fields: &Fields) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now();
    let tags =
        serde_json::to_string(&clean_tags(fields.tags.as_deref().unwrap_or_default())).unwrap_or_else(|_| "[]".into());
    db.write(|tx| {
        tx.execute(
            "INSERT INTO notes (id, title, body, tags, pinned, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id, clean_title(&fields.title), clean_body(&fields.body), tags, fields.pinned.unwrap_or(false), now],
        )
    })?;
    Ok(id)
}

/// Replaces the note's title, body and pin (a missing pin is off) and moves it to the top; tags missing or empty
/// leave its tags as they are. False when there's no such note.
pub fn update(db: &Db, id: &str, fields: &Fields) -> Result<bool, String> {
    let tags = Some(clean_tags(fields.tags.as_deref().unwrap_or_default()))
        .filter(|tags| !tags.is_empty())
        .map(|tags| serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()));
    let changed = db.write(|tx| {
        tx.execute(
            "UPDATE notes SET title = ?2, body = ?3, tags = COALESCE(?4, tags), pinned = ?5, updated_at = ?6
             WHERE id = ?1 AND deleted_at IS NULL",
            params![
                id,
                clean_title(&fields.title),
                clean_body(&fields.body),
                tags,
                fields.pinned.unwrap_or(false),
                now()
            ],
        )
    })?;
    Ok(changed > 0)
}

/// Takes the note's tags off (`update` keeps them when given none). False when there's no such note.
pub fn clear_tags(db: &Db, id: &str) -> Result<bool, String> {
    let changed =
        db.write(|tx| tx.execute("UPDATE notes SET tags = '[]' WHERE id = ?1 AND deleted_at IS NULL", params![id]))?;
    Ok(changed > 0)
}

/// Pins or unpins the note without moving it (its date stays). False when there's no such note.
pub fn set_pinned(db: &Db, id: &str, pinned: bool) -> Result<bool, String> {
    let changed = db.write(|tx| {
        tx.execute("UPDATE notes SET pinned = ?2 WHERE id = ?1 AND deleted_at IS NULL", params![id, pinned])
    })?;
    Ok(changed > 0)
}

/// Marks the note deleted. False when there's no such note.
pub fn delete(db: &Db, id: &str) -> Result<bool, String> {
    let changed = db.write(|tx| {
        tx.execute("UPDATE notes SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL", params![id, now()])
    })?;
    Ok(changed > 0)
}

/// Drops the notes deleted more than 30 days before `now` (ms); how many.
pub fn purge_deleted(db: &Db, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        tx.execute("DELETE FROM notes WHERE deleted_at IS NOT NULL AND deleted_at < ?1", [now - KEEP_DELETED_MS])
    })
}

/// The notes whose title, body or tags contain `query` (case and runs of spaces don't matter, as in the Notes tab's
/// search), and that have `tag` when one is given; in `list`'s order.
pub fn search(db: &Db, query: &str, tag: Option<&str>) -> Result<Vec<Note>, String> {
    let needle = query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let tag = tag.map(clean_tag).filter(|tag| !tag.is_empty());
    // SQLite's LIKE and lower() only fold ASCII, so the matching is done here, as the tab does it.
    Ok(list(db)?
        .into_iter()
        .filter(|note| tag.as_ref().is_none_or(|tag| note.tags.contains(tag)))
        .filter(|note| {
            needle.is_empty() || {
                let mut haystack = vec![note.title.as_str(), note.body.as_str()];
                haystack.extend(note.tags.iter().map(String::as_str));
                haystack.join("\n").to_lowercase().contains(&needle)
            }
        })
        .collect())
}

/// Every tag in use with how many notes have it, the most used first.
pub fn tags(db: &Db) -> Result<Vec<(String, usize)>, String> {
    db.with(|conn| {
        conn.prepare_cached(
            "SELECT tag.value, COUNT(*) AS uses FROM notes, json_each(notes.tags) AS tag
             WHERE notes.deleted_at IS NULL GROUP BY tag.value ORDER BY uses DESC, tag.value",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as usize)))?
        .collect()
    })
}

/// One line, at most 120 characters.
pub fn clean_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TITLE).collect()
}

/// `\n` line ends, trimmed, at most 8000 UTF-16 units.
pub fn clean_body(body: &str) -> String {
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut units = 0;
    body.trim()
        .chars()
        .take_while(|char| {
            units += char.len_utf16();
            units <= MAX_BODY
        })
        .collect()
}

fn clean_tag(tag: &str) -> String {
    tag.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().chars().take(MAX_TAG).collect()
}

/// Lowercased, on one line, at most 24 characters each, no repeats, at most 6.
fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut clean: Vec<String> = Vec::new();
    for tag in tags.iter().map(|tag| clean_tag(tag)) {
        if !tag.is_empty() && !clean.contains(&tag) && clean.len() < MAX_TAGS {
            clean.push(tag);
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::temp_path;

    fn fields(title: &str, body: &str, tags: &[&str]) -> Fields {
        Fields {
            title: title.into(),
            body: body.into(),
            tags: Some(tags.iter().map(|tag| tag.to_string()).collect()),
            pinned: None,
        }
    }

    #[test]
    fn notes_are_made_changed_pinned_and_deleted() {
        let db = Db::in_memory().unwrap();
        let groceries = create(&db, &fields("Groceries", "- [ ] milk", &["Home"])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let ideas = create(&db, &fields("Ideas", "A pet that naps", &[])).unwrap();
        let ids: Vec<String> = list(&db).unwrap().into_iter().map(|note| note.id).collect();
        assert_eq!(ids, [ideas.clone(), groceries.clone()]);
        let note = get(&db, &groceries).unwrap().unwrap();
        assert_eq!(
            (note.title.as_str(), note.tags.as_slice(), note.pinned),
            ("Groceries", ["home".to_string()].as_slice(), false)
        );

        // An edit moves the note to the top; its tags stay when none are given.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(
            update(
                &db,
                &groceries,
                &Fields { title: "Groceries".into(), body: "- [x] milk".into(), tags: None, pinned: Some(true) }
            )
            .unwrap()
        );
        let note = get(&db, &groceries).unwrap().unwrap();
        assert_eq!(
            (note.body.as_str(), note.tags.as_slice(), note.pinned),
            ("- [x] milk", ["home".to_string()].as_slice(), true)
        );
        assert!(note.updated_at > note.created_at);
        assert_eq!(list(&db).unwrap()[0].id, groceries);

        // Pinning doesn't move it.
        let before = get(&db, &ideas).unwrap().unwrap().updated_at;
        assert!(set_pinned(&db, &ideas, true).unwrap());
        let note = get(&db, &ideas).unwrap().unwrap();
        assert!(note.pinned);
        assert_eq!(note.updated_at, before);

        assert!(delete(&db, &ideas).unwrap());
        assert_eq!(get(&db, &ideas).unwrap(), None);
        assert_eq!(list(&db).unwrap().len(), 1);
        // A deleted or unknown note can't be changed, pinned or deleted again.
        assert!(!update(&db, &ideas, &fields("Ideas", "again", &[])).unwrap());
        assert!(!set_pinned(&db, "nope", true).unwrap());
        assert!(!delete(&db, &ideas).unwrap());
    }

    #[test]
    fn notes_are_kept_across_restarts() {
        let path = temp_path("notes");
        let id = {
            let db = Db::open(&path).unwrap();
            let id = create(&db, &Fields { pinned: Some(true), ..fields("Kept", "Still here", &["later"]) }).unwrap();
            assert!(
                update(&db, &id, &Fields { pinned: Some(true), ..fields("Kept", "Still here, edited", &[]) }).unwrap()
            );
            id
        };
        let db = Db::open(&path).unwrap();
        let note = get(&db, &id).unwrap().unwrap();
        assert_eq!(
            (note.body.as_str(), note.pinned, note.tags),
            ("Still here, edited", true, vec!["later".to_string()])
        );
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn search_matches_title_body_and_tags_like_the_tab() {
        let db = Db::in_memory().unwrap();
        create(&db, &fields("Trip to İzmir", "Pack the CAMERA", &["travel"])).unwrap();
        create(&db, &fields("Ünïcode", "Ärger", &["work", "travel"])).unwrap();
        create(&db, &fields("", "nothing here", &[])).unwrap();
        let titles = |notes: Vec<Note>| notes.into_iter().map(|note| note.title).collect::<Vec<_>>();
        assert_eq!(titles(search(&db, "  the   camera ", None).unwrap()), ["Trip to İzmir"]);
        assert_eq!(titles(search(&db, "ärger", None).unwrap()), ["Ünïcode"]);
        assert_eq!(search(&db, "TRAVEL", None).unwrap().len(), 2);
        assert_eq!(titles(search(&db, "", Some("Work")).unwrap()), ["Ünïcode"]);
        assert_eq!(search(&db, "", None).unwrap().len(), 3);
        assert!(search(&db, "absent", None).unwrap().is_empty());
        assert_eq!(tags(&db).unwrap(), [("travel".to_string(), 2), ("work".to_string(), 1)]);
    }

    #[test]
    fn notes_are_cleaned_up_as_they_are_saved() {
        assert_eq!(clean_title("  Two\n lines\t here "), "Two lines here");
        assert_eq!(clean_title(&"x".repeat(200)).len(), MAX_TITLE);
        assert_eq!(clean_body("\r\n one\r\ntwo \n\n"), "one\ntwo");
        // Cut by UTF-16 units: an emoji is two.
        assert_eq!(clean_body(&"😀".repeat(5_000)).chars().count(), MAX_BODY / 2);
        let tags: Vec<String> =
            ["Work", "work", " Big   Ideas ", "", "a", "b", "c", "d", "e"].iter().map(|tag| tag.to_string()).collect();
        assert_eq!(clean_tags(&tags), ["work", "big ideas", "a", "b", "c", "d"]);
        assert_eq!(clean_tag(&"t".repeat(40)).len(), MAX_TAG);
    }

    #[test]
    fn deleted_notes_are_dropped_after_30_days() {
        let db = Db::in_memory().unwrap();
        let id = create(&db, &fields("Old", "gone", &[])).unwrap();
        delete(&db, &id).unwrap();
        assert_eq!(purge_deleted(&db, now()).unwrap(), 0);
        assert_eq!(purge_deleted(&db, now() + KEEP_DELETED_MS + 1_000).unwrap(), 1);
    }
}

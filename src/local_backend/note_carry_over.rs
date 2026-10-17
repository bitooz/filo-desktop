// Notes an older Filo kept on this Mac before saving them (its outbox: ~/.config/filo/offline/local/local/notes.json),
// carried into Filo's database once, when the app opens (backend.rs `carry_over_old_notes`). An edit goes to its note, a
// note made there becomes a note, a deletion deletes it. The file goes once every one of them is in; one that couldn't be
// saved stays in it for the next launch, and a file that can't be read is left alone.

use std::collections::BTreeMap;
use std::path::Path;

use filo_local::db::Db;
use filo_local::notes;
use serde::Deserialize;
use serde_json::{Value, json};

/// The outbox file as the older Filo wrote it.
#[derive(Debug, Default, Deserialize)]
struct Outbox {
    #[serde(default)]
    drafts: Vec<Value>,
    /// The ids notes made there were given, by their `local-…` key.
    #[serde(default)]
    ids: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Draft {
    /// The note's id, or `local-…` for a note made there and not saved yet.
    key: String,
    /// The note's id, once it was saved.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    fields: Fields,
    #[serde(default)]
    deleted: bool,
    /// A create went out without an answer: the note may be in the database already.
    #[serde(default)]
    create_sent: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fields {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    pinned: bool,
}

impl super::Local {
    /// Carries the outbox `file` into the database, then the live queries hear of it.
    pub fn carry_over_old_notes(&self, file: &Path) {
        let Ok(db) = self.db() else { return };
        match carry_over(db, file) {
            Ok(0) => {}
            Ok(count) => {
                eprintln!("filo: {count} notes carried over from {}", file.display());
                self.changed();
            }
            Err(error) => eprintln!("filo: {error}"),
        }
    }
}

/// Carries the drafts in `file` into `db`; how many notes changed.
fn carry_over(db: &Db, file: &Path) -> Result<usize, String> {
    let Ok(bytes) = std::fs::read(file) else { return Ok(0) };
    let outbox: Outbox =
        serde_json::from_slice(&bytes).map_err(|error| format!("{} couldn't be read: {error}", file.display()))?;
    let mut changed = 0;
    let mut left = Vec::new();
    for value in outbox.drafts {
        let carried = serde_json::from_value::<Draft>(value.clone())
            .map_err(|error| error.to_string())
            .and_then(|draft| carry(db, &outbox.ids, &draft));
        match carried {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(error) => {
                eprintln!("filo: a note in {} couldn't be carried over: {error}", file.display());
                left.push(value);
            }
        }
    }
    if left.is_empty() {
        std::fs::remove_file(file).map_err(|error| format!("{} couldn't be removed: {error}", file.display()))?;
    } else {
        let bytes = serde_json::to_vec_pretty(&json!({ "drafts": left, "ids": outbox.ids }))
            .map_err(|error| error.to_string())?;
        crate::config::write_atomic(file, &bytes)
            .map_err(|error| format!("{} couldn't be saved: {error}", file.display()))?;
    }
    Ok(changed)
}

/// Puts one draft in the database; whether a note changed.
fn carry(db: &Db, ids: &BTreeMap<String, String>, draft: &Draft) -> Result<bool, String> {
    let id = draft
        .id
        .clone()
        .or_else(|| ids.get(&draft.key).cloned())
        .or_else(|| (!draft.key.starts_with("local-")).then(|| draft.key.clone()));
    if draft.deleted {
        return match id {
            Some(id) => notes::delete(db, &id),
            None => Ok(false),
        };
    }
    let fields = notes::Fields {
        title: draft.fields.title.clone(),
        body: draft.fields.body.clone(),
        tags: draft.fields.tags.clone(),
        pinned: Some(draft.fields.pinned),
    };
    let id = match id {
        Some(id) => Some(id),
        None if draft.create_sent => same_note(db, &fields)?,
        None => None,
    };
    if let Some(id) = &id {
        if let Some(note) = notes::get(db, id)?
            && note.title == notes::clean_title(&fields.title)
            && note.body == notes::clean_body(&fields.body)
            && note.pinned == draft.fields.pinned
        {
            return Ok(false);
        }
        if notes::update(db, id, &fields)? {
            return Ok(true);
        }
    }
    // Made there and never saved, or deleted meanwhile: a note of its own rather than lost.
    notes::create(db, &fields)?;
    Ok(true)
}

/// A note already made from these fields, by a create whose answer never came.
fn same_note(db: &Db, fields: &notes::Fields) -> Result<Option<String>, String> {
    let (title, body) = (notes::clean_title(&fields.title), notes::clean_body(&fields.body));
    Ok(notes::list(db)?.into_iter().find(|note| note.title == title && note.body == body).map(|note| note.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(bytes: &str) -> std::path::PathBuf {
        let file =
            std::env::temp_dir().join(format!("filo-note-carry-over-{}", uuid::Uuid::new_v4())).join("notes.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, bytes).unwrap();
        file
    }

    fn fields(title: &str, body: &str) -> notes::Fields {
        notes::Fields { title: title.into(), body: body.into(), tags: None, pinned: None }
    }

    fn titles(db: &Db) -> Vec<String> {
        let mut titles: Vec<String> = notes::list(db).unwrap().into_iter().map(|note| note.title).collect();
        titles.sort();
        titles
    }

    #[test]
    fn the_outbox_is_carried_over_once() {
        let db = Db::in_memory().unwrap();
        let edited = notes::create(&db, &fields("Groceries", "milk")).unwrap();
        let deleted = notes::create(&db, &fields("Old plan", "gone")).unwrap();
        let answered = notes::create(&db, &fields("Sent before Filo quit", "twice?")).unwrap();
        let made = notes::create(&db, &fields("Made there", "same note")).unwrap();
        let untouched = notes::create(&db, &fields("Untouched", "same")).unwrap();
        let file = temp_file(
            &json!({
                "drafts": [
                    // An edit not saved yet.
                    { "key": edited, "id": edited, "fields": { "title": "Groceries", "body": "milk, eggs", "tags": ["home"], "pinned": true },
                      "updatedAt": 2.0, "deleted": false, "createSent": false, "error": null, "version": 3 },
                    // A note written there that never reached the database.
                    { "key": "local-1", "id": null, "fields": { "title": "Written offline", "body": "kept", "pinned": false },
                      "updatedAt": 3.0, "deleted": false, "createSent": false, "version": 1 },
                    // A deletion.
                    { "key": deleted, "id": deleted, "fields": { "title": "", "body": "" }, "updatedAt": 4.0, "deleted": true, "version": 1 },
                    // A create whose answer never came: found, not made twice.
                    { "key": "local-2", "id": null, "fields": { "title": "Sent before Filo quit", "body": "twice?", "pinned": false },
                      "updatedAt": 5.0, "deleted": false, "createSent": true, "version": 1 },
                    // Already saved: left as it is, its date too.
                    { "key": untouched, "id": untouched, "fields": { "title": "Untouched", "body": "same", "pinned": false },
                      "updatedAt": 6.0, "deleted": false, "version": 1 },
                    // Made there and saved, then edited (its id by its key).
                    { "key": "local-3", "fields": { "title": "Renamed", "body": "same note" }, "updatedAt": 7.0, "version": 2 },
                ],
                "ids": { "local-3": made },
            })
            .to_string(),
        );
        let untouched_at = notes::get(&db, &untouched).unwrap().unwrap().updated_at;

        assert_eq!(carry_over(&db, &file).unwrap(), 4);
        let note = notes::get(&db, &edited).unwrap().unwrap();
        assert_eq!(
            (note.body.as_str(), note.tags.as_slice(), note.pinned),
            ("milk, eggs", ["home".to_string()].as_slice(), true)
        );
        assert_eq!(notes::get(&db, &deleted).unwrap(), None);
        assert_eq!(notes::get(&db, &untouched).unwrap().unwrap().updated_at, untouched_at);
        assert_eq!(notes::get(&db, &answered).unwrap().unwrap().title, "Sent before Filo quit");
        assert_eq!(notes::get(&db, &made).unwrap().unwrap().title, "Renamed");
        assert_eq!(titles(&db), ["Groceries", "Renamed", "Sent before Filo quit", "Untouched", "Written offline"]);
        assert!(!file.exists());

        // Once: the next launch finds no file and changes nothing.
        assert_eq!(carry_over(&db, &file).unwrap(), 0);
        assert_eq!(notes::list(&db).unwrap().len(), 5);
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn an_unanswered_create_found_in_the_database_is_not_made_again() {
        let db = Db::in_memory().unwrap();
        let file = temp_file(
            &json!({ "drafts": [{ "key": "local-1", "fields": { "title": "List", "body": "milk" }, "createSent": true, "version": 1 }] }).to_string(),
        );
        notes::create(&db, &fields("List", "milk")).unwrap();
        assert_eq!(carry_over(&db, &file).unwrap(), 0);
        assert_eq!(titles(&db), ["List"]);
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }

    #[test]
    fn a_draft_that_cant_be_read_stays_for_the_next_launch() {
        let db = Db::in_memory().unwrap();
        let file = temp_file(
            &json!({
                "drafts": [
                    { "key": "local-1", "fields": { "title": "Saved", "body": "yes" } },
                    { "fields": { "title": "No key" } },
                ],
                "ids": { "local-9": "n9" },
            })
            .to_string(),
        );
        assert_eq!(carry_over(&db, &file).unwrap(), 1);
        assert_eq!(titles(&db), ["Saved"]);
        let left: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(left, json!({ "drafts": [{ "fields": { "title": "No key" } }], "ids": { "local-9": "n9" } }));

        // A file that isn't JSON is left as it is.
        std::fs::write(&file, "not json").unwrap();
        assert!(carry_over(&db, &file).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json");
        std::fs::remove_dir_all(file.parent().unwrap()).ok();
    }
}

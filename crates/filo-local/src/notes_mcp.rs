// The Notes tab's MCP tools (notes.rs): what the coding tool calls in a Filo conversation to find, read, write,
// change and delete the user's notes on this Mac. They're the same notes the Notes tab shows, cleaned up the same way
// (a title of one line, a body of at most 8000 characters, at most 6 lowercased tags), and a deleted note is kept 30
// days as the tab keeps it.

use std::path::Path;

use serde_json::{Value, json};

use crate::db::Db;
use crate::notes::{self, Fields, Note};

const NAMES: &[&str] = &["list_notes", "read_note", "create_note", "update_note", "delete_note"];
/// `list_notes` shows at most this many.
const LIST_DEFAULT: usize = 30;
const LIST_MAX: usize = 100;
/// What `list_notes` shows of a body.
const PREVIEW_CHARS: usize = 160;

/// Whether `name` is one of these tools.
pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn tools() -> Vec<Value> {
    let annotations = |read_only: bool, destructive: bool, idempotent: bool| json!({ "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": false });
    let tags =
        json!({ "type": "array", "items": { "type": "string" }, "description": "Up to 6 short tags (lowercased)." });
    vec![
        json!({
            "name": "list_notes",
            "title": "List notes",
            "description": "The user's notes on the Notes tab, most recently changed first (pinned ones say so), with their ids, tags and the start of each: those containing `query` (empty for all) and having `tag`, if given. read_note gives a note in full.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text in the title, body or tags." },
                    "tag": { "type": "string" },
                    "limit": { "type": "number", "description": "At most this many (100 at most, 30 by default)." }
                },
                "additionalProperties": false
            },
            "annotations": annotations(true, false, true)
        }),
        json!({
            "name": "read_note",
            "title": "Read note",
            "description": "One of the user's notes in full, by its id (from list_notes).",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
            "annotations": annotations(true, false, true)
        }),
        json!({
            "name": "create_note",
            "title": "Create note",
            "description": "Write a new note on the user's Notes tab, when they ask you to take, make or save a note, or to note something down for later (also when they say it in an answer to a coding tool's question). A note is this, not a memory (save_memory) and not something for a coding tool to keep. Say it's noted only once this succeeded. Returns its id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "One line (at most 120 characters)." },
                    "body": { "type": "string", "description": "The note, plain text or Markdown (at most 8000 characters)." },
                    "tags": tags,
                    "pinned": { "type": "boolean", "description": "Pin it to the top of the Notes tab." }
                },
                "required": ["title"],
                "additionalProperties": false
            },
            "annotations": annotations(false, false, false)
        }),
        json!({
            "name": "update_note",
            "title": "Update note",
            "description": "Change one of the user's notes, by its id: only what's given changes (`body` replaces the whole body, so read_note first to add to it). Given `tags` replace its tags. The note moves to the top of the Notes tab.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "tags": tags,
                    "pinned": { "type": "boolean" }
                },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": annotations(false, false, true)
        }),
        json!({
            "name": "delete_note",
            "title": "Delete note",
            "description": "Delete one of the user's notes, by its id, when they ask you to. It's kept 30 days, then dropped for good.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
            "annotations": annotations(false, true, true)
        }),
    ]
}

use crate::mcp::text_arg as text;

fn id(args: &Value) -> Result<&str, String> {
    crate::mcp::id_arg(args, "Give the note's `id` (from list_notes).")
}

fn tags_of(args: &Value) -> Result<Option<Vec<String>>, String> {
    match args.get("tags") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            Ok(Some(items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()))
        }
        Some(Value::String(one)) => Ok(Some(one.split(',').map(str::to_string).collect())),
        Some(_) => Err("`tags` is a list of short words.".into()),
    }
}

fn day(at: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(at)
        .earliest()
        .map(|time| time.format("%a %-d %b %Y").to_string())
        .unwrap_or_default()
}

fn line(note: &Note) -> String {
    let flat = note.body.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview =
        if flat.is_empty() { String::new() } else { format!(": {}", crate::store::cut(&flat, PREVIEW_CHARS)) };
    let tags = if note.tags.is_empty() { String::new() } else { format!(" #{}", note.tags.join(" #")) };
    let pinned = if note.pinned { ", pinned" } else { "" };
    let title = if note.title.is_empty() { "(untitled)" } else { note.title.as_str() };
    format!("- [{}] {title}{tags} (changed {}{pinned}){preview}", note.id, day(note.updated_at))
}

fn full(note: &Note) -> String {
    let tags = if note.tags.is_empty() { "none".to_string() } else { note.tags.join(", ") };
    let title = if note.title.is_empty() { "(untitled)" } else { note.title.as_str() };
    format!(
        "{title} (id {}, changed {}{})\nTags: {tags}\n\n{}",
        note.id,
        day(note.updated_at),
        if note.pinned { ", pinned" } else { "" },
        if note.body.is_empty() { "(empty)" } else { note.body.as_str() }
    )
}

/// Runs one of these tools on the database at `db_path`.
pub fn call(db_path: &Path, name: &str, args: &Value) -> Result<String, String> {
    let db = Db::open(db_path).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    call_with(&db, name, args)
}

fn call_with(db: &Db, name: &str, args: &Value) -> Result<String, String> {
    const GONE: &str = "There's no note with that id (or it was deleted). list_notes shows them.";
    match name {
        "list_notes" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_f64)
                .map_or(LIST_DEFAULT, |limit| (limit.max(1.) as usize).min(LIST_MAX));
            let found = notes::search(db, text(args, "query").unwrap_or_default(), text(args, "tag"))?;
            if found.is_empty() {
                return Ok("No notes match.".into());
            }
            let lines: Vec<String> = found.iter().take(limit).map(line).collect();
            let more = found.len().saturating_sub(lines.len());
            Ok(if more > 0 {
                format!("{}\n({more} more: narrow the query or raise the limit.)", lines.join("\n"))
            } else {
                lines.join("\n")
            })
        }
        "read_note" => notes::get(db, id(args)?)?.map(|note| full(&note)).ok_or_else(|| GONE.into()),
        "create_note" => {
            let fields = Fields {
                title: text(args, "title").unwrap_or_default().to_string(),
                body: text(args, "body").unwrap_or_default().to_string(),
                tags: tags_of(args)?,
                pinned: args.get("pinned").and_then(Value::as_bool),
            };
            if notes::clean_title(&fields.title).is_empty() && notes::clean_body(&fields.body).is_empty() {
                return Err("A note needs a `title` or a `body`.".into());
            }
            let id = notes::create(db, &fields)?;
            Ok(format!("Saved (id {id}). The user sees it on the Notes tab."))
        }
        "update_note" => {
            let id = id(args)?;
            let note = notes::get(db, id)?.ok_or(GONE)?;
            let tags = tags_of(args)?;
            let fields = Fields {
                title: text(args, "title").map_or(note.title, str::to_string),
                body: text(args, "body").map_or(note.body, str::to_string),
                tags: tags.clone(),
                pinned: Some(args.get("pinned").and_then(Value::as_bool).unwrap_or(note.pinned)),
            };
            if !notes::update(db, id, &fields)? {
                return Err(GONE.into());
            }
            // notes::update keeps the tags when given none (the note page sends none); here an empty list clears them.
            if tags.as_ref().is_some_and(|tags| tags.iter().all(|tag| tag.trim().is_empty())) {
                notes::clear_tags(db, id)?;
            }
            let note = notes::get(db, id)?.ok_or(GONE)?;
            Ok(format!("Updated:\n{}", line(&note)))
        }
        "delete_note" => {
            if notes::delete(db, id(args)?)? {
                Ok("Deleted. It's kept 30 days, then dropped.".into())
            } else {
                Err(GONE.into())
            }
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_named_and_handled() {
        let names: Vec<String> = tools().iter().map(|tool| tool["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, NAMES);
        assert!(names.iter().all(|name| handles(name)));
        assert!(!handles("send_message"));
    }

    #[test]
    fn the_tools_list_create_read_update_and_delete() {
        let db = Db::in_memory().unwrap();
        let call = |name: &str, args: Value| call_with(&db, name, &args);

        assert_eq!(call("list_notes", json!({})).unwrap(), "No notes match.");
        assert!(call("create_note", json!({ "title": "  ", "body": "" })).is_err());
        let made =
            call("create_note", json!({ "title": "Groceries", "body": "Milk\nEggs", "tags": ["Shopping", "home"] }))
                .unwrap();
        let id = notes::list(&db).unwrap()[0].id.clone();
        assert!(made.contains(&id), "{made}");
        call("create_note", json!({ "title": "Ideas", "body": "A boat", "pinned": true })).unwrap();

        let listed = call("list_notes", json!({ "query": "eggs" })).unwrap();
        assert!(
            listed.starts_with(&format!("- [{id}] Groceries #shopping #home (changed "))
                && listed.ends_with("): Milk Eggs"),
            "{listed}"
        );
        assert_eq!(call("list_notes", json!({ "tag": "home" })).unwrap().lines().count(), 1);
        assert!(call("list_notes", json!({ "limit": 1 })).unwrap().contains("(1 more"));
        assert!(call("read_note", json!({ "id": id })).unwrap().ends_with("Tags: shopping, home\n\nMilk\nEggs"));

        // Only what's given changes; the pin and tags stay unless given.
        call("update_note", json!({ "id": id, "body": "Milk\nEggs\nBread" })).unwrap();
        let note = notes::get(&db, &id).unwrap().unwrap();
        assert_eq!(
            (note.title.as_str(), note.body.as_str(), note.tags.len(), note.pinned),
            ("Groceries", "Milk\nEggs\nBread", 2, false)
        );
        let ideas = notes::search(&db, "boat", None).unwrap().remove(0);
        call("update_note", json!({ "id": ideas.id, "title": "Boat ideas" })).unwrap();
        assert!(notes::get(&db, &ideas.id).unwrap().unwrap().pinned);
        call("update_note", json!({ "id": id, "tags": [] })).unwrap();
        assert!(notes::get(&db, &id).unwrap().unwrap().tags.is_empty());
        assert!(call("update_note", json!({ "id": "missing", "title": "x" })).is_err());

        assert!(call("delete_note", json!({ "id": id })).unwrap().starts_with("Deleted"));
        assert!(call("delete_note", json!({ "id": id })).is_err());
        assert!(call("read_note", json!({ "id": id })).is_err());
        assert_eq!(call("list_notes", json!({})).unwrap().lines().count(), 1);
    }
}

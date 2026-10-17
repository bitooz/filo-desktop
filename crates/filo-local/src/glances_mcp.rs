// `show_glance`: a small visual above Filo's reply in the chat, the MCP tool
// the coding tool calls before send_message. The glance is kept in Filo's database (glances.rs) with an assistant
// message that says the same in words, which is what the engine's history, previews and notifications read; the app
// draws the card from the message's `glance`. In a turn of the agents' group chat it posts in the group, in the
// speaker's name, like send_message.

use serde_json::{Value, json};

use crate::db::Db;
use crate::glance_document::{MAX_HTML, MAX_TEXT};
use crate::glances::{self, New, Table};
use crate::mcp::Target;

pub const NAME: &str = "show_glance";

pub fn handles(name: &str) -> bool {
    name == NAME
}

fn description() -> String {
    format!(
        "Show a glance: a small visual above your reply in the chat (a day's shape, a comparison, progress, a trend over weeks, where time or money went, a timeline). Use it when a picture says it faster than sentences; plain answers need none. Call it before send_message, then reply in a sentence or two about what matters, without repeating its numbers.
{FRAGMENT_RULES}
- The card is as wide as the content needs, from about 560 px (a reply bubble) up to the chat's width, and as tall as the content: keep it under about 300 px. Fluid content (percentages, flex, wrapping text and chips) fills the card and keeps it near a bubble's width; a fixed or nowrap width (a table, a timeline) makes the card that wide, and scrolls sideways where the chat is narrower. Opened larger it's about 680 px wide and html[data-size=large] matches, for a taller chart.
{TEXT_RULES}
- wide: true for a glance that needs room (a wide table, a long timeline): the card takes the chat's whole width. Otherwise it's as wide as its content, from a reply bubble's width up to the chat's, and content wider than that scrolls sideways.
- asOf: when the data is from (\"as of 10:41\", \"Aug 10 to Oct 4\"). icon: the card's head shows it, so pick the one that fits what the visual shows: {}.",
        icon_uses()
    )
}

/// How a glance's `html` is written: the sandbox, the theme's colours and sizes, and the helpers (the Feed's glances,
/// feed_mcp.rs, follow the same).
pub(crate) const FRAGMENT_RULES: &str = "- html: a body fragment (no <html>, <head> or <body>), with inline <style>, SVG and <script> allowed. It runs in a sandbox with no network: no links, fonts or images from URLs (data: URIs only). It sits on the card's surface: keep the background transparent and use the theme's colours, never fixed ones: var(--text), var(--textSecondary), var(--textTertiary), var(--surfaceStrong), var(--border), var(--accent), var(--success), var(--warning), var(--danger); series use var(--c1) to var(--c4), in that order; one measure uses var(--accent) alone. Sizes: var(--subhead), var(--footnote), var(--caption).
- Helpers: .legend-row with <span><i class=\"key-box\" style=\"--k:var(--c1)\"></i>Name</span> (or .key-line for lines), .axis for tick labels, .grid-h for gridlines, table.dt for small tables (td.r for numbers), .num for tabular figures. Put a tooltip on any mark with data-tt='{\"t\":\"Week of Sep 7\",\"rows\":[[\"var(--c1)\",\"Focus\",\"12 h\",\"line\"]]}'.";

/// What `text`, `table` and `notes` are for.
pub(crate) const TEXT_RULES: &str = "- text: the same content in plain sentences, one line per item; Copy and screen readers use it, and it's what the chat's history keeps.
- table: the data behind a chart, when there is any (the \"Table\" view). notes: one to three short lines on what to notice, shown when it's opened larger.";

/// The head's icons with what each is for, as the descriptions list them.
pub(crate) fn icon_uses() -> String {
    ICON_USES.iter().map(|(icon, shows)| format!("{icon} ({shows})")).collect::<Vec<_>>().join(", ")
}

/// What each of the head's icons is for, so the glance gets the one that fits rather than the first that might.
const ICON_USES: &[(&str, &str)] = &[
    ("calendar-clock", "a day, a schedule"),
    ("trending-up", "a trend, growth"),
    ("chart-line", "a measure over time"),
    ("chart-column-stacked", "amounts side by side, or made of parts"),
    ("chart-pie", "shares of a whole"),
    ("chart-gantt", "a timeline, a plan"),
    ("grid-3x3", "a matrix, a comparison table"),
    ("layout-grid", "a mockup, a layout"),
];

/// A text field of at most `max` characters, for a glance tool's schema.
pub(crate) fn short(max: usize, description: &str) -> Value {
    json!({ "type": "string", "maxLength": max, "description": description })
}

/// The fields every glance tool takes, the chat's (`show_glance`) and the Feed's (feed_mcp.rs); each adds its own title
/// and the rest.
pub(crate) fn glance_fields() -> serde_json::Map<String, Value> {
    let cells = json!({ "type": "array", "items": { "type": "string", "maxLength": 200 }, "maxItems": 8 });
    let fields = json!({
        "html": { "type": "string", "minLength": 1, "maxLength": MAX_HTML, "description": "The visual, as a body fragment." },
        "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT, "description": "The same in plain sentences, one line per item." },
        "asOf": short(60, "When the data is from."),
        "icon": { "type": "string", "enum": glances::ICONS },
        "source": short(120, "Where the data came from (\"your calendar\")."),
        "notes": short(1_200, "One to three short lines on what to notice, shown opened larger."),
        "table": {
            "type": "object",
            "properties": { "columns": cells, "rows": { "type": "array", "items": cells, "maxItems": 60 } },
            "required": ["columns", "rows"],
            "additionalProperties": false
        }
    });
    let Value::Object(fields) = fields else { unreachable!("an object") };
    fields
}

pub fn tools() -> Vec<Value> {
    let mut properties = glance_fields();
    properties.insert("title".into(), json!({ "type": "string", "minLength": 2, "maxLength": 80, "description": "What it shows, in a few words (the card's head)." }));
    properties.insert("visualLabel".into(), short(24, "What kind of visual it is (\"Chart\", \"Timeline\")."));
    properties.insert(
        "wide".into(),
        json!({ "type": "boolean", "description": "As wide as the chat allows, for a glance that needs room." }),
    );
    vec![json!({
        "name": NAME,
        "title": "Show glance",
        "description": description(),
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": ["title", "html", "text"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    })]
}

/// The glance Filo asked for. Models fill optional fields with "" and empty lists: those count as left out.
pub(crate) fn new_from(arguments: &Value) -> New {
    let text = |key: &str| {
        arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
    };
    let strings = |value: &Value| {
        value.as_array().map(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_string).unwrap_or_else(|| item.to_string()))
                .collect::<Vec<_>>()
        })
    };
    let table = arguments.get("table").and_then(|table| {
        let columns = strings(table.get("columns")?)?;
        let rows = table.get("rows")?.as_array()?.iter().filter_map(strings).collect::<Vec<_>>();
        Some(Table { columns, rows })
    });
    New {
        title: text("title").unwrap_or_default(),
        html: text("html").unwrap_or_default(),
        text: text("text").unwrap_or_default(),
        as_of: text("asOf"),
        icon: text("icon"),
        source: text("source"),
        visual_label: text("visualLabel"),
        notes: text("notes"),
        table,
        wide: arguments.get("wide").and_then(Value::as_bool).unwrap_or(false),
    }
}

pub fn call(target: &Target, arguments: &Value) -> Result<String, String> {
    let new = glances::checked(&new_from(arguments))?;
    let content = glances::plain_text(&new.title, new.as_of.as_deref(), &new.text);
    let db = Db::open(&target.db).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    let now = crate::now_ms() as i64;
    let turn = target.turn_id.as_deref();
    if let Some(speaker) = &target.group_speaker {
        let session = crate::group::ensure(&db, now)?;
        // The message and its glance land together: the group never shows the message without its card.
        let posted = crate::group::post_with(
            &db,
            speaker,
            &content,
            crate::group::Asks::default(),
            turn,
            now,
            |tx, message| glances::insert(tx, &new, &session, &message.id, turn, now).map(|_| ()),
        )?;
        // Run in the speaker's own chat: a line there linking to it (mcp.rs `send_message`).
        if target.own_chat {
            let mut line = target.store.agent_message(&target.chat_id, content, target.turn_id.clone());
            line.in_group = Some(posted.id);
            target
                .store
                .append(&target.chat_id, &line)
                .map_err(|error| format!("Shown in the group, but couldn't note it in your chat: {error}"))?;
        }
        return Ok(SHOWN.into());
    }
    let message = target.store.agent_message(&target.chat_id, content, target.turn_id.clone());
    // The glance first, then its message: the chat reads the message's card as soon as the message is there.
    let glance = db.write(|tx| glances::insert(tx, &new, &target.chat_id, &message.id, turn, now))?;
    if let Err(error) = target.store.append(&target.chat_id, &message) {
        let _ = glances::remove(&db, &glance.id);
        return Err(format!("Couldn't show the glance: {error}"));
    }
    Ok(SHOWN.into())
}

const SHOWN: &str = "Shown in the chat. Now reply with send_message: a sentence or two about what matters, without repeating its numbers.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::engine::Engine;
    use crate::store::Role;
    use crate::store::tests::temp_store;

    fn args() -> Value {
        json!({
            "title": "Your week",
            "html": "<div class=\"chart\" style=\"height:120px\"></div>",
            "text": "Mon: 3 h\nTue: 5 h",
            "asOf": "Oct 4",
            "icon": "chart-column-stacked",
            "notes": "",
            "wide": true,
            "table": { "columns": ["Day", "Hours"], "rows": [["Mon", "3 h"], ["Tue", "5 h"]] }
        })
    }

    #[test]
    fn every_icon_says_what_its_for() {
        let described: Vec<&str> = ICON_USES.iter().map(|(icon, _)| *icon).collect();
        assert!(
            glances::ICONS.iter().all(|icon| described.contains(icon)) && described.len() == glances::ICONS.len(),
            "{described:?}"
        );
    }

    #[test]
    fn a_glance_in_a_chat_is_a_message_with_its_card() {
        let store = temp_store("glance-chat");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("turn-1".into()),
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: crate::mcp::Part::All,
        };
        assert_eq!(call(&target, &args()).unwrap(), SHOWN);
        let messages = store.messages(&chat.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            (messages[0].role, messages[0].text.as_str(), messages[0].turn.as_deref()),
            (Role::Assistant, "Your week (Oct 4)\nMon: 3 h\nTue: 5 h", Some("turn-1"))
        );
        let db = Db::open(&target.db).unwrap();
        let head = glances::heads(&db, &chat.id).unwrap().remove(&messages[0].id).unwrap();
        assert_eq!(
            (head.title.as_str(), head.as_of.as_deref(), head.icon.as_deref(), head.wide),
            ("Your week", Some("Oct 4"), Some("chart-column-stacked"), true)
        );
        let glance = glances::get(&db, &head.glance_id).unwrap().unwrap();
        assert_eq!((glance.notes, glance.table.unwrap().rows.len()), (None, 2));

        // Nothing is kept for a glance that can't be shown.
        assert!(call(&target, &json!({ "title": "x", "html": "", "text": "y" })).is_err());
        let gone = Target { chat_id: "no-such-chat".into(), ..target.clone() };
        assert!(call(&gone, &args()).is_err());
        assert!(glances::heads(&db, "no-such-chat").unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn in_a_group_turn_the_glance_is_posted_in_the_group() {
        let store = temp_store("glance-group");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let db_path = store.root().join(db::FILE);
        let db = Db::open(&db_path).unwrap();
        let session = crate::group::ensure(&db, 1).unwrap();
        crate::group::add_user_message(&db, "thread-1", "@Chef chart my week", 1).unwrap();
        let turn = crate::group::enqueue(&db, "thread-1", "chef", 1, "asked", true, 1).unwrap().unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some(turn.clone()),
            db: db_path,
            group_speaker: Some("chef".into()),
            own_chat: false,
            part: crate::mcp::Part::All,
        };
        call(&target, &args()).unwrap();
        let (messages, _) = crate::group::messages(&db, 10).unwrap();
        let posted = messages.last().unwrap();
        assert_eq!(
            (posted.speaker.as_deref(), posted.thread.as_deref(), posted.turn.as_deref()),
            (Some("chef"), Some("thread-1"), Some(turn.as_str()))
        );
        assert!(glances::heads(&db, &session).unwrap().contains_key(&posted.id));
        assert!(store.messages(&chat.id).unwrap().is_empty());
        // Run in the speaker's own chat: the chat gets a line linking to the post, no card of its own.
        let own = Target { own_chat: true, ..target.clone() };
        call(&own, &args()).unwrap();
        let (messages, _) = crate::group::messages(&db, 10).unwrap();
        let line = store.messages(&chat.id).unwrap().pop().unwrap();
        assert_eq!(
            (line.in_group.as_deref(), line.turn.as_deref()),
            (Some(messages.last().unwrap().id.as_str()), Some(turn.as_str()))
        );
        assert!(glances::heads(&db, &chat.id).unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }
}

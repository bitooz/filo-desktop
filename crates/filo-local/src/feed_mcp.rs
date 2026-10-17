// The Feed's MCP tools (feed.rs): what the coding tool calls to publish, change, list and take down posts in the
// user's Feed in the Filo app, and to see the topics they follow. They act on the Feed in Filo's database on this Mac,
// nothing outside it. `publish_feed_glance` posts a glance (the live visual of glances_mcp.rs, made the same way) and
// `update_feed_glance` changes one in place; a glance can offer `choices`, and the user's pick comes back to the
// conversation that posted it (local_backend/feed.rs `feed:chooseInGlance`).
//
// They're offered two ways:
//
// - In every Filo conversation (mcp.rs): Filo's chats and the agents'. A post says who shared it (the agent's name, or
//   the main agent's in a chat of no agent's) and keeps the conversation it came from. That's how the Feed fills: each topic is watched by an agent on a
//   routine (local_backend/feed.rs), and its runs, in the agent's conversation, publish with these tools.
// - On their own, for a coding tool's session outside Filo (`filo --feed-mcp`, `filo-local feed-mcp`):
//
//       claude mcp add filo-feed -- /Applications/Filo.app/Contents/MacOS/filo --feed-mcp --author "Claude Code"
//       codex mcp add filo-feed -- /Applications/Filo.app/Contents/MacOS/filo --feed-mcp --author Codex
//
//   `--db <filo.db>` for another database (the app's by default), `--author <name>` for who posts say shared them.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{NaiveDateTime, TimeZone};
use serde_json::{Value, json};

use crate::db::{self, Db};
use crate::feed::{self, NewPost, Origin, Post, PostChange};

const NAMES: &[&str] = &[
    "publish_feed_post",
    "update_feed_post",
    "list_feed_posts",
    "remove_feed_post",
    "list_feed_topics",
    "publish_feed_glance",
    "update_feed_glance",
];
/// A glance offers at most this many choices.
const MAX_CHOICES: usize = 6;
/// Around the choices made from `choices` in a glance's html, so a change can put others in their place.
const CHOICES_START: &str = "<!--filo:choices-->";
const CHOICES_END: &str = "<!--/filo:choices-->";
/// The standalone server's name, as the CLI shows its tools (`mcp__filo__publish_feed_post`): the same as in a chat.
const SERVER_NAME: &str = crate::mcp::SERVER_NAME;
/// `list_feed_posts` shows at most this many.
const LIST_LIMIT: usize = 50;

/// Whether `name` is one of these tools.
pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn tools() -> Vec<Value> {
    let kinds = feed::KINDS;
    let post_fields = json!({
        "title": { "type": "string", "description": "The headline, as the page has it (one line)." },
        "url": { "type": "string", "description": "The page's link (https://…). Leave it out for a post that's only your words. A link can be in the Feed once." },
        "note": { "type": "string", "description": "One or two sentences in your own words: what it is and why it's worth the user's time." },
        "topic": { "type": "string", "description": "The topic it's about, as the user names it (see list_feed_topics). Without one it goes under \u{201c}From your agents\u{201d}." },
        "kind": { "type": "string", "enum": kinds, "description": "What it is (article by default)." },
        "source_name": { "type": "string", "description": "Who published it, like \u{201c}The Guardian\u{201d} (the link's site by default)." },
        "why": { "type": "string", "description": "Why the user is seeing it, shown when they ask, like \u{201c}You asked me to keep an eye on Lisbon's jazz clubs\u{201d}." },
        "summary": { "type": "string", "description": "A short summary of the page, shown under More." },
        "points": { "type": "array", "items": { "type": "string" }, "description": "Up to 8 key points, one line each." },
        "image_url": { "type": "string", "description": "A picture for the post (https://…), only one you saw on the page, like its preview image." },
        "event_starts_at": { "type": "string", "description": "For an event: when it starts, the user's local time \u{201c}YYYY-MM-DD HH:MM\u{201d} (or ISO 8601 with an offset)." }
    });
    let mut publish_fields = post_fields.clone();
    let mut update_fields = post_fields;
    publish_fields["title"]["description"] = json!("The headline, as the page has it (one line). Required.");
    update_fields["id"] =
        json!({ "type": "string", "description": "The post's id (from publish_feed_post or list_feed_posts)." });
    update_fields["event_starts_at"]["description"] =
        json!("When the event starts, local \u{201c}YYYY-MM-DD HH:MM\u{201d}; empty to clear it.");
    vec![
        json!({
            "name": "publish_feed_post",
            "title": "Publish feed post",
            "description": "Add a post to the user's Feed in the Filo app: a link worth their time (an article, video, thread or event) with your note on it, or a short update of your own. It shows at the top of their Feed on this Mac, saying you shared it. Use it when the user asks you to put something in their Feed, or for something they asked you to watch for. Returns the post's id, to change it later with update_feed_post.",
            "inputSchema": { "type": "object", "properties": publish_fields, "required": ["title"], "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "update_feed_post",
            "title": "Update feed post",
            "description": "Change a post in the user's Feed by its id: only the fields given change (an empty text clears an optional one). The user's likes, saves and hides stay.",
            "inputSchema": { "type": "object", "properties": update_fields, "required": ["id"], "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "list_feed_posts",
            "title": "List feed posts",
            "description": "The posts in the user's Feed, newest first, with their ids: those containing every word of `query` (empty for all), on `topic` when given. Check it before publishing so you don't repeat a post.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "topic": { "type": "string" },
                    "include_hidden": { "type": "boolean", "description": "Also the posts the user hid." },
                    "limit": { "type": "number", "description": "At most this many (50 at most, 20 by default)." }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "remove_feed_post",
            "title": "Remove feed post",
            "description": "Take a post out of the user's Feed for good, by its id: one you published by mistake, or when the user asks.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "list_feed_topics",
            "title": "List feed topics",
            "description": "The topics the user follows in their Feed: what they want to hear about.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "publish_feed_glance",
            "title": "Publish feed glance",
            "description": glance_description(),
            "inputSchema": { "type": "object", "properties": glance_fields(false), "required": ["title", "html", "text"], "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "update_feed_glance",
            "title": "Update feed glance",
            "description": "Change a glance in the user's Feed in place, by its post id (from publish_feed_glance or list_feed_posts): only the fields given change, and the post keeps its place, likes and saves. Use it to keep a glance current (today's numbers, a plan that moved) instead of posting a copy. `choices` replaces the choices (an empty list takes them away) and clears the user's last pick.",
            "inputSchema": { "type": "object", "properties": glance_fields(true), "required": ["id"], "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
    ]
}

fn glance_description() -> String {
    format!(
        "Post a glance to the user's Feed in the Filo app: a small live visual (a chart, a timeline, a table, a comparison, a question with choices) the user sees in their Feed and can hover, sort and click, with your one-line note on why it matters. Use it for something worth coming back to outside the chat, like a daily focus chart or a week's summary; for a visual in your reply, use show_glance instead. Returns the post's id: update it in place later with update_feed_glance rather than posting it again.
{}
- The Feed shows it about 600 px wide and as tall as the content: keep it under about 300 px, with fluid widths. Opened larger it's about 680 px wide and html[data-size=large] matches.
{}
- note: one line on why the user is seeing it. topic: one of the topics they follow (list_feed_topics), if it fits.
- choices: up to {MAX_CHOICES} answers the user can pick, shown as buttons under the visual (`label`, and an optional short `detail`). Their pick comes back to you as a message in this conversation, so ask only what you'll act on. Any element in html with data-choice=\"answer\" sends that answer too.
- asOf: when the data is from. icon: the one that fits what the visual shows: {}.",
        crate::glances_mcp::FRAGMENT_RULES,
        crate::glances_mcp::TEXT_RULES.replace(", and it's what the chat's history keeps", ""),
        crate::glances_mcp::icon_uses()
    )
}

/// The glance tools' fields (the update's with the post's `id`, and none required).
fn glance_fields(update: bool) -> Value {
    use crate::glances_mcp::short;
    let mut fields = Value::Object(crate::glances_mcp::glance_fields());
    let own = json!({
        "title": { "type": "string", "minLength": 2, "maxLength": 80, "description": "What it shows, in a few words (the post's title)." },
        "note": short(feed::MAX_NOTE, "One line on why it matters to the user now."),
        "topic": { "type": "string", "description": "The topic it's about, as the user names it (see list_feed_topics). Without one it goes under \u{201c}From your agents\u{201d}." },
        "why": short(feed::MAX_WHY, "Why the user is seeing it, shown when they ask."),
        "choices": {
            "type": "array",
            "maxItems": MAX_CHOICES,
            "description": "Answers the user can pick; the pick comes back to you in this conversation.",
            "items": {
                "type": "object",
                "properties": {
                    "label": { "type": "string", "minLength": 1, "maxLength": 80 },
                    "detail": { "type": "string", "maxLength": 120 }
                },
                "required": ["label"],
                "additionalProperties": false
            }
        }
    });
    if let (Value::Object(fields), Value::Object(own)) = (&mut fields, own) {
        fields.extend(own);
    }
    if update {
        fields["id"] = json!({ "type": "string", "description": "The glance post's id." });
    }
    fields
}

/// The `choices` given, as (label, detail); None when none were.
fn choices(args: &Value) -> Result<Option<Vec<(String, Option<String>)>>, String> {
    let Some(items) = args.get("choices").filter(|value| !value.is_null()) else { return Ok(None) };
    let items = items.as_array().ok_or("`choices` is a list of {label, detail}.")?;
    let text = |item: &Value, key: &str, max: usize| {
        item.get(key)
            .and_then(Value::as_str)
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect::<String>())
            .filter(|text| !text.is_empty())
    };
    let picked: Vec<(String, Option<String>)> = items
        .iter()
        .filter_map(|item| match item {
            Value::String(label) => {
                Some((label.trim().chars().take(80).collect::<String>(), None)).filter(|(label, _)| !label.is_empty())
            }
            _ => Some((text(item, "label", 80)?, text(item, "detail", 120))),
        })
        .take(MAX_CHOICES)
        .collect();
    Ok(Some(picked))
}

/// The choices as buttons under the visual, styled with the theme's roles; a click sends the answer in the Feed
/// (`data-choice`, src/screens/feed_glance.rs), and does nothing where nothing listens.
fn choices_html(choices: &[(String, Option<String>)]) -> String {
    if choices.is_empty() {
        return String::new();
    }
    let escape = crate::glance_document::escape_html;
    let buttons: String = choices
        .iter()
        .map(|(label, detail)| {
            let detail = detail
                .as_deref()
                .map(|detail| format!("<span class=\"filo-choice-detail\">{}</span>", escape(detail)))
                .unwrap_or_default();
            format!(
                "<button type=\"button\" class=\"filo-choice\" data-choice=\"{}\"><span>{}</span>{detail}</button>",
                escape(label),
                escape(label)
            )
        })
        .collect();
    format!(
        "{CHOICES_START}<style>.filo-choices{{display:flex;flex-direction:column;gap:6px;margin-top:10px}}.filo-choice{{display:flex;align-items:center;gap:10px;width:100%;padding:9px 12px;border:1px solid var(--border);border-radius:10px;background:transparent;color:var(--text);font:inherit;font-size:var(--subhead);text-align:left;cursor:pointer}}.filo-choice:hover{{background:var(--surfaceStrong)}}.filo-choice-detail{{margin-left:auto;color:var(--textSecondary);font-size:var(--footnote)}}.filo-choice.filo-picked{{border-color:var(--accent);background:color-mix(in srgb,var(--accent) 12%,transparent)}}</style><div class=\"filo-choices\">{buttons}</div>{CHOICES_END}"
    )
}

/// A glance's html without the choices made from `choices`, and those choices' html.
fn split_choices(html: &str) -> (String, String) {
    match (html.find(CHOICES_START), html.find(CHOICES_END)) {
        (Some(start), Some(end)) if end > start => {
            let end = end + CHOICES_END.len();
            (format!("{}{}", &html[..start], &html[end..]), html[start..end].to_string())
        }
        _ => (html.to_string(), String::new()),
    }
}

/// The glance as it is after `args` change `old` (none for a new one): only the fields given change.
fn glance_from(args: &Value, old: Option<&crate::glances::Glance>) -> Result<crate::glances::New, String> {
    let given = crate::glances_mcp::new_from(args);
    let has = |key: &str| args.get(key).is_some_and(|value| !value.is_null());
    let (base, block) = old.map(|old| split_choices(&old.html)).unwrap_or_default();
    let base = if has("html") || old.is_none() { given.html.clone() } else { base };
    let block = match choices(args)? {
        Some(choices) => choices_html(&choices),
        None => block,
    };
    let new = match old {
        None => crate::glances::New { html: format!("{base}{block}"), ..given },
        Some(old) => crate::glances::New {
            title: if has("title") { given.title } else { old.title.clone() },
            html: format!("{base}{block}"),
            text: if has("text") { given.text } else { old.text.clone() },
            as_of: if has("asOf") { given.as_of } else { old.as_of.clone() },
            icon: if has("icon") { given.icon } else { old.icon.clone() },
            source: if has("source") { given.source } else { old.source.clone() },
            visual_label: old.visual_label.clone(),
            notes: if has("notes") { given.notes } else { old.notes.clone() },
            table: if has("table") { given.table } else { old.table.clone() },
            wide: old.wide,
        },
    };
    crate::glances::checked(&new)
}

/// Runs a tool on the database at `db_path` for `origin`.
pub fn call(db_path: &Path, origin: &Origin, name: &str, args: &Value) -> Result<String, String> {
    let db = Db::open(db_path).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    call_with(&db, &chrono::Local, origin, name, args, crate::now_ms() as i64)
}

/// Who a post from a Filo conversation says shared it, from the database at `db_path` (`origin_of_chat`).
pub fn origin_in_chat(db_path: &Path, chat_id: &str) -> Origin {
    match Db::open(db_path) {
        Ok(db) => origin_of_chat(&db, chat_id),
        Err(_) => Origin { author: "Filo".into(), chat_id: Some(chat_id.to_string()) },
    }
}

/// Who a post from a Filo conversation says shared it: the agent whose conversation it is, else the main agent, who
/// owns what's no agent's (Filo, the app, while there are no agents).
pub fn origin_of_chat(db: &Db, chat_id: &str) -> Origin {
    let agent = crate::app_data::agent_of_chat(db, chat_id).ok().flatten().map(|agent| agent.name);
    let main = || crate::main_agent::name(db);
    Origin { author: agent.or_else(main).unwrap_or_else(|| "Filo".into()), chat_id: Some(chat_id.to_string()) }
}

/// Who watches a topic: its routine's agent by name, the main agent for one of no agent's.
fn watcher(db: &Db, topic: &feed::Topic) -> Option<String> {
    let routine = crate::schedule::routine(db, topic.routine_id.as_deref()?, None).ok()??;
    let Some(agent) = routine.agent_id else { return crate::main_agent::name(db) };
    crate::app_data::agent(db, &agent).ok()?.map(|agent| agent.name)
}

use crate::mcp::text_arg as text;

fn id(args: &Value) -> Result<&str, String> {
    crate::mcp::id_arg(args, "Give the post's `id`.")
}

fn points(args: &Value) -> Result<Option<Vec<String>>, String> {
    match args.get("points") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            Ok(Some(items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()))
        }
        Some(Value::String(one)) => {
            Ok(Some(one.lines().map(|line| line.trim_start_matches(['-', '*', '•', ' ']).to_string()).collect()))
        }
        Some(_) => Err("`points` is a list of short lines.".into()),
    }
}

/// When an event starts: a local "YYYY-MM-DD HH:MM", ISO 8601 with an offset, or ms since the epoch. `None` for empty.
fn starts_at<Tz: TimeZone>(tz: &Tz, value: &Value) -> Result<Option<i64>, String> {
    let text = match value {
        Value::Null => return Ok(None),
        Value::Number(number) => return number.as_f64().map(|ms| Some(ms as i64)).ok_or_else(|| "Not a time.".into()),
        Value::String(text) => text.trim(),
        _ => return Err("`event_starts_at` is a local date and time, like \u{201c}2026-10-12 21:00\u{201d}.".into()),
    };
    if text.is_empty() {
        return Ok(None);
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(Some(time.timestamp_millis()));
    }
    for format in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(local) = NaiveDateTime::parse_from_str(text, format) {
            return tz
                .from_local_datetime(&local)
                .earliest()
                .map(|time| Some(time.timestamp_millis()))
                .ok_or_else(|| "That time doesn't exist here.".into());
        }
    }
    Err(format!("Couldn't read {text:?} as a time: use the local \u{201c}YYYY-MM-DD HH:MM\u{201d}."))
}

fn post_line(post: &Post, now: i64) -> String {
    let mut marks = Vec::new();
    if post.liked {
        marks.push("liked".to_string());
    }
    if post.saved {
        marks.push("saved to Notes".into());
    }
    if let Some(hidden) = &post.hidden {
        marks.push(format!("hidden ({})", hidden.replace('_', " ")));
    }
    if let Some(choice) = &post.glance_choice {
        marks.push(format!("the user picked \u{201c}{choice}\u{201d}"));
    }
    let link = if post.url.is_empty() { String::new() } else { format!(" · {}", post.url) };
    let marks = if marks.is_empty() { String::new() } else { format!(" · {}", marks.join(", ")) };
    format!(
        "- [{}] {} ({}, {}) · {} · shared by {} {}{link}{marks}",
        post.id,
        post.title,
        post.kind,
        post.source_name,
        post.topic_label,
        post.author,
        crate::schedule::ago(now - post.created_at)
    )
}

fn call_with<Tz: TimeZone>(
    db: &Db,
    tz: &Tz,
    origin: &Origin,
    name: &str,
    args: &Value,
    now: i64,
) -> Result<String, String> {
    let owned = |key: &str| text(args, key).map(str::to_string);
    match name {
        "publish_feed_post" => {
            let new = NewPost {
                title: owned("title").unwrap_or_default(),
                url: owned("url").unwrap_or_default(),
                note: owned("note").unwrap_or_default(),
                why: owned("why"),
                topic: owned("topic"),
                kind: owned("kind"),
                source_name: owned("source_name"),
                image_url: owned("image_url"),
                event_starts_at: starts_at(tz, args.get("event_starts_at").unwrap_or(&Value::Null))?,
                summary: owned("summary"),
                points: points(args)?.unwrap_or_default(),
            };
            let post = feed::publish(db, &new, origin, now)?;
            Ok(format!(
                "Published (id {}) under \u{201c}{}\u{201d}. It's at the top of the user's Feed.",
                post.id, post.topic_label
            ))
        }
        "update_feed_post" => {
            let change = PostChange {
                title: owned("title"),
                url: owned("url"),
                note: owned("note"),
                why: owned("why"),
                topic: owned("topic"),
                kind: owned("kind"),
                source_name: owned("source_name"),
                image_url: owned("image_url"),
                event_starts_at: match args.get("event_starts_at") {
                    None => None,
                    Some(value) => Some(starts_at(tz, value)?),
                },
                summary: owned("summary"),
                points: points(args)?,
            };
            match feed::update(db, id(args)?, &change, now)? {
                Some(post) => Ok(format!("Updated:\n{}", post_line(&post, now))),
                None => Err("There's no post with that id in the Feed.".into()),
            }
        }
        "list_feed_posts" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_f64)
                .map_or(20, |limit| limit.clamp(1., LIST_LIMIT as f64) as usize);
            let with_hidden = args.get("include_hidden").and_then(Value::as_bool).unwrap_or(false);
            let posts =
                feed::search(db, text(args, "query").unwrap_or_default(), text(args, "topic"), with_hidden, limit)?;
            Ok(if posts.is_empty() {
                "No posts match.".into()
            } else {
                posts.iter().map(|post| post_line(post, now)).collect::<Vec<_>>().join("\n")
            })
        }
        "remove_feed_post" => {
            if feed::remove(db, id(args)?)? {
                Ok("Removed from the Feed.".into())
            } else {
                Err("There's no post with that id in the Feed.".into())
            }
        }
        "publish_feed_glance" => {
            let glance = glance_from(args, None)?;
            let new = NewPost {
                note: owned("note").unwrap_or_default(),
                why: owned("why"),
                topic: owned("topic"),
                ..NewPost::default()
            };
            let post = feed::publish_glance(db, &new, &glance, origin, now)?;
            let asks = if args.get("choices").and_then(Value::as_array).is_some_and(|choices| !choices.is_empty()) {
                " When the user picks a choice, it comes back to you as a message in this conversation."
            } else {
                ""
            };
            Ok(format!(
                "Posted the glance to the Feed (id {}) under \u{201c}{}\u{201d}. Keep it current with update_feed_glance and this id rather than posting it again.{asks}",
                post.id, post.topic_label
            ))
        }
        "update_feed_glance" => {
            let id = id(args)?;
            let Some(post) = feed::get(db, id)? else { return Err("There's no post with that id in the Feed.".into()) };
            if post.kind != feed::GLANCE_KIND {
                return Err("That post isn't a glance: change it with update_feed_post.".into());
            }
            const GLANCE_FIELDS: [&str; 9] =
                ["title", "html", "text", "asOf", "icon", "source", "notes", "table", "choices"];
            let remake = GLANCE_FIELDS.iter().any(|key| args.get(*key).is_some_and(|value| !value.is_null()));
            let glance = match remake {
                true => Some(glance_from(args, feed::post_glance(db, id)?.as_ref())?),
                false => None,
            };
            let change =
                PostChange { note: owned("note"), why: owned("why"), topic: owned("topic"), ..PostChange::default() };
            let post = feed::update_glance(db, id, &change, glance.as_ref(), origin, now)?
                .ok_or("There's no post with that id in the Feed.")?;
            // New choices ask again.
            if args.get("choices").is_some_and(|value| !value.is_null()) {
                feed::set_glance_choice(db, &post.id, None)?;
            }
            Ok(format!("Updated in place:\n{}", post_line(&post, now)))
        }
        "list_feed_topics" => {
            let topics = feed::topics(db)?;
            Ok(if topics.is_empty() {
                "The user follows no topics yet.".into()
            } else {
                topics
                    .iter()
                    .map(|topic| match watcher(db, topic) {
                        Some(name) => format!("- {} (watched by {name})", topic.label),
                        None => format!("- {}", topic.label),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

// ---------------------------------------------------------------------------
// The server on its own
// ---------------------------------------------------------------------------

/// Where the standalone server works, and as whom.
#[derive(Clone, Debug)]
pub struct Target {
    pub db: PathBuf,
    pub origin: Origin,
}

impl Target {
    /// `[--db <filo.db>] [--author <name>]`; the database is the app's unless given.
    pub fn from_args(args: &[String], default_db: PathBuf) -> Result<Target, String> {
        let mut target = Target { db: default_db, origin: Origin { author: "Your agent".into(), chat_id: None } };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().cloned().ok_or_else(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--db" => target.db = PathBuf::from(value()?),
                "--author" => target.origin.author = value()?,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(target)
    }
}

/// Runs the Feed's tools on stdin/stdout until the client closes stdin (`filo --feed-mcp …`). The process exit code.
pub fn main_from_args(args: &[String], default_db: PathBuf) -> i32 {
    let target = match Target::from_args(args, default_db) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("filo feed-mcp: {error}");
            return 2;
        }
    };
    match crate::mcp::serve_stdio(&target, io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("filo feed-mcp: {error}");
            1
        }
    }
}

impl crate::mcp::StdioServer for Target {
    fn info(&self) -> (&'static str, &'static str, &'static str) {
        (
            SERVER_NAME,
            "Filo Feed",
            "The user's Feed in the Filo app on this Mac: publish posts to it, change them and see what's there. Only the Feed is changed; nothing is posted anywhere else.",
        )
    }

    fn tools(&self) -> Value {
        json!(tools())
    }

    fn call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        crate::mcp::tool_result(if !handles(name) {
            Err(format!("Unknown tool: {name}"))
        } else if !crate::mcp::pages(&self.db).feed {
            // Feed is off in Filo (pages.rs): its posts stay, nothing new comes in.
            Err("Feed is off in Settings".into())
        } else {
            call(&self.db, &self.origin, name, &arguments)
        })
    }
}

/// The app's database, for the standalone server's default.
pub fn default_db() -> PathBuf {
    crate::store::default_root().join(db::FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(target: &Target, input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        crate::mcp::serve_stdio(target, input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    #[test]
    fn the_standalone_server_publishes_updates_lists_and_removes() {
        let path = crate::db::tests::temp_path("feed-mcp");
        let target = Target::from_args(
            &["--db".into(), path.display().to_string(), "--author".into(), "Claude Code".into()],
            PathBuf::new(),
        )
        .unwrap();
        let db = Db::open(&path).unwrap();
        feed::add_topic(&db, "Rust GUIs", 0).unwrap();
        let call = |id: u32, name: &str, arguments: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
            run(&target, &request.to_string()).remove(0)["result"].clone()
        };
        let text = |result: &Value| result["content"][0]["text"].as_str().unwrap().to_string();

        let replies = run(
            &target,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}",
        );
        assert_eq!(replies.len(), 2);
        let names: Vec<&str> =
            replies[1]["result"]["tools"].as_array().unwrap().iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(names, NAMES);

        let published = call(
            3,
            "publish_feed_post",
            json!({ "title": "GPUI 1.0", "url": "https://gpui.rs/blog", "note": "It shipped.", "topic": "rust guis", "points": ["Fast", "Native"] }),
        );
        assert_eq!(published["isError"], false, "{published}");
        let post = feed::list(&db, crate::now_ms() as i64).unwrap().remove(0);
        assert!(text(&published).contains(&post.id) && text(&published).contains("Rust GUIs"));
        assert_eq!(
            (post.author.as_str(), post.why.as_str()),
            ("Claude Code", "Claude Code shared this from a conversation.")
        );

        let updated = call(
            4,
            "update_feed_post",
            json!({ "id": post.id, "kind": "event", "event_starts_at": "2026-10-12 21:00", "summary": "The release." }),
        );
        assert_eq!(updated["isError"], false, "{updated}");
        let post = feed::get(&db, &post.id).unwrap().unwrap();
        assert_eq!(
            (post.kind.as_str(), post.summary.as_deref(), post.points.len()),
            ("event", Some("The release."), 2)
        );
        let expected = chrono::Local
            .from_local_datetime(&NaiveDateTime::parse_from_str("2026-10-12 21:00", "%Y-%m-%d %H:%M").unwrap())
            .earliest()
            .unwrap();
        assert_eq!(post.event_starts_at, Some(expected.timestamp_millis()));

        assert!(
            text(&call(5, "list_feed_posts", json!({ "query": "gpui" }))).contains(&format!("[{}] GPUI 1.0", post.id))
        );
        assert_eq!(text(&call(6, "list_feed_posts", json!({ "topic": "elsewhere" }))), "No posts match.");
        assert_eq!(text(&call(7, "list_feed_topics", json!({}))), "- Rust GUIs");
        assert_eq!(
            call(8, "publish_feed_post", json!({ "title": "Dup", "url": "https://gpui.rs/blog" }))["isError"],
            true
        );
        assert_eq!(
            call(9, "publish_feed_post", json!({ "title": "When?", "kind": "event", "event_starts_at": "soon" }))["isError"],
            true
        );
        assert_eq!(call(10, "update_feed_post", json!({ "id": "nope", "title": "x" }))["isError"], true);
        assert_eq!(call(11, "remove_feed_post", json!({ "id": post.id }))["isError"], false);
        assert_eq!(call(12, "remove_feed_post", json!({ "id": post.id }))["isError"], true);
        assert_eq!(call(13, "send_message", json!({ "text": "hi" }))["isError"], true);
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_glance_is_posted_with_its_choices_and_changed_in_place() {
        let db = Db::in_memory().unwrap();
        let tz = chrono::FixedOffset::east_opt(0).unwrap();
        let origin = Origin { author: "Release Manager".into(), chat_id: Some("chat-r".into()) };
        let call = |name: &str, args: Value| call_with(&db, &tz, &origin, name, &args, 1_000);
        let published = call(
            "publish_feed_glance",
            json!({
                "title": "Ship Nightly build 15?",
                "html": "<p class=\"q\">Ready?</p>",
                "text": "Ship tonight, or wait.",
                "note": "Needs your pick.",
                "icon": "layout-grid",
                "choices": [{ "label": "Ship tonight", "detail": "All ready" }, { "label": "Wait <a bit>" }, { "label": "  " }]
            }),
        )
        .unwrap();
        assert!(published.contains("update_feed_glance") && published.contains("comes back to you"), "{published}");
        let post = feed::list(&db, 1_000).unwrap().remove(0);
        assert_eq!(
            (post.kind.as_str(), post.note.as_str(), post.author.as_str()),
            ("glance", "Needs your pick.", "Release Manager")
        );
        let html = feed::post_glance(&db, &post.id).unwrap().unwrap().html;
        assert!(html.starts_with("<p class=\"q\">Ready?</p><!--filo:choices-->"), "{html}");
        assert!(html.contains(
            "data-choice=\"Ship tonight\"><span>Ship tonight</span><span class=\"filo-choice-detail\">All ready</span>"
        ));
        assert!(html.contains("data-choice=\"Wait &lt;a bit&gt;\"") && html.matches("<button").count() == 2);

        // A new visual keeps the choices; new choices take the place of the old and ask again.
        feed::set_glance_choice(&db, &post.id, Some("Ship tonight")).unwrap();
        call("update_feed_glance", json!({ "id": post.id, "html": "<p>Still ready?</p>" })).unwrap();
        let html = feed::post_glance(&db, &post.id).unwrap().unwrap().html;
        assert!(
            html.starts_with("<p>Still ready?</p><!--filo:choices-->") && html.matches("<button").count() == 2,
            "{html}"
        );
        assert_eq!(feed::get(&db, &post.id).unwrap().unwrap().glance_choice.as_deref(), Some("Ship tonight"));
        let updated =
            call("update_feed_glance", json!({ "id": post.id, "choices": ["Friday"], "title": "Ship on Friday?" }))
                .unwrap();
        assert!(updated.contains("Ship on Friday?"), "{updated}");
        let glance = feed::post_glance(&db, &post.id).unwrap().unwrap();
        assert!(glance.html.starts_with("<p>Still ready?</p>") && glance.html.matches("<button").count() == 1);
        assert_eq!(
            (glance.title.as_str(), glance.text.as_str(), glance.icon.as_deref()),
            ("Ship on Friday?", "Ship tonight, or wait.", Some("layout-grid"))
        );
        let post = feed::get(&db, &post.id).unwrap().unwrap();
        assert_eq!((post.title.as_str(), post.glance_choice.as_deref()), ("Ship on Friday?", None));
        call("update_feed_glance", json!({ "id": post.id, "choices": [] })).unwrap();
        assert!(!feed::post_glance(&db, &post.id).unwrap().unwrap().html.contains("filo:choices"));
        // Only the note: the same glance.
        let before = feed::get(&db, &post.id).unwrap().unwrap().glance.unwrap().glance_id;
        call("update_feed_glance", json!({ "id": post.id, "note": "Decided." })).unwrap();
        assert_eq!(feed::get(&db, &post.id).unwrap().unwrap().glance.unwrap().glance_id, before);

        assert!(call("publish_feed_glance", json!({ "title": "No visual", "text": "x" })).is_err());
        let link = call("publish_feed_post", json!({ "title": "A link", "url": "https://example.com" })).unwrap();
        let link_id = feed::list(&db, 1_000).unwrap().into_iter().find(|post| post.kind == "article").unwrap().id;
        assert!(link.contains(&link_id));
        assert!(
            call("update_feed_glance", json!({ "id": link_id, "html": "<p>x</p>" }))
                .unwrap_err()
                .contains("isn't a glance")
        );
        assert!(call("update_feed_glance", json!({ "id": "nope", "note": "x" })).is_err());
        assert!(call("list_feed_posts", json!({})).unwrap().contains("(glance, Release Manager)"));
    }

    #[test]
    fn topics_say_which_agent_watches_them() {
        let path = crate::db::tests::temp_path("feed-mcp-topics");
        let db = Db::open(&path).unwrap();
        crate::app_data::add_agent(&db, &crate::app_data::tests::agent("a1", "Scout", Some("chat-a"), 1.)).unwrap();
        let key = feed::add_topic(&db, "Lisbon", 0).unwrap();
        let routine = crate::schedule::NewRoutine {
            title: "Feed: Lisbon".into(),
            instructions: "Look.".into(),
            days: json!([0, 1, 2, 3, 4, 5, 6]),
            minute_of_day: json!(480),
            schedule: Value::Null,
            agent_id: Some("a1".into()),
        };
        let routine = crate::schedule::create_routine(&db, &chrono::Local, routine, 0).unwrap();
        feed::set_topic_routine(&db, &key, Some(&routine.id)).unwrap();
        let origin = origin_in_chat(&path, "chat-a");
        assert_eq!(call(&path, &origin, "list_feed_topics", &json!({})).unwrap(), "- Lisbon (watched by Scout)");
        let result = call(
            &path,
            &origin,
            "publish_feed_post",
            &json!({ "title": "Fado", "url": "https://example.com/fado", "topic": "Lisbon" }),
        )
        .unwrap();
        assert!(result.starts_with("Published"));
        assert_eq!(feed::list(&db, crate::now_ms() as i64).unwrap()[0].author, "Scout");
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn posts_from_a_conversation_say_who_shared_them() {
        let path = crate::db::tests::temp_path("feed-mcp-origin");
        let db = Db::open(&path).unwrap();
        crate::app_data::add_agent(&db, &crate::app_data::tests::agent("a1", "Scout", Some("chat-a"), 1.)).unwrap();
        assert_eq!(origin_in_chat(&path, "chat-a").author, "Scout");
        // A chat of no agent's is the main agent's; with none, Filo's, the app's.
        assert_eq!(origin_in_chat(&path, "chat-b").author, "Filo");
        crate::app_data::set_profile(&db, crate::app_data::MAIN_AGENT, Some(&"a1".into())).unwrap();
        assert_eq!(origin_in_chat(&path, "chat-b").author, "Scout");
        assert_eq!(origin_in_chat(&path, "chat-b").chat_id.as_deref(), Some("chat-b"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn event_times_are_read_in_local_time() {
        let tz = chrono::FixedOffset::east_opt(5 * 3600).unwrap();
        assert_eq!(starts_at(&tz, &json!("2026-10-10 17:30")).unwrap(), Some(1_791_635_400_000));
        assert_eq!(starts_at(&tz, &json!("2026-10-10T12:30:00Z")).unwrap(), Some(1_791_635_400_000));
        assert_eq!(starts_at(&tz, &json!(5)).unwrap(), Some(5));
        assert_eq!(starts_at(&tz, &json!("")).unwrap(), None);
        assert!(starts_at(&tz, &json!("tomorrow")).is_err());
    }
}

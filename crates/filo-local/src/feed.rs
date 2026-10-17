// The Feed in Filo's database (db.rs): the topics the user follows, the posts on them and the Feed's settings. The
// app's `feed:*` functions read and change it; Filo's MCP server (feed_mcp.rs) lets the coding tool publish and
// update posts from a conversation.
//
// Posts come from agents, not a hosted service: each topic is watched by one of the user's agents on a routine of its
// own (schedule.rs; `routine_id` here, made when the topic is added, local_backend/feed.rs), whose runs search the web
// and publish what they find, and any conversation can publish a post too. A post says who shared it. A post is shaped
// as the pages read a `PublicPost` (docs/spec-shell-feed-artifacts-connectors.md, B › Post shape). A link is in the Feed once: publishing it again is refused with the post that has it.
//
// The Feed used to run a check of its own (`feed_runs`, and the schedule in `feed_settings`); the table stays, unused,
// and the old schedule only says how often the topics carried over to their routines are watched.
//
// A post can be a glance (kind `glance`, design: the "Feed with glances" mock): the same live visual a conversation
// shows above a reply (glances.rs), kept in the glances table and named by the post's `glance_id`. Changing it makes a
// new glance for the same post (a glance never changes once made), so the post stays where it is with its likes and
// saves; a pick the user made in it is kept as `glance_choice`.

use chrono::TimeZone;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::Serialize;

use crate::db::Db;

/// The Feed's tables (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE feed_topics (
    topic_key  TEXT PRIMARY KEY NOT NULL,
    label      TEXT NOT NULL,
    source     TEXT NOT NULL DEFAULT 'user_added',
    created_at INTEGER NOT NULL
);
CREATE TABLE feed_posts (
    id              TEXT PRIMARY KEY NOT NULL,
    topic_key       TEXT NOT NULL,
    topic_label     TEXT NOT NULL,
    kind            TEXT NOT NULL,
    title           TEXT NOT NULL,
    note            TEXT NOT NULL,
    why             TEXT NOT NULL,
    url             TEXT NOT NULL,
    source_name     TEXT NOT NULL,
    image_url       TEXT,
    event_starts_at INTEGER,
    summary         TEXT,
    points          TEXT NOT NULL DEFAULT '[]',
    rank            INTEGER NOT NULL DEFAULT 0,
    liked           INTEGER NOT NULL DEFAULT 0,
    note_id         TEXT,
    hidden          TEXT,
    hidden_at       INTEGER,
    reminder_id     TEXT,
    author          TEXT NOT NULL,
    chat_id         TEXT,
    run_id          TEXT,
    day_key         TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX feed_posts_by_time ON feed_posts (created_at DESC, rank);
CREATE UNIQUE INDEX feed_posts_by_url ON feed_posts (url) WHERE url != '';
CREATE TABLE feed_settings (
    id               INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    pick_minute      INTEGER NOT NULL DEFAULT 480,
    interval_minutes INTEGER,
    next_run_at      INTEGER
);
CREATE TABLE feed_runs (
    id          TEXT PRIMARY KEY NOT NULL,
    topic_key   TEXT,
    trigger     TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    error       TEXT
);
CREATE INDEX feed_runs_by_start ON feed_runs (started_at DESC);
";

const MINUTE: i64 = 60_000;
/// `feed:mine` lists this many of the newest posts (`FEED_PAGE` on the server), plus those hidden lately.
const FEED_PAGE: usize = 40;
/// Hidden posts stay in `feed:mine` this long, so the Feed's undo row has its post.
const HIDDEN_LISTED_MS: i64 = 12 * 60 * MINUTE;
pub const MAX_TOPICS: usize = 20;
const MAX_TOPIC_LABEL: usize = 80;
/// `feed:topicsMine` lists this many, newest first.
const TOPICS_LISTED: usize = 50;
const MAX_TITLE: usize = 200;
pub const MAX_NOTE: usize = 600;
pub const MAX_WHY: usize = 300;
const MAX_SOURCE: usize = 80;
const MAX_SUMMARY: usize = 4_000;
const MAX_POINT: usize = 300;
const MAX_POINTS: usize = 8;
const MAX_URL: usize = 2_000;
pub const KINDS: [&str; 4] = ["article", "video", "thread", "event"];
/// A glance post's kind: not one `publish` takes, only `publish_glance`.
pub const GLANCE_KIND: &str = "glance";
pub const MAX_CHOICE: usize = 200;
/// A topic's routine runs once a day at this time unless the user picks how often.
pub const DEFAULT_PICK_MINUTE: i64 = 8 * 60;
/// The topic posts go under when the one who publishes names none.
const DEFAULT_TOPIC: &str = "From your agents";

/// A post as the pages read it (`PublicPost`), with who published it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    #[serde(rename = "_id")]
    pub id: String,
    pub topic_key: String,
    pub topic_label: String,
    pub kind: String,
    pub title: String,
    pub note: String,
    pub why: String,
    pub url: String,
    pub source_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_starts_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub points: Vec<String>,
    pub liked: bool,
    pub saved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<String>,
    pub reminded: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// A glance post's card head (src/screens/chat/types.rs `GlanceRef`); the page comes from `glances:get`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glance: Option<crate::glances::Head>,
    /// What the user last picked in a glance post's choices.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glance_choice: Option<String>,
}

/// What a new post is made of. Only the title is required; the rest is filled in or tidied (`publish`).
#[derive(Clone, Debug, Default)]
pub struct NewPost {
    pub title: String,
    pub url: String,
    pub note: String,
    pub why: Option<String>,
    pub topic: Option<String>,
    pub kind: Option<String>,
    pub source_name: Option<String>,
    pub image_url: Option<String>,
    pub event_starts_at: Option<i64>,
    pub summary: Option<String>,
    pub points: Vec<String>,
}

/// A post's fields to change; `None` leaves one as it is, an empty text clears an optional one.
#[derive(Clone, Debug, Default)]
pub struct PostChange {
    pub title: Option<String>,
    pub url: Option<String>,
    pub note: Option<String>,
    pub why: Option<String>,
    pub topic: Option<String>,
    pub kind: Option<String>,
    pub source_name: Option<String>,
    pub image_url: Option<String>,
    /// `Some(None)` clears it.
    pub event_starts_at: Option<Option<i64>>,
    pub summary: Option<String>,
    pub points: Option<Vec<String>>,
}

/// Who publishes: a name the post says shared it, and the conversation it came from.
#[derive(Clone, Debug, Default)]
pub struct Origin {
    pub author: String,
    pub chat_id: Option<String>,
}

/// A topic the user follows, and the routine that watches it (schedule.rs; none when it was deleted).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Topic {
    pub topic_key: String,
    pub label: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<String>,
    pub created_at: i64,
}

/// The Feed's settings: who answers Ask on posts (none: the main agent), and the schedule its own check had, which the
/// topics carried over to their routines get.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub ask_agent_id: Option<String>,
    pub pick_minute: i64,
    pub interval_minutes: Option<i64>,
}

#[cfg(test)]
fn now() -> i64 {
    crate::now_ms() as i64
}

// ---------------------------------------------------------------------------
// Tidying
// ---------------------------------------------------------------------------

/// One line, at most `max` characters.
fn line(text: &str, max: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// Paragraphs kept, trimmed, at most `max` characters.
fn block(text: &str, max: usize) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").trim().chars().take(max).collect()
}

fn optional(text: Option<&str>, max: usize) -> Option<String> {
    text.map(|text| block(text, max)).filter(|text| !text.is_empty())
}

/// A topic's key: its label in lowercase (the server's rule).
fn topic_key(label: &str) -> String {
    line(label, MAX_TOPIC_LABEL).to_lowercase()
}

/// An http(s) link, or why it isn't one. Empty is allowed (a post that's only Filo's words).
fn clean_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(String::new());
    }
    let lower = url.to_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"));
    if rest.is_none_or(|rest| rest.is_empty() || rest.starts_with('/')) || url.chars().any(char::is_whitespace) {
        return Err(format!("Not a web link: {url:?}. Links start with https:// or http://."));
    }
    if url.len() > MAX_URL {
        return Err(format!("The link is too long (at most {MAX_URL} characters)."));
    }
    Ok(url.to_string())
}

/// "example.com" for "https://www.example.com/a/b".
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?.split(':').next()?;
    let host = host.strip_prefix("www.").unwrap_or(host).to_lowercase();
    (!host.is_empty()).then_some(host)
}

fn clean_kind(kind: Option<&str>) -> Result<String, String> {
    let kind =
        kind.map(|kind| kind.trim().to_lowercase()).filter(|kind| !kind.is_empty()).unwrap_or_else(|| "article".into());
    if KINDS.contains(&kind.as_str()) {
        Ok(kind)
    } else {
        Err(format!("A post's kind is one of {}, not {kind:?}.", KINDS.join(", ")))
    }
}

fn clean_points(points: &[String]) -> Vec<String> {
    points.iter().map(|point| line(point, MAX_POINT)).filter(|point| !point.is_empty()).take(MAX_POINTS).collect()
}

fn day_key(at: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(at)
        .earliest()
        .map(|time| time.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Posts
// ---------------------------------------------------------------------------

const COLUMNS: &str = "id, day_key, topic_key, topic_label, kind, title, note, why, url, source_name, image_url, event_starts_at, \
summary, points, liked, note_id, hidden, reminder_id, created_at, updated_at, author, chat_id, glance_id, glance_choice, \
(SELECT as_of FROM glances WHERE glances.id = feed_posts.glance_id), (SELECT icon FROM glances WHERE glances.id = feed_posts.glance_id), \
(SELECT wide FROM glances WHERE glances.id = feed_posts.glance_id)";

fn post(row: &Row) -> rusqlite::Result<Post> {
    let points: String = row.get(13)?;
    let note_id: Option<String> = row.get(15)?;
    let reminder_id: Option<String> = row.get(17)?;
    let title: String = row.get(5)?;
    let glance_id: Option<String> = row.get(22)?;
    let glance = match glance_id {
        Some(glance_id) => Some(crate::glances::Head {
            glance_id,
            title: title.clone(),
            as_of: row.get(24)?,
            icon: row.get(25)?,
            wide: row.get::<_, Option<bool>>(26)?.unwrap_or(false),
        }),
        None => None,
    };
    Ok(Post {
        id: row.get(0)?,
        topic_key: row.get(2)?,
        topic_label: row.get(3)?,
        kind: row.get(4)?,
        title,
        note: row.get(6)?,
        why: row.get(7)?,
        url: row.get(8)?,
        source_name: row.get(9)?,
        image_url: row.get(10)?,
        event_starts_at: row.get(11)?,
        summary: row.get(12)?,
        points: serde_json::from_str(&points).unwrap_or_default(),
        liked: row.get(14)?,
        saved: note_id.is_some(),
        hidden: row.get(16)?,
        reminded: reminder_id.is_some(),
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
        author: row.get(20)?,
        note_id,
        chat_id: row.get(21)?,
        glance,
        glance_choice: row.get(23)?,
    })
}

fn find(conn: &Connection, id: &str) -> rusqlite::Result<Option<Post>> {
    conn.prepare_cached(&format!("SELECT {COLUMNS} FROM feed_posts WHERE id = ?1"))?
        .query_row([id.trim()], post)
        .optional()
}

/// The post, if there is one.
pub fn get(db: &Db, id: &str) -> Result<Option<Post>, String> {
    db.with(|conn| find(conn, id))
}

/// The Feed (`feed:mine`): the newest posts not hidden, and those hidden lately (the Feed shows them as undo rows
/// until it's opened again), newest first.
pub fn list(db: &Db, now: i64) -> Result<Vec<Post>, String> {
    db.with(|conn| {
        let mut posts: Vec<Post> = conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM feed_posts WHERE hidden IS NULL ORDER BY created_at DESC, id LIMIT ?1"))?
            .query_map([FEED_PAGE as i64], post)?
            .collect::<rusqlite::Result<_>>()?;
        let hidden = conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM feed_posts WHERE hidden IS NOT NULL AND hidden_at >= ?1 ORDER BY created_at DESC LIMIT ?2"))?
            .query_map(params![now - HIDDEN_LISTED_MS, FEED_PAGE as i64], post)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let oldest = posts.last().map(|post| post.created_at).filter(|_| posts.len() >= FEED_PAGE).unwrap_or(i64::MIN);
        posts.extend(hidden.into_iter().filter(|post| post.created_at >= oldest));
        posts.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(posts)
    })
}

/// Posts whose title, note, summary, link, source or topic contain every word of `query` (empty for any), on the
/// topic when one is given, hidden ones included when asked; newest first, at most `limit`.
pub fn search(db: &Db, query: &str, topic: Option<&str>, with_hidden: bool, limit: usize) -> Result<Vec<Post>, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let topic = topic.map(topic_key).filter(|key| !key.is_empty());
    let all: Vec<Post> = db.with(|conn| {
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM feed_posts ORDER BY created_at DESC, id"))?
            .query_map([], post)?
            .collect()
    })?;
    Ok(all
        .into_iter()
        .filter(|post| with_hidden || post.hidden.is_none())
        .filter(|post| topic.as_ref().is_none_or(|key| &post.topic_key == key))
        .filter(|post| {
            let text = [
                &post.title,
                &post.note,
                post.summary.as_deref().unwrap_or_default(),
                &post.url,
                &post.source_name,
                &post.topic_label,
            ]
            .iter()
            .map(|text| text.to_lowercase())
            .collect::<Vec<_>>()
            .join("\n");
            words.iter().all(|word| text.contains(word))
        })
        .take(limit)
        .collect())
}

/// The topic a post goes under: a followed topic's label when the key matches one.
fn resolve_topic(conn: &Connection, topic: Option<&str>) -> rusqlite::Result<(String, String)> {
    let label = topic
        .map(|topic| line(topic, MAX_TOPIC_LABEL))
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| DEFAULT_TOPIC.into());
    let key = topic_key(&label);
    let followed: Option<String> =
        conn.query_row("SELECT label FROM feed_topics WHERE topic_key = ?1", [&key], |row| row.get(0)).optional()?;
    Ok((key, followed.unwrap_or(label)))
}

fn post_with_url(conn: &Connection, url: &str, except: Option<&str>) -> rusqlite::Result<Option<(String, String)>> {
    if url.is_empty() {
        return Ok(None);
    }
    conn.query_row(
        "SELECT id, title FROM feed_posts WHERE url = ?1 AND id != ?2",
        params![url, except.unwrap_or_default()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

fn already_there((id, title): (String, String)) -> String {
    format!(
        "That link is already in the Feed as post {id} (\u{201c}{title}\u{201d}). Change that post with its id instead."
    )
}

/// Adds a post to the Feed; the post as it was saved. Refused without a title, with a link that isn't one, or with a
/// link the Feed has already.
pub fn publish(db: &Db, new: &NewPost, origin: &Origin, now: i64) -> Result<Post, String> {
    let title = line(&new.title, MAX_TITLE);
    if title.is_empty() {
        return Err("A post needs a title.".into());
    }
    let url = clean_url(&new.url)?;
    let kind = clean_kind(new.kind.as_deref())?;
    let image_url = match new.image_url.as_deref().map(clean_url).transpose()? {
        Some(url) if !url.is_empty() => Some(url),
        _ => None,
    };
    if kind == "event" && new.event_starts_at.is_none() {
        return Err("An event needs when it starts (`event_starts_at`).".into());
    }
    let author = line(&origin.author, MAX_SOURCE);
    let author = if author.is_empty() { "Filo".to_string() } else { author };
    let source_name = new.source_name.as_deref().map(|name| line(name, MAX_SOURCE)).filter(|name| !name.is_empty());
    let source_name = source_name.or_else(|| host_of(&url)).unwrap_or_else(|| author.clone());
    let note = block(&new.note, MAX_NOTE);
    let summary = optional(new.summary.as_deref(), MAX_SUMMARY);
    let points = serde_json::to_string(&clean_points(&new.points)).unwrap_or_else(|_| "[]".into());
    let id = uuid::Uuid::new_v4().to_string();
    let saved = db.write(|tx| {
        if let Some(existing) = post_with_url(tx, &url, None)? {
            return Ok(Err(already_there(existing)));
        }
        let (key, label) = resolve_topic(tx, new.topic.as_deref())?;
        let why = optional(new.why.as_deref(), MAX_WHY).unwrap_or_else(|| format!("{author} shared this from a conversation."));
        tx.execute(
            "INSERT INTO feed_posts (id, topic_key, topic_label, kind, title, note, why, url, source_name, image_url, event_starts_at,
                summary, points, author, chat_id, day_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)",
            params![id, key, label, kind, title, note, why, url, source_name, image_url, new.event_starts_at, summary, points, author, origin.chat_id, day_key(now), now],
        )?;
        Ok(Ok(()))
    })?;
    saved?;
    get(db, &id)?.ok_or_else(|| "The post wasn't saved.".into())
}

/// Changes a post's fields (likes, saves and hides stay); the post as it is now, or None when there's no such post.
pub fn update(db: &Db, id: &str, change: &PostChange, now: i64) -> Result<Option<Post>, String> {
    let Some(mut post) = get(db, id)? else { return Ok(None) };
    if let Some(title) = &change.title {
        post.title = line(title, MAX_TITLE);
        if post.title.is_empty() {
            return Err("A post needs a title.".into());
        }
    }
    if let Some(url) = &change.url {
        post.url = clean_url(url)?;
    }
    if let Some(note) = &change.note {
        post.note = block(note, MAX_NOTE);
    }
    if let Some(why) = &change.why {
        let why = block(why, MAX_WHY);
        if !why.is_empty() {
            post.why = why;
        }
    }
    if let Some(kind) = &change.kind {
        if post.kind == GLANCE_KIND {
            return Err("This post is a glance: it stays one (change it with update_feed_glance).".into());
        }
        post.kind = clean_kind(Some(kind))?;
    }
    if let Some(name) = &change.source_name {
        let name = line(name, MAX_SOURCE);
        post.source_name =
            if name.is_empty() { host_of(&post.url).unwrap_or_else(|| post.author.clone()) } else { name };
    }
    if let Some(image) = &change.image_url {
        post.image_url = Some(clean_url(image)?).filter(|url| !url.is_empty());
    }
    if let Some(starts) = change.event_starts_at {
        post.event_starts_at = starts;
    }
    if post.kind == "event" && post.event_starts_at.is_none() {
        return Err("An event needs when it starts (`event_starts_at`).".into());
    }
    if let Some(summary) = &change.summary {
        post.summary = optional(Some(summary), MAX_SUMMARY);
    }
    if let Some(points) = &change.points {
        post.points = clean_points(points);
    }
    let points = serde_json::to_string(&post.points).unwrap_or_else(|_| "[]".into());
    let saved = db.write(|tx| {
        if let Some(existing) = post_with_url(tx, &post.url, Some(&post.id))? {
            return Ok(Err(already_there(existing)));
        }
        let (key, label) = match &change.topic {
            Some(topic) => resolve_topic(tx, Some(topic))?,
            None => (post.topic_key.clone(), post.topic_label.clone()),
        };
        tx.execute(
            "UPDATE feed_posts SET topic_key = ?2, topic_label = ?3, kind = ?4, title = ?5, note = ?6, why = ?7, url = ?8, source_name = ?9,
                image_url = ?10, event_starts_at = ?11, summary = ?12, points = ?13, updated_at = ?14
             WHERE id = ?1",
            params![
                post.id, key, label, post.kind, post.title, post.note, post.why, post.url, post.source_name, post.image_url, post.event_starts_at,
                post.summary, points, now
            ],
        )?;
        Ok(Ok(()))
    })?;
    saved?;
    get(db, &post.id)
}

/// Takes a post out of the Feed for good (a glance post's glance with it). False when there's no such post.
pub fn remove(db: &Db, id: &str) -> Result<bool, String> {
    Ok(db.write(|tx| {
        tx.execute("DELETE FROM glances WHERE id = (SELECT glance_id FROM feed_posts WHERE id = ?1)", [id.trim()])?;
        tx.execute("DELETE FROM feed_posts WHERE id = ?1", [id.trim()])
    })? > 0)
}

/// Where a glance post's glance says it is: the conversation it came from (the Feed's own when none), and a message id
/// no conversation has, so no chat shows its card.
fn glance_place(origin: &Origin, post_id: &str) -> (String, String) {
    (origin.chat_id.clone().unwrap_or_else(|| "feed".into()), format!("feed-post:{post_id}"))
}

/// Adds a glance post to the Feed: `glance` (checked already, glances.rs `checked`) with the post's note, topic and why
/// from `new` (its title is the glance's). The post as it was saved.
pub fn publish_glance(
    db: &Db,
    new: &NewPost,
    glance: &crate::glances::New,
    origin: &Origin,
    now: i64,
) -> Result<Post, String> {
    let title = line(&glance.title, MAX_TITLE);
    if title.is_empty() {
        return Err("A post needs a title.".into());
    }
    let author = line(&origin.author, MAX_SOURCE);
    let author = if author.is_empty() { "Filo".to_string() } else { author };
    let note = block(&new.note, MAX_NOTE);
    let id = uuid::Uuid::new_v4().to_string();
    let (chat, message) = glance_place(origin, &id);
    db.write(|tx| {
        let made = crate::glances::insert(tx, glance, &chat, &message, None, now)?;
        let (key, label) = resolve_topic(tx, new.topic.as_deref())?;
        let why = optional(new.why.as_deref(), MAX_WHY).unwrap_or_else(|| format!("{author} shared this from a conversation."));
        tx.execute(
            "INSERT INTO feed_posts (id, topic_key, topic_label, kind, title, note, why, url, source_name, author, chat_id, day_key, created_at,
                updated_at, glance_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', ?8, ?8, ?9, ?10, ?11, ?11, ?12)",
            params![id, key, label, GLANCE_KIND, title, note, why, author, origin.chat_id, day_key(now), now, made.id],
        )
    })?;
    get(db, &id)?.ok_or_else(|| "The post wasn't saved.".into())
}

/// The glance a glance post shows now (none for another post).
pub fn post_glance(db: &Db, id: &str) -> Result<Option<crate::glances::Glance>, String> {
    let glance_id: Option<String> = db
        .with(|conn| {
            conn.query_row("SELECT glance_id FROM feed_posts WHERE id = ?1", [id.trim()], |row| row.get(0)).optional()
        })?
        .flatten();
    match glance_id {
        Some(glance_id) => crate::glances::get(db, &glance_id),
        None => Ok(None),
    }
}

/// Changes a glance post in place: its note, topic and why (`change`), and its glance, made again from `glance` (checked
/// already) when given, which the post then shows instead of the one before. Likes, saves and hides stay, and so
/// does its place in the Feed. None when there's no such post; refused for a post that isn't a glance.
pub fn update_glance(
    db: &Db,
    id: &str,
    change: &PostChange,
    glance: Option<&crate::glances::New>,
    origin: &Origin,
    now: i64,
) -> Result<Option<Post>, String> {
    let Some(post) = get(db, id)? else { return Ok(None) };
    if post.kind != GLANCE_KIND {
        return Err("That post isn't a glance: change it with update_feed_post.".into());
    }
    let title = glance.map(|glance| line(&glance.title, MAX_TITLE));
    if title.as_deref().is_some_and(str::is_empty) {
        return Err("A post needs a title.".into());
    }
    let note = change.note.as_deref().map(|note| block(note, MAX_NOTE)).unwrap_or(post.note.clone());
    let why =
        change.why.as_deref().map(|why| block(why, MAX_WHY)).filter(|why| !why.is_empty()).unwrap_or(post.why.clone());
    let old = post.glance.as_ref().map(|glance| glance.glance_id.clone());
    db.write(|tx| {
        let (key, label) = match &change.topic {
            Some(topic) => resolve_topic(tx, Some(topic))?,
            None => (post.topic_key.clone(), post.topic_label.clone()),
        };
        let mut glance_id = old.clone();
        if let Some(glance) = glance {
            // Kept with the conversation the post came from, or the one changing it.
            let place = Origin { author: String::new(), chat_id: post.chat_id.clone().or_else(|| origin.chat_id.clone()) };
            let (chat, message) = glance_place(&place, &post.id);
            glance_id = Some(crate::glances::insert(tx, glance, &chat, &message, None, now)?.id);
            if let Some(old) = &old {
                tx.execute("DELETE FROM glances WHERE id = ?1", [old])?;
            }
        }
        tx.execute(
            "UPDATE feed_posts SET topic_key = ?2, topic_label = ?3, title = ?4, note = ?5, why = ?6, glance_id = ?7, updated_at = ?8 WHERE id = ?1",
            params![post.id, key, label, title.clone().unwrap_or(post.title.clone()), note, why, glance_id, now],
        )
    })?;
    get(db, &post.id)
}

/// What the user picked in a glance post's choices (none clears it). False when there's no such post.
pub fn set_glance_choice(db: &Db, id: &str, choice: Option<&str>) -> Result<bool, String> {
    let choice = choice.map(|choice| line(choice, MAX_CHOICE)).filter(|choice| !choice.is_empty());
    Ok(db
        .write(|tx| tx.execute("UPDATE feed_posts SET glance_choice = ?2 WHERE id = ?1", params![id.trim(), choice]))?
        > 0)
}

pub fn set_liked(db: &Db, id: &str, liked: bool) -> Result<bool, String> {
    Ok(db.write(|tx| tx.execute("UPDATE feed_posts SET liked = ?2 WHERE id = ?1", params![id, liked]))? > 0)
}

/// `less` or `not_interested` hides the post; `None` shows it again.
pub fn set_hidden(db: &Db, id: &str, reason: Option<&str>, now: i64) -> Result<bool, String> {
    if let Some(reason) = reason.filter(|reason| !["less", "not_interested"].contains(reason)) {
        return Err(format!("A post is hidden for less or not_interested, not {reason:?}."));
    }
    let at = reason.map(|_| now);
    Ok(db.write(|tx| {
        tx.execute("UPDATE feed_posts SET hidden = ?2, hidden_at = ?3 WHERE id = ?1", params![id, reason, at])
    })? > 0)
}

/// The note a saved post became (`None`: not saved).
pub fn set_note(db: &Db, id: &str, note_id: Option<&str>) -> Result<bool, String> {
    Ok(db.write(|tx| tx.execute("UPDATE feed_posts SET note_id = ?2 WHERE id = ?1", params![id, note_id]))? > 0)
}

pub fn set_reminder(db: &Db, id: &str, reminder_id: Option<&str>) -> Result<bool, String> {
    Ok(db.write(|tx| tx.execute("UPDATE feed_posts SET reminder_id = ?2 WHERE id = ?1", params![id, reminder_id]))? > 0)
}

// ---------------------------------------------------------------------------
// Topics
// ---------------------------------------------------------------------------

/// The columns the Feed has beside its migrations (db.rs runs this on every open): each topic's routine, who answers
/// Ask, and a glance post's glance. Added when missing, so a Filo from before them still opens the file.
pub fn ensure_columns(conn: &Connection) -> rusqlite::Result<()> {
    let has = |table: &str, column: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)"),
            [column],
            |row| row.get(0),
        )
    };
    if !has("feed_topics", "routine_id")? {
        conn.execute_batch("ALTER TABLE feed_topics ADD COLUMN routine_id TEXT")?;
    }
    if !has("feed_settings", "ask_agent_id")? {
        conn.execute_batch("ALTER TABLE feed_settings ADD COLUMN ask_agent_id TEXT")?;
    }
    // A glance post's glance (glances.rs) and the pick the user made in it.
    if !has("feed_posts", "glance_id")? {
        conn.execute_batch("ALTER TABLE feed_posts ADD COLUMN glance_id TEXT")?;
    }
    if !has("feed_posts", "glance_choice")? {
        conn.execute_batch("ALTER TABLE feed_posts ADD COLUMN glance_choice TEXT")?;
    }
    Ok(())
}

const TOPIC_COLUMNS: &str = "topic_key, label, source, routine_id, created_at";

fn topic_row(row: &Row) -> rusqlite::Result<Topic> {
    Ok(Topic {
        topic_key: row.get(0)?,
        label: row.get(1)?,
        source: row.get(2)?,
        routine_id: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// The topics followed, newest first.
pub fn topics(db: &Db) -> Result<Vec<Topic>, String> {
    db.with(|conn| {
        conn.prepare_cached(&format!(
            "SELECT {TOPIC_COLUMNS} FROM feed_topics ORDER BY created_at DESC, topic_key LIMIT ?1"
        ))?
        .query_map([TOPICS_LISTED as i64], topic_row)?
        .collect()
    })
}

pub fn topic(db: &Db, key: &str) -> Result<Option<Topic>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {TOPIC_COLUMNS} FROM feed_topics WHERE topic_key = ?1"), [key], topic_row)
            .optional()
    })
}

/// Follows a topic; its key. Refused when it's followed already or there are 20. Its routine is made by the caller
/// (`set_topic_routine`).
pub fn add_topic(db: &Db, label: &str, now: i64) -> Result<String, String> {
    let label = line(label, MAX_TOPIC_LABEL);
    if label.is_empty() {
        return Err("A topic needs a name.".into());
    }
    let key = topic_key(&label);
    let added = db.write(|tx| {
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM feed_topics", [], |row| row.get(0))?;
        let exists: bool =
            tx.query_row("SELECT EXISTS (SELECT 1 FROM feed_topics WHERE topic_key = ?1)", [&key], |row| row.get(0))?;
        if exists {
            return Ok(Err(format!("You already follow \u{201c}{label}\u{201d}.")));
        }
        if count as usize >= MAX_TOPICS {
            return Ok(Err(format!("You can follow up to {MAX_TOPICS} topics. Remove one first.")));
        }
        tx.execute(
            "INSERT INTO feed_topics (topic_key, label, created_at) VALUES (?1, ?2, ?3)",
            params![key, label, now],
        )?;
        // Posts already published under that name join the topic.
        tx.execute("UPDATE feed_posts SET topic_label = ?2 WHERE topic_key = ?1", params![key, label])?;
        Ok(Ok(()))
    })?;
    added.map(|_| key)
}

/// The routine that watches the topic (none: nobody does). False when it isn't followed.
pub fn set_topic_routine(db: &Db, key: &str, routine_id: Option<&str>) -> Result<bool, String> {
    Ok(db.write(|tx| {
        tx.execute("UPDATE feed_topics SET routine_id = ?2 WHERE topic_key = ?1", params![key, routine_id])
    })? > 0)
}

/// Renames a topic (its key stays, so its posts stay with it). False when it isn't followed.
pub fn rename_topic(db: &Db, key: &str, label: &str) -> Result<bool, String> {
    let label = line(label, MAX_TOPIC_LABEL);
    if label.is_empty() {
        return Err("A topic needs a name.".into());
    }
    db.write(|tx| {
        let changed = tx.execute("UPDATE feed_topics SET label = ?2 WHERE topic_key = ?1", params![key, label])?;
        if changed > 0 {
            tx.execute("UPDATE feed_posts SET topic_label = ?2 WHERE topic_key = ?1", params![key, label])?;
        }
        Ok(changed > 0)
    })
}

/// Stops following a topic; its posts stay. The topic it was (for its routine), none when it wasn't followed.
pub fn remove_topic(db: &Db, key: &str) -> Result<Option<Topic>, String> {
    db.write(|tx| {
        let topic = tx
            .query_row(&format!("SELECT {TOPIC_COLUMNS} FROM feed_topics WHERE topic_key = ?1"), [key], topic_row)
            .optional()?;
        tx.execute("DELETE FROM feed_topics WHERE topic_key = ?1", [key])?;
        Ok(topic)
    })
}

/// How many posts went under the topic from `from` to `to` (what one of its routine's runs found).
pub fn posts_between(db: &Db, key: &str, from: i64, to: i64) -> Result<i64, String> {
    db.with(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM feed_posts WHERE topic_key = ?1 AND created_at >= ?2 AND created_at <= ?3",
            params![key, from, to],
            |row| row.get(0),
        )
    })
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The Feed's settings.
pub fn settings(db: &Db) -> Result<Settings, String> {
    db.with(|conn| {
        conn.execute("INSERT OR IGNORE INTO feed_settings (id) VALUES (1)", [])?;
        conn.query_row(
            "SELECT ask_agent_id, pick_minute, interval_minutes FROM feed_settings WHERE id = 1",
            [],
            |row| Ok(Settings { ask_agent_id: row.get(0)?, pick_minute: row.get(1)?, interval_minutes: row.get(2)? }),
        )
    })
}

/// Who answers Ask on posts (none: the main agent).
pub fn set_ask_agent(db: &Db, agent_id: Option<&str>) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("INSERT OR IGNORE INTO feed_settings (id) VALUES (1)", [])?;
        tx.execute("UPDATE feed_settings SET ask_agent_id = ?1 WHERE id = 1", [agent_id]).map(|_| ())
    })
}

/// This Mac's time zone name, for the settings' `timezone`.
pub fn iana_timezone() -> String {
    std::env::var("TZ").ok().filter(|zone| !zone.is_empty()).unwrap_or_else(|| {
        std::fs::read_link("/etc/localtime")
            .ok()
            .and_then(|path| path.to_str().and_then(|path| path.split("zoneinfo/").nth(1)).map(str::to_string))
            .unwrap_or_else(|| "UTC".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::temp_path;

    fn new(title: &str, url: &str) -> NewPost {
        NewPost { title: title.into(), url: url.into(), note: "Worth a look.".into(), ..NewPost::default() }
    }

    fn agent() -> Origin {
        Origin { author: "Tutor".into(), chat_id: Some("chat-1".into()) }
    }

    #[test]
    fn posts_are_published_listed_and_tidied() {
        let db = Db::in_memory().unwrap();
        let now = now();
        add_topic(&db, "Jazz clubs in Lisbon", now).unwrap();
        let first = publish(
            &db,
            &NewPost {
                topic: Some("jazz CLUBS in lisbon".into()),
                points: vec!["  One ".into(), "".into(), "Two".into()],
                summary: Some("  A summary.  ".into()),
                ..new("  Hot\nClub  ", "https://www.hotclubportugal.pt/agenda")
            },
            &agent(),
            now,
        )
        .unwrap();
        assert_eq!(
            (first.title.as_str(), first.topic_key.as_str(), first.topic_label.as_str()),
            ("Hot Club", "jazz clubs in lisbon", "Jazz clubs in Lisbon")
        );
        assert_eq!(
            (first.source_name.as_str(), first.kind.as_str(), first.author.as_str()),
            ("hotclubportugal.pt", "article", "Tutor")
        );
        assert_eq!(first.points, ["One", "Two"]);
        assert_eq!(first.summary.as_deref(), Some("A summary."));
        assert_eq!(first.why, "Tutor shared this from a conversation.");
        assert_eq!(first.chat_id.as_deref(), Some("chat-1"));

        // No topic: the agents' own; no link: the author is the source.
        let second = publish(&db, &new("A thought", ""), &agent(), now + 1).unwrap();
        assert_eq!(
            (second.topic_label.as_str(), second.source_name.as_str(), second.url.as_str()),
            (DEFAULT_TOPIC, "Tutor", "")
        );
        let ids: Vec<String> = list(&db, now).unwrap().into_iter().map(|post| post.id).collect();
        assert_eq!(ids, [second.id.clone(), first.id.clone()]);

        // The same link again is refused with the post that has it; bad input is refused.
        let again =
            publish(&db, &new("Hot Club again", "https://www.hotclubportugal.pt/agenda"), &agent(), now).unwrap_err();
        assert!(again.contains(&first.id), "{again}");
        assert!(publish(&db, &new("  ", "https://a.example"), &agent(), now).is_err());
        assert!(publish(&db, &new("Bad", "ftp://x"), &agent(), now).is_err());
        assert!(publish(&db, &NewPost { kind: Some("podcast".into()), ..new("Bad kind", "") }, &agent(), now).is_err());
        assert!(publish(&db, &NewPost { kind: Some("event".into()), ..new("No time", "") }, &agent(), now).is_err());
        // Two posts without links are fine.
        publish(&db, &new("Another thought", ""), &agent(), now).unwrap();
    }

    #[test]
    fn posts_are_updated_without_losing_the_users_marks() {
        let db = Db::in_memory().unwrap();
        let now = now();
        let post = publish(&db, &new("Draft", "https://example.com/a"), &agent(), now).unwrap();
        set_liked(&db, &post.id, true).unwrap();
        set_note(&db, &post.id, Some("note-1")).unwrap();
        let change = PostChange {
            title: Some("Final".into()),
            summary: Some("Read it.".into()),
            points: Some(vec!["p".into()]),
            kind: Some("event".into()),
            event_starts_at: Some(Some(now + 3_600_000)),
            image_url: Some("https://example.com/cover.jpg".into()),
            ..PostChange::default()
        };
        let updated = update(&db, &post.id, &change, now + 5).unwrap().unwrap();
        assert_eq!(
            (updated.title.as_str(), updated.kind.as_str(), updated.summary.as_deref()),
            ("Final", "event", Some("Read it."))
        );
        assert!(updated.liked && updated.saved);
        assert_eq!(updated.image_url.as_deref(), Some("https://example.com/cover.jpg"));
        // Clearing the time of an event, or taking another post's link, is refused; empty texts clear.
        assert!(
            update(&db, &post.id, &PostChange { event_starts_at: Some(None), ..PostChange::default() }, now).is_err()
        );
        let other = publish(&db, &new("Other", "https://example.com/b"), &agent(), now).unwrap();
        assert!(
            update(
                &db,
                &other.id,
                &PostChange { url: Some("https://example.com/a".into()), ..PostChange::default() },
                now
            )
            .is_err()
        );
        let cleared = update(
            &db,
            &post.id,
            &PostChange { summary: Some("".into()), image_url: Some("".into()), ..PostChange::default() },
            now,
        )
        .unwrap()
        .unwrap();
        assert_eq!((cleared.summary, cleared.image_url), (None, None));
        assert_eq!(update(&db, "nope", &PostChange::default(), now).unwrap(), None);
        assert!(remove(&db, &other.id).unwrap());
        assert!(!remove(&db, &other.id).unwrap());
    }

    #[test]
    fn hidden_posts_stay_listed_for_a_while_for_undo() {
        let db = Db::in_memory().unwrap();
        let now = now();
        let post = publish(&db, &new("Meh", "https://example.com/meh"), &agent(), now).unwrap();
        assert!(set_hidden(&db, &post.id, Some("less"), now).unwrap());
        assert_eq!(list(&db, now).unwrap()[0].hidden.as_deref(), Some("less"));
        assert!(list(&db, now + HIDDEN_LISTED_MS + 1).unwrap().is_empty());
        assert!(set_hidden(&db, &post.id, Some("whatever"), now).is_err());
        set_hidden(&db, &post.id, None, now).unwrap();
        assert_eq!(list(&db, now + HIDDEN_LISTED_MS + 1).unwrap().len(), 1);
        assert_eq!(search(&db, "meh", None, false, 10).unwrap().len(), 1);
        assert!(search(&db, "meh other", None, false, 10).unwrap().is_empty());
    }

    #[test]
    fn topics_are_followed_renamed_and_removed() {
        let db = Db::in_memory().unwrap();
        let now = now();
        let key = add_topic(&db, "  Rust   GUIs ", now).unwrap();
        assert_eq!(key, "rust guis");
        assert!(add_topic(&db, "rust guis", now).unwrap_err().contains("already follow"));
        assert!(add_topic(&db, " ", now).is_err());
        let post =
            publish(&db, &NewPost { topic: Some("Rust GUIs".into()), ..new("GPUI", "https://gpui.rs") }, &agent(), now)
                .unwrap();
        assert!(rename_topic(&db, &key, "Rust desktop apps").unwrap());
        assert_eq!(get(&db, &post.id).unwrap().unwrap().topic_label, "Rust desktop apps");
        assert_eq!(topics(&db).unwrap()[0].label, "Rust desktop apps");
        assert!(!rename_topic(&db, "nope", "x").unwrap());
        for index in 1..MAX_TOPICS {
            add_topic(&db, &format!("topic {index}"), now + index as i64).unwrap();
        }
        assert!(add_topic(&db, "one too many", now).unwrap_err().contains("up to 20"));
        assert!(remove_topic(&db, &key).unwrap().is_some());
        assert!(remove_topic(&db, &key).unwrap().is_none());
        // Its posts stay.
        assert!(get(&db, &post.id).unwrap().is_some());
    }

    #[test]
    fn each_topic_keeps_its_routine_and_the_settings_who_answers_ask() {
        let db = Db::in_memory().unwrap();
        let now = now();
        let key = add_topic(&db, "Lisbon", now).unwrap();
        assert_eq!(topic(&db, &key).unwrap().unwrap().routine_id, None);
        assert!(set_topic_routine(&db, &key, Some("routine-1")).unwrap());
        assert!(!set_topic_routine(&db, "paris", Some("routine-2")).unwrap());
        assert_eq!(topics(&db).unwrap()[0].routine_id.as_deref(), Some("routine-1"));
        publish(
            &db,
            &NewPost { topic: Some("Lisbon".into()), ..new("Fado night", "https://example.com/fado") },
            &agent(),
            now + 5,
        )
        .unwrap();
        assert_eq!(
            (posts_between(&db, &key, now, now + 10).unwrap(), posts_between(&db, &key, now + 6, now + 10).unwrap()),
            (1, 0)
        );
        assert_eq!(remove_topic(&db, &key).unwrap().and_then(|topic| topic.routine_id).as_deref(), Some("routine-1"));
        assert_eq!(remove_topic(&db, &key).unwrap(), None);

        let fresh = settings(&db).unwrap();
        assert_eq!((fresh.ask_agent_id, fresh.pick_minute, fresh.interval_minutes), (None, DEFAULT_PICK_MINUTE, None));
        set_ask_agent(&db, Some("chef")).unwrap();
        assert_eq!(settings(&db).unwrap().ask_agent_id.as_deref(), Some("chef"));
        set_ask_agent(&db, None).unwrap();
        assert_eq!(settings(&db).unwrap().ask_agent_id, None);
        // Run again on a file that has the columns, nothing changes.
        db.with(ensure_columns).unwrap();
    }

    #[test]
    fn the_feed_survives_a_restart() {
        let path = temp_path("feed");
        let now = now();
        let id = {
            let db = Db::open(&path).unwrap();
            let key = add_topic(&db, "Kept", now).unwrap();
            set_topic_routine(&db, &key, Some("r")).unwrap();
            publish(&db, &new("Still here", "https://example.com/kept"), &agent(), now).unwrap().id
        };
        let db = Db::open(&path).unwrap();
        assert_eq!(get(&db, &id).unwrap().unwrap().title, "Still here");
        assert_eq!(
            (topics(&db).unwrap()[0].label.as_str(), topics(&db).unwrap()[0].routine_id.as_deref()),
            ("Kept", Some("r"))
        );
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    fn glance(title: &str, html: &str) -> crate::glances::New {
        crate::glances::checked(&crate::glances::New {
            title: title.into(),
            html: html.into(),
            text: "Mon: 3 h".into(),
            icon: Some("chart-line".into()),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn a_glance_post_is_published_changed_in_place_and_removed_with_its_glance() {
        let db = Db::in_memory().unwrap();
        let now = now();
        let older = publish(&db, &new("A link", "https://example.com/a"), &agent(), now - 10).unwrap();
        let post = publish_glance(
            &db,
            &NewPost { note: "Meetings won.".into(), ..NewPost::default() },
            &glance("Your focus", "<div>v1</div>"),
            &agent(),
            now,
        )
        .unwrap();
        assert_eq!(
            (post.kind.as_str(), post.title.as_str(), post.url.as_str(), post.source_name.as_str()),
            (GLANCE_KIND, "Your focus", "", "Tutor")
        );
        let head = post.glance.clone().unwrap();
        assert_eq!((head.title.as_str(), head.icon.as_deref()), ("Your focus", Some("chart-line")));
        let first = crate::glances::get(&db, &head.glance_id).unwrap().unwrap();
        assert_eq!((first.chat_id.as_str(), first.message_id), ("chat-1", format!("feed-post:{}", post.id)));
        // The chat it came from shows no card for it: no message there has its id.
        assert!(crate::glances::heads(&db, "chat-1").unwrap().keys().all(|message| message.starts_with("feed-post:")));
        assert_eq!(
            list(&db, now).unwrap().iter().map(|post| post.id.clone()).collect::<Vec<_>>(),
            [post.id.clone(), older.id.clone()]
        );
        // A link post has no glance, and can't become one or stop being one.
        assert!(list(&db, now).unwrap()[1].glance.is_none());
        assert!(publish(&db, &NewPost { kind: Some(GLANCE_KIND.into()), ..new("Sneaky", "") }, &agent(), now).is_err());
        assert!(
            update(&db, &post.id, &PostChange { kind: Some("article".into()), ..PostChange::default() }, now).is_err()
        );
        assert!(update_glance(&db, &older.id, &PostChange::default(), None, &agent(), now).is_err());

        set_liked(&db, &post.id, true).unwrap();
        assert!(set_glance_choice(&db, &post.id, Some("  Ship   tonight ")).unwrap());
        let changed = update_glance(
            &db,
            &post.id,
            &PostChange { note: Some("Now with Friday.".into()), ..PostChange::default() },
            Some(&glance("Your focus this week", "<div>v2</div>")),
            &agent(),
            now + 5,
        )
        .unwrap()
        .unwrap();
        let second = changed.glance.clone().unwrap();
        assert_ne!(second.glance_id, head.glance_id);
        assert_eq!(
            (changed.title.as_str(), changed.note.as_str(), changed.liked, changed.glance_choice.as_deref()),
            ("Your focus this week", "Now with Friday.", true, Some("Ship tonight"))
        );
        assert_eq!(post_glance(&db, &post.id).unwrap().unwrap().html, "<div>v2</div>");
        assert!(crate::glances::get(&db, &head.glance_id).unwrap().is_none(), "the glance before goes");
        // Still where it was in the Feed, not a copy at the top.
        let listed = list(&db, now + 5).unwrap();
        assert_eq!((listed.len(), listed[0].id.as_str(), listed[0].created_at), (2, post.id.as_str(), post.created_at));
        // Only the note changes: the glance stays.
        let noted = update_glance(
            &db,
            &post.id,
            &PostChange { note: Some("Again.".into()), ..PostChange::default() },
            None,
            &agent(),
            now + 6,
        )
        .unwrap()
        .unwrap();
        assert_eq!(noted.glance.unwrap().glance_id, second.glance_id);
        assert!(remove(&db, &post.id).unwrap());
        assert!(crate::glances::get(&db, &second.glance_id).unwrap().is_none());
        assert_eq!(update_glance(&db, &post.id, &PostChange::default(), None, &agent(), now).unwrap(), None);
    }

    #[test]
    fn links_and_hosts_are_read_carefully() {
        assert_eq!(clean_url(" https://a.example/x ").unwrap(), "https://a.example/x");
        assert_eq!(clean_url("").unwrap(), "");
        for bad in ["javascript:alert(1)", "https://", "http:///x", "https://a b", "file:///etc/passwd", "a.example"] {
            assert!(clean_url(bad).is_err(), "{bad}");
        }
        assert_eq!(host_of("https://user@www.Example.com:8080/a?b").as_deref(), Some("example.com"));
        assert_eq!(host_of("nope"), None);
    }
}

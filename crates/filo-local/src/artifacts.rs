// Artifacts in Filo's database (db.rs) and beside it on disk: the reports, pages, diagrams and small tools agents make
// for the user, the Artifacts tab and an artifact's page (`canvasArtifacts:*`), and the MCP tools that publish and open
// them (artifacts_mcp.rs).
//
//   <root>/artifacts/<id>/v<n>.html   each version's page, written once and never changed
//   <root>/artifacts/<id>/index.html  a copy of the current version, for the folder the page's Show in Finder opens
//   <root>/artifacts/<id>/claude/index.html  while "Share to claude.ai" is on, the copy that goes there (artifact_share.rs)
//   <root>/artifacts/<id>/images/            the web pictures its pages name, saved before each version (artifact_images.rs)
//
// The database has the rest: the artifact (title, summary, kind, who made it and in which conversation, the current
// version, the file it's published from), its versions (the file, what changed) and its chat links (each time a
// conversation's agent created, changed or opened it), which the conversation shows as cards. A version's file is
// written before its row, in a write transaction taken up front (`BEGIN IMMEDIATE`), so the app and an MCP server
// saving at once each get their own version number and file. Restoring an old version saves its page again as a new
// version: nothing is lost.
//
// Agents work file first, like Claude's own Artifact tool: they build a page as an HTML file (the artifacts skill's
// template), edit it with their file tools and publish it (`publish`). The artifact keeps that file's path (`source`),
// so publishing the same file again saves its next version, and a page the same as the current version saves none.
// Opening one to change it gives that file (`page_file`), or makes one for an artifact that has none yet.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::db::Db;
use crate::store::{cut, valid_id};

/// The tables (db.rs `MIGRATIONS`). `IF NOT EXISTS`: `ensure` makes them too, so a database whose migration count was
/// taken by another page's tables first still gets them.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS artifacts (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT NOT NULL,
    summary         TEXT NOT NULL DEFAULT '',
    kind            TEXT NOT NULL DEFAULT 'page',
    current_version INTEGER NOT NULL,
    author          TEXT NOT NULL DEFAULT '',
    chat_id         TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    opened_at       INTEGER
);
CREATE INDEX IF NOT EXISTS artifacts_by_update ON artifacts (updated_at DESC);
CREATE TABLE IF NOT EXISTS artifact_versions (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    file        TEXT NOT NULL,
    bytes       INTEGER NOT NULL,
    note        TEXT NOT NULL DEFAULT '',
    author      TEXT NOT NULL DEFAULT '',
    chat_id     TEXT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, version)
);
CREATE TABLE IF NOT EXISTS artifact_links (
    id          TEXT PRIMARY KEY NOT NULL,
    artifact_id TEXT NOT NULL REFERENCES artifacts (id) ON DELETE CASCADE,
    chat_id     TEXT NOT NULL,
    turn_id     TEXT,
    action      TEXT NOT NULL,
    version     INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS artifact_links_by_chat ON artifact_links (chat_id, created_at);
";

/// The folder beside the database.
const DIR: &str = "artifacts";
/// What an artifact is; the page shows them all the same way.
pub const KINDS: &[&str] = &["report", "page", "diagram", "tool"];
const MAX_TITLE: usize = 120;
const MAX_SUMMARY: usize = 600;
const MAX_NOTE: usize = 300;
/// One version's page at most this big (a runaway tool call shouldn't fill the disk).
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
/// `canvasArtifacts:list` and the MCP's list show at most this many.
const LIST_LIMIT: usize = 100;
/// Where an artifact that has no file yet gets one to edit (`page_file`), in the turn's working folder.
pub const PAGES_DIR: &str = "pages";
/// Where a page's theme goes: the app puts its colours and font in there (src/screens/chat/glance.rs `themed_page`),
/// and so does the copy for claude.ai (artifact_share.rs).
const THEME_MARKER: &str = "<!--filo:head-->";

/// Makes the tables when the migrations didn't (see `SCHEMA`), with the columns added since.
pub fn ensure(db: &Db) -> Result<(), String> {
    db.with(|conn| {
        conn.execute_batch(SCHEMA)?;
        ensure_columns(conn)
    })
}

/// The file an artifact is published from (`source`), added beside the migrations (db.rs `ready`), so a Filo that
/// doesn't know it still opens the database. Only once the table is there (`ensure` makes it otherwise).
pub fn ensure_columns(conn: &Connection) -> rusqlite::Result<()> {
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('artifacts')")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if columns.is_empty() {
        return Ok(());
    }
    if !columns.iter().any(|name| name == "source") {
        conn.execute_batch("ALTER TABLE artifacts ADD COLUMN source TEXT")?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS artifacts_by_source ON artifacts (source)")
}

/// The artifacts folder for the database at `db_path`.
pub fn folder_for(db_path: &Path) -> PathBuf {
    db_path.parent().unwrap_or(Path::new(".")).join(DIR)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub kind: String,
    pub current_version: i64,
    /// Who made it: the agent's name, or Filo's.
    pub author: String,
    /// The conversation it was made in.
    pub chat_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub opened_at: Option<i64>,
    /// The HTML file on this Mac it's published from (`publish`), as `source_key` gives it: publishing it again saves
    /// the next version. None for one made before, or by the app.
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub version: i64,
    /// The page's file in the artifact's folder.
    pub file: String,
    pub bytes: i64,
    /// What changed, as the agent said (publish_artifact's `label`).
    pub note: String,
    pub author: String,
    pub created_at: i64,
}

/// Who is saving, and from where: the agent and its conversation and turn (none from the app's own pages).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Origin {
    pub author: String,
    pub chat_id: Option<String>,
    pub turn_id: Option<String>,
}

/// A new artifact.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct New {
    pub title: String,
    pub html: String,
    pub summary: String,
    pub kind: Option<String>,
}

/// A change to an artifact: a new page (`html`) makes a new version; the rest only changes what it's called and how
/// it's described.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub html: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub kind: Option<String>,
    pub note: Option<String>,
}

/// A page published from its file (publish_artifact): where the file is, its page, and what to call it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Publish {
    /// The file, as `source_key` gives it.
    pub source: String,
    pub html: String,
    /// What to call it; a new one is called by the page's `<title>` (else its file's name) without one.
    pub title: Option<String>,
    pub summary: Option<String>,
    pub kind: Option<String>,
    /// A few words on this version: what changed, or a tag like "first draft" (its note).
    pub label: Option<String>,
}

/// What a publish did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Published {
    /// A new artifact, at version 1.
    Created,
    /// The next version of the artifact the file was published as before.
    Saved,
    /// The page is the current version's already: no new version (its title, summary or kind may have changed).
    Unchanged,
}

/// The file `page_file` gives to read and edit.
#[derive(Clone, Debug, PartialEq)]
pub struct PageFile {
    pub path: PathBuf,
    /// It holds the current version's page; otherwise it has changes not published yet.
    pub current: bool,
}

/// An artifact as a conversation shows it: one card per artifact and turn, at its latest change in that turn.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatCard {
    /// The first link's id: stays the same as the turn changes the artifact again.
    pub id: String,
    pub artifact_id: String,
    pub title: String,
    /// What the card says: the version's note, else the artifact's summary.
    pub text: String,
    /// The first thing the turn did with it: "created", "updated" or "opened".
    pub action: String,
    pub version: i64,
    pub first_at: i64,
    pub at: i64,
}

const COLUMNS: &str =
    "id, title, summary, kind, current_version, author, chat_id, created_at, updated_at, opened_at, source";

fn artifact(row: &Row) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: row.get(0)?,
        title: row.get(1)?,
        summary: row.get(2)?,
        kind: row.get(3)?,
        current_version: row.get(4)?,
        author: row.get(5)?,
        chat_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        opened_at: row.get(9)?,
        source: row.get(10)?,
    })
}

fn version_row(row: &Row) -> rusqlite::Result<Version> {
    Ok(Version {
        version: row.get(0)?,
        file: row.get(1)?,
        bytes: row.get(2)?,
        note: row.get(3)?,
        author: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn clean_title(title: &str) -> String {
    cut(title, MAX_TITLE)
}

fn clean_kind(kind: Option<&str>) -> Result<Option<String>, String> {
    match kind.map(str::trim).filter(|kind| !kind.is_empty()) {
        None => Ok(None),
        Some(kind) => {
            let kind = kind.to_lowercase();
            if KINDS.contains(&kind.as_str()) {
                Ok(Some(kind))
            } else {
                Err(format!("`kind` is one of {}.", KINDS.join(", ")))
            }
        }
    }
}

fn check_html(html: &str) -> Result<(), String> {
    if html.trim().is_empty() {
        return Err("The page is empty: its file needs the whole HTML document.".into());
    }
    if html.len() > MAX_HTML_BYTES {
        return Err(format!(
            "The page is too big ({} KB, at most {} KB). Make it smaller.",
            html.len() / 1024,
            MAX_HTML_BYTES / 1024
        ));
    }
    Ok(())
}

/// The page in an HTML file on this Mac, held to the size limit before it's read.
pub fn read_page_file(path: &Path) -> Result<String, String> {
    let shown = path.display();
    let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or_default();
    if !extension.eq_ignore_ascii_case("html") && !extension.eq_ignore_ascii_case("htm") {
        return Err(format!("{shown} isn't an HTML file (.html or .htm)."));
    }
    let metadata = std::fs::metadata(path).map_err(|_| format!("There's no file at {shown}."))?;
    if !metadata.is_file() {
        return Err(format!("{shown} isn't a file."));
    }
    if metadata.len() > MAX_HTML_BYTES as u64 {
        return Err(format!(
            "{shown} is too big ({} KB, at most {} KB). Make it smaller.",
            metadata.len() / 1024,
            MAX_HTML_BYTES / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("Couldn't read {shown}: {error}"))?;
    let page = String::from_utf8(bytes).map_err(|_| format!("{shown} isn't text (UTF-8)."))?;
    if page.trim().is_empty() {
        return Err(format!("{shown} is empty."));
    }
    check_html(&page)?;
    Ok(page)
}

/// How a file is known as an artifact's `source`: its full path with links followed (`/tmp` is `/private/tmp`), so the
/// same file always finds the same artifact.
pub fn source_key(path: &Path) -> String {
    let real = path.canonicalize().ok().or_else(|| Some(path.parent()?.canonicalize().ok()?.join(path.file_name()?)));
    real.unwrap_or_else(|| path.to_path_buf()).display().to_string()
}

/// The page without its comments, whose words aren't the page's (the template's has its own how-to).
fn without_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = rest[start..].find("-->").map_or("", |end| &rest[start + end + 3..]);
    }
    out.push_str(rest);
    out
}

/// The page's own `<title>`, when it has one.
fn page_title(html: &str) -> Option<String> {
    let html = without_comments(html);
    // ASCII lowercasing keeps every byte where it was.
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open = start + lower[start..].find('>')? + 1;
    let end = open + lower[open..].find("</title")?;
    let text = html[open..end]
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&mdash;", "\u{2014}")
        .replace("&amp;", "&");
    Some(clean_title(&text)).filter(|title| !title.is_empty())
}

/// A title from a file's name: `meal-plan.html` is "Meal plan".
fn file_title(source: &str) -> Option<String> {
    let stem = Path::new(source).file_stem()?.to_str()?.replace(['-', '_'], " ");
    let stem = clean_title(&stem);
    let mut chars = stem.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// What may keep a page from showing as written: files it loads from the internet, which its sandbox blocks (the
/// preview has no network), browser storage, which the sandbox doesn't have (a frame with no origin: using it throws),
/// and no theme marker, so it doesn't follow the app's colours and font. For the agent that published it to fix, if it
/// wants; nothing is refused for them.
pub fn page_warnings(page: &str) -> Vec<String> {
    let html = without_comments(page);
    let html = html.as_str();
    let lower = html.to_ascii_lowercase();
    let mut remote: Vec<String> = Vec::new();
    let mut note = |at: usize| {
        let link: String = html[at..].chars().take_while(|char| !"\"' )>\n".contains(*char)).take(80).collect();
        if !link.is_empty() && !remote.contains(&link) {
            remote.push(link);
        }
    };
    for pattern in ["src=\"", "src='", "url(\"", "url('", "url(", "@import \"", "@import '"] {
        let mut from = 0;
        while let Some(found) = lower[from..].find(pattern) {
            let at = from + found + pattern.len();
            if lower[at..].starts_with("http:") || lower[at..].starts_with("https:") || lower[at..].starts_with("//") {
                note(at);
            }
            from = at;
        }
    }
    // Stylesheets and icons from elsewhere (`<link href>`); a page's own `<a href>` opens in the browser, which is fine.
    let mut from = 0;
    while let Some(found) = lower[from..].find("<link") {
        let start = from + found;
        let end = lower[start..].find('>').map_or(lower.len(), |end| start + end);
        for pattern in ["href=\"", "href='"] {
            if let Some(found) = lower[start..end].find(pattern) {
                let at = start + found + pattern.len();
                if lower[at..].starts_with("http:")
                    || lower[at..].starts_with("https:")
                    || lower[at..].starts_with("//")
                {
                    note(at);
                }
            }
        }
        from = end;
    }
    let mut warnings = Vec::new();
    if !remote.is_empty() {
        let shown = remote.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        warnings.push(format!(
            "It loads {} from the internet ({shown}{}), which its sandbox blocks: put it in the page (inline <style> and <script>, data: URLs, inline SVG).",
            if remote.len() == 1 { "a file".to_string() } else { format!("{} files", remote.len()) },
            if remote.len() > 3 { ", …" } else { "" }
        ));
    }
    let storage: Vec<&str> = ["localStorage", "sessionStorage", "indexedDB", "document.cookie"]
        .into_iter()
        .filter(|name| html.contains(name))
        .collect();
    if !storage.is_empty() {
        warnings.push(format!(
            "It uses {}, which its sandbox doesn't have (using it throws there): keep its state in memory.",
            storage.join(", ")
        ));
    }
    if !page.contains(THEME_MARKER) {
        warnings.push(format!(
            "It has no {THEME_MARKER} in its <head>, so it keeps its own look rather than the app's theme and font."
        ));
    }
    warnings
}

/// The artifact's own folder.
pub fn artifact_dir(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err(format!("Not an artifact id: {id:?}"));
    }
    Ok(dir.join(id))
}

/// Writes through a temporary file and a rename, so a reader never sees half a page.
fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    crate::files::write_atomic(path, contents.as_bytes())
        .map_err(|error| format!("Couldn't save {}: {error}", path.display()))
}

/// A version's page; the file's name is the database's (only ever `v<n>.html`).
fn read_page(dir: &Path, id: &str, file: &str) -> Result<String, String> {
    if file.contains('/') || file.contains("..") {
        return Err(format!("Not a version file: {file:?}"));
    }
    let path = artifact_dir(dir, id)?.join(file);
    std::fs::read_to_string(&path).map_err(|error| format!("Couldn't read {}: {error}", path.display()))
}

/// Runs `work` in a write transaction taken at once; it's committed only when `work` succeeds (a database error, or
/// one of `work`'s own).
fn immediate<T>(db: &Db, work: impl FnOnce(&Transaction) -> rusqlite::Result<Result<T, String>>) -> Result<T, String> {
    db.with(|conn| {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        match work(&tx)? {
            Ok(value) => {
                tx.commit()?;
                Ok(Ok(value))
            }
            // Dropping the transaction rolls it back.
            Err(error) => Ok(Err(error)),
        }
    })?
}

fn add_link(
    conn: &Connection,
    id: &str,
    origin: &Origin,
    action: &str,
    version: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let Some(chat_id) = origin.chat_id.as_deref() else { return Ok(()) };
    conn.execute(
        "INSERT INTO artifact_links (id, artifact_id, chat_id, turn_id, action, version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![uuid::Uuid::new_v4().to_string(), id, chat_id, origin.turn_id, action, version, now],
    )?;
    Ok(())
}

fn version_file(version: i64) -> String {
    format!("v{version}.html")
}

/// Saves a new artifact `id` as version 1 in `tx`: its page's file, then its rows, linked to the conversation it came
/// from. The caller removes its folder when it isn't saved.
fn insert_new(
    tx: &Transaction,
    dir: &Path,
    id: &str,
    new: &New,
    source: Option<&str>,
    note: &str,
    origin: &Origin,
    now: i64,
) -> rusqlite::Result<Result<(), String>> {
    let title = clean_title(&new.title);
    if title.is_empty() {
        return Ok(Err("Give the artifact a `title`.".into()));
    }
    if let Err(error) = check_html(&new.html) {
        return Ok(Err(error));
    }
    let kind = match clean_kind(new.kind.as_deref()) {
        Ok(kind) => kind.unwrap_or_else(|| "page".into()),
        Err(error) => return Ok(Err(error)),
    };
    let summary = cut(&new.summary, MAX_SUMMARY);
    let folder = match artifact_dir(dir, id) {
        Ok(folder) => folder,
        Err(error) => return Ok(Err(error)),
    };
    let file = version_file(1);
    if let Err(error) = write_file(&folder.join(&file), &new.html) {
        return Ok(Err(error));
    }
    tx.execute(
        &format!("INSERT INTO artifacts ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?7, NULL, ?8)"),
        params![id, title, summary, kind, origin.author, origin.chat_id, now, source],
    )?;
    tx.execute(
        "INSERT INTO artifact_versions (artifact_id, version, file, bytes, note, author, chat_id, created_at) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, file, new.html.len() as i64, note, origin.author, origin.chat_id, now],
    )?;
    add_link(tx, id, origin, "created", 1, now)?;
    Ok(Ok(()))
}

/// After a new artifact's transaction: its copy of the current page, or its folder gone when it wasn't saved.
fn finish_new(db: &Db, dir: &Path, id: &str, saved: Result<(), String>, html: &str) -> Result<Artifact, String> {
    let folder = artifact_dir(dir, id)?;
    if let Err(error) = saved {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(error);
    }
    let _ = write_file(&folder.join("index.html"), html);
    crate::artifact_share::page_changed(dir, id);
    get(db, id)?.ok_or_else(|| "The artifact wasn't saved.".into())
}

/// Downloads the web pictures a page about to be saved for artifact `id` names into its folder
/// (artifact_images.rs), when it's a page that will be.
fn save_images(dir: &Path, id: &str, html: &str) {
    if let (Ok(folder), Ok(())) = (artifact_dir(dir, id), check_html(html)) {
        crate::artifact_images::save(&folder, html);
    }
}

/// Saves a new artifact as version 1, linked to the conversation it came from.
pub fn create(db: &Db, dir: &Path, new: &New, origin: &Origin, now: i64) -> Result<Artifact, String> {
    let id = uuid::Uuid::new_v4().to_string();
    save_images(dir, &id, &new.html);
    let saved = immediate(db, |tx| insert_new(tx, dir, &id, new, None, "", origin, now));
    finish_new(db, dir, &id, saved, &new.html)
}

/// Makes `change` to artifact `id` (there) in `tx`: a new page is the next version, written to its file, which
/// `written` then names with the page. The write lock is held, so the version number is this save's alone.
fn save_change(
    tx: &Transaction,
    dir: &Path,
    id: &str,
    change: &Change,
    origin: &Origin,
    now: i64,
    written: &mut Option<(PathBuf, String)>,
) -> rusqlite::Result<Result<(), String>> {
    let title = change.title.as_deref().map(clean_title);
    if title.as_deref() == Some("") {
        return Ok(Err("The title can't be empty.".into()));
    }
    let kind = match clean_kind(change.kind.as_deref()) {
        Ok(kind) => kind,
        Err(error) => return Ok(Err(error)),
    };
    let summary = change.summary.as_deref().map(|summary| cut(summary, MAX_SUMMARY));
    let note = change.note.as_deref().map(|note| cut(note, MAX_NOTE)).unwrap_or_default();
    let mut version: i64 =
        tx.query_row("SELECT current_version FROM artifacts WHERE id = ?1", [id], |row| row.get(0))?;
    if let Some(page) = &change.html {
        if let Err(error) = check_html(page) {
            return Ok(Err(error));
        }
        let latest: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM artifact_versions WHERE artifact_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        version = latest + 1;
        let file = version_file(version);
        let path = match artifact_dir(dir, id) {
            Ok(folder) => folder.join(&file),
            Err(error) => return Ok(Err(error)),
        };
        if let Err(error) = write_file(&path, page) {
            return Ok(Err(error));
        }
        *written = Some((path, page.clone()));
        tx.execute(
            "INSERT INTO artifact_versions (artifact_id, version, file, bytes, note, author, chat_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, version, file, page.len() as i64, note, origin.author, origin.chat_id, now],
        )?;
    } else if title.is_none() && summary.is_none() && kind.is_none() {
        return Ok(Err("Nothing to change: give a new page, `title`, `summary` or `kind`.".into()));
    }
    tx.execute(
        "UPDATE artifacts SET title = COALESCE(?2, title), summary = COALESCE(?3, summary), kind = COALESCE(?4, kind), current_version = ?5, updated_at = ?6 WHERE id = ?1",
        params![id, title, summary, kind, version, now],
    )?;
    add_link(tx, id, origin, "updated", version, now)?;
    Ok(Ok(()))
}

/// After a change's transaction: the copy of the current page when it has a new one, or the new version's file gone
/// when it wasn't saved.
fn finish_change(dir: &Path, id: &str, saved: &Result<bool, String>, written: Option<(PathBuf, String)>) {
    let Some((path, page)) = written else { return };
    match saved {
        Ok(true) => {
            if let Ok(folder) = artifact_dir(dir, id) {
                let _ = write_file(&folder.join("index.html"), &page);
            }
            crate::artifact_share::page_changed(dir, id);
        }
        _ => {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Changes an artifact: a new page makes the next version. `None` when there's no such artifact.
pub fn update(
    db: &Db,
    dir: &Path,
    id: &str,
    change: &Change,
    origin: &Origin,
    now: i64,
) -> Result<Option<Artifact>, String> {
    if !valid_id(id) {
        return Ok(None);
    }
    if let Some(page) = &change.html
        && get(db, id)?.is_some()
    {
        save_images(dir, id, page);
    }
    let mut written = None;
    let found = immediate(db, |tx| {
        if tx.query_row("SELECT 1 FROM artifacts WHERE id = ?1", [id], |_| Ok(())).optional()?.is_none() {
            return Ok(Ok(false));
        }
        Ok(save_change(tx, dir, id, change, origin, now, &mut written)?.map(|()| true))
    });
    finish_change(dir, id, &found, written);
    match found? {
        false => Ok(None),
        true => get(db, id),
    }
}

/// Publishes a page from its file: the first time a new artifact, then the next version of that one each time the
/// file is published again, or no new version when its page is the current one already. The artifact is looked up
/// and saved in one write transaction, so two publishes of a new file at once make one artifact.
pub fn publish(
    db: &Db,
    dir: &Path,
    publish: &Publish,
    origin: &Origin,
    now: i64,
) -> Result<(Artifact, Published), String> {
    check_html(&publish.html)?;
    let title = publish.title.as_deref().map(clean_title).filter(|title| !title.is_empty());
    let summary = publish.summary.as_deref().map(|summary| cut(summary, MAX_SUMMARY));
    let kind = clean_kind(publish.kind.as_deref())?;
    let label = publish.label.as_deref().map(|label| cut(label, MAX_NOTE)).unwrap_or_default();
    let new_id = uuid::Uuid::new_v4().to_string();
    // Its pictures go in first, into the artifact the file is (or will be): the transaction finds that again.
    let guess = db
        .with(|conn| {
            conn.query_row(
                "SELECT id FROM artifacts WHERE source = ?1 ORDER BY created_at LIMIT 1",
                [&publish.source],
                |row| row.get::<_, String>(0),
            )
            .optional()
        })?
        .unwrap_or_else(|| new_id.clone());
    save_images(dir, &guess, &publish.html);
    let mut written = None;
    let saved = immediate(db, |tx| {
        let known = tx
            .query_row(
                &format!("SELECT {COLUMNS} FROM artifacts WHERE source = ?1 ORDER BY created_at LIMIT 1"),
                [&publish.source],
                artifact,
            )
            .optional()?;
        let Some(known) = known else {
            let title = title
                .clone()
                .or_else(|| page_title(&publish.html))
                .or_else(|| file_title(&publish.source))
                .unwrap_or_else(|| "Untitled".into());
            let new = New {
                title,
                html: publish.html.clone(),
                summary: summary.clone().unwrap_or_default(),
                kind: kind.clone(),
            };
            return Ok(insert_new(tx, dir, &new_id, &new, Some(&publish.source), &label, origin, now)?
                .map(|()| (new_id.clone(), Published::Created)));
        };
        let file: String = tx.query_row(
            "SELECT file FROM artifact_versions WHERE artifact_id = ?1 AND version = ?2",
            params![known.id, known.current_version],
            |row| row.get(0),
        )?;
        let new_page = read_page(dir, &known.id, &file).ok().as_deref() != Some(publish.html.as_str());
        // Only what's different counts: an agent may give the same title and summary each time.
        let change = Change {
            html: new_page.then(|| publish.html.clone()),
            title: title.clone().filter(|title| *title != known.title),
            summary: summary.clone().filter(|summary| *summary != known.summary),
            kind: kind.clone().filter(|kind| *kind != known.kind),
            note: Some(label.clone()),
        };
        if change.html.is_none() && change.title.is_none() && change.summary.is_none() && change.kind.is_none() {
            // Nothing new; the conversation still shows its card.
            add_link(tx, &known.id, origin, "opened", known.current_version, now)?;
            return Ok(Ok((known.id, Published::Unchanged)));
        }
        let outcome = if new_page { Published::Saved } else { Published::Unchanged };
        Ok(save_change(tx, dir, &known.id, &change, origin, now, &mut written)?.map(|()| (known.id.clone(), outcome)))
    });
    if let Ok((id, _)) = &saved {
        // Another publish of the file got there first: the pictures go with the artifact it made.
        if *id != guess {
            save_images(dir, id, &publish.html);
        }
        if *id != new_id {
            let _ = std::fs::remove_dir_all(dir.join(&new_id));
        }
    }
    match saved {
        Ok((id, Published::Created)) => {
            finish_new(db, dir, &id, Ok(()), &publish.html).map(|made| (made, Published::Created))
        }
        Ok((id, outcome)) => {
            finish_change(dir, &id, &Ok(true), written);
            Ok((get(db, &id)?.ok_or("The artifact wasn't saved.")?, outcome))
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(dir.join(&new_id));
            finish_change(dir, "", &Err(error.clone()), written);
            Err(error)
        }
    }
}

/// Saves an old version's page again as the newest version. `None` when there's no such artifact or version. The file
/// it's published from follows, unless that file has changes not published yet (they stay, for its agent to publish).
pub fn restore(
    db: &Db,
    dir: &Path,
    id: &str,
    version: i64,
    origin: &Origin,
    now: i64,
) -> Result<Option<Artifact>, String> {
    let Some((_, html)) = document(db, dir, id, Some(version))? else { return Ok(None) };
    let before = document(db, dir, id, None)?.map(|(_, page)| page);
    let change =
        Change { html: Some(html.clone()), note: Some(format!("Restored version {version}")), ..Change::default() };
    let restored = update(db, dir, id, &change, origin, now)?;
    if let Some(source) = restored.as_ref().and_then(|found| found.source.as_deref()).map(Path::new)
        && before.is_some()
        && std::fs::read_to_string(source).ok() == before
    {
        let _ = write_file(source, &html);
    }
    Ok(restored)
}

/// The file artifact `id` is published from, to read and to edit before publishing it again: its `source`, written
/// again from the current version when it's gone, or, for an artifact without one (made before publish_artifact, or by
/// the app), a new file named for it in `pages`, which becomes its source. None when there's no such artifact.
pub fn page_file(db: &Db, dir: &Path, pages: &Path, id: &str) -> Result<Option<PageFile>, String> {
    let Some(found) = get(db, id)? else { return Ok(None) };
    let Some((_, html)) = document(db, dir, id, None)? else { return Ok(None) };
    if let Some(source) = found.source.as_deref().map(PathBuf::from) {
        match std::fs::read_to_string(&source) {
            Ok(text) => return Ok(Some(PageFile { current: text == html, path: source })),
            // Its folder is still there: the file goes back where it was.
            Err(_) if source.parent().is_some_and(Path::is_dir) => {
                write_file(&source, &html)?;
                return Ok(Some(PageFile { path: source, current: true }));
            }
            Err(_) => {}
        }
    }
    let stem =
        Some(crate::skills::clean_name(&found.title)).filter(|stem| !stem.is_empty()).unwrap_or_else(|| "page".into());
    let taken = |path: &Path| -> Result<bool, String> {
        let key = source_key(path);
        Ok(path.exists()
            || db
                .with(|conn| {
                    conn.query_row("SELECT 1 FROM artifacts WHERE source = ?1", [&key], |_| Ok(())).optional()
                })?
                .is_some())
    };
    let mut path = pages.join(format!("{stem}.html"));
    let mut number = 1;
    while taken(&path)? {
        number += 1;
        path = pages.join(format!("{stem}-{number}.html"));
    }
    write_file(&path, &html)?;
    let key = source_key(&path);
    db.write(|tx| tx.execute("UPDATE artifacts SET source = ?2 WHERE id = ?1", params![id, key]))?;
    Ok(Some(PageFile { path: PathBuf::from(key), current: true }))
}

/// Records that a conversation's agent opened the artifact, so the conversation shows its card.
pub fn link_opened(db: &Db, id: &str, origin: &Origin, now: i64) -> Result<bool, String> {
    let Some(found) = get(db, id)? else { return Ok(false) };
    db.write(|tx| add_link(tx, id, origin, "opened", found.current_version, now))?;
    Ok(true)
}

pub fn get(db: &Db, id: &str) -> Result<Option<Artifact>, String> {
    db.with(|conn| conn.query_row(&format!("SELECT {COLUMNS} FROM artifacts WHERE id = ?1"), [id], artifact).optional())
}

/// The newest changed first.
pub fn list(db: &Db, limit: usize) -> Result<Vec<Artifact>, String> {
    db.with(|conn| {
        let mut statement = conn
            .prepare(&format!("SELECT {COLUMNS} FROM artifacts ORDER BY updated_at DESC, created_at DESC LIMIT ?1"))?;
        let rows = statement.query_map([limit.clamp(1, LIST_LIMIT) as i64], artifact)?;
        rows.collect()
    })
}

/// Those whose title, summary or kind has every word of `query` (any case), newest changed first.
pub fn search(db: &Db, query: &str, limit: usize) -> Result<Vec<Artifact>, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let all = list(db, LIST_LIMIT)?;
    Ok(all
        .into_iter()
        .filter(|found| {
            let text = format!("{} {} {}", found.title, found.summary, found.kind).to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .take(limit)
        .collect())
}

/// Newest first.
pub fn versions(db: &Db, id: &str, limit: usize) -> Result<Vec<Version>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(
            "SELECT version, file, bytes, note, author, created_at FROM artifact_versions WHERE artifact_id = ?1 ORDER BY version DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![id, limit.max(1) as i64], version_row)?;
        rows.collect()
    })
}

/// A version (the current one when `version` is none) and its page.
pub fn document(db: &Db, dir: &Path, id: &str, version: Option<i64>) -> Result<Option<(Version, String)>, String> {
    let row = db.with(|conn| {
        conn.query_row(
            "SELECT v.version, v.file, v.bytes, v.note, v.author, v.created_at FROM artifact_versions v JOIN artifacts a ON a.id = v.artifact_id
             WHERE v.artifact_id = ?1 AND v.version = COALESCE(?2, a.current_version)",
            params![id, version],
            version_row,
        )
        .optional()
    })?;
    let Some(row) = row else { return Ok(None) };
    let html = read_page(dir, id, &row.file)?;
    Ok(Some((row, html)))
}

/// Marks it opened by the user (the Artifacts tab stops calling it new). False when it was already, or isn't there.
pub fn mark_opened(db: &Db, id: &str, now: i64) -> Result<bool, String> {
    db.write(|tx| {
        tx.execute("UPDATE artifacts SET opened_at = ?2 WHERE id = ?1 AND opened_at IS NULL", params![id, now])
            .map(|changed| changed > 0)
    })
}

/// The cards a conversation shows, oldest first: what each turn did with each artifact, at the latest change.
pub fn chat_cards(db: &Db, chat_id: &str) -> Result<Vec<ChatCard>, String> {
    struct Linked {
        id: String,
        artifact_id: String,
        turn_id: Option<String>,
        action: String,
        version: i64,
        at: i64,
        title: String,
        summary: String,
        note: Option<String>,
    }
    let links: Vec<Linked> = db.with(|conn| {
        let mut statement = conn.prepare(
            "SELECT l.id, l.artifact_id, l.turn_id, l.action, l.version, l.created_at, a.title, a.summary,
                    (SELECT note FROM artifact_versions v WHERE v.artifact_id = l.artifact_id AND v.version = l.version)
             FROM artifact_links l JOIN artifacts a ON a.id = l.artifact_id WHERE l.chat_id = ?1 ORDER BY l.created_at, l.rowid",
        )?;
        let rows = statement.query_map([chat_id], |row| {
            Ok(Linked {
                id: row.get(0)?,
                artifact_id: row.get(1)?,
                turn_id: row.get(2)?,
                action: row.get(3)?,
                version: row.get(4)?,
                at: row.get(5)?,
                title: row.get(6)?,
                summary: row.get(7)?,
                note: row.get(8)?,
            })
        })?;
        rows.collect()
    })?;
    let mut cards: Vec<ChatCard> = Vec::new();
    let mut keys: Vec<(String, String)> = Vec::new();
    for link in links {
        let key = (link.artifact_id.clone(), link.turn_id.clone().unwrap_or_else(|| link.id.clone()));
        let note = link.note.filter(|note| !note.trim().is_empty() && link.action == "updated");
        let text = note.unwrap_or_else(|| link.summary.clone());
        match keys.iter().position(|known| *known == key) {
            Some(index) => {
                let card = &mut cards[index];
                card.version = card.version.max(link.version);
                card.at = link.at;
                if card.action != "created" {
                    card.text = text;
                }
            }
            None => {
                keys.push(key);
                cards.push(ChatCard {
                    id: link.id,
                    artifact_id: link.artifact_id,
                    title: link.title,
                    text,
                    action: link.action,
                    version: link.version,
                    first_at: link.at,
                    at: link.at,
                });
            }
        }
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::temp_path;

    fn setup(name: &str) -> (Db, PathBuf) {
        let path = temp_path(name);
        let db = Db::open(&path).unwrap();
        (db, folder_for(&path))
    }

    fn origin(chat: &str, turn: &str) -> Origin {
        Origin { author: "Filo".into(), chat_id: Some(chat.into()), turn_id: Some(turn.into()) }
    }

    fn page(body: &str) -> String {
        format!("<!doctype html><html><head><!--filo:head--></head><body>{body}</body></html>")
    }

    #[test]
    fn create_update_restore_and_versions_survive_reopening() {
        let path = temp_path("artifacts");
        let dir = folder_for(&path);
        let db = Db::open(&path).unwrap();
        let new = New {
            title: "  Budget\nplanner ".into(),
            html: page("<h1>v1</h1>"),
            summary: "Monthly budget".into(),
            kind: Some("Tool".into()),
        };
        let made = create(&db, &dir, &new, &origin("chat-1", "turn-1"), 1_000).unwrap();
        assert_eq!(
            (made.title.as_str(), made.kind.as_str(), made.current_version, made.opened_at, made.source.as_deref()),
            ("Budget planner", "tool", 1, None, None)
        );
        assert!(dir.join(&made.id).join("v1.html").is_file());
        assert_eq!(std::fs::read_to_string(dir.join(&made.id).join("index.html")).unwrap(), page("<h1>v1</h1>"));

        let change =
            Change { html: Some(page("<h1>v2</h1>")), note: Some("Renamed the heading".into()), ..Change::default() };
        let changed = update(&db, &dir, &made.id, &change, &origin("chat-1", "turn-1"), 2_000).unwrap().unwrap();
        assert_eq!((changed.current_version, changed.updated_at), (2, 2_000));
        assert_eq!(document(&db, &dir, &made.id, None).unwrap().unwrap().1, page("<h1>v2</h1>"));
        assert_eq!(document(&db, &dir, &made.id, Some(1)).unwrap().unwrap().1, page("<h1>v1</h1>"));

        // Only the title: no new version.
        let renamed = update(
            &db,
            &dir,
            &made.id,
            &Change { title: Some("Budget".into()), ..Change::default() },
            &Origin::default(),
            3_000,
        )
        .unwrap()
        .unwrap();
        assert_eq!((renamed.title.as_str(), renamed.current_version), ("Budget", 2));

        let restored = restore(&db, &dir, &made.id, 1, &Origin::default(), 4_000).unwrap().unwrap();
        assert_eq!(restored.current_version, 3);
        let rows = versions(&db, &made.id, 50).unwrap();
        assert_eq!(rows.iter().map(|row| row.version).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(rows[0].note, "Restored version 1");
        assert_eq!(std::fs::read_to_string(dir.join(&made.id).join("index.html")).unwrap(), page("<h1>v1</h1>"));

        assert!(mark_opened(&db, &made.id, 5_000).unwrap());
        assert!(!mark_opened(&db, &made.id, 6_000).unwrap());
        drop(db);
        // After a restart: all of it is still there.
        let db = Db::open(&path).unwrap();
        let again = get(&db, &made.id).unwrap().unwrap();
        assert_eq!((again.current_version, again.opened_at, again.title.as_str()), (3, Some(5_000), "Budget"));
        assert_eq!(list(&db, 10).unwrap().len(), 1);
        assert_eq!(search(&db, "budget TOOL", 10).unwrap().len(), 1);
        assert!(search(&db, "diagram", 10).unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn bad_input_changes_nothing() {
        let (db, dir) = setup("artifacts-bad");
        let origin = origin("chat-1", "turn-1");
        assert!(create(&db, &dir, &New { title: " ".into(), html: page("x"), ..New::default() }, &origin, 1).is_err());
        assert!(
            create(&db, &dir, &New { title: "T".into(), html: "  ".into(), ..New::default() }, &origin, 1).is_err()
        );
        assert!(
            create(
                &db,
                &dir,
                &New { title: "T".into(), html: page("x"), kind: Some("movie".into()), ..New::default() },
                &origin,
                1
            )
            .is_err()
        );
        let huge = "x".repeat(MAX_HTML_BYTES + 1);
        assert!(create(&db, &dir, &New { title: "T".into(), html: huge, ..New::default() }, &origin, 1).is_err());
        assert!(list(&db, 10).unwrap().is_empty());
        // Nothing left behind on disk either.
        assert!(std::fs::read_dir(&dir).map_or(true, |mut entries| entries.next().is_none()));

        let made = create(&db, &dir, &New { title: "T".into(), html: page("<p>a</p>"), ..New::default() }, &origin, 1)
            .unwrap();
        assert!(update(&db, &dir, &made.id, &Change::default(), &origin, 2).is_err());
        assert!(
            update(&db, &dir, &made.id, &Change { html: Some(" ".into()), ..Change::default() }, &origin, 2).is_err()
        );
        assert!(
            update(
                &db,
                &dir,
                &made.id,
                &Change { html: Some(page("b")), kind: Some("movie".into()), ..Change::default() },
                &origin,
                2
            )
            .is_err()
        );
        let after = get(&db, &made.id).unwrap().unwrap();
        assert_eq!((after.current_version, after.updated_at), (1, 1));
        assert_eq!(versions(&db, &made.id, 10).unwrap().len(), 1);
        assert!(!dir.join(&made.id).join("v2.html").exists());
        assert_eq!(
            update(&db, &dir, "no-such", &Change { title: Some("x".into()), ..Change::default() }, &origin, 2).unwrap(),
            None
        );
        assert_eq!(
            update(&db, &dir, "../escape", &Change { title: Some("x".into()), ..Change::default() }, &origin, 2)
                .unwrap(),
            None
        );
        assert_eq!(document(&db, &dir, &made.id, Some(9)).unwrap(), None);
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn a_conversation_shows_one_card_per_artifact_and_turn() {
        let (db, dir) = setup("artifacts-cards");
        let made = create(
            &db,
            &dir,
            &New { title: "Plan".into(), html: page("1"), summary: "Trip plan".into(), ..New::default() },
            &origin("chat-1", "turn-1"),
            10,
        )
        .unwrap();
        let edit = |text: &str, turn: &str, at: i64| {
            let change = Change { html: Some(page(text)), note: Some(format!("Now {text}")), ..Change::default() };
            update(&db, &dir, &made.id, &change, &origin("chat-1", turn), at).unwrap().unwrap();
        };
        edit("2", "turn-1", 20);
        edit("3", "turn-2", 30);
        link_opened(&db, &made.id, &origin("chat-2", "turn-9"), 40).unwrap();
        let cards = chat_cards(&db, "chat-1").unwrap();
        assert_eq!(cards.len(), 2, "{cards:#?}");
        assert_eq!(
            (cards[0].action.as_str(), cards[0].version, cards[0].first_at, cards[0].at, cards[0].text.as_str()),
            ("created", 2, 10, 20, "Trip plan")
        );
        assert_eq!((cards[1].action.as_str(), cards[1].version, cards[1].text.as_str()), ("updated", 3, "Now 3"));
        let opened = chat_cards(&db, "chat-2").unwrap();
        assert_eq!((opened.len(), opened[0].action.as_str()), (1, "opened"));
        assert!(chat_cards(&db, "chat-3").unwrap().is_empty());
        // The app's own changes (no conversation) leave no card.
        restore(&db, &dir, &made.id, 1, &Origin::default(), 50).unwrap();
        assert_eq!(chat_cards(&db, "chat-1").unwrap().len(), 2);
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn two_databases_saving_at_once_get_their_own_versions() {
        let path = temp_path("artifacts-race");
        let dir = folder_for(&path);
        let db = Db::open(&path).unwrap();
        let made =
            create(&db, &dir, &New { title: "T".into(), html: page("0"), ..New::default() }, &Origin::default(), 1)
                .unwrap();
        let threads: Vec<_> = (0..4)
            .map(|index| {
                let (path, dir, id) = (path.clone(), dir.clone(), made.id.clone());
                std::thread::spawn(move || {
                    let db = Db::open(&path).unwrap();
                    for step in 0..5 {
                        let change = Change { html: Some(page(&format!("{index}-{step}"))), ..Change::default() };
                        update(&db, &dir, &id, &change, &Origin::default(), 2).unwrap().unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let rows = versions(&db, &made.id, 100).unwrap();
        assert_eq!(rows.len(), 21);
        for row in &rows {
            let html = document(&db, &dir, &made.id, Some(row.version)).unwrap().unwrap().1;
            assert_eq!(html.len() as i64, row.bytes);
        }
        assert_eq!(get(&db, &made.id).unwrap().unwrap().current_version, 21);
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A page published from `file`, as publish_artifact reads it.
    fn from_file(file: &Path, title: Option<&str>, label: Option<&str>) -> Publish {
        Publish {
            source: source_key(file),
            html: std::fs::read_to_string(file).unwrap(),
            title: title.map(str::to_string),
            label: label.map(str::to_string),
            ..Publish::default()
        }
    }

    #[test]
    fn publishing_a_file_again_saves_its_next_version() {
        let (db, dir) = setup("artifacts-publish");
        let work = dir.parent().unwrap().join("workspace/pages");
        std::fs::create_dir_all(&work).unwrap();
        let file = work.join("meal-plan.html");
        std::fs::write(
            &file,
            "<html><head><title>Meals &amp; more</title><!--filo:head--></head><body>Week 1</body></html>",
        )
        .unwrap();

        // The first publish makes the artifact, named by the page's <title>, with its label on version 1.
        let (made, outcome) =
            publish(&db, &dir, &from_file(&file, None, Some("First draft")), &origin("chat-1", "turn-1"), 10).unwrap();
        assert_eq!(
            (outcome, made.title.as_str(), made.current_version, made.kind.as_str()),
            (Published::Created, "Meals & more", 1, "page")
        );
        assert_eq!(made.source.as_deref(), Some(source_key(&file).as_str()));
        assert_eq!(versions(&db, &made.id, 5).unwrap()[0].note, "First draft");
        assert_eq!(
            std::fs::read_to_string(dir.join(&made.id).join("index.html")).unwrap(),
            std::fs::read_to_string(&file).unwrap()
        );

        // Edited and published again, by another spelling of the same path: the next version of the same artifact.
        std::fs::write(&file, "<html><head><title>Meals</title><!--filo:head--></head><body>Week 2</body></html>")
            .unwrap();
        let roundabout = work.join("../pages/./meal-plan.html");
        let (saved, outcome) = publish(
            &db,
            &dir,
            &from_file(&roundabout, Some("Meal plan"), Some("Week 2")),
            &origin("chat-1", "turn-2"),
            20,
        )
        .unwrap();
        assert_eq!(
            (outcome, saved.id.as_str(), saved.current_version, saved.title.as_str()),
            (Published::Saved, made.id.as_str(), 2, "Meal plan")
        );
        assert!(document(&db, &dir, &made.id, Some(1)).unwrap().unwrap().1.contains("Week 1"));

        // The same page again: no new version, and the same title and summary given again change nothing.
        let (same, outcome) =
            publish(&db, &dir, &from_file(&file, Some("Meal plan"), Some("Again")), &origin("chat-2", "turn-3"), 30)
                .unwrap();
        assert_eq!((outcome, same.current_version, same.updated_at), (Published::Unchanged, 2, 20));
        // ...but that conversation shows its card.
        assert_eq!(chat_cards(&db, "chat-2").unwrap()[0].action, "opened");
        let (described, outcome) = publish(
            &db,
            &dir,
            &Publish { summary: Some("Dinners".into()), ..from_file(&file, None, None) },
            &Origin::default(),
            40,
        )
        .unwrap();
        assert_eq!(
            (outcome, described.current_version, described.summary.as_str()),
            (Published::Unchanged, 2, "Dinners")
        );

        // Another file is another artifact, named by its file without a <title>.
        let other = work.join("trip_notes.html");
        std::fs::write(&other, "<p>Lisbon</p>").unwrap();
        let (trip, outcome) = publish(&db, &dir, &from_file(&other, None, None), &Origin::default(), 50).unwrap();
        assert_eq!((outcome, trip.title.as_str()), (Published::Created, "Trip notes"));
        assert_ne!(trip.id, made.id);
        // A bad kind or an empty page saves nothing.
        assert!(
            publish(
                &db,
                &dir,
                &Publish { kind: Some("movie".into()), ..from_file(&file, None, None) },
                &Origin::default(),
                60
            )
            .is_err()
        );
        assert!(
            publish(&db, &dir, &Publish { html: " ".into(), ..from_file(&other, None, None) }, &Origin::default(), 60)
                .is_err()
        );
        assert_eq!(list(&db, 10).unwrap().len(), 2);
        assert_eq!(get(&db, &trip.id).unwrap().unwrap().current_version, 1);
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn two_publishes_of_a_new_file_at_once_make_one_artifact() {
        let path = temp_path("artifacts-publish-race");
        let dir = folder_for(&path);
        drop(Db::open(&path).unwrap());
        let file = path.parent().unwrap().join("page.html");
        std::fs::write(&file, page("same")).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (path, dir, file) = (path.clone(), dir.clone(), file.clone());
                std::thread::spawn(move || {
                    publish(&Db::open(&path).unwrap(), &dir, &from_file(&file, None, None), &Origin::default(), 1)
                        .unwrap()
                        .1
                })
            })
            .collect();
        let outcomes: Vec<Published> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|outcome| **outcome == Published::Created).count(), 1, "{outcomes:?}");
        let db = Db::open(&path).unwrap();
        assert_eq!(list(&db, 10).unwrap().len(), 1);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn the_page_file_follows_restores_and_old_artifacts_get_one() {
        let (db, dir) = setup("artifacts-page-file");
        let pages = dir.parent().unwrap().join("workspace").join(PAGES_DIR);
        std::fs::create_dir_all(&pages).unwrap();
        let file = pages.join("plan.html");
        std::fs::write(&file, page("one")).unwrap();
        let (made, _) = publish(&db, &dir, &from_file(&file, None, None), &Origin::default(), 1).unwrap();
        std::fs::write(&file, page("two")).unwrap();
        publish(&db, &dir, &from_file(&file, None, None), &Origin::default(), 2).unwrap();
        assert_eq!(
            page_file(&db, &dir, &pages, &made.id).unwrap().unwrap(),
            PageFile { path: PathBuf::from(source_key(&file)), current: true }
        );

        // The user restores version 1 in the app: the file follows, since it had nothing unpublished.
        restore(&db, &dir, &made.id, 1, &Origin::default(), 3).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), page("one"));
        // Edits not published yet stay as they are through a restore, and the file says it isn't the current page.
        std::fs::write(&file, page("draft")).unwrap();
        restore(&db, &dir, &made.id, 2, &Origin::default(), 4).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), page("draft"));
        assert!(!page_file(&db, &dir, &pages, &made.id).unwrap().unwrap().current);
        // A file deleted from its folder comes back with the current page.
        std::fs::remove_file(&file).unwrap();
        assert!(page_file(&db, &dir, &pages, &made.id).unwrap().unwrap().current);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), page("two"));

        // One made by the app has no file: it gets one in `pages`, named for it and not over another's, and publishing
        // that file saves its next version.
        std::fs::write(pages.join("budget.html"), "someone else's").unwrap();
        let old = create(
            &db,
            &dir,
            &New { title: "Budget!".into(), html: page("old"), ..New::default() },
            &Origin::default(),
            5,
        )
        .unwrap();
        let given = page_file(&db, &dir, &pages, &old.id).unwrap().unwrap();
        assert_eq!(given.path, PathBuf::from(source_key(&pages.join("budget-2.html"))));
        assert_eq!(std::fs::read_to_string(&given.path).unwrap(), page("old"));
        std::fs::write(&given.path, page("new")).unwrap();
        let (saved, outcome) = publish(&db, &dir, &from_file(&given.path, None, None), &Origin::default(), 6).unwrap();
        assert_eq!((saved.id, outcome, saved.current_version), (old.id.clone(), Published::Saved, 2));
        assert_eq!(page_file(&db, &dir, &pages, "missing").unwrap(), None);
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn titles_and_warnings_come_from_the_page() {
        assert_eq!(
            page_title("<HTML><Title id=x>  Q3 &lt;draft&gt; &amp; notes </TITLE>").as_deref(),
            Some("Q3 <draft> & notes")
        );
        assert_eq!(page_title("<html><title> </title>"), None);
        assert_eq!(page_title("<p>no title</p>"), None);
        // The template's how-to comment names <title> too.
        assert_eq!(page_title("<!-- Set <title> and the h1 --><title>Budget</title>").as_deref(), Some("Budget"));
        assert!(page_warnings(&format!("<!-- no <script src=\"https://x.com/a.js\"> -->{}", page("ok"))).is_empty());
        assert!(
            page_warnings(&page("<script>localStorage.setItem('a', 1)</script>"))[0]
                .contains("localStorage, which its sandbox doesn't have")
        );
        assert_eq!(file_title("/a/b/q3_sales-report.html").as_deref(), Some("Q3 sales report"));
        assert!(
            page_warnings(&page("<a href=\"https://example.com\">fine</a><img src=\"data:image/png;base64,AA\">"))
                .is_empty()
        );
        let remote = page_warnings(
            "<html><head><!--filo:head--><link rel=stylesheet href=\"https://cdn.example.com/a.css\"><script src='//cdn.example.com/b.js'></script><style>@import url(https://fonts.example.com/c.css)</style></head></html>",
        );
        assert_eq!(remote.len(), 1, "{remote:?}");
        assert!(
            remote[0].contains("3 files")
                && remote[0].contains("https://cdn.example.com/a.css")
                && remote[0].contains("//cdn.example.com/b.js"),
            "{remote:?}"
        );
        assert!(page_warnings("<html><body>plain</body></html>")[0].contains("<!--filo:head-->"));
    }
}

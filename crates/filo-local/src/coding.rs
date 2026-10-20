// Coding on this Mac, in Filo's database (db.rs): the Coding page's settings (each tool plugged in or not, its mode,
// model and reasoning level; where tasks work; how freely Filo works on the computer), the project folders (git repos
// found under the home folder, and folders the user added, git or not), and coding tasks with their history.
//
// A task is the user's brief for a coding tool (Claude Code, Codex, Muse Code or OpenCode) in one project folder. It
// runs in the folder as it is, or in a git worktree of its own on a new branch (the "Own worktree" setting, git projects
// only). Everything it does lands in `coding_events` as it happens: the brief and follow-ups (`owner`), the tool's
// messages (`tool`), the commands it ran, files it changed, its plan and searches (`activity`), and where the task got
// to (`marker`). The page's thread reads them back in order.
//
// The app runs the tools (src/local_backend/coding.rs, through coding_session.rs); this module has the rows, the walk
// that finds repos and the reading of Claude Code's JSON events into steps.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::db::Db;
use crate::engine::Engine;

/// The tables (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE coding_settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE coding_tools (
    id      TEXT PRIMARY KEY NOT NULL,
    plugged INTEGER NOT NULL,
    mode    TEXT NOT NULL,
    model   TEXT,
    effort  TEXT
);
CREATE TABLE coding_projects (
    path    TEXT PRIMARY KEY NOT NULL,
    name    TEXT NOT NULL,
    branch  TEXT,
    remote  TEXT,
    git     INTEGER NOT NULL,
    added   INTEGER NOT NULL DEFAULT 0,
    found   INTEGER NOT NULL DEFAULT 0,
    seen_at INTEGER NOT NULL
);
CREATE TABLE coding_tasks (
    id           TEXT PRIMARY KEY NOT NULL,
    title        TEXT NOT NULL,
    brief        TEXT NOT NULL,
    tool         TEXT NOT NULL,
    mode         TEXT NOT NULL,
    model        TEXT,
    effort       TEXT,
    project_path TEXT NOT NULL,
    project_name TEXT NOT NULL,
    work_path    TEXT NOT NULL,
    worktree     INTEGER NOT NULL,
    plain_folder INTEGER NOT NULL,
    branch       TEXT,
    status       TEXT NOT NULL,
    owner        TEXT NOT NULL,
    session      TEXT,
    error        TEXT,
    last_message TEXT,
    rounds       INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX coding_tasks_by_update ON coding_tasks (updated_at DESC);
CREATE TABLE coding_events (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
    at      INTEGER NOT NULL,
    source  TEXT NOT NULL,
    kind    TEXT,
    text    TEXT NOT NULL
);
CREATE INDEX coding_events_by_task ON coding_events (task_id, id);
";

/// The tools, as the page names them (`CODING_TOOLS`), in `Engine::ALL`'s order.
pub const TOOLS: [&str; 5] = ["claude-code", "codex", "muse", "opencode", "pi"];

/// The reasoning levels any tool names, lowest first: what a level's nearest offered one is measured on (the app's
/// `nearest_effort`) and what `start_work` may ask for.
pub const EFFORT_ORDER: [&str; 8] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

/// How a reasoning level reads in the pickers: "Extra high" for "xhigh", the others capitalised.
pub fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra high".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}
/// The tools a page that never touched them has unplugged: the ones added after the first two, for the user to plug in.
const UNPLUGGED_AT_FIRST: [&str; 3] = ["muse", "opencode", "pi"];
/// How freely a tool works (`CODING_MODES`); a new task takes its tool's, and so does a chat turn on its engine
/// (`turn_mode`). A tool saved in a mode the page no longer offers (supervised, auto_accept_edits) reads as the
/// default; tasks made in one keep it.
const MODES: [&str; 2] = ["auto", "full_access"];
pub const DEFAULT_MODE: &str = "auto";
/// Where tasks work: their own git worktree, or the project folder as it is.
const WORKSPACES: [&str; 2] = ["worktree", "checkout"];
const DEFAULT_WORKSPACE: &str = "worktree";

/// A task's brief, and a follow-up, at most this long.
const MAX_BRIEF: usize = 20_000;
/// One step's text is cut to this.
const MAX_STEP: usize = 8_000;
/// A task keeps at most this many steps; the thread reads the newest.
const MAX_EVENTS: usize = 3_000;
/// The task list's limit when the page names none.
pub const DEFAULT_LIMIT: usize = 30;

const TITLE_CHARS: usize = 80;

pub fn engine_of(tool: &str) -> Option<Engine> {
    match tool {
        "claude-code" => Some(Engine::Claude),
        "codex" => Some(Engine::Codex),
        "muse" => Some(Engine::Muse),
        "opencode" => Some(Engine::OpenCode),
        "pi" => Some(Engine::Pi),
        _ => None,
    }
}

pub fn tool_of(engine: Engine) -> &'static str {
    match engine {
        Engine::Claude => "claude-code",
        Engine::Codex => "codex",
        Engine::Muse => "muse",
        Engine::OpenCode => "opencode",
        Engine::Pi => "pi",
    }
}

pub fn is_finished(status: &str) -> bool {
    matches!(status, "done" | "failed" | "stopped")
}

// ---------------------------------------------------------------------------------------------------------------
// Settings

/// A tool's settings for new tasks. A tool nobody has touched is plugged in (but for `UNPLUGGED_AT_FIRST`), in the
/// default mode, with its own model.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolSettings {
    pub id: String,
    pub plugged: bool,
    pub mode: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl ToolSettings {
    fn default_for(id: &str) -> ToolSettings {
        ToolSettings {
            id: id.into(),
            plugged: !UNPLUGGED_AT_FIRST.contains(&id),
            mode: DEFAULT_MODE.into(),
            model: None,
            effort: None,
        }
    }
}

pub fn tools(db: &Db) -> Result<Vec<ToolSettings>, String> {
    db.with(|conn| {
        let mut saved: Vec<ToolSettings> = conn
            .prepare("SELECT id, plugged, mode, model, effort FROM coding_tools")?
            .query_map([], |row| {
                Ok(ToolSettings {
                    id: row.get(0)?,
                    plugged: row.get::<_, i64>(1)? != 0,
                    mode: row.get(2)?,
                    model: row.get(3)?,
                    effort: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        for tool in &mut saved {
            if !MODES.contains(&tool.mode.as_str()) {
                tool.mode = DEFAULT_MODE.into();
            }
        }
        Ok(TOOLS
            .iter()
            .map(|id| {
                saved
                    .iter()
                    .position(|tool| tool.id == *id)
                    .map(|at| saved.swap_remove(at))
                    .unwrap_or_else(|| ToolSettings::default_for(id))
            })
            .collect())
    })
}

pub fn tool(db: &Db, id: &str) -> Result<ToolSettings, String> {
    tools(db)?.into_iter().find(|tool| tool.id == id).ok_or_else(|| format!("Not a coding tool: {id}"))
}

/// Changes one tool's settings: plugged in or out, its mode, its model and reasoning level ("" or none for its own).
pub fn update_tool(
    db: &Db,
    id: &str,
    change: impl FnOnce(&mut ToolSettings) -> Result<(), String>,
) -> Result<ToolSettings, String> {
    let mut settings = tool(db, id)?;
    change(&mut settings)?;
    if !MODES.contains(&settings.mode.as_str()) {
        return Err(format!("Not a mode: {}", settings.mode));
    }
    for value in [&mut settings.model, &mut settings.effort] {
        *value = value.take().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        if let Some(value) = value.as_deref().filter(|value| !crate::engine::valid_model(value)) {
            return Err(format!("Not a name the tool takes: {value}"));
        }
    }
    db.write(|tx| {
        tx.execute(
            "INSERT INTO coding_tools (id, plugged, mode, model, effort) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET plugged = ?2, mode = ?3, model = ?4, effort = ?5",
            params![settings.id, settings.plugged as i64, settings.mode, settings.model, settings.effort],
        )
    })?;
    Ok(settings)
}

fn setting(db: &Db, key: &str) -> Result<Option<String>, String> {
    db.with(|conn| {
        conn.query_row("SELECT value FROM coding_settings WHERE key = ?1", [key], |row| row.get(0)).optional()
    })
}

fn set_setting(db: &Db, key: &str, value: &str) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "INSERT INTO coding_settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, value],
        )
    })
    .map(|_| ())
}

/// Where new tasks work: "worktree" or "checkout".
pub fn workspace(db: &Db) -> Result<String, String> {
    Ok(setting(db, "workspace")?
        .filter(|value| WORKSPACES.contains(&value.as_str()))
        .unwrap_or_else(|| DEFAULT_WORKSPACE.into()))
}

pub fn set_workspace(db: &Db, value: &str) -> Result<(), String> {
    if !WORKSPACES.contains(&value) {
        return Err(format!("Not a place tasks work: {value}"));
    }
    set_setting(db, "workspace", value)
}

/// How freely a chat turn on `engine` works on this Mac (Filo's, an agent's, a group member's, a routine's): its tool's
/// mode on the Coding page, the default when the database can't say.
pub fn turn_mode(db_path: &Path, engine: Engine) -> String {
    Db::open(db_path)
        .and_then(|db| tool(&db, tool_of(engine)))
        .map(|tool| tool.mode)
        .unwrap_or_else(|_| DEFAULT_MODE.into())
}

/// The model and reasoning level a chat turn on Muse Code or OpenCode gets when the chat names none: its tool's on the
/// Coding page. Claude Code and Codex chats keep their own (none here).
pub fn turn_model(db_path: &Path, engine: Engine) -> (Option<String>, Option<String>) {
    if engine.runs_invocation() {
        return (None, None);
    }
    Db::open(db_path)
        .and_then(|db| tool(&db, tool_of(engine)))
        .map(|tool| (tool.model, tool.effort))
        .unwrap_or_default()
}

/// "Keep coding after Filo quits" (on until the user turns it off): quitting leaves running tasks and turns working;
/// off, quitting stops them as Stop would.
pub fn keep_on_quit(db: &Db) -> Result<bool, String> {
    Ok(setting(db, "keep_on_quit")?.is_none_or(|value| value != "0"))
}

pub fn set_keep_on_quit(db: &Db, on: bool) -> Result<(), String> {
    set_setting(db, "keep_on_quit", if on { "1" } else { "0" })
}

/// "Keep Mac awake while tasks run" (off until the user turns it on): the Mac doesn't idle-sleep while a coding task's
/// round or a routine runs (the app's local_backend/awake.rs).
pub fn keep_awake(db: &Db) -> Result<bool, String> {
    Ok(setting(db, "keep_awake")?.is_some_and(|value| value == "1"))
}

pub fn set_keep_awake(db: &Db, on: bool) -> Result<(), String> {
    set_setting(db, "keep_awake", if on { "1" } else { "0" })
}

/// "Keep running with the lid closed (plugged in only)" (off until the user turns it on, which installs what it needs
/// with an administrator's password): while tasks keep the Mac awake, closing the lid doesn't sleep it either (the
/// app's local_backend/lid.rs).
pub fn keep_awake_lid(db: &Db) -> Result<bool, String> {
    Ok(setting(db, "keep_awake_lid")?.is_some_and(|value| value == "1"))
}

pub fn set_keep_awake_lid(db: &Db, on: bool) -> Result<(), String> {
    set_setting(db, "keep_awake_lid", if on { "1" } else { "0" })
}

// ---------------------------------------------------------------------------------------------------------------
// Projects

/// A project folder: a repo the walk found, or a folder the user added (git or not).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// No git history: tasks work in it as it is.
    pub plain_folder: bool,
    /// The user added it (it can be removed again); otherwise the walk found it.
    pub added: bool,
}

/// A folder as it is on disk now.
#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub path: String,
    pub name: String,
    pub git: bool,
    pub branch: Option<String>,
    pub remote: Option<String>,
}

fn project_row(row: &Row) -> rusqlite::Result<Project> {
    Ok(Project {
        path: row.get(0)?,
        name: row.get(1)?,
        branch: row.get(2)?,
        remote: row.get(3)?,
        plain_folder: row.get::<_, i64>(4)? == 0,
        added: row.get::<_, i64>(5)? != 0,
    })
}

const PROJECT_COLUMNS: &str = "path, name, branch, remote, git, added";

/// The projects, by name; with `search`, those whose name or path has it.
pub fn projects(db: &Db, search: Option<&str>) -> Result<Vec<Project>, String> {
    let search = search.map(|search| search.trim().to_lowercase()).filter(|search| !search.is_empty());
    let all: Vec<Project> = db.with(|conn| {
        conn.prepare(&format!("SELECT {PROJECT_COLUMNS} FROM coding_projects ORDER BY name COLLATE NOCASE, path"))?
            .query_map([], project_row)?
            .collect()
    })?;
    Ok(match search {
        Some(search) => all
            .into_iter()
            .filter(|project| {
                project.name.to_lowercase().contains(&search) || project.path.to_lowercase().contains(&search)
            })
            .collect(),
        None => all,
    })
}

pub fn project(db: &Db, path: &str) -> Result<Option<Project>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {PROJECT_COLUMNS} FROM coding_projects WHERE path = ?1"), [path], project_row)
            .optional()
    })
}

/// Adds a folder the user picked, git or not; adding one already listed keeps it for good.
pub fn add_project(db: &Db, path: &Path, now: i64) -> Result<Project, String> {
    let path = path.canonicalize().map_err(|error| format!("Couldn't open {}: {error}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} isn't a folder.", path.display()));
    }
    let folder = describe_folder(&path);
    db.write(|tx| {
        tx.execute(
            "INSERT INTO coding_projects (path, name, branch, remote, git, added, found, seen_at) VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, ?6)
             ON CONFLICT (path) DO UPDATE SET added = 1, branch = ?3, remote = ?4, git = ?5, seen_at = ?6",
            params![folder.path, folder.name, folder.branch, folder.remote, folder.git as i64, now],
        )
    })?;
    project(db, &folder.path)?.ok_or_else(|| "The folder wasn't saved.".into())
}

/// Takes back a folder the user added; one the walk finds stays listed.
pub fn remove_project(db: &Db, path: &str) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("UPDATE coding_projects SET added = 0 WHERE path = ?1", [path])?;
        tx.execute("DELETE FROM coding_projects WHERE path = ?1 AND added = 0 AND found = 0", [path])
    })
    .map(|_| ())
}

/// What a walk found replaces what the last one did; the folders the user added are looked at again (a branch
/// changed, `git init` run in a plain folder). Returns whether anything changed.
pub fn replace_found(db: &Db, found: &[Folder], now: i64) -> Result<bool, String> {
    let added: Vec<String> = db.with(|conn| {
        conn.prepare("SELECT path FROM coding_projects WHERE added = 1")?.query_map([], |row| row.get(0))?.collect()
    })?;
    let added: Vec<Folder> =
        added.iter().filter(|path| Path::new(path).is_dir()).map(|path| describe_folder(Path::new(path))).collect();
    let before = projects(db, None)?;
    db.write(|tx| {
        tx.execute("UPDATE coding_projects SET found = 0", [])?;
        for folder in found {
            tx.execute(
                "INSERT INTO coding_projects (path, name, branch, remote, git, added, found, seen_at) VALUES (?1, ?2, ?3, ?4, ?5, 0, 1, ?6)
                 ON CONFLICT (path) DO UPDATE SET found = 1, name = ?2, branch = ?3, remote = ?4, git = ?5, seen_at = ?6",
                params![folder.path, folder.name, folder.branch, folder.remote, folder.git as i64, now],
            )?;
        }
        for folder in &added {
            tx.execute("UPDATE coding_projects SET branch = ?2, remote = ?3, git = ?4, seen_at = ?5 WHERE path = ?1", params![folder.path, folder.branch, folder.remote, folder.git as i64, now])?;
        }
        tx.execute("DELETE FROM coding_projects WHERE found = 0 AND added = 0", [])
    })?;
    Ok(projects(db, None)? != before)
}

/// The walk stays out of these: system folders, dependencies and build output.
const SKIP: [&str; 15] = [
    "Library",
    "node_modules",
    "Applications",
    "Movies",
    "Music",
    "Pictures",
    "Public",
    "vendor",
    "target",
    "dist",
    "build",
    "Pods",
    "DerivedData",
    "venv",
    "Trash",
];
const MAX_DEPTH: usize = 5;
const MAX_FOLDERS: usize = 25_000;
const MAX_REPOS: usize = 500;

/// The git repos under `root`, by name: a bounded walk (a few levels deep, skipping hidden folders and `SKIP`), never
/// going into a repo, reading each one's branch and remote from its .git files rather than running git.
pub fn find_repos(root: &Path) -> Vec<Folder> {
    let mut repos = Vec::new();
    let mut visited = 0;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(root.to_path_buf(), 0)]);
    while let Some((dir, depth)) = queue.pop_front() {
        if visited >= MAX_FOLDERS || repos.len() >= MAX_REPOS {
            break;
        }
        visited += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
        // A .git folder makes a repo; a .git file is a worktree or submodule, which is left alone.
        if dir != root
            && entries
                .iter()
                .any(|entry| entry.file_name() == ".git" && entry.file_type().is_ok_and(|kind| kind.is_dir()))
        {
            repos.push(describe_folder(&dir));
            continue;
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let mut children: Vec<PathBuf> = entries
            .iter()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| !name.starts_with('.') && !SKIP.contains(&name))
            })
            .map(|entry| entry.path())
            .collect();
        children.sort();
        queue.extend(children.into_iter().map(|child| (child, depth + 1)));
    }
    repos.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    let mut seen = HashSet::new();
    repos.retain(|repo| seen.insert(repo.path.clone()));
    repos
}

/// A folder's name, and whether it's a git repo, with its branch and origin from the .git files.
fn describe_folder(path: &Path) -> Folder {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().to_string());
    let git_dir = path.join(".git");
    let git = git_dir.is_dir();
    let read = |file: &str| if git { std::fs::read_to_string(git_dir.join(file)).ok() } else { None };
    let branch = read("HEAD").and_then(|head| head.trim().strip_prefix("ref: refs/heads/").map(str::to_string));
    let remote = read("config").and_then(|config| origin_url(&config));
    Folder { path: path.display().to_string(), name, git, branch, remote }
}

fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return Some(value.trim().to_string()).filter(|url| !url.is_empty());
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------
// Tasks

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub brief: String,
    pub tool: String,
    pub mode: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub project_path: String,
    pub project_name: String,
    /// Where it runs: the project folder, or its worktree.
    pub work_path: String,
    pub worktree: bool,
    pub plain_folder: bool,
    pub branch: Option<String>,
    /// "queued", "running", "monitoring" (its turn ended with background work still going, coding_session.rs), "done",
    /// "failed" or "stopped".
    pub status: String,
    /// Who started it: "You" from the page.
    pub owner: String,
    /// The tool's session, resumed by a follow-up.
    pub session: Option<String>,
    pub error: Option<String>,
    pub last_message: Option<String>,
    pub rounds: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

const TASK_COLUMNS: &str = "id, title, brief, tool, mode, model, effort, project_path, project_name, work_path, worktree, plain_folder, branch, \
status, owner, session, error, last_message, rounds, created_at, updated_at";

fn task_row(row: &Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        brief: row.get(2)?,
        tool: row.get(3)?,
        mode: row.get(4)?,
        model: row.get(5)?,
        effort: row.get(6)?,
        project_path: row.get(7)?,
        project_name: row.get(8)?,
        work_path: row.get(9)?,
        worktree: row.get::<_, i64>(10)? != 0,
        plain_folder: row.get::<_, i64>(11)? != 0,
        branch: row.get(12)?,
        status: row.get(13)?,
        owner: row.get(14)?,
        session: row.get(15)?,
        error: row.get(16)?,
        last_message: row.get(17)?,
        rounds: row.get(18)?,
        created_at: row.get(19)?,
        updated_at: row.get(20)?,
    })
}

/// The newest tasks first (by when they last changed).
pub fn tasks(db: &Db, limit: usize) -> Result<Vec<Task>, String> {
    db.with(|conn| {
        conn.prepare(&format!("SELECT {TASK_COLUMNS} FROM coding_tasks ORDER BY updated_at DESC, id LIMIT ?1"))?
            .query_map([limit as i64], task_row)?
            .collect()
    })
}

pub fn task(db: &Db, id: &str) -> Result<Option<Task>, String> {
    db.with(|conn| get_task(conn, id))
}

fn get_task(conn: &Connection, id: &str) -> rusqlite::Result<Option<Task>> {
    conn.prepare_cached(&format!("SELECT {TASK_COLUMNS} FROM coding_tasks WHERE id = ?1"))?
        .query_row([id], task_row)
        .optional()
}

/// What a new task is.
#[derive(Clone, Debug)]
pub struct NewTask {
    pub brief: String,
    pub tool: String,
    pub project: Project,
    /// Its own worktree (a git project with "Own worktree" on); the app makes it before the tool starts.
    pub worktree: bool,
    pub owner: String,
}

/// The title a brief gives a task: its first line, cut short.
pub fn title_of(brief: &str) -> String {
    let line = brief.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("Coding task");
    let line = line.trim_start_matches('#').trim();
    if line.chars().count() <= TITLE_CHARS {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(TITLE_CHARS - 1).collect::<String>().trim_end())
    }
}

fn clean_brief(text: &str) -> Result<String, String> {
    let text = text.replace('\r', "").trim().to_string();
    if text.is_empty() {
        return Err("Say what the task is.".into());
    }
    if text.chars().count() > MAX_BRIEF {
        return Err(format!("That's longer than {MAX_BRIEF} characters."));
    }
    Ok(text)
}

/// Saves a new task, queued, with its brief as the first step; it takes its tool's mode, model and reasoning level.
pub fn create_task(db: &Db, new: &NewTask, now: i64) -> Result<Task, String> {
    let brief = clean_brief(&new.brief)?;
    let settings = tool(db, &new.tool)?;
    let id = uuid::Uuid::new_v4().to_string();
    let worktree = new.worktree && !new.project.plain_folder;
    db.write(|tx| {
        tx.execute(
            &format!("INSERT INTO coding_tasks ({TASK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, 'queued', ?13, NULL, NULL, NULL, 0, ?14, ?14)"),
            params![
                id,
                title_of(&brief),
                brief,
                new.tool,
                settings.mode,
                settings.model,
                settings.effort,
                new.project.path,
                new.project.name,
                new.project.path,
                worktree as i64,
                new.project.plain_folder as i64,
                new.owner,
                now
            ],
        )?;
        insert_event(tx, &id, now, "owner", None, &brief)
    })?;
    task(db, &id)?.ok_or_else(|| "The task wasn't saved.".into())
}

/// A follow-up for a finished task: its message as the next step, and the task queued again.
pub fn follow_up(db: &Db, id: &str, message: &str, now: i64) -> Result<Task, String> {
    let message = clean_brief(message)?;
    let current = task(db, id)?.ok_or("This task isn't here anymore.")?;
    if !is_finished(&current.status) {
        return Err("It's still working: stop it first, or wait until it's done.".into());
    }
    db.write(|tx| {
        insert_event(tx, id, now, "owner", None, &message)?;
        tx.execute(
            "UPDATE coding_tasks SET status = 'queued', error = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )
    })?;
    task(db, id)?.ok_or_else(|| "This task isn't here anymore.".into())
}

/// The message the tool gets next: the brief for a first round, then the follow-ups since the last round started.
pub fn next_message(db: &Db, id: &str) -> Result<String, String> {
    let events = events(db, id)?;
    // The owner's steps after the tool last said or did anything, in order.
    let since = events.iter().rposition(|event| event.source != "owner").map_or(0, |at| at + 1);
    let text = events[since..]
        .iter()
        .filter(|event| event.source == "owner")
        .map(|event| event.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.is_empty() {
        task(db, id)?.map(|task| task.brief).ok_or_else(|| "This task isn't here anymore.".into())
    } else {
        Ok(text)
    }
}

/// Where a task runs now, once its worktree is made (or it fell back to the project folder).
pub fn set_work(
    db: &Db,
    id: &str,
    work_path: &str,
    worktree: bool,
    branch: Option<&str>,
    now: i64,
) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_tasks SET work_path = ?2, worktree = ?3, branch = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, work_path, worktree as i64, branch, now],
        )
    })
    .map(|_| ())
}

pub fn set_running(db: &Db, id: &str, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_tasks SET status = 'running', error = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, now],
        )
    })
    .map(|_| ())
}

pub fn set_session(db: &Db, id: &str, session: &str) -> Result<(), String> {
    db.write(|tx| tx.execute("UPDATE coding_tasks SET session = ?2 WHERE id = ?1", params![id, session])).map(|_| ())
}

/// Whether the tasks have `run_model` yet: a column added the first time a tool says its model, not by a migration, so
/// it takes no migration's slot (see artifacts.rs `SCHEMA`).
fn has_run_model(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached("SELECT 1 FROM pragma_table_info('coding_tasks') WHERE name = 'run_model'")?.exists([])
}

/// The model the task's tool said its round started on (what its own default or an alias came to), for the view.
pub fn set_run_model(db: &Db, id: &str, model: &str) -> Result<(), String> {
    db.write(|tx| {
        if !has_run_model(tx)? {
            tx.execute_batch("ALTER TABLE coding_tasks ADD COLUMN run_model TEXT")?;
        }
        tx.execute("UPDATE coding_tasks SET run_model = ?2 WHERE id = ?1", params![id, model])
    })
    .map(|_| ())
}

/// What `set_run_model` saved, if a tool has said.
pub fn run_model(db: &Db, id: &str) -> Result<Option<String>, String> {
    db.with(|conn| {
        if !has_run_model(conn)? {
            return Ok(None);
        }
        conn.query_row("SELECT run_model FROM coding_tasks WHERE id = ?1", [id], |row| row.get(0))
            .optional()
            .map(Option::flatten)
    })
}

/// The model Claude Code's init line names: what the round runs on, its default or an alias resolved.
pub fn claude_init_model(event: &Value) -> Option<String> {
    (event["type"] == "system" && event["subtype"] == "init")
        .then(|| event["model"].as_str())
        .flatten()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// A model a coding tool offers, for the Coding page's pickers (Muse Code's and OpenCode's, which they say
/// themselves: coding_muse.rs and coding_opencode.rs `models`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolModel {
    pub id: String,
    pub name: String,
    /// The reasoning levels it takes.
    pub efforts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// What the tool answers with when it isn't given a model.
    pub is_default: bool,
}

impl ToolModel {
    /// The model as a row of the composer's picker.
    pub fn option(self, engine: Engine) -> crate::engine::ModelOption {
        crate::engine::ModelOption {
            engine,
            id: self.id,
            label: self.name,
            default: self.is_default,
            aliases: Vec::new(),
        }
    }
}

/// A model id as people say it when the picker doesn't list it: "Opus 5.5" for `claude-opus-5-5`, "Sonnet 5.5 (1M)"
/// for `claude-sonnet-5-5[1m]`; other ids as they are (`codingModelLabel` in coding-protocol.ts).
pub fn model_label(id: &str) -> String {
    let (base, long) = match id.strip_suffix("[1m]").or_else(|| id.strip_suffix("[1M]")) {
        Some(base) => (base, true),
        None => (id, false),
    };
    let mut parts: Vec<&str> = base.split('-').collect();
    // A dated snapshot ("claude-haiku-4-5-20251001") reads as its model.
    if parts.len() > 3 && parts.last().is_some_and(|part| part.len() == 8 && part.chars().all(|c| c.is_ascii_digit())) {
        parts.pop();
    }
    let number = |part: &&str| !part.is_empty() && part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit());
    let family = match parts.as_slice() {
        ["claude", family, major, rest @ ..]
            if family.chars().all(|c| c.is_ascii_alphabetic())
                && number(major)
                && rest.len() <= 1
                && rest.iter().all(number) =>
        {
            family
        }
        _ => return id.to_string(),
    };
    let mut chars = family.chars();
    let family: String = chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default();
    let version = parts[2..].join(".");
    format!("{family} {version}{}", if long { " (1M)" } else { "" })
}

/// How a round ended: the status ("done", "failed" or "stopped"), why it failed, the tool's last message, and a marker
/// step saying so.
pub fn finish_round(
    db: &Db,
    id: &str,
    status: &str,
    error: Option<&str>,
    last_message: Option<&str>,
    now: i64,
) -> Result<(), String> {
    let marker = match status {
        "done" => "Finished".to_string(),
        "stopped" => "Stopped".to_string(),
        _ => format!("Failed: {}", error.unwrap_or("the tool stopped without saying why")),
    };
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_tasks SET status = ?2, error = ?3, last_message = COALESCE(?4, last_message), rounds = rounds + 1, updated_at = ?5 WHERE id = ?1",
            params![id, status, error, last_message, now],
        )?;
        insert_event(tx, id, now, "marker", Some(if status == "failed" { "error" } else { "status" }), &marker)
    })
    .map(|_| ())
}

/// A handed-off round's turn ended with background work still going (coding_session.rs): the task isn't finished, it
/// waits on that work with the tool kept running. `ends_round` counts the round that just ended (the first time); the
/// marker says what it waits on.
pub fn set_monitoring(
    db: &Db,
    id: &str,
    last_message: Option<&str>,
    marker: &str,
    ends_round: bool,
    now: i64,
) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_tasks SET status = 'monitoring', error = NULL, last_message = COALESCE(?2, last_message), rounds = rounds + ?3, updated_at = ?4 WHERE id = ?1",
            params![id, last_message, ends_round as i64, now],
        )?;
        insert_event(tx, id, now, "marker", Some("monitoring"), marker)
    })
    .map(|_| ())
}

/// A task that waited on background work ended before its tool carried on (stopped, or the tool died): `finish_round`
/// without counting another round, the one it waited after being counted already.
pub fn finish_waiting(db: &Db, id: &str, status: &str, error: Option<&str>, now: i64) -> Result<(), String> {
    let marker = match status {
        "done" => "Finished".to_string(),
        "stopped" => "Stopped".to_string(),
        _ => format!("Failed: {}", error.unwrap_or("the tool stopped without saying why")),
    };
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_tasks SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, status, error, now],
        )?;
        insert_event(tx, id, now, "marker", Some(if status == "failed" { "error" } else { "status" }), &marker)
    })
    .map(|_| ())
}

/// Tasks a closed Filo left working (or waiting): stopped, saying why. Returns how many. A task a conversation handed
/// off that hasn't started yet (coding_handoff.rs: a routine's turn may start one while the app is closed) stays queued,
/// for the app to start. Those in `still_working` (their round's process carried on after Filo quit) are left alone.
pub fn stop_interrupted(db: &Db, now: i64, still_working: &[String]) -> Result<usize, String> {
    let ids: Vec<String> = db.with(|conn| {
        conn.prepare("SELECT id FROM coding_tasks WHERE status IN ('running', 'monitoring') OR (status = 'queued' AND id NOT IN (SELECT task_id FROM coding_handoffs))")?.query_map([], |row| row.get(0))?.collect()
    })?;
    let ids: Vec<String> = ids.into_iter().filter(|id| !still_working.contains(id)).collect();
    db.write(|tx| {
        for id in &ids {
            tx.execute("UPDATE coding_tasks SET status = 'stopped', updated_at = ?2 WHERE id = ?1", params![id, now])?;
            insert_event(tx, id, now, "marker", Some("status"), "Stopped: Filo closed while it was working")?;
        }
        Ok(ids.len())
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Steps

/// One step of a task's history.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub id: i64,
    pub at: i64,
    /// "owner", "tool", "activity" or "marker".
    pub source: String,
    pub kind: Option<String>,
    pub text: String,
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>())
    }
}

fn insert_event(
    tx: &Connection,
    task_id: &str,
    at: i64,
    source: &str,
    kind: Option<&str>,
    text: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO coding_events (task_id, at, source, kind, text) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task_id, at, source, kind, cut(text, MAX_STEP)],
    )?;
    tx.execute("UPDATE coding_tasks SET updated_at = MAX(updated_at, ?2) WHERE id = ?1", params![task_id, at])?;
    Ok(())
}

/// Adds a step; past `MAX_EVENTS` the oldest steps after the brief go.
pub fn add_event(db: &Db, task_id: &str, at: i64, step: &Step) -> Result<(), String> {
    db.write(|tx| {
        insert_event(tx, task_id, at, &step.source, step.kind.as_deref(), &step.text)?;
        // Looked at every so often rather than on every step.
        if tx.last_insert_rowid() % 50 == 0 {
            tx.execute(
                "DELETE FROM coding_events WHERE task_id = ?1 AND id NOT IN (SELECT id FROM coding_events WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2)
                 AND id <> (SELECT MIN(id) FROM coding_events WHERE task_id = ?1)",
                params![task_id, MAX_EVENTS as i64 - 1],
            )?;
        }
        Ok(())
    })
}

pub fn events(db: &Db, task_id: &str) -> Result<Vec<Event>, String> {
    db.with(|conn| {
        conn.prepare("SELECT id, at, source, kind, text FROM coding_events WHERE task_id = ?1 ORDER BY id")?
            .query_map([task_id], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    source: row.get(2)?,
                    kind: row.get(3)?,
                    text: row.get(4)?,
                })
            })?
            .collect()
    })
}

/// The task's latest step not from `source` (the tool's latest, for "owner"), without reading the rest: a chat's task
/// rows show it for each of its tasks every time the chat refreshes.
pub fn latest_event_not_from(db: &Db, task_id: &str, source: &str) -> Result<Option<Event>, String> {
    db.with(|conn| {
        conn.prepare_cached("SELECT id, at, source, kind, text FROM coding_events WHERE task_id = ?1 AND source != ?2 ORDER BY id DESC LIMIT 1")?
            .query_row(params![task_id, source], |row| Ok(Event { id: row.get(0)?, at: row.get(1)?, source: row.get(2)?, kind: row.get(3)?, text: row.get(4)? }))
            .optional()
    })
}

/// A step read from a CLI's output: the tool saying something (`tool`), or doing something (`activity`, with its kind:
/// command, files, plan, search or tool).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub source: String,
    pub kind: Option<String>,
    pub text: String,
}

impl Step {
    pub fn said(text: &str) -> Step {
        Step { source: "tool".into(), kind: None, text: text.trim().to_string() }
    }

    pub fn did(kind: &str, text: impl Into<String>) -> Step {
        Step { source: "activity".into(), kind: Some(kind.into()), text: text.into() }
    }

    pub fn marker(kind: &str, text: impl Into<String>) -> Step {
        Step { source: "marker".into(), kind: Some(kind.into()), text: text.into() }
    }
}

/// The steps in one line of a CLI's JSON output (none for the lines that aren't steps: results, usage, reasoning).
pub fn steps(engine: Engine, line: &str) -> Vec<Step> {
    let Ok(event) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let text = |value: Option<&Value>| {
        value.and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
    };
    match engine {
        Engine::Claude => {
            if event.get("type").and_then(Value::as_str) != Some("assistant") {
                return Vec::new();
            }
            let content = event.pointer("/message/content").and_then(Value::as_array).cloned().unwrap_or_default();
            content
                .iter()
                .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                    Some("text") => text(item.get("text")).map(|said| Step::said(&said)),
                    Some("tool_use") => claude_tool_step(
                        item.get("name").and_then(Value::as_str).unwrap_or("tool"),
                        item.get("input").unwrap_or(&Value::Null),
                    ),
                    _ => None,
                })
                .collect()
        }
        // Their rounds read their own events (coding_session.rs, coding_muse.rs, coding_opencode.rs, coding_pi.rs).
        Engine::Codex | Engine::Muse | Engine::OpenCode | Engine::Pi => Vec::new(),
    }
}

/// Codex runs commands as `/bin/zsh -lc '<command>'`: the command itself reads better.
pub(crate) fn unwrap_shell(command: &str) -> String {
    for shell in ["/bin/zsh -lc ", "/bin/bash -lc ", "bash -lc ", "zsh -lc ", "/bin/sh -c "] {
        if let Some(rest) = command.strip_prefix(shell) {
            let rest = rest.trim();
            let unquoted = rest
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .or_else(|| rest.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')));
            return unquoted.unwrap_or(rest).replace("'\\''", "'");
        }
    }
    command.to_string()
}

fn claude_tool_step(name: &str, input: &Value) -> Option<Step> {
    let field = |key: &str| {
        input.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
    };
    Some(match name {
        "Bash" => Step::did("command", field("command")?),
        "Edit" | "MultiEdit" => Step::did("files", format!("Edited {}", field("file_path")?)),
        "Write" => Step::did("files", format!("Wrote {}", field("file_path")?)),
        "NotebookEdit" => Step::did("files", format!("Edited {}", field("notebook_path")?)),
        "Read" => Step::did("tool", format!("Read {}", field("file_path")?)),
        "Grep" | "Glob" => Step::did("search", field("pattern")?),
        "WebSearch" => Step::did("search", field("query")?),
        "WebFetch" => Step::did("tool", format!("Read {}", field("url")?)),
        "TodoWrite" => {
            let todos: Vec<String> = input
                .get("todos")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|todo| {
                    let done = todo.get("status").and_then(Value::as_str) == Some("completed");
                    Some(format!(
                        "{} {}",
                        if done { "☑" } else { "☐" },
                        todo.get("content").and_then(Value::as_str)?.trim()
                    ))
                })
                .collect();
            if todos.is_empty() {
                return None;
            }
            Step::did("plan", todos.join("\n"))
        }
        "Task" | "Agent" => Step::did(
            "tool",
            field("description").map_or_else(|| "Started a subagent".to_string(), |what| format!("Subagent: {what}")),
        ),
        other => Step::did("tool", other.to_string()),
    })
}

/// Claude Code's permission mode for each of the page's, and for the ones tasks made earlier may still have.
pub(crate) fn claude_mode(mode: &str) -> &'static str {
    match mode {
        "supervised" => "manual",
        "auto" => "auto",
        _ => "acceptEdits",
    }
}

/// The branch a task's worktree gets: `filo/<words of its title>-<a bit of its id>`.
pub fn branch_name(title: &str, id: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let short: String = id.chars().filter(char::is_ascii_alphanumeric).take(6).collect();
    if slug.is_empty() { format!("filo/task-{short}") } else { format!("filo/{slug}-{short}") }
}

/// The page's row for a task (with what this Mac has), with how many of its requests
/// still wait on an answer (coding_handoff.rs `pending_counts`).
pub fn task_json(task: &Task, model: Option<String>, reasoning: String, pending: usize) -> Value {
    let mut row = json!({
        "taskId": task.id,
        "title": task.title,
        "tool": task.tool,
        "status": task.status,
        "owner": task.owner,
        "repoName": task.project_name,
        "repoPath": task.project_path,
        "workPath": task.work_path,
        "runner": "This Mac",
        "worktree": task.worktree,
        "plainFolder": task.plain_folder,
        "reasoning": reasoning,
        "rounds": task.rounds,
        "pending": pending,
        "createdAt": task.created_at,
        "updatedAt": task.updated_at,
    });
    for (key, value) in [("branch", task.branch.clone()), ("model", model), ("error", task.error.clone())] {
        if let Some(value) = value {
            row[key] = json!(value);
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(path: &str, plain: bool) -> Project {
        Project { name: "app".into(), path: path.into(), branch: None, remote: None, plain_folder: plain, added: true }
    }

    #[test]
    fn tool_settings_default_and_persist() {
        let path = crate::db::tests::temp_path("coding-settings");
        let db = Db::open(&path).unwrap();
        let all = tools(&db).unwrap();
        assert_eq!(all.iter().map(|tool| tool.id.as_str()).collect::<Vec<_>>(), TOOLS);
        assert!(all.iter().all(|tool| tool.mode == DEFAULT_MODE && tool.model.is_none()));
        // Claude Code and Codex start plugged in; Muse Code, OpenCode and pi wait for the user to plug them in.
        assert_eq!(all.iter().map(|tool| tool.plugged).collect::<Vec<_>>(), [true, true, false, false, false]);
        assert!(
            update_tool(&db, "opencode", |tool| {
                let _: () = tool.plugged = true;
                Ok(())
            })
            .unwrap()
            .plugged
        );
        assert!(tool(&db, "opencode").unwrap().plugged && !tool(&db, "muse").unwrap().plugged);
        update_tool(&db, "codex", |tool| {
            tool.plugged = false;
            tool.mode = "full_access".into();
            tool.model = Some(" gpt-x ".into());
            tool.effort = Some("high".into());
            Ok(())
        })
        .unwrap();
        assert!(
            update_tool(&db, "codex", |tool| {
                let _: () = tool.mode = "reckless".into();
                Ok(())
            })
            .is_err()
        );
        assert!(
            update_tool(&db, "codex", |tool| {
                let _: () = tool.model = Some("-rm".into());
                Ok(())
            })
            .is_err()
        );
        assert!(tool(&db, "cursor").is_err());
        set_workspace(&db, "checkout").unwrap();
        assert!(set_workspace(&db, "cloud").is_err());
        assert!(!keep_awake(&db).unwrap(), "off until the user turns it on");
        set_keep_awake(&db, true).unwrap();
        assert!(!keep_awake_lid(&db).unwrap(), "off until the user turns it on");
        set_keep_awake_lid(&db, true).unwrap();
        drop(db);
        // Opened again (Filo restarted): the same settings.
        let db = Db::open(&path).unwrap();
        let codex = tool(&db, "codex").unwrap();
        assert_eq!(
            (codex.plugged, codex.mode.as_str(), codex.model.as_deref(), codex.effort.as_deref()),
            (false, "full_access", Some("gpt-x"), Some("high"))
        );
        assert!(tool(&db, "claude-code").unwrap().plugged);
        assert_eq!(workspace(&db).unwrap(), "checkout");
        // A chat turn works as its engine's tool does.
        assert_eq!(
            (turn_mode(&path, Engine::Codex).as_str(), turn_mode(&path, Engine::Claude).as_str()),
            ("full_access", DEFAULT_MODE)
        );
        // A tool saved in a mode the page no longer offers reads as the default, and changes without an error.
        for old in ["supervised", "auto_accept_edits"] {
            db.write(|tx| tx.execute("UPDATE coding_tools SET mode = ?1 WHERE id = 'codex'", [old])).unwrap();
            assert_eq!(tool(&db, "codex").unwrap().mode, DEFAULT_MODE);
            assert_eq!(
                update_tool(&db, "codex", |tool| {
                    let _: () = tool.plugged = true;
                    Ok(())
                })
                .unwrap()
                .mode,
                DEFAULT_MODE
            );
        }
        assert!(keep_awake(&db).unwrap());
        assert!(keep_awake_lid(&db).unwrap());
        // "" goes back to the tool's own.
        update_tool(&db, "codex", |tool| {
            let _: () = tool.model = Some(String::new());
            Ok(())
        })
        .unwrap();
        assert_eq!(tool(&db, "codex").unwrap().model, None);
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn finds_repos_and_keeps_added_plain_folders() {
        let root = crate::test_support::temp_dir("coding-walk");
        let make_repo = |path: &Path, branch: &str| {
            std::fs::create_dir_all(path.join(".git")).unwrap();
            std::fs::write(path.join(".git/HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
            std::fs::write(
                path.join(".git/config"),
                "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:me/app.git\n",
            )
            .unwrap();
        };
        make_repo(&root.join("code/app"), "main");
        make_repo(&root.join("code/app/inner"), "nope");
        make_repo(&root.join("node_modules/dep"), "main");
        make_repo(&root.join(".hidden/secret"), "main");
        make_repo(&root.join("a/b/c/d/e/f/deep"), "main");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let found = find_repos(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            (found[0].name.as_str(), found[0].branch.as_deref(), found[0].remote.as_deref()),
            ("app", Some("main"), Some("git@github.com:me/app.git"))
        );

        let path = crate::db::tests::temp_path("coding-projects");
        let db = Db::open(&path).unwrap();
        assert!(replace_found(&db, &found, 1).unwrap());
        assert!(!replace_found(&db, &found, 2).unwrap());
        let plain = add_project(&db, &root.join("notes"), 3).unwrap();
        assert!(plain.plain_folder && plain.added && plain.branch.is_none());
        assert!(add_project(&db, &root.join("missing"), 3).is_err());
        assert_eq!(projects(&db, None).unwrap().len(), 2);
        assert_eq!(projects(&db, Some("NOT")).unwrap().len(), 1);
        // The plain folder becomes a repo: the next walk sees its branch.
        make_repo(&root.join("notes"), "draft");
        replace_found(&db, &found, 4).unwrap();
        let notes = projects(&db, Some("notes")).unwrap().remove(0);
        assert_eq!((notes.plain_folder, notes.branch.as_deref()), (false, Some("draft")));
        // A repo gone from disk goes from the list; the folder the user added stays until they remove it.
        assert!(replace_found(&db, &[], 5).unwrap());
        assert_eq!(projects(&db, None).unwrap().len(), 1);
        remove_project(&db, &notes.path).unwrap();
        assert!(projects(&db, None).unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_task_keeps_its_history_across_restarts() {
        let path = crate::db::tests::temp_path("coding-tasks");
        let db = Db::open(&path).unwrap();
        update_tool(&db, "claude-code", |tool| {
            let _: () = tool.mode = "auto".into();
            Ok(())
        })
        .unwrap();
        let new = NewTask {
            brief: "  Add a dark mode toggle\n\nIn settings.  ".into(),
            tool: "claude-code".into(),
            project: project("/p/app", true),
            worktree: true,
            owner: "You".into(),
        };
        assert!(create_task(&db, &NewTask { brief: " ".into(), ..new.clone() }, 1).is_err());
        let task = create_task(&db, &new, 10).unwrap();
        assert_eq!(
            (task.title.as_str(), task.status.as_str(), task.mode.as_str()),
            ("Add a dark mode toggle", "queued", "auto")
        );
        // A plain folder never gets a worktree.
        assert!(!task.worktree && task.plain_folder);
        assert_eq!(next_message(&db, &task.id).unwrap(), "Add a dark mode toggle\n\nIn settings.");
        set_running(&db, &task.id, 11).unwrap();
        set_session(&db, &task.id, "s-1").unwrap();
        add_event(&db, &task.id, 12, &Step::did("command", "npm test")).unwrap();
        add_event(&db, &task.id, 13, &Step::said("Done: added the toggle.")).unwrap();
        assert!(follow_up(&db, &task.id, "Also the menu", 14).is_err(), "still running");
        finish_round(&db, &task.id, "done", None, Some("Done: added the toggle."), 15).unwrap();
        follow_up(&db, &task.id, "Also the menu", 16).unwrap();
        assert_eq!(next_message(&db, &task.id).unwrap(), "Also the menu");
        drop(db);

        let db = Db::open(&path).unwrap();
        let again = super::task(&db, &task.id).unwrap().unwrap();
        assert_eq!(
            (again.status.as_str(), again.session.as_deref(), again.rounds, again.last_message.as_deref()),
            ("queued", Some("s-1"), 1, Some("Done: added the toggle."))
        );
        let history: Vec<(String, String)> =
            events(&db, &task.id).unwrap().into_iter().map(|event| (event.source, event.text)).collect();
        assert_eq!(
            history.iter().map(|(source, _)| source.as_str()).collect::<Vec<_>>(),
            ["owner", "activity", "tool", "marker", "owner"]
        );
        assert_eq!(history[3].1, "Finished");
        // Filo closed while it waited: stopped, and it says so.
        assert_eq!(stop_interrupted(&db, 20, &[]).unwrap(), 1);
        assert_eq!(super::task(&db, &task.id).unwrap().unwrap().status, "stopped");
        assert_eq!(events(&db, &task.id).unwrap().last().unwrap().text, "Stopped: Filo closed while it was working");
        assert_eq!(tasks(&db, 10).unwrap().len(), 1);
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn long_histories_keep_the_brief_and_the_newest_steps() {
        let db = Db::in_memory().unwrap();
        let task = create_task(
            &db,
            &NewTask {
                brief: "Brief".into(),
                tool: "codex".into(),
                project: project("/p", false),
                worktree: false,
                owner: "You".into(),
            },
            1,
        )
        .unwrap();
        for at in 0..MAX_EVENTS + 200 {
            add_event(&db, &task.id, at as i64, &Step::said(&format!("step {at}"))).unwrap();
        }
        let all = events(&db, &task.id).unwrap();
        assert!(all.len() <= MAX_EVENTS + 50, "{}", all.len());
        assert_eq!(all[0].text, "Brief");
        assert_eq!(all.last().unwrap().text, format!("step {}", MAX_EVENTS + 199));
    }

    #[test]
    fn keeps_the_model_the_tool_said() {
        let db = Db::in_memory().unwrap();
        let task = create_task(
            &db,
            &NewTask {
                brief: "Brief".into(),
                tool: "claude-code".into(),
                project: project("/p", false),
                worktree: false,
                owner: "You".into(),
            },
            1,
        )
        .unwrap();
        assert_eq!(run_model(&db, &task.id).unwrap(), None);
        let init = json!({ "type": "system", "subtype": "init", "session_id": "s-1", "model": "claude-opus-5-5[1m]" });
        set_run_model(&db, &task.id, &claude_init_model(&init).unwrap()).unwrap();
        set_run_model(&db, &task.id, "claude-opus-5-5").unwrap();
        assert_eq!(run_model(&db, &task.id).unwrap().as_deref(), Some("claude-opus-5-5"));
        assert_eq!(claude_init_model(&json!({ "type": "assistant", "model": "x" })), None);
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-5-5[1m]"), "Sonnet 5.5 (1M)");
        assert_eq!(model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_label("claude-opus-4"), "Opus 4");
        assert_eq!(model_label("gpt-5-codex"), "gpt-5-codex");
    }

    #[test]
    fn reads_claude_codes_steps() {
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"text","text":"Let me look."},
            {"type":"tool_use","name":"Bash","input":{"command":"cargo test"}},
            {"type":"tool_use","name":"Edit","input":{"file_path":"/p/src/main.rs"}},
            {"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"Read","status":"completed"},{"content":"Fix","status":"pending"}]}},
            {"type":"tool_use","name":"Grep","input":{"pattern":"fn main"}},
            {"type":"tool_use","name":"mcp__x__y","input":{}}]}}"#
            .replace('\n', "");
        let steps = steps(Engine::Claude, &line);
        assert_eq!(steps[0], Step::said("Let me look."));
        assert_eq!(steps[1], Step::did("command", "cargo test"));
        assert_eq!(steps[2], Step::did("files", "Edited /p/src/main.rs"));
        assert_eq!(steps[3], Step::did("plan", "☑ Read\n☐ Fix"));
        assert_eq!(steps[4], Step::did("search", "fn main"));
        assert_eq!(steps[5], Step::did("tool", "mcp__x__y"));
        assert!(super::steps(Engine::Claude, r#"{"type":"result","result":"done"}"#).is_empty());
        assert!(super::steps(Engine::Claude, "not json").is_empty());
    }

    #[test]
    fn titles_and_branches() {
        assert_eq!(title_of("\n\n# Make the sidebar compact\nmore"), "Make the sidebar compact");
        assert_eq!(title_of(&"x".repeat(200)).chars().count(), TITLE_CHARS);
        assert_eq!(branch_name("Make the sidebar compact!", "ab12cd-34"), "filo/make-the-sidebar-compact-ab12cd");
        assert_eq!(branch_name("!!!", "ab"), "filo/task-ab");
    }
}

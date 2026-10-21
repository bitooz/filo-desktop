// Coding tasks handed off from a conversation: Filo or an agent, in its own chat or in the agents' group chat, gives
// a piece of work to a coding tool with `start_work` (coding_mcp.rs), and the task belongs to that conversation.
// What the tool asks while it works (a permission, a question) and how each round ends go back there: the app starts a
// turn in the conversation on its own (a "wake", hidden from the chat: store.rs `Message::hidden`), or, for the group,
// queues the member's turn in the group. The answer comes back through the MCP tools (or the user's tap on the card the
// chat shows), and the app hands it to the tool, which waited.
//
// The rows here, in Filo's database (db.rs), beside the Coding page's tables (coding.rs):
//
// - `coding_handoffs`: which conversation a task belongs to (and which agent or group member started it), the brief's
//   parts, whether it only looks, a Stop asked for from another process, and when its current round started.
// - `coding_requests`: what the tool asked, its state (pending, answered, expired), who decided, and the words the
//   user was asked in when it went to them.
// - `coding_notes`: notes for the tool while it works, handed to it at its next step.
// - `coding_rounds`: each round's end: its status, the tool's final message (the round's result page) and the folder
//   where it saved its proof.
// - `coding_wakes`: what the conversation is to be told, kept until the app could start its turn (one turn at a time
//   in a conversation), and which turn told it, so the reply carries the round's Open proof link.
// - `coding_asks`: the turns that asked a task for work (started it, gave it its next round or a note), so the reply
//   reporting a round answers the message that asked (`asking_turns`).
//
// The app (src/local_backend/coding.rs) runs the rounds and the wakes; this module has the rows, the brief, the tool's
// instructions and what each wake says.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension as _, Row, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::coding::{self, Task};
use crate::db::Db;

/// The tables (db.rs `MIGRATIONS`). New tables only, each made if missing, so the entry doesn't depend on where it
/// lands among other pages' entries.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS coding_handoffs (
    task_id          TEXT PRIMARY KEY NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
    chat_id          TEXT NOT NULL,
    agent_id         TEXT,
    speaker          TEXT,
    goal             TEXT NOT NULL,
    done_when        TEXT NOT NULL,
    notes            TEXT,
    look_only        INTEGER NOT NULL DEFAULT 0,
    stop_requested   INTEGER NOT NULL DEFAULT 0,
    round_started_at INTEGER,
    created_at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS coding_handoffs_by_chat ON coding_handoffs (chat_id, created_at);
CREATE TABLE IF NOT EXISTS coding_requests (
    id           TEXT PRIMARY KEY NOT NULL,
    task_id      TEXT NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
    round        INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    title        TEXT NOT NULL,
    detail       TEXT,
    options      TEXT NOT NULL DEFAULT '[]',
    state        TEXT NOT NULL,
    asked_user   INTEGER NOT NULL DEFAULT 0,
    question     TEXT,
    user_options TEXT,
    answer       TEXT,
    decided_by   TEXT,
    created_at   INTEGER NOT NULL,
    asked_at     INTEGER,
    answered_at  INTEGER
);
CREATE INDEX IF NOT EXISTS coding_requests_by_task ON coding_requests (task_id, created_at);
CREATE TABLE IF NOT EXISTS coding_notes (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      TEXT NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
    text         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    delivered_at INTEGER
);
CREATE INDEX IF NOT EXISTS coding_notes_by_task ON coding_notes (task_id, id);
CREATE TABLE IF NOT EXISTS coding_rounds (
    task_id     TEXT NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
    round       INTEGER NOT NULL,
    status      TEXT NOT NULL,
    result      TEXT,
    error       TEXT,
    proof_dir   TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    PRIMARY KEY (task_id, round)
);
CREATE TABLE IF NOT EXISTS coding_wakes (
    id         TEXT PRIMARY KEY NOT NULL,
    task_id    TEXT NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
    chat_id    TEXT NOT NULL,
    speaker    TEXT,
    kind       TEXT NOT NULL,
    round      INTEGER NOT NULL,
    text       TEXT NOT NULL,
    state      TEXT NOT NULL,
    turn_id    TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS coding_wakes_by_state ON coding_wakes (state, created_at);
CREATE INDEX IF NOT EXISTS coding_wakes_by_turn ON coding_wakes (turn_id);
";

/// Rounds a task may run before the user says to keep going.
pub const MAX_ROUNDS: i64 = 5;
/// A note for the tool, at most this long.
const MAX_NOTE: usize = 1_500;
/// An answer to the tool, at most this long.
const MAX_ANSWER: usize = 4_000;
/// A request still undecided this long after it was asked shows in the chat for the user to answer, even if the
/// conversation's agent never handed it over (it may have failed or stayed quiet).
pub const SHOW_UNDECIDED_AFTER_MS: i64 = 90_000;

// ---------------------------------------------------------------------------------------------------------------
// Handoffs

/// Which conversation a task belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Handoff {
    pub task_id: String,
    /// The conversation: a Filo chat, an agent's chat, or the group's conversation.
    pub chat_id: String,
    /// The Agents-tab agent that started it; none: Filo.
    pub agent_id: Option<String>,
    /// In the group: the member that started it (an agent's id), who is woken there.
    pub speaker: Option<String>,
    pub goal: String,
    pub done_when: String,
    pub notes: Option<String>,
    pub look_only: bool,
    pub stop_requested: bool,
    pub round_started_at: Option<i64>,
    pub created_at: i64,
}

const HANDOFF_COLUMNS: &str = "task_id, chat_id, agent_id, speaker, goal, done_when, notes, look_only, stop_requested, round_started_at, created_at";

fn handoff_row(row: &Row) -> rusqlite::Result<Handoff> {
    Ok(Handoff {
        task_id: row.get(0)?,
        chat_id: row.get(1)?,
        agent_id: row.get(2)?,
        speaker: row.get(3)?,
        goal: row.get(4)?,
        done_when: row.get(5)?,
        notes: row.get(6)?,
        look_only: row.get::<_, i64>(7)? != 0,
        stop_requested: row.get::<_, i64>(8)? != 0,
        round_started_at: row.get(9)?,
        created_at: row.get(10)?,
    })
}

/// What a new hand-off is, beside its task.
#[derive(Clone, Debug)]
pub struct NewHandoff {
    pub chat_id: String,
    pub agent_id: Option<String>,
    pub speaker: Option<String>,
    pub title: String,
    pub goal: String,
    pub done_when: String,
    pub notes: Option<String>,
    pub look_only: bool,
    /// The tool's model and reasoning level for this task, when the conversation asked for them (else its settings').
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Saves the hand-off of a task just made (`coding::create_task`, queued): which conversation it belongs to, its title
/// and the model and level asked for. The app picks it up and runs it.
pub fn create(db: &Db, task_id: &str, new: &NewHandoff, now: i64) -> Result<Task, String> {
    let notes = new.notes.as_deref().map(str::trim).filter(|notes| !notes.is_empty());
    db.write(|tx| {
        tx.execute(
            &format!(
                "INSERT INTO coding_handoffs ({HANDOFF_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, NULL, ?9)"
            ),
            params![
                task_id,
                new.chat_id,
                new.agent_id,
                new.speaker,
                new.goal.trim(),
                new.done_when.trim(),
                notes,
                new.look_only as i64,
                now
            ],
        )?;
        tx.execute("UPDATE coding_tasks SET title = ?2 WHERE id = ?1", params![task_id, coding::title_of(&new.title)])?;
        // A model alone keeps the Coding page's level, which its rounds move to the nearest the model takes.
        if new.model.is_some() {
            tx.execute("UPDATE coding_tasks SET model = ?2 WHERE id = ?1", params![task_id, new.model])?;
        }
        if new.effort.is_some() {
            tx.execute("UPDATE coding_tasks SET effort = ?2 WHERE id = ?1", params![task_id, new.effort])?;
        }
        Ok(())
    })?;
    coding::task(db, task_id)?.ok_or_else(|| "The task wasn't saved.".into())
}

pub fn get(db: &Db, task_id: &str) -> Result<Option<Handoff>, String> {
    db.with(|conn| {
        conn.query_row(
            &format!("SELECT {HANDOFF_COLUMNS} FROM coding_handoffs WHERE task_id = ?1"),
            [task_id],
            handoff_row,
        )
        .optional()
    })
}

/// A conversation's tasks, oldest first, with their hand-offs.
pub fn for_chat(db: &Db, chat_id: &str) -> Result<Vec<(Task, Handoff)>, String> {
    let handoffs: Vec<Handoff> = db.with(|conn| {
        conn.prepare(&format!(
            "SELECT {HANDOFF_COLUMNS} FROM coding_handoffs WHERE chat_id = ?1 ORDER BY created_at, task_id"
        ))?
        .query_map([chat_id], handoff_row)?
        .collect()
    })?;
    let mut tasks = Vec::new();
    for handoff in handoffs {
        if let Some(task) = coding::task(db, &handoff.task_id)? {
            tasks.push((task, handoff));
        }
    }
    Ok(tasks)
}

/// Tasks handed off that aren't finished, with their hand-offs.
pub fn unfinished(db: &Db) -> Result<Vec<(Task, Handoff)>, String> {
    let ids: Vec<String> = db.with(|conn| {
        conn.prepare("SELECT h.task_id FROM coding_handoffs h JOIN coding_tasks t ON t.id = h.task_id WHERE t.status IN ('queued', 'running', 'monitoring') ORDER BY t.created_at")?
            .query_map([], |row| row.get(0))?
            .collect()
    })?;
    let mut tasks = Vec::new();
    for id in ids {
        if let (Some(task), Some(handoff)) = (coding::task(db, &id)?, get(db, &id)?) {
            tasks.push((task, handoff));
        }
    }
    Ok(tasks)
}

/// Tasks handed off that finished (done, failed or stopped) at `since` or later, newest first, with their hand-offs.
pub fn finished_since(db: &Db, since: i64) -> Result<Vec<(Task, Handoff)>, String> {
    let ids: Vec<String> = db.with(|conn| {
        conn.prepare("SELECT h.task_id FROM coding_handoffs h JOIN coding_tasks t ON t.id = h.task_id WHERE t.status IN ('done', 'failed', 'stopped') AND t.updated_at >= ?1 ORDER BY t.updated_at DESC")?
            .query_map([since], |row| row.get(0))?
            .collect()
    })?;
    let mut tasks = Vec::new();
    for id in ids {
        if let (Some(task), Some(handoff)) = (coding::task(db, &id)?, get(db, &id)?) {
            tasks.push((task, handoff));
        }
    }
    Ok(tasks)
}

/// Stop, asked for from the MCP server (another process): the app sees it and stops the round.
pub fn request_stop(db: &Db, task_id: &str) -> Result<(), String> {
    db.write(|tx| tx.execute("UPDATE coding_handoffs SET stop_requested = 1 WHERE task_id = ?1", [task_id])).map(|_| ())
}

/// The tasks whose Stop was asked for, the asks cleared.
pub fn take_stop_requests(db: &Db) -> Result<Vec<String>, String> {
    db.write(|tx| {
        let ids: Vec<String> = tx
            .prepare("SELECT task_id FROM coding_handoffs WHERE stop_requested = 1")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute("UPDATE coding_handoffs SET stop_requested = 0 WHERE stop_requested = 1", [])?;
        Ok(ids)
    })
}

/// A conversation (or the user) takes over a task started elsewhere (the Coding page): it belongs to that conversation
/// from now on.
pub fn adopt(
    db: &Db,
    task: &Task,
    chat_id: &str,
    agent_id: Option<&str>,
    speaker: Option<&str>,
    now: i64,
) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            &format!("INSERT INTO coding_handoffs ({HANDOFF_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, '', NULL, 0, 0, NULL, ?6) ON CONFLICT (task_id) DO NOTHING"),
            params![task.id, chat_id, agent_id, speaker, task.title, now],
        )
    })
    .map(|_| ())
}

// ---------------------------------------------------------------------------------------------------------------
// Rounds

/// A round's end, for its result page.
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub task_id: String,
    pub round: i64,
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub proof_dir: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

fn round_row(row: &Row) -> rusqlite::Result<Round> {
    Ok(Round {
        task_id: row.get(0)?,
        round: row.get(1)?,
        status: row.get(2)?,
        result: row.get(3)?,
        error: row.get(4)?,
        proof_dir: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
    })
}

const ROUND_COLUMNS: &str = "task_id, round, status, result, error, proof_dir, started_at, finished_at";

/// Where a round's proof goes: `<data>/coding/proof/<task>/round-<n>`.
pub fn proof_dir(data_root: &Path, task_id: &str, round: i64) -> PathBuf {
    data_root.join("coding").join("proof").join(task_id).join(format!("round-{round}"))
}

/// A round starts: its row, and when (the chat's "Working · 4m").
pub fn start_round(db: &Db, task_id: &str, round: i64, proof_dir: &Path, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            &format!("INSERT INTO coding_rounds ({ROUND_COLUMNS}) VALUES (?1, ?2, 'running', NULL, NULL, ?3, ?4, NULL)
             ON CONFLICT (task_id, round) DO UPDATE SET status = 'running', result = NULL, error = NULL, proof_dir = ?3, started_at = ?4, finished_at = NULL"),
            params![task_id, round, proof_dir.display().to_string(), now],
        )?;
        tx.execute("UPDATE coding_handoffs SET round_started_at = ?2 WHERE task_id = ?1", params![task_id, now])
    })
    .map(|_| ())
}

pub fn finish_round(
    db: &Db,
    task_id: &str,
    round: i64,
    status: &str,
    result: Option<&str>,
    error: Option<&str>,
    now: i64,
) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_rounds SET status = ?3, result = ?4, error = ?5, finished_at = ?6 WHERE task_id = ?1 AND round = ?2",
            params![task_id, round, status, result, error, now],
        )
    })
    .map(|_| ())
}

/// A round that ended while its tool waited on background work ("monitoring") gets its real end if the wait ended the
/// task (stopped, failed, or the tool quit): one the tool carried on from stays "monitoring", its follow-up the next
/// round.
pub fn settle_round(db: &Db, task_id: &str, round: i64, status: &str, error: Option<&str>) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_rounds SET status = ?3, error = COALESCE(?4, error) WHERE task_id = ?1 AND round = ?2",
            params![task_id, round, status, error],
        )
    })
    .map(|_| ())
}

pub fn round(db: &Db, task_id: &str, round: i64) -> Result<Option<Round>, String> {
    db.with(|conn| {
        conn.query_row(
            &format!("SELECT {ROUND_COLUMNS} FROM coding_rounds WHERE task_id = ?1 AND round = ?2"),
            params![task_id, round],
            round_row,
        )
        .optional()
    })
}

/// The newest round with an end.
pub fn latest_round(db: &Db, task_id: &str) -> Result<Option<Round>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {ROUND_COLUMNS} FROM coding_rounds WHERE task_id = ?1 AND finished_at IS NOT NULL ORDER BY round DESC LIMIT 1"), [task_id], round_row).optional()
    })
}

/// The task's rounds, oldest first.
fn rounds(db: &Db, task_id: &str) -> Result<Vec<Round>, String> {
    db.with(|conn| {
        conn.prepare_cached(&format!("SELECT {ROUND_COLUMNS} FROM coding_rounds WHERE task_id = ?1 ORDER BY round"))?
            .query_map([task_id], round_row)?
            .collect()
    })
}

/// How long an ended round worked, in milliseconds: its start to its end, less the time what the tool asked (`requests`,
/// the task's) waited on the user: from when the chat showed it to them (`shows_in_chat`: handed to them, or left
/// undecided a while) to its answer or the round's end. None for a round still under way. The result page, the chat's
/// task rows and cards and the Coding page all read this (`worked_ms`), so they say the same time.
pub fn round_worked_ms(round: &Round, requests: &[Request]) -> Option<i64> {
    let (start, end) = (round.started_at, round.finished_at?);
    let mut waits: Vec<(i64, i64)> = requests
        .iter()
        .filter_map(|request| {
            let shown = request.created_at + SHOW_UNDECIDED_AFTER_MS;
            let shown = request.asked_at.filter(|_| request.asked_user).map_or(shown, |asked| asked.min(shown));
            let (from, to) = (shown.max(start), request.answered_at.unwrap_or(end).min(end));
            (from < to).then_some((from, to))
        })
        .collect();
    waits.sort_unstable();
    // Questions open at once count their shared stretch once.
    let (mut waited, mut reached) = (0, start);
    for (from, to) in waits {
        let from = from.max(reached);
        if to > from {
            waited += to - from;
            reached = to;
        }
    }
    Some((end - start - waited).max(0))
}

/// How long a task has worked, in milliseconds: its ended rounds' `round_worked_ms` added up (the time between rounds,
/// waiting on a follow-up or on background work, isn't). None while no round has ended, or for a task from before
/// rounds were recorded.
pub fn worked_ms(db: &Db, task_id: &str) -> Result<Option<i64>, String> {
    let requests = requests_for_task(db, task_id)?;
    let worked: Vec<i64> = rounds(db, task_id)?.iter().filter_map(|round| round_worked_ms(round, &requests)).collect();
    Ok((!worked.is_empty()).then(|| worked.iter().sum()))
}

/// When an unfinished task's "Working …" counts from, the same in the Agents list, the pet's tray and the chat's side
/// panel: the start of the round that took its current request (the brief, or the conversation's latest follow-up),
/// so a follow-up starts at 0, while a round that only carries on that request (the tool picking up after background
/// work) keeps counting from it, the wait included. coding_rounds doesn't say why a round started: it took a new
/// request when one of the owner's steps that isn't a note (`coding::follow_up`'s message, or the brief) came after the
/// round before it started; notes go to the tool mid-round, and a carried-on round has none. "Done · …" (`worked_ms`)
/// still counts every round. The round's start (`round_started_at`), else the task's creation, for a task without its
/// rounds recorded.
pub fn working_since(db: &Db, task: &Task, owner: &Handoff) -> Result<i64, String> {
    let rounds = rounds(db, &task.id)?;
    if rounds.is_empty() {
        return Ok(owner.round_started_at.unwrap_or(task.created_at));
    }
    let asked: Vec<i64> = db.with(|conn| {
        conn.prepare_cached("SELECT at FROM coding_events WHERE task_id = ?1 AND source = 'owner' AND kind IS NULL")?
            .query_map([&task.id], |row| row.get(0))?
            .collect()
    })?;
    let request = rounds
        .windows(2)
        .rposition(|pair| asked.iter().any(|at| *at > pair[0].started_at && *at <= pair[1].started_at))
        .map_or(0, |before| before + 1);
    Ok(rounds[request].started_at)
}

/// A file the tool saved in a round's proof folder.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProofFile {
    pub name: String,
    pub path: PathBuf,
    /// "image", "video" or "file".
    pub kind: &'static str,
    pub content_type: &'static str,
    pub size: u64,
}

/// The proof a round saved itself. A round whose turn ended waiting on background work shares its folder with the
/// round that carried on (the tool was told one folder), so each lists only what was saved in its time: one that ended
/// waiting (or was carried on from) what was there by its end, one that carried on what came after the round before
/// it ended (the background work's output included, which it reports on). A folder of its own lists all its files.
pub fn round_proof(db: &Db, round: &Round) -> Result<Vec<ProofFile>, String> {
    let sharing: Vec<(i64, Option<i64>)> = db.with(|conn| {
        conn.prepare(
            "SELECT round, finished_at FROM coding_rounds WHERE task_id = ?1 AND proof_dir = ?2 AND round != ?3",
        )?
        .query_map(params![round.task_id, round.proof_dir, round.round], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
    })?;
    // Saved after the round before it in the folder ended…
    let after = sharing
        .iter()
        .filter(|(other, _)| *other < round.round)
        .max_by_key(|(other, _)| *other)
        .map(|(_, ended)| ended.unwrap_or(i64::MAX));
    // …and by its own end, when it ended waiting or a later round carried on from it.
    let carried_on = round.status == "monitoring" || sharing.iter().any(|(other, _)| *other > round.round);
    let until = round.finished_at.filter(|_| carried_on);
    let files = proof_files(Path::new(&round.proof_dir));
    if after.is_none() && until.is_none() {
        return Ok(files);
    }
    Ok(files
        .into_iter()
        .filter(|file| {
            let Some(saved) = saved_at(&file.path) else { return false };
            after.is_none_or(|after| saved > after) && until.is_none_or(|until| saved <= until)
        })
        .collect())
}

/// When a file was last written, in ms since the epoch.
fn saved_at(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// The files in a proof folder, by name (its own folders and hidden files left out).
pub fn proof_files(dir: &Path) -> Vec<ProofFile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<ProofFile> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if name.starts_with('.') {
                return None;
            }
            let (kind, content_type) = proof_kind(&name);
            Some(ProofFile {
                size: entry.metadata().map_or(0, |meta| meta.len()),
                path: entry.path(),
                name,
                kind,
                content_type,
            })
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

/// A proof file's kind (a picture the page shows, a video, else a file) and type, by its name (attachments.rs's table).
fn proof_kind(name: &str) -> (&'static str, &'static str) {
    let content_type = crate::attachments::content_type_for(Path::new(name));
    let kind = if crate::attachments::is_image(content_type) {
        "image"
    } else if content_type.starts_with("video/") {
        "video"
    } else {
        "file"
    };
    (kind, content_type)
}

// ---------------------------------------------------------------------------------------------------------------
// Requests: what the tool asks

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: String,
    pub task_id: String,
    pub round: i64,
    /// "approval" or "question".
    pub kind: String,
    /// What the tool asks, in a line ("Run `npm install`").
    pub title: String,
    pub detail: Option<String>,
    pub options: Vec<String>,
    /// "pending", "answered" or "expired".
    pub state: String,
    /// Handed to the user (ask_user_about_coding_request): the card shows `question` and `user_options`.
    pub asked_user: bool,
    pub question: Option<String>,
    pub user_options: Vec<String>,
    pub answer: Option<String>,
    /// "agent" (the conversation's Filo or agent) or "user".
    pub decided_by: Option<String>,
    pub created_at: i64,
    pub asked_at: Option<i64>,
    pub answered_at: Option<i64>,
    /// What options mean, by label: the tool's own descriptions, and those the agent gave handing it to the user.
    pub option_notes: BTreeMap<String, String>,
}

const REQUEST_COLUMNS: &str = "id, task_id, round, kind, title, detail, options, state, asked_user, question, user_options, answer, decided_by, created_at, asked_at, answered_at, option_notes";

/// `SCHEMA`'s tables (in case another page's migration took its slot on a development Mac), then the columns and
/// tables added beside it (db.rs runs this on every open), so a Filo from before them still opens the file.
pub fn ensure_columns(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    let has: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('coding_requests') WHERE name = 'option_notes')",
        [],
        |row| row.get(0),
    )?;
    if !has {
        conn.execute_batch("ALTER TABLE coding_requests ADD COLUMN option_notes TEXT")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS coding_asks (
            task_id TEXT NOT NULL REFERENCES coding_tasks (id) ON DELETE CASCADE,
            turn_id TEXT NOT NULL,
            at      INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS coding_asks_by_task ON coding_asks (task_id, at);",
    )
}

/// What `options` mean (`notes`, by place; empty for none), added to what request `id` has. Labels without a note,
/// or not among the options, are left out.
pub fn add_option_notes(db: &Db, id: &str, options: &[String], notes: &[String]) -> Result<(), String> {
    let Some(current) = request(db, id)? else { return Ok(()) };
    let mut merged = current.option_notes;
    for (option, note) in options.iter().zip(notes) {
        let (option, note) = (option.trim(), note.trim());
        if !option.is_empty() && !note.is_empty() {
            merged.insert(cut(option, 120), cut(note, 300));
        }
    }
    db.write(|tx| {
        tx.execute("UPDATE coding_requests SET option_notes = ?2 WHERE id = ?1", params![id, json!(merged).to_string()])
    })?;
    Ok(())
}

fn list_of(text: Option<String>) -> Vec<String> {
    text.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn request_row(row: &Row) -> rusqlite::Result<Request> {
    Ok(Request {
        id: row.get(0)?,
        task_id: row.get(1)?,
        round: row.get(2)?,
        kind: row.get(3)?,
        title: row.get(4)?,
        detail: row.get(5)?,
        options: list_of(row.get(6)?),
        state: row.get(7)?,
        asked_user: row.get::<_, i64>(8)? != 0,
        question: row.get(9)?,
        user_options: list_of(row.get(10)?),
        answer: row.get(11)?,
        decided_by: row.get(12)?,
        created_at: row.get(13)?,
        asked_at: row.get(14)?,
        answered_at: row.get(15)?,
        option_notes: row
            .get::<_, Option<String>>(16)?
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default(),
    })
}

/// The tool asks something: saved pending. Returns its id.
pub fn add_request(
    db: &Db,
    task_id: &str,
    round: i64,
    kind: &str,
    title: &str,
    detail: Option<&str>,
    options: &[String],
    now: i64,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let title = cut(title.trim(), 1_000);
    let detail = detail.map(|detail| cut(detail.trim(), 2_000)).filter(|detail| !detail.is_empty());
    db.write(|tx| {
        tx.execute(
            &format!("INSERT INTO coding_requests ({REQUEST_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, NULL, NULL, NULL, NULL, ?8, NULL, NULL, NULL)"),
            params![id, task_id, round, kind, title, detail, json!(options).to_string(), now],
        )
    })?;
    Ok(id)
}

pub fn request(db: &Db, id: &str) -> Result<Option<Request>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {REQUEST_COLUMNS} FROM coding_requests WHERE id = ?1"), [id], request_row)
            .optional()
    })
}

pub fn requests_for_task(db: &Db, task_id: &str) -> Result<Vec<Request>, String> {
    db.with(|conn| {
        conn.prepare_cached(&format!(
            "SELECT {REQUEST_COLUMNS} FROM coding_requests WHERE task_id = ?1 ORDER BY created_at, id"
        ))?
        .query_map([task_id], request_row)?
        .collect()
    })
}

/// How many requests each task still waits on an answer to (the Coding page's "N questions"), by task id.
pub fn pending_counts(db: &Db) -> Result<HashMap<String, usize>, String> {
    db.with(|conn| {
        conn.prepare("SELECT task_id, COUNT(*) FROM coding_requests WHERE state = 'pending' GROUP BY task_id")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize)))?
            .collect()
    })
}

/// The requests of a conversation's tasks, oldest first.
pub fn requests_for_chat(db: &Db, chat_id: &str) -> Result<Vec<Request>, String> {
    db.with(|conn| {
        conn.prepare(&format!(
            "SELECT {} FROM coding_requests r JOIN coding_handoffs h ON h.task_id = r.task_id WHERE h.chat_id = ?1 ORDER BY r.created_at, r.id",
            REQUEST_COLUMNS.split(", ").map(|column| format!("r.{column}")).collect::<Vec<_>>().join(", ")
        ))?
        .query_map([chat_id], request_row)?
        .collect()
    })
}

/// The answer an approval takes: allow, allow_always or deny, from the words an agent or a button might use.
pub fn approval_answer(answer: &str) -> Option<&'static str> {
    match answer.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
        "allow" | "allowed" | "yes" | "approve" | "accept" | "ok" => Some("allow"),
        "allow_always" | "always" | "allow_for_session" | "always_allow" => Some("allow_always"),
        "deny" | "denied" | "no" | "decline" | "reject" => Some("deny"),
        _ => None,
    }
}

/// Answers a pending request: the tool gets it at its next look. `by` is "agent" or "user". An approval takes allow,
/// allow_always or deny; a question any short text.
pub fn answer(db: &Db, id: &str, answer: &str, by: &str, now: i64) -> Result<Request, String> {
    let current = request(db, id)?.ok_or("There's no such request (it may belong to a task that's gone).")?;
    if current.state != "pending" {
        return Err(match current.state.as_str() {
            "answered" => format!("It was already answered ({}).", current.answer.unwrap_or_default()),
            _ => "It closed without an answer: the round it belonged to ended.".into(),
        });
    }
    let answer = answer.trim();
    let answer = if current.kind == "approval" {
        approval_answer(answer).ok_or("For a permission request answer allow, allow_always or deny.")?.to_string()
    } else if answer.is_empty() {
        return Err("Give the answer.".into());
    } else {
        cut(answer, MAX_ANSWER)
    };
    let changed = db.write(|tx| {
        tx.execute(
            "UPDATE coding_requests SET state = 'answered', answer = ?2, decided_by = ?3, answered_at = ?4 WHERE id = ?1 AND state = 'pending'",
            params![id, answer, by, now],
        )
    })?;
    if changed == 0 {
        return Err("It was answered a moment ago.".into());
    }
    request(db, id)?.ok_or_else(|| "There's no such request.".into())
}

/// Hands a pending request to the user: the chat's card asks it in these words, with these options.
pub fn ask_user(db: &Db, id: &str, question: &str, options: &[String], now: i64) -> Result<Request, String> {
    let current = request(db, id)?.ok_or("There's no such request.")?;
    if current.state != "pending" {
        return Err("It's no longer waiting for an answer.".into());
    }
    let question = question.trim();
    if question.is_empty() {
        return Err("Say what to ask the user.".into());
    }
    let options: Vec<String> =
        options.iter().map(|option| cut(option.trim(), 120)).filter(|option| !option.is_empty()).take(4).collect();
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_requests SET asked_user = 1, question = ?2, user_options = ?3, asked_at = ?4 WHERE id = ?1",
            params![id, cut(question, 1_000), json!(options).to_string(), now],
        )
    })?;
    request(db, id)?.ok_or_else(|| "There's no such request.".into())
}

/// Filo opened after it closed mid-round: the requests of tasks that ended close without an answer, and their rounds
/// read as stopped.
pub fn expire_finished(db: &Db, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        let finished = "SELECT id FROM coding_tasks WHERE status IN ('done', 'failed', 'stopped')";
        tx.execute(&format!("UPDATE coding_rounds SET status = 'stopped', finished_at = ?1 WHERE finished_at IS NULL AND task_id IN ({finished})"), [now])?;
        // A round the tool carried on from (a later round follows it) stays "monitoring".
        tx.execute(
            &format!(
                "UPDATE coding_rounds SET status = 'stopped' WHERE status = 'monitoring' AND task_id IN ({finished})
                 AND NOT EXISTS (SELECT 1 FROM coding_rounds later WHERE later.task_id = coding_rounds.task_id AND later.round > coding_rounds.round)"
            ),
            [],
        )?;
        tx.execute(&format!("UPDATE coding_requests SET state = 'expired', answered_at = ?1 WHERE state = 'pending' AND task_id IN ({finished})"), [now])
    })
}

/// A round ended: what it still asked closes without an answer. Returns how many.
pub fn expire_pending(db: &Db, task_id: &str, now: i64) -> Result<usize, String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE coding_requests SET state = 'expired', answered_at = ?2 WHERE task_id = ?1 AND state = 'pending'",
            params![task_id, now],
        )
    })
}

/// Whether the chat shows the request: once it's decided, handed to the user, or left undecided a while.
pub fn shows_in_chat(request: &Request, now: i64) -> bool {
    request.state != "pending" || request.asked_user || now - request.created_at >= SHOW_UNDECIDED_AFTER_MS
}

// ---------------------------------------------------------------------------------------------------------------
// Notes for the tool while it works

pub fn add_note(db: &Db, task_id: &str, text: &str, now: i64) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Say what to tell it.".into());
    }
    if text.chars().count() > MAX_NOTE {
        return Err(format!("Keep a note under {MAX_NOTE} characters: a sentence or two."));
    }
    db.write(|tx| {
        tx.execute(
            "INSERT INTO coding_notes (task_id, text, created_at) VALUES (?1, ?2, ?3)",
            params![task_id, text, now],
        )?;
        // In the thread too, as the owner's words.
        tx.execute(
            "INSERT INTO coding_events (task_id, at, source, kind, text) VALUES (?1, ?2, 'owner', 'note', ?3)",
            params![task_id, now, text],
        )
    })
    .map(|_| ())
}

/// Notes not handed to the tool yet, oldest first, with their ids.
pub fn undelivered_notes(db: &Db, task_id: &str) -> Result<Vec<(i64, String)>, String> {
    db.with(|conn| {
        conn.prepare("SELECT id, text FROM coding_notes WHERE task_id = ?1 AND delivered_at IS NULL ORDER BY id")?
            .query_map([task_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    })
}

pub fn mark_delivered(db: &Db, ids: &[i64], now: i64) -> Result<(), String> {
    db.write(|tx| {
        for id in ids {
            tx.execute("UPDATE coding_notes SET delivered_at = ?2 WHERE id = ?1", params![id, now])?;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Wakes: what the conversation is told

#[derive(Clone, Debug, PartialEq)]
pub struct Wake {
    pub id: String,
    pub task_id: String,
    pub chat_id: String,
    pub speaker: Option<String>,
    /// "question", "round" (finished, with the tool's report), "monitoring" (its turn ended with background work still
    /// going, the task not finished) or "failed".
    pub kind: String,
    pub round: i64,
    pub text: String,
    /// "pending" until a turn took it, then "sent".
    pub state: String,
    pub turn_id: Option<String>,
    pub created_at: i64,
}

const WAKE_COLUMNS: &str = "id, task_id, chat_id, speaker, kind, round, text, state, turn_id, created_at";

fn wake_row(row: &Row) -> rusqlite::Result<Wake> {
    Ok(Wake {
        id: row.get(0)?,
        task_id: row.get(1)?,
        chat_id: row.get(2)?,
        speaker: row.get(3)?,
        kind: row.get(4)?,
        round: row.get(5)?,
        text: row.get(6)?,
        state: row.get(7)?,
        turn_id: row.get(8)?,
        created_at: row.get(9)?,
    })
}

pub fn add_wake(db: &Db, handoff: &Handoff, kind: &str, round: i64, text: &str, now: i64) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    db.write(|tx| {
        tx.execute(
            &format!(
                "INSERT INTO coding_wakes ({WAKE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', NULL, ?8)"
            ),
            params![id, handoff.task_id, handoff.chat_id, handoff.speaker, kind, round, text, now],
        )
    })?;
    Ok(id)
}

/// The wake `id`.
pub fn wake(db: &Db, id: &str) -> Result<Option<Wake>, String> {
    db.with(|conn| {
        conn.query_row(&format!("SELECT {WAKE_COLUMNS} FROM coding_wakes WHERE id = ?1"), [id], wake_row).optional()
    })
}

/// Wakes no turn has taken yet, oldest first.
pub fn pending_wakes(db: &Db) -> Result<Vec<Wake>, String> {
    db.with(|conn| {
        conn.prepare(&format!(
            "SELECT {WAKE_COLUMNS} FROM coding_wakes WHERE state = 'pending' ORDER BY created_at, id"
        ))?
        .query_map([], wake_row)?
        .collect()
    })
}

/// These wakes went with this turn.
pub fn mark_sent(db: &Db, ids: &[String], turn_id: &str) -> Result<(), String> {
    db.write(|tx| {
        for id in ids {
            tx.execute("UPDATE coding_wakes SET state = 'sent', turn_id = ?2 WHERE id = ?1", params![id, turn_id])?;
        }
        Ok(())
    })
}

/// The round a turn reported on, when it was woken for a round that finished (or ended its turn with background work
/// still going): what its reply's Open proof opens.
pub fn reported_round(db: &Db, turn_id: &str) -> Result<Option<(String, i64)>, String> {
    db.with(|conn| {
        conn.query_row("SELECT task_id, round FROM coding_wakes WHERE turn_id = ?1 AND kind IN ('round', 'monitoring') ORDER BY created_at DESC LIMIT 1", [turn_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
    })
}

/// Turn `turn_id` asked task `task_id` for work: started it, gave it its next round or a note.
pub fn add_ask(db: &Db, task_id: &str, turn_id: &str, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("INSERT INTO coding_asks (task_id, turn_id, at) VALUES (?1, ?2, ?3)", params![task_id, turn_id, now])
            .map(|_| ())
    })
}

/// When turn `turn_id` reports how a task's round ended (it finished, failed or waits on background work): the turns
/// that asked that task for work, latest first, for its reply to answer the latest one its conversation shows. None
/// for any other turn.
pub fn asking_turns(db: &Db, turn_id: &str) -> Result<Option<Vec<String>>, String> {
    let wakes: Vec<Wake> = db.with(|conn| {
        conn.prepare(&format!("SELECT {WAKE_COLUMNS} FROM coding_wakes WHERE turn_id = ?1"))?
            .query_map([turn_id], wake_row)?
            .collect()
    })?;
    asks_reported(db, &wakes)
}

/// `asking_turns` for the turn telling `wakes`: none of them when they report on several tasks.
pub fn asks_reported(db: &Db, wakes: &[Wake]) -> Result<Option<Vec<String>>, String> {
    let mut tasks: Vec<&str> = wakes
        .iter()
        .filter(|wake| matches!(wake.kind.as_str(), "round" | "monitoring" | "failed"))
        .map(|wake| wake.task_id.as_str())
        .collect();
    tasks.dedup();
    match tasks.as_slice() {
        [] => Ok(None),
        [task] => db
            .with(|conn| {
                conn.prepare("SELECT turn_id FROM coding_asks WHERE task_id = ?1 ORDER BY at DESC, rowid DESC")?
                    .query_map([task], |row| row.get(0))?
                    .collect()
            })
            .map(Some),
        _ => Ok(Some(Vec::new())),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The brief

/// Words models capitalise to shout an order; in capitals they're emphasis, not names or code.
const SHOUTED: [&str; 34] = [
    "NOT",
    "NO",
    "NEVER",
    "ALWAYS",
    "ONLY",
    "MUST",
    "DO",
    "DON'T",
    "DONT",
    "ALL",
    "EVERY",
    "EVERYTHING",
    "FIRST",
    "IMPORTANT",
    "CRITICAL",
    "EXACTLY",
    "BOTH",
    "NOW",
    "STOP",
    "REQUIRED",
    "MANDATORY",
    "READ-ONLY",
    "SEPARATE",
    "EXISTING",
    "CURRENT",
    "LATEST",
    "ANY",
    "NONE",
    "WITHOUT",
    "BEFORE",
    "AFTER",
    "ENTIRE",
    "WHOLE",
    "STRICTLY",
];

/// The capitalised orders in `text`, outside code spans, once each.
fn shouted_words(text: &str) -> Vec<String> {
    let mut outside = String::new();
    let mut in_code = false;
    for c in text.chars() {
        if c == '`' {
            in_code = !in_code;
            outside.push(' ');
        } else if !in_code {
            outside.push(c);
        }
    }
    let mut found: Vec<String> = Vec::new();
    for word in outside.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-')) {
        let word = word.trim_matches(|c| c == '\'' || c == '-');
        if SHOUTED.contains(&word) && !found.iter().any(|seen| seen == word) {
            found.push(word.to_string());
        }
    }
    found
}

/// Rules a tool already has (its workspace, git, permissions, how it proves its work), restated in notes.
fn process_rule(notes: &str) -> Option<String> {
    let lower = notes.to_lowercase();
    let mut rules: Vec<String> = Vec::new();
    for lead in ["no ", "don't ", "dont ", "do not ", "never ", "without "] {
        for verb in ["commit", "push", "deploy"] {
            rules.push(format!("{lead}{verb}"));
        }
    }
    rules.extend(
        [
            "worktree",
            "other session",
            "another session",
            "other coding task",
            "dirty file",
            "dirty tree",
            "independent review",
        ]
        .map(str::to_string),
    );
    rules.into_iter().find(|rule| lower.contains(rule.as_str()))
}

/// What a brief that can't go out is missing, as a sentence the conversation can fix it with.
pub fn brief_problem(title: &str, goal: &str, done_when: &str, notes: Option<&str>) -> Option<String> {
    let shouted = shouted_words(&[title, goal, done_when, notes.unwrap_or_default()].join("\n"));
    if !shouted.is_empty() {
        return Some(format!(
            "Write the hand-off in sentence case, without capitalised words for emphasis (found: {}). Capitals make tools over-react; say why something matters instead.",
            shouted.join(", ")
        ));
    }
    if let Some(rule) = notes.and_then(process_rule) {
        return Some(format!(
            "Leave rules like \"{rule}\" out of notes: the tool has its own for its workspace, git, permissions and checks. Notes are for what it can't learn otherwise, in a line or two (where the problem shows, what must stay as it is), or nothing."
        ));
    }
    None
}

/// The brief as the tool reads it.
pub fn render_brief(goal: &str, done_when: &str, notes: Option<&str>) -> String {
    [
        format!("Goal: {}", goal.trim()),
        format!("Done when: {}", done_when.trim()),
        notes.map(str::trim).unwrap_or_default().to_string(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// What the coding tool is told about a task handed off from a conversation, on top of its brief. A page it
/// builds for the user's Artifacts follows Filo's artifacts skill, in `artifacts_skill` (skills.rs), and the
/// conversation publishes it.
pub fn instructions(task: &Task, handoff: &Handoff, owner: &str, proof_dir: &Path, artifacts_skill: &Path) -> String {
    let place = if task.plain_folder {
        format!(
            "in the folder {} (not a git repo: nothing is committed there, and nobody runs `git init`)",
            task.work_path
        )
    } else if task.worktree {
        format!(
            "in a git worktree of {} at {}, on its own branch{}: commit your work there when it's done",
            task.project_name,
            task.work_path,
            task.branch.as_deref().map(|branch| format!(" `{branch}`")).unwrap_or_default()
        )
    } else {
        format!(
            "in the project folder {}, the user's own checkout: other sessions may be working there too, so leave changes you didn't make alone, and don't commit, switch branches or reset anything",
            task.work_path
        )
    };
    let look = if handoff.look_only {
        " This task only looks (investigate, review or research): change nothing in the project."
    } else {
        ""
    };
    format!(
        "You are working on a coding task that {owner}, an assistant in the Filo app on the user's Mac, handed you from a conversation with the user, {place}.{look} \
Questions you ask and permissions you need go to {owner}, who answers from what the user said or asks the user; you wait meanwhile, so ask only when it matters. \
{owner} may add a short note while you work: read it as coming from them. \
When the user's answer also asks for something besides the task (taking a note, a reminder), {owner} sees it and does that in Filo: don't save it in your own memory or notes. \
Stay inside this folder. Don't push, open pull requests or publish anything unless the task says to. \
When the task asks for a page for the user's Artifacts in Filo (a report, a page, a diagram or a small tool), build it as one self-contained HTML file the way Filo's artifacts skill says ({skill}/SKILL.md; start from its template.html), \
keep it at a path in this folder that stays the same from round to round, and give its full path in your summary: {owner} publishes it, and publishing the same file again after a later round saves its next version. \
Check your work in proportion to the change. When a screenshot, a short screen recording or a command's output (.txt or .log) shows the change and you can make it easily, save it in {proof}, named for what it shows (like login-works.png or tests.log); you may show it in your final message by its name, as a README would: ![Login works](login-works.png). \
Never capture the screen for one (`screencapture`, ScreenCaptureKit and the like): macOS then asks the user for Screen Recording, on their screen, every time. Picture an app's window the way the project says (its AGENTS.md or CLAUDE.md), with the app's own screenshot option, or with a headless browser for a web page; when none of those works, go without the screenshot. \
The task stays open while anything you started runs in the background: wait on something with a command that ends when it's over (like `until grep -q done app.log; do sleep 5; done`), not a `tail -f` or a Monitor that never ends, and stop any watcher you started before your final summary. \
End with a short summary: what you changed, how you checked it, what you didn't check, and anything left to do.",
        proof = proof_dir.display(),
        skill = artifacts_skill.display()
    )
}

// ---------------------------------------------------------------------------------------------------------------
// What a wake says

pub fn tool_name(tool: &str) -> &'static str {
    coding::engine_of(tool).map_or("The coding tool", |engine| engine.label())
}

/// Which task a wake is about. The task, not the tool, is what it's about: the user sees the tool on the task's card.
fn task_line(task: &Task) -> String {
    format!(
        "Task \"{}\" in {}{}, run by {} (task {}).",
        task.title,
        task.project_name,
        task.branch.as_deref().map(|branch| format!(" (branch {branch})")).unwrap_or_default(),
        tool_name(&task.tool),
        task.id
    )
}

fn speak(group: bool) -> &'static str {
    if group {
        "This is Filo's app telling you, not a message from anyone in the group. Only send_message reaches the group (and the user): post there only what they need to hear."
    } else {
        "This is Filo's app telling you, not a message from the user, who doesn't see it. Only send_message reaches the user; end the turn without it when there's nothing they need to hear."
    }
}

fn clipped(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        format!("{}… (cut; watch_coding_task shows the rest)", text.chars().take(max).collect::<String>())
    } else {
        text.to_string()
    }
}

/// At most `max` characters of a round's report, its start and its end: it leads with the outcome and ends with what
/// wasn't checked or is left to do, so the middle is what's cut, at line breaks when they're near, with a line there
/// saying how much and where it is (`whole`).
fn clipped_report(text: &str, max: usize, whole: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let marker = |left_out: usize| format!("\n[… {left_out} characters of the middle cut here; {whole} …]\n");
    let keep = max.saturating_sub(marker(chars.len()).chars().count());
    let mut start: String = chars[..keep / 2].iter().collect();
    if let Some(at) = start.rfind('\n').filter(|at| *at >= start.len() * 3 / 4) {
        start.truncate(at);
    }
    let mut end: String = chars[chars.len() - (keep - keep / 2)..].iter().collect();
    if let Some(at) = end.find('\n').filter(|at| *at <= end.len() / 4) {
        end.drain(..=at);
    }
    let left_out = chars.len() - start.chars().count() - end.chars().count();
    format!("{}{}{}", start.trim_end(), marker(left_out), end.trim_start())
}

/// How much of a round's report watch_coding_task gives (`lastMessage`): as much as a wake in the conversation quotes.
const LAST_MESSAGE_MAX: usize = 12_000;

/// A round's report as a wake quotes it, and where the rest of it is when it's cut.
fn quoted_report(text: &str, max: usize) -> String {
    let whole = if max < LAST_MESSAGE_MAX {
        "watch_coding_task's lastMessage has more of it"
    } else {
        "the round's result page has all of it"
    };
    clipped_report(text, max, whole)
}

/// A wake (`wake`, from the part of the report it quotes) with as much of the report as fits: in the group, what's
/// left of a turn's reason (group.rs `REASON_CHARS`, which cuts a longer one at its end) after the rest of the wake, so
/// its end, which says what to do, is never what's cut. The rest is kept short there (`listed`) so the report has room;
/// when it has none, the report is pointed to rather than quoted.
fn with_report(report: &str, max: usize, group: bool, wake: impl Fn(&str) -> String) -> String {
    if !group {
        return wake(&quoted_report(report, max));
    }
    let room = crate::group::REASON_CHARS.saturating_sub(wake("").chars().count());
    let quoted = [quoted_report(report, room), NO_ROOM.to_string()]
        .into_iter()
        .find(|quoted| quoted.chars().count() <= room)
        .unwrap_or_default();
    wake(&quoted)
}

/// What a group's wake says of a report there's no room to quote.
const NO_ROOM: &str = "(too long to quote here; watch_coding_task's lastMessage has it)";

/// Lines for `items` ("- …", each `item` cut at `max` characters): in the group, the first `GROUP_LIST` and how many
/// more there are and where (`rest`), so a wake with many keeps room for the report and its own end.
fn listed(items: &[String], group: bool, max: usize, rest: &str) -> String {
    let shown = if group { GROUP_LIST } else { items.len() };
    let mut lines: Vec<String> = items.iter().take(shown).map(|item| format!("- {}", clipped(item, max))).collect();
    if items.len() > shown {
        lines.push(format!("- and {} more ({rest})", items.len() - shown));
    }
    lines.join("\n")
}

/// How many late notes or saved files a group's wake lists.
const GROUP_LIST: usize = 2;

/// How much of the brief and the tool's final message a wake quotes: less in the group, whose turns take a short
/// reason (group.rs; its report takes the room left, `with_report`).
fn budget(handoff: &Handoff) -> (usize, usize) {
    if handoff.speaker.is_some() { (500, 0) } else { (2_000, LAST_MESSAGE_MAX) }
}

/// Telling the conversation the tool asks something.
pub fn question_message(task: &Task, handoff: &Handoff, request: &Request) -> String {
    let tool = tool_name(&task.tool);
    let group = handoff.speaker.is_some();
    let (brief_max, _) = budget(handoff);
    let asks = if request.kind == "approval" {
        format!(
            "{tool} asks for permission: {}{}",
            request.title,
            request.detail.as_deref().map(|detail| format!("\n{detail}")).unwrap_or_default()
        )
    } else {
        format!(
            "{tool} asks: {}{}",
            request.title,
            request.detail.as_deref().map(|detail| format!("\n{detail}")).unwrap_or_default()
        )
    };
    let options = if request.options.is_empty() {
        String::new()
    } else {
        let described: Vec<String> = request
            .options
            .iter()
            .map(|option| match request.option_notes.get(option) {
                Some(note) => format!("{option} ({note})"),
                None => option.clone(),
            })
            .collect();
        format!("\nOptions: {}", described.join(" / "))
    };
    let how = if request.kind == "approval" {
        format!(
            "Answer with answer_coding_request (requestId {}, answer allow, allow_always or deny). Allow what fits what the user asked for; deny what goes beyond it (merging, deleting, touching other projects, anything risky they didn't ask for).",
            request.id
        )
    } else {
        format!(
            "Answer with answer_coding_request (requestId {}) when what you know settles it: the user's words, your memory, this conversation. Answer like a coworker would, in a sentence or two.",
            request.id
        )
    };
    [
        format!("[Coding task question] {} {}", task_line(task), speak(group)),
        format!("The brief was:\n{}", clipped(&task.brief, brief_max)),
        format!("{asks}{options}"),
        how,
        format!(
            "If only the user can decide, call ask_user_about_coding_request (requestId {}) with the question in plain words and, for a question, two to four short answers to pick from: the user sees it in this conversation as a card with answer buttons, and their tap answers the tool directly. Then send one short line with send_message saying the tool needs their decision (don't repeat the question or the options: the card shows them). If they reply in words instead, answer the request yourself with their answer. Don't mention request ids to them. {} waits until it's answered.",
            request.id, tool
        ),
    ]
    .join("\n\n")
}

/// What a question the user skipped on its card tells the tool (the chat's question panel sends it).
pub const SKIPPED: &str = "No preference: go with what you think is best.";

/// Whether the user answered a question on its card in their own words rather than picking one of its answers or
/// skipping it: words that may ask the conversation for something too ("leave it for later, take a note").
pub fn answered_in_words(request: &Request) -> bool {
    let Some(answer) = request.answer.as_deref().map(str::trim) else { return false };
    request.kind == "question"
        && request.decided_by.as_deref() == Some("user")
        && answer != SKIPPED
        && !request.options.iter().chain(&request.user_options).any(|option| option.trim() == answer)
}

/// Telling the conversation what the user typed on a question's card: the tool has it as their answer, and anything
/// else it asks for (a note, a reminder) is the conversation's to do. Without Notes on (pages.rs), it says nothing of
/// notes.
pub fn typed_answer_message(task: &Task, handoff: &Handoff, request: &Request, notes: bool) -> String {
    let tool = tool_name(&task.tool);
    let group = handoff.speaker.is_some();
    let (examples, note) = if notes {
        ("take a note, set a reminder, tell someone", " a note goes on Filo's Notes tab with create_note.")
    } else {
        ("set a reminder, tell someone", "")
    };
    [
        format!("[Coding task question answered by the user] {} {}", task_line(task), speak(group)),
        format!("{tool} asked: {}", request.question.as_deref().unwrap_or(&request.title)),
        format!("The user typed this answer on the card, and it has gone to {tool} as their answer:\n{}", request.answer.as_deref().unwrap_or_default()),
        format!(
            "{tool} only works on the task. If the user's words also ask for something else ({examples}), it's yours to do now with your own tools:{note} \
Then say in one short line with send_message what you did. If they only answer the question, end the turn without sending anything."
        ),
    ]
    .join("\n\n")
}

/// Telling the conversation a round ended, with the tool's report of it.
///
/// How it asks for the message to the user follows Anthropic's prompting guide for current Claude models
/// (<https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices>):
/// - It says why the message is short (the result page has the full report) rather than setting caps and bans:
///   Claude generalizes from the reason ("Add context to improve performance", #add-context-to-improve-performance).
/// - It frames the report as the work's own account, which the conversation tells as plain news, and says why it can:
///   the task's card shows which tool did it and its result page keeps the commit. Told it was "as the tool reported
///   it… which you haven't checked yourself", agents passed it on as "Claude Code says it's fixed (`926bc63`, not
///   pushed)", and a chat's earlier replies in that style pull a new one back to it; the reason (it reads as doubt and
///   repeats the card), not a ban on those words, is what moves them off it. It claims no more than the report does:
///   whatever checks it ran are done, and what it didn't check or do is passed on when it matters to the user. A round
///   that ended without a report is said to have, and the conversation looks at the task's thread before telling.
/// - A long report is cut in the middle (`clipped_report`): what it didn't check or do comes at its end.
/// - It says what to write, and where the details go, rather than what not to write ("Tell Claude what to do
///   instead of what not to do", #control-the-format-of-responses), in plain prose like the message it wants.
/// - It asks for brevity outright, with room for more when the user asked for it, since some models run long by
///   default and effort doesn't shorten replies
///   (<https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/prompting-claude-opus-5#response-length-and-verbosity>).
pub fn round_message(
    task: &Task,
    handoff: &Handoff,
    round: &Round,
    proof: &[ProofFile],
    missed_notes: &[String],
) -> String {
    let group = handoff.speaker.is_some();
    let (brief_max, result_max) = budget(handoff);
    // A round that ended without a word gives nothing to pass on: what the work did is in its thread.
    let report = round.result.as_deref().map(str::trim).filter(|report| !report.is_empty());
    let proof_lines = if proof.is_empty() && report.is_none() {
        "It saved no screenshots, recordings or logs this round.".to_string()
    } else if proof.is_empty() {
        "It saved no screenshots, recordings or logs this round. That's fine: the report stands on its own.".to_string()
    } else {
        let files: Vec<String> = proof.iter().map(|file| format!("{} ({})", file.name, file.kind)).collect();
        format!(
            "Files it saved this round (the result page shows them):\n{}",
            listed(&files, group, 200, "on the result page")
        )
    };
    let missed = if missed_notes.is_empty() {
        String::new()
    } else {
        let max = if group { 200 } else { 500 };
        format!(
            "It finished before reading these notes of yours:\n{}",
            listed(missed_notes, group, max, "the task's thread in watch_coding_task has them")
        )
    };
    let last = if round.round >= MAX_ROUNDS {
        format!(" This was round {}, the most it runs without the user's yes to keep going.", round.round)
    } else {
        String::new()
    };
    let who = if group { "the group" } else { "the user" };
    let tell = format!(
        "Tell {who} with send_message what changed, as plain facts about their project, the way you'd tell a coworker about a change that was just made. \
Who did it is already on the task's card in this conversation, and your message's Open proof button opens the round's result page, with the full report, the files \
and where the work was committed; so the message can be about the work itself, and leave commits, branches and push state to that page unless {who} asked for them or \
has something to do with them. Passed on as what the tool says or reports, news of work it checked reads to them as doubt and only repeats the card; earlier messages in this \
conversation may have told such news that way, and plain news reads better to them. After the outcome, say what only they can decide or do (a choice it made that they need to settle, \
a related problem it left alone) and any part of the brief it didn't do, and pass on a gap in its checks when the gap changes what they'd do next: something they'll only see \
after restarting the app, part of the brief it couldn't try, a check the brief needed failing. Gaps that change nothing for them can stay on the result page. \
Most of the time that's a short paragraph of plain sentences. When {who} asked in this conversation for the details, or there are several things for them to decide, write as much as that takes."
    );
    let what_to_do = match report {
        Some(_) => format!(
            "The round ended normally, and this report is how the work tells you what it did: what it changed, what it checked and what it didn't, and what it left \
undone, as a careful coworker would account for it, so you can pass it on as such. {tell} \
Whatever checks it ran are done, so the work doesn't go back to repeat them or for screenshots or other proof. Send it back with tell_coding_task only when the report itself shows \
the brief isn't met (part of the work wasn't done, or a check the brief needed failed) and the fix is clearly what the user wants; otherwise let the user decide.{last}"
        ),
        None => format!(
            "The round ended normally but without a report: the tool said nothing at the end, so there's nothing here to pass on as what it did. Before telling {who} \
anything, look with watch_coding_task: its thread shows what the work did, step by step (its lastMessage may be an earlier round's). Then tell {who} with send_message \
that the round ended without a report and what the thread shows was done, and not done, in plain words about their project; when it shows nothing done, say so. \
Send it back with tell_coding_task only when the thread shows part of the brief wasn't done and the fix is clearly what the user wants; otherwise let the user decide.{last}"
        ),
    };
    let header = format!("[Coding task round {} done] {} {}", round.round, task_line(task), speak(group));
    let brief = format!("The brief was:\n{}", clipped(&task.brief, brief_max));
    with_report(report.unwrap_or_default(), result_max, group, |quoted| {
        let quoted = report.map(|_| format!("The round's report:\n{quoted}")).unwrap_or_default();
        [header.as_str(), &brief, &quoted, &proof_lines, &missed, &what_to_do]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

/// Telling the conversation a round's turn ended with background work still going: the task isn't done, and it'll hear
/// the tool's follow-up once that work ends.
pub fn monitoring_message(task: &Task, handoff: &Handoff, round: &Round, waiting_on: &[String]) -> String {
    let tool = tool_name(&task.tool);
    let group = handoff.speaker.is_some();
    let (_, result_max) = budget(handoff);
    let who = if group { "the group" } else { "the user" };
    let what = if waiting_on.is_empty() {
        "background work".to_string()
    } else {
        waiting_on.iter().map(|what| clipped(what, 200)).collect::<Vec<_>>().join("; ")
    };
    let header = format!(
        "[Coding task round {}: still running background work] {} {}",
        round.round,
        task_line(task),
        speak(group)
    );
    let running = format!("Still running in the background: {what}");
    let what_to_do = format!(
        "The task isn't done and hasn't failed: Filo keeps {tool} running, and the task shows as Monitoring until that work ends. Then {tool} picks the task back up by itself, and you'll be told how that goes as its next round. \
Don't follow it up, stop it or redo its work meanwhile. Tell {who} in one short message with send_message what's still running, in plain words about their project (the build, the tests); the task's card shows who's running it."
    );
    // A turn that ended without a word has nothing to quote.
    let said = round.result.as_deref().map(str::trim).filter(|said| !said.is_empty());
    with_report(said.unwrap_or_default(), result_max, group, |quoted| {
        let quoted = said.map(|_| format!("What it said as its turn ended:\n{quoted}")).unwrap_or_default();
        [header.as_str(), &quoted, &running, &what_to_do]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

/// Telling the conversation a round failed. It asks for the message the way [`round_message`] does, after the same guide.
pub fn failed_message(task: &Task, handoff: &Handoff, round: i64, error: &str) -> String {
    let group = handoff.speaker.is_some();
    let who = if group { "the group" } else { "the user" };
    [
        format!("[Coding task round {round} failed] {} {}", task_line(task), speak(group)),
        format!("Why: {}", clipped(error, 2_000)),
        format!(
            "It isn't done. Tell {who} plainly with send_message, the way you'd tell a coworker: that it didn't finish and why, in your own words rather than the error's, \
and what would fix it if that's theirs to do (signing the tool in, its usage limit, trying again). A short message usually covers it; the error text, logs and paths are for you \
unless they ask to see them. If the failure looks passing, you may send it back once with tell_coding_task."
        ),
    ]
    .join("\n\n")
}

// ---------------------------------------------------------------------------------------------------------------
// What the conversation's tools and the chat read

/// A task as watch_coding_task and the tools' replies give it.
pub fn status_json(db: &Db, task: &Task, handoff: Option<&Handoff>, recent: usize) -> Result<Value, String> {
    let events = coding::events(db, &task.id)?;
    let now = events.iter().rev().find(|event| event.source != "owner").map(|event| clipped(&event.text, 300));
    let thread: Vec<Value> = events
        .iter()
        .rev()
        .take(recent)
        .rev()
        .map(|event| json!({ "from": event.source, "kind": event.kind, "text": clipped(&event.text, 1_500), "at": event.at }))
        .collect();
    let open: Vec<Value> = requests_for_task(db, &task.id)?
        .into_iter()
        .filter(|request| request.state == "pending")
        .map(|request| json!({ "requestId": request.id, "kind": request.kind, "asks": request.title, "detail": request.detail, "options": request.options, "askedUser": request.asked_user }))
        .collect();
    let mut row = json!({
        "taskId": task.id,
        "title": task.title,
        "tool": task.tool,
        "status": task.status,
        "project": task.project_name,
        "projectPath": task.project_path,
        "workPath": task.work_path,
        "rounds": task.rounds,
        "openQuestions": open,
        "thread": thread,
    });
    for (key, value) in [
        ("branch", task.branch.clone()),
        ("model", task.model.clone()),
        ("error", task.error.clone()),
        ("now", now),
        (
            "lastMessage",
            task.last_message
                .as_deref()
                .map(|text| clipped_report(text, LAST_MESSAGE_MAX, "the round's result page has all of it")),
        ),
    ] {
        if let Some(value) = value {
            row[key] = json!(value);
        }
    }
    if let Some(handoff) = handoff {
        row["onlyLooks"] = json!(handoff.look_only);
        if handoff.round_started_at.is_some() && task.status == "running" {
            row["roundStartedAt"] = json!(working_since(db, task, handoff)?);
        }
    }
    if let Some(round) = latest_round(db, &task.id)? {
        let files: Vec<String> = round_proof(db, &round)?.into_iter().map(|file| file.name).collect();
        if !files.is_empty() {
            row["proof"] = json!(files);
        }
    }
    Ok(row)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::{NewTask, Project};

    fn new_handoff(chat: &str) -> NewHandoff {
        NewHandoff {
            chat_id: chat.into(),
            agent_id: None,
            speaker: None,
            title: "Wrap table text".into(),
            goal: "Tables wrap long text".into(),
            done_when: "A long cell wraps".into(),
            notes: None,
            look_only: false,
            model: Some("haiku".into()),
            effort: Some("low".into()),
        }
    }

    pub(crate) fn task_with_handoff(db: &Db, chat: &str) -> (Task, Handoff) {
        let project = Project {
            name: "app".into(),
            path: "/p/app".into(),
            branch: None,
            remote: None,
            plain_folder: true,
            added: true,
        };
        let brief = render_brief("Tables wrap long text", "A long cell wraps", None);
        let task = coding::create_task(
            db,
            &NewTask { brief, tool: "claude-code".into(), project, worktree: true, owner: "Filo".into() },
            1,
        )
        .unwrap();
        let task = create(db, &task.id, &new_handoff(chat), 2).unwrap();
        let handoff = get(db, &task.id).unwrap().unwrap();
        (task, handoff)
    }

    #[test]
    fn a_task_another_process_ends_reads_ended_after_the_chats_rows_read_it() {
        let path = crate::db::tests::temp_path("handoff-snapshot");
        let (app, worker) = (Db::open(&path).unwrap(), Db::open(&path).unwrap());
        let (task, owner) = task_with_handoff(&app, "chat-1");
        start_round(&app, &task.id, 1, Path::new("/p"), 10).unwrap();
        coding::set_running(&app, &task.id, 10).unwrap();
        coding::add_event(&app, &task.id, 11, &coding::Step::said("Working on it")).unwrap();
        // What the chat's rows read while it runs (local_backend/handoff.rs `chat_sessions`).
        let read = |db: &Db| {
            let (task, _) = for_chat(db, "chat-1").unwrap().pop().unwrap();
            (
                task.status,
                requests_for_task(db, &task.id).unwrap().len(),
                coding::latest_event_not_from(db, &task.id, "owner").unwrap().map(|event| event.text),
                worked_ms(db, &task.id).unwrap(),
                unfinished(db).unwrap().len(),
            )
        };
        assert_eq!(read(&app).0, "running");
        assert_eq!(working_since(&app, &task, &owner).unwrap(), 10);
        // The round's worker ends it.
        finish_round(&worker, &task.id, 1, "done", Some("Done"), None, 20).unwrap();
        coding::finish_round(&worker, &task.id, "done", None, Some("Done"), 20).unwrap();
        coding::add_event(&worker, &task.id, 21, &coding::Step::said("All done")).unwrap();
        assert_eq!(read(&app), ("done".to_string(), 0, Some("All done".to_string()), Some(10), 0));
        drop((app, worker));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_handoff_belongs_to_its_conversation() {
        let db = Db::in_memory().unwrap();
        let (task, handoff) = task_with_handoff(&db, "chat-1");
        assert_eq!(
            (task.title.as_str(), task.model.as_deref(), task.effort.as_deref(), task.status.as_str()),
            ("Wrap table text", Some("haiku"), Some("low"), "queued")
        );
        assert_eq!(handoff.chat_id, "chat-1");
        // A model alone keeps the level the task got from the Coding page.
        coding::update_tool(&db, "claude-code", |tool| {
            tool.effort = Some("high".into());
            Ok(())
        })
        .unwrap();
        let brief = render_brief("Tables wrap long text", "A long cell wraps", None);
        let paged = coding::create_task(
            &db,
            &NewTask {
                brief,
                tool: "claude-code".into(),
                project: Project {
                    name: "app".into(),
                    path: "/p/app".into(),
                    branch: None,
                    remote: None,
                    plain_folder: true,
                    added: true,
                },
                worktree: false,
                owner: "Filo".into(),
            },
            3,
        )
        .unwrap();
        let only_model = NewHandoff { model: Some("opus".into()), effort: None, ..new_handoff("chat-1") };
        let paged = create(&db, &paged.id, &only_model, 3).unwrap();
        assert_eq!((paged.model.as_deref(), paged.effort.as_deref()), (Some("opus"), Some("high")));
        assert_eq!(for_chat(&db, "chat-1").unwrap().len(), 2);
        assert!(for_chat(&db, "chat-2").unwrap().is_empty());
        assert_eq!(unfinished(&db).unwrap().len(), 2);
        request_stop(&db, &task.id).unwrap();
        assert_eq!(take_stop_requests(&db).unwrap(), [task.id.clone()]);
        assert!(take_stop_requests(&db).unwrap().is_empty());
    }

    #[test]
    fn a_tasks_time_leaves_out_waiting_on_the_user_and_between_rounds() {
        let db = Db::in_memory().unwrap();
        let (task, _) = task_with_handoff(&db, "chat-1");
        const MIN: i64 = 60_000;
        assert_eq!(worked_ms(&db, &task.id).unwrap(), None);
        // Round 1, 0 to 20 min: the agent answers a permission at once (that counts as work), the user is asked a
        // question at 5 min and answers at 9, and a request left undecided from 12 min shows to the user after 90 s
        // and is answered at 15, while a second one asked of the user at 14 is open until 16.
        start_round(&db, &task.id, 1, Path::new("/p"), 0).unwrap();
        let quick = add_request(&db, &task.id, 1, "approval", "Run tests", None, &[], MIN).unwrap();
        answer(&db, &quick, "allow", "agent", MIN + 20_000).unwrap();
        let asked = add_request(&db, &task.id, 1, "question", "Which one?", None, &[], 4 * MIN).unwrap();
        ask_user(&db, &asked, "Which one?", &[], 5 * MIN).unwrap();
        answer(&db, &asked, "The first", "user", 9 * MIN).unwrap();
        let undecided = add_request(&db, &task.id, 1, "approval", "Push", None, &[], 12 * MIN).unwrap();
        let overlapping = add_request(&db, &task.id, 1, "question", "Push too?", None, &[], 13 * MIN).unwrap();
        ask_user(&db, &overlapping, "Push too?", &[], 14 * MIN).unwrap();
        answer(&db, &undecided, "deny", "user", 15 * MIN).unwrap();
        answer(&db, &overlapping, "No", "user", 16 * MIN).unwrap();
        finish_round(&db, &task.id, 1, "done", Some("Done"), None, 20 * MIN).unwrap();
        let waited = 4 * MIN + (16 * MIN - (12 * MIN + SHOW_UNDECIDED_AFTER_MS));
        let first = round(&db, &task.id, 1).unwrap().unwrap();
        assert_eq!(round_worked_ms(&first, &requests_for_task(&db, &task.id).unwrap()), Some(20 * MIN - waited));
        // Round 2 an hour later, 3 min, its question still open at its end: the hour between doesn't count, nor the open
        // question's wait, nor a round still under way.
        start_round(&db, &task.id, 2, Path::new("/p"), 80 * MIN).unwrap();
        let open = add_request(&db, &task.id, 2, "question", "Ok?", None, &[], 81 * MIN).unwrap();
        ask_user(&db, &open, "Ok?", &[], 82 * MIN).unwrap();
        finish_round(&db, &task.id, 2, "done", Some("Done"), None, 83 * MIN).unwrap();
        start_round(&db, &task.id, 3, Path::new("/p"), 90 * MIN).unwrap();
        assert_eq!(worked_ms(&db, &task.id).unwrap(), Some(20 * MIN - waited + 2 * MIN));
    }

    #[test]
    fn working_counts_from_the_current_request_and_carries_on_through_its_rounds() {
        let db = Db::in_memory().unwrap();
        let (task, owner) = task_with_handoff(&db, "chat-1");
        const MIN: i64 = 60_000;
        let since = || working_since(&db, &task, &get(&db, &task.id).unwrap().unwrap()).unwrap();
        // Queued, no round yet: from its creation.
        assert_eq!(working_since(&db, &task, &owner).unwrap(), task.created_at);
        // The brief's round from 10 min works an hour, then waits on background work (Monitoring): the time goes on
        // through the wait.
        start_round(&db, &task.id, 1, Path::new("/p"), 10 * MIN).unwrap();
        coding::set_running(&db, &task.id, 10 * MIN).unwrap();
        assert_eq!(since(), 10 * MIN);
        finish_round(&db, &task.id, 1, "monitoring", Some("Waiting on the tests"), None, 70 * MIN).unwrap();
        assert_eq!(since(), 10 * MIN);
        // The tool picks up at 75 min, round 2: the same request, so "Working 1h 8m" at 78 min, not "Working 3m".
        start_round(&db, &task.id, 2, Path::new("/p"), 75 * MIN).unwrap();
        assert_eq!(since(), 10 * MIN);
        finish_round(&db, &task.id, 2, "done", Some("Done"), None, 78 * MIN).unwrap();
        coding::finish_round(&db, &task.id, "done", None, Some("Done"), 78 * MIN).unwrap();
        // The agent follows up at 100 min: its round starts at 0.
        coding::follow_up(&db, &task.id, "Also the menu", 100 * MIN).unwrap();
        start_round(&db, &task.id, 3, Path::new("/p"), 100 * MIN).unwrap();
        assert_eq!(since(), 100 * MIN);
        // A note mid-round, then background work and the tool carrying on: still the follow-up's time.
        add_note(&db, &task.id, "The user wants it blue", 105 * MIN).unwrap();
        finish_round(&db, &task.id, 3, "monitoring", Some("Waiting"), None, 110 * MIN).unwrap();
        start_round(&db, &task.id, 4, Path::new("/p"), 115 * MIN).unwrap();
        assert_eq!(since(), 100 * MIN);
        // "Done · …" still counts every round: 60 + 3 + 10 min worked, and round 4's 5.
        finish_round(&db, &task.id, 4, "done", Some("Done"), None, 120 * MIN).unwrap();
        assert_eq!(worked_ms(&db, &task.id).unwrap(), Some(78 * MIN));
    }

    #[test]
    fn an_answer_in_the_users_own_words_is_told_to_the_conversation() {
        let db = Db::in_memory().unwrap();
        let (task, owner) = task_with_handoff(&db, "chat-1");
        let question = |words: &str, by: &str| {
            let id = add_request(&db, &task.id, 1, "question", "Fix the band now?", None, &["Now".into()], 1).unwrap();
            ask_user(&db, &id, "Fix the menu band now?", &["Fix it now".into(), "Leave it".into()], 2).unwrap();
            answer(&db, &id, words, by, 3).unwrap()
        };
        assert!(answered_in_words(&question("leave it for later, take a note", "user")));
        assert!(
            !answered_in_words(&question("Leave it", "user"))
                && !answered_in_words(&question("Now", "user"))
                && !answered_in_words(&question(SKIPPED, "user"))
        );
        assert!(!answered_in_words(&question("leave it for later, take a note", "agent")));
        let typed = question("leave it for later, take a note", "user");
        let text = typed_answer_message(&task, &owner, &typed, true);
        assert!(
            text.starts_with("[Coding task question answered by the user]") && text.contains("Fix the menu band now?"),
            "{text}"
        );
        assert!(text.contains("leave it for later, take a note") && text.contains("create_note"), "{text}");
        assert!(text.contains("your own tools: a note goes on Filo's Notes tab with create_note. Then say"), "{text}");
        // Notes off: nothing about notes.
        let without = typed_answer_message(&task, &owner, &typed, false);
        assert!(
            !without.contains("create_note")
                && without
                    .contains("(set a reminder, tell someone), it's yours to do now with your own tools: Then say"),
            "{without}"
        );
        // The tool is told notes are Filo's to keep, not its own memory's.
        let told =
            instructions(&task, &owner, "Filo", Path::new("/tmp/proof"), Path::new("/w/.claude/skills/artifacts"));
        assert!(told.contains("don't save it in your own memory"));
        // A page for Artifacts: built from Filo's skill, at a path that stays, for the conversation to publish.
        assert!(
            told.contains("the way Filo's artifacts skill says (/w/.claude/skills/artifacts/SKILL.md")
                && told.contains("Filo publishes it"),
            "{told}"
        );
    }

    #[test]
    fn requests_are_answered_once_and_expire_with_their_round() {
        let db = Db::in_memory().unwrap();
        let (task, _) = task_with_handoff(&db, "chat-1");
        let approval =
            add_request(&db, &task.id, 1, "approval", "Run `npm install`", Some("needs the network"), &[], 10).unwrap();
        let question =
            add_request(&db, &task.id, 1, "question", "Which color?", None, &["Blue".into(), "Green".into()], 11)
                .unwrap();
        assert!(answer(&db, &approval, "maybe", "agent", 12).is_err());
        assert_eq!(
            answer(&db, &approval, "Allow always", "agent", 12).unwrap().answer.as_deref(),
            Some("allow_always")
        );
        assert!(answer(&db, &approval, "deny", "user", 13).unwrap_err().contains("already answered"));
        let asked = ask_user(
            &db,
            &question,
            "Which color should the button be?",
            &["Blue".into(), " ".into(), "Green".into()],
            14,
        )
        .unwrap();
        assert!(asked.asked_user && asked.user_options == ["Blue", "Green"]);
        let pending = request(&db, &question).unwrap().unwrap();
        assert!(shows_in_chat(&pending, 15));
        assert_eq!(requests_for_chat(&db, "chat-1").unwrap().len(), 2);
        // The Coding page counts what's still waiting: the question, not the approval answered.
        assert_eq!(pending_counts(&db).unwrap(), HashMap::from([(task.id.clone(), 1)]));
        assert_eq!(expire_pending(&db, &task.id, 20).unwrap(), 1);
        assert!(pending_counts(&db).unwrap().is_empty());
        assert!(answer(&db, &question, "Blue", "user", 21).unwrap_err().contains("closed"));
        // An undecided one shows once it has waited a while.
        let late = add_request(&db, &task.id, 2, "question", "Port?", None, &[], 100).unwrap();
        let late = request(&db, &late).unwrap().unwrap();
        assert!(!shows_in_chat(&late, 101) && shows_in_chat(&late, 100 + SHOW_UNDECIDED_AFTER_MS));
    }

    #[test]
    fn notes_rounds_and_wakes() {
        let db = Db::in_memory().unwrap();
        let (task, handoff) = task_with_handoff(&db, "chat-1");
        add_note(&db, &task.id, "Keep the header as it is", 5).unwrap();
        assert!(add_note(&db, &task.id, &"x".repeat(MAX_NOTE + 1), 5).is_err());
        let notes = undelivered_notes(&db, &task.id).unwrap();
        assert_eq!(notes.len(), 1);
        mark_delivered(&db, &[notes[0].0], 6).unwrap();
        assert!(undelivered_notes(&db, &task.id).unwrap().is_empty());
        assert_eq!(coding::events(&db, &task.id).unwrap().last().unwrap().kind.as_deref(), Some("note"));

        let dir = crate::test_support::temp_dir("proof");
        let proof = proof_dir(&dir, &task.id, 1);
        std::fs::create_dir_all(&proof).unwrap();
        std::fs::write(proof.join("tests.log"), "ok").unwrap();
        std::fs::write(proof.join("after.png"), [0u8; 4]).unwrap();
        std::fs::write(proof.join(".DS_Store"), "").unwrap();
        start_round(&db, &task.id, 1, &proof, 7).unwrap();
        assert_eq!(get(&db, &task.id).unwrap().unwrap().round_started_at, Some(7));
        finish_round(&db, &task.id, 1, "done", Some("Wrapped them. ![After](after.png)"), None, 9).unwrap();
        let round = latest_round(&db, &task.id).unwrap().unwrap();
        let files = proof_files(Path::new(&round.proof_dir));
        assert_eq!(
            files.iter().map(|file| (file.name.as_str(), file.kind)).collect::<Vec<_>>(),
            [("after.png", "image"), ("tests.log", "file")]
        );

        let text = round_message(&task, &handoff, &round, &files, &["Also the footer".into()]);
        assert!(
            text.contains("[Coding task round 1 done]")
                && text.contains("after.png (image)")
                && text.contains("Also the footer")
                && text.contains("doesn't see it")
        );
        // The message is short because the result page has the full report, and fuller when the user asked for detail:
        // the prompt gives the reason, not caps or bans.
        assert!(
            text.contains("full report")
                && text.contains("short paragraph")
                && text.contains("asked in this conversation for the details"),
            "{text}"
        );
        // The work is told as plain news, with the reason it can be (the card shows the tool, the result page the
        // commit), not framed as the tool's unchecked claim, which agents passed on as "Claude Code says…".
        assert!(
            text.contains("plain facts")
                && text.contains("task's card in this conversation")
                && text.contains("where the work was committed"),
            "{text}"
        );
        for framing in ["reported complete", "reported it", "haven't checked", "is working on"] {
            assert!(!text.contains(framing), "{framing}: {text}");
        }
        // It claims no more checking than the report does, and asks for what wasn't done along with the gaps.
        assert!(
            text.contains("Whatever checks it ran are done")
                && !text.contains("The checks are done")
                && text.contains("any part of the brief it didn't do"),
            "{text}"
        );
        assert!(
            text.contains("a change that was just made") && text.contains("plain news reads better to them"),
            "{text}"
        );
        // Who doesn't go back is the work, not the conversation's agent.
        assert!(
            text.contains("so the work doesn't go back to repeat them") && !text.contains("so it doesn't go back"),
            "{text}"
        );
        let failed = failed_message(&task, &handoff, 1, "usage limit reached");
        assert!(failed.contains("short message usually") && failed.contains("usage limit reached"), "{failed}");
        for prompt in [&text, &failed] {
            for rule in
                ["sentences:", "four short", "three short", "one caveat only", "no headings", "Don't paste", "at most"]
            {
                assert!(!prompt.contains(rule), "{rule}: {prompt}");
            }
        }
        let wake = add_wake(&db, &handoff, "round", 1, &text, 10).unwrap();
        assert_eq!(
            pending_wakes(&db).unwrap().iter().map(|wake| wake.task_id.as_str()).collect::<Vec<_>>(),
            [task.id.as_str()]
        );
        mark_sent(&db, &[wake], "turn-9").unwrap();
        assert!(pending_wakes(&db).unwrap().is_empty());
        assert_eq!(reported_round(&db, "turn-9").unwrap(), Some((task.id.clone(), 1)));
        assert_eq!(reported_round(&db, "turn-x").unwrap(), None);
        let status = status_json(&db, &task, Some(&handoff), 5).unwrap();
        assert_eq!(status["proof"], json!(["after.png", "tests.log"]));
    }

    /// A round that ended without a word isn't told as done work: the conversation looks at the task's thread first.
    #[test]
    fn a_round_without_a_report_is_looked_at_before_its_told() {
        let db = Db::in_memory().unwrap();
        let (task, handoff) = task_with_handoff(&db, "chat-1");
        let dir = crate::test_support::temp_dir("no-report");
        start_round(&db, &task.id, 1, &proof_dir(&dir, &task.id, 1), 7).unwrap();
        for said in [None, Some("  \n")] {
            finish_round(&db, &task.id, 1, "done", said, None, 9).unwrap();
            let text = round_message(&task, &handoff, &latest_round(&db, &task.id).unwrap().unwrap(), &[], &[]);
            assert!(
                text.contains("without a report")
                    && text.contains("look with watch_coding_task")
                    && text.contains("when it shows nothing done, say so"),
                "{text}"
            );
            for claim in ["this report is how", "The round's report", "stands on its own", "(nothing)"] {
                assert!(!text.contains(claim), "{claim}: {text}");
            }
        }
    }

    /// A long report keeps its start and its end, where what wasn't checked or done is, in the group's short wake too;
    /// the group's wake fits in the reason a group turn keeps (group.rs, 4,000 characters).
    #[test]
    fn a_long_report_is_cut_in_the_middle() {
        let db = Db::in_memory().unwrap();
        let (mut task, handoff) = task_with_handoff(&db, "chat-1");
        task.brief = "b".repeat(3_000);
        let dir = crate::test_support::temp_dir("long-report");
        start_round(&db, &task.id, 1, &proof_dir(&dir, &task.id, 1), 7).unwrap();
        let steps: String = (0..600).map(|step| format!("- step {step}: changed a file\n")).collect();
        let report = format!("Tables wrap long text now.\n{steps}What I didn't check: the app after a restart.");
        finish_round(&db, &task.id, 1, "done", Some(&report), None, 9).unwrap();
        let round = latest_round(&db, &task.id).unwrap().unwrap();
        let group = Handoff { speaker: Some("filo".into()), ..handoff.clone() };
        for (owner, max, whole) in [
            (&handoff, 12_000, "the round's result page has all of it"),
            (&group, 4_000, "watch_coding_task's lastMessage has more of it"),
        ] {
            let text = round_message(&task, owner, &round, &[], &[]);
            let quoted = text.split("The round's report:\n").nth(1).unwrap().split("\n\nIt saved no").next().unwrap();
            assert!(
                quoted.starts_with("Tables wrap long text now.\n- step 0: changed a file\n")
                    && quoted.ends_with("\nWhat I didn't check: the app after a restart."),
                "{quoted}"
            );
            assert!(
                quoted.contains(&format!("characters of the middle cut here; {whole} …]\n- step"))
                    && quoted.chars().count() <= max,
                "{quoted}"
            );
            // Cut at a line's end.
            assert!(
                quoted.lines().all(|line| line.starts_with("- step") && line.ends_with("changed a file")
                    || !line.starts_with("- step")),
                "{quoted}"
            );
        }
        let wake = round_message(&task, &group, &round, &[], &[]);
        assert!(wake.chars().count() <= 4_000, "{} {wake}", wake.chars().count());
        // What watch_coding_task gives keeps its end too.
        let long = format!("Done.\n{}\nNot checked: the restart.", "x".repeat(20_000));
        let cut = clipped_report(&long, LAST_MESSAGE_MAX, "the round's result page has all of it");
        assert!(
            cut.starts_with("Done.\n")
                && cut.ends_with("\nNot checked: the restart.")
                && cut.chars().count() <= LAST_MESSAGE_MAX,
            "{cut}"
        );
        assert_eq!(clipped_report("short", 10, "-"), "short");
    }

    /// With many late notes and saved files, the group's wake lists a few of each, quotes the report's start and end
    /// in the room left, and keeps its own closing instructions within the reason a group turn keeps (4,000 characters).
    #[test]
    fn a_group_wake_keeps_its_end_with_many_notes_and_files() {
        let db = Db::in_memory().unwrap();
        let (mut task, handoff) = task_with_handoff(&db, "chat-1");
        task.brief = "b".repeat(3_000);
        task.title = "A task whose title runs on for quite a while, as some of them do".into();
        let dir = crate::test_support::temp_dir("group-wake");
        start_round(&db, &task.id, 1, &proof_dir(&dir, &task.id, 1), 7).unwrap();
        let report = format!(
            "Tables wrap long text now.\n{}What I didn't check: the app after a restart.",
            "- changed a file\n".repeat(600)
        );
        finish_round(&db, &task.id, 1, "done", Some(&report), None, 9).unwrap();
        let round = latest_round(&db, &task.id).unwrap().unwrap();
        let group = Handoff { speaker: Some("filo".into()), ..handoff.clone() };
        let notes: Vec<String> = (0..12).map(|n| format!("Note {n}: {}", "keep the header ".repeat(90))).collect();
        let file = |n: usize| ProofFile {
            name: format!("screenshot-of-the-table-{n}.png"),
            path: dir.join("x.png"),
            kind: "image",
            content_type: "image/png",
            size: 1,
        };
        let files: Vec<ProofFile> = (0..40).map(file).collect();
        for (notes, files) in
            [(&notes[..1], &files[..0]), (&notes[..], &files[..0]), (&notes[..0], &files[..]), (&notes[..], &files[..])]
        {
            let wake = round_message(&task, &group, &round, files, notes);
            assert!(wake.chars().count() <= crate::group::REASON_CHARS, "{} {wake}", wake.chars().count());
            assert!(wake.ends_with("otherwise let the user decide."), "{wake}");
            assert!(
                wake.contains("Tables wrap long text now.")
                    && wake.contains("What I didn't check: the app after a restart."),
                "{wake}"
            );
            if notes.len() > 2 {
                assert!(
                    wake.contains("- Note 1: ")
                        && !wake.contains("Note 2:")
                        && wake.contains("- and 10 more (the task's thread in watch_coding_task has them)"),
                    "{wake}"
                );
            }
            if files.len() > 2 {
                assert!(
                    wake.contains("screenshot-of-the-table-1.png (image)")
                        && wake.contains("- and 38 more (on the result page)")
                        && !wake.contains("table-2.png"),
                    "{wake}"
                );
            }
            // The report keeps 350 characters or more however many notes and files (726 with one note).
            let quoted = wake
                .split("The round's report:\n")
                .nth(1)
                .unwrap()
                .split("\n\nIt saved")
                .next()
                .unwrap()
                .split("\n\nFiles it saved")
                .next()
                .unwrap();
            assert!(quoted.chars().count() >= 350, "{quoted}");
        }
        // In the user's own chat, all of them.
        let wake = round_message(&task, &handoff, &round, &files, &notes);
        assert!(
            wake.contains("table-39.png (image)")
                && wake.contains("- Note 11: ")
                && !wake.contains("more (on the result page)"),
            "{wake}"
        );
        // With no room left for the report, it says where it is rather than cut the end.
        let quoted = with_report(&report, 0, true, |quoted| {
            format!("{}{quoted}|end", "x".repeat(crate::group::REASON_CHARS - 80))
        });
        assert!(
            quoted.ends_with(&format!("{NO_ROOM}|end")) && quoted.chars().count() <= crate::group::REASON_CHARS,
            "{quoted}"
        );
    }

    /// A round still running background work whose turn said nothing quotes nothing, rather than "(nothing)".
    #[test]
    fn a_monitoring_wake_quotes_only_what_the_turn_said() {
        let db = Db::in_memory().unwrap();
        let (task, handoff) = task_with_handoff(&db, "chat-1");
        let dir = crate::test_support::temp_dir("monitoring-wake");
        start_round(&db, &task.id, 1, &proof_dir(&dir, &task.id, 1), 7).unwrap();
        let group = Handoff { speaker: Some("filo".into()), ..handoff.clone() };
        for said in [None, Some(" \n")] {
            finish_round(&db, &task.id, 1, "monitoring", said, None, 9).unwrap();
            let round = latest_round(&db, &task.id).unwrap().unwrap();
            for owner in [&handoff, &group] {
                let text = monitoring_message(&task, owner, &round, &["cargo test".into()]);
                assert!(
                    !text.contains("(nothing)")
                        && !text.contains("What it said as its turn ended")
                        && text.contains("Still running in the background: cargo test"),
                    "{text}"
                );
            }
        }
        finish_round(&db, &task.id, 1, "monitoring", Some("Waiting for the tests."), None, 9).unwrap();
        let text = monitoring_message(&task, &handoff, &latest_round(&db, &task.id).unwrap().unwrap(), &[]);
        assert!(text.contains("What it said as its turn ended:\nWaiting for the tests.\n\nStill running in the background: background work"), "{text}");
    }

    #[test]
    fn a_round_lists_only_the_proof_it_saved_in_a_folder_it_shares() {
        let db = Db::in_memory().unwrap();
        let (task, _) = task_with_handoff(&db, "chat-1");
        let dir = crate::test_support::temp_dir("shared-proof");
        let (shared, own) = (proof_dir(&dir, &task.id, 1), proof_dir(&dir, &task.id, 3));
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&own).unwrap();
        let save = |folder: &Path, name: &str, at: i64| {
            let file = std::fs::File::create(folder.join(name)).unwrap();
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(at as u64)).unwrap();
        };
        let names = |round: i64| -> Vec<String> {
            round_proof(&db, &super::round(&db, &task.id, round).unwrap().unwrap())
                .unwrap()
                .into_iter()
                .map(|file| file.name)
                .collect()
        };
        // Round 1 saves a screenshot, starts a build and ends its turn waiting on it.
        start_round(&db, &task.id, 1, &shared, 1_000).unwrap();
        save(&shared, "before.png", 1_500);
        finish_round(&db, &task.id, 1, "monitoring", Some("Waiting for the build."), None, 2_000).unwrap();
        save(&shared, "build.log", 2_500);
        // While it waits, its page has what it saved by then, not the build's output.
        assert_eq!(names(1), ["before.png"]);
        // The tool carries on as round 2 in the same folder; while it runs and after, round 1 keeps only its own.
        start_round(&db, &task.id, 2, &shared, 3_000).unwrap();
        save(&shared, "after.png", 3_500);
        assert_eq!(names(1), ["before.png"]);
        assert_eq!(names(2), ["after.png", "build.log"]);
        finish_round(&db, &task.id, 2, "done", Some("Built it."), None, 4_000).unwrap();
        assert_eq!(
            (names(1), names(2)),
            (vec!["before.png".to_string()], vec!["after.png".to_string(), "build.log".to_string()])
        );
        // A round in a folder of its own lists everything in it, whenever it was written (a copy keeps its old date).
        start_round(&db, &task.id, 3, &own, 5_000).unwrap();
        save(&own, "copied.png", 10);
        finish_round(&db, &task.id, 3, "done", None, None, 6_000).unwrap();
        assert_eq!(names(3), ["copied.png"]);
    }

    #[test]
    fn briefs_are_calm_and_free_of_rules() {
        assert!(brief_problem("Fix it", "Do NOT touch the header", "It works", None).unwrap().contains("NOT"));
        assert!(brief_problem("Fix it", "Rename `MAX_ROWS` to `ROW_LIMIT`", "Tests pass", None).is_none());
        assert!(
            brief_problem("Fix it", "Fix the wrap", "It wraps", Some("Don't commit anything"))
                .unwrap()
                .contains("don't commit")
        );
        assert!(brief_problem("Fix it", "Fix the wrap", "It wraps", Some("The header must stay as it is")).is_none());
        assert_eq!(render_brief(" Fix the wrap ", "It wraps", Some("  ")), "Goal: Fix the wrap\n\nDone when: It wraps");
        assert_eq!(approval_answer("Allow always"), Some("allow_always"));
        assert_eq!(approval_answer("no"), Some("deny"));
    }
}

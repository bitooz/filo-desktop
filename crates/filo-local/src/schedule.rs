// Scheduled work in Filo's database (db.rs): reminders, routines and Filo's own check-ins ("wakes"), and what comes
// due when. Nothing here fires anything: `claim_due` hands what is due to whoever runs the schedule (the app while it's
// open, otherwise the background helper launchd keeps running, src/launch_agent.rs), and that process notifies or
// starts a turn. A claim is a write in an IMMEDIATE transaction that moves the item on, so an item is handed out once
// even when both processes look at the same moment.
//
// - A reminder is pending until it's done or cancelled. When it comes due it's announced once (`notified_at` keeps the
//   due time that was announced); snoozing moves it, and completing a repeating one moves it to its next time.
// - A routine runs a turn on its schedule: weekly on `days` at `minute_of_day`, once at a time, or every N minutes
//   (optionally only within a window of the day). Each run is a row in `routine_runs`.
// - A wake is a check-in Filo (or an agent) planned for itself in a conversation: when it comes due, a turn runs there.
//
// Times are ms since the epoch. Wall-clock schedules (a routine at 8:00 on weekdays, a daily reminder) follow this
// Mac's time zone, so they stay at 8:00 across daylight saving changes. The time zone is a parameter so tests can fix
// it; the app passes `chrono::Local`.

use chrono::{DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Weekday};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::Db;

/// The scheduling tables (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE reminders (
    id          TEXT PRIMARY KEY NOT NULL,
    title       TEXT NOT NULL,
    note        TEXT,
    due_at      INTEGER NOT NULL,
    recurrence  TEXT,
    anchor_at   INTEGER,
    delivery    TEXT NOT NULL DEFAULT 'notify',
    status      TEXT NOT NULL DEFAULT 'pending',
    notified_at INTEGER,
    agent_id    TEXT,
    chat_id     TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX reminders_by_due ON reminders (status, due_at);
CREATE TABLE routines (
    id               TEXT PRIMARY KEY NOT NULL,
    title            TEXT NOT NULL,
    instructions     TEXT NOT NULL,
    days             TEXT NOT NULL,
    minute_of_day    INTEGER NOT NULL,
    schedule         TEXT,
    status           TEXT NOT NULL DEFAULT 'active',
    next_run_at      INTEGER,
    last_run_at      INTEGER,
    last_skip_reason TEXT,
    agent_id         TEXT,
    chat_id          TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);
CREATE INDEX routines_by_next_run ON routines (status, next_run_at);
CREATE TABLE routine_runs (
    id          TEXT PRIMARY KEY NOT NULL,
    routine_id  TEXT NOT NULL,
    due_at      INTEGER,
    state       TEXT NOT NULL,
    chat_id     TEXT,
    turn_id     TEXT,
    pid         INTEGER,
    summary     TEXT,
    error       TEXT,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX routine_runs_by_routine ON routine_runs (routine_id, created_at DESC);
CREATE INDEX routine_runs_by_state ON routine_runs (state);
CREATE TABLE wakes (
    id         TEXT PRIMARY KEY NOT NULL,
    title      TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    run_at     INTEGER NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending',
    chat_id    TEXT,
    agent_id   TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX wakes_by_run_at ON wakes (status, run_at);
";

pub const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;

const MAX_REMINDER_TITLE: usize = 200;
const MAX_REMINDER_NOTE: usize = 1_000;
const MAX_SNOOZE_MINUTES: f64 = 7. * 24. * 60.;
/// `reminders:myReminders` lists this many pending reminders.
const MAX_VISIBLE_REMINDERS: usize = 100;
const MAX_ROUTINE_TITLE: usize = 120;
const MAX_INSTRUCTIONS: usize = 3_000;
/// Room for the user's own and one per Feed topic (feed.rs `MAX_TOPICS`), each watched by an agent on a routine.
pub const MAX_ROUTINES: usize = 10 + crate::feed::MAX_TOPICS;
pub const MIN_INTERVAL_MINUTES: i64 = 30;
const MAX_INTERVAL_MINUTES: i64 = 7 * 24 * 60;
/// A routine run this late (the Mac was off or asleep) is skipped instead of delivering a morning brief at night.
const MAX_LATENESS: i64 = 3 * 60 * MINUTE;
/// A routine's runs listed on its page.
const MAX_VISIBLE_RUNS: usize = 20;
const MAX_WAKE_TITLE: usize = 120;
const MAX_WAKE_NOTE: usize = 2_000;
const MAX_PENDING_WAKES: usize = 30;
const MAX_WAKE_HORIZON: i64 = 60 * DAY;
/// A check-in missed by more than this (the Mac was off) is dropped: the moment it was for has passed.
const WAKE_EXPIRY: i64 = 6 * 60 * MINUTE;
/// A run claimed but never started (its process died between the two) is given up after this.
const QUEUED_EXPIRY: i64 = 5 * MINUTE;

const PREVIOUS_STILL_WORKING: &str = "The previous run was still working.";
const MISSED: &str = "Missed its time.";

// ---------------------------------------------------------------- rows

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub due_at: i64,
    pub status: String,
    /// `daily`, `weekdays` or `weekly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// `alarm`; none is a plain notification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    /// The agent that made it; none for Filo's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The conversation it was made in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    pub created_at: i64,
}

/// A one-off or repeating schedule; none on a routine means weekly on its days.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Repeat {
    Once {
        at: i64,
    },
    Interval {
        every_minutes: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_minute: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_minute: Option<i64>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_creationTime")]
    pub creation_time: i64,
    pub title: String,
    pub instructions: String,
    /// 0 = Sunday … 6 = Saturday.
    pub days: Vec<u8>,
    pub minute_of_day: i64,
    /// `active` or `paused`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Repeat>,
    /// Runs are turns in a conversation (the server's "chat" routines).
    pub delivery: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Where its runs happen: its own chat for Filo's, the agent's conversation for an agent's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Routine {
    pub fn active(&self) -> bool {
        self.status == "active"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(rename = "_id")]
    pub id: String,
    pub routine_id: String,
    /// When it was scheduled for; none for Run now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<i64>,
    /// `queued` (claimed), `active` (its turn runs), `completed` or `failed`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// What the run sent the user (its first reply), for the routine's page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Wake {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    pub note: String,
    pub run_at: i64,
    /// `pending`, `done`, `canceled` or `expired`.
    pub status: String,
    /// The conversation it runs in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub created_at: i64,
}

/// Something that came due, for the process running the schedule to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// Announce it (a notification; the app's pet and menu bar show it too).
    Remind(Reminder),
    /// Run a turn for it; `run` is its row, `queued`.
    Routine { routine: Routine, run: Run },
    /// Run a turn in the conversation it was planned in.
    Wake(Wake),
}

/// Whose items a caller sees and may change: Filo (and the user's pages) see everything, an agent only its own. The
/// main agent (main_agent.rs, Filo's agent until the user picks another) also has what belongs to no agent (made on the
/// Scheduled page or in Filo's own chats), but not the other agents' items.
pub type Agent<'a> = Option<&'a str>;

/// The SQL test for rows `?1` (an `Agent`) may see, as `may_touch` decides; `?2` is the main agent's id.
const VISIBLE_TO: &str = "(?1 IS NULL OR agent_id = ?1 OR (?1 = ?2 AND agent_id IS NULL))";

// ---------------------------------------------------------------- time

fn local<Tz: TimeZone>(tz: &Tz, at: i64) -> DateTime<Tz> {
    tz.timestamp_millis_opt(at).earliest().unwrap_or_else(|| tz.timestamp_millis_opt(0).unwrap())
}

/// The instant a wall-clock time has in `tz`: the earlier one when the clocks go back, the same time an hour later when
/// they skip it.
pub fn at_local<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> i64 {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(time) => time.timestamp_millis(),
        LocalResult::Ambiguous(earlier, _) => earlier.timestamp_millis(),
        LocalResult::None => tz
            .from_local_datetime(&(naive + chrono::Duration::hours(1)))
            .earliest()
            .map_or_else(|| naive.and_utc().timestamp_millis(), |time| time.timestamp_millis()),
    }
}

fn weekday_number(date: NaiveDate) -> u8 {
    date.weekday().num_days_from_sunday() as u8
}

fn time_of(minute_of_day: i64) -> NaiveTime {
    let minute = minute_of_day.clamp(0, 24 * 60 - 1);
    NaiveTime::from_hms_opt((minute / 60) as u32, (minute % 60) as u32, 0).unwrap_or(NaiveTime::MIN)
}

fn minute_in_day<Tz: TimeZone>(tz: &Tz, at: i64) -> i64 {
    let time = local(tz, at);
    (time.hour() * 60 + time.minute()) as i64
}

/// The first time on one of `days` at `minute_of_day` strictly after `after`.
pub fn next_weekly<Tz: TimeZone>(tz: &Tz, days: &[u8], minute_of_day: i64, after: i64) -> Option<i64> {
    let today = local(tz, after).date_naive();
    let time = time_of(minute_of_day);
    // Eight days always hold the next matching weekday, even when today's time has passed.
    (0..=7).find_map(|offset| {
        let date = today + chrono::Duration::days(offset);
        let candidate = at_local(tz, date.and_time(time));
        (days.contains(&weekday_number(date)) && candidate > after).then_some(candidate)
    })
}

/// A routine's next run strictly after `after`; none when a one-off's time has gone. Intervals keep their cadence
/// from the last due time and, outside their window of the day, wait for it to open (09:00, not 09:30).
fn next_run<Tz: TimeZone>(
    tz: &Tz,
    days: &[u8],
    minute_of_day: i64,
    schedule: Option<&Repeat>,
    after: i64,
    last_due: Option<i64>,
) -> Option<i64> {
    match schedule {
        None => next_weekly(tz, days, minute_of_day, after),
        Some(Repeat::Once { at }) => (*at > after).then_some(*at),
        Some(Repeat::Interval { every_minutes, from_minute, to_minute }) => {
            let step = every_minutes.max(&1) * MINUTE;
            let candidate = match last_due {
                Some(last) if last + step > after => last + step,
                _ => after + step,
            };
            let (Some(from), Some(to)) = (*from_minute, *to_minute) else { return Some(candidate) };
            if from == to {
                return Some(candidate);
            }
            let minute = minute_in_day(tz, candidate);
            let inside = if from < to { minute >= from && minute < to } else { minute >= from || minute < to };
            if inside { Some(candidate) } else { next_weekly(tz, &[0, 1, 2, 3, 4, 5, 6], from, candidate) }
        }
    }
}

/// A repeating reminder's next time after `now`, at the wall-clock time of its first one.
fn next_recurring<Tz: TimeZone>(tz: &Tz, anchor: i64, now: i64, recurrence: &str) -> i64 {
    let start = local(tz, anchor).naive_local();
    let time = start.time();
    let mut date = start.date();
    // A long-stale anchor (the Mac was off for weeks) starts near today, keeping the weekday for weekly ones.
    let behind = (local(tz, now).date_naive() - date).num_days();
    if behind > 8 {
        date += chrono::Duration::days(if recurrence == "weekly" { behind / 7 * 7 - 7 } else { behind - 2 });
    }
    for _ in 0..400 {
        date += chrono::Duration::days(if recurrence == "weekly" { 7 } else { 1 });
        if recurrence == "weekdays" {
            while matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
                date += chrono::Duration::days(1);
            }
        }
        let candidate = at_local(tz, date.and_time(time));
        if candidate > now {
            return candidate;
        }
    }
    now + if recurrence == "weekly" { 7 * DAY } else { DAY }
}

// ---------------------------------------------------------------- checks

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn whole(value: &Value, what: &str, range: std::ops::RangeInclusive<i64>) -> Result<i64, String> {
    let number = number(value)
        .filter(|number| number.fract() == 0.)
        .ok_or_else(|| format!("{what} must be a whole number"))? as i64;
    if !range.contains(&number) {
        return Err(format!("{what} must be from {} to {}", range.start(), range.end()));
    }
    Ok(number)
}

fn title(text: &str, max: usize, what: &str) -> Result<String, String> {
    let title = one_line(text);
    if title.is_empty() {
        return Err(format!("The {what} needs a title."));
    }
    if title.chars().count() > max {
        return Err(format!("The {what}'s title is too long (at most {max} characters)."));
    }
    Ok(title)
}

fn days_of(value: &Value) -> Result<Vec<u8>, String> {
    let list = value.as_array().ok_or("days must be a list of weekday numbers (0 = Sunday … 6 = Saturday)")?;
    let mut days = Vec::new();
    for day in list {
        let day = whole(day, "A day", 0..=6)? as u8;
        if !days.contains(&day) {
            days.push(day);
        }
    }
    if days.is_empty() {
        return Err("A routine needs at least one day.".into());
    }
    days.sort_unstable();
    Ok(days)
}

fn minute_of(value: &Value) -> Result<i64, String> {
    whole(value, "minuteOfDay", 0..=24 * 60 - 1)
}

/// A schedule given by a page or a tool (`{kind:"once",at}` or `{kind:"interval",everyMinutes,fromMinute?,toMinute?}`).
fn repeat_of(value: &Value, now: i64) -> Result<Option<Repeat>, String> {
    if value.is_null() {
        return Ok(None);
    }
    match value.get("kind").and_then(Value::as_str) {
        Some("once") => {
            let at = value.get("at").and_then(number).ok_or("A one-off routine needs `at`.")? as i64;
            if at <= now {
                return Err("A one-off routine needs a time in the future.".into());
            }
            Ok(Some(Repeat::Once { at }))
        }
        Some("interval") => {
            let every = value.get("everyMinutes").ok_or("A repeating routine needs `everyMinutes`.")?;
            let every_minutes = whole(every, "everyMinutes", MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES)
                .map_err(|_| format!("A repeating routine runs every {MIN_INTERVAL_MINUTES} minutes to 7 days."))?;
            let from = value.get("fromMinute").filter(|value| !value.is_null());
            let to = value.get("toMinute").filter(|value| !value.is_null());
            let (from_minute, to_minute) = match (from, to) {
                (None, None) => (None, None),
                (Some(from), Some(to)) => (Some(minute_of(from)?), Some(minute_of(to)?)),
                _ => return Err("fromMinute and toMinute go together.".into()),
            };
            Ok(Some(Repeat::Interval { every_minutes, from_minute, to_minute }))
        }
        _ => Err("A schedule's kind is `once` or `interval`.".into()),
    }
}

fn recurrence_of(value: Option<&str>) -> Result<Option<String>, String> {
    match value.map(str::trim).filter(|value| !value.is_empty() && *value != "none") {
        None => Ok(None),
        Some(value @ ("daily" | "weekdays" | "weekly")) => Ok(Some(value.to_string())),
        Some(other) => Err(format!("A reminder repeats daily, weekdays or weekly, not {other:?}.")),
    }
}

// ---------------------------------------------------------------- database

/// Runs `write` holding the database's write lock from the start, so what it reads is still true when it writes.
fn immediate<T>(db: &Db, write: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
    db.with(|conn| {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let result = write(&tx)?;
        tx.commit()?;
        Ok(result)
    })
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

const REMINDER_COLUMNS: &str = "id, title, note, due_at, status, recurrence, delivery, agent_id, chat_id, created_at";

fn reminder_row(row: &Row) -> rusqlite::Result<Reminder> {
    let delivery: String = row.get(6)?;
    Ok(Reminder {
        id: row.get(0)?,
        title: row.get(1)?,
        note: row.get(2)?,
        due_at: row.get(3)?,
        status: row.get(4)?,
        recurrence: row.get(5)?,
        delivery: (delivery != "notify").then_some(delivery),
        agent_id: row.get(7)?,
        chat_id: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn reminder_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<Reminder>> {
    conn.query_row(&format!("SELECT {REMINDER_COLUMNS} FROM reminders WHERE id = ?1"), [id], reminder_row).optional()
}

const ROUTINE_COLUMNS: &str = "id, title, instructions, days, minute_of_day, schedule, status, next_run_at, last_run_at, last_skip_reason, agent_id, chat_id, created_at, updated_at";

fn routine_row(row: &Row) -> rusqlite::Result<Routine> {
    let days: String = row.get(3)?;
    let schedule: Option<String> = row.get(5)?;
    let created_at = row.get(12)?;
    Ok(Routine {
        id: row.get(0)?,
        creation_time: created_at,
        title: row.get(1)?,
        instructions: row.get(2)?,
        days: serde_json::from_str(&days).unwrap_or_default(),
        minute_of_day: row.get(4)?,
        schedule: schedule.and_then(|text| serde_json::from_str(&text).ok()),
        status: row.get(6)?,
        next_run_at: row.get(7)?,
        last_run_at: row.get(8)?,
        last_skip_reason: row.get(9)?,
        delivery: "chat",
        agent_id: row.get(10)?,
        chat_id: row.get(11)?,
        created_at,
        updated_at: row.get(13)?,
    })
}

fn routine_by_id(conn: &Connection, id: &str) -> rusqlite::Result<Option<Routine>> {
    conn.query_row(&format!("SELECT {ROUTINE_COLUMNS} FROM routines WHERE id = ?1"), [id], routine_row).optional()
}

const RUN_COLUMNS: &str = "id, routine_id, due_at, state, chat_id, turn_id, summary, error, created_at, finished_at";

fn run_row(row: &Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        routine_id: row.get(1)?,
        due_at: row.get(2)?,
        state: row.get(3)?,
        chat_id: row.get(4)?,
        turn_id: row.get(5)?,
        summary: row.get(6)?,
        error: row.get(7)?,
        created_at: row.get(8)?,
        completed_at: row.get(9)?,
    })
}

const WAKE_COLUMNS: &str = "id, title, note, run_at, status, chat_id, agent_id, created_at";

fn wake_row(row: &Row) -> rusqlite::Result<Wake> {
    Ok(Wake {
        id: row.get(0)?,
        title: row.get(1)?,
        note: row.get(2)?,
        run_at: row.get(3)?,
        status: row.get(4)?,
        chat_id: row.get(5)?,
        agent_id: row.get(6)?,
        created_at: row.get(7)?,
    })
}

/// Whether `agent` may see or change a row owned by `owner`, `lead` being the main agent.
fn may_touch(owner: Option<&str>, agent: Agent, lead: &str) -> bool {
    agent.is_none() || owner == agent || (agent == Some(lead) && owner.is_none())
}

// ---------------------------------------------------------------- reminders

/// A new reminder (`reminders:createMine`, the `create_reminder` tool).
#[derive(Clone, Debug, Default)]
pub struct NewReminder {
    pub title: String,
    pub note: Option<String>,
    pub due_at: i64,
    pub recurrence: Option<String>,
    /// `notify` (default) or `alarm`.
    pub delivery: Option<String>,
    pub agent_id: Option<String>,
    pub chat_id: Option<String>,
}

pub fn create_reminder(db: &Db, new: NewReminder, now: i64) -> Result<Reminder, String> {
    let title = title(&new.title, MAX_REMINDER_TITLE, "reminder")?;
    let note = new.note.map(|note| note.trim().to_string()).filter(|note| !note.is_empty());
    if note.as_ref().is_some_and(|note| note.chars().count() > MAX_REMINDER_NOTE) {
        return Err(format!("The reminder's note is too long (at most {MAX_REMINDER_NOTE} characters)."));
    }
    if new.due_at < now - 5 * MINUTE {
        return Err("The reminder's time must be in the future.".into());
    }
    let recurrence = recurrence_of(new.recurrence.as_deref())?;
    let delivery = match new.delivery.as_deref().unwrap_or("notify") {
        "notify" | "" => "notify",
        "alarm" => "alarm",
        "call" => return Err("Filo can't call on this Mac: reminders come as notifications (or alarms).".into()),
        other => return Err(format!("A reminder's delivery is notify or alarm, not {other:?}.")),
    };
    let id = new_id();
    immediate(db, |conn| {
        conn.execute(
            "INSERT INTO reminders (id, title, note, due_at, recurrence, anchor_at, delivery, agent_id, chat_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![id, title, note, new.due_at, recurrence, recurrence.as_ref().map(|_| new.due_at), delivery, new.agent_id, new.chat_id, now],
        )?;
        Ok(reminder_by_id(conn, &id)?.expect("the reminder just made"))
    })
}

/// Pending reminders `agent` may see, the latest first (as the Scheduled page lists them).
pub fn reminders(db: &Db, agent: Agent) -> Result<Vec<Reminder>, String> {
    let lead = crate::main_agent::id(db);
    db.with(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {REMINDER_COLUMNS} FROM reminders WHERE status = 'pending' AND {VISIBLE_TO} ORDER BY due_at DESC LIMIT ?3"
        ))?;
        let rows = statement.query_map(params![agent, lead, MAX_VISIBLE_REMINDERS as i64], reminder_row)?;
        rows.collect()
    })
}

/// A reminder `agent` may change, still pending, or what to answer instead: `Ok(false)` when it's gone or not theirs,
/// `Ok(true)` when it's already done or cancelled.
fn pending_reminder(conn: &Connection, id: &str, agent: Agent, lead: &str) -> rusqlite::Result<Result<Reminder, bool>> {
    Ok(match reminder_by_id(conn, id)? {
        Some(reminder) if !may_touch(reminder.agent_id.as_deref(), agent, lead) => Err(false),
        Some(reminder) if reminder.status != "pending" => Err(true),
        Some(reminder) => Ok(reminder),
        None => Err(false),
    })
}

/// Done: a repeating reminder moves to its next time and stays; any other is finished.
pub fn complete_reminder<Tz: TimeZone>(db: &Db, tz: &Tz, id: &str, agent: Agent, now: i64) -> Result<bool, String> {
    let lead = crate::main_agent::id(db);
    immediate(db, |conn| {
        let reminder = match pending_reminder(conn, id, agent, &lead)? {
            Ok(reminder) => reminder,
            Err(answer) => return Ok(answer),
        };
        match &reminder.recurrence {
            Some(recurrence) => {
                let anchor: Option<i64> =
                    conn.query_row("SELECT anchor_at FROM reminders WHERE id = ?1", [id], |row| row.get(0))?;
                let next = next_recurring(tz, anchor.unwrap_or(reminder.due_at), now, recurrence);
                conn.execute(
                    "UPDATE reminders SET due_at = ?1, notified_at = NULL, updated_at = ?2 WHERE id = ?3",
                    params![next, now, id],
                )?;
            }
            None => {
                conn.execute(
                    "UPDATE reminders SET status = 'completed', finished_at = ?1, updated_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
        }
        Ok(true)
    })
}

pub fn cancel_reminder(db: &Db, id: &str, agent: Agent, now: i64) -> Result<bool, String> {
    let lead = crate::main_agent::id(db);
    immediate(db, |conn| {
        if let Err(answer) = pending_reminder(conn, id, agent, &lead)? {
            return Ok(answer);
        }
        conn.execute(
            "UPDATE reminders SET status = 'canceled', finished_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(true)
    })
}

/// Due again `minutes` from now (0 < minutes ≤ a week).
pub fn snooze_reminder(db: &Db, id: &str, minutes: f64, agent: Agent, now: i64) -> Result<bool, String> {
    if !minutes.is_finite() || minutes <= 0. || minutes > MAX_SNOOZE_MINUTES {
        return Err("A reminder snoozes for 1 minute to 7 days.".into());
    }
    let due_at = now + (minutes.floor().max(1.) as i64) * MINUTE;
    let lead = crate::main_agent::id(db);
    immediate(db, |conn| {
        if let Err(answer) = pending_reminder(conn, id, agent, &lead)? {
            return Ok(answer);
        }
        conn.execute(
            "UPDATE reminders SET due_at = ?1, notified_at = NULL, updated_at = ?2 WHERE id = ?3",
            params![due_at, now, id],
        )?;
        Ok(true)
    })
}

// ---------------------------------------------------------------- routines

/// A new routine (`routines:createMine`, the `create_routine` tool). `days`, `minuteOfDay` and `schedule` as the
/// pages send them.
#[derive(Clone, Debug, Default)]
pub struct NewRoutine {
    pub title: String,
    pub instructions: String,
    pub days: Value,
    pub minute_of_day: Value,
    pub schedule: Value,
    pub agent_id: Option<String>,
}

pub fn create_routine<Tz: TimeZone>(db: &Db, tz: &Tz, new: NewRoutine, now: i64) -> Result<Routine, String> {
    let title = title(&new.title, MAX_ROUTINE_TITLE, "routine")?;
    let instructions = instructions_of(&new.instructions)?;
    let schedule = repeat_of(&new.schedule, now)?;
    // Once and interval routines keep weekly fields too (the page edits them); every day at midnight by default.
    let days = if new.days.is_null() && schedule.is_some() { vec![0, 1, 2, 3, 4, 5, 6] } else { days_of(&new.days)? };
    let minute_of_day =
        if new.minute_of_day.is_null() && schedule.is_some() { 0 } else { minute_of(&new.minute_of_day)? };
    let next_run_at = next_run(tz, &days, minute_of_day, schedule.as_ref(), now, None);
    let id = new_id();
    let days_json = serde_json::to_string(&days).unwrap_or_else(|_| "[]".into());
    let schedule_json = schedule.as_ref().and_then(|schedule| serde_json::to_string(schedule).ok());
    let created = immediate(db, |conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM routines", [], |row| row.get(0))?;
        if count as usize >= MAX_ROUTINES {
            return Ok(None);
        }
        conn.execute(
            "INSERT INTO routines (id, title, instructions, days, minute_of_day, schedule, status, next_run_at, agent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?9)",
            params![id, title, instructions, days_json, minute_of_day, schedule_json, next_run_at, new.agent_id, now],
        )?;
        routine_by_id(conn, &id)
    })?;
    created.ok_or_else(|| format!("Routine limit reached ({MAX_ROUTINES}). Delete one first."))
}

fn instructions_of(text: &str) -> Result<String, String> {
    let instructions = text.trim().to_string();
    if instructions.is_empty() {
        return Err("The routine needs instructions.".into());
    }
    if instructions.chars().count() > MAX_INSTRUCTIONS {
        return Err(format!("The routine's instructions are too long (at most {MAX_INSTRUCTIONS} characters)."));
    }
    Ok(instructions)
}

/// The routines `agent` may see, the oldest first.
pub fn routines(db: &Db, agent: Agent) -> Result<Vec<Routine>, String> {
    let lead = crate::main_agent::id(db);
    db.with(|conn| {
        let mut statement = conn
            .prepare(&format!("SELECT {ROUTINE_COLUMNS} FROM routines WHERE {VISIBLE_TO} ORDER BY created_at ASC"))?;
        let rows = statement.query_map(params![agent, lead], routine_row)?;
        rows.collect()
    })
}

pub fn routine(db: &Db, id: &str, agent: Agent) -> Result<Option<Routine>, String> {
    let lead = crate::main_agent::id(db);
    Ok(db.with(|conn| routine_by_id(conn, id))?.filter(|routine| may_touch(routine.agent_id.as_deref(), agent, &lead)))
}

/// A routine's runs, the newest first.
pub fn runs(db: &Db, routine_id: &str) -> Result<Vec<Run>, String> {
    db.with(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM routine_runs WHERE routine_id = ?1 ORDER BY created_at DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![routine_id, MAX_VISIBLE_RUNS as i64], run_row)?;
        rows.collect()
    })
}

/// Changes what `patch` names (`title`, `instructions`, `days`, `minuteOfDay`, `status`, `schedule` with `null` back
/// to weekly); `Ok(false)` when the routine is gone. The next run is worked out again when the schedule or the status
/// changes, and a paused routine has none.
pub fn update_routine<Tz: TimeZone>(
    db: &Db,
    tz: &Tz,
    id: &str,
    patch: &Value,
    agent: Agent,
    now: i64,
) -> Result<bool, String> {
    let Some(mut routine) = routine(db, id, agent)? else { return Ok(false) };
    let mut reschedule = false;
    if let Some(text) = patch.get("title").and_then(Value::as_str) {
        routine.title = title(text, MAX_ROUTINE_TITLE, "routine")?;
    }
    if let Some(text) = patch.get("instructions").and_then(Value::as_str) {
        routine.instructions = instructions_of(text)?;
    }
    if let Some(days) = patch.get("days") {
        routine.days = days_of(days)?;
        reschedule = true;
    }
    if let Some(minute) = patch.get("minuteOfDay") {
        routine.minute_of_day = minute_of(minute)?;
        reschedule = true;
    }
    if let Some(schedule) = patch.get("schedule") {
        routine.schedule = repeat_of(schedule, now)?;
        reschedule = true;
    }
    if let Some(status) = patch.get("status").and_then(Value::as_str) {
        if status != "active" && status != "paused" {
            return Err("A routine's status is active or paused.".into());
        }
        reschedule |= status != routine.status;
        routine.status = status.to_string();
    }
    if reschedule {
        routine.next_run_at = if routine.active() {
            // Only the weekly fields change on a one-off or repeating routine; its own times stay.
            let next = next_run(tz, &routine.days, routine.minute_of_day, routine.schedule.as_ref(), now, None);
            if next.is_none() {
                return Err("This one-off routine's time has passed. Give it a new time first.".into());
            }
            next
        } else {
            None
        };
    }
    let days_json = serde_json::to_string(&routine.days).unwrap_or_else(|_| "[]".into());
    let schedule_json = routine.schedule.as_ref().and_then(|schedule| serde_json::to_string(schedule).ok());
    immediate(db, |conn| {
        let changed = conn.execute(
            "UPDATE routines SET title = ?1, instructions = ?2, days = ?3, minute_of_day = ?4, schedule = ?5, status = ?6, next_run_at = ?7,
             updated_at = ?8 WHERE id = ?9",
            params![routine.title, routine.instructions, days_json, routine.minute_of_day, schedule_json, routine.status, routine.next_run_at, now, id],
        )?;
        Ok(changed == 1)
    })
}

/// Deletes the routine and its runs (their conversations stay).
pub fn delete_routine(db: &Db, id: &str, agent: Agent) -> Result<bool, String> {
    if routine(db, id, agent)?.is_none() {
        return Ok(false);
    }
    immediate(db, |conn| {
        conn.execute("DELETE FROM routine_runs WHERE routine_id = ?1", [id])?;
        Ok(conn.execute("DELETE FROM routines WHERE id = ?1", [id])? == 1)
    })
}

/// Gives a routine to another agent (none: the main agent's); its runs happen in that agent's conversation from then on.
pub fn set_routine_agent(db: &Db, id: &str, agent_id: Option<&str>, now: i64) -> Result<bool, String> {
    immediate(db, |conn| {
        conn.execute(
            "UPDATE routines SET agent_id = ?1, chat_id = NULL, updated_at = ?2 WHERE id = ?3",
            params![agent_id, now, id],
        )
        .map(|changed| changed == 1)
    })
}

fn run_in_progress(conn: &Connection, routine_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM routine_runs WHERE routine_id = ?1 AND state IN ('queued', 'active'))",
        [routine_id],
        |row| row.get(0),
    )
}

fn insert_run(conn: &Connection, routine_id: &str, due_at: Option<i64>, now: i64) -> rusqlite::Result<Run> {
    let id = new_id();
    conn.execute(
        "INSERT INTO routine_runs (id, routine_id, due_at, state, created_at) VALUES (?1, ?2, ?3, 'queued', ?4)",
        params![id, routine_id, due_at, now],
    )?;
    conn.query_row(&format!("SELECT {RUN_COLUMNS} FROM routine_runs WHERE id = ?1"), [&id], run_row)
}

/// Run now: a queued run for the caller to start; `Ok(None)` when the routine is gone, an error when its last run is
/// still working.
pub fn start_run_now(db: &Db, id: &str, agent: Agent, now: i64) -> Result<Option<(Routine, Run)>, String> {
    let lead = crate::main_agent::id(db);
    let outcome = immediate(db, |conn| {
        let Some(routine) =
            routine_by_id(conn, id)?.filter(|routine| may_touch(routine.agent_id.as_deref(), agent, &lead))
        else {
            return Ok(None);
        };
        if run_in_progress(conn, id)? {
            return Ok(Some(Err(())));
        }
        let run = insert_run(conn, id, None, now)?;
        conn.execute("UPDATE routines SET last_run_at = ?1 WHERE id = ?2", params![now, id])?;
        Ok(Some(Ok((routine, run))))
    })?;
    match outcome {
        None => Ok(None),
        Some(Ok(started)) => Ok(Some(started)),
        Some(Err(())) => Err(PREVIOUS_STILL_WORKING.into()),
    }
}

/// The run's turn started, in this process.
pub fn run_started(db: &Db, run_id: &str, chat_id: &str, turn_id: &str) -> Result<(), String> {
    immediate(db, |conn| {
        conn.execute(
            "UPDATE routine_runs SET state = 'active', chat_id = ?1, turn_id = ?2, pid = ?3 WHERE id = ?4",
            params![chat_id, turn_id, std::process::id() as i64, run_id],
        )
        .map(|_| ())
    })
}

/// The run's turn ended: `summary` is what it sent the user, `error` what went wrong.
pub fn run_finished(db: &Db, run_id: &str, summary: Option<&str>, error: Option<&str>, now: i64) -> Result<(), String> {
    let state = if error.is_some() { "failed" } else { "completed" };
    immediate(db, |conn| {
        conn.execute(
            "UPDATE routine_runs SET state = ?1, summary = ?2, error = ?3, finished_at = ?4 WHERE id = ?5",
            params![state, summary, error, now, run_id],
        )
        .map(|_| ())
    })
}

/// The turns of the routine runs working now: started, in a process that's still there (the app or the scheduler
/// helper). Each keeps the Mac awake while "Keep Mac awake while tasks run" is on (the app's local_backend/awake.rs).
pub fn active_run_turns(db: &Db) -> Result<Vec<String>, String> {
    let runs: Vec<(Option<String>, Option<i64>)> = db.with(|conn| {
        let mut statement = conn.prepare("SELECT turn_id, pid FROM routine_runs WHERE state = 'active'")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    })?;
    Ok(runs.into_iter().filter(|(_, pid)| pid.is_some_and(process_alive)).filter_map(|(turn, _)| turn).collect())
}

use crate::lock::process_alive;

/// Runs whose process is gone (Filo quit or crashed mid-run) or that were never started are marked failed, so the
/// routine can run again. Returns how many.
pub fn settle_orphaned_runs(db: &Db, now: i64) -> Result<usize, String> {
    immediate(db, |conn| {
        let mut statement =
            conn.prepare("SELECT id, state, pid, created_at FROM routine_runs WHERE state IN ('queued', 'active')")?;
        let open: Vec<(String, String, Option<i64>, i64)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut settled = 0;
        for (id, state, pid, created_at) in open {
            let orphaned = match state.as_str() {
                "queued" => now - created_at > QUEUED_EXPIRY,
                _ => !pid.is_some_and(process_alive),
            };
            if orphaned {
                settled += conn.execute(
                    "UPDATE routine_runs SET state = 'failed', error = 'Filo stopped before this run finished.', finished_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
        }
        Ok(settled)
    })
}

// ---------------------------------------------------------------- wakes

#[derive(Clone, Debug, Default)]
pub struct NewWake {
    pub title: String,
    pub note: String,
    pub run_at: i64,
    pub chat_id: Option<String>,
    pub agent_id: Option<String>,
}

pub fn create_wake(db: &Db, new: NewWake, now: i64) -> Result<Wake, String> {
    let title = title(&new.title, MAX_WAKE_TITLE, "check-in")?;
    let note = new.note.trim().to_string();
    if note.chars().count() > MAX_WAKE_NOTE {
        return Err(format!("The check-in's note is too long (at most {MAX_WAKE_NOTE} characters)."));
    }
    if new.run_at < now + MINUTE {
        return Err("A check-in needs to be at least a minute from now.".into());
    }
    if new.run_at > now + MAX_WAKE_HORIZON {
        return Err("A check-in can be at most 60 days ahead.".into());
    }
    let id = new_id();
    let made = immediate(db, |conn| {
        let pending: i64 =
            conn.query_row("SELECT COUNT(*) FROM wakes WHERE status = 'pending'", [], |row| row.get(0))?;
        if pending as usize >= MAX_PENDING_WAKES {
            return Ok(None);
        }
        conn.execute(
            "INSERT INTO wakes (id, title, note, run_at, chat_id, agent_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, title, note, new.run_at, new.chat_id, new.agent_id, now],
        )?;
        conn.query_row(&format!("SELECT {WAKE_COLUMNS} FROM wakes WHERE id = ?1"), [&id], wake_row).optional()
    })?;
    made.ok_or_else(|| format!("There are already {MAX_PENDING_WAKES} check-ins planned. Cancel one first."))
}

/// Pending check-ins `agent` may see, the soonest first.
pub fn wakes(db: &Db, agent: Agent) -> Result<Vec<Wake>, String> {
    let lead = crate::main_agent::id(db);
    db.with(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {WAKE_COLUMNS} FROM wakes WHERE status = 'pending' AND {VISIBLE_TO} ORDER BY run_at ASC"
        ))?;
        let rows = statement.query_map(params![agent, lead], wake_row)?;
        rows.collect()
    })
}

pub fn cancel_wake(db: &Db, id: &str, agent: Agent, now: i64) -> Result<bool, String> {
    let lead = crate::main_agent::id(db);
    immediate(db, |conn| {
        let owner: Option<(Option<String>, String)> = conn
            .query_row("SELECT agent_id, status FROM wakes WHERE id = ?1", [id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        match owner {
            Some((owner, _)) if !may_touch(owner.as_deref(), agent, &lead) => Ok(false),
            Some((_, status)) if status != "pending" => Ok(true),
            Some(_) => conn
                .execute("UPDATE wakes SET status = 'canceled', updated_at = ?1 WHERE id = ?2", params![now, id])
                .map(|_| true),
            None => Ok(false),
        }
    })
}

// ---------------------------------------------------------------- agents

/// An agent's pending reminders and its routines (asked before it's deleted).
pub fn agent_counts(db: &Db, agent_id: &str) -> Result<(usize, usize), String> {
    db.with(|conn| {
        let reminders: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reminders WHERE status = 'pending' AND agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?;
        let routines: i64 =
            conn.query_row("SELECT COUNT(*) FROM routines WHERE agent_id = ?1", [agent_id], |row| row.get(0))?;
        Ok((reminders as usize, routines as usize))
    })
}

/// The agent was deleted: its reminders and routines go to `heir` (the main agent; none: to Filo, owned by no agent),
/// or with it when `delete`. Its check-ins are cancelled either way (they were about its conversation, which is gone).
pub fn agent_removed(db: &Db, agent_id: &str, delete: bool, heir: Option<&str>, now: i64) -> Result<(), String> {
    immediate(db, |conn| {
        if delete {
            conn.execute("UPDATE reminders SET status = 'canceled', finished_at = ?1, updated_at = ?1 WHERE status = 'pending' AND agent_id = ?2", params![now, agent_id])?;
            conn.execute(
                "DELETE FROM routine_runs WHERE routine_id IN (SELECT id FROM routines WHERE agent_id = ?1)",
                [agent_id],
            )?;
            conn.execute("DELETE FROM routines WHERE agent_id = ?1", [agent_id])?;
        } else {
            let heir = heir.filter(|heir| *heir != agent_id);
            conn.execute(
                "UPDATE reminders SET agent_id = ?3, chat_id = NULL, updated_at = ?1 WHERE agent_id = ?2",
                params![now, agent_id, heir],
            )?;
            // Its runs happened in the agent's conversation; the heir's happen in its own (Filo's, in a chat of their own).
            conn.execute(
                "UPDATE routines SET agent_id = ?3, chat_id = NULL, updated_at = ?1 WHERE agent_id = ?2",
                params![now, agent_id, heir],
            )?;
        }
        conn.execute(
            "UPDATE wakes SET status = 'canceled', updated_at = ?1 WHERE status = 'pending' AND agent_id = ?2",
            params![now, agent_id],
        )?;
        Ok(())
    })
}

/// How many reminders, routines and check-ins were ever made (done and cancelled ones too): when it grows, something
/// was scheduled, from the app or a turn's MCP server.
pub fn made(db: &Db) -> Result<i64, String> {
    db.with(|conn| {
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM reminders) + (SELECT COUNT(*) FROM routines) + (SELECT COUNT(*) FROM wakes)",
            [],
            |row| row.get(0),
        )
    })
}

// ---------------------------------------------------------------- what comes due

/// The soonest time something is due: when the process running the schedule should look again.
pub fn next_due(db: &Db) -> Result<Option<i64>, String> {
    db.with(|conn| {
        conn.query_row(
            "SELECT MIN(at) FROM (
                SELECT MIN(due_at) AS at FROM reminders WHERE status = 'pending' AND (notified_at IS NULL OR notified_at <> due_at)
                UNION ALL SELECT MIN(next_run_at) FROM routines WHERE status = 'active'
                UNION ALL SELECT MIN(run_at) FROM wakes WHERE status = 'pending'
            )",
            [],
            |row| row.get(0),
        )
    })
}

/// Everything due by `now`, each handed out once: reminders marked announced, routines moved to their next time (a run
/// too late or still busy is skipped and says why), check-ins marked done (or expired when long missed).
pub fn claim_due<Tz: TimeZone>(db: &Db, tz: &Tz, now: i64) -> Result<Vec<Job>, String> {
    immediate(db, |conn| {
        let mut jobs = Vec::new();

        let ids: Vec<String> = conn
            .prepare(
                "UPDATE reminders SET notified_at = due_at, updated_at = ?1
                 WHERE status = 'pending' AND due_at <= ?1 AND (notified_at IS NULL OR notified_at <> due_at) RETURNING id",
            )?
            .query_map([now], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            if let Some(reminder) = reminder_by_id(conn, &id)? {
                jobs.push(Job::Remind(reminder));
            }
        }

        let woken: Vec<String> = conn
            .prepare(
                "UPDATE wakes SET status = CASE WHEN ?1 - run_at > ?2 THEN 'expired' ELSE 'done' END, updated_at = ?1
                 WHERE status = 'pending' AND run_at <= ?1 RETURNING id",
            )?
            .query_map(params![now, WAKE_EXPIRY], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in woken {
            let wake = conn.query_row(&format!("SELECT {WAKE_COLUMNS} FROM wakes WHERE id = ?1"), [&id], wake_row)?;
            if wake.status == "done" {
                jobs.push(Job::Wake(wake));
            }
        }

        let due: Vec<Routine> = conn
            .prepare(&format!("SELECT {ROUTINE_COLUMNS} FROM routines WHERE status = 'active' AND next_run_at IS NOT NULL AND next_run_at <= ?1"))?
            .query_map([now], routine_row)?
            .collect::<rusqlite::Result<_>>()?;
        for mut routine in due {
            let Some(scheduled) = routine.next_run_at else { continue };
            let next =
                next_run(tz, &routine.days, routine.minute_of_day, routine.schedule.as_ref(), now, Some(scheduled));
            // A one-off is done after its time: it stays, paused, as a record.
            let status = if next.is_some() { "active" } else { "paused" };
            let skip = if now - scheduled > MAX_LATENESS {
                Some(MISSED)
            } else if run_in_progress(conn, &routine.id)? {
                Some(PREVIOUS_STILL_WORKING)
            } else {
                None
            };
            let moved = conn.execute(
                "UPDATE routines SET next_run_at = ?1, status = ?2, last_skip_reason = ?3,
                 last_run_at = CASE WHEN ?3 IS NULL THEN ?4 ELSE last_run_at END, updated_at = ?4
                 WHERE id = ?5 AND status = 'active' AND next_run_at = ?6",
                params![next, status, skip, now, routine.id, scheduled],
            )?;
            if moved == 1 && skip.is_none() {
                let run = insert_run(conn, &routine.id, Some(scheduled), now)?;
                routine.next_run_at = next;
                routine.status = status.to_string();
                routine.last_run_at = Some(now);
                routine.last_skip_reason = None;
                jobs.push(Job::Routine { routine, run });
            }
        }
        Ok(jobs)
    })
}

// ---------------------------------------------------------------- words

/// "Weekdays · 8:00", "Every 2 hours · 9:00–17:00", "Once · Sat 10 Oct 17:30": a routine's schedule for prompts and tools.
pub fn describe_schedule<Tz: TimeZone>(tz: &Tz, routine: &Routine) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let clock = |minute: i64| format!("{}:{:02}", minute / 60, minute % 60);
    match &routine.schedule {
        Some(Repeat::Once { at }) => format!("Once · {}", local(tz, *at).format("%a %-d %b %H:%M")),
        Some(Repeat::Interval { every_minutes, from_minute, to_minute }) => {
            let every = if every_minutes % 60 == 0 {
                format!("Every {} h", every_minutes / 60)
            } else {
                format!("Every {every_minutes} min")
            };
            match (from_minute, to_minute) {
                (Some(from), Some(to)) => format!("{every} · {}–{}", clock(*from), clock(*to)),
                _ => every,
            }
        }
        None => {
            const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            let days = match routine.days.as_slice() {
                [0, 1, 2, 3, 4, 5, 6] => "Every day".to_string(),
                [1, 2, 3, 4, 5] => "Weekdays".to_string(),
                [0, 6] => "Weekends".to_string(),
                days => days.iter().map(|day| NAMES[(*day as usize).min(6)]).collect::<Vec<_>>().join(", "),
            };
            format!("{days} · {}", clock(routine.minute_of_day))
        }
    }
}

/// How long ago, as an agent is told it (the group's transcript, its members' work, Feed posts): "just now",
/// "40 min ago", "18 hr ago", "1 day ago", "3 days ago".
pub(crate) fn ago(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms.max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} hr ago", minutes / 60),
        1440..=2879 => "1 day ago".into(),
        _ => format!("{} days ago", minutes / 1440),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Db {
        Db::in_memory().unwrap()
    }

    use crate::test_support::tz;

    /// Local wall-clock time in `tz()` → ms.
    fn at(text: &str) -> i64 {
        at_local(&tz(), NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap())
    }

    #[test]
    fn weekly_and_interval_times() {
        let tz = tz();
        // Saturday 10 Oct 2026, 05:40.
        let now = at("2026-10-10 05:40");
        assert_eq!(next_weekly(&tz, &[1, 2, 3, 4, 5], 8 * 60, now), Some(at("2026-10-12 08:00")));
        assert_eq!(next_weekly(&tz, &[6], 8 * 60, now), Some(at("2026-10-10 08:00")));
        assert_eq!(next_weekly(&tz, &[6], 5 * 60, now), Some(at("2026-10-17 05:00")));
        assert_eq!(next_weekly(&tz, &[], 5 * 60, now), None);

        let once = Repeat::Once { at: now + MINUTE };
        assert_eq!(next_run(&tz, &[], 0, Some(&once), now, None), Some(now + MINUTE));
        assert_eq!(next_run(&tz, &[], 0, Some(&once), now + MINUTE, Some(now + MINUTE)), None);

        let hourly = Repeat::Interval { every_minutes: 60, from_minute: None, to_minute: None };
        assert_eq!(next_run(&tz, &[], 0, Some(&hourly), now, None), Some(now + 60 * MINUTE));
        // The cadence holds from the last due time when the sweep is a little late.
        assert_eq!(next_run(&tz, &[], 0, Some(&hourly), now + 2 * MINUTE, Some(now)), Some(now + 60 * MINUTE));
        // Within 9:00–17:00: at 16:30 the next is tomorrow at 9:00, not 17:30.
        let office = Repeat::Interval { every_minutes: 60, from_minute: Some(9 * 60), to_minute: Some(17 * 60) };
        assert_eq!(next_run(&tz, &[], 0, Some(&office), at("2026-10-10 16:30"), None), Some(at("2026-10-11 09:00")));
        assert_eq!(next_run(&tz, &[], 0, Some(&office), at("2026-10-10 10:00"), None), Some(at("2026-10-10 11:00")));
        // A window across midnight.
        let night = Repeat::Interval { every_minutes: 60, from_minute: Some(22 * 60), to_minute: Some(2 * 60) };
        assert_eq!(next_run(&tz, &[], 0, Some(&night), at("2026-10-10 23:00"), None), Some(at("2026-10-11 00:00")));
        assert_eq!(next_run(&tz, &[], 0, Some(&night), at("2026-10-11 01:30"), None), Some(at("2026-10-11 22:00")));
    }

    #[test]
    fn repeating_reminders_keep_their_wall_clock_time() {
        let tz = tz();
        let anchor = at("2026-10-09 09:00"); // a Friday
        assert_eq!(next_recurring(&tz, anchor, at("2026-10-09 09:01"), "daily"), at("2026-10-10 09:00"));
        assert_eq!(next_recurring(&tz, anchor, at("2026-10-09 09:01"), "weekdays"), at("2026-10-12 09:00"));
        assert_eq!(next_recurring(&tz, anchor, at("2026-10-09 09:01"), "weekly"), at("2026-10-16 09:00"));
        // Months later it still lands on a Friday at 9:00, the next one after now.
        assert_eq!(next_recurring(&tz, anchor, at("2027-03-03 12:00"), "weekly"), at("2027-03-05 09:00"));
        assert_eq!(next_recurring(&tz, anchor, at("2027-03-03 12:00"), "daily"), at("2027-03-04 09:00"));
    }

    #[test]
    fn reminders_come_due_once_and_snooze_complete_cancel() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        assert_eq!(made(&db).unwrap(), 0);
        let reminder = create_reminder(
            &db,
            NewReminder { title: "  Take   the pills ".into(), due_at: now + 10 * MINUTE, ..Default::default() },
            now,
        )
        .unwrap();
        assert_eq!(reminder.title, "Take the pills");
        assert_eq!(made(&db).unwrap(), 1);
        assert!(
            create_reminder(&db, NewReminder { title: "late".into(), due_at: now - DAY, ..Default::default() }, now)
                .is_err()
        );
        assert!(
            create_reminder(
                &db,
                NewReminder { title: "x".into(), due_at: now, delivery: Some("call".into()), ..Default::default() },
                now
            )
            .is_err()
        );
        assert_eq!(next_due(&db).unwrap(), Some(now + 10 * MINUTE));

        assert!(claim_due(&db, &tz, now + 9 * MINUTE).unwrap().is_empty());
        let jobs = claim_due(&db, &tz, now + 10 * MINUTE).unwrap();
        assert!(matches!(&jobs[..], [Job::Remind(r)] if r.id == reminder.id));
        // Announced once; it stays pending until it's done.
        assert!(claim_due(&db, &tz, now + 11 * MINUTE).unwrap().is_empty());
        assert_eq!(reminders(&db, None).unwrap().len(), 1);
        assert_eq!(next_due(&db).unwrap(), None);

        // Snoozed: due again, announced again.
        assert!(snooze_reminder(&db, &reminder.id, 10., None, now + 11 * MINUTE).unwrap());
        assert!(snooze_reminder(&db, &reminder.id, 0., None, now).is_err());
        assert_eq!(claim_due(&db, &tz, now + 21 * MINUTE).unwrap().len(), 1);

        // An agent can't touch Filo's; done is done.
        assert!(!complete_reminder(&db, &tz, &reminder.id, Some("agent-1"), now).unwrap());
        assert!(complete_reminder(&db, &tz, &reminder.id, None, now + 22 * MINUTE).unwrap());
        assert!(complete_reminder(&db, &tz, &reminder.id, None, now + 22 * MINUTE).unwrap());
        assert!(reminders(&db, None).unwrap().is_empty());
        assert!(!cancel_reminder(&db, "nope", None, now).unwrap());
    }

    #[test]
    fn a_repeating_reminder_moves_on_when_done() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-09 08:00");
        let daily = NewReminder {
            title: "Stretch".into(),
            due_at: at("2026-10-09 09:00"),
            recurrence: Some("daily".into()),
            agent_id: Some("a".into()),
            ..Default::default()
        };
        let reminder = create_reminder(&db, daily, now).unwrap();
        assert_eq!(claim_due(&db, &tz, at("2026-10-09 09:00")).unwrap().len(), 1);
        assert!(complete_reminder(&db, &tz, &reminder.id, Some("a"), at("2026-10-09 09:05")).unwrap());
        let pending = reminders(&db, Some("a")).unwrap();
        assert_eq!(pending[0].due_at, at("2026-10-10 09:00"));
        assert!(reminders(&db, Some("b")).unwrap().is_empty());
        // And it's announced again at its next time.
        assert_eq!(claim_due(&db, &tz, at("2026-10-10 09:00")).unwrap().len(), 1);
    }

    #[test]
    fn routines_run_on_schedule_skip_when_late_or_busy() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        let new = NewRoutine {
            title: "Morning brief".into(),
            instructions: "Summarise the news.".into(),
            days: json!([0, 1, 2, 3, 4, 5, 6]),
            minute_of_day: json!(480.0),
            ..Default::default()
        };
        let routine = create_routine(&db, &tz, new.clone(), now).unwrap();
        assert_eq!(routine.next_run_at, Some(at("2026-10-10 08:00")));
        assert_eq!(routine.days, [0, 1, 2, 3, 4, 5, 6]);

        let jobs = claim_due(&db, &tz, at("2026-10-10 08:00")).unwrap();
        let [Job::Routine { routine: fired, run }] = &jobs[..] else { panic!("{jobs:?}") };
        assert_eq!(fired.next_run_at, Some(at("2026-10-11 08:00")));
        assert_eq!(run.state, "queued");
        // Claimed once.
        assert!(claim_due(&db, &tz, at("2026-10-10 08:00")).unwrap().is_empty());

        // The next day the first run is still working: skipped, with the reason on the routine.
        run_started(&db, &run.id, "chat-1", "turn-1").unwrap();
        assert!(claim_due(&db, &tz, at("2026-10-11 08:00")).unwrap().is_empty());
        let skipped = routine_detail(&db, &routine.id);
        assert_eq!(skipped.last_skip_reason.as_deref(), Some(PREVIOUS_STILL_WORKING));
        assert_eq!(skipped.next_run_at, Some(at("2026-10-12 08:00")));
        assert_eq!(start_run_now(&db, &routine.id, None, now).unwrap_err(), PREVIOUS_STILL_WORKING);

        run_finished(&db, &run.id, Some("Here's your brief."), None, at("2026-10-11 08:01")).unwrap();
        let listed = runs(&db, &routine.id).unwrap();
        assert_eq!((listed[0].state.as_str(), listed[0].summary.as_deref()), ("completed", Some("Here's your brief.")));

        // The Mac was off for the morning: more than 3 hours late is skipped, not run at noon.
        assert!(claim_due(&db, &tz, at("2026-10-12 11:30")).unwrap().is_empty());
        let missed = routine_detail(&db, &routine.id);
        assert_eq!(missed.last_skip_reason.as_deref(), Some(MISSED));
        assert_eq!(missed.next_run_at, Some(at("2026-10-13 08:00")));

        // Run now starts one whenever nothing is running.
        let (_, manual) = start_run_now(&db, &routine.id, None, now).unwrap().unwrap();
        assert_eq!(manual.due_at, None);
        assert_eq!(start_run_now(&db, "gone", None, now).unwrap(), None);
    }

    fn routine_detail(db: &Db, id: &str) -> Routine {
        routine(db, id, None).unwrap().unwrap()
    }

    #[test]
    fn routines_update_pause_once_and_limits() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        let new = NewRoutine {
            title: "Brief".into(),
            instructions: "Do it.".into(),
            days: json!([1]),
            minute_of_day: json!(600),
            ..Default::default()
        };
        let routine = create_routine(&db, &tz, new.clone(), now).unwrap();
        assert_eq!(routine.next_run_at, Some(at("2026-10-12 10:00")));

        assert!(
            update_routine(&db, &tz, &routine.id, &json!({ "days": [6.0], "minuteOfDay": 420.0 }), None, now).unwrap()
        );
        assert_eq!(routine_detail(&db, &routine.id).next_run_at, Some(at("2026-10-10 07:00")));
        assert!(update_routine(&db, &tz, &routine.id, &json!({ "status": "paused" }), None, now).unwrap());
        assert_eq!(routine_detail(&db, &routine.id).next_run_at, None);
        assert!(claim_due(&db, &tz, at("2026-10-10 07:00")).unwrap().is_empty());
        assert!(
            update_routine(&db, &tz, &routine.id, &json!({ "status": "active", "title": " New  name " }), None, now)
                .unwrap()
        );
        let active = routine_detail(&db, &routine.id);
        assert_eq!((active.title.as_str(), active.next_run_at), ("New name", Some(at("2026-10-10 07:00"))));
        assert!(update_routine(&db, &tz, &routine.id, &json!({ "days": [] }), None, now).is_err());
        assert!(update_routine(&db, &tz, &routine.id, &json!({ "minuteOfDay": 1440 }), None, now).is_err());
        assert!(!update_routine(&db, &tz, "gone", &json!({ "title": "x" }), None, now).unwrap());
        assert!(!update_routine(&db, &tz, &routine.id, &json!({ "title": "x" }), Some("agent"), now).unwrap());

        // A one-off runs once, then rests paused.
        let once = NewRoutine {
            title: "Once".into(),
            instructions: "Once.".into(),
            schedule: json!({ "kind": "once", "at": now + 5 * MINUTE }),
            ..Default::default()
        };
        let once = create_routine(&db, &tz, once, now).unwrap();
        assert_eq!(once.days, [0, 1, 2, 3, 4, 5, 6]);
        let jobs = claim_due(&db, &tz, now + 5 * MINUTE).unwrap();
        assert!(
            matches!(&jobs[..], [Job::Routine { routine, .. }] if routine.id == once.id && routine.status == "paused")
        );
        assert!(update_routine(&db, &tz, &once.id, &json!({ "status": "active" }), None, now + 6 * MINUTE).is_err());
        let past = NewRoutine { schedule: json!({ "kind": "once", "at": now - 1 }), ..new.clone() };
        assert!(create_routine(&db, &tz, past, now).is_err());
        let fast = NewRoutine { schedule: json!({ "kind": "interval", "everyMinutes": 5 }), ..new.clone() };
        assert!(create_routine(&db, &tz, fast, now).is_err());

        for _ in 2..MAX_ROUTINES {
            create_routine(&db, &tz, new.clone(), now).unwrap();
        }
        assert!(create_routine(&db, &tz, new.clone(), now).unwrap_err().contains("limit"));
        assert!(delete_routine(&db, &once.id, None).unwrap());
        assert!(!delete_routine(&db, &once.id, None).unwrap());
        create_routine(&db, &tz, new, now).unwrap();
    }

    #[test]
    fn wakes_fire_once_and_expire_when_long_missed() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        let soon = NewWake {
            title: "Look at the essay".into(),
            note: "Did they finish the intro?".into(),
            run_at: now + 30 * MINUTE,
            chat_id: Some("chat-1".into()),
            agent_id: None,
        };
        let wake = create_wake(&db, soon.clone(), now).unwrap();
        assert!(create_wake(&db, NewWake { run_at: now, ..soon.clone() }, now).is_err());
        assert!(create_wake(&db, NewWake { run_at: now + 61 * DAY, ..soon.clone() }, now).is_err());
        let jobs = claim_due(&db, &tz, now + 30 * MINUTE).unwrap();
        assert!(matches!(&jobs[..], [Job::Wake(w)] if w.id == wake.id && w.chat_id.as_deref() == Some("chat-1")));
        assert!(claim_due(&db, &tz, now + 31 * MINUTE).unwrap().is_empty());

        let missed = create_wake(&db, soon.clone(), now).unwrap();
        assert!(claim_due(&db, &tz, now + 30 * MINUTE + WAKE_EXPIRY + MINUTE).unwrap().is_empty());
        assert!(wakes(&db, None).unwrap().iter().all(|wake| wake.id != missed.id));

        let cancelled = create_wake(&db, NewWake { agent_id: Some("a".into()), ..soon.clone() }, now).unwrap();
        assert!(!cancel_wake(&db, &cancelled.id, Some("b"), now).unwrap());
        assert!(cancel_wake(&db, &cancelled.id, Some("a"), now).unwrap());
        assert!(claim_due(&db, &tz, now + DAY).unwrap().is_empty());
    }

    #[test]
    fn an_agent_removed_gives_its_items_to_filo_or_takes_them() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        let mine = |agent: &str| NewReminder {
            title: "r".into(),
            due_at: now + DAY,
            agent_id: Some(agent.into()),
            ..Default::default()
        };
        create_reminder(&db, mine("a"), now).unwrap();
        create_reminder(&db, mine("b"), now).unwrap();
        let routine = NewRoutine {
            title: "t".into(),
            instructions: "i".into(),
            days: json!([1]),
            minute_of_day: json!(0),
            agent_id: Some("a".into()),
            ..Default::default()
        };
        create_routine(&db, &tz, routine.clone(), now).unwrap();
        create_routine(&db, &tz, NewRoutine { agent_id: Some("b".into()), ..routine }, now).unwrap();
        assert_eq!(agent_counts(&db, "a").unwrap(), (1, 1));

        agent_removed(&db, "a", false, None, now).unwrap();
        assert_eq!(agent_counts(&db, "a").unwrap(), (0, 0));
        assert_eq!(reminders(&db, None).unwrap().iter().filter(|r| r.agent_id.is_none()).count(), 1);
        // Given to the main agent, they're its own.
        agent_removed(&db, "b", false, Some("main"), now).unwrap();
        assert_eq!(agent_counts(&db, "main").unwrap(), (1, 1));
        agent_removed(&db, "main", true, None, now).unwrap();
        assert_eq!((reminders(&db, None).unwrap().len(), routines(&db, None).unwrap().len()), (1, 1));
        let left = routines(&db, None).unwrap().remove(0);
        assert!(set_routine_agent(&db, &left.id, Some("chef"), now).unwrap());
        assert_eq!(routines(&db, Some("chef")).unwrap().len(), 1);
    }

    #[test]
    fn the_main_agent_picked_has_what_belongs_to_no_agent() {
        let db = Db::in_memory().unwrap();
        crate::app_data::add_agent(&db, &crate::app_data::tests::agent("chef", "Chef", None, 1.)).unwrap();
        crate::app_data::set_profile(&db, crate::app_data::MAIN_AGENT, Some(&"chef".into())).unwrap();
        let now = at("2026-10-10 05:40");
        let page =
            create_reminder(&db, NewReminder { title: "r".into(), due_at: now + DAY, ..Default::default() }, now)
                .unwrap();
        assert_eq!(reminders(&db, Some("chef")).unwrap().len(), 1);
        assert!(reminders(&db, Some(crate::apps::FILO_AGENT)).unwrap().is_empty());
        assert!(snooze_reminder(&db, &page.id, 5., Some("chef"), now).unwrap());
        assert!(!cancel_reminder(&db, &page.id, Some(crate::apps::FILO_AGENT), now).unwrap());
    }

    #[test]
    fn the_filo_agent_has_filos_items_but_not_other_agents() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        let filo = Some(crate::apps::FILO_AGENT);
        // One made on the Scheduled page (no agent), one by the Filo agent, one by another agent.
        let reminder = |agent: Option<&str>| NewReminder {
            title: "r".into(),
            due_at: now + DAY,
            agent_id: agent.map(str::to_string),
            ..Default::default()
        };
        let page = create_reminder(&db, reminder(None), now).unwrap();
        create_reminder(&db, reminder(filo), now).unwrap();
        let chef = create_reminder(&db, reminder(Some("chef")), now).unwrap();
        assert_eq!(reminders(&db, filo).unwrap().len(), 2);
        assert!(reminders(&db, filo).unwrap().iter().all(|r| r.id != chef.id));
        assert_eq!(reminders(&db, Some("chef")).unwrap().len(), 1);
        assert!(snooze_reminder(&db, &page.id, 5., filo, now).unwrap());
        assert!(!cancel_reminder(&db, &chef.id, filo, now).unwrap());
        assert!(!cancel_reminder(&db, &page.id, Some("chef"), now).unwrap());

        let routine = NewRoutine {
            title: "t".into(),
            instructions: "i".into(),
            days: json!([1]),
            minute_of_day: json!(0),
            ..Default::default()
        };
        let mine = create_routine(&db, &tz, routine.clone(), now).unwrap();
        let theirs = create_routine(&db, &tz, NewRoutine { agent_id: Some("chef".into()), ..routine }, now).unwrap();
        assert_eq!(routines(&db, filo).unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), [mine.id.as_str()]);
        assert!(update_routine(&db, &tz, &mine.id, &json!({ "title": "Renamed" }), filo, now).unwrap());
        assert!(!delete_routine(&db, &theirs.id, filo).unwrap());

        let wake = create_wake(
            &db,
            NewWake { title: "w".into(), note: String::new(), run_at: now + DAY, chat_id: None, agent_id: None },
            now,
        )
        .unwrap();
        assert_eq!(wakes(&db, filo).unwrap().len(), 1);
        assert!(wakes(&db, Some("chef")).unwrap().is_empty());
        assert!(cancel_wake(&db, &wake.id, filo, now).unwrap());
    }

    #[test]
    fn orphaned_runs_are_settled() {
        let (db, tz) = (db(), tz());
        let now = at("2026-10-10 05:40");
        let new = NewRoutine {
            title: "t".into(),
            instructions: "i".into(),
            days: json!([6]),
            minute_of_day: json!(6 * 60),
            ..Default::default()
        };
        let routine = create_routine(&db, &tz, new, now).unwrap();
        let (_, run) = start_run_now(&db, &routine.id, None, now).unwrap().unwrap();
        // Queued a moment ago: left alone. Started by a process that's gone: failed.
        assert_eq!(settle_orphaned_runs(&db, now + MINUTE).unwrap(), 0);
        assert!(active_run_turns(&db).unwrap().is_empty(), "queued isn't working yet");
        run_started(&db, &run.id, "c", "t").unwrap();
        assert_eq!(settle_orphaned_runs(&db, now + MINUTE).unwrap(), 0, "this process is alive");
        assert_eq!(active_run_turns(&db).unwrap(), ["t"]);
        db.with(|conn| conn.execute("UPDATE routine_runs SET pid = 999999 WHERE id = ?1", [&run.id])).unwrap();
        assert!(active_run_turns(&db).unwrap().is_empty(), "its process is gone");
        assert_eq!(settle_orphaned_runs(&db, now + MINUTE).unwrap(), 1);
        assert_eq!(runs(&db, &routine.id).unwrap()[0].state, "failed");
        let (_, stale) = start_run_now(&db, &routine.id, None, now).unwrap().unwrap();
        assert_eq!(settle_orphaned_runs(&db, now + QUEUED_EXPIRY + MINUTE).unwrap(), 1);
        assert!(runs(&db, &routine.id).unwrap().iter().any(|run| run.id == stale.id && run.state == "failed"));
    }

    #[test]
    fn schedules_read_as_words() {
        let tz = tz();
        let now = at("2026-10-10 05:40");
        let db = db();
        let weekdays = NewRoutine {
            title: "t".into(),
            instructions: "i".into(),
            days: json!([1, 2, 3, 4, 5]),
            minute_of_day: json!(8 * 60 + 5),
            ..Default::default()
        };
        let routine = create_routine(&db, &tz, weekdays, now).unwrap();
        assert_eq!(describe_schedule(&tz, &routine), "Weekdays · 8:05");
        let mut interval = routine.clone();
        interval.schedule =
            Some(Repeat::Interval { every_minutes: 120, from_minute: Some(540), to_minute: Some(1020) });
        assert_eq!(describe_schedule(&tz, &interval), "Every 2 h · 9:00–17:00");
        assert!(repeat_of(&json!({ "kind": "interval", "everyMinutes": 60, "fromMinute": 1 }), now).is_err());
        assert!(repeat_of(&json!({ "kind": "weekly" }), now).is_err());
        assert_eq!(repeat_of(&Value::Null, now).unwrap(), None);
    }
}

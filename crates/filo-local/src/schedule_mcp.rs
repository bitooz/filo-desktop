// The MCP tools for scheduled work (schedule.rs): what the coding tool calls to make, list and change reminders,
// routines and its own check-ins, from the conversation it's in.
//
// In an agent's conversation the agent owns what it makes and only sees its own (the agent is found from the
// conversation, from the agents in the database); in a chat of no agent's, what's made is no agent's and everything is
// seen. The user's main agent (main_agent.rs) also has what belongs to no agent, like what the user made on the
// Scheduled page (schedule.rs `Agent`). In a turn of the agents' group chat a check-in comes back in the group
// (`call`'s `check_in_chat`), as that member's turn there. Times are this Mac's
// local time: tools take `in_minutes` or a local date and time ("2026-10-10 17:30", "17:30" for the next one), and
// each turn's message says what time it is now (`now_note`), since the CLI doesn't know. Not the tool list: it starts
// the CLI's cached prompt, and a clock there would make every new session in a new minute write it all again.

use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde_json::{Value, json};

use crate::db::Db;
use crate::schedule::{self, MINUTE, NewReminder, NewRoutine, NewWake, Reminder, Routine, Wake};

const NAMES: &[&str] = &[
    "create_reminder",
    "list_reminders",
    "complete_reminder",
    "snooze_reminder",
    "cancel_reminder",
    "create_routine",
    "list_routines",
    "update_routine",
    "delete_routine",
    "schedule_check_in",
    "list_check_ins",
    "cancel_check_in",
];

/// Whether `name` is one of these tools.
pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

/// What time it is here: "Sat 10 Oct 2026, 05:40 (UTC+05:00)".
fn now_line<Tz: TimeZone>(tz: &Tz, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let time =
        tz.timestamp_millis_opt(now).earliest().map(|time| time.format("%a %-d %b %Y, %H:%M (UTC%:z)").to_string());
    time.unwrap_or_default()
}

/// What a turn's message says of the time, for these tools: "It's now Sat 10 Oct 2026, 05:40 (UTC+05:00), the user's
/// local time."
pub fn now_note<Tz: TimeZone>(tz: &Tz, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!("It's now {}, the user's local time.", now_line(tz, now))
}

/// The tools, the same text every time (no clock: see the top of the file).
pub fn tools() -> Vec<Value> {
    let times = "Times are the user's local time; your message says what time it is now.";
    let when = json!({
        "in_minutes": { "type": "number", "description": "How many minutes from now." },
        "at": { "type": "string", "description": "Or a local date and time, \"YYYY-MM-DD HH:MM\" (or just \"HH:MM\" for the next one)." }
    });
    let id = json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false });
    let empty = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    let annotations = |read_only: bool, destructive: bool| json!({ "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": false, "openWorldHint": false });
    let with = |base: &Value, extra: Value| {
        let mut properties = base.as_object().cloned().unwrap_or_default();
        properties.extend(extra.as_object().cloned().unwrap_or_default());
        Value::Object(properties)
    };
    vec![
        json!({
            "name": "create_reminder",
            "title": "Create reminder",
            "description": format!("Remind the user of something at a time: a notification on their Mac (it comes even when the Filo app is closed), and it shows on the Scheduled page until they mark it done. Give `in_minutes` or `at`. {times}"),
            "inputSchema": {
                "type": "object",
                "properties": with(&when, json!({
                    "title": { "type": "string", "description": "What to remind them of, short (\"Take the pills\")." },
                    "note": { "type": "string", "description": "Details, if any." },
                    "repeat": { "type": "string", "enum": ["daily", "weekdays", "weekly"], "description": "Repeat at the same time; done moves it to the next one." },
                    "alarm": { "type": "boolean", "description": "Mark it as an alarm (something they mustn't miss)." }
                })),
                "required": ["title"],
                "additionalProperties": false
            },
            "annotations": annotations(false, false)
        }),
        json!({
            "name": "list_reminders",
            "title": "List reminders",
            "description": format!("The user's pending reminders, with their ids and times. {times}"),
            "inputSchema": empty,
            "annotations": annotations(true, false)
        }),
        json!({ "name": "complete_reminder", "title": "Complete reminder", "description": "Mark a reminder done, by id (a repeating one moves to its next time).", "inputSchema": id, "annotations": annotations(false, false) }),
        json!({
            "name": "snooze_reminder",
            "title": "Snooze reminder",
            "description": "Remind again in `minutes` (1 to 10080), by id.",
            "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "minutes": { "type": "number" } }, "required": ["id", "minutes"], "additionalProperties": false },
            "annotations": annotations(false, false)
        }),
        json!({ "name": "cancel_reminder", "title": "Cancel reminder", "description": "Cancel a reminder, by id.", "inputSchema": id, "annotations": annotations(false, true) }),
        json!({
            "name": "create_routine",
            "title": "Create routine",
            "description": format!("Do a job for the user on a schedule, like a morning brief: at each time you get the instructions in this conversation and do them, and the user is notified of what you send. Weekly at `time` on `days` by default; or once at `once_at`; or every `every_minutes` (30 to 10080), optionally only between `from_time` and `to_time`. At most {} routines. {times}", schedule::MAX_ROUTINES),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A short name (\"Morning brief\")." },
                    "instructions": { "type": "string", "description": "What to do each time, in full: you read them later with no other context." },
                    "time": { "type": "string", "description": "\"HH:MM\", 24-hour local time (weekly routines)." },
                    "days": { "description": "Weekdays: \"daily\", \"weekdays\", \"weekends\", or a list like [\"mon\", \"thu\"] or [1, 4] (0 = Sunday). Default every day.", "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": ["string", "integer"] } }] },
                    "once_at": { "type": "string", "description": "Run once, at this local \"YYYY-MM-DD HH:MM\"." },
                    "every_minutes": { "type": "integer", "description": "Run every this many minutes (30 to 10080)." },
                    "from_time": { "type": "string", "description": "With every_minutes: only from this \"HH:MM\"…" },
                    "to_time": { "type": "string", "description": "…until this \"HH:MM\"." }
                },
                "required": ["title", "instructions"],
                "additionalProperties": false
            },
            "annotations": annotations(false, false)
        }),
        json!({
            "name": "list_routines",
            "title": "List routines",
            "description": format!("The user's routines, with their ids, schedules and next runs. {times}"),
            "inputSchema": empty,
            "annotations": annotations(true, false)
        }),
        json!({
            "name": "update_routine",
            "title": "Update routine",
            "description": "Change a routine, by id: only what's given changes. `active: false` pauses it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "instructions": { "type": "string" },
                    "time": { "type": "string", "description": "\"HH:MM\"." },
                    "days": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": ["string", "integer"] } }] },
                    "active": { "type": "boolean" }
                },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": annotations(false, false)
        }),
        json!({ "name": "delete_routine", "title": "Delete routine", "description": "Delete a routine, by id. What it already sent stays.", "inputSchema": id, "annotations": annotations(false, true) }),
        json!({
            "name": "schedule_check_in",
            "title": "Schedule check-in",
            "description": format!("Plan to come back to this conversation yourself at a time (\"see whether they finished the essay\"): then you get the title and note here and can follow up with the user. Private to you; for the user to be reminded, use create_reminder. Give `in_minutes` or `at`. {times}"),
            "inputSchema": {
                "type": "object",
                "properties": with(&when, json!({
                    "title": { "type": "string", "description": "What to check." },
                    "note": { "type": "string", "description": "What you'll need to know then." }
                })),
                "required": ["title"],
                "additionalProperties": false
            },
            "annotations": annotations(false, false)
        }),
        json!({ "name": "list_check_ins", "title": "List check-ins", "description": format!("The check-ins you have planned, with their ids. {times}"), "inputSchema": empty, "annotations": annotations(true, false) }),
        json!({ "name": "cancel_check_in", "title": "Cancel check-in", "description": "Cancel a planned check-in, by id.", "inputSchema": id, "annotations": annotations(false, true) }),
    ]
}

/// The agent whose conversation `chat_id` is.
fn agent_of(db: &Db, chat_id: &str) -> Option<String> {
    crate::app_data::agent_of_chat(db, chat_id).ok().flatten().map(|agent| agent.id)
}

/// Runs one of these tools for the conversation `chat_id` (whose agent owns what's made). A check-in comes back in
/// `check_in_chat` when given (the group's conversation, in a group turn), else in `chat_id`.
pub fn call(
    db_path: &Path,
    chat_id: &str,
    check_in_chat: Option<&str>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let db = Db::open(db_path).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    let agent = agent_of(&db, chat_id);
    call_with(&db, &chrono::Local, chat_id, check_in_chat, agent.as_deref(), name, args, crate::now_ms() as i64)
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

fn id(args: &Value) -> Result<&str, String> {
    crate::mcp::id_arg(args, "Give the item's `id` (from the list tool).")
}

/// "HH:MM" → minute of the day.
fn minute_of_day(text: &str) -> Result<i64, String> {
    let time = NaiveTime::parse_from_str(text.trim(), "%H:%M")
        .map_err(|_| format!("{text:?} isn't a time like \"08:30\"."))?;
    Ok(chrono::Timelike::hour(&time) as i64 * 60 + chrono::Timelike::minute(&time) as i64)
}

/// A local date and time (or an RFC 3339 instant, or "HH:MM" for its next time) → ms.
fn parse_at<Tz: TimeZone>(tz: &Tz, text: &str, now: i64) -> Result<i64, String> {
    let text = text.trim();
    if let Ok(instant) = DateTime::parse_from_rfc3339(text) {
        return Ok(instant.timestamp_millis());
    }
    for format in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Ok(schedule::at_local(tz, naive));
        }
    }
    if let Ok(minute) = minute_of_day(text) {
        return schedule::next_weekly(tz, &[0, 1, 2, 3, 4, 5, 6], minute, now)
            .ok_or_else(|| "That time doesn't exist.".into());
    }
    if NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok() {
        return Err(format!("{text:?} needs a time too, like \"{text} 09:00\"."));
    }
    Err(format!("{text:?} isn't a date and time like \"2026-10-10 17:30\"."))
}

/// `in_minutes` or `at`.
fn when<Tz: TimeZone>(tz: &Tz, args: &Value, now: i64) -> Result<i64, String> {
    if let Some(minutes) = args.get("in_minutes").and_then(Value::as_f64) {
        if !minutes.is_finite() || minutes <= 0. {
            return Err("`in_minutes` must be more than 0.".into());
        }
        return Ok(now + (minutes * MINUTE as f64).round() as i64);
    }
    match text(args, "at") {
        Some(at) => parse_at(tz, at, now),
        None => Err("Say when: `in_minutes` or `at`.".into()),
    }
}

/// `days` as the tools take them → weekday numbers.
fn days_of(value: &Value) -> Result<Value, String> {
    const SHORT: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let day = |value: &Value| -> Result<u8, String> {
        if let Some(number) = value.as_u64().filter(|number| *number <= 6) {
            return Ok(number as u8);
        }
        let name = value.as_str().map(|name| name.trim().to_lowercase()).unwrap_or_default();
        SHORT
            .iter()
            .position(|short| name.starts_with(short))
            .map(|index| index as u8)
            .ok_or_else(|| format!("{value} isn't a weekday."))
    };
    let days: Vec<u8> = match value {
        Value::String(text) => match text.trim().to_lowercase().as_str() {
            "daily" | "every day" | "everyday" => vec![0, 1, 2, 3, 4, 5, 6],
            "weekdays" => vec![1, 2, 3, 4, 5],
            "weekends" => vec![0, 6],
            _ => text.split(',').map(|part| day(&json!(part))).collect::<Result<_, _>>()?,
        },
        Value::Array(list) => list.iter().map(day).collect::<Result<_, _>>()?,
        _ => return Err("`days` is \"daily\", \"weekdays\", \"weekends\" or a list of weekdays.".into()),
    };
    Ok(json!(days))
}

/// "Sat 10 Oct 17:30 (in 2 h 5 min)".
fn describe<Tz: TimeZone>(tz: &Tz, at: i64, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = tz
        .timestamp_millis_opt(at)
        .earliest()
        .map(|time| time.format("%a %-d %b %H:%M").to_string())
        .unwrap_or_default();
    let minutes = (at - now).abs() / MINUTE;
    let span = if minutes < 60 {
        format!("{minutes} min")
    } else if minutes < 48 * 60 {
        format!("{} h {} min", minutes / 60, minutes % 60)
    } else {
        format!("{} days", minutes / (24 * 60))
    };
    if at >= now { format!("{local} (in {span})") } else { format!("{local} ({span} ago)") }
}

fn reminder_line<Tz: TimeZone>(tz: &Tz, reminder: &Reminder, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let repeat = reminder.recurrence.as_deref().map(|repeat| format!(", repeats {repeat}")).unwrap_or_default();
    let note = reminder.note.as_deref().map(|note| format!(" — {note}")).unwrap_or_default();
    format!("- [{}] {}{note}: {}{repeat}", reminder.id, reminder.title, describe(tz, reminder.due_at, now))
}

fn routine_line<Tz: TimeZone>(tz: &Tz, routine: &Routine, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let next = match routine.next_run_at {
        Some(next) if routine.active() => format!("next {}", describe(tz, next, now)),
        _ => "paused".into(),
    };
    let skipped =
        routine.last_skip_reason.as_deref().map(|reason| format!(", skipped last time: {reason}")).unwrap_or_default();
    format!(
        "- [{}] {} ({}; {next}{skipped}): {}",
        routine.id,
        routine.title,
        schedule::describe_schedule(tz, routine),
        routine.instructions
    )
}

fn wake_line<Tz: TimeZone>(tz: &Tz, wake: &Wake, now: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let note = if wake.note.is_empty() { String::new() } else { format!(" — {}", wake.note) };
    format!("- [{}] {}{note}: {}", wake.id, wake.title, describe(tz, wake.run_at, now))
}

fn answer(changed: bool, done: &str) -> Result<String, String> {
    if changed { Ok(done.into()) } else { Err("There's nothing with that id (or it isn't yours).".into()) }
}

/// The routine fields the tools give, as the pages send them (`days`, `minuteOfDay`, `schedule`).
fn routine_fields<Tz: TimeZone>(tz: &Tz, args: &Value, now: i64) -> Result<(Value, Value, Value), String> {
    let once = text(args, "once_at").map(|at| parse_at(tz, at, now)).transpose()?;
    let every = args.get("every_minutes").filter(|value| !value.is_null());
    let schedule = match (once, every) {
        (Some(_), Some(_)) => return Err("Give once_at or every_minutes, not both.".into()),
        (Some(at), None) => json!({ "kind": "once", "at": at }),
        (None, Some(every)) => {
            let mut schedule = json!({ "kind": "interval", "everyMinutes": every });
            match (text(args, "from_time"), text(args, "to_time")) {
                (Some(from), Some(to)) => {
                    schedule["fromMinute"] = json!(minute_of_day(from)?);
                    schedule["toMinute"] = json!(minute_of_day(to)?);
                }
                (None, None) => {}
                _ => return Err("from_time and to_time go together.".into()),
            }
            schedule
        }
        (None, None) => Value::Null,
    };
    let days = match args.get("days").filter(|value| !value.is_null()) {
        Some(days) => days_of(days)?,
        None if schedule.is_null() => json!([0, 1, 2, 3, 4, 5, 6]),
        None => Value::Null,
    };
    let minute = match text(args, "time") {
        Some(time) => json!(minute_of_day(time)?),
        None if schedule.is_null() => {
            return Err("Say what time it runs (`time`, \"HH:MM\"), or use once_at or every_minutes.".into());
        }
        None => Value::Null,
    };
    Ok((days, minute, schedule))
}

#[allow(clippy::too_many_arguments)]
fn call_with<Tz: TimeZone>(
    db: &Db,
    tz: &Tz,
    chat_id: &str,
    check_in_chat: Option<&str>,
    agent: Option<&str>,
    name: &str,
    args: &Value,
    now: i64,
) -> Result<String, String>
where
    Tz::Offset: std::fmt::Display,
{
    let owner = agent.map(str::to_string);
    match name {
        "create_reminder" => {
            let due_at = when(tz, args, now)?;
            let new = NewReminder {
                title: text(args, "title").unwrap_or_default().to_string(),
                note: text(args, "note").map(str::to_string),
                due_at,
                recurrence: text(args, "repeat").map(str::to_string),
                delivery: (args.get("alarm").and_then(Value::as_bool) == Some(true)).then(|| "alarm".to_string()),
                agent_id: owner,
                chat_id: Some(chat_id.to_string()),
            };
            let reminder = schedule::create_reminder(db, new, now)?;
            Ok(format!(
                "Reminder set (id {}): {}, {}. The user gets a notification then.",
                reminder.id,
                reminder.title,
                describe(tz, reminder.due_at, now)
            ))
        }
        "list_reminders" => {
            let mut list = schedule::reminders(db, agent)?;
            list.sort_by_key(|reminder| reminder.due_at);
            Ok(if list.is_empty() {
                "No pending reminders.".into()
            } else {
                list.iter().map(|reminder| reminder_line(tz, reminder, now)).collect::<Vec<_>>().join("\n")
            })
        }
        "complete_reminder" => answer(schedule::complete_reminder(db, tz, id(args)?, agent, now)?, "Marked done."),
        "snooze_reminder" => {
            let minutes = args.get("minutes").and_then(Value::as_f64).ok_or("Give `minutes`.")?;
            answer(
                schedule::snooze_reminder(db, id(args)?, minutes, agent, now)?,
                &format!("Snoozed: due again {}.", describe(tz, now + minutes.floor().max(1.) as i64 * MINUTE, now)),
            )
        }
        "cancel_reminder" => answer(schedule::cancel_reminder(db, id(args)?, agent, now)?, "Cancelled."),
        "create_routine" => {
            let (days, minute_of_day, schedule) = routine_fields(tz, args, now)?;
            let new = NewRoutine {
                title: text(args, "title").unwrap_or_default().to_string(),
                instructions: text(args, "instructions").unwrap_or_default().to_string(),
                days,
                minute_of_day,
                schedule,
                agent_id: owner,
            };
            let routine = schedule::create_routine(db, tz, new, now)?;
            let next = routine.next_run_at.map(|next| describe(tz, next, now)).unwrap_or_else(|| "never".into());
            Ok(format!(
                "Routine created (id {}): {}, {}. First run {next}. The user can change or pause it on the Scheduled page.",
                routine.id,
                routine.title,
                schedule::describe_schedule(tz, &routine)
            ))
        }
        "list_routines" => {
            let list = schedule::routines(db, agent)?;
            Ok(if list.is_empty() {
                "No routines.".into()
            } else {
                list.iter().map(|routine| routine_line(tz, routine, now)).collect::<Vec<_>>().join("\n")
            })
        }
        "update_routine" => {
            let mut patch = json!({});
            for key in ["title", "instructions"] {
                if let Some(value) = text(args, key) {
                    patch[key] = json!(value);
                }
            }
            if let Some(time) = text(args, "time") {
                patch["minuteOfDay"] = json!(minute_of_day(time)?);
            }
            if let Some(days) = args.get("days").filter(|value| !value.is_null()) {
                patch["days"] = days_of(days)?;
            }
            if let Some(active) = args.get("active").and_then(Value::as_bool) {
                patch["status"] = json!(if active { "active" } else { "paused" });
            }
            let id = id(args)?;
            if !schedule::update_routine(db, tz, id, &patch, agent, now)? {
                return answer(false, "");
            }
            let routine = schedule::routine(db, id, agent)?.ok_or("The routine is gone.")?;
            Ok(format!("Updated:\n{}", routine_line(tz, &routine, now)))
        }
        "delete_routine" => answer(schedule::delete_routine(db, id(args)?, agent)?, "Deleted."),
        "schedule_check_in" => {
            let run_at = when(tz, args, now)?;
            let new = NewWake {
                title: text(args, "title").unwrap_or_default().to_string(),
                note: text(args, "note").unwrap_or_default().to_string(),
                run_at,
                chat_id: Some(check_in_chat.unwrap_or(chat_id).to_string()),
                agent_id: owner,
            };
            let wake = schedule::create_wake(db, new, now)?;
            let place = if check_in_chat.is_some() { "in the group chat" } else { "in this conversation" };
            Ok(format!(
                "Check-in planned (id {}) for {}. You'll be back {place} then.",
                wake.id,
                describe(tz, wake.run_at, now)
            ))
        }
        "list_check_ins" => {
            let list = schedule::wakes(db, agent)?;
            Ok(if list.is_empty() {
                "No check-ins planned.".into()
            } else {
                list.iter().map(|wake| wake_line(tz, wake, now)).collect::<Vec<_>>().join("\n")
            })
        }
        "cancel_check_in" => answer(schedule::cancel_wake(db, id(args)?, agent, now)?, "Cancelled."),
        other => Err(format!("Unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::tz;

    fn at(text: &str) -> i64 {
        schedule::at_local(&tz(), NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap())
    }

    #[test]
    fn every_tool_is_listed_and_handled() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(names, NAMES);
        // No clock in the descriptions: the time goes with each message.
        assert_eq!(serde_json::to_string(&tools).unwrap(), serde_json::to_string(&super::tools()).unwrap());
        assert!(!serde_json::to_string(&tools).unwrap().contains("it's now"));
        assert_eq!(
            now_note(&tz(), at("2026-10-10 05:40")),
            "It's now Sat 10 Oct 2026, 05:40 (UTC+05:00), the user's local time."
        );
    }

    #[test]
    fn times_parse_in_local_time() {
        let (tz, now) = (tz(), at("2026-10-10 05:40"));
        assert_eq!(parse_at(&tz, "2026-10-10 17:30", now).unwrap(), at("2026-10-10 17:30"));
        assert_eq!(parse_at(&tz, "2026-10-10T17:30", now).unwrap(), at("2026-10-10 17:30"));
        assert_eq!(parse_at(&tz, "2026-10-10T12:30:00Z", now).unwrap(), at("2026-10-10 17:30"));
        assert_eq!(parse_at(&tz, "05:00", now).unwrap(), at("2026-10-11 05:00"));
        assert_eq!(parse_at(&tz, "06:00", now).unwrap(), at("2026-10-10 06:00"));
        assert!(parse_at(&tz, "2026-10-10", now).unwrap_err().contains("needs a time"));
        assert!(parse_at(&tz, "tomorrow", now).is_err());
        assert_eq!(when(&tz, &json!({ "in_minutes": 2 }), now).unwrap(), now + 2 * MINUTE);
        assert!(when(&tz, &json!({}), now).is_err());
        assert_eq!(days_of(&json!("weekdays")).unwrap(), json!([1, 2, 3, 4, 5]));
        assert_eq!(days_of(&json!(["Mon", "thursday", 0])).unwrap(), json!([1, 4, 0]));
        assert!(days_of(&json!(["someday"])).is_err());
    }

    #[test]
    fn tools_make_list_and_change_scheduled_work() {
        let (db, tz, now) = (Db::in_memory().unwrap(), tz(), at("2026-10-10 05:40"));
        let call =
            |agent: Option<&str>, name: &str, args: Value| call_with(&db, &tz, "chat-1", None, agent, name, &args, now);

        let made =
            call(None, "create_reminder", json!({ "title": "Stretch", "in_minutes": 2, "repeat": "daily" })).unwrap();
        assert!(made.contains("Sat 10 Oct 05:42 (in 2 min)"), "{made}");
        let reminder = schedule::reminders(&db, None).unwrap().remove(0);
        assert_eq!((reminder.chat_id.as_deref(), reminder.recurrence.as_deref()), (Some("chat-1"), Some("daily")));
        assert!(call(None, "list_reminders", json!({})).unwrap().contains(&reminder.id));
        // An agent sees and touches only its own.
        assert_eq!(call(Some("agent-1"), "list_reminders", json!({})).unwrap(), "No pending reminders.");
        assert!(call(Some("agent-1"), "cancel_reminder", json!({ "id": reminder.id })).is_err());
        assert!(call(None, "snooze_reminder", json!({ "id": reminder.id, "minutes": 10 })).unwrap().contains("05:50"));
        call(None, "cancel_reminder", json!({ "id": reminder.id })).unwrap();

        let made = call(
            Some("agent-1"),
            "create_routine",
            json!({ "title": "Brief", "instructions": "News.", "time": "08:00", "days": "weekdays" }),
        )
        .unwrap();
        assert!(made.contains("Weekdays · 8:00") && made.contains("Mon 12 Oct 08:00"), "{made}");
        let routine = schedule::routines(&db, None).unwrap().remove(0);
        assert_eq!(routine.agent_id.as_deref(), Some("agent-1"));
        assert!(
            call(None, "create_routine", json!({ "title": "x", "instructions": "y" }))
                .unwrap_err()
                .contains("what time")
        );
        let updated =
            call(Some("agent-1"), "update_routine", json!({ "id": routine.id, "time": "07:30", "days": ["sat"] }))
                .unwrap();
        assert!(updated.contains("next Sat 10 Oct 07:30"), "{updated}");
        assert!(
            call(Some("agent-1"), "update_routine", json!({ "id": routine.id, "active": false }))
                .unwrap()
                .contains("paused")
        );
        let hourly = call(None, "create_routine", json!({ "title": "Inbox", "instructions": "Check.", "every_minutes": 60, "from_time": "09:00", "to_time": "17:00" })).unwrap();
        assert!(hourly.contains("Every 1 h · 9:00–17:00") && hourly.contains("Sat 10 Oct 09:00"), "{hourly}");
        let once = call(
            None,
            "create_routine",
            json!({ "title": "Once", "instructions": "Once.", "once_at": "2026-10-10 18:00" }),
        )
        .unwrap();
        assert!(once.contains("Once · Sat 10 Oct 18:00"), "{once}");
        assert_eq!(call(None, "list_routines", json!({})).unwrap().lines().count(), 3);
        call(Some("agent-1"), "delete_routine", json!({ "id": routine.id })).unwrap();

        let planned =
            call(None, "schedule_check_in", json!({ "title": "Essay", "note": "Intro done?", "at": "06:30" })).unwrap();
        assert!(planned.contains("Sat 10 Oct 06:30"), "{planned}");
        let wake = schedule::wakes(&db, None).unwrap().remove(0);
        assert_eq!(wake.chat_id.as_deref(), Some("chat-1"));
        assert!(call(None, "list_check_ins", json!({})).unwrap().contains("Intro done?"));
        call(None, "cancel_check_in", json!({ "id": wake.id })).unwrap();
        assert_eq!(call(None, "list_check_ins", json!({})).unwrap(), "No check-ins planned.");

        // In a group turn the check-in comes back in the group, still the speaker's.
        let planned = call_with(
            &db,
            &tz,
            "chat-1",
            Some("group-1"),
            Some("agent-1"),
            "schedule_check_in",
            &json!({ "title": "Recipes", "in_minutes": 30 }),
            now,
        )
        .unwrap();
        assert!(planned.contains("back in the group chat"), "{planned}");
        let wake = schedule::wakes(&db, Some("agent-1")).unwrap().remove(0);
        assert_eq!((wake.chat_id.as_deref(), wake.agent_id.as_deref()), (Some("group-1"), Some("agent-1")));
    }

    #[test]
    fn the_agent_is_found_from_its_conversation() {
        use crate::app_data::{self, tests::agent};
        let db = crate::db::tests::temp_path("schedule-mcp");
        let opened = Db::open(&db).unwrap();
        app_data::add_agent(&opened, &agent("a1", "A1", Some("chat-a"), 1.)).unwrap();
        app_data::add_agent(&opened, &agent("a2", "A2", None, 2.)).unwrap();
        assert_eq!(agent_of(&opened, "chat-a").as_deref(), Some("a1"));
        assert_eq!(agent_of(&opened, "chat-b"), None);
        call(&db, "chat-a", None, "create_reminder", &json!({ "title": "Mine", "in_minutes": 5 })).unwrap();
        assert_eq!(schedule::reminders(&opened, None).unwrap()[0].agent_id.as_deref(), Some("a1"));

        // The default Filo agent's conversation has what the user made on the Scheduled page, not a1's.
        app_data::remove_agent(&opened, "a2").unwrap();
        app_data::add_agent(&opened, &agent(crate::apps::FILO_AGENT, "Filo", Some("chat-f"), 0.)).unwrap();
        let page =
            NewReminder { title: "Dentist".into(), due_at: crate::now_ms() as i64 + 3_600_000, ..Default::default() };
        let page = schedule::create_reminder(&opened, page, crate::now_ms() as i64).unwrap();
        let listed = call(&db, "chat-f", None, "list_reminders", &json!({})).unwrap();
        assert!(listed.contains("Dentist") && !listed.contains("Mine"), "{listed}");
        call(&db, "chat-f", None, "complete_reminder", &json!({ "id": page.id })).unwrap();
        assert!(call(&db, "chat-a", None, "list_reminders", &json!({})).unwrap().contains("Mine"));
        drop(opened);
        std::fs::remove_dir_all(db.parent().unwrap()).ok();
    }
}

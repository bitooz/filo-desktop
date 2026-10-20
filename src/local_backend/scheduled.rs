// Scheduled work on this Mac: the pages' `reminders:*`, `routines:*` and `agenda:*` functions, answered from Filo's
// database (crates/filo-local `schedule`), and the loop that does what comes due.
//
// Two processes can run the schedule: the app while it's open, and `filo --scheduler`, the background helper launchd
// keeps running (launch_agent.rs) so reminders and routines fire when the app is closed. The app holds
// `local/scheduler.lock` for as long as it's open; the helper takes it only for the moment of a claim, so while the app
// is open the helper finds it taken and leaves the work to the app (whose pages then show turns working). A claim
// moves the item on in the database, so nothing fires twice even if both look at once.
//
// What a job does: a reminder becomes a notification (the pet and menu bar show it too, from `reminders:myReminders`);
// a routine's run is a turn in its own chat (Filo's) or in the agent's conversation, with its instructions as the
// message; a check-in is a turn in the conversation it was planned in (in the group, the planner's turn there:
// local_backend/group.rs `group_check_in`, run when the app is open). When the turn has sent something, a
// notification shows its first message. The Feed's topics are routines too, each watched by an agent (feed.rs).
//
// The first thing scheduled installs the helper, and the first one made or coming due while the app is open asks for
// notifications (notifications.rs `needed`); until then Filo adds no background item and asks nothing.

use std::fs::{File, OpenOptions};
use std::sync::Arc;
use std::time::Duration;

use filo_local::schedule::{self, Job, NewReminder, NewRoutine, NewWake, Routine, Run, Wake};
use filo_local::store::{CHECK_IN, RoutineRun};
use filo_local::{Role, Store};
use serde::Serialize;
use serde_json::{Value, json};

use super::{Local, arg, in_namespace, not_here, now_ms, number, required};

/// The functions answered here, for the test that each is (the module answers its namespaces whole: `in_namespace`).
#[cfg(test)]
pub const SCHEDULED: &[&str] = &[
    "reminders:myReminders",
    "reminders:createMine",
    "reminders:completeMine",
    "reminders:snoozeMine",
    "reminders:cancelMine",
    "routines:myRoutines",
    "routines:myRoutine",
    "routines:agentRoutines",
    "routines:createMine",
    "routines:updateMine",
    "routines:deleteMine",
    "routines:runNowMine",
    "agenda:myWakes",
    "agenda:addWakeMine",
    "agenda:cancelWakeMine",
];

/// How often the app looks at the schedule (and at the database for other processes' writes).
const APP_TICK: Duration = Duration::from_secs(1);
/// How often the helper looks, at most: it also wakes for the next item due.
const HELPER_TICK: Duration = Duration::from_secs(5);
/// How long a routine's run waits for its conversation to be free before it gives up.
const ROUTINE_WAIT: Duration = Duration::from_secs(60 * 60);
/// What a run keeps of what it sent, for the routine's page.
const SUMMARY_CHARS: usize = 4_000;
const BANNER_CHARS: usize = 240;

fn routine_message(routine: &Routine) -> String {
    let when = schedule::describe_schedule(&chrono::Local, routine);
    format!(
        "⏰ Routine “{}” ({when}). This is the routine running on its schedule, not the user typing: do what it says and send the result with send_message. \
If it says to stay quiet when there's nothing to report and there's nothing, end without sending anything.\n\n{}",
        routine.title, routine.instructions
    )
}

fn wake_message(wake: &Wake) -> String {
    let note = if wake.note.is_empty() { String::new() } else { format!("\n\n{}", wake.note) };
    format!(
        "{CHECK_IN}{}{note}\n\n(This is your own scheduled check-in, not a message from the user. Follow up as you planned and send the user what's useful with send_message. \
If there's nothing new for them, end without sending anything.)",
        wake.title
    )
}

/// What a member is told when a check-in it planned in the group comes due.
fn group_wake_reason(wake: &Wake) -> String {
    let note = if wake.note.is_empty() { String::new() } else { format!("\n\n{}", wake.note) };
    format!(
        "A check-in you planned for yourself in the group came due: {}{note}\n\n(Nobody wrote to you: follow up as you planned. If there's something useful for the group, post it with send_message; else send nothing.)",
        wake.title
    )
}

impl Local {
    /// Answers the scheduling functions (`reminders:`, `routines:`, `agenda:`); `None` for any other path.
    pub(super) fn scheduled_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        in_namespace(path, &["reminders", "routines", "agenda"]).then(|| self.scheduled_answer(path, args))
    }

    /// `agents:scheduledCount`: how many reminders and routines an agent has (the Agents tab asks before removing it).
    pub(super) fn scheduled_count(&self, args: &Value) -> Result<Value, String> {
        let (reminders, routines) = schedule::agent_counts(self.db()?, required(args, "id")?)?;
        Ok(json!({ "reminders": reminders, "routines": routines }))
    }

    fn scheduled_answer(self: &Arc<Self>, path: &str, args: &Value) -> Result<Value, String> {
        let tz = &chrono::Local;
        let now = now_ms();
        let db = self.db()?;
        match path {
            "reminders:myReminders" => Ok(self.named(schedule::reminders(db, None)?)),
            "reminders:createMine" => {
                let new = NewReminder {
                    title: arg(args, "title").unwrap_or_default().to_string(),
                    note: arg(args, "note").map(str::to_string),
                    due_at: number(args, "dueAt").ok_or("`dueAt` is required")? as i64,
                    recurrence: arg(args, "recurrence").map(str::to_string),
                    delivery: arg(args, "delivery").map(str::to_string),
                    agent_id: arg(args, "agentId").map(str::to_string),
                    chat_id: arg(args, "sessionId").map(str::to_string),
                };
                self.write_db(|db| schedule::create_reminder(db, new, now))
            }
            "reminders:completeMine" => {
                self.write_db(|db| schedule::complete_reminder(db, tz, required(args, "id")?, None, now))
            }
            "reminders:snoozeMine" => {
                let minutes = number(args, "minutes").ok_or("`minutes` is required")?;
                self.write_db(|db| schedule::snooze_reminder(db, required(args, "id")?, minutes, None, now))
            }
            "reminders:cancelMine" => {
                self.write_db(|db| schedule::cancel_reminder(db, required(args, "id")?, None, now))
            }
            "routines:myRoutines" => Ok(self.named(schedule::routines(db, None)?)),
            "routines:agentRoutines" => Ok(self.named(schedule::routines(db, Some(required(args, "agentId")?))?)),
            "routines:myRoutine" => {
                let Some(routine) = schedule::routine(db, required(args, "id")?, None)? else { return Ok(Value::Null) };
                let runs = schedule::runs(db, &routine.id)?;
                let routine =
                    self.named(vec![routine]).as_array_mut().and_then(|rows| rows.pop()).unwrap_or(Value::Null);
                Ok(json!({ "routine": routine, "runs": runs }))
            }
            "routines:createMine" => {
                let field = |key: &str| args.get(key).cloned().unwrap_or(Value::Null);
                let new = NewRoutine {
                    title: arg(args, "title").unwrap_or_default().to_string(),
                    instructions: arg(args, "instructions").unwrap_or_default().to_string(),
                    days: field("days"),
                    minute_of_day: field("minuteOfDay"),
                    schedule: field("schedule"),
                    agent_id: arg(args, "agentId").map(str::to_string),
                };
                self.write_db(|db| schedule::create_routine(db, tz, new, now))
            }
            "routines:updateMine" => {
                self.write_db(|db| schedule::update_routine(db, tz, required(args, "id")?, args, None, now))
            }
            "routines:deleteMine" => self.write_db(|db| schedule::delete_routine(db, required(args, "id")?, None)),
            "routines:runNowMine" => {
                // Throws the reason when it can't start ("The previous run was still working."), as the page expects.
                let Some((routine, run)) = schedule::start_run_now(db, required(args, "id")?, None, now)? else {
                    return Ok(Value::Null);
                };
                let id = run.id.clone();
                self.changed();
                self.run_job(Job::Routine { routine, run }, false);
                Ok(json!(id))
            }
            "agenda:myWakes" => Ok(self.named(schedule::wakes(db, None)?)),
            "agenda:addWakeMine" => {
                let session = arg(args, "sessionId").map(str::to_string);
                let agent_id = session.as_deref().and_then(|session| self.agent_of(session));
                let new = NewWake {
                    title: arg(args, "title").unwrap_or_default().to_string(),
                    note: arg(args, "note").unwrap_or_default().to_string(),
                    run_at: number(args, "runAt").ok_or("`runAt` is required")? as i64,
                    chat_id: session,
                    agent_id,
                };
                self.write_db(|db| schedule::create_wake(db, new, now))
            }
            "agenda:cancelWakeMine" => self.write_db(|db| schedule::cancel_wake(db, required(args, "id")?, None, now)),
            _ => Err(not_here(path)),
        }
    }

    /// Rows with their agent's name beside its id (`agentName`), for the owners' sections; an agent that's gone
    /// leaves the row Filo's.
    fn named<T: Serialize>(&self, rows: Vec<T>) -> Value {
        let agents = self.agents();
        let rows = rows
            .into_iter()
            .map(|row| {
                let mut row = serde_json::to_value(row).unwrap_or(Value::Null);
                if let Some(agent) = row.get("agentId").and_then(Value::as_str).map(str::to_string) {
                    match agents.iter().find(|record| record.id == agent) {
                        Some(record) => row["agentName"] = json!(record.name),
                        None => {
                            if let Some(fields) = row.as_object_mut() {
                                fields.remove("agentId");
                            }
                        }
                    }
                }
                row
            })
            .collect();
        Value::Array(rows)
    }

    /// The agent whose conversation `session_id` is.
    pub(super) fn agent_of(&self, session_id: &str) -> Option<String> {
        self.agents().into_iter().find(|agent| agent.session_id.as_deref() == Some(session_id)).map(|agent| agent.id)
    }

    /// An agent was deleted (`agents:remove`): `scheduled` "delete" takes its reminders and routines with it, else they
    /// go to the main agent.
    pub(super) fn scheduled_agent_removed(&self, agent_id: &str, scheduled: Option<&str>) {
        if let Ok(db) = self.db() {
            let heir = self.main_agent_id();
            if let Err(error) =
                schedule::agent_removed(db, agent_id, scheduled == Some("delete"), Some(&heir), now_ms())
            {
                eprintln!("filo: the deleted agent's scheduled work: {error}");
            }
        }
    }

    // --- Doing what came due ---

    /// Does one job. `headless` (the helper) also says when a turn failed, since no window will.
    pub(super) fn run_job(self: &Arc<Self>, job: Job, headless: bool) {
        match job {
            Job::Remind(reminder) => {
                let body = reminder.note.clone().unwrap_or_else(|| {
                    if reminder.delivery.as_deref() == Some("alarm") { "Alarm".into() } else { "Reminder".into() }
                });
                crate::notifications::needed(crate::notifications::Need::Scheduled);
                crate::notifications::show_scheduled(&format!("filo-reminder-{}", reminder.id), &reminder.title, &body);
            }
            Job::Routine { routine, run } => {
                if let Err(error) = self.run_routine(&routine, &run, headless) {
                    eprintln!("filo: routine “{}” couldn't run: {error}", routine.title);
                    if let Ok(db) = self.db() {
                        let _ = schedule::run_finished(db, &run.id, None, Some(&error), now_ms());
                    }
                    self.changed();
                }
            }
            Job::Wake(wake) => {
                if let Err(error) = self.run_wake(&wake, headless) {
                    eprintln!("filo: check-in “{}” couldn't run: {error}", wake.title);
                }
            }
        }
    }

    /// Where a routine runs: its agent's conversation, else (one of no agent's, or whose agent is gone) the main
    /// agent's, who owns what's no agent's.
    fn routine_conversation(&self, routine: &Routine) -> Result<(Store, String), String> {
        Ok((self.agent_chats.clone(), self.agent_or_main_chat(routine.agent_id.as_deref())?))
    }

    /// A routine's run, in its conversation once nothing else runs there (a turn of the user's, another routine's: an
    /// agent watching several Feed topics gets their runs one after the other). It counts as started meanwhile.
    fn run_routine(self: &Arc<Self>, routine: &Routine, run: &Run, headless: bool) -> Result<(), String> {
        let (store, chat_id) = self.routine_conversation(routine)?;
        let turn_id = uuid::Uuid::new_v4().to_string();
        schedule::run_started(self.db()?, &run.id, &chat_id, &turn_id)?;
        self.changed();
        if self.start_routine_turn_if_free(&store, &chat_id, &turn_id, routine, run, headless)? {
            return Ok(());
        }
        let (local, routine, run) = (self.clone(), routine.clone(), run.clone());
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + ROUTINE_WAIT;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let late = std::time::Instant::now() > deadline;
                let started = if late {
                    Err("Its conversation was busy for too long.".to_string())
                } else {
                    local.start_routine_turn_if_free(&store, &chat_id, &turn_id, &routine, &run, headless)
                };
                match started {
                    Ok(false) => continue,
                    Ok(true) => break,
                    Err(error) => {
                        eprintln!("filo: routine “{}” couldn't run: {error}", routine.title);
                        if let Ok(db) = local.db() {
                            let _ = schedule::run_finished(db, &run.id, None, Some(&error), now_ms());
                        }
                        local.changed();
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    /// Starts the run's turn unless the conversation has one running; false when it had to wait.
    fn start_routine_turn_if_free(
        self: &Arc<Self>,
        store: &Store,
        chat_id: &str,
        turn_id: &str,
        routine: &Routine,
        run: &Run,
        headless: bool,
    ) -> Result<bool, String> {
        static STARTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _starting = STARTING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.running.lock().unwrap().values().any(|running| running.session_id == chat_id) {
            return Ok(false);
        }
        self.start_routine_turn(store.clone(), chat_id.to_string(), turn_id.to_string(), routine, run, headless)?;
        Ok(true)
    }

    fn start_routine_turn(
        self: &Arc<Self>,
        store: Store,
        chat_id: String,
        turn_id: String,
        routine: &Routine,
        run: &Run,
        headless: bool,
    ) -> Result<(), String> {
        let mark =
            RoutineRun { title: routine.title.clone(), when: schedule::describe_schedule(&chrono::Local, routine) };
        store
            .add_routine_message(&chat_id, &routine_message(routine), &turn_id, mark)
            .map_err(|error| format!("Couldn't save the run's message: {error}"))?;
        self.changed();
        let (local, run_id, title) = (self.clone(), run.id.clone(), routine.title.clone());
        let (chat, turn) = (chat_id.clone(), turn_id.clone());
        let reader = store.clone();
        self.start_turn_then(store, chat_id, turn_id, move |report| {
            let replies = replies_of(&reader, &chat, &turn);
            let summary = (!replies.is_empty()).then(|| filo_local::store::cut(&replies.join("\n\n"), SUMMARY_CHARS));
            if let Ok(db) = local.db() {
                let _ = schedule::run_finished(db, &run_id, summary.as_deref(), report.error.as_deref(), now_ms());
            }
            local.changed();
            announce(&local, &chat, &title, &replies, report.error.as_deref(), headless);
        });
        Ok(())
    }

    fn run_wake(self: &Arc<Self>, wake: &Wake, headless: bool) -> Result<(), String> {
        // Planned in the group: the planner's turn there (the app runs the group's turns, so from the helper it waits
        // for the app to open).
        if wake.chat_id.is_some() && wake.chat_id == self.group_session() {
            return self.group_check_in(wake.agent_id.as_deref(), &group_wake_reason(wake));
        }
        // The conversation it was planned in; if that's gone, its agent's conversation now, else the main agent's.
        let (store, chat_id) =
            match wake.chat_id.as_deref().and_then(|chat| Some((self.store_of(chat)?.clone(), chat.to_string()))) {
                Some(found) => found,
                None => (self.agent_chats.clone(), self.agent_or_main_chat(wake.agent_id.as_deref())?),
            };
        let title = store
            .chat(&chat_id)
            .ok()
            .flatten()
            .map(|chat| chat.title)
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Filo".into());
        let turn_id = uuid::Uuid::new_v4().to_string();
        store
            .add_check_in_message(&chat_id, &wake_message(wake), &turn_id)
            .map_err(|error| format!("Couldn't save the check-in: {error}"))?;
        self.changed();
        let (local, reader, chat, turn) = (self.clone(), store.clone(), chat_id.clone(), turn_id.clone());
        self.start_turn_then(store, chat_id, turn_id, move |report| {
            announce(&local, &chat, &title, &replies_of(&reader, &chat, &turn), report.error.as_deref(), headless);
        });
        Ok(())
    }
}

/// What a turn sent the user, in order.
fn replies_of(store: &Store, chat_id: &str, turn_id: &str) -> Vec<String> {
    let messages = store.messages(chat_id).unwrap_or_default();
    messages
        .into_iter()
        .filter(|message| message.role == Role::Assistant && message.turn.as_deref() == Some(turn_id))
        .map(|message| message.text)
        .collect()
}

/// A notification for what a scheduled turn sent, or why it failed (it replaces the chat's last one), from the helper
/// while the app is closed (`announces`).
fn announce(local: &Local, chat_id: &str, title: &str, replies: &[String], error: Option<&str>, headless: bool) {
    if !announces(local, headless) {
        return;
    }
    let id = format!("filo-chat-{chat_id}");
    if let Some(first) = replies.first() {
        crate::notifications::show_scheduled(&id, title, &filo_local::store::preview(first, BANNER_CHARS));
    } else if let Some(error) = error {
        crate::notifications::show_scheduled(
            &id,
            title,
            &filo_local::store::cut(&format!("Couldn't run: {error}"), BANNER_CHARS),
        );
    }
}

/// Whether a scheduled turn's end is a banner: only from the helper, and only while no Filo window is open (one that
/// opened while the turn ran). An open app notifies about replies nobody has seen itself (`pushedUntil`, unseen.rs): a
/// banner while the user is in another app, the card in its window while they're in Filo.
fn announces(local: &Local, headless: bool) -> bool {
    headless && !super::coding::app_open(local)
}

/// `local/scheduler.lock`, taken without waiting; held while the returned file is open.
fn try_lock(local: &Local) -> Option<File> {
    let _ = std::fs::create_dir_all(&local.root);
    let file =
        OpenOptions::new().create(true).truncate(false).write(true).open(local.root.join("scheduler.lock")).ok()?;
    filo_local::lock::try_lock(&file).then_some(file)
}

/// Runs the schedule on a thread of its own for as long as the process lives. The app (`headless` false) keeps the
/// lock once it has it and refreshes its pages when another process writes the database; the helper takes the lock
/// only to claim.
pub fn start(local: Arc<Local>, headless: bool) {
    std::thread::Builder::new()
        .name("filo-schedule".into())
        .spawn(move || {
            let tick = if headless { HELPER_TICK } else { APP_TICK };
            // Feed topics from before each was watched by an agent get their routines (local_backend/feed.rs).
            if !headless {
                local.feed_carry_over();
            }
            let mut held: Option<File> = None;
            let mut seen_version = None;
            // How many reminders, routines and check-ins were made, as of the last look (the app's).
            let mut made: Option<i64> = None;
            loop {
                if held.is_none() {
                    held = try_lock(&local);
                }
                let mut wait = tick;
                if held.is_some() {
                    if let Ok(db) = local.db() {
                        let now = now_ms();
                        let _ = schedule::settle_orphaned_runs(db, now);
                        match schedule::claim_due(db, &chrono::Local, now) {
                            Ok(jobs) => {
                                if !jobs.is_empty() {
                                    local.changed();
                                }
                                for job in jobs {
                                    local.run_job(job, headless);
                                }
                            }
                            Err(error) => eprintln!("filo: the schedule couldn't be read: {error}"),
                        }
                        if let Ok(Some(next)) = schedule::next_due(db) {
                            let until = Duration::from_millis((next - now_ms()).max(0) as u64);
                            wait = wait.min(until).max(Duration::from_millis(200));
                        }
                    }
                    if headless {
                        held = None;
                    }
                }
                if !headless {
                    // A reminder made by a turn's MCP server, a run the helper finished: the pages show it.
                    let version = local.db().ok().and_then(|db| db.data_version().ok());
                    if seen_version.is_some() && version != seen_version {
                        local.changed();
                    }
                    seen_version = version;
                    // Something was scheduled (or was already, at launch): the helper runs it while the app is closed.
                    // One made while the app is open asks for notifications.
                    if let Some(count) = local.db().ok().and_then(|db| schedule::made(db).ok()) {
                        if count > made.unwrap_or(0) {
                            crate::launch_agent::install_in_background();
                            if made.is_some() {
                                crate::notifications::needed(crate::notifications::Need::Scheduled);
                            }
                        }
                        made = Some(count);
                    }
                }
                std::thread::sleep(wait);
            }
        })
        .expect("the schedule's thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("scheduled")
    }

    #[test]
    fn a_scheduled_turns_banner_comes_from_the_helper_only_while_the_app_is_closed() {
        let local = temp();
        // The app's own turns: it notifies about their replies itself (a card while the user is in Filo).
        assert!(!announces(&local, false));
        assert!(announces(&local, true));
        // A window opened on this data while the helper's turn ran (it holds coding.lock): it notifies instead.
        let window = File::create(local.root.join("coding.lock")).unwrap();
        assert!(filo_local::lock::try_lock(&window));
        assert!(!announces(&local, true));
        drop(window);
        assert!(announces(&local, true));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_pages_functions_make_list_and_change() {
        let local = temp();
        let now = now_ms();
        let made = local
            .call("reminders:createMine", &json!({ "title": "Pills", "dueAt": (now + 3_600_000) as f64 }))
            .unwrap();
        let id = made["_id"].as_str().unwrap().to_string();
        let listed = local.call("reminders:myReminders", &json!({})).unwrap();
        assert_eq!((listed[0]["title"].as_str(), listed[0]["dueAt"].as_i64()), (Some("Pills"), Some(now + 3_600_000)));
        assert_eq!(local.call("reminders:snoozeMine", &json!({ "id": id, "minutes": 10.0 })).unwrap(), json!(true));
        assert_eq!(local.call("reminders:completeMine", &json!({ "id": id })).unwrap(), json!(true));
        assert_eq!(local.call("reminders:cancelMine", &json!({ "id": "gone" })).unwrap(), json!(false));
        assert_eq!(local.call("reminders:myReminders", &json!({})).unwrap(), json!([]));

        // An agent's routine carries its name; the routine page gets it with its runs.
        let agent = local.call("agents:create", &json!({ "name": "Tutor", "instructions": "Teach." })).unwrap();
        let agent = agent.as_str().unwrap();
        let routine = local
            .call("routines:createMine", &json!({ "title": "Quiz", "instructions": "Quiz me.", "days": [1.0, 3.0], "minuteOfDay": 540.0, "agentId": agent }))
            .unwrap();
        let routine_id = routine["_id"].as_str().unwrap().to_string();
        let listed = local.call("routines:myRoutines", &json!({})).unwrap();
        assert_eq!((listed[0]["agentName"].as_str(), listed[0]["status"].as_str()), (Some("Tutor"), Some("active")));
        assert!(listed[0]["nextRunAt"].as_i64().unwrap() > now);
        assert_eq!(
            local.call("agents:scheduledCount", &json!({ "id": agent })).unwrap(),
            json!({ "reminders": 0, "routines": 1 })
        );
        assert_eq!(
            local.call("routines:agentRoutines", &json!({ "agentId": agent })).unwrap().as_array().unwrap().len(),
            1
        );
        assert_eq!(
            local.call("routines:updateMine", &json!({ "id": routine_id, "status": "paused" })).unwrap(),
            json!(true)
        );
        let detail = local.call("routines:myRoutine", &json!({ "id": routine_id })).unwrap();
        assert_eq!(
            (detail["routine"]["status"].as_str(), detail["runs"].as_array().map(Vec::len)),
            (Some("paused"), Some(0))
        );
        assert!(detail["routine"].get("nextRunAt").is_none());

        // Deleting the agent with "filo" gives the routine to the main agent (Filo's, here).
        local.call("agents:remove", &json!({ "id": agent, "scheduled": "filo" })).unwrap();
        let listed = local.call("routines:myRoutines", &json!({})).unwrap();
        assert_eq!(listed[0]["agentId"], super::super::DEFAULT_AGENT_ID);
        assert_eq!(local.call("routines:deleteMine", &json!({ "id": routine_id })).unwrap(), json!(true));
        assert_eq!(local.call("routines:myRoutine", &json!({ "id": routine_id })).unwrap(), Value::Null);

        let wake =
            local.call("agenda:addWakeMine", &json!({ "title": "Essay", "runAt": (now + 600_000) as f64 })).unwrap();
        assert_eq!(local.call("agenda:myWakes", &json!({})).unwrap()[0]["title"], "Essay");
        assert_eq!(local.call("agenda:cancelWakeMine", &json!({ "id": wake["_id"] })).unwrap(), json!(true));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn only_one_process_holds_the_schedule() {
        let local = temp();
        let first = try_lock(&local).expect("free at first");
        // flock locks belong to the open file, so a second open (another process, or here) can't take it.
        assert!(try_lock(&local).is_none());
        drop(first);
        assert!(try_lock(&local).is_some());
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_check_in_planned_in_the_group_comes_back_there() {
        let local = temp();
        let chef = filo_local::app_data::Agent {
            id: "chef-id".into(),
            name: "Chef".into(),
            instructions: "Cooks.".into(),
            picture: "mint".into(),
            background: None,
            session_id: None,
            engine: None,
            model: None,
            earlier_chat: None,
            created_at: now_ms() as f64,
        };
        filo_local::app_data::add_agent(local.db().unwrap(), &chef).unwrap();
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let now = now_ms();
        let chats_before = local.agent_chats.list_chats().len();
        let new = NewWake {
            title: "Did they cook it?".into(),
            note: "Pasta".into(),
            run_at: now + 60_000,
            chat_id: Some(session),
            agent_id: Some("chef-id".into()),
        };
        let wake = schedule::create_wake(db, new, now).unwrap();
        let mut due = schedule::claim_due(db, &chrono::Local, wake.run_at).unwrap();
        let Some(Job::Wake(wake)) = due.pop() else { panic!("the check-in isn't due") };
        local.run_job(Job::Wake(wake), false);
        // Chef's turn in the group, not in a chat.
        let waiting = filo_local::group::active(db).unwrap();
        assert_eq!(waiting.iter().map(|turn| turn.speaker.as_str()).collect::<Vec<_>>(), ["chef-id"]);
        assert!(
            waiting[0].reason.contains("Did they cook it?") && waiting[0].reason.contains("Pasta"),
            "{}",
            waiting[0].reason
        );
        assert_eq!(local.agent_chats.list_chats().len(), chats_before);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_routine_of_no_agents_runs_in_the_main_agents_chat() {
        let local = temp();
        let now = now_ms();
        let db = local.db().unwrap();
        let main = local
            .call("agents:openChat", &json!({ "id": super::super::DEFAULT_AGENT_ID }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let new = NewRoutine {
            title: "Brief".into(),
            instructions: "Summarise.".into(),
            days: json!([0, 1, 2, 3, 4, 5, 6]),
            minute_of_day: json!(0),
            ..Default::default()
        };
        let routine = schedule::create_routine(db, &chrono::Local, new, now).unwrap();
        let chats_before = local.agent_chats.list_chats().len();
        let (store, chat) = local.routine_conversation(&routine).unwrap();
        assert_eq!((store.root(), chat.as_str()), (local.agent_chats.root(), main.as_str()));
        // One from before, with a chat of its own (local/chats), and one whose agent is gone: the same.
        let mut old = routine.clone();
        old.chat_id = Some("an-old-chat".into());
        assert_eq!(local.routine_conversation(&old).unwrap().1, main);
        old.agent_id = Some("gone".into());
        assert_eq!(local.routine_conversation(&old).unwrap().1, main);
        assert_eq!(local.agent_chats.list_chats().len(), chats_before);
        assert!(routine_message(&routine).starts_with("⏰ Routine “Brief” (Every day · 0:00)"));
        // A routine that says to stay quiet with nothing to report may end without a message.
        assert!(routine_message(&routine).contains("end without sending anything"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_check_in_whose_chat_is_gone_comes_back_in_its_agents_chat_or_the_main_agents() {
        let local = temp();
        local.no_turns.store(true, std::sync::atomic::Ordering::SeqCst);
        let db = local.db().unwrap();
        let now = now_ms();
        let main = local
            .call("agents:openChat", &json!({ "id": super::super::DEFAULT_AGENT_ID }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let tutor = local
            .call("agents:create", &json!({ "name": "Tutor", "instructions": "Teaches." }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let tutors = local.call("agents:openChat", &json!({ "id": tutor })).unwrap().as_str().unwrap().to_string();
        let chats_before = local.agent_chats.list_chats().len();
        let run = |title: &str, agent: Option<&str>| {
            let new = NewWake {
                title: title.into(),
                note: String::new(),
                run_at: now + 60_000,
                chat_id: Some("deleted-chat".into()),
                agent_id: agent.map(str::to_string),
            };
            let wake = schedule::create_wake(db, new, now).unwrap();
            let Some(Job::Wake(wake)) = schedule::claim_due(db, &chrono::Local, wake.run_at).unwrap().pop() else {
                panic!("the check-in isn't due")
            };
            local.run_job(Job::Wake(wake), false);
        };
        // The check-in's own line, wherever it lands: a turn that can't start (no coding tool, as on CI) adds its
        // error after it.
        let texts = |chat: &str| -> Vec<String> {
            local.agent_chats.messages(chat).unwrap().into_iter().map(|message| message.text).collect()
        };
        let has = |chat: &str, said: &str| texts(chat).iter().any(|text| text.contains(said));
        run("Did the lesson stick?", Some(&tutor));
        assert!(has(&tutors, "Did the lesson stick?"), "{:?}", texts(&tutors));
        // Marked as Filo's, so it may end quietly (turn.rs `quiet_allowed`), and told it may.
        let check_in = local.agent_chats.messages(&tutors).unwrap().into_iter().find(|message| message.check_in);
        let check_in = check_in.expect("the check-in has its mark");
        assert!(check_in.from_app() && check_in.text.contains("end without sending anything"), "{}", check_in.text);
        run("Water the plants", None);
        assert!(has(&main, "Water the plants"), "{:?}", texts(&main));
        assert_eq!(local.agent_chats.list_chats().len(), chats_before);
        std::fs::remove_dir_all(&local.root).ok();
    }
}

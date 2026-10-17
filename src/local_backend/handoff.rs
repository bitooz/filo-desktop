// Coding tasks a conversation handed off (crates/filo-local `coding_handoff`, started by its `coding_mcp` tools): the
// app runs them and keeps the conversation that owns them in the loop.
//
// - A loop (every 0.7 s) starts tasks the MCP server queued, stops those it was asked to stop, and tells
//   conversations what they're waiting to hear.
// - A round runs with the tool kept talking to (`coding_session`): each step lands in the task's thread as on the
//   Coding page; what it asks is saved as a request and the conversation is woken to answer it (or hand it to the
//   user, whose tap on the chat's card answers it; an answer they type there wakes the conversation too, for what else
//   it asks of it, like a note); notes reach it at its next step.
// - A round whose turn ends with background work still going (a background command, a Monitor) ends there, but the
//   task doesn't: it's "monitoring", the tool kept running (coding_session.rs), and the conversation hears what it
//   waits on. When the tool picks the task back up, its follow-up runs as the next round, which ends as any other.
// - A round that ends (done or failed) wakes the conversation with what the tool reported: a turn of its own in a
//   Filo or agent chat (a hidden message, store.rs), or the member's turn in the group. One turn at a time in a chat:
//   a wake waits while a turn runs there. The reply to a finished round carries its Open proof.
// - The chat reads it all back: `coding:sessionsForChat` (the task rows), `coding:requestsForSession` (the question
//   cards), `coding:answerRequest` (the user's answer), `coding:roundResult` (a round's result page with its proof
//   files), `coding:workingChats` (the chats a task works for) and `coding:trayTasks` (each task for the pet's tray:
//   what it asks the user, and those just finished). `coding:shareResult` and `coding:unshareResult` put a round's
//   result on filo.im and take it off (result_share.rs).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use filo_local::coding::{self, Step, Task};
use filo_local::coding_handoff::{self as handoff, Handoff, Wake};
use filo_local::coding_session::{self, Ask, Host};
use filo_local::db::Db;
use filo_local::engine::Engine;
use filo_local::{Message, Role};
use serde_json::{Value, json};

use super::{Local, Stoppable, now_ms, required};

/// The functions answered here (checked by a test, like `MOVED`).
#[cfg(test)]
pub const HANDOFF: &[&str] = &[
    "coding:requestsForSession",
    "coding:sessionsForChat",
    "coding:workingChats",
    "coding:trayTasks",
    "coding:answerRequest",
    "coding:roundResult",
    "coding:sharePreview",
    "coding:shareResult",
    "coding:unshareResult",
];

/// The thread of a group turn telling a member a coding task's news: this, then the wake's id.
pub(super) const NEWS_THREAD: &str = "coding-";

/// How often the loop looks for work.
const LOOK_EVERY: Duration = Duration::from_millis(700);
/// A round that runs longer is stopped (waiting on an answer included).
const ROUND_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);
/// `coding:trayTasks` lists tasks finished this recently (the pet keeps them up for less).
const TRAY_FINISHED_MS: i64 = 10 * 60 * 1000;

/// The app opens: requests of tasks Filo was running when it closed close, and the loop starts.
pub fn start(local: Arc<Local>) {
    if let Ok(db) = local.db()
        && handoff::expire_finished(db, now_ms()).is_ok_and(|count| count > 0)
    {
        local.changed();
    }
    std::thread::spawn(move || {
        let mut input_owners = local.coding_input_owners(now_ms());
        loop {
            local.handoff_tick();
            // An undecided request becomes a user card after 90 seconds, even when the tool is quiet.
            let next = local.coding_input_owners(now_ms());
            if next != input_owners {
                input_owners = next;
                local.changed();
            }
            std::thread::sleep(LOOK_EVERY);
        }
    });
}

impl Local {
    /// Chats showing unanswered question/permission cards, and the agents that started those tasks.
    /// Use the same visibility rule as the cards; a tool waiting for its agent alone isn't waiting for the user.
    pub(super) fn coding_input_owners(&self, now: i64) -> (HashSet<String>, HashSet<String>) {
        let (mut chats, mut agents) = (HashSet::new(), HashSet::new());
        let Ok(db) = self.db() else { return (chats, agents) };
        for (task, owner) in handoff::unfinished(db).unwrap_or_default() {
            let waiting = handoff::requests_for_task(db, &task.id)
                .unwrap_or_default()
                .iter()
                .any(|request| request.state == "pending" && handoff::shows_in_chat(request, now));
            if waiting {
                chats.insert(owner.chat_id);
                agents.extend(owner.agent_id);
            }
        }
        (chats, agents)
    }

    /// One look: workers another process started, Stops asked for, queued tasks to start, wakes to send.
    fn handoff_tick(self: &Arc<Self>) {
        self.adopt_workers();
        let Ok(db) = self.db() else { return };
        for id in handoff::take_stop_requests(db).unwrap_or_default() {
            if let Err(error) = self.stop_coding_task(&id) {
                eprintln!("coding: couldn't stop {id}: {error}");
            }
        }
        for (task, _) in handoff::unfinished(db).unwrap_or_default() {
            if task.status == "queued" {
                self.run_round(task.id);
            }
        }
        self.deliver_wakes();
    }

    /// A round failed before its tool ran (not installed, the folder gone): the conversation hears it.
    pub(super) fn handoff_failed(&self, id: &str, error: &str) {
        let Ok(db) = self.db() else { return };
        let (Ok(Some(task)), Ok(Some(owner))) = (coding::task(db, id), handoff::get(db, id)) else { return };
        let now = now_ms();
        let _ = handoff::expire_pending(db, id, now);
        let text = handoff::failed_message(&task, &owner, task.rounds, error);
        let _ = handoff::add_wake(db, &owner, "failed", task.rounds, &text, now);
    }

    /// One round of a task handed off: the tool kept talking to, its steps saved as they come, what it asks put to
    /// the conversation, and the conversation told how it ended.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handoff_round(
        self: &Arc<Self>,
        task: Task,
        owner: Handoff,
        engine: Engine,
        program: PathBuf,
        path_env: Option<String>,
        stop: &Stoppable,
    ) -> Result<(), String> {
        let db = self.db()?;
        let now = || now_ms();
        let id = task.id.clone();
        let round = task.rounds + 1;
        let proof = handoff::proof_dir(&self.root, &id, round);
        std::fs::create_dir_all(&proof)
            .map_err(|error| format!("Couldn't make the proof folder {}: {error}", proof.display()))?;
        coding::set_running(db, &id, now())?;
        handoff::start_round(db, &id, round, &proof, now())?;
        self.changed();
        // The brief or the follow-ups since the last round, with notes not handed over yet.
        let mut message = coding::next_message(db, &id)?;
        let notes = handoff::undelivered_notes(db, &id)?;
        for (_, note) in &notes {
            if !message.contains(note.as_str()) {
                message.push_str(&format!("\n\n{note}"));
            }
        }
        // A cold session is compacted first (`catch_up`), its notes in the thread after the follow-up read above.
        self.catch_up(&task, engine, &program, path_env.as_deref(), stop);
        handoff::mark_delivered(db, &notes.iter().map(|(note, _)| *note).collect::<Vec<_>>(), now())?;
        // Claude Code's session is named up front, so a round stopped early can still be followed up.
        let new_session =
            (task.session.is_none() && engine == Engine::Claude).then(|| uuid::Uuid::new_v4().to_string());
        if let Some(session) = &new_session {
            coding::set_session(db, &id, session)?;
        }
        let spec = coding_session::Spec {
            engine,
            program,
            cwd: PathBuf::from(&task.work_path),
            path_env,
            message,
            resume: task.session.clone(),
            new_session,
            mode: task.mode.clone(),
            look_only: owner.look_only,
            model: task.model.clone(),
            effort: task.effort.clone(),
            instructions: handoff::instructions(
                &task,
                &owner,
                &task.owner,
                &proof,
                &self.agent_chats.skill_folder(filo_local::skills::ARTIFACTS_NAME),
            ),
            writable: vec![proof.clone()],
            timeout: ROUND_TIMEOUT,
            servers: Vec::new(),
            skills: None,
            chat: false,
            command: None,
            images: Vec::new(),
        };
        let mut host = RoundHost {
            local: self,
            db,
            task: &task,
            owner: &owner,
            round,
            proof: proof.clone(),
            waiting: false,
            followed: false,
            stop,
            answered: HashSet::new(),
        };
        let end = coding_session::run(&spec, &mut host, |started| stop.started(started));
        // A follow-up after background work is a round of its own.
        let (round, waiting) = (host.round, host.waiting);
        if let Some(session) = &end.session_id {
            coding::set_session(db, &id, session)?;
        }
        let (status, error) = match (end.stopped, end.error) {
            (true, _) => ("stopped", None),
            (false, Some(error)) => ("failed", Some(error)),
            (false, None) => ("done", None),
        };
        if let Some(error) = &error {
            self.note_failure("round", &id, Some(engine), task.model.clone(), error, end.stderr.clone());
        }
        // It ended while it waited on background work, before it carried on: the round it waited after ends so. The
        // conversation, told of that round as Monitoring already, hears of a failure, or that it's done: its work was,
        // and only a watcher it left running (stopped now, coding_session.rs) kept it open.
        if waiting {
            coding::finish_waiting(db, &id, status, error.as_deref(), now())?;
            handoff::settle_round(db, &id, round, status, error.as_deref())?;
            handoff::expire_pending(db, &id, now())?;
            let task = coding::task(db, &id)?.unwrap_or(task);
            match (status, handoff::round(db, &id, round)?) {
                ("done", Some(ended)) => {
                    let missed = handoff::undelivered_notes(db, &id)?;
                    handoff::mark_delivered(db, &missed.iter().map(|(note, _)| *note).collect::<Vec<_>>(), now())?;
                    let missed: Vec<String> = missed.into_iter().map(|(_, note)| note).collect();
                    let text =
                        handoff::round_message(&task, &owner, &ended, &handoff::round_proof(db, &ended)?, &missed);
                    handoff::add_wake(db, &owner, "round", round, &text, now())?;
                }
                ("failed", _) => {
                    let text = handoff::failed_message(
                        &task,
                        &owner,
                        round,
                        error.as_deref().unwrap_or("it stopped without saying why"),
                    );
                    handoff::add_wake(db, &owner, "failed", round, &text, now())?;
                }
                _ => {}
            }
            self.changed();
            self.deliver_wakes();
            return Ok(());
        }
        coding::finish_round(db, &id, status, error.as_deref(), end.final_text.as_deref(), now())?;
        handoff::finish_round(db, &id, round, status, end.final_text.as_deref(), error.as_deref(), now())?;
        handoff::expire_pending(db, &id, now())?;
        // Notes that came too late for this round: the conversation hears which.
        let missed = handoff::undelivered_notes(db, &id)?;
        handoff::mark_delivered(db, &missed.iter().map(|(note, _)| *note).collect::<Vec<_>>(), now())?;
        let missed: Vec<String> = missed.into_iter().map(|(_, note)| note).collect();
        let task = coding::task(db, &id)?.unwrap_or(task);
        match (status, handoff::round(db, &id, round)?) {
            ("done", Some(ended)) => {
                let text = handoff::round_message(&task, &owner, &ended, &handoff::round_proof(db, &ended)?, &missed);
                handoff::add_wake(db, &owner, "round", round, &text, now())?;
            }
            ("failed", _) => {
                let text = handoff::failed_message(
                    &task,
                    &owner,
                    round,
                    error.as_deref().unwrap_or("it stopped without saying why"),
                );
                handoff::add_wake(db, &owner, "failed", round, &text, now())?;
            }
            _ => {}
        }
        self.changed();
        self.deliver_wakes();
        Ok(())
    }

    /// Tells conversations what they're waiting to hear: a chat's wakes go together in a turn of their own once no
    /// turn runs there; the group's each queue their member's turn.
    pub(super) fn deliver_wakes(self: &Arc<Self>) {
        // Only the app tells conversations (a worker's round leaves its wakes for it).
        if self.in_worker.load(Ordering::SeqCst) {
            return;
        }
        // One delivery at a time; the loop looks again soon.
        let Ok(_delivering) = self.coding.waking.try_lock() else { return };
        let Ok(db) = self.db() else { return };
        let Ok(wakes) = handoff::pending_wakes(db) else { return };
        if wakes.is_empty() {
            return;
        }
        let mut chats: Vec<(String, Vec<Wake>)> = Vec::new();
        let now = now_ms();
        for wake in wakes {
            if let Some(speaker) = wake.speaker.clone() {
                // A thread of its own, so it isn't folded into another turn the member has queued.
                let thread = format!("{NEWS_THREAD}{}", wake.id);
                if let Ok(Some(turn)) = filo_local::group::enqueue(db, &thread, &speaker, 1, &wake.text, false, now) {
                    let _ = handoff::mark_sent(db, &[wake.id], &turn);
                }
                continue;
            }
            match chats.iter_mut().find(|(chat, _)| *chat == wake.chat_id) {
                Some((_, list)) => list.push(wake),
                None => chats.push((wake.chat_id.clone(), vec![wake])),
            }
        }
        for (chat_id, wakes) in chats {
            if self.running.lock().unwrap().values().any(|running| running.session_id == chat_id) {
                continue;
            }
            let ids: Vec<String> = wakes.iter().map(|wake| wake.id.clone()).collect();
            let Some(store) = self.store_of(&chat_id).cloned() else {
                // The conversation is gone: nobody to tell.
                let _ = handoff::mark_sent(db, &ids, "");
                continue;
            };
            let turn_id = uuid::Uuid::new_v4().to_string();
            let text = wakes.iter().map(|wake| wake.text.as_str()).collect::<Vec<_>>().join("\n\n---\n\n");
            // News of a round answers what asked the task for work, the latest ask this chat shows.
            let answers =
                handoff::asks_reported(db, &wakes).ok().flatten().and_then(|asks| store.shown_ask(&chat_id, &asks));
            if store.add_hidden_message(&chat_id, &text, &turn_id, answers).is_err() {
                continue;
            }
            let _ = handoff::mark_sent(db, &ids, &turn_id);
            #[cfg(not(test))]
            self.start_turn(store, chat_id, turn_id);
            #[cfg(test)]
            self.coding.woken.lock().unwrap().push((chat_id, turn_id));
        }
        self.changed();
    }

    /// Replies that report a coding round (the last of their turn), with the round they open: `codingResult`.
    pub(super) fn coding_results(&self, messages: &[Message]) -> HashMap<String, Value> {
        self.coding_results_by_turn(
            messages
                .iter()
                .filter(|message| message.role == Role::Assistant)
                .map(|message| (message.id.as_str(), message.turn.as_deref())),
        )
    }

    /// `coding_results` for (reply id, its turn) pairs, oldest first: a chat's messages or the group's posts.
    pub(super) fn coding_results_by_turn<'a>(
        &self,
        replies: impl Iterator<Item = (&'a str, Option<&'a str>)>,
    ) -> HashMap<String, Value> {
        let Ok(db) = self.db() else { return HashMap::new() };
        let mut last_of_turn: HashMap<&str, &str> = HashMap::new();
        for (id, turn) in replies {
            if let Some(turn) = turn {
                last_of_turn.insert(turn, id);
            }
        }
        last_of_turn
            .into_iter()
            .filter_map(|(turn, id)| {
                let (task_id, round) = handoff::reported_round(db, turn).ok()??;
                let mut result = json!({ "taskId": task_id, "round": round as f64 });
                // Its row's head and state, for a conversation that doesn't list the task (the group, for one a member
                // started in its own chat): the task's title and tool, how the round ended and how the task is now.
                if let (Ok(Some(task)), Ok(Some(ended))) =
                    (coding::task(db, &task_id), handoff::round(db, &task_id, round))
                {
                    result["title"] = json!(task.title);
                    result["tool"] = json!(task.tool);
                    result["status"] = json!(ended.status);
                    result["taskStatus"] =
                        json!(task_status(db, &task, now_ms()).unwrap_or_else(|_| task.status.clone()));
                    if let Ok(Some(worked)) = handoff::worked_ms(db, &task_id) {
                        result["workedMs"] = json!(worked as f64);
                    }
                }
                Some((id.to_string(), result))
            })
            .collect()
    }

    /// The conversations coding tasks work for now, with when their work started.
    pub(super) fn coding_working_since(&self) -> HashMap<String, f64> {
        let mut since: HashMap<String, f64> = HashMap::new();
        let Ok(db) = self.db() else { return since };
        for (task, owner) in handoff::unfinished(db).unwrap_or_default() {
            let started = handoff::working_since(db, &task, &owner).unwrap_or(task.created_at) as f64;
            let entry = since.entry(owner.chat_id).or_insert(started);
            *entry = entry.min(started);
        }
        since
    }

    pub(super) fn handoff_call(&self, path: &str, args: &Value) -> Option<Result<Value, String>> {
        Some(match path {
            "coding:requestsForSession" => required(args, "sessionId").and_then(|session| self.chat_requests(session)),
            "coding:sessionsForChat" => required(args, "sessionId").and_then(|session| self.chat_sessions(session)),
            "coding:workingChats" => {
                (|| {
                    let db = self.db()?;
                    let mut chats: Vec<(String, f64, Vec<String>)> = Vec::new();
                    for (task, owner) in handoff::unfinished(db)? {
                        let started = handoff::working_since(db, &task, &owner)? as f64;
                        match chats.iter_mut().find(|(chat, _, _)| *chat == owner.chat_id) {
                            Some((_, since, agents)) => {
                                *since = since.min(started);
                                agents.extend(owner.agent_id.filter(|agent| !agents.contains(agent)));
                            }
                            None => chats.push((owner.chat_id, started, owner.agent_id.into_iter().collect())),
                        }
                    }
                    Ok(Value::Array(chats.into_iter().map(|(chat, since, agents)| json!({ "sessionId": chat, "since": since, "agentIds": agents })).collect()))
                })()
            }
            "coding:trayTasks" => self.tray_tasks(),
            "coding:answerRequest" => (|| {
                let (id, answer) = (required(args, "requestId")?, required(args, "answer")?);
                let (db, now) = (self.db()?, now_ms());
                let answered = handoff::answer(db, id, answer, "user", now)?;
                // Typed in their own words: the conversation hears them too, for what they ask of it besides (a note).
                if handoff::answered_in_words(&answered)
                    && let (Some(task), Some(owner)) =
                        (coding::task(db, &answered.task_id)?, handoff::get(db, &answered.task_id)?)
                {
                    handoff::add_wake(
                        db,
                        &owner,
                        "answer",
                        answered.round,
                        &handoff::typed_answer_message(&task, &owner, &answered, filo_local::pages::get(db).notes),
                        now,
                    )?;
                }
                self.changed();
                Ok(Value::Null)
            })(),
            "coding:roundResult" => (|| {
                let db = self.db()?;
                let Some(task) = coding::task(db, required(args, "taskId")?)? else { return Ok(Value::Null) };
                let round = match args.get("round").and_then(Value::as_f64) {
                    Some(round) => handoff::round(db, &task.id, round as i64)?,
                    None => handoff::latest_round(db, &task.id)?,
                };
                let Some(round) = round else { return Ok(Value::Null) };
                let mut page = round_result(&task, &round, handoff::round_proof(db, &round)?);
                // How long the round worked, for the page's header.
                if let Some(worked) = handoff::round_worked_ms(&round, &handoff::requests_for_task(db, &task.id)?) {
                    page["workedMs"] = json!(worked as f64);
                }
                // The task's state, as its card and row say it, heads the page while it still works.
                page["taskStatus"] = json!(task_status(db, &task, now_ms())?);
                // Shared: its link, for the page's Copy link and Unshare.
                if let Some(share) = self.result_share(&task.id, round.round) {
                    page["share"] = json!({ "url": share.url });
                }
                Ok(page)
            })(),
            "coding:sharePreview" => self.share_preview(args),
            "coding:shareResult" => self.share_result(args, &super::result_share::endpoint("results")),
            "coding:unshareResult" => self.unshare_result(args, &super::result_share::endpoint("results")),
            _ => return None,
        })
    }

    /// The question cards of a conversation's tasks: those decided, handed to the user, or left undecided a while.
    fn chat_requests(&self, session: &str) -> Result<Value, String> {
        let db = self.db()?;
        let now = now_ms();
        let tasks: HashMap<String, Task> =
            handoff::for_chat(db, session)?.into_iter().map(|(task, _)| (task.id.clone(), task)).collect();
        let rows: Vec<Value> = handoff::requests_for_chat(db, session)?
            .into_iter()
            .filter(|request| handoff::shows_in_chat(request, now))
            .map(|request| {
                let task = tasks.get(&request.task_id);
                let (question, options) = match (&request.question, request.asked_user) {
                    (Some(question), true) => (question.clone(), request.user_options.clone()),
                    _ => (request.title.clone(), request.options.clone()),
                };
                // What each option means, by its place (empty for none).
                let notes: Vec<&str> = options
                    .iter()
                    .map(|option| request.option_notes.get(option.trim()).map_or("", String::as_str))
                    .collect();
                json!({
                    "requestId": request.id,
                    "taskId": request.task_id,
                    "kind": request.kind,
                    "question": question,
                    "detail": request.detail,
                    "options": options,
                    "optionNotes": notes,
                    "tool": task.map_or("Coding tool", |task| handoff::tool_name(&task.tool)),
                    "project": task.map(|task| task.project_name.clone()).unwrap_or_default(),
                    "taskTitle": task.map(|task| task.title.clone()).unwrap_or_default(),
                    "state": request.state,
                    "answer": request.answer,
                    "decidedBy": request.decided_by,
                    "askedAt": request.asked_at.unwrap_or(request.created_at) as f64,
                    "createdAt": request.created_at as f64,
                })
            })
            .collect();
        Ok(Value::Array(rows))
    }

    /// A conversation's tasks, as the chat's task rows show them.
    fn chat_sessions(&self, session: &str) -> Result<Value, String> {
        let db = self.db()?;
        let now = now_ms();
        let models = self.all_model_options();
        let mut rows = Vec::new();
        for (task, owner) in handoff::for_chat(db, session)? {
            let open = open_requests(db, &task.id)?;
            let status = shown_status(&task, &open, now);
            let doing = doing(db, &task, &open)?;
            let mut row = json!({
                "taskId": task.id,
                "tool": task.tool,
                "title": task.title,
                "status": status,
                "now": doing,
                "pending": open.len() as f64,
                "createdAt": task.created_at as f64,
                "updatedAt": task.updated_at as f64,
                "computer": { "name": "This Mac", "lastSeenAt": now as f64 },
                "repoName": task.project_name,
                "plainFolder": task.plain_folder,
            });
            if let Some(branch) = &task.branch {
                row["branch"] = json!(branch);
            }
            if let Some(model) = &task.model {
                let engine = coding::engine_of(&task.tool);
                let label = models
                    .iter()
                    .find(|option| Some(option.engine) == engine && option.matches(model))
                    .map_or_else(|| model.clone(), |option| option.label.clone());
                row["model"] = json!(label);
            }
            if let Some(started) = owner.round_started_at.filter(|_| task.status == "running") {
                row["roundStartedAt"] = json!(started as f64);
            }
            // How long it has worked, for "Done · 18m" once it's finished.
            if let Some(worked) = handoff::worked_ms(db, &task.id)? {
                row["workedMs"] = json!(worked as f64);
            }
            rows.push(row);
        }
        Ok(Value::Array(rows))
    }
}

impl Local {
    /// The pet's tray: each task working, with the question or permission it waits on the user for (the chat's card
    /// rule), and those finished in the last few minutes with how they went.
    fn tray_tasks(&self) -> Result<Value, String> {
        let db = self.db()?;
        let now = now_ms();
        let group = self.group_session();
        let mut rows = Vec::new();
        for (task, owner) in handoff::unfinished(db)? {
            let requests = handoff::requests_for_task(db, &task.id)?;
            let ask = requests
                .iter()
                .find(|request| request.state == "pending" && handoff::shows_in_chat(request, now))
                .map(|request| {
                    let (question, options) = match (&request.question, request.asked_user) {
                        (Some(question), true) => (question.clone(), request.user_options.clone()),
                        _ => (request.title.clone(), request.options.clone()),
                    };
                    json!({ "requestId": request.id, "kind": request.kind, "question": question, "options": options })
                });
            let mut row = tray_row(&task, &owner, group.as_deref());
            row["since"] = json!(handoff::working_since(db, &task, &owner)? as f64);
            row["ask"] = ask.unwrap_or(Value::Null);
            rows.push(row);
        }
        for (task, owner) in handoff::finished_since(db, now - TRAY_FINISHED_MS)? {
            let round = handoff::latest_round(db, &task.id)?;
            let summary = match (
                task.status.as_str(),
                round.as_ref().and_then(|round| round.result.as_deref()),
                task.error.as_deref(),
            ) {
                ("failed", _, Some(error)) => summary_of(error),
                ("stopped", None, _) => String::new(),
                (_, Some(result), _) => summary_of(result),
                _ => task.last_message.as_deref().map(summary_of).unwrap_or_default(),
            };
            let mut row = tray_row(&task, &owner, group.as_deref());
            row["finishedAt"] =
                json!(round.as_ref().and_then(|round| round.finished_at).unwrap_or(task.updated_at) as f64);
            row["summary"] = json!(summary);
            row["round"] = round.map_or(Value::Null, |round| json!(round.round as f64));
            rows.push(row);
        }
        Ok(Value::Array(rows))
    }
}

/// What a tray row says about any task: which it is, whose, and where its conversation is (`group`: the group's).
fn tray_row(task: &Task, owner: &Handoff, group: Option<&str>) -> Value {
    json!({
        "taskId": task.id,
        "sessionId": owner.chat_id,
        "agentId": owner.agent_id,
        // The agents' group's (one a member started in its own chat for the group is that chat's).
        "group": group == Some(owner.chat_id.as_str()),
        "title": task.title,
        "status": task.status,
    })
}

/// A task's requests still waiting on an answer.
fn open_requests(db: &Db, task_id: &str) -> Result<Vec<handoff::Request>, String> {
    Ok(handoff::requests_for_task(db, task_id)?.into_iter().filter(|request| request.state == "pending").collect())
}

/// A task's status as every place shows it (the chat's rows and result cards, the Coding page, a round's result page;
/// `screens::coding::task_state` reads it): its own, but a running one with a question open is "waiting_on_you" while
/// the question is with the user, else "waiting_on_agent".
fn shown_status<'a>(task: &'a Task, open: &[handoff::Request], now: i64) -> &'a str {
    match task.status.as_str() {
        "running" if open.iter().any(|request| handoff::shows_in_chat(request, now)) => "waiting_on_you",
        "running" if !open.is_empty() => "waiting_on_agent",
        status => status,
    }
}

/// `shown_status`, reading the task's open requests.
pub(super) fn task_status(db: &Db, task: &Task, now: i64) -> Result<String, String> {
    if task.status != "running" {
        return Ok(task.status.clone());
    }
    Ok(shown_status(task, &open_requests(db, &task.id)?, now).to_string())
}

/// What a task is doing now, in a line: what it asks, else its latest step, else its state.
fn doing(db: &Db, task: &Task, open: &[handoff::Request]) -> Result<String, String> {
    Ok(match open.first() {
        Some(request) => format!("Asks: {}", request.title),
        None => coding::latest_event_not_from(db, &task.id, "owner")?
            .map(|event| filo_local::store::cut(&event.text, 120))
            .unwrap_or_else(|| status_word(&task.status).to_string()),
    })
}

/// A round's report in a line: its first line of prose, without the markdown around it.
fn summary_of(text: &str) -> String {
    let line = text
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '>', '-', '*', ' ']).trim())
        .find(|line| !line.is_empty() && !line.starts_with("```") && !line.starts_with("!["))
        .unwrap_or_default();
    filo_local::store::cut(&line.replace("**", "").replace('`', ""), 160)
}

fn status_word(status: &str) -> &'static str {
    match status {
        // The words of the chat card's `status_label` (screens/coding.rs), which it then doesn't repeat under the pill.
        "queued" => "Queued",
        "running" => "Working",
        "monitoring" => "Monitoring",
        "done" => "Done",
        "failed" => "Failed",
        _ => "Stopped",
    }
}

/// A round's result page: the tool's final message and the files it saved as proof (`handoff::round_proof`: its own, not
/// those of a round that carried on from it in the same folder).
pub(super) fn round_result(task: &Task, round: &handoff::Round, proof: Vec<handoff::ProofFile>) -> Value {
    let result = match (round.status.as_str(), &round.result, &round.error) {
        ("failed", _, Some(error)) => format!("It didn't finish: {error}"),
        (_, Some(result), _) => result.clone(),
        ("stopped", None, _) => "It was stopped before it finished.".into(),
        _ => "It ended without a final message.".into(),
    };
    // The page shows a proof file where the message links it by name; those it didn't link (or named by their full
    // path) are listed after it, so every file the tool saved can be opened.
    let unlinked: Vec<&str> = proof
        .iter()
        .map(|file| file.name.as_str())
        .filter(|name| !result.contains(&format!("]({name})")) && !result.contains(&format!("](<{name}>)")))
        .collect();
    let result = if unlinked.is_empty() {
        result
    } else {
        // As embeds: a picture draws, any other file is a row that opens it.
        let links = unlinked.iter().map(|name| format!("![{name}](<{name}>)")).collect::<Vec<_>>().join("\n\n");
        format!("{result}\n\n**Proof it saved**\n\n{links}")
    };
    let files: Vec<Value> = proof
        .into_iter()
        .map(|file| {
            json!({
                "proofId": format!("{}-{}-{}", task.id, round.round, file.name),
                "kind": file.kind,
                "name": file.name,
                "url": file.path.display().to_string(),
                "contentType": file.content_type,
                "size": file.size as f64,
            })
        })
        .collect();
    json!({
        "title": task.title,
        "tool": task.tool,
        "status": round.status,
        "round": round.round as f64,
        "rounds": task.rounds.max(round.round) as f64,
        "createdAt": round.finished_at.unwrap_or(round.started_at) as f64,
        "result": result,
        "files": files,
    })
}

/// What a round reports to: the task's thread and requests in Filo's database, the conversation's answers and notes.
struct RoundHost<'a> {
    local: &'a Arc<Local>,
    db: &'a Db,
    task: &'a Task,
    owner: &'a Handoff,
    /// The round under way, or the one that ended while it waits on background work.
    round: i64,
    /// The round's proof folder: a follow-up after background work saves in it too (the tool was told it).
    proof: PathBuf,
    /// Its turn ended with background work still going: `round` ended, and the next starts when it carries on.
    waiting: bool,
    /// A follow-up round runs: background work again at its turn's end keeps it open.
    followed: bool,
    stop: &'a Stoppable,
    /// Requests whose answer the thread already shows.
    answered: HashSet<String>,
}

impl Host for RoundHost<'_> {
    fn step(&mut self, step: Step) {
        let _ = coding::add_event(self.db, &self.task.id, now_ms(), &step);
        self.local.changed();
    }

    fn session(&mut self, id: &str) {
        let _ = coding::set_session(self.db, &self.task.id, id);
    }

    fn model(&mut self, model: &str) {
        if coding::set_run_model(self.db, &self.task.id, model).is_ok() {
            self.local.changed();
        }
    }

    fn ask(&mut self, ask: &Ask) -> String {
        let now = now_ms();
        let Ok(request) = handoff::add_request(
            self.db,
            &self.task.id,
            self.round,
            ask.kind,
            &ask.title,
            ask.detail.as_deref(),
            &ask.options,
            now,
        ) else {
            // Not saved, so nobody could answer it: refused, and the tool carries on without.
            return String::new();
        };
        let _ = handoff::add_option_notes(self.db, &request, &ask.options, &ask.notes);
        let who = if self.task.owner.trim().is_empty() { "Filo" } else { self.task.owner.as_str() };
        let _ =
            coding::add_event(self.db, &self.task.id, now, &Step::marker("note", format!("Asks {who}: {}", ask.title)));
        if let Ok(Some(saved)) = handoff::request(self.db, &request) {
            let text = handoff::question_message(self.task, self.owner, &saved);
            let _ = handoff::add_wake(self.db, self.owner, "question", self.round, &text, now);
        }
        self.local.changed();
        self.local.deliver_wakes();
        request
    }

    fn answer(&mut self, key: &str) -> Option<String> {
        if key.is_empty() {
            return Some("deny".into());
        }
        let request = handoff::request(self.db, key).ok()??;
        match request.state.as_str() {
            "answered" => {
                let answer = request.answer.clone().unwrap_or_else(|| "deny".into());
                if self.answered.insert(request.id.clone()) {
                    let by = if request.decided_by.as_deref() == Some("user") {
                        "you".to_string()
                    } else {
                        self.task.owner.clone()
                    };
                    let shown = match answer.as_str() {
                        "allow" => "Allowed".to_string(),
                        "allow_always" => "Allowed for the rest of the session".to_string(),
                        "deny" => "Denied".to_string(),
                        other => format!("\"{other}\""),
                    };
                    let _ = coding::add_event(
                        self.db,
                        &self.task.id,
                        now_ms(),
                        &Step::marker("note", format!("{shown} (answered by {by})")),
                    );
                    self.local.changed();
                }
                Some(answer)
            }
            "expired" => Some("deny".into()),
            _ => None,
        }
    }

    fn notes(&mut self) -> Vec<String> {
        let Ok(notes) = handoff::undelivered_notes(self.db, &self.task.id) else { return Vec::new() };
        if notes.is_empty() {
            return Vec::new();
        }
        let _ = handoff::mark_delivered(self.db, &notes.iter().map(|(id, _)| *id).collect::<Vec<_>>(), now_ms());
        let who = if self.task.owner.trim().is_empty() { "Filo" } else { self.task.owner.as_str() };
        notes.into_iter().map(|(_, note)| format!("A note from {who}: {note}")).collect()
    }

    fn stop_requested(&mut self) -> bool {
        self.stop.requested()
    }

    /// Its turn ended with background work going: the round ends (the conversation hears what it said and what it
    /// waits on) and the task is "monitoring". A follow-up's turn ending so stays its round.
    fn background(&mut self, end: &coding_session::End, what: &[String]) {
        let (db, id, now) = (self.db, self.task.id.as_str(), now_ms());
        let marker = match what {
            [] => "Monitoring background work".to_string(),
            what => format!("Monitoring: {}", filo_local::store::cut(&what.join("; "), 240)),
        };
        if self.followed {
            let _ = coding::set_monitoring(db, id, end.final_text.as_deref(), &marker, false, now);
        } else {
            let _ = handoff::finish_round(db, id, self.round, "monitoring", end.final_text.as_deref(), None, now);
            let _ = handoff::expire_pending(db, id, now);
            let _ = coding::set_monitoring(db, id, end.final_text.as_deref(), &marker, true, now);
            if let (Ok(Some(task)), Ok(Some(ended))) = (coding::task(db, id), handoff::round(db, id, self.round)) {
                let text = handoff::monitoring_message(&task, self.owner, &ended, what);
                let _ = handoff::add_wake(db, self.owner, "monitoring", self.round, &text, now);
            }
            self.waiting = true;
        }
        self.local.changed();
        self.local.deliver_wakes();
    }

    /// It picked the task back up: the round it waited after stays Monitoring (it reported no result, only what it
    /// waited on), and its follow-up is the next round.
    fn resumed(&mut self, why: Option<&str>) {
        let (db, id, now) = (self.db, self.task.id.as_str(), now_ms());
        if self.waiting {
            self.round += 1;
            let _ = handoff::start_round(db, id, self.round, &self.proof, now);
            self.waiting = false;
            self.followed = true;
        }
        let _ = coding::set_running(db, id, now);
        let said = why.map_or_else(
            || "Carrying on".to_string(),
            |why| format!("Background work ended: {}", filo_local::store::cut(why, 400)),
        );
        let _ = coding::add_event(db, id, now, &Step::marker("note", said));
        self.local.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_backend::test_support::{call, script};
    use filo_local::coding::{NewTask, Project};
    use filo_local::coding_handoff::NewHandoff;
    use std::path::Path;
    use std::time::Instant;

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("handoff")
    }

    fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
        let started = Instant::now();
        while !ready() {
            assert!(started.elapsed() < Duration::from_secs(20), "waited too long for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A task as start_work makes it (coding_mcp.rs), owned by `chat` (and `speaker` in the group).
    fn hand_off(local: &Arc<Local>, chat: &str, speaker: Option<&str>, folder: &Path) -> Task {
        let db = local.db().unwrap();
        let project: Project = filo_local::coding::add_project(db, folder, 1).unwrap();
        let brief = handoff::render_brief("Install the dependencies", "npm install has run", None);
        let task = filo_local::coding::create_task(
            db,
            &NewTask { brief, tool: "claude-code".into(), project, worktree: false, owner: "Filo".into() },
            1,
        )
        .unwrap();
        let new = NewHandoff {
            chat_id: chat.into(),
            agent_id: None,
            speaker: speaker.map(str::to_string),
            title: "Install dependencies".into(),
            goal: "Install the dependencies".into(),
            done_when: "npm install has run".into(),
            notes: None,
            look_only: false,
            model: None,
            effort: None,
        };
        handoff::create(db, &task.id, &new, 1).unwrap()
    }

    #[test]
    fn needs_input_follows_unanswered_cards_here_and_on_tasks_started_elsewhere() {
        let local = temp();
        let folder = local.root.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let db = local.db().unwrap();
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let agent_id = local.filo_agent().id;
        filo_local::app_data::set_agent_session(db, &agent_id, &chat.id).unwrap();
        let rows = || call(&local, "agents:list", json!({})).unwrap();
        assert_eq!(rows()[0]["needsInput"], false);

        // The tool asks its agent first. It needs the user only when the card becomes visible.
        let task = hand_off(&local, &chat.id, None, &folder);
        let now = now_ms();
        let question = handoff::add_request(db, &task.id, 1, "question", "Which color?", None, &[], now).unwrap();
        assert!(!local.coding_input_owners(now).0.contains(&chat.id));
        assert!(local.coding_input_owners(now + handoff::SHOW_UNDECIDED_AFTER_MS).0.contains(&chat.id));
        handoff::ask_user(db, &question, "Which color?", &[], now).unwrap();
        assert_eq!(rows()[0]["needsInput"], true);
        let approval = handoff::add_request(db, &task.id, 1, "approval", "Run install?", None, &[], now).unwrap();
        handoff::ask_user(db, &approval, "Allow install?", &[], now).unwrap();
        call(&local, "coding:answerRequest", json!({ "requestId": question, "answer": "Blue" })).unwrap();
        assert_eq!(rows()[0]["needsInput"], true);
        call(&local, "coding:answerRequest", json!({ "requestId": approval, "answer": "deny" })).unwrap();
        assert_eq!(rows()[0]["needsInput"], false);
        // The task is still working: clearing input leaves its normal working state available.
        assert!(
            call(&local, "coding:workingChats", json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["sessionId"] == chat.id)
        );

        // A task the agent started in the group still marks its own row, even with its chat closed.
        let group = filo_local::group::ensure(db, now).unwrap();
        let elsewhere = hand_off(&local, &group, Some(&agent_id), &folder);
        db.write(|tx| {
            tx.execute("UPDATE coding_handoffs SET agent_id = ?1 WHERE task_id = ?2", [&agent_id, &elsewhere.id])
        })
        .unwrap();
        let request = handoff::add_request(db, &elsewhere.id, 1, "approval", "Run command?", None, &[], now).unwrap();
        handoff::ask_user(db, &request, "Allow command?", &[], now).unwrap();
        assert_eq!(rows()[0]["needsInput"], true);
        assert_eq!(call(&local, "agents:group", json!({})).unwrap()["needsInput"], true);
        handoff::expire_pending(db, &elsewhere.id, now + 1).unwrap();
        assert_eq!(rows()[0]["needsInput"], false);
        assert_eq!(call(&local, "agents:group", json!({})).unwrap()["needsInput"], false);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_tray_says_what_each_task_does_asks_and_finished_with() {
        let local = temp();
        let folder = local.root.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let db = local.db().unwrap();
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let task = hand_off(&local, &chat.id, None, &folder);
        let now = now_ms();
        filo_local::coding::set_running(db, &task.id, now).unwrap();
        let tray = || call(&local, "coding:trayTasks", json!({})).unwrap();
        let row = tray()[0].clone();
        assert_eq!(
            (row["taskId"].as_str(), row["sessionId"].as_str(), row["title"].as_str()),
            (Some(task.id.as_str()), Some(chat.id.as_str()), Some("Install dependencies"))
        );
        assert_eq!(
            (row["status"].as_str(), row["ask"].is_null(), row["group"].as_bool()),
            (Some("running"), true, Some(false))
        );

        // Asked of its agent first: it isn't the user's yet; handed over, it is.
        let request = handoff::add_request(db, &task.id, 1, "approval", "Run `npm install`", None, &[], now).unwrap();
        assert!(tray()[0]["ask"].is_null());
        handoff::ask_user(db, &request, "Let Claude Code run npm install?", &[], now).unwrap();
        assert_eq!(
            tray()[0]["ask"],
            json!({ "requestId": request, "kind": "approval", "question": "Let Claude Code run npm install?", "options": [] })
        );
        call(&local, "coding:answerRequest", json!({ "requestId": request, "answer": "allow" })).unwrap();
        assert!(tray()[0]["ask"].is_null());

        // Finished: listed for a while with its report's first line.
        handoff::start_round(db, &task.id, 1, &folder, now).unwrap();
        handoff::finish_round(
            db,
            &task.id,
            1,
            "done",
            Some("## Done\n\nInstalled **12** packages with `npm install`.\n\nMore."),
            None,
            now + 5,
        )
        .unwrap();
        filo_local::coding::finish_round(db, &task.id, "done", None, None, now + 5).unwrap();
        let row = tray()[0].clone();
        assert_eq!(
            (row["status"].as_str(), row["summary"].as_str(), row["round"].as_f64(), row["finishedAt"].as_f64()),
            (Some("done"), Some("Done"), Some(1.), Some((now + 5) as f64))
        );
        assert!(row.get("ask").is_none() && row.get("since").is_none());
        // Its time, the same on its result page, its chat row and the Coding page.
        let page = call(&local, "coding:roundResult", json!({ "taskId": task.id })).unwrap()["workedMs"].clone();
        assert!(page.as_f64().is_some_and(|worked| (0. ..=5.).contains(&worked)));
        assert_eq!(
            call(&local, "coding:sessionsForChat", json!({ "sessionId": chat.id })).unwrap()[0]["workedMs"],
            page
        );
        assert_eq!(call(&local, "coding:tasks", json!({})).unwrap()[0]["workedMs"], page);
        assert_eq!(summary_of("## Done\n\nInstalled **12** packages"), "Done");
        assert_eq!(summary_of("![shot](a.png)\n- Installed **12** `pkgs`"), "Installed 12 pkgs");
        // Long ago: gone.
        db.write(|tx| tx.execute("UPDATE coding_tasks SET updated_at = 1 WHERE id = ?1", [&task.id])).unwrap();
        assert_eq!(tray(), json!([]));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_result_page_lists_proof_the_message_didnt_link() {
        let dir = std::env::temp_dir().join(format!("filo-round-result-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("check.log"), "ok").unwrap();
        std::fs::write(dir.join("after.png"), [0u8; 4]).unwrap();
        let db = filo_local::db::Db::in_memory().unwrap();
        let project = Project {
            name: "p".into(),
            path: "/p".into(),
            branch: None,
            remote: None,
            plain_folder: true,
            added: true,
        };
        let task = filo_local::coding::create_task(
            &db,
            &NewTask { brief: "Goal: x".into(), tool: "codex".into(), project, worktree: false, owner: "Filo".into() },
            1,
        )
        .unwrap();
        let round = handoff::Round {
            task_id: task.id.clone(),
            round: 1,
            status: "done".into(),
            result: Some(format!("Done. ![After](after.png) Log at {}/check.log", dir.display())),
            error: None,
            proof_dir: dir.display().to_string(),
            started_at: 1,
            finished_at: Some(2),
        };
        let page = round_result(&task, &round, handoff::proof_files(&dir));
        let result = page["result"].as_str().unwrap();
        assert!(result.ends_with("**Proof it saved**\n\n![check.log](<check.log>)"), "{result}");
        assert_eq!(page["files"].as_array().unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A chat hands a task off: the loop starts it, the tool asks for permission and the chat is woken to answer; the
    /// user's tap answers it; the round ends with proof, and the chat is woken with the result, whose reply carries the
    /// round's Open proof.
    #[test]
    fn a_chat_hears_its_tasks_question_and_result() {
        let local = temp();
        let folder = local.root.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        // Speaks Claude Code's stream-json control protocol: asks to run a command, waits for the answer, saves a log in
        // its proof folder (`--add-dir`) and reports.
        let fake = script(
            &local.root,
            "claude",
            r#"#!/bin/sh
proof=""
while [ $# -gt 0 ]; do case "$1" in --add-dir) proof="$2"; shift;; esac; shift; done
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"npm install"}}]},"parent_tool_use_id":null}'
echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"npm install"},"tool_use_id":"t1"}}'
while read line; do case "$line" in *control_response*) answer="$line"; break;; esac; done
case "$answer" in *'"behavior":"allow"'*) echo "added 12 packages" > "$proof/install.log";; esac
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Installed them."}]},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Installed the dependencies. See [the log](install.log).","session_id":"s-1"}'
read rest
"#,
        );
        local.set_coding_program("claude-code", fake);
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let task = hand_off(&local, &chat.id, None, &folder);

        local.handoff_tick();
        let db = local.db().unwrap();
        wait_for("the question", || handoff::requests_for_task(db, &task.id).unwrap().len() == 1);
        let request = handoff::requests_for_task(db, &task.id).unwrap().remove(0);
        assert_eq!((request.kind.as_str(), request.title.as_str()), ("approval", "Run `npm install`"));
        // The chat was woken with the question, in a message only its engine sees.
        wait_for("the question's wake", || local.coding.woken.lock().unwrap().len() == 1);
        let messages = local.agent_chats.messages(&chat.id).unwrap();
        let woken = messages.iter().find(|message| message.hidden).unwrap();
        assert!(woken.text.starts_with("[Coding task question]") && woken.text.contains(&request.id), "{}", woken.text);
        let detail = call(&local, "voiceSessions:detail", json!({ "id": chat.id })).unwrap();
        assert!(detail["messages"].as_array().unwrap().is_empty());
        let rows = call(&local, "coding:sessionsForChat", json!({ "sessionId": chat.id })).unwrap();
        assert_eq!((rows[0]["status"].as_str(), rows[0]["pending"].as_f64()), (Some("waiting_on_agent"), Some(1.)));
        // Handed to the user: the card shows; their tap answers the tool.
        handoff::ask_user(db, &request.id, "Let Claude Code run npm install in project?", &[], 2).unwrap();
        let cards = call(&local, "coding:requestsForSession", json!({ "sessionId": chat.id })).unwrap();
        assert_eq!(
            (cards[0]["question"].as_str(), cards[0]["state"].as_str(), cards[0]["tool"].as_str()),
            (Some("Let Claude Code run npm install in project?"), Some("pending"), Some("Claude Code"))
        );
        // Its row, the Coding page and the round's page all say it needs the user.
        let rows = call(&local, "coding:sessionsForChat", json!({ "sessionId": chat.id })).unwrap();
        assert_eq!(rows[0]["status"], "waiting_on_you");
        assert_eq!(call(&local, "coding:tasks", json!({})).unwrap()[0]["status"], "waiting_on_you");
        assert_eq!(
            call(&local, "coding:roundResult", json!({ "taskId": task.id, "round": 1 })).unwrap()["taskStatus"],
            "waiting_on_you"
        );
        call(&local, "coding:answerRequest", json!({ "requestId": request.id, "answer": "allow" })).unwrap();
        assert!(call(&local, "coding:answerRequest", json!({ "requestId": request.id, "answer": "deny" })).is_err());

        wait_for("the round's end", || {
            filo_local::coding::task(db, &task.id).unwrap().unwrap().status == "done"
                && !local.coding_round_running(&task.id)
        });
        wait_for("the result's wake", || local.coding.woken.lock().unwrap().len() == 2);
        let (_, turn) = local.coding.woken.lock().unwrap()[1].clone();
        let messages = local.agent_chats.messages(&chat.id).unwrap();
        let report =
            messages.iter().find(|message| message.hidden && message.turn.as_deref() == Some(turn.as_str())).unwrap();
        assert!(
            report.text.starts_with("[Coding task round 1 done")
                && report.text.contains("Installed the dependencies")
                && report.text.contains("install.log (file)"),
            "{}",
            report.text
        );
        // Its reply (what the MCP server's send_message saves) carries the round's Open proof.
        local
            .agent_chats
            .append(
                &chat.id,
                &Message::new(Role::Assistant, "Claude Code installed the dependencies.", Some(turn.clone())),
            )
            .unwrap();
        let detail = call(&local, "voiceSessions:detail", json!({ "id": chat.id })).unwrap();
        let shown = detail["messages"].as_array().unwrap();
        assert_eq!(shown.len(), 1);
        let mut coding_result = shown[0]["codingResult"].clone();
        // With how long the task worked (the script's round, in milliseconds).
        assert!(
            coding_result
                .as_object_mut()
                .unwrap()
                .remove("workedMs")
                .is_some_and(|worked| worked.as_f64().is_some_and(|worked| worked >= 0.))
        );
        assert_eq!(
            coding_result,
            json!({ "taskId": task.id, "round": 1.0, "title": "Install dependencies", "tool": "claude-code", "status": "done", "taskStatus": "done" })
        );

        let result = call(&local, "coding:roundResult", json!({ "taskId": task.id, "round": 1 })).unwrap();
        assert_eq!(
            (result["status"].as_str(), result["result"].as_str()),
            (Some("done"), Some("Installed the dependencies. See [the log](install.log)."))
        );
        assert_eq!(
            (result["files"][0]["name"].as_str(), result["files"][0]["kind"].as_str()),
            (Some("install.log"), Some("file"))
        );
        assert!(Path::new(result["files"][0]["url"].as_str().unwrap()).is_file());
        let cards = call(&local, "coding:requestsForSession", json!({ "sessionId": chat.id })).unwrap();
        assert_eq!(
            (cards[0]["state"].as_str(), cards[0]["answer"].as_str(), cards[0]["decidedBy"].as_str()),
            (Some("answered"), Some("allow"), Some("user"))
        );
        let thread = call(&local, "coding:thread", json!({ "taskId": task.id })).unwrap();
        let texts: Vec<&str> =
            thread["entries"].as_array().unwrap().iter().map(|entry| entry["text"].as_str().unwrap()).collect();
        assert!(
            texts.contains(&"Asks Filo: Run `npm install`")
                && texts.contains(&"Allowed (answered by you)")
                && texts.contains(&"Installed them."),
            "{texts:?}"
        );
        assert_eq!(call(&local, "coding:workingChats", json!({})).unwrap(), json!([]));
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// The user answers a card in their own words: the tool gets the words, and the chat is woken with them too, for
    /// what else they ask of it ("take a note"). A picked or skipped answer only goes to the tool.
    #[test]
    fn an_answer_typed_on_a_card_reaches_the_chat_too() {
        let local = temp();
        let folder = local.root.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let task = hand_off(&local, &chat.id, None, &folder);
        let db = local.db().unwrap();
        let ask = |title: &str| {
            let id = handoff::add_request(
                db,
                &task.id,
                1,
                "question",
                title,
                None,
                &["Option A".into(), "Option B".into()],
                1,
            )
            .unwrap();
            handoff::ask_user(db, &id, title, &["Fix it now".into(), "Leave it".into()], 2).unwrap();
            id
        };
        let (picked, skipped, typed) = (ask("Which band?"), ask("Which colour?"), ask("Fix the menu band now?"));
        call(&local, "coding:answerRequest", json!({ "requestId": picked, "answer": "Leave it" })).unwrap();
        call(&local, "coding:answerRequest", json!({ "requestId": skipped, "answer": handoff::SKIPPED })).unwrap();
        local.deliver_wakes();
        assert!(local.coding.woken.lock().unwrap().is_empty());

        call(
            &local,
            "coding:answerRequest",
            json!({ "requestId": typed, "answer": "okay let's leave it for later take a note" }),
        )
        .unwrap();
        assert_eq!(
            handoff::request(db, &typed).unwrap().unwrap().answer.as_deref(),
            Some("okay let's leave it for later take a note")
        );
        local.deliver_wakes();
        assert_eq!(local.coding.woken.lock().unwrap().len(), 1);
        let messages = local.agent_chats.messages(&chat.id).unwrap();
        let woken = messages.iter().find(|message| message.hidden).unwrap();
        assert!(woken.text.starts_with("[Coding task question answered by the user]"), "{}", woken.text);
        assert!(
            woken.text.contains("Fix the menu band now?")
                && woken.text.contains("okay let's leave it for later take a note")
                && woken.text.contains("create_note"),
            "{}",
            woken.text
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A round whose turn ends with a build still running in the background (Claude Code's `run_in_background`): the
    /// task shows as monitoring everywhere until the build ends, the chat hears what it waits on, and the tool's
    /// follow-up once the build ends is the next round, reported as any other.
    #[test]
    fn a_round_that_leaves_background_work_monitors_it_and_reports_the_follow_up() {
        let local = temp();
        let folder = local.root.join("app");
        std::fs::create_dir_all(&folder).unwrap();
        // Starts a build in the background and ends its turn; once the test says the build is over (a file in its
        // proof folder), carries on by itself as Claude Code does, while its stdin is open.
        let fake = script(
            &local.root,
            "claude",
            r#"#!/bin/sh
proof=""
while [ $# -gt 0 ]; do case "$1" in --add-dir) proof="$2"; shift;; esac; shift; done
read init
read message
( cat > /dev/null; touch "$proof/closed" ) <&0 &
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"scripts/package.sh","run_in_background":true}}]},"parent_tool_use_id":null}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"Build the app"}]}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"The build runs in the background; I'"'"'ll check it when it ends.","session_id":"s-1"}'
while [ ! -f "$proof/built" ]; do [ -f "$proof/closed" ] && exit 0; sleep 0.1; done
echo '{"type":"system","subtype":"task_notification","task_id":"b1","status":"completed","summary":"Background command \"Build the app\" completed (exit code 0)"}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"The build passed."}]},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Built it: dist/App.zip passes its checks.","session_id":"s-1"}'
wait
"#,
        );
        local.set_coding_program("claude-code", fake);
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let task = hand_off(&local, &chat.id, None, &folder);
        let db = local.db().unwrap();
        let status = || filo_local::coding::task(db, &task.id).unwrap().unwrap().status;

        local.handoff_tick();
        wait_for("monitoring", || status() == "monitoring");
        // Still at work: its round runs on, the chat's row, the Coding page and the round's page say Monitoring, and the
        // chat works.
        assert!(local.coding_round_running(&task.id));
        let rows = call(&local, "coding:sessionsForChat", json!({ "sessionId": chat.id })).unwrap();
        assert_eq!(rows[0]["status"], "monitoring");
        assert_eq!(call(&local, "coding:tasks", json!({})).unwrap()[0]["status"], "monitoring");
        let result = call(&local, "coding:roundResult", json!({ "taskId": task.id, "round": 1 })).unwrap();
        assert_eq!(
            (result["status"].as_str(), result["taskStatus"].as_str()),
            (Some("monitoring"), Some("monitoring"))
        );
        assert_eq!(call(&local, "coding:workingChats", json!({})).unwrap()[0]["sessionId"], json!(chat.id));
        // The chat hears what it said and what it waits on; its reply opens the round, and the round isn't done.
        wait_for("the monitoring wake", || local.coding.woken.lock().unwrap().len() == 1);
        let (_, turn) = local.coding.woken.lock().unwrap()[0].clone();
        let messages = local.agent_chats.messages(&chat.id).unwrap();
        let told =
            messages.iter().find(|message| message.hidden && message.turn.as_deref() == Some(turn.as_str())).unwrap();
        assert!(
            told.text.starts_with("[Coding task round 1: still running background work]")
                && told.text.contains("Build the app")
                && told.text.contains("The build runs in the background"),
            "{}",
            told.text
        );
        local
            .agent_chats
            .append(
                &chat.id,
                &Message::new(
                    Role::Assistant,
                    "Claude Code is building it; I'll tell you when it's done.",
                    Some(turn.clone()),
                ),
            )
            .unwrap();
        let detail = call(&local, "voiceSessions:detail", json!({ "id": chat.id })).unwrap();
        let result = &detail["messages"][0]["codingResult"];
        assert_eq!((result["taskId"].as_str(), result["round"].as_f64()), (Some(task.id.as_str()), Some(1.)));
        assert_eq!(
            (result["status"].as_str(), result["taskStatus"].as_str()),
            (Some("monitoring"), Some("monitoring"))
        );
        // Asked to carry on meanwhile, the conversation's follow-up waits: it's still working.
        assert!(filo_local::coding::follow_up(db, &task.id, "Also sign it", 3).is_err());

        // The build ends: the tool carries on, as round 2, and the chat hears that round as any other.
        let proof = handoff::proof_dir(&local.root, &task.id, 1);
        std::fs::write(proof.join("built"), "").unwrap();
        wait_for("the follow-up's end", || status() == "done" && !local.coding_round_running(&task.id));
        let task_now = filo_local::coding::task(db, &task.id).unwrap().unwrap();
        assert_eq!(
            (task_now.rounds, task_now.last_message.as_deref()),
            (2, Some("Built it: dist/App.zip passes its checks."))
        );
        let first = handoff::round(db, &task.id, 1).unwrap().unwrap();
        let second = handoff::round(db, &task.id, 2).unwrap().unwrap();
        // The round it waited after stays Monitoring, never "done" with only what it waited on; its page leaves the
        // proof both share to round 2.
        assert_eq!(
            (first.status.as_str(), first.result.as_deref()),
            ("monitoring", Some("The build runs in the background; I'll check it when it ends."))
        );
        assert_eq!(
            (second.status.as_str(), second.result.as_deref()),
            ("done", Some("Built it: dist/App.zip passes its checks."))
        );
        wait_for("the follow-up's wake", || local.coding.woken.lock().unwrap().len() == 2);
        let (_, turn) = local.coding.woken.lock().unwrap()[1].clone();
        let messages = local.agent_chats.messages(&chat.id).unwrap();
        let report =
            messages.iter().find(|message| message.hidden && message.turn.as_deref() == Some(turn.as_str())).unwrap();
        assert!(
            report.text.starts_with("[Coding task round 2 done") && report.text.contains("dist/App.zip"),
            "{}",
            report.text
        );
        let thread = call(&local, "coding:thread", json!({ "taskId": task.id })).unwrap();
        let texts: Vec<&str> =
            thread["entries"].as_array().unwrap().iter().map(|entry| entry["text"].as_str().unwrap()).collect();
        let at = |text: &str| {
            texts.iter().position(|entry| *entry == text).unwrap_or_else(|| panic!("no {text:?} in {texts:?}"))
        };
        assert!(
            at("Monitoring: Build the app")
                < at("Background work ended: Background command \"Build the app\" completed (exit code 0)")
        );
        assert!(
            at("Background work ended: Background command \"Build the app\" completed (exit code 0)")
                < at("The build passed.")
                && at("The build passed.") < at("Finished")
        );
        assert_eq!(texts.iter().filter(|text| **text == "Finished").count(), 1, "{texts:?}");
        let page = call(&local, "coding:roundResult", json!({ "taskId": task.id, "round": 1 })).unwrap();
        assert_eq!((page["status"].as_str(), page["files"].as_array().map(Vec::len)), (Some("monitoring"), Some(0)));
        assert_eq!(page["taskStatus"], "done");
        let page = call(&local, "coding:roundResult", json!({ "taskId": task.id, "round": 2 })).unwrap();
        assert_eq!(page["files"][0]["name"], "built");
        let detail = call(&local, "voiceSessions:detail", json!({ "id": chat.id })).unwrap();
        let result = &detail["messages"][0]["codingResult"];
        assert_eq!((result["status"].as_str(), result["taskStatus"].as_str()), (Some("monitoring"), Some("done")));
        // Filo opening again leaves it so.
        handoff::expire_finished(db, 10).unwrap();
        assert_eq!(handoff::round(db, &task.id, 1).unwrap().unwrap().status, "monitoring");
        assert_eq!(call(&local, "coding:workingChats", json!({})).unwrap(), json!([]));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_wake_waits_for_the_chats_turn_and_the_group_gets_its_members_turn() {
        let local = temp();
        let folder = local.root.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let db = local.db().unwrap();
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let task = hand_off(&local, &chat.id, None, &folder);
        let owner = handoff::get(db, &task.id).unwrap().unwrap();
        handoff::add_wake(db, &owner, "failed", 1, "[Coding task round 1 failed]", 1).unwrap();
        // A turn runs in the chat: the wake waits.
        let running = super::super::Running { session_id: chat.id.clone(), ..Default::default() };
        local.running.lock().unwrap().insert("busy".into(), running);
        local.deliver_wakes();
        assert!(local.coding.woken.lock().unwrap().is_empty());
        assert_eq!(handoff::pending_wakes(db).unwrap().len(), 1);
        local.running.lock().unwrap().remove("busy");
        local.deliver_wakes();
        assert_eq!(local.coding.woken.lock().unwrap().len(), 1);
        assert!(handoff::pending_wakes(db).unwrap().is_empty());
        // Working chats list the chat while its task works.
        let working = call(&local, "coding:workingChats", json!({})).unwrap();
        assert_eq!(working[0]["sessionId"].as_str(), Some(chat.id.as_str()));
        let rows = call(&local, "voiceSessions:working", json!({ "include": [chat.id] })).unwrap();
        assert_eq!(rows[0]["_id"].as_str(), Some(chat.id.as_str()));

        // In the group, the member who started it gets a turn there with the news.
        let group = filo_local::group::ensure(db, 1).unwrap();
        let other = local.root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let group_task = hand_off(&local, &group, Some("filo"), &other);
        let owner = handoff::get(db, &group_task.id).unwrap().unwrap();
        handoff::add_wake(db, &owner, "round", 1, "[Coding task round 1 done]", 2).unwrap();
        local.deliver_wakes();
        let turn = filo_local::group::claim_next(db, 3).unwrap().unwrap();
        assert_eq!((turn.speaker.as_str(), turn.answers_user), ("filo", false));
        assert!(turn.reason.starts_with("[Coding task round 1 done"));
        assert_eq!(handoff::reported_round(db, &turn.id).unwrap(), Some((group_task.id.clone(), 1)));
        filo_local::group::finish(db, &turn.id, "completed", None, 4).unwrap();

        // Started by a member for the group in its own chat: the task is that chat's (its card and the pet's row open
        // there), and its news is the member's turn in the group, run in that chat with the news for the engine only.
        let filo_chat =
            local.open_agent_chat(crate::local_backend::DEFAULT_AGENT_ID).unwrap().as_str().unwrap().to_string();
        let third = local.root.join("third");
        std::fs::create_dir_all(&third).unwrap();
        let own_task = hand_off(&local, &filo_chat, Some("filo"), &third);
        let tray = call(&local, "coding:trayTasks", json!({})).unwrap();
        let row = tray.as_array().unwrap().iter().find(|row| row["taskId"] == json!(own_task.id)).unwrap().clone();
        assert_eq!((row["sessionId"].as_str(), row["group"].as_bool()), (Some(filo_chat.as_str()), Some(false)));
        let owner = handoff::get(db, &own_task.id).unwrap().unwrap();
        handoff::add_wake(db, &owner, "round", 1, "[Coding task round 1 done]", 5).unwrap();
        local.deliver_wakes();
        let turn = filo_local::group::claim_next(db, 6).unwrap().unwrap();
        assert_eq!((turn.speaker.as_str(), turn.answers_user), ("filo", false));
        local.add_group_call(&local.agent_chats, &filo_chat, &turn, &local.group_members()).unwrap();
        let news = local.agent_chats.messages(&filo_chat).unwrap().pop().unwrap();
        assert!(news.hidden && news.group_call.is_some() && news.turn.as_deref() == Some(turn.id.as_str()));
        assert!(news.text.contains("Why you're speaking now: [Coding task round 1 done"), "{}", news.text);
        // Its post in the group carries the turn, so it gets the round's Open proof; the chat has a line linking to it.
        assert_eq!(handoff::reported_round(db, &turn.id).unwrap(), Some((own_task.id.clone(), 1)));
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A conversation hands a small task to the installed `FILO_REAL_TOOL` (muse or opencode; `FILO_REAL_MODEL`,
    /// `FILO_REAL_EFFORT` for its model and reasoning level) end to end in Filo's backend: the loop starts it, what it
    /// asks (a question, then approvals: the task is in Supervised mode) comes back as the conversation's requests and
    /// is answered as the user's tap would, its steps show as they come, and the round ends with its report for the
    /// conversation. Uses the tool's sign-in, so it runs only when asked:
    /// `FILO_REAL_TOOL=muse cargo test --bin filo real_handoff -- --ignored --nocapture` (`FILO_REAL_LOG=<file>` keeps
    /// the thread).
    #[test]
    #[ignore]
    fn real_handoff_runs_end_to_end() {
        let tool = std::env::var("FILO_REAL_TOOL").unwrap_or_else(|_| "muse".into());
        let local = temp();
        let folder = local.root.join("hello-project");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("README.md"), "# Hello project\n").unwrap();
        call(&local, "coding:setPlugged", json!({ "tool": tool, "plugged": true })).unwrap();
        if let Ok(model) = std::env::var("FILO_REAL_MODEL") {
            call(&local, "coding:setToolModel", json!({ "tool": tool, "model": model })).unwrap();
        }
        if let Ok(effort) = std::env::var("FILO_REAL_EFFORT") {
            call(&local, "coding:setToolEffort", json!({ "tool": tool, "effort": effort })).unwrap();
        }
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let db = local.db().unwrap();
        let project: Project = filo_local::coding::add_project(db, &folder, 1).unwrap();
        let goal = "First ask the user, with your tool for asking a question (options hi and hello), which word to write. Then write it into word.txt with a shell command, and show the file with another shell command";
        let brief = handoff::render_brief(goal, "word.txt holds the word the user picked", None);
        let task = filo_local::coding::create_task(
            db,
            &NewTask { brief, tool: tool.clone(), project, worktree: false, owner: "Filo".into() },
            1,
        )
        .unwrap();
        // Supervised (a mode tasks made earlier may have), so it has to ask before its commands.
        db.write(|tx| tx.execute("UPDATE coding_tasks SET mode = 'supervised' WHERE id = ?1", [&task.id])).unwrap();
        let new = NewHandoff {
            chat_id: chat.id.clone(),
            agent_id: None,
            speaker: None,
            title: "Write the word".into(),
            goal: goal.into(),
            done_when: "word.txt holds the word the user picked".into(),
            notes: None,
            look_only: false,
            model: None,
            effort: None,
        };
        handoff::create(db, &task.id, &new, 1).unwrap();

        local.handoff_tick();
        let started = Instant::now();
        let mut answered = Vec::new();
        loop {
            for request in handoff::requests_for_task(db, &task.id)
                .unwrap()
                .into_iter()
                .filter(|request| request.state == "pending")
            {
                let answer = if request.kind == "question" { "hello" } else { "allow" };
                println!("asks ({}): {} -> {answer}", request.kind, request.title);
                call(&local, "coding:answerRequest", json!({ "requestId": request.id, "answer": answer })).unwrap();
                answered.push((request.kind.clone(), request.title.clone()));
            }
            let status = filo_local::coding::task(db, &task.id).unwrap().unwrap().status;
            if filo_local::coding::is_finished(&status) && !local.coding_round_running(&task.id) {
                break;
            }
            if started.elapsed() > Duration::from_secs(300) {
                let thread = call(&local, "coding:thread", json!({ "taskId": task.id })).unwrap();
                panic!("the round took too long: {:#}", thread["entries"]);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let thread = call(&local, "coding:thread", json!({ "taskId": task.id })).unwrap();
        let lines: Vec<String> = thread["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                format!(
                    "[{}{}] {}",
                    entry["from"].as_str().unwrap(),
                    entry["kind"].as_str().map(|kind| format!(":{kind}")).unwrap_or_default(),
                    entry["text"].as_str().unwrap()
                )
            })
            .collect();
        let row = &thread["task"];
        let summary =
            format!("{} task {}: {} (model {})\n{}\n", tool, task.id, row["status"], row["model"], lines.join("\n"));
        println!("{summary}");
        if let Ok(file) = std::env::var("FILO_REAL_LOG") {
            std::fs::write(file, &summary).unwrap();
        }
        assert_eq!(row["status"], "done");
        assert_eq!(std::fs::read_to_string(folder.join("word.txt")).unwrap().trim(), "hello");
        assert!(answered.iter().any(|(kind, _)| kind == "question"), "it asked which word: {answered:?}");
        // The conversation hears how the round went.
        wait_for("the result's wake", || {
            local.coding.woken.lock().unwrap().iter().any(|(chat_id, _)| *chat_id == chat.id)
        });
        let messages = local.agent_chats.messages(&chat.id).unwrap();
        assert!(
            messages.iter().any(|message| message.hidden && message.text.starts_with("[Coding task round 1 done")),
            "{messages:?}"
        );
        std::fs::remove_dir_all(&local.root).ok();
    }
}

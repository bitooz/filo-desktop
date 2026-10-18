// The agents' group chat on this Mac: `agents:group` and `agents:openGroup`, and the conversation functions
// (`voiceSessions:detail`, `sendMessage`, `chatTurns`, …) for the group's session, answered from Filo's database
// (crates/filo-local `group`), plus the runner that takes the group's turns one at a time while the app is open, and the
// turns of one message asking several members (@Names, "@everyone", an ask list) all at once.
//
// It's optional (crates/filo-local `group::Settings`): off until the user sets it up on the group's page in the Agents
// tab (`agents:setUpGroup`, the same call its Group settings save with), on with the agents they picked, and off again
// from its ⋯ (`agents:turnOffGroup`); `agents:groupSetup` says how it's set. While it's off it isn't a chat anywhere
// (`agents:group` has none), and agents asking each other work in their own chats (`run_agent_asks`).
//
// Who's in it: the agents the user picked, the main agent leading (crates/filo-local `main_agent`). What names no one
// goes to one quick call to the routing model (`run_routes`, crates/filo-local `group_route`), which picks who answers
// from the post, the recent messages and what each member is busy with; the group shows "Picking who answers"
// meanwhile. When the call can't be made, the one leading takes it. A member's post wakes only those in its ask list
// (send_message's `ask`), never one its text @-names. "@everyone" reaches every member at the same time, each
// checking in its own chat whether it's for them, free to say nothing, while the group shows who is checking. A check
// that stayed quiet leaves nothing in the member's chat (store.rs `shown`). The Filo agent fresh data starts with
// (`DEFAULT_AGENT_ID`) is Filo here (speaking by that id, named as on the profile), with its instructions, its own chat and
// its model, rather than a second "Filo".
//
// A member does its turns in its own chat (`run_in_own_chat`): the ask and the group's latest messages go there as a
// message from the group (store.rs `Message::group_call`), whose turn runs as any of the chat's: after the ones before
// it, resuming the chat's session (so it remembers the work later), in a worker, with its live activity and the coding
// tasks it starts, which report back there. Its send_message posts in the group in its name, leaving a line in the
// chat that links to the post (crates/filo-local mcp.rs `--own-chat`); the group shows it working, linked to its chat. Only Filo without the Filo
// agent (no chat of its own) answers on a fresh session that reads the group (`run_fresh`).
//
// Only the app runs the queue (`start`); the background scheduler opens the same data but leaves the group alone. A
// turn left running when the app closed is marked failed when it opens again, unless its worker carried on (it ends
// as it would have), and turns still waiting then run.
//
// Asks from the user's other chats (the MCP server's `ask_agents`, crates/filo-local `group::ask`) are threads queued
// by another process: the runner finds their turns as it finds any, and once nobody is left to speak in the thread
// gives the asker a turn back in the chat it asked from (`report_asks`).
//
// What a member makes in its turn (artifact cards) shows in the group, and a check-in it plans comes back here as its
// own turn (local_backend/scheduled.rs `run_wake`, `group_check_in`).
//
// The user names the group and picks its picture on its edit page (`agents:updateGroup`, crates/filo-local
// `group::look`): its name is its title wherever a conversation's title shows, "Group" while it has none.
//
// Files and pictures the user sends there are saved and shown as in an agent's chat (`sent_files`, `attachments_json`),
// and go with the group's message in the chat of each member answering in its thread (`add_group_call`), which hands
// them to its engine as that chat's own message's files would be (crates/filo-local turn.rs).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use filo_local::group::{self, Member, Setup, Turn};
use filo_local::store::{Attachment, GroupCall, LinkedMember, valid_id};
use filo_local::turn_state::{self, End};
use filo_local::{Message, Role, Store, chat_activity, memories};
use serde_json::{Value, json};

use super::workers::{self, Job};
use super::{
    DEFAULT_AGENT_ID, DEFAULT_AGENT_INSTRUCTIONS, DEFAULT_LIMIT, Local, PREVIEW_CHARS, Running, Stoppable, arg,
    default_engine, now_ms, required,
};

/// How often the runner looks for turns queued by another process (a terminal check) when nothing woke it.
const IDLE_LOOK: Duration = Duration::from_secs(2);

/// The runner's state: whether this process runs the group's turns, and its wake-up call.
#[derive(Default)]
pub struct State {
    driving: AtomicBool,
    halted: AtomicBool,
    wake: (Mutex<bool>, Condvar),
}

impl State {
    fn kick(&self) {
        let (lock, condvar) = &self.wake;
        *lock.lock().unwrap() = true;
        condvar.notify_one();
    }

    /// Waits for a kick, or `IDLE_LOOK`.
    fn rest(&self) {
        let (lock, condvar) = &self.wake;
        let guard = lock.lock().unwrap();
        let (mut woken, _) = condvar.wait_timeout_while(guard, IDLE_LOOK, |woken| !*woken).unwrap();
        *woken = false;
    }
}

/// Runs the group's turns while the app is open: what a closed app left running failed, what waits runs now.
pub fn start(local: Arc<Local>) {
    if local.group.driving.swap(true, Ordering::SeqCst) {
        return;
    }
    // Turns whose worker carried on while the app was closed end as they would have; the rest failed.
    let carried_on: Vec<workers::Worker> = workers::running(&local.root)
        .into_iter()
        .filter(|worker| matches!(&worker.job, Job::Turn { turn, .. } if local.is_group_turn(turn)))
        .collect();
    let ids: Vec<String> = carried_on
        .iter()
        .filter_map(|worker| if let Job::Turn { turn, .. } = &worker.job { Some(turn.clone()) } else { None })
        .collect();
    if let Ok(db) = local.db()
        && group::interrupted_except(db, now_ms(), &ids).unwrap_or(0) > 0
    {
        local.changed();
    }
    for worker in carried_on {
        local.follow_carried_on(worker);
    }
    // Posts the app left waiting for the routing call are routed now.
    if local.db().is_ok_and(|db| filo_local::group_route::requeue_running(db).is_ok()) {
        local.route_soon();
    }
    std::thread::spawn(move || {
        while !local.group.halted.load(Ordering::SeqCst) {
            local.report_asks();
            local.run_agent_asks();
            let next = local.db().ok().and_then(|db| group::claim_next(db, now_ms()).ok().flatten());
            match next {
                Some(turn) => local.run_group_turns(turn),
                None => local.group.rest(),
            }
        }
    });
}

impl Local {
    /// The group's conversation id, once there is a group.
    pub(super) fn group_session(&self) -> Option<String> {
        self.db().ok().and_then(|db| group::session(db).ok().flatten())
    }

    /// The group's name and picture as the user set them (`group::look`).
    fn group_look(&self) -> group::Look {
        self.db().ok().and_then(|db| group::look(db).ok()).unwrap_or_default()
    }

    /// The group's title wherever a conversation's title shows: its name, or "Group".
    fn group_title(&self) -> String {
        self.group_look().title().to_string()
    }

    /// The main agent leading, then the others (the Filo agent is Filo itself), oldest first (crates/filo-local
    /// `group::roster`).
    pub(super) fn group_members(&self) -> Vec<Member> {
        let mut members = self.db().map(group::members).unwrap_or_default();
        if let Some(filo) = members.iter_mut().find(|filo| filo.id == DEFAULT_AGENT_ID && filo.about.trim().is_empty())
        {
            filo.about = DEFAULT_AGENT_INSTRUCTIONS.to_string();
        }
        members
    }

    /// The agent a speaker is, for its own chat: Filo is the Filo agent while there is one.
    fn speaker_agent(&self, speaker: &str) -> Option<filo_local::app_data::Agent> {
        self.agents().into_iter().find(|agent| agent.id == speaker)
    }

    /// The speaker's own conversation, made the first time (the Filo agent's, for Filo); none for Filo without it.
    fn speaker_chat(&self, speaker: &str) -> Option<String> {
        let agent = self.speaker_agent(speaker)?;
        self.open_agent_chat(&agent.id).ok()?.as_str().map(str::to_string)
    }

    /// The group's functions, and the conversation functions when they're about the group's session; None for the rest.
    pub(super) fn group_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        match path {
            "agents:group" => return Some(self.group_row()),
            "agents:openGroup" => return Some(self.open_group()),
            "agents:updateGroup" => return Some(self.update_group(args)),
            "agents:groupSetup" => return Some(self.group_setup()),
            "agents:setUpGroup" => return Some(self.set_up_group(args)),
            "agents:turnOffGroup" => return Some(self.turn_off_group()),
            "agents:routingModels" => return Some(Ok(self.routing_models())),
            _ => {}
        }
        if !path.starts_with("voiceSessions:") {
            return None;
        }
        let session = self.group_session()?;
        let about = arg(args, "sessionId").or_else(|| arg(args, "id"));
        // A failure the group shows (a thread's, or a turn's under a member's message), or a member's own chat retrying
        // the group turn that failed there: the group runs it again, so it posts there and whoever it names follows.
        if path == "voiceSessions:retryChatTurn" {
            let turn = arg(args, "turnId")?;
            let db = self.db().ok()?;
            let ours = group::turns_of(db, turn).is_ok_and(|turns| !turns.is_empty())
                || group::turn(db, turn).ok().flatten().is_some_and(|turn| turn.state == "failed");
            return ours.then(|| self.group_retry(args));
        }
        // The chat's React names only the message: a group message's is kept with it.
        if path == "voiceSessions:reactToMessage" {
            let id = arg(args, "messageId")?;
            let db = self.db().ok()?;
            group::message(db, id).ok().flatten()?;
            let reacted = group::react(db, id, arg(args, "reaction").filter(|reaction| !reaction.trim().is_empty()));
            self.changed();
            return Some(reacted.map(|_| Value::Null));
        }
        if about != Some(session.as_str()) {
            return None;
        }
        Some(match path {
            "voiceSessions:detail" => self.group_detail(&session, args),
            "voiceSessions:chatTurns" => self.group_turns(),
            "voiceSessions:sendMessage" => self.group_send(&session, args),
            "voiceSessions:stopChatTurns" => {
                self.stop_group(&session);
                Ok(Value::Null)
            }
            _ => return None,
        })
    }

    /// `agents:group`: the group while it's on and there's an agent, with its last message, unseen count and members.
    fn group_row(&self) -> Result<Value, String> {
        let db = self.db()?;
        let agents = self.agents();
        let Some(session) = group::session(db)? else { return Ok(Value::Null) };
        if agents.is_empty() || !group::is_on(db) {
            return Ok(Value::Null);
        }
        let members = self.group_members();
        let seen = filo_local::app_data::seen_until(db, &session)?;
        let (unseen, _) = group::unseen(db, seen.unwrap_or(0.) as i64)?;
        let (last, _) = group::messages(db, 1)?;
        let last = last.first().map(|message| {
            let speaker = message.speaker.as_deref().map_or_else(|| "You".to_string(), |speaker| group::speaker_name(speaker, &members));
            json!({ "speaker": speaker, "preview": filo_local::store::preview(&message.summary(), PREVIEW_CHARS), "at": message.at as f64 })
        });
        let mut sorted = agents;
        sorted.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        let rows: Vec<Value> = sorted
            .iter()
            .filter(|agent| agent.id != DEFAULT_AGENT_ID && members.iter().any(|member| member.id == agent.id))
            .map(|agent| json!({ "id": agent.id, "name": agent.name, "picture": agent.picture, "background": agent.background }))
            .collect();
        // Who leads (the main agent's speaker id) and whether Filo is in it (while it's an agent, or leads).
        let lead = group::leader(&members).to_string();
        let filo = members.iter().any(|member| member.id == DEFAULT_AGENT_ID);
        let needs_input = self.coding_input_owners(super::now_ms()).0.contains(&session);
        // Its name and picture as the user set them (none: "Group" and the Agents tab's faces).
        let look = group::look(db)?;
        Ok(json!({
            "sessionId": session, "last": last, "unseen": unseen, "replyFailed": group::reply_failed(db)? && !self.group_failures()?.is_empty(), "needsInput": needs_input, "members": rows, "lead": lead, "filo": filo,
            "name": look.name, "picture": look.picture, "background": look.background,
        }))
    }

    /// `agents:updateGroup`: the group's name, picture and colour, from its edit page. A blank name is "Group" again, and
    /// no picture the Agents tab's faces; `null` colour is the theme's.
    fn update_group(&self, args: &Value) -> Result<Value, String> {
        let look = group::Look {
            name: arg(args, "name").map(str::to_string),
            picture: arg(args, "picture").map(str::to_string),
            background: arg(args, "background").map(str::to_string),
        };
        group::set_look(self.db()?, &look)?;
        self.changed();
        Ok(Value::Null)
    }

    /// `agents:openGroup`: the group's conversation, made the first time on, with every agent (terminal checks; the
    /// Agents tab sets it up with `agents:setUpGroup`). One turned off stays off.
    fn open_group(&self) -> Result<Value, String> {
        if self.agents().is_empty() {
            return Err("Make an agent first.".into());
        }
        let settings = group::settings(self.db()?)?;
        if settings.made && !settings.on {
            return Err("The group is off. Turn it on in the Agents tab.".into());
        }
        let session = group::ensure(self.db()?, now_ms())?;
        self.changed();
        Ok(json!(session))
    }

    /// `agents:groupSetup`: how the group is set, for its page in the Agents tab: on or not, set up before or not, its
    /// name, picture and colour, whether every agent is in it, its members by agent id, and the routing model picked.
    fn group_setup(&self) -> Result<Value, String> {
        let db = self.db()?;
        let settings = group::settings(db)?;
        let look = group::look(db)?;
        let members: Vec<String> = self.group_members().into_iter().map(|member| member.id).collect();
        Ok(json!({
            "on": settings.on && settings.made,
            "made": settings.made,
            "name": look.name,
            "picture": look.picture,
            "background": look.background,
            "all": settings.members.is_none(),
            "members": members,
            "route": settings.route,
        }))
    }

    /// `agents:setUpGroup`: the group's setup and its Group settings, one call: its name, picture and colour, who's in
    /// it (`all`, or the agent ids in `members`, at least two) and the routing model (`engine`, `model`, and its
    /// `reasoning` level). `turnOn` turns it
    /// on (the setup's Turn on). Its conversation's id.
    fn set_up_group(&self, args: &Value) -> Result<Value, String> {
        let db = self.db()?;
        let agents = self.agents();
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(true);
        let picked: Vec<String> = args
            .get("members")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| agents.iter().any(|agent| agent.id == *id))
            .map(str::to_string)
            .collect();
        let count = if all { agents.len() } else { picked.len() };
        if count < 2 {
            return Err("A group needs at least two agents.".into());
        }
        let route = match (
            arg(args, "engine").and_then(filo_local::Engine::parse),
            arg(args, "model").filter(|model| !model.trim().is_empty()),
        ) {
            (Some(engine), Some(model)) => {
                let reasoning =
                    arg(args, "reasoning").filter(|level| super::EFFORT_ORDER.contains(level)).map(str::to_string);
                Some(group::RouteModel { engine, model: model.to_string(), reasoning })
            }
            _ => None,
        };
        let now = now_ms();
        group::set_settings(db, (!all).then_some(picked.as_slice()), route.as_ref(), now)?;
        let look = group::Look {
            name: arg(args, "name").map(str::to_string),
            picture: arg(args, "picture").map(str::to_string),
            background: arg(args, "background").map(str::to_string),
        };
        group::set_look(db, &look)?;
        if args.get("turnOn").and_then(Value::as_bool).unwrap_or(false) {
            group::switch(db, true, now)?;
        }
        // Who left has nothing more to say there.
        let members = self.group_members();
        for agent in agents.iter().filter(|agent| !members.iter().any(|member| member.id == agent.id)) {
            let _ = group::drop_speaker(db, &agent.id, now);
        }
        self.group.kick();
        self.changed();
        Ok(json!(group::session(db)?))
    }

    /// `agents:turnOffGroup`: the group goes off: nothing waiting in it speaks, and the answer under way stops.
    fn turn_off_group(&self) -> Result<Value, String> {
        let db = self.db()?;
        if group::switch(db, false, now_ms())?
            && let Some(session) = group::session(db)?
        {
            self.stop_session(&session);
        }
        self.changed();
        Ok(Value::Null)
    }

    /// The coding tools installed and signed in on this Mac, in the app's order.
    fn ready_engines(&self) -> Vec<filo_local::Engine> {
        let states = self.coding_tool_states().unwrap_or_default();
        filo_local::Engine::ALL
            .into_iter()
            .filter(|engine| states.iter().any(|state| state.engine == *engine && state.installed && state.signed_in))
            .collect()
    }

    /// `agents:routingModels`: whether a coding tool is installed and signed in, and the model the routing call uses
    /// when none is picked (the first such tool's cheapest fast one).
    fn routing_models(&self) -> Value {
        let ready = self.ready_engines();
        json!({ "signedIn": !ready.is_empty(), "default": filo_local::group_route::default_model(&self.model_options(), &ready) })
    }

    /// The model the routing call uses: the one picked while its tool is still signed in and lists it, else the
    /// cheapest fast one there is.
    fn route_model(&self) -> Option<group::RouteModel> {
        let ready = self.ready_engines();
        let rows = self.model_options();
        let picked = self.db().ok().and_then(|db| group::settings(db).ok()).and_then(|settings| settings.route);
        picked
            .filter(|route| {
                ready.contains(&route.engine)
                    && rows.iter().any(|row| row.engine == route.engine && row.matches(&route.model))
            })
            .or_else(|| filo_local::group_route::default_model(&rows, &ready))
    }

    /// The level the routing call thinks at: the one picked, else Low, as the nearest `route`'s model takes (as a chat's
    /// turns get theirs, `chat_effort`). While its levels aren't known, the level as it is, except on OpenCode, where it's
    /// a variant the model has to have.
    fn route_reasoning(&self, route: &group::RouteModel) -> Option<String> {
        let level = route.reasoning.as_deref().unwrap_or(filo_local::group_route::DEFAULT_REASONING);
        let offered = self.chat_efforts(route.engine, Some(&route.model));
        if offered.is_empty() {
            return (route.engine != filo_local::Engine::OpenCode).then(|| level.to_string());
        }
        super::nearest_effort(level, &offered)
    }

    /// Routes the posts waiting for the routing call, one after another, on this thread.
    pub(super) fn run_routes(self: &Arc<Self>) {
        while let Some((thread, note)) = self.db().ok().and_then(|db| filo_local::group_route::claim(db).ok().flatten())
        {
            self.changed();
            self.run_route(&thread, note.as_deref());
            self.group.kick();
            self.changed();
        }
    }

    /// Routes the waiting posts on a thread of their own, so the post shows at once; in tests, right away.
    fn route_soon(self: &Arc<Self>) {
        if cfg!(test) {
            return self.run_routes();
        }
        let local = self.clone();
        std::thread::spawn(move || local.run_routes());
    }

    /// The routing call for the user's post in `thread`, then whoever it picked is queued (the one leading when it
    /// couldn't be asked).
    fn run_route(self: &Arc<Self>, thread: &str, note: Option<&str>) {
        let Ok(db) = self.db() else { return };
        let members = self.group_members();
        let Some(message) = group::thread_messages(db, thread)
            .ok()
            .and_then(|messages| messages.into_iter().find(|message| message.role == Role::User))
        else {
            let _ = filo_local::group_route::finish(db, thread, &[]);
            return;
        };
        let text = message.with_files(&message.text);
        let picked = self.ask_router(&message, &text, &members);
        if let Err(error) = group::routed(db, thread, &text, picked.as_deref(), note, &members, now_ms()) {
            eprintln!("group: couldn't queue who answers: {error}");
        }
    }

    /// Who the routing model picks for the user's post `message` (`text` with its files named): the members by id, or
    /// none when it can't be asked or its answer can't be read. Tests never ask a model.
    fn ask_router(&self, message: &group::GroupMessage, text: &str, members: &[Member]) -> Option<Vec<String>> {
        if cfg!(test) {
            return None;
        }
        let db = self.db().ok()?;
        let route = self.route_model()?;
        let runtime = super::runtime(self.root.join(filo_local::db::FILE));
        let program = filo_local::engine::find_program(route.engine, runtime.path_env.as_deref())?;
        let now = now_ms();
        let latest: Vec<group::GroupMessage> =
            group::latest(db).ok()?.into_iter().filter(|other| other.id != message.id).collect();
        let chats: Vec<(String, String)> = members
            .iter()
            .filter_map(|member| Some((member.id.clone(), self.speaker_agent(&member.id)?.session_id?)))
            .collect();
        let work = group::work(db, &self.agent_chats, members, &chats, "", now);
        let prompt = filo_local::group_route::prompt(
            text,
            &latest,
            members,
            &work,
            self.group_look().name.as_deref(),
            &chrono::Local,
            now,
        );
        let reasoning = self.route_reasoning(&route);
        match filo_local::group_route::call(
            route.engine,
            &program,
            &route.model,
            reasoning.as_deref(),
            &prompt,
            runtime.path_env.as_deref(),
            &self.root.join("route"),
        ) {
            Ok(answer) => {
                let picked = filo_local::group_route::parse(&answer, members);
                if picked.is_none() {
                    eprintln!(
                        "group: the routing model's answer couldn't be read: {}",
                        filo_local::store::cut(answer.trim(), 300)
                    );
                }
                picked
            }
            Err(error) => {
                eprintln!("group: {error}");
                None
            }
        }
    }

    fn group_detail(&self, session: &str, args: &Value) -> Result<Value, String> {
        let limit = super::limit(args, DEFAULT_LIMIT, usize::MAX);
        let (messages, has_more) = group::messages(self.db()?, limit)?;
        // Where it went off and on, and who joined or left, as marks among the messages loaded.
        let from = if has_more { messages.first().map_or(0, |message| message.at) } else { 0 };
        let marks = group::marks(self.db()?, from)?;
        let mut messages = self.group_rows(session, &messages)?;
        self.with_marks(&mut messages, &marks);
        let seen = self.db().and_then(|db| filo_local::app_data::seen_until(db, session)).ok().flatten();
        Ok(json!({
            "session": { "_id": session, "title": self.group_title(), "seenUntil": seen, "engine": null, "model": null },
            "messages": messages,
            "hasMore": has_more,
            // Artifacts the members' turns made, changed or opened, as finished-work cards (local_backend/artifacts.rs).
            "tasks": self.chat_tasks(session),
        }))
    }

    /// Group messages as the group shows them: with their files, what they answer, the user's reaction, a coding
    /// round's Open proof and a glance's card. An agent's own chat shows its posts this way too (local_backend.rs
    /// `detail`).
    pub(super) fn group_rows(&self, session: &str, messages: &[group::GroupMessage]) -> Result<Vec<Value>, String> {
        // A member's post reporting a coding round carries its Open proof (local_backend/coding.rs).
        let proof = self.coding_results_by_turn(
            messages
                .iter()
                .filter(|message| message.role != Role::User)
                .map(|message| (message.id.as_str(), message.turn.as_deref())),
        );
        let ids: Vec<&str> = messages.iter().map(|message| message.id.as_str()).collect();
        let (replies, reactions) = (group::replies_to(self.db()?, &ids)?, group::reactions(self.db()?, &ids)?);
        let reports = group::reports(self.db()?, &ids)?;
        let voices = group::voices(self.db()?, &ids)?;
        let mut rows: Vec<Value> = messages
            .iter()
            .map(|message| {
                let mut row = json!({
                    "_id": message.id,
                    "_creationTime": message.at as f64,
                    "role": if message.role == Role::User { "user" } else { "assistant" },
                    "content": message.text,
                    "speaker": message.speaker,
                    "attachments": super::attachments_json(&message.attachments),
                    "replyToMessageId": replies.get(&message.id),
                    "turn": message.turn,
                    "reaction": reactions.get(&message.id),
                    // A member's full report under its short post (send_message's `report`), opened as a page from it.
                    "report": reports.get(&message.id),
                });
                if let Some(result) = proof.get(&message.id) {
                    row["codingResult"] = result.clone();
                }
                // The user's voice message (local_backend/voice.rs).
                if let Some(voice) = voices.get(&message.id) {
                    row["voiceRecording"] = self.voice_json(voice);
                }
                row
            })
            .collect();
        // Glances members showed in the group, as cards above their posts (local_backend/glances.rs).
        self.with_glances(session, &mut rows);
        Ok(rows)
    }

    /// The group's marks (`group::marks`) among its rows, each where it happened: "You turned the group off", "Researcher
    /// joined".
    fn with_marks(&self, rows: &mut Vec<Value>, marks: &[group::Mark]) {
        let names = |ids: &[String]| -> Vec<String> {
            let agents = self.agents();
            ids.iter()
                .map(|id| {
                    agents
                        .iter()
                        .find(|agent| agent.id == *id)
                        .map_or_else(|| "An agent".to_string(), |agent| agent.name.clone())
                })
                .collect()
        };
        for mark in marks {
            let text = match mark.kind.as_str() {
                "off" => "You turned the group off".to_string(),
                "on" => "You turned the group on".to_string(),
                _ => {
                    let joined = names(&mark.joined).into_iter().map(|name| format!("{name} joined"));
                    let left = names(&mark.left).into_iter().map(|name| format!("{name} left"));
                    joined.chain(left).collect::<Vec<_>>().join(" · ")
                }
            };
            let row = json!({ "_id": format!("mark-{}", mark.seq), "_creationTime": mark.at as f64, "role": "mark", "content": "", "mark": text });
            let at = rows
                .iter()
                .position(|row| row["_creationTime"].as_f64().unwrap_or(0.) > mark.at as f64)
                .unwrap_or(rows.len());
            rows.insert(at, row);
        }
    }

    /// One turn per message of the user's, as the conversation counts them, pending while anyone in its thread is still
    /// to answer; and the answers that failed (`group_failures`), each under the message that asked, the user's or a
    /// member's (an ask from its chat, a member naming another), with who failed and why. Retry runs those again.
    fn group_turns(&self) -> Result<Value, String> {
        let db = self.db()?;
        let members = self.group_members();
        let (messages, _) = group::messages(db, DEFAULT_LIMIT)?;
        let mut failures = self.group_failures()?;
        // When a failure on a usage limit is tried again on its own (local_backend/limit_retry.rs): its first turn's.
        let (retried, now) = (self.limit_retried(), now_ms());
        let retry_at = |failure: &Failure| {
            failure
                .turns
                .iter()
                .filter_map(|turn| {
                    self.limit_retry_at(&retried, &turn.id, turn.error.as_deref()?, turn.created_at, now)
                })
                .min()
        };
        let mut rows = Vec::new();
        for message in messages.iter().filter(|message| message.role == Role::User) {
            let Some(thread) = &message.thread else { continue };
            let turns = group::turns_of(db, thread)?;
            let failed = failures.iter().position(|failure| failure.key == *thread).map(|index| failures.remove(index));
            // Waiting for the routing call: "Picking who answers", with the routing model's tool.
            if filo_local::group_route::routing(db, thread) {
                let engine = self.route_model().map(|route| route.engine);
                let row = json!({ "turnId": thread, "requestId": thread, "createdAt": message.at, "userMessageId": message.id, "state": "pending", "activity": "Picking who answers", "routing": engine });
                rows.push((message.at, row));
                continue;
            }
            let (state, error, retry_at) =
                if turns.iter().any(|turn| turn.state == "queued" || self.preparing_post(&turn.id, &turn.state)) {
                    ("pending", None, None)
                } else if let Some(failure) = failed {
                    ("failed", Some(failure.text(&members)), retry_at(&failure))
                } else {
                    ("completed", None, None)
                };
            let mut row = json!({ "turnId": thread, "requestId": thread, "createdAt": message.at, "userMessageId": message.id, "state": state, "error": error, "retryAt": retry_at });
            // Who's answering in their own chat, and what they're doing there: the group links to it.
            if let Some((speaker, activity)) = self.working_in_own_chat(&turns, &members) {
                row["speaker"] = json!(speaker);
                row["activity"] = json!(activity);
            }
            rows.push((message.at, row));
        }
        // The rest under a member's message (or, for a turn nobody asked for, a check-in or a coding task's news, at
        // when it was queued).
        for failure in failures {
            let at = failure.asked.as_ref().map_or(failure.turns[0].created_at, |asked| asked.at);
            let asked = failure.asked.as_ref().map(|asked| asked.id.clone());
            let row = json!({ "turnId": failure.key, "requestId": failure.key, "createdAt": at, "userMessageId": asked, "state": "failed", "error": failure.text(&members), "retryAt": retry_at(&failure) });
            rows.push((at, row));
        }
        rows.sort_by_key(|(at, _)| *at);
        let start = rows.len().saturating_sub(30);
        Ok(Value::Array(rows.split_off(start).into_iter().map(|(_, row)| row).collect()))
    }

    /// The group's failed answers it still shows (`group::failures`), by the message that asked for them: one ask
    /// naming two members who both hit a limit is one failure, with one Retry. One run in its member's own chat goes
    /// once that chat settled it (`settled_in_own_chat`).
    fn group_failures(&self) -> Result<Vec<Failure>, String> {
        let db = self.db()?;
        let mut failures: Vec<Failure> = Vec::new();
        for turn in group::failures(db)?.into_iter().filter(|turn| !self.settled_in_own_chat(turn)) {
            let asked = group::call_of(db, &turn)?;
            let same = |failure: &Failure| {
                failure.turns[0].thread == turn.thread
                    && failure.asked.as_ref().map(|asked| &asked.id) == asked.as_ref().map(|asked| &asked.id)
            };
            match failures.iter_mut().find(|failure| asked.is_some() && same(failure)) {
                Some(failure) => failure.turns.push(turn),
                None => {
                    // The user's message has its row already (its thread's); a member's is the first failed turn's.
                    let key = if asked.as_ref().is_some_and(|asked| asked.role == Role::User) {
                        turn.thread.clone()
                    } else {
                        turn.id.clone()
                    };
                    failures.push(Failure { key, asked, turns: vec![turn] });
                }
            }
        }
        Ok(failures)
    }

    /// Whether the failed turn `turn` ran in its member's own chat, and a turn there went through since
    /// (crates/filo-local `turn_state`): the failure no longer stands.
    fn settled_in_own_chat(&self, turn: &Turn) -> bool {
        let Some(own) = self.speaker_chat(&turn.speaker) else { return false };
        let messages = self.agent_chats.messages(&own).unwrap_or_default();
        let last_ok_at = self.agent_chats.chat(&own).ok().flatten().and_then(|chat| chat.last_ok_at);
        turn_state::ends(&messages, last_ok_at).iter().any(|(id, end)| *id == turn.id && matches!(end, End::Settled(_)))
    }

    /// The failed turns the group shows (`group_failures`), each on its own.
    pub(super) fn shown_group_failures(&self) -> Vec<Turn> {
        self.group_failures()
            .map(|failures| failures.into_iter().flat_map(|failure| failure.turns).collect())
            .unwrap_or_default()
    }

    /// Runs the group's failed turn `id` again, on its own (local_backend/limit_retry.rs).
    pub(super) fn retry_group_turn(&self, id: &str) -> Result<(), String> {
        group::retry_turns(self.db()?, &[id.to_string()], now_ms())?;
        self.group.kick();
        self.changed();
        Ok(())
    }

    /// The user wrote in the group: whoever they named answers; naming no one, the routing model picks who does. Sent
    /// again with the same request id, it's the message already there.
    fn group_send(self: &Arc<Self>, session: &str, args: &Value) -> Result<Value, String> {
        let thread = arg(args, "requestId")
            .filter(|id| valid_id(id))
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.group_send_as(session, &thread, arg(args, "message").unwrap_or_default(), args)
    }

    /// `group_send` for thread `thread` with `text`, typed or said (voice.rs `send_voice`), with the files, reply and
    /// tasks in `args`.
    pub(super) fn group_send_as(
        self: &Arc<Self>,
        session: &str,
        thread: &str,
        text: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let files = self.sent_files(args)?;
        // A reply (the chat's Reply) has what it answers quoted at its top.
        let reply_to = match arg(args, "replyToMessageId") {
            Some(id) => group::message(self.db()?, id)?.filter(|message| message.role != Role::Error),
            None => None,
        };
        // The tasks it points at, for the speakers.
        let note = self.message_context(args)?;
        self.group_user_message(session, thread, text, files, note.as_deref(), reply_to.as_ref())
    }

    /// The user's message in the group, with the files sent with it, starting thread `thread` (`note`: what the speakers
    /// are told besides, like the artifact page it was sent from). Sent again for the same thread, it's the message
    /// already there.
    pub(super) fn group_user_message(
        self: &Arc<Self>,
        session: &str,
        thread: &str,
        text: &str,
        files: Vec<Attachment>,
        note: Option<&str>,
        reply_to: Option<&group::GroupMessage>,
    ) -> Result<Value, String> {
        let text = text.trim();
        if text.is_empty() && files.is_empty() {
            return Err("The message is empty.".into());
        }
        let db = self.db()?;
        if !group::is_on(db) {
            return Err("The group is off.".into());
        }
        let now = now_ms();
        let (message, new) = group::add_user_reply(db, thread, text, files, reply_to, now)?;
        if new {
            let text = message.with_files(&message.text);
            let members = self.group_members();
            if group::needs_route(&text, reply_to, &members) {
                filo_local::group_route::queue(db, thread, note, now)?;
                self.changed();
                self.route_soon();
            } else {
                group::start_reply_thread(db, thread, &text, reply_to, note, &members, now)?;
            }
            self.group.kick();
        }
        self.changed();
        Ok(json!({ "turnId": thread, "sessionId": session }))
    }

    /// A check-in a member planned in the group came due: its turn there, as a thread of its own. `agent` is whose it
    /// is (none or the Filo agent: Filo).
    pub(super) fn group_check_in(&self, agent: Option<&str>, reason: &str) -> Result<(), String> {
        let speaker = agent.unwrap_or(DEFAULT_AGENT_ID);
        if !self.group_members().iter().any(|member| member.id == speaker) {
            return Err("The agent who planned it is no longer in the group.".into());
        }
        let now = now_ms();
        group::enqueue(self.db()?, &uuid::Uuid::new_v4().to_string(), speaker, 1, reason, false, now)?;
        self.group.kick();
        self.changed();
        Ok(())
    }

    /// Asks whose thread is over: the asker gets a turn in the chat it asked from, with what the group said. One whose
    /// chat is busy with a turn waits for it; one whose chat is gone is dropped.
    fn report_asks(self: &Arc<Self>) {
        for (store, chat, turn) in self.ask_reports() {
            self.start_turn(store, chat, turn);
        }
    }

    /// Agents asking each other while the group is off (crates/filo-local `agent_asks`): an ask whose turn ended goes
    /// back to the asker's chat with what it said, and the asker gets a turn there to pass it on; each ask waiting is a
    /// message in the asked agent's chat, from the asker, with its turn there (once that chat is free).
    pub(super) fn run_agent_asks(self: &Arc<Self>) {
        use filo_local::agent_asks;
        let Ok(db) = self.db() else { return };
        let now = now_ms();
        let busy = |chat: &str| self.running.lock().unwrap().values().any(|running| running.session_id == chat);
        let roster = group::roster(db);
        for ask in agent_asks::in_state(db, "running").unwrap_or_default() {
            let (Some(own), Some(turn)) = (ask.chat.as_deref(), ask.turn.as_deref()) else {
                let _ = agent_asks::finish(db, &ask.id, "failed", None, Some("It didn't start."), now);
                continue;
            };
            if self.running.lock().unwrap().contains_key(turn) {
                continue;
            }
            let messages = self.agent_chats.messages(own).unwrap_or_default();
            let last_ok_at = self.agent_chats.chat(own).ok().flatten().and_then(|chat| chat.last_ok_at);
            let error =
                turn_state::ends(&messages, last_ok_at).into_iter().find(|(id, _)| *id == turn).and_then(|(_, end)| {
                    match end {
                        End::Failed(error) => Some(error.text.clone()),
                        _ => None,
                    }
                });
            let last = messages.iter().rev().find(|message| {
                message.role == Role::Assistant && message.turn.as_deref() == Some(turn) && !message.hidden
            });
            let state = if error.is_some() { "failed" } else { "done" };
            let _ =
                agent_asks::finish(db, &ask.id, state, last.map(|message| message.id.as_str()), error.as_deref(), now);
            self.changed();
        }
        for ask in agent_asks::to_report(db).unwrap_or_default() {
            if busy(&ask.origin_chat) {
                continue;
            }
            let Some(store) = self.store_of(&ask.origin_chat).cloned() else {
                let _ = agent_asks::claim_report(db, &ask.id, now);
                continue;
            };
            if !agent_asks::claim_report(db, &ask.id, now).unwrap_or(false) {
                continue;
            }
            let name = group::speaker_name(&ask.agent, &roster);
            let replies: Vec<String> = match (ask.chat.as_deref(), ask.turn.as_deref()) {
                (Some(own), Some(turn)) => self
                    .agent_chats
                    .messages(own)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|message| {
                        message.role == Role::Assistant && message.turn.as_deref() == Some(turn) && !message.hidden
                    })
                    .map(|message| message.text)
                    .collect(),
                _ => Vec::new(),
            };
            let turn_id = uuid::Uuid::new_v4().to_string();
            let mut message = Message::new(
                Role::User,
                agent_asks::report_text(&name, &ask.text, &replies, ask.error.as_deref()),
                Some(turn_id.clone()),
            );
            message.group_link = Some(filo_local::store::GroupLink {
                kind: "answered".into(),
                who: vec![LinkedMember { speaker: ask.agent.clone(), name }],
                message_id: ask.result_id.clone().or(ask.message_id.clone()),
                agent: Some(ask.agent.clone()),
                ask: Some(ask.id.clone()),
            });
            if store.append(&ask.origin_chat, &message).is_ok() {
                self.start_turn(store, ask.origin_chat.clone(), turn_id);
            }
            self.changed();
        }
        for ask in agent_asks::in_state(db, "queued").unwrap_or_default() {
            let Some(own) = self.speaker_chat(&ask.agent) else {
                let _ = agent_asks::finish(db, &ask.id, "failed", None, Some("That agent is gone."), now);
                continue;
            };
            if busy(&own) || !agent_asks::claim(db, &ask.id, now).unwrap_or(false) {
                continue;
            }
            let name = group::speaker_name(&ask.asker, &roster);
            let turn_id = uuid::Uuid::new_v4().to_string();
            let mut message = Message::new(Role::User, agent_asks::call_text(&name, &ask.text), Some(turn_id.clone()));
            message.asked_by = Some(filo_local::store::AskedBy {
                speaker: ask.asker.clone(),
                name,
                chat: ask.origin_chat.clone(),
                message_id: ask.link_id.clone(),
                ask: ask.text.clone(),
            });
            let store = self.agent_chats.clone();
            if let Err(error) = store.append(&own, &message) {
                let _ = agent_asks::finish(
                    db,
                    &ask.id,
                    "failed",
                    None,
                    Some(&format!("Couldn't write to its chat: {error}")),
                    now,
                );
                continue;
            }
            let _ = agent_asks::started(db, &ask.id, &own, &message.id, &turn_id);
            self.start_turn(store, own, turn_id);
            self.changed();
        }
    }

    /// `report_asks`' messages, each saved in the chat it asked from: the turns to start there.
    fn ask_reports(&self) -> Vec<(filo_local::Store, String, String)> {
        let Ok(db) = self.db() else { return Vec::new() };
        let mut turns = Vec::new();
        for ask in group::asks_answered(db).unwrap_or_default() {
            let busy = self.running.lock().unwrap().values().any(|running| running.session_id == ask.origin_chat);
            if busy || !group::claim_report(db, &ask.thread, now_ms()).unwrap_or(false) {
                continue;
            }
            let Some(store) = self.store_of(&ask.origin_chat).cloned() else { continue };
            let messages = group::thread_messages(db, &ask.thread).unwrap_or_default();
            let text = group::report_message(
                &messages,
                &self.group_members(),
                self.group_look().name.as_deref(),
                &chrono::Local,
                now_ms(),
            );
            let turn_id = uuid::Uuid::new_v4().to_string();
            if store.add_user_message(&ask.origin_chat, &text, &turn_id).is_ok() {
                turns.push((store, ask.origin_chat.clone(), turn_id));
            }
            self.changed();
        }
        turns
    }

    /// Retry: the failed answers the group shows under one message run again (`turnId` is that failure's row, from
    /// `group_turns`); a member's own chat retries its group turn by the turn's id. An older row's thread: its
    /// failed answers.
    fn group_retry(&self, args: &Value) -> Result<Value, String> {
        let id = required(args, "turnId")?;
        let db = self.db()?;
        let now = now_ms();
        let failure = self.group_failures()?.into_iter().find(|failure| failure.key == id);
        if let Some(failure) = failure {
            group::retry_turns(db, &failure.turns.into_iter().map(|turn| turn.id).collect::<Vec<_>>(), now)?;
        } else if group::turn(db, id)?.is_some() {
            group::retry_turns(db, &[id.to_string()], now)?;
        } else {
            group::retry(db, id, now)?;
        }
        self.group.kick();
        self.changed();
        Ok(json!({ "turnId": id }))
    }

    /// Stop: the answer under way ends and nobody else in the queue speaks.
    fn stop_group(&self, session: &str) {
        if let Ok(db) = self.db() {
            let _ = group::stop_queued(db, now_ms());
        }
        self.stop_session(session);
        self.changed();
    }

    /// A removed agent's waiting turns in the group go with it, and its asks.
    pub(super) fn group_agent_removed(&self, id: &str) {
        if let Ok(db) = self.db() {
            let _ = group::drop_speaker(db, id, now_ms());
            let _ = filo_local::agent_asks::drop_agent(db, id, now_ms());
        }
    }

    /// Quitting: the runner takes no more turns (the one under way is stopped with the others).
    pub(super) fn halt_group(&self) {
        self.group.halted.store(true, Ordering::SeqCst);
        self.group.kick();
    }

    /// The group's rows (`group_rows`) for the posts `messages` link to (`in_group`), by the post's id.
    pub(super) fn posts_in_group(&self, messages: &[Message]) -> std::collections::HashMap<String, Value> {
        let (Some(session), Ok(db)) = (self.group_session(), self.db()) else { return Default::default() };
        let posts: Vec<group::GroupMessage> = messages
            .iter()
            .filter_map(|message| group::message(db, message.in_group.as_deref()?).ok().flatten())
            .collect();
        if posts.is_empty() {
            return Default::default();
        }
        let rows = self.group_rows(&session, &posts).unwrap_or_default();
        rows.into_iter().filter_map(|row| Some((row["_id"].as_str()?.to_string(), row))).collect()
    }

    /// The time of group message `id`, for `markSeen`.
    pub(super) fn group_message_at(&self, id: &str) -> Option<f64> {
        group::message(self.db().ok()?, id).ok().flatten().map(|message| message.at as f64)
    }

    /// The group's row in `voiceSessions:unseen`, when it has replies not seen yet.
    pub(super) fn group_unseen(&self, seen: &std::collections::HashMap<String, f64>) -> Option<Value> {
        let session = self.group_session().filter(|_| self.db().is_ok_and(group::is_on))?;
        let seen_until = seen.get(&session).copied().unwrap_or(0.);
        let (count, latest) = group::unseen(self.db().ok()?, seen_until as i64).ok()?;
        let latest = latest?;
        let mut row =
            super::unseen_row(&session, &self.group_title(), count, seen_until, latest.at as f64, &latest.summary());
        row["agentGroup"] = json!(true);
        Some(row)
    }

    /// The group's row in `voiceSessions:working`, while someone in it is answering.
    pub(super) fn group_working(&self, session: &str, started: f64) -> Option<Value> {
        (self.group_session().as_deref() == Some(session)).then(
            || json!({ "_id": session, "title": self.group_title(), "workStartedAt": started, "agentGroup": true }),
        )
    }

    /// Whether the group's turn `turn_id` (in `state`) is running with nothing posted yet. Once its member has posted,
    /// whatever its turn does after (watching a build it started) is work in its own chat, not the group's.
    fn preparing_post(&self, turn_id: &str, state: &str) -> bool {
        state == "running"
            && self
                .db()
                .ok()
                .and_then(|db| group::replies_of(db, turn_id).ok())
                .is_some_and(|replies| replies.is_empty())
    }

    /// Whether the group's entry `key` in `Local::running` (a turn's, or the one `in_group_key` makes for a turn in its
    /// member's chat) is a turn that posted already: the group isn't working on it any more.
    pub(super) fn posted_in_group(&self, key: &str) -> bool {
        let turn_id = key.strip_prefix(IN_GROUP).unwrap_or(key);
        self.db().ok().and_then(|db| group::replies_of(db, turn_id).ok()).is_some_and(|replies| !replies.is_empty())
    }

    /// The turn just claimed, and with it the turns of the same message asking several (`group::claim_alongside`), each
    /// on a thread of its own, so they all start at the same time. Back once they all ended.
    fn run_group_turns(self: &Arc<Self>, turn: Turn) {
        let alongside =
            self.db().ok().and_then(|db| group::claim_alongside(db, &turn, now_ms()).ok()).unwrap_or_default();
        let running: Vec<_> = alongside
            .into_iter()
            .map(|turn| {
                let local = self.clone();
                std::thread::spawn(move || local.run_group_turn(turn))
            })
            .collect();
        self.run_group_turn(turn);
        for thread in running {
            let _ = thread.join();
        }
    }

    /// One member's turn, start to end: it posts in the group, then whoever its posts asked is queued.
    fn run_group_turn(self: &Arc<Self>, turn: Turn) {
        let Ok(db) = self.db() else { return };
        let members = self.group_members();
        if !members.iter().any(|member| member.id == turn.speaker) {
            let _ = group::finish(db, &turn.id, "stopped", None, now_ms());
            return;
        }
        match self.speaker_chat(&turn.speaker) {
            Some(own) => self.run_in_own_chat(turn, &own, &members),
            None => self.run_fresh(turn, &members),
        }
    }

    /// The turn in the member's own chat: the group's message there, then the chat's turn for it, which the group
    /// shows as its own until it ends.
    fn run_in_own_chat(self: &Arc<Self>, turn: Turn, own: &str, members: &[Member]) {
        let Ok(db) = self.db() else { return };
        let Some(session) = self.group_session() else { return };
        let store = self.agent_chats.clone();
        if let Err(error) = self.add_group_call(&store, own, &turn, members) {
            let _ = group::finish(db, &turn.id, "failed", Some(&error), now_ms());
            self.changed();
            return;
        }
        let stopped = self.run_own_chat_turn(&store, own, &session, &turn.id);
        self.end_own_chat_turn(&turn, own, members, stopped);
    }

    /// The group's message starting `turn` in the member's chat `own`: what was asked and by whom, and the group's
    /// latest messages, which the engine reads with how to answer there. A turn nobody asked for in the group (a coding
    /// task's news, a check-in) is the engine's only; one that only checks the message (`group::checking`) shows once it
    /// posted or failed (store.rs `shown`). The files the user's message in its thread came with go with it, so the turn
    /// hands them to the engine as the chat's own message's would be (`group::thread_files`). Run again (Retry), the
    /// message is there already.
    pub(super) fn add_group_call(
        &self,
        store: &Store,
        own: &str,
        turn: &Turn,
        members: &[Member],
    ) -> Result<(), String> {
        let messages = store.messages(own).map_err(|error| format!("Couldn't read the agent's chat: {error}"))?;
        if messages
            .iter()
            .any(|message| message.role == Role::User && message.turn.as_deref() == Some(turn.id.as_str()))
        {
            return Ok(());
        }
        let db = self.db()?;
        let now = now_ms();
        let latest = group::latest(db)?;
        let asked = group::call_of(db, turn)?;
        let checking = group::checking(turn, asked.as_ref());
        let work = self.group_work(&turn.speaker, members, now);
        let mut message = Message::new(
            Role::User,
            group::own_chat_prompt(
                turn,
                members,
                &work,
                &latest,
                group::look(db)?.name.as_deref(),
                &chrono::Local,
                now,
            ),
            Some(turn.id.clone()),
        );
        message.hidden = asked.is_none();
        // A coding task's news: its replies answer what asked the task for work, the latest ask this chat shows.
        if asked.is_none() {
            use filo_local::coding_handoff::{asks_reported, wake};
            let news = turn.thread.strip_prefix(super::handoff::NEWS_THREAD).and_then(|id| wake(db, id).ok().flatten());
            message.reply_to = news
                .and_then(|news| asks_reported(db, &[news]).ok().flatten())
                .and_then(|asks| store.shown_ask(own, &asks));
        }
        message.attachments = group::thread_files(db, &turn.thread)?;
        message.group_call = Some(GroupCall {
            speaker: turn.speaker.clone(),
            from: asked
                .as_ref()
                .and_then(|asked| asked.speaker.clone())
                .map(|speaker| LinkedMember { name: group::speaker_name(&speaker, members), speaker }),
            message_id: asked.as_ref().map(|asked| asked.id.clone()),
            ask: asked.map_or_else(|| turn.reason.clone(), |asked| asked.summary()),
            context: group::transcript(&latest, members, &chrono::Local, now),
            answers_user: turn.answers_user,
            after_question: turn.passes_on.is_some(),
            checking,
        });
        store.append(own, &message).map_err(|error| format!("Couldn't write to the agent's chat: {error}"))?;
        self.changed();
        Ok(())
    }

    /// Runs the chat's turn `turn_id` (in a worker, as the chat's own turns run) and waits for it. It counts as running
    /// in the chat and in the group, so both show it working and either's Stop reaches it. Whether it was stopped.
    fn run_own_chat_turn(self: &Arc<Self>, store: &Store, own: &str, session: &str, turn_id: &str) -> bool {
        self.set_turn_model(store, own);
        let detached = self.detaches();
        let running =
            Running { session_id: own.to_string(), started_at: now_ms() as f64, detached, ..Running::default() };
        let stop = running.stop.clone();
        let in_group =
            Running { session_id: session.to_string(), started_at: running.started_at, stop: stop.clone(), detached };
        {
            let mut map = self.running.lock().unwrap();
            map.insert(turn_id.to_string(), running);
            map.insert(in_group_key(turn_id), in_group);
        }
        self.watch_turns();
        self.changed();
        match self.start_worker(Job::Turn { session: own.to_string(), turn: turn_id.to_string() }, &stop) {
            Some(worker) => worker.wait(),
            None => {
                self.run_turn_here(store, own, turn_id, &stop);
            }
        }
        {
            let mut map = self.running.lock().unwrap();
            map.remove(turn_id);
            map.remove(&in_group_key(turn_id));
        }
        self.changed();
        stop.requested()
    }

    /// How the chat's turn went is the group turn's: failed with the chat's error, else done (`end_group_turn`). A
    /// retry that went through with nothing to post leaves the earlier error last in the chat: it's done
    /// (crates/filo-local `turn_state`).
    fn end_own_chat_turn(&self, turn: &Turn, own: &str, members: &[Member], stopped: bool) {
        let Ok(db) = self.db() else { return };
        let messages = self.agent_chats.messages(own).unwrap_or_default();
        let last_ok_at = self.agent_chats.chat(own).ok().flatten().and_then(|chat| chat.last_ok_at);
        let error =
            turn_state::ends(&messages, last_ok_at).into_iter().find(|(id, _)| *id == turn.id).and_then(|(_, end)| {
                match end {
                    End::Failed(error) => Some(error.text.clone()),
                    _ => None,
                }
            });
        let replies = group::replies_of(db, &turn.id).map(|replies| replies.len()).unwrap_or(0);
        self.end_group_turn(turn, members, stopped, error.as_deref(), replies);
    }

    /// A group turn ended: stopped, failed with `error`, or done, having posted `replies` messages. Then whoever its
    /// posts asked is next.
    fn end_group_turn(&self, turn: &Turn, members: &[Member], stopped: bool, error: Option<&str>, replies: usize) {
        let Ok(db) = self.db() else { return };
        let now = now_ms();
        let state = if stopped {
            "stopped"
        } else if error.is_some() {
            "failed"
        } else {
            "completed"
        };
        let _ = group::finish(db, &turn.id, state, error, now);
        if !stopped && replies > 0 {
            let _ = group::follow(db, turn, members, now);
        }
        self.changed();
    }

    /// A group turn whose worker carried on while the app was closed: working in the group until it ends, then ended
    /// as `run_in_own_chat` would have.
    fn follow_carried_on(self: &Arc<Self>, worker: workers::Worker) {
        let Job::Turn { session: own, turn: turn_id } = worker.job.clone() else { return };
        let Some(session) = self.group_session() else { return };
        let in_group = Running {
            session_id: session,
            started_at: worker.started_at,
            stop: Stoppable::of(worker.stopper()),
            detached: true,
        };
        self.running.lock().unwrap().insert(in_group_key(&turn_id), in_group);
        self.watch_turns();
        let local = self.clone();
        std::thread::spawn(move || {
            worker.wait();
            let stopped = local
                .running
                .lock()
                .unwrap()
                .remove(&in_group_key(&turn_id))
                .is_some_and(|running| running.stop.requested());
            if let Some(turn) = local.db().ok().and_then(|db| group::turn(db, &turn_id).ok().flatten()) {
                local.end_own_chat_turn(&turn, &own, &local.group_members(), stopped);
            }
        });
    }

    /// Whether `turn_id` is one of the group's turns.
    fn is_group_turn(&self, turn_id: &str) -> bool {
        self.db().ok().is_some_and(|db| group::turn(db, turn_id).ok().flatten().is_some())
    }

    /// A thread's turns (`turns`) running in their members' own chats with nothing posted yet: the first one's member, and
    /// what the group says it's doing ("Chef is working in its chat", or what its chat shows it doing). One checking a
    /// message to everyone, or the user's right after its message, is "Chef is checking"; several at once (an
    /// "@everyone") are "Chef, Coach and Filo are checking".
    fn working_in_own_chat(&self, turns: &[Turn], members: &[Member]) -> Option<(String, String)> {
        let preparing: Vec<&Turn> = turns.iter().filter(|turn| self.preparing_post(&turn.id, &turn.state)).collect();
        let turn = *preparing.first()?;
        let own = self.speaker_agent(&turn.speaker)?.session_id?;
        let name = group::speaker_name(&turn.speaker, members);
        let db = self.db().ok()?;
        let checking = |turn: &Turn| group::checking(turn, group::call_of(db, turn).ok().flatten().as_ref());
        if preparing.len() > 1 {
            let names: Vec<String> = preparing.iter().map(|turn| group::speaker_name(&turn.speaker, members)).collect();
            let doing =
                if preparing.iter().all(|turn| checking(turn)) { "are checking" } else { "are working in their chats" };
            return Some((turn.speaker.clone(), format!("{} {doing}", few_names(&names))));
        }
        let doing = chat_activity::get(&self.agent_chats, &own, &turn.id);
        let doing = match (checking(turn), doing) {
            (false, None) => format!("{name} is working in its chat"),
            (false, Some(doing)) => format!("{name}: {doing}"),
            (true, None) => format!("{name} is checking"),
            (true, Some(doing)) => format!("{name} is checking: {doing}"),
        };
        Some((turn.speaker.clone(), doing))
    }

    /// What the members are busy with outside the group, for `speaker`'s turn when it leads (`group::work`): their coding
    /// tasks and what the user last wrote to them in their own chats. Nothing for the others.
    fn group_work(&self, speaker: &str, members: &[Member], now: i64) -> group::Work {
        let Ok(db) = self.db() else { return group::Work::new() };
        if speaker != group::leader(members) || members.len() < 2 {
            return group::Work::new();
        }
        let chats: Vec<(String, String)> = members
            .iter()
            .filter_map(|member| Some((member.id.clone(), self.speaker_agent(&member.id)?.session_id?)))
            .collect();
        group::work(db, &self.agent_chats, members, &chats, speaker, now)
    }

    /// A turn of Filo's while it has no chat of its own (the Filo agent removed): its CLI on a fresh session that reads
    /// the group, posting there.
    fn run_fresh(self: &Arc<Self>, turn: Turn, members: &[Member]) {
        let Ok(db) = self.db() else { return };
        let Some(session) = self.group_session() else { return };
        let Some(member) = members.iter().find(|member| member.id == turn.speaker).cloned() else { return };
        let db_path = self.root.join(filo_local::db::FILE);
        // The engine and model picked for the speaker (Filo without its agent: the app's default).
        let (engine, model) = self
            .speaker_agent(&turn.speaker)
            .map_or_else(|| (default_engine(), None), |agent| self.agent_model(&agent));
        let now = now_ms();
        let memories = memories::all(db).map(|all| memories::prompt_block(&all)).unwrap_or_default();
        let work = self.group_work(&turn.speaker, members, now);
        let prompt = match group::setup_prompt(&turn, members, &work, db, &memories, now) {
            Ok(prompt) => prompt,
            Err(error) => {
                let _ = group::finish(db, &turn.id, "failed", Some(&error), now);
                self.changed();
                return;
            }
        };
        let setup = Setup {
            engine,
            model,
            instructions: group::instructions(&member, members),
            prompt,
            workspace: self.agent_chats.root().join("workspace"),
            chat_id: session.clone(),
            store_root: self.agent_chats.root().to_path_buf(),
        };

        let running = Running { session_id: session, started_at: now_ms() as f64, ..Running::default() };
        let stop = running.stop.clone();
        // The Mac stays awake while it runs (local_backend/awake.rs).
        let _awake = self.keep_awake("reply: the agents' group".into());
        self.running.lock().unwrap().insert(turn.id.clone(), running);
        self.watch_turns();
        self.changed();
        let report =
            group::run(db, &db_path, &super::runtime(db_path.clone()), &setup, &turn, |started| stop.started(started));
        self.running.lock().unwrap().remove(&turn.id);
        self.end_group_turn(&turn, members, report.stopped, report.error.as_deref(), report.replies);
    }
}

/// Answers that failed in the group, under the message that asked for them (none: a turn nobody asked for).
struct Failure {
    /// Its row's turn id in `group_turns`, which Retry sends back: the thread, for the user's message; else the first
    /// failed turn's id.
    key: String,
    asked: Option<group::GroupMessage>,
    turns: Vec<Turn>,
}

impl Failure {
    /// Who couldn't answer and why, in a line each reason: "App Designer couldn't reply: Claude Code: You've hit your
    /// session limit · resets 2:20am". The reason is the error's first paragraph (what follows is the chat's advice).
    fn text(&self, members: &[Member]) -> String {
        let mut reasons: Vec<(&str, Vec<String>)> = Vec::new();
        for turn in &self.turns {
            let reason = turn
                .error
                .as_deref()
                .and_then(|error| error.trim().split("\n\n").next())
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .unwrap_or("it stopped without an answer.");
            let name = group::speaker_name(&turn.speaker, members);
            match reasons.iter_mut().find(|(known, _)| *known == reason) {
                Some((_, names)) if !names.contains(&name) => names.push(name),
                Some(_) => {}
                None => reasons.push((reason, vec![name])),
            }
        }
        reasons
            .iter()
            .map(|(reason, names)| format!("{} couldn't reply: {reason}", names_list(names)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// "Chef", "Chef and Filo", "Chef, Filo and Coach".
fn names_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// "Chef, Filo and Coach", or "Chef, Filo, Coach and 17 others" for more than four.
fn few_names(names: &[String]) -> String {
    match names.len() {
        0..=4 => names_list(names),
        count => format!("{} and {} others", names[..3].join(", "), count - 3),
    }
}

/// The key of a turn's entry for the group in `Local::running`, beside its own chat's (the turn's id).
fn in_group_key(turn_id: &str) -> String {
    format!("{IN_GROUP}{turn_id}")
}

/// What `in_group_key` puts before the turn's id.
const IN_GROUP: &str = "group-";

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("group")
    }

    #[test]
    fn the_group_opens_keeps_its_messages_and_routes_named_agents() {
        let local = temp();
        // Fresh data has the Filo agent: the group can be made, with Filo leading and no other member yet.
        assert_eq!(local.call("agents:group", &json!({})).unwrap(), Value::Null);
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        assert_eq!(local.call("agents:openGroup", &json!({})).unwrap(), json!(session));
        let row = local.call("agents:group", &json!({})).unwrap();
        assert_eq!(
            (row["sessionId"].as_str(), row["members"].as_array().map(Vec::len)),
            (Some(session.as_str()), Some(0))
        );

        let chef = local.call("agents:create", &json!({ "name": "Chef", "instructions": "Cooks.", "picture": "mint" }));
        // Without a coding tool plugged in, a test Mac may refuse; the group is checked with the agent added directly.
        let chef = match chef {
            Ok(id) => id.as_str().unwrap().to_string(),
            Err(_) => {
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
                "chef-id".into()
            }
        };
        let row = local.call("agents:group", &json!({})).unwrap();
        assert_eq!(row["members"][0]["name"], "Chef");

        // The user names Chef: the message is kept, Chef's turn waits (nothing runs it in a test), the turn is pending.
        let sent = local
            .call(
                "voiceSessions:sendMessage",
                &json!({ "sessionId": session, "message": "@Chef dinner?", "requestId": "req-1" }),
            )
            .unwrap();
        assert_eq!(sent, json!({ "turnId": "req-1", "sessionId": session }));
        local
            .call(
                "voiceSessions:sendMessage",
                &json!({ "sessionId": session, "message": "@Chef dinner?", "requestId": "req-1" }),
            )
            .unwrap();
        let db = local.db().unwrap();
        let waiting = group::active(db).unwrap();
        assert_eq!(waiting.iter().map(|turn| turn.speaker.as_str()).collect::<Vec<_>>(), [chef.as_str()]);
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!((turns[0]["requestId"].as_str(), turns[0]["state"].as_str()), (Some("req-1"), Some("pending")));

        // Chef's turn is a message of the group's in Chef's own chat, starting the chat's turn of the same id: the user's
        // ask, and the group's latest messages. Its MCP server posts in the group, leaving a line in the chat.
        let turn = group::claim_next(db, 1).unwrap().unwrap();
        let members = local.group_members();
        let chef_chat = local.speaker_chat(&chef).unwrap();
        local.add_group_call(&local.agent_chats, &chef_chat, &turn, &members).unwrap();
        local.add_group_call(&local.agent_chats, &chef_chat, &turn, &members).unwrap();
        let called = local.agent_chats.messages(&chef_chat).unwrap();
        assert_eq!(called.len(), 1);
        let call = called[0].group_call.clone().unwrap();
        assert_eq!(
            (called[0].role, called[0].turn.as_deref(), called[0].hidden),
            (Role::User, Some(turn.id.as_str()), false)
        );
        assert_eq!(
            (call.speaker.as_str(), call.from, call.ask.as_str(), call.answers_user),
            (chef.as_str(), None, "@Chef dinner?", true)
        );
        assert!(call.context.ends_with("User: @Chef dinner?"), "{}", call.context);
        assert!(
            called[0].text.contains("send_message posts in the group")
                && called[0].text.contains("Why you're speaking now: The user wrote to you"),
            "{}",
            called[0].text
        );
        // While Chef's chat runs the turn, the group works on it until Chef posts; after, only Chef's chat does (its
        // turn carrying on, watching a build), and the thread counts as answered.
        local
            .running
            .lock()
            .unwrap()
            .insert(turn.id.clone(), Running { session_id: chef_chat.clone(), ..Running::default() });
        local
            .running
            .lock()
            .unwrap()
            .insert(in_group_key(&turn.id), Running { session_id: session.clone(), ..Running::default() });
        let working = || {
            let rows = local.call("voiceSessions:working", &json!({})).unwrap();
            rows.as_array()
                .unwrap()
                .iter()
                .map(|row| row["_id"].as_str().unwrap().to_string())
                .collect::<std::collections::HashSet<_>>()
        };
        let thread = || local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap()[0].clone();
        assert_eq!(working(), [session.clone(), chef_chat.clone()].into());
        assert_eq!((thread()["state"].as_str(), thread()["speaker"].as_str()), (Some("pending"), Some(chef.as_str())));
        let posted = group::post(
            db,
            &chef,
            "Pasta. @Filo can you shop?",
            group::asks(&["Filo"], &chef, &members).unwrap(),
            Some(&turn.id),
            now_ms(),
        )
        .unwrap();
        let mut line = Message::new(Role::Assistant, posted.text.clone(), Some(turn.id.clone()));
        line.in_group = Some(posted.id.clone());
        local.agent_chats.append(&chef_chat, &line).unwrap();
        assert_eq!(working(), [chef_chat.clone()].into());
        assert_eq!((thread()["state"].as_str(), thread()["speaker"].as_str()), (Some("completed"), None));
        local.running.lock().unwrap().clear();
        // The chat's turn ended well: so did the group's, and Chef asked Filo, who is next.
        local.end_own_chat_turn(&turn, &chef_chat, &members, false);
        assert_eq!(group::turns_of(db, "req-1").unwrap()[0].state, "completed");
        assert_eq!(group::active(db).unwrap()[0].speaker, DEFAULT_AGENT_ID);
        // Chef's chat shows the ask as the group's, and its post in full, labelled as posted there: unseen in the group
        // only.
        let detail = local.call("voiceSessions:detail", &json!({ "id": chef_chat })).unwrap();
        assert_eq!(
            (detail["messages"][0]["content"].as_str(), detail["messages"][0]["groupCall"]["answersUser"].as_bool()),
            (Some("@Chef dinner?"), Some(true))
        );
        assert_eq!(detail["messages"][1]["groupLink"], json!({ "kind": "posted", "who": [], "messageId": posted.id }));
        assert_eq!(detail["messages"][1]["content"], "Pasta. @Filo can you shop?");
        let unread = |agent: &str| {
            let rows = local.call("agents:list", &json!({})).unwrap();
            rows.as_array().unwrap().iter().find(|row| row["_id"] == json!(agent)).unwrap()["unseen"].clone()
        };
        assert_eq!(unread(&chef), json!(0));
        let list = local.call("agents:list", &json!({})).unwrap();
        let chef_row = list.as_array().unwrap().iter().find(|row| row["_id"] == json!(chef)).unwrap().clone();
        assert_eq!(chef_row["last"]["fromUser"], false);

        // Filo's turn in its chat is Chef's ask, which the user isn't waiting on.
        let filo_turn = group::claim_next(db, 3).unwrap().unwrap();
        let filo_chat = local.speaker_chat(DEFAULT_AGENT_ID).unwrap();
        local.add_group_call(&local.agent_chats, &filo_chat, &filo_turn, &members).unwrap();
        let call = local.agent_chats.messages(&filo_chat).unwrap().pop().unwrap().group_call.unwrap();
        assert_eq!(
            (call.from.map(|from| from.name), call.message_id, call.answers_user),
            (Some("Chef".to_string()), Some(posted.id.clone()), false)
        );
        // While it runs the group says who works on it, where; the error its chat ends with is the group turn's.
        let working = local.working_in_own_chat(std::slice::from_ref(&filo_turn), &members).unwrap();
        assert_eq!(working, (DEFAULT_AGENT_ID.to_string(), "Filo is working in its chat".to_string()));
        local
            .agent_chats
            .append(
                &filo_chat,
                &Message::new(Role::Error, "Claude Code finished without replying.", Some(filo_turn.id.clone())),
            )
            .unwrap();
        local.end_own_chat_turn(&filo_turn, &filo_chat, &members, false);
        let ended = group::turn(db, &filo_turn.id).unwrap().unwrap();
        assert_eq!(
            (ended.state.as_str(), ended.error.as_deref()),
            ("failed", Some("Claude Code finished without replying."))
        );
        group::retry(db, "req-1", 4).unwrap();

        // The conversation reads the group with who said what; unseen and working rows say it's the group.
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let unseen = local.call("voiceSessions:unseen", &json!({})).unwrap();
        assert!(!unseen["chats"].as_array().unwrap().iter().any(|chat| chat["sessionId"] == json!(chef_chat)));
        let said: Vec<(Value, Value)> =
            detail["messages"].as_array().unwrap().iter().map(|m| (m["role"].clone(), m["speaker"].clone())).collect();
        assert_eq!(said, [(json!("user"), Value::Null), (json!("assistant"), json!(chef))]);
        assert_eq!(detail["session"]["title"], "Group");
        // Named, the group is its name; a blank name is "Group" again.
        let named = json!({ "name": " Team ", "picture": "helper:mint", "background": "#4FB3B8" });
        local.call("agents:updateGroup", &named).unwrap();
        let row = local.call("agents:group", &json!({})).unwrap();
        assert_eq!(
            (&row["name"], &row["picture"], &row["background"]),
            (&json!("Team"), &json!("helper:mint"), &json!("#4FB3B8"))
        );
        assert_eq!(local.call("voiceSessions:detail", &json!({ "id": session })).unwrap()["session"]["title"], "Team");
        local.call("agents:updateGroup", &json!({ "name": "", "picture": null, "background": null })).unwrap();
        let row = local.call("agents:group", &json!({})).unwrap();
        assert_eq!((&row["name"], &row["picture"]), (&Value::Null, &Value::Null));
        assert_eq!(local.call("voiceSessions:detail", &json!({ "id": session })).unwrap()["session"]["title"], "Group");
        let unseen = local.call("voiceSessions:unseen", &json!({})).unwrap();
        assert!(unseen["chats"].as_array().unwrap().iter().any(|chat| chat["sessionId"] == json!(session)
            && chat["agentGroup"] == json!(true)
            && chat["count"] == json!(1)));
        local.call("voiceSessions:markSeen", &json!({ "sessionId": session })).unwrap();
        assert_eq!(local.call("agents:group", &json!({})).unwrap()["unseen"], 0);

        // Stop drops Filo's waiting turn; the thread counts as answered.
        local.call("voiceSessions:stopChatTurns", &json!({ "sessionId": session })).unwrap();
        assert!(group::active(db).unwrap().is_empty());
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!(turns[0]["state"], "completed");

        // A restarted app reads the same group.
        let reopened = Local::open(local.root.clone());
        let again = reopened.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        assert_eq!(again["messages"], detail["messages"]);
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// An agent added straight to the database (`agents:create` needs a coding tool plugged in, which a test Mac may not
    /// have), with its conversation.
    fn add_agent(local: &Arc<Local>, id: &str, name: &str) -> String {
        let agent = filo_local::app_data::Agent {
            id: id.into(),
            name: name.into(),
            instructions: "Cooks.".into(),
            picture: "mint".into(),
            background: None,
            session_id: None,
            engine: None,
            model: None,
            earlier_chat: None,
            created_at: now_ms() as f64,
        };
        filo_local::app_data::add_agent(local.db().unwrap(), &agent).unwrap();
        local.open_agent_chat(id).unwrap().as_str().unwrap().to_string()
    }

    #[test]
    fn files_sent_in_the_group_show_stay_and_go_to_whoever_answers() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let picture = local.save_attachment("image/png", b"png").unwrap();
        let saved = filo_local::attachments::file(&local.root, &picture).unwrap();
        let files = json!([{ "storageId": picture, "name": "shot.png", "contentType": "image/png", "localPath": "/Users/me/shot.png" }]);
        let args = json!({ "sessionId": session, "message": "@Chef look", "requestId": "req-1", "attachments": files });
        // Sent, and sent again as Retry does: one message, with its picture.
        local.call("voiceSessions:sendMessage", &args).unwrap();
        local.call("voiceSessions:sendMessage", &args).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
        let shown = &detail["messages"][0]["attachments"][0];
        assert_eq!(
            (shown["name"].as_str(), shown["contentType"].as_str(), shown["localFile"].as_str()),
            (Some("shot.png"), Some("image/png"), saved.to_str())
        );

        // Chef answers in its own chat: the group's message there has the picture, which its turn hands the engine as
        // a picture of the chat's own, and the group's latest messages say where it's saved.
        let turn = group::claim_next(db, 1).unwrap().unwrap();
        local.add_group_call(&local.agent_chats, &chef_chat, &turn, &local.group_members()).unwrap();
        let messages = local.agent_chats.messages(&chef_chat).unwrap();
        assert_eq!(messages[0].attachments.iter().map(|file| file.file.clone()).collect::<Vec<_>>(), [saved.clone()]);
        let prompt = filo_local::turn::prompt_for(&messages, &turn.id, true, None);
        assert!(prompt.contains(&format!("shot.png (image/png, 3 B), saved at {} (the user's own file: /Users/me/shot.png): given to you with this message as an image", saved.display())), "{prompt}");
        let context = &messages[0].group_call.as_ref().unwrap().context;
        assert!(
            context.ends_with(&format!(
                "User: @Chef look\n[Attached: shot.png (image/png), saved at {}]",
                saved.display()
            )),
            "{context}"
        );
        group::finish(db, &turn.id, "completed", None, 2).unwrap();

        // A picture alone goes too; the Agents tab's row names it.
        let alone = json!({ "sessionId": session, "message": "", "requestId": "req-2", "attachments": files });
        local.call("voiceSessions:sendMessage", &alone).unwrap();
        assert_eq!(local.call("agents:group", &json!({})).unwrap()["last"]["preview"], "shot.png");
        // A file no longer on this Mac doesn't.
        let gone = json!({ "sessionId": session, "message": "here", "requestId": "req-3", "attachments": [{ "storageId": "../profile.json" }] });
        assert!(local.call("voiceSessions:sendMessage", &gone).is_err());

        // A restarted app shows them still.
        let reopened = Local::open(local.root.clone());
        let again = reopened.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let names: Vec<&Value> =
            again["messages"].as_array().unwrap().iter().map(|message| &message["attachments"][0]["name"]).collect();
        assert_eq!(names, [&json!("shot.png"), &json!("shot.png")]);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn an_agents_chat_shows_its_post_as_the_group_has_it() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        local.call("agents:openGroup", &json!({})).unwrap();
        let db = local.db().unwrap();
        let posted = group::post(db, "chef-id", "Pasta tonight.", group::Asks::default(), None, now_ms()).unwrap();
        // A line whose words drifted from its post shows the post's; one whose post is gone keeps its own.
        let mut drifted = Message::new(Role::Assistant, "Old words", None);
        drifted.in_group = Some(posted.id.clone());
        local.agent_chats.append(&chef_chat, &drifted).unwrap();
        let mut gone = Message::new(Role::Assistant, "A post no longer there", None);
        gone.in_group = Some("gone".into());
        local.agent_chats.append(&chef_chat, &gone).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": chef_chat })).unwrap();
        let shown = |index: usize, key: &str| detail["messages"][index][key].clone();
        assert_eq!(
            (shown(0, "_id"), shown(0, "content"), shown(0, "groupLink")["messageId"].clone()),
            (json!(drifted.id), json!("Pasta tonight."), json!(posted.id))
        );
        assert_eq!(
            (shown(1, "content"), shown(1, "groupLink")["kind"].clone()),
            (json!("A post no longer there"), json!("posted"))
        );
        // Neither counts as unseen in Chef's chat: the group's notice is the only one.
        let unseen = local.call("voiceSessions:unseen", &json!({})).unwrap();
        assert!(
            unseen["chats"].as_array().unwrap().iter().all(|chat| chat["sessionId"] != json!(chef_chat)),
            "{unseen}"
        );
        assert_eq!(
            unseen["chats"].as_array().unwrap().iter().filter(|chat| chat["agentGroup"] == json!(true)).count(),
            1
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn files_an_agent_posts_show_in_the_group_and_its_own_chat() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        // What send_message's `files` keeps (mcp.rs): the agent's temp files, gone right after.
        let (photo, menu) = (local.root.join("pasta.jpg"), local.root.join("menu.pdf"));
        std::fs::write(&photo, b"jpg").unwrap();
        std::fs::write(&menu, b"pdf").unwrap();
        let files: Vec<_> =
            [&photo, &menu].map(|path| filo_local::attachments::keep_sent(&local.root, path).unwrap()).into();
        std::fs::remove_file(&photo).unwrap();
        std::fs::remove_file(&menu).unwrap();
        let posted =
            group::post_files(db, "chef-id", "", files.clone(), group::Asks::default(), None, now_ms()).unwrap();
        let mut line = Message::new(Role::Assistant, "", None);
        line.in_group = Some(posted.id.clone());
        line.attachments = files.clone();
        local.agent_chats.append(&chef_chat, &line).unwrap();
        for chat in [&session, &chef_chat] {
            let detail = local.call("voiceSessions:detail", &json!({ "id": chat })).unwrap();
            let message = detail["messages"].as_array().unwrap().last().unwrap().clone();
            assert_eq!(message["role"], "assistant", "{message}");
            let shown: Vec<(&str, &str, &str)> = message["attachments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| {
                    (
                        file["name"].as_str().unwrap(),
                        file["contentType"].as_str().unwrap(),
                        file["localFile"].as_str().unwrap(),
                    )
                })
                .collect();
            let expected: Vec<(&str, &str, &str)> = files
                .iter()
                .map(|file| (file.name.as_str(), file.content_type.as_str(), file.file.to_str().unwrap()))
                .collect();
            assert_eq!(shown, expected);
            assert_eq!((shown[0].1, shown[1].1), ("image/jpeg", "application/pdf"));
        }
        assert!(files.iter().all(|file| file.file.is_file()));
        // The Agents tab's row names it.
        assert_eq!(local.call("agents:group", &json!({})).unwrap()["last"]["preview"], "pasta.jpg, menu.pdf");
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn what_names_no_one_goes_to_filo_and_at_everyone_to_every_member_at_once() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        let coach_chat = add_agent(&local, "coach-id", "Coach");
        let filo_chat = local.open_agent_chat(DEFAULT_AGENT_ID).unwrap().as_str().unwrap().to_string();
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let members = local.group_members();
        let send = |text: &str, request: &str| {
            local
                .call(
                    "voiceSessions:sendMessage",
                    &json!({ "sessionId": session, "message": text, "requestId": request }),
                )
                .unwrap()
        };
        let queued = || {
            group::active(db)
                .unwrap()
                .into_iter()
                .filter(|turn| turn.state == "queued")
                .map(|turn| (turn.speaker, turn.answers_user))
                .collect::<Vec<_>>()
        };
        let row = |request: &str| {
            let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
            turns.as_array().unwrap().iter().find(|row| row["requestId"] == request).unwrap().clone()
        };
        // An agent's row in the list: what it shows, and from when.
        let last = |agent: &str| {
            let rows = local.call("agents:list", &json!({})).unwrap();
            rows.as_array().unwrap().iter().find(|row| row["_id"] == json!(agent)).unwrap()["last"].clone()
        };
        let shown = |chat: &str| {
            local.call("voiceSessions:detail", &json!({ "id": chat })).unwrap()["messages"].as_array().unwrap().len()
        };

        // Filo takes it, reading what Chef is busy with in its own chat. There's no setting for it.
        assert!(local.call("agents:group", &json!({})).unwrap().get("answering").is_none());
        local.agent_chats.add_user_message(&chef_chat, "make voice messages play inline", "chef-turn").unwrap();
        send("which one of you is working on voice messages?", "req-1");
        assert_eq!(queued(), [(DEFAULT_AGENT_ID.to_string(), true)]);
        let turn = group::claim_next(db, 1).unwrap().unwrap();
        assert!(group::claim_alongside(db, &turn, 1).unwrap().is_empty());
        local.add_group_call(&local.agent_chats, &filo_chat, &turn, &members).unwrap();
        let asked = local.agent_chats.messages(&filo_chat).unwrap().pop().unwrap();
        assert!(asked.text.contains("- @Chef: Cooks.\n  - The user last wrote to it in its own chat just now: \"make voice messages play inline\""), "{}", asked.text);
        assert!(asked.text.contains("ask them in send_message's `ask`"), "{}", asked.text);
        assert!(!asked.group_call.unwrap().checking);
        group::finish(db, &turn.id, "completed", None, 2).unwrap();

        // "@everyone": every member's turn starts at once, each in its own chat.
        let (chef_before, coach_before) = (last("chef-id"), last("coach-id"));
        let (chef_shown, coach_shown) = (shown(&chef_chat), shown(&coach_chat));
        send("@everyone who is working on voice messages?", "req-2");
        let first = group::claim_next(db, 3).unwrap().unwrap();
        let mut turns = group::claim_alongside(db, &first, 3).unwrap();
        turns.insert(0, first);
        let mut speakers: Vec<&str> = turns.iter().map(|turn| turn.speaker.as_str()).collect();
        speakers.sort();
        assert_eq!(speakers, [DEFAULT_AGENT_ID, "chef-id", "coach-id"]);
        assert!(queued().is_empty());
        let chat_of = |speaker: &str| match speaker {
            "chef-id" => chef_chat.clone(),
            "coach-id" => coach_chat.clone(),
            _ => filo_chat.clone(),
        };
        for turn in &turns {
            local.add_group_call(&local.agent_chats, &chat_of(&turn.speaker), turn, &members).unwrap();
            let call = local.agent_chats.messages(&chat_of(&turn.speaker)).unwrap().pop().unwrap().group_call.unwrap();
            assert_eq!((call.from, call.answers_user, call.checking), (None, false, true));
        }
        // The group says they're all checking; their chats show no card yet, and their rows stay where they were.
        assert_eq!(row("req-2")["state"], "pending");
        let activity = row("req-2")["activity"].as_str().unwrap().to_string();
        assert!(
            activity.ends_with(" are checking") && ["Chef", "Coach", "Filo"].iter().all(|name| activity.contains(name)),
            "{activity}"
        );
        assert_eq!((last("chef-id"), last("coach-id")), (chef_before.clone(), coach_before.clone()));
        assert_eq!((shown(&chef_chat), shown(&coach_chat)), (chef_shown, coach_shown));
        // Chef's it is: it posts. Filo and Coach end quietly, with no error.
        let posted = group::post(
            db,
            "chef-id",
            "That's me.",
            group::Asks::default(),
            Some(&turns.iter().find(|turn| turn.speaker == "chef-id").unwrap().id),
            now_ms(),
        )
        .unwrap();
        let mut line = Message::new(Role::Assistant, posted.text.clone(), Some(posted.turn.clone().unwrap()));
        line.in_group = Some(posted.id.clone());
        local.agent_chats.append(&chef_chat, &line).unwrap();
        for turn in &turns {
            local.end_own_chat_turn(turn, &chat_of(&turn.speaker), &members, false);
        }
        assert!(turns.iter().all(|turn| group::turn(db, &turn.id).unwrap().unwrap().state == "completed"));
        assert_eq!((row("req-2")["state"].as_str(), row("req-2")["error"].clone()), (Some("completed"), Value::Null));
        assert_eq!(local.call("agents:group", &json!({})).unwrap()["replyFailed"], false);
        // Coach stayed quiet: no card in its chat, its row as it was. Chef answered: the card and its post's line show.
        assert_eq!((shown(&coach_chat), last("coach-id")), (coach_shown, coach_before));
        assert_eq!(shown(&chef_chat), chef_shown + 2);
        assert_eq!(last("chef-id")["preview"], "That's me.");

        // @Chef still goes to Chef alone, who has to answer.
        send("@Chef dinner?", "req-3");
        assert_eq!(queued(), [("chef-id".to_string(), true)]);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_routing_call_thinks_at_the_level_picked_else_low_as_the_model_takes_it() {
        let local = temp();
        let route = |engine, model: &str, reasoning: Option<&str>| group::RouteModel {
            engine,
            model: model.into(),
            reasoning: reasoning.map(str::to_string),
        };
        assert_eq!(local.route_reasoning(&route(filo_local::Engine::Claude, "haiku", None)).as_deref(), Some("low"));
        assert_eq!(
            local.route_reasoning(&route(filo_local::Engine::Claude, "haiku", Some("high"))).as_deref(),
            Some("high")
        );
        // Claude Code takes nothing under Low: the nearest it takes.
        assert_eq!(
            local.route_reasoning(&route(filo_local::Engine::Claude, "haiku", Some("minimal"))).as_deref(),
            Some("low")
        );
    }

    #[test]
    fn the_group_is_set_up_turned_off_and_on_and_shows_where() {
        let local = temp();
        add_agent(&local, "chef-id", "Chef");
        // Fresh data: off, never set up, and not a chat anywhere.
        let setup = local.call("agents:groupSetup", &json!({})).unwrap();
        assert_eq!(
            (setup["on"].clone(), setup["made"].clone(), setup["all"].clone()),
            (json!(false), json!(false), json!(true))
        );
        assert_eq!(local.call("agents:group", &json!({})).unwrap(), Value::Null);
        local.open_agent_chat(DEFAULT_AGENT_ID).unwrap();
        add_agent(&local, "coach-id", "Coach");
        assert!(
            local.call("agents:setUpGroup", &json!({ "all": false, "members": ["chef-id"], "turnOn": true })).is_err()
        );
        // Two picked, a name and a routing model: on, with only them in it.
        let session = local
            .call("agents:setUpGroup", &json!({ "name": "Kitchen", "all": false, "members": ["chef-id", DEFAULT_AGENT_ID], "engine": "claude", "model": "haiku", "reasoning": "medium", "turnOn": true }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let setup = local.call("agents:groupSetup", &json!({})).unwrap();
        assert_eq!(
            (setup["on"].clone(), setup["name"].clone(), setup["route"]["model"].clone()),
            (json!(true), json!("Kitchen"), json!("haiku"))
        );
        assert_eq!(setup["route"]["reasoning"], json!("medium"));
        let members: Vec<String> = local.group_members().into_iter().map(|member| member.id).collect();
        assert_eq!(members, [DEFAULT_AGENT_ID.to_string(), "chef-id".to_string()]);
        let row = local.call("agents:group", &json!({})).unwrap();
        assert_eq!((row["sessionId"].as_str(), row["members"].as_array().unwrap().len()), (Some(session.as_str()), 1));
        // A post, then off: it's no chat, takes no posts, and turned on again it keeps them, with a mark each time.
        local
            .call(
                "voiceSessions:sendMessage",
                &json!({ "sessionId": session, "message": "@Chef dinner?", "requestId": "r1" }),
            )
            .unwrap();
        local.call("agents:turnOffGroup", &json!({})).unwrap();
        assert_eq!(local.call("agents:group", &json!({})).unwrap(), Value::Null);
        assert!(group::active(local.db().unwrap()).unwrap().is_empty());
        assert!(
            local
                .call("voiceSessions:sendMessage", &json!({ "sessionId": session, "message": "hi", "requestId": "r2" }))
                .is_err()
        );
        assert!(local.call("agents:openGroup", &json!({})).is_err());
        let setup = local.call("agents:groupSetup", &json!({})).unwrap();
        assert_eq!(
            (setup["on"].clone(), setup["made"].clone(), setup["members"].as_array().unwrap().len()),
            (json!(false), json!(true), 2)
        );
        local.call("agents:setUpGroup", &json!({ "name": "Kitchen", "all": true, "turnOn": true })).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let shown: Vec<(String, String)> = detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["role"].as_str().unwrap().to_string(),
                    row["mark"].as_str().unwrap_or(row["content"].as_str().unwrap()).to_string(),
                )
            })
            .collect();
        assert_eq!(&shown[..1], [("user".to_string(), "@Chef dinner?".to_string())]);
        assert_eq!(
            &shown[1..],
            [
                ("mark".to_string(), "You turned the group off".to_string()),
                ("mark".to_string(), "You turned the group on".to_string())
            ]
        );
        assert_eq!(local.group_members().len(), 3);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_post_naming_no_one_waits_for_the_routing_call_and_the_main_agent_takes_it_without_one() {
        let local = temp();
        add_agent(&local, "chef-id", "Chef");
        local.open_agent_chat(DEFAULT_AGENT_ID).unwrap();
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let queued =
            || group::active(db).unwrap().into_iter().map(|turn| (turn.speaker, turn.answers_user)).collect::<Vec<_>>();
        let row = |request: &str| {
            let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
            turns.as_array().unwrap().iter().find(|row| row["requestId"] == request).unwrap().clone()
        };
        // While the call runs, the group says so.
        group::add_user_message(db, "waiting", "Can build 18 go out tonight?", now_ms()).unwrap();
        filo_local::group_route::queue(db, "waiting", None, now_ms()).unwrap();
        assert_eq!(
            (row("waiting")["state"].as_str(), row("waiting")["activity"].as_str()),
            (Some("pending"), Some("Picking who answers"))
        );
        // Tests ask no model: the main agent takes it, whoever posted last.
        local.run_routes();
        assert!(!filo_local::group_route::routing(db, "waiting"));
        assert_eq!(queued(), [(DEFAULT_AGENT_ID.to_string(), true)]);
        group::stop_queued(db, now_ms()).unwrap();
        group::post(db, "chef-id", "Pasta tonight.", group::Asks::default(), None, now_ms()).unwrap();
        local
            .call("voiceSessions:sendMessage", &json!({ "sessionId": session, "message": "yes", "requestId": "req-1" }))
            .unwrap();
        assert_eq!(queued(), [(DEFAULT_AGENT_ID.to_string(), true)]);
        group::stop_queued(db, now_ms()).unwrap();
        // @Chef skips it, and Reply too.
        local
            .call(
                "voiceSessions:sendMessage",
                &json!({ "sessionId": session, "message": "@Chef more?", "requestId": "req-2" }),
            )
            .unwrap();
        assert_eq!(queued(), [("chef-id".to_string(), true)]);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn with_the_group_off_an_ask_runs_in_the_askeds_chat_and_comes_back_to_the_askers() {
        let local = temp();
        local.no_turns.store(true, Ordering::SeqCst);
        let designer_chat = add_agent(&local, "designer-id", "App Designer");
        let dev_chat = add_agent(&local, "dev-id", "Developer");
        let db = local.db().unwrap();
        let members = group::roster(db);
        let dev: Vec<Member> = members.iter().filter(|member| member.id == "dev-id").cloned().collect();
        // App Designer asks Developer from its chat (as ask_agents does with the group off).
        filo_local::agent_asks::ask(
            db,
            &local.agent_chats,
            &designer_chat,
            "designer-id",
            &dev,
            "Check the picture at 24 px.",
            Some("t0"),
            now_ms(),
        )
        .unwrap();
        local.run_agent_asks();
        let ask = filo_local::agent_asks::in_state(db, "running").unwrap().remove(0);
        // Developer's chat has the ask, from App Designer, opening its chat.
        let detail = |chat: &str| {
            local.call("voiceSessions:detail", &json!({ "id": chat })).unwrap()["messages"].as_array().unwrap().clone()
        };
        let asked = detail(&dev_chat).pop().unwrap();
        assert_eq!(
            (asked["content"].as_str(), asked["askedBy"]["name"].as_str(), asked["askedBy"]["chat"].as_str()),
            (Some("Check the picture at 24 px."), Some("App Designer"), Some(designer_chat.as_str()))
        );
        let line = detail(&designer_chat).pop().unwrap();
        assert_eq!(
            (
                line["groupLink"]["kind"].as_str(),
                line["groupLink"]["agent"].as_str(),
                line["groupLink"]["state"].as_str()
            ),
            (Some("asked"), Some("dev-id"), Some("working"))
        );
        assert_eq!(line["groupLink"]["messageId"], asked["_id"]);
        // Developer answers there; its turn over, the answer goes back to App Designer's chat.
        let turn = ask.turn.clone().unwrap();
        local
            .agent_chats
            .append(
                &dev_chat,
                &Message::new(Role::Assistant, "At 24 px the faces blur; a 2 px ring fixes it.", Some(turn)),
            )
            .unwrap();
        local.run_agent_asks();
        let messages = local.agent_chats.messages(&designer_chat).unwrap();
        let back = messages.last().unwrap();
        assert!(
            back.text.contains("Developer answered:\n\nAt 24 px the faces blur") && back.role == Role::User,
            "{}",
            back.text
        );
        let shown = detail(&designer_chat);
        let (line, answered) = (&shown[shown.len() - 2], &shown[shown.len() - 1]);
        assert_eq!(
            (
                line["groupLink"]["state"].as_str(),
                answered["groupLink"]["kind"].as_str(),
                answered["groupLink"]["agent"].as_str()
            ),
            (Some("done"), Some("answered"), Some("dev-id"))
        );
        assert!(filo_local::agent_asks::to_report(db).unwrap().is_empty());
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn an_ask_from_a_chat_is_the_askeds_turn_in_its_chat_and_reports_back_when_the_thread_ends() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        let filo_chat = local.open_agent_chat(DEFAULT_AGENT_ID).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        // Chef asks Filo from its own chat (as the MCP server's ask_agents does).
        let members = local.group_members();
        group::ask(db, &members, &chef_chat, "chef-id", "@Filo can you add milk to the list?", &["Filo"], now_ms())
            .unwrap();

        // Filo's turn is waiting: nothing to report yet. Once it runs, it's in Filo's chat, Chef's ask.
        assert!(local.ask_reports().is_empty());
        let turn = group::claim_next(db, 1).unwrap().unwrap();
        local.add_group_call(&local.agent_chats, &filo_chat, &turn, &members).unwrap();
        let call = local.agent_chats.messages(&filo_chat).unwrap().pop().unwrap().group_call.unwrap();
        assert_eq!(
            (call.from.map(|from| from.name).as_deref(), call.ask.as_str()),
            (Some("Chef"), "@Filo can you add milk to the list?")
        );
        // The asker's own chat has nothing yet: it asked from there.
        assert!(local.agent_chats.messages(&chef_chat).unwrap().is_empty());
        group::post(db, DEFAULT_AGENT_ID, "Added milk.", group::Asks::default(), Some(&turn.id), now_ms()).unwrap();
        group::finish(db, &turn.id, "completed", None, 2).unwrap();
        let reports = local.ask_reports();
        assert_eq!(reports.len(), 1);
        let (_, chat, turn_id) = &reports[0];
        assert_eq!(chat, &chef_chat);
        let message = local.agent_chats.messages(&chef_chat).unwrap().pop().unwrap();
        assert_eq!((message.role, message.turn.as_deref()), (Role::User, Some(turn_id.as_str())));
        assert!(
            message.text.starts_with("↩ What came of what you asked") && message.text.contains("Filo: Added milk."),
            "{}",
            message.text
        );
        assert!(local.ask_reports().is_empty());
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_failed_answer_to_a_members_ask_shows_under_it_with_retry_in_the_group_and_the_members_chat() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        let filo_chat = local.open_agent_chat(DEFAULT_AGENT_ID).unwrap().as_str().unwrap().to_string();
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let members = local.group_members();
        // Filo asks Chef from its chat (no message of the user's in the thread); Chef's turn fails in Chef's chat.
        group::ask(db, &members, &filo_chat, DEFAULT_AGENT_ID, "@Chef where does the icon go?", &["Chef"], 1).unwrap();
        let asked = group::messages(db, 10).unwrap().0.pop().unwrap();
        let turn = group::claim_next(db, 2).unwrap().unwrap();
        local.add_group_call(&local.agent_chats, &chef_chat, &turn, &members).unwrap();
        let limit = "Claude Code: You've hit your session limit · resets 2:20am\n\nThat's your Claude Code plan's limit, not Filo's.";
        local.agent_chats.append(&chef_chat, &Message::new(Role::Error, limit, Some(turn.id.clone()))).unwrap();
        local.end_own_chat_turn(&turn, &chef_chat, &members, false);

        // The group shows it under Filo's ask: who, and why in a line; its row in the list says so too, after a restart.
        let failure = |local: &Arc<Local>| {
            let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
            turns.as_array().unwrap().iter().find(|row| row["state"] == "failed").cloned()
        };
        let reopened = Local::open(local.root.clone());
        let row = failure(&reopened).unwrap();
        assert_eq!(
            (row["turnId"].as_str(), row["userMessageId"].as_str()),
            (Some(turn.id.as_str()), Some(asked.id.as_str()))
        );
        assert_eq!(row["error"], "Chef couldn't reply: Claude Code: You've hit your session limit · resets 2:20am");
        assert_eq!(reopened.call("agents:group", &json!({})).unwrap()["replyFailed"], true);
        // Chef's chat has it under the group's ask, the same turn.
        let own = local.call("voiceSessions:chatTurns", &json!({ "sessionId": chef_chat })).unwrap();
        assert_eq!((own[0]["turnId"].as_str(), own[0]["state"].as_str()), (Some(turn.id.as_str()), Some("failed")));

        // Retry, from the group or from Chef's chat, queues Chef's turn in the group again.
        let retry = |local: &Arc<Local>| {
            local.call("voiceSessions:retryChatTurn", &json!({ "turnId": turn.id, "sessionId": chef_chat })).unwrap();
        };
        retry(&local);
        assert_eq!(group::turn(db, &turn.id).unwrap().unwrap().state, "queued");
        assert!(failure(&local).is_none());
        let again = group::claim_next(db, 3).unwrap().unwrap();
        group::finish(db, &again.id, "failed", Some(limit), 4).unwrap();
        assert!(failure(&local).is_some());
        retry(&reopened);
        assert_eq!(group::turn(db, &turn.id).unwrap().unwrap().state, "queued");
        // It ran and failed again; then Chef posted after all: nothing to show, and the row matches.
        group::claim_next(db, 5).unwrap().unwrap();
        group::finish(db, &turn.id, "failed", Some(limit), 6).unwrap();
        group::post(db, "chef-id", "Under the avatar.", group::Asks::default(), None, now_ms()).unwrap();
        assert!(failure(&local).is_none());
        assert_eq!(local.call("agents:group", &json!({})).unwrap()["replyFailed"], false);
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// As on Oct 13: Chef's turn for a coding task's news failed on the session limit in its chat. Retried, it went
    /// through with nothing to post, the earlier error still last in its chat: it's done in the group, settled in its
    /// chat, and neither row in the list says the reply failed. A failure from before this was kept the same way (in
    /// the group as failed) goes once a later turn in Chef's chat replied.
    #[test]
    fn a_members_failure_goes_once_its_turn_or_a_later_one_goes_through() {
        let local = temp();
        let chef_chat = add_agent(&local, "chef-id", "Chef");
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let members = local.group_members();
        let limit = "Claude Code: You've hit your session limit · resets 2:50pm\n\nThat's your Claude Code plan's limit, not Filo's.";
        let fail = |thread: &str| {
            let id = group::enqueue(db, thread, "chef-id", 0, "[Coding task round 1 failed]", false, now_ms())
                .unwrap()
                .unwrap();
            let turn = group::claim_next(db, now_ms()).unwrap().unwrap();
            assert_eq!(turn.id, id);
            local.add_group_call(&local.agent_chats, &chef_chat, &turn, &members).unwrap();
            local.agent_chats.append(&chef_chat, &Message::new(Role::Error, limit, Some(turn.id.clone()))).unwrap();
            local.end_own_chat_turn(&turn, &chef_chat, &members, false);
            turn
        };
        let shown = || {
            local
                .call("voiceSessions:chatTurns", &json!({ "sessionId": session }))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["state"] == "failed")
                .count()
        };
        let group_failed = || local.call("agents:group", &json!({})).unwrap()["replyFailed"].clone();
        let chef_failed = || {
            local
                .call("agents:list", &json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["_id"] == "chef-id")
                .unwrap()["replyFailed"]
                .clone()
        };
        let own_state = |turn: &str| {
            let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": chef_chat })).unwrap();
            turns.as_array().unwrap().iter().find(|row| row["turnId"] == turn).unwrap()["state"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let turn = fail("coding-1");
        assert_eq!(
            (shown(), group_failed(), chef_failed(), own_state(&turn.id)),
            (1, json!(true), json!(true), "failed".into())
        );
        group::retry_turns(db, std::slice::from_ref(&turn.id), now_ms()).unwrap();
        let again = group::claim_next(db, now_ms()).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        local.agent_chats.update_chat(&chef_chat, |chat| chat.last_ok_at = Some(filo_local::now_ms())).unwrap();
        local.end_own_chat_turn(&again, &chef_chat, &members, false);
        assert_eq!(group::turn(db, &turn.id).unwrap().unwrap().state, "completed");
        assert_eq!(
            (shown(), group_failed(), chef_failed(), own_state(&turn.id)),
            (0, json!(false), json!(false), "settled".into())
        );

        // Kept failed in the group by a build from before: it goes once a later turn in Chef's chat replied.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let older = fail("coding-2");
        group::finish(db, &older.id, "failed", Some(limit), now_ms() + 60_000).unwrap();
        assert_eq!((shown(), group_failed(), chef_failed()), (1, json!(true), json!(true)));
        std::thread::sleep(std::time::Duration::from_millis(2));
        local.agent_chats.add_user_message(&chef_chat, "how's it going?", "t-later").unwrap();
        local.agent_chats.append(&chef_chat, &Message::new(Role::Assistant, "Fine.", Some("t-later".into()))).unwrap();
        assert_eq!(
            (shown(), group_failed(), chef_failed(), own_state(&older.id)),
            (0, json!(false), json!(false), "settled".into())
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn group_check_ins_and_artifacts_belong_to_the_group() {
        let local = temp();
        add_agent(&local, "chef-id", "Chef");
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let db = local.db().unwrap();
        let now = now_ms();

        // A check-in planned in the group is its planner's turn there; the Filo agent's is Filo's.
        local.group_check_in(Some("chef-id"), "Check.").unwrap();
        local.group_check_in(Some(DEFAULT_AGENT_ID), "Check.").unwrap();
        local.group_check_in(None, "Check.").unwrap();
        let speakers: Vec<String> = group::active(db).unwrap().into_iter().map(|turn| turn.speaker).collect();
        assert_eq!(speakers, ["chef-id", DEFAULT_AGENT_ID, DEFAULT_AGENT_ID]);
        assert!(local.group_check_in(Some("gone"), "Check.").is_err());
        group::stop_queued(db, now).unwrap();

        // An artifact made in a group turn is a card in the group, and its page's question goes to the group.
        let origin = filo_local::artifacts::Origin {
            author: "Chef".into(),
            chat_id: Some(session.clone()),
            turn_id: Some("turn-1".into()),
        };
        let new = filo_local::artifacts::New {
            title: "Menu".into(),
            html: "<html><body>Pasta</body></html>".into(),
            ..Default::default()
        };
        let made = filo_local::artifacts::create(db, &local.artifacts_dir(), &new, &origin, now).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        assert_eq!(detail["tasks"][0]["artifact"]["_id"], json!(made.id));
        let sent = local
            .call(
                "canvasArtifacts:sendArtifactMessage",
                &json!({ "artifactId": made.id, "text": "Make it vegetarian" }),
            )
            .unwrap();
        assert_eq!(sent["sessionId"], json!(session));
        let (messages, _) = group::messages(db, 10).unwrap();
        assert_eq!(
            (messages.last().unwrap().role, messages.last().unwrap().text.as_str()),
            (Role::User, "Make it vegetarian")
        );
        let turn = group::active(db).unwrap().remove(0);
        assert_eq!(turn.speaker, DEFAULT_AGENT_ID);
        assert!(turn.reason.contains("artifact \u{201c}Menu\u{201d}"), "{}", turn.reason);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_reply_in_the_group_shows_what_it_answers_and_members_read_the_quote() {
        let local = temp();
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let posted =
            group::post(local.db().unwrap(), DEFAULT_AGENT_ID, "Pasta or rice?", group::Asks::default(), None, 1)
                .unwrap();
        let args =
            json!({ "sessionId": session, "message": "pasta", "requestId": "req-1", "replyToMessageId": posted.id });
        local.call("voiceSessions:sendMessage", &args).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session, "limit": 50 })).unwrap();
        let sent =
            detail["messages"].as_array().unwrap().iter().find(|message| message["role"] == "user").unwrap().clone();
        assert_eq!(
            (sent["content"].as_str(), sent["replyToMessageId"].as_str()),
            (Some("> Pasta or rice?\n\npasta"), Some(posted.id.as_str()))
        );
        assert_eq!(detail["messages"][0]["replyToMessageId"], Value::Null);
        // Filo, who answers, is told what the user quoted.
        assert!(group::active(local.db().unwrap()).unwrap()[0].reason.contains("> Pasta or rice?"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_reaction_in_the_group_is_kept_with_its_message() {
        let local = temp();
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let posted =
            group::post(local.db().unwrap(), DEFAULT_AGENT_ID, "Pasta or rice?", group::Asks::default(), None, 1)
                .unwrap();
        let reaction = |local: &Arc<Local>| {
            let detail = local.call("voiceSessions:detail", &json!({ "id": session, "limit": 50 })).unwrap();
            detail["messages"][0]["reaction"].clone()
        };
        let changes = local.changes();
        local.call("voiceSessions:reactToMessage", &json!({ "messageId": posted.id, "reaction": "👍" })).unwrap();
        assert!(changes.has_changed().unwrap());
        assert_eq!(reaction(&local), json!("👍"));
        // Taken back.
        local.call("voiceSessions:reactToMessage", &json!({ "messageId": posted.id })).unwrap();
        assert_eq!(reaction(&local), Value::Null);
        // A message that isn't the group's is looked for in the chats (local_backend.rs `react`).
        let error = local
            .call(
                "voiceSessions:reactToMessage",
                &json!({ "messageId": "elsewhere", "sessionId": session, "reaction": "👍" }),
            )
            .unwrap_err();
        assert!(error.contains("no longer exists"), "{error}");
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_filo_agent_is_filo_in_the_group() {
        let local = temp();
        let members = local.group_members();
        assert_eq!(members.len(), 1);
        assert_eq!((members[0].id.as_str(), members[0].name.as_str()), (DEFAULT_AGENT_ID, "Filo"));
        assert!(members[0].about.starts_with("You are Filo, the user's general assistant"));
        assert_eq!(local.speaker_agent(DEFAULT_AGENT_ID).unwrap().id, DEFAULT_AGENT_ID);
        // Another main agent leads, Filo answering when named; once Filo is removed it's no longer in the group.
        let chef = local.call("agents:create", &json!({ "name": "Chef", "instructions": "Cooks." })).unwrap();
        let chef = chef.as_str().unwrap().to_string();
        local.call("users:setMainAgent", &json!({ "agentId": chef })).unwrap();
        let ids = |local: &Local| local.group_members().into_iter().map(|member| member.id).collect::<Vec<_>>();
        assert_eq!(ids(&local), [chef.clone(), DEFAULT_AGENT_ID.to_string()]);
        let session = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let row = local.call("agents:group", &json!({})).unwrap();
        assert_eq!((row["lead"].as_str(), row["filo"].as_bool()), (Some(chef.as_str()), Some(true)));
        local.call("voiceSessions:sendMessage", &json!({ "sessionId": session, "message": "Dinner?" })).unwrap();
        assert_eq!(group::active(local.db().unwrap()).unwrap()[0].speaker, chef);
        local.call("agents:remove", &json!({ "id": DEFAULT_AGENT_ID })).unwrap();
        assert!(local.speaker_chat(DEFAULT_AGENT_ID).is_none());
        assert_eq!(ids(&local), [chef.clone()]);
        assert_eq!(local.call("agents:group", &json!({})).unwrap()["filo"], false);
        std::fs::remove_dir_all(&local.root).ok();
    }
}

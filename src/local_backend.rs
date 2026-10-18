// The backend on this Mac: the functions the pages call by name (`users:current`, `agents:list`, `voiceSessions:detail`…),
// answered from files under ~/.config/filo/local. backend.rs hands every call here, and its live
// queries run again whenever `version` changes.
//
//   local/agents/chats/<id>/… the agents' conversations (crates/filo-local's store), each with the agent as its
//                             instructions. Routines and check-ins of no agent's run in the main agent's. Filo's own
//                             chats (local/chats) are no longer read: nothing could open them.
//   local/pictures/<id>.png   pictures the user uploaded for agents (local_backend/pictures.rs)
//   local/filo.db             Filo's SQLite database (crates/filo-local `db`): notes, memories, skills, the schedule, coding,
//                             the agents' group chat (local_backend/group.rs), and a row each (crates/filo-local
//                             `app_data`) for the agents (fresh data starts with Filo as one, `add_default_agent`, until
//                             the first run ends, `remove_stand_in`), the profile (the user's name, the main agent), how
//                             far each conversation has been read and the uploaded pictures. Those were JSON files
//                             (agents.json, profile.json, seen.json, pictures.json), copied in once as the database
//                             opens and renamed `<name>.migrated`.
//
// A conversation's turn runs the user's coding tool (Claude Code, Codex, Muse Code or OpenCode; crates/filo-local `turn::run`) in a process of its own
// (local_backend/workers.rs), so it carries on when Filo quits or restarts; replies arrive through Filo's MCP server,
// which appends to the chat's file, so while turns run a watcher thread notices the files growing and bumps `version`.
// It notices the turn's activity file change the same way ("Running a command", crates/filo-local `chat_activity`).
// A turn's state (pending, completed, failed) is read from the chat itself, so it survives restarts.
//
// Anything else answers with an error naming the function ("… isn't on this Mac yet"); `MOVED` lists what this file
// answers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use filo_local::app_data;
use filo_local::chat_activity;
use filo_local::db::Db;
use filo_local::engine::{self, ModelOption, Stopper};
use filo_local::memories;
use filo_local::models;
use filo_local::notes;
use filo_local::skills;
use filo_local::store::valid_id;
use filo_local::turn::{self, Runtime};
use filo_local::turn_state;
use filo_local::{Engine, Message, Role, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

// Reminders, routines and check-ins, and the loop that fires them (local_backend/scheduled.rs).
mod scheduled;
pub use scheduled::start as start_schedule;
// The Coding page: its settings, projects and tasks, and the tasks' runs (local_backend/coding.rs).
mod coding;
pub(crate) use coding::free_disk_bytes;
pub use coding::start as start_coding;
pub(crate) use coding::{CLAUDE_EFFORTS, EFFORT_ORDER, effort_label, nearest_effort};
// Coding tasks conversations hand off (start_work): their rounds, questions, results and the wakes that report them.
mod handoff;
// A round's result shared at filo.im/results/<id>, and unshared (local_backend/result_share.rs).
pub(crate) mod result_share;
// An artifact shared at filo.im/artifacts/<id>, updated and unshared, the same way (local_backend/artifact_link.rs).
mod artifact_link;
// The Feed: its topics (each watched by an agent on a routine), posts and settings, and Ask (local_backend/feed.rs).
mod feed;
// The group chat of Filo and the agents, and the runner of its turns (local_backend/group.rs).
mod group;
// Turns that failed on a usage limit, tried again once it lifts (local_backend/limit_retry.rs).
mod limit_retry;

/// While the app is open: the group's turns run (local_backend/group.rs), and turns that failed on a usage limit are
/// tried again once it lifts (local_backend/limit_retry.rs).
pub fn start_group(local: Arc<Local>) {
    group::start(local.clone());
    limit_retry::start(local);
}

/// Once, in the background: the web pictures of the artifacts made before Filo saved them beside each page
/// (filo_local artifact_images.rs).
pub fn save_artifact_pictures(local: Arc<Local>) {
    std::thread::spawn(move || {
        let Ok(db) = local.db() else { return };
        match filo_local::artifact_images::save_existing(db, &local.artifacts_dir()) {
            Ok(0) => {}
            Ok(saved) => eprintln!("filo artifacts: saved {saved} web pictures of earlier artifacts"),
            Err(error) => eprintln!("filo artifacts: {error}"),
        }
    });
}
// Extensions › Apps: the user's MCP servers, their checks and sign-ins, and the "Your computer" switch (local_backend/apps.rs).
mod apps;
// Artifacts: the Artifacts tab, an artifact's page and their cards in a conversation (local_backend/artifacts.rs).
mod artifacts;
// Glances: the small visuals above Filo's replies, their pages and Save to Artifacts (local_backend/glances.rs).
mod glances;
// What a conversation shared, for the Shared section of an agent's profile (local_backend/shared.rs).
mod shared;
// The pictures the user uploaded for their agents (local_backend/pictures.rs).
mod pictures;
// The pets the agents drew, as custom avatars (local_backend/characters.rs).
mod characters;
pub use pictures::{picture_path as uploaded_picture_path, upload_id_of, upload_ref};
// Turns and coding rounds in processes of their own, which outlive the app (`filo --worker`, local_backend/workers.rs).
mod awake;
mod lid;
pub use lid::{backstop_main as lid_backstop_main, on_ac_power, uninstall_main as lid_uninstall_main};

mod workers;
pub use workers::{main as worker_main, restarting};

/// Chat replies (turns) running in workers on the data in `root` (`local/`): what the updater's install on quit waits
/// for (updater.rs).
pub fn replies_running(root: &Path) -> usize {
    workers::running(root).iter().filter(|worker| matches!(worker.job, workers::Job::Turn { .. })).count()
}

/// Whether a Filo window has the data in `root` open: it holds `coding.lock` while it does (coding.rs `owner_lock`).
pub fn window_open(root: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(root.join("coding.lock"))
    else {
        return false;
    };
    !filo_local::lock::try_lock(&file)
}
// Voice messages: sent with what was said, their recordings kept here (local_backend/voice.rs).
mod voice;
// Notes an older Filo kept waiting to be saved, carried into the database once (local_backend/note_carry_over.rs).
mod note_carry_over;
// ⌘P's search through the agents' and the group's messages (local_backend/search.rs).
mod search;
// Feed and Notes, on or off (local_backend/pages.rs).
mod pages;

/// The functions answered in this file (checked by a test); the other files list theirs.
#[cfg(test)]
pub const MOVED: &[&str] = &[
    "users:current",
    "users:updateUserName",
    "users:setMainAgent",
    "agents:list",
    "agents:create",
    "agents:canCreate",
    "agents:update",
    "agents:duplicate",
    "agents:remove",
    "agents:removeStandIn",
    "agents:scheduledCount",
    "agents:openChat",
    "agents:ofSession",
    "agents:group",
    "agents:openGroup",
    "agents:updateGroup",
    "agents:groupSetup",
    "agents:setUpGroup",
    "agents:turnOffGroup",
    "agents:routingModels",
    "customAvatars:list",
    "customAvatars:remove",
    "customAvatars:picked",
    "agentPictures:list",
    "agentPictures:add",
    "agentPictures:remove",
    "voiceSessions:detail",
    "voiceSessions:chatTurns",
    "voiceSessions:sendMessage",
    "voiceSessions:retryChatTurn",
    "voiceSessions:stopChatTurns",
    "voiceSessions:markSeen",
    "voiceSessions:setReasoning",
    "voiceSessions:reactToMessage",
    "voiceSessions:unseen",
    "voiceSessions:working",
    "voiceRecordings:sendLive",
    "voiceRecordings:createUpload",
    "voiceRecordings:send",
    "chatShared:list",
    // A conversation's coding tasks, their questions and the chats they work for are in local_backend/coding.rs
    // (`CODING`).
    "notes:mine",
    "notes:getMine",
    "notes:search",
    "notes:tags",
    "notes:createMine",
    "notes:updateMine",
    "notes:setPinnedMine",
    "notes:deleteMine",
    "memories:list",
    "memories:create",
    "memories:update",
    "memories:remove",
    "skills:list",
    "skills:save",
    "skills:remove",
    "skills:import",
];

/// The model picker's cache is asked about again only when older than this (ms): an app restarted soon after reuses it.
const MODELS_FRESH_FOR: i64 = 10 * 60 * 1000;
/// While turns run, how often their chats' files are checked for replies.
const WATCH_EVERY: Duration = Duration::from_millis(300);
/// A reply nobody has seen for this long (ms) is notified about (`pushedUntil` in `voiceSessions:unseen`): a banner, the
/// card in the window, the pet. A chat on screen marks it seen well before.
const PUSH_AFTER: f64 = if cfg!(test) { 1000. } else { 5000. };
/// `voiceSessions:detail` without a limit.
const DEFAULT_LIMIT: usize = 100;
const PREVIEW_CHARS: usize = 120;
/// A message's files, as the composer limits them (attachments.rs), and the copies it makes (its pictures made fit
/// for the model): a file of the user's is kept whole, of any size (`keep_attachment`).
const MAX_ATTACHMENTS: usize = filo_local::attachments::MAX_PER_MESSAGE;
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
/// Notes and tasks one message carries (note_share.rs, work_share.rs), and the notes' text together.
const MAX_SHARED_NOTES: usize = 3;
const MAX_SHARED_NOTE_CHARS: usize = 8_000;
const MAX_SHARED_WORK: usize = 5;

/// The agent fresh local data starts with: Filo, the main agent until the user makes one. The first run's setup
/// (screens/welcome.rs) removes it while it was never talked to (`remove_stand_in`), so the user's own first agent is
/// the only one, or the Agents tab shows its empty state. A fixed id, so the app and the background scheduler seeding at
/// once still make one: filo-local's `apps::FILO_AGENT`, which is also its speaker id in the group.
pub const DEFAULT_AGENT_ID: &str = filo_local::apps::FILO_AGENT;
/// Its instructions (`persona` wraps them): a general assistant.
const DEFAULT_AGENT_INSTRUCTIONS: &str = "You are Filo, the user's general assistant. Help with whatever they bring: \
questions, writing, planning, research, ideas and everyday tasks on their Mac. Be warm, direct and concise, ask when \
something important is unclear, and use the tools you have (notes, memories, skills, reminders) when they help.";
/// Why `agents:remove` refuses the main agent.
pub const MAIN_AGENT_STAYS: &str = "This is your main agent. Pick another main agent first, then remove it.";
/// Why `agents:create` refuses while neither coding tool is plugged in on the Coding page (`coding_tools`).
pub const NO_CODING_TOOL: &str = "Agents answer with a coding tool (Claude Code, Codex, Muse Code or OpenCode), and all of them are unplugged. Plug one in on the Coding page, then create your agent.";

/// The profile as the pages read it (`users:current`), from its rows (crates/filo-local `app_data`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    #[serde(skip_serializing_if = "Option::is_none")]
    user_name: Option<String>,
    /// The agent that leads the group, answers Ask on Feed posts and owns what belongs to no agent (crates/filo-local
    /// `main_agent`); Filo's until the user picks another (`ensure_main_agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_agent_id: Option<String>,
}

/// The engine and model an agent's turns run with, in its own chat and the group: the one picked for it, else (one from
/// before there was a pick) the app's default engine and its model.
fn model_choice(agent: &app_data::Agent) -> (Engine, Option<String>) {
    match agent.known_engine() {
        Some(engine) => (engine, agent.model.clone()),
        None => (default_engine(), None),
    }
}

/// What Stop reaches in work under way (a turn, a coding round, a worker): what stops it once it has started (its CLI's
/// or its worker's stopper), and whether Stop came first, which stops what starts.
#[derive(Clone, Default)]
pub(super) struct Stoppable {
    stopper: Arc<Mutex<Option<Stopper>>>,
    requested: Arc<AtomicBool>,
}

impl Stoppable {
    /// Work started elsewhere (an adopted worker), stopped with `stopper`.
    fn of(stopper: Stopper) -> Stoppable {
        Stoppable { stopper: Arc::new(Mutex::new(Some(stopper))), ..Stoppable::default() }
    }

    /// Stop: what's running stops, and what starts from now on.
    fn stop(&self) {
        let stopper = self.stopper.lock().unwrap();
        self.requested.store(true, Ordering::SeqCst);
        if let Some(stopper) = stopper.as_ref() {
            stopper.stop();
        }
    }

    /// The work started: `stopper` stops it, at once when Stop came first.
    fn started(&self, stopper: Stopper) {
        let mut held = self.stopper.lock().unwrap();
        if self.requested.load(Ordering::SeqCst) {
            stopper.stop();
        }
        *held = Some(stopper);
    }

    fn requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

/// A turn under way.
#[derive(Default)]
struct Running {
    session_id: String,
    started_at: f64,
    stop: Stoppable,
    /// In a worker process (local_backend/workers.rs): it carries on when Filo quits with "Keep coding after Filo quits".
    detached: bool,
}

pub struct Local {
    root: PathBuf,
    /// The agents' conversations.
    agent_chats: Store,
    /// One writer at a time for the JSON files that are left (`update_json`: limit retries, the computer's settings,
    /// sharing to claude.ai).
    files: Mutex<()>,
    /// Filo's database, or why it couldn't open (its pages show that).
    db: Result<Db, String>,
    running: Mutex<HashMap<String, Running>>,
    watching: AtomicBool,
    version: watch::Sender<u64>,
    /// When (ms) the live queries run again for a reply that will have waited `PUSH_AFTER` unseen (`unseen`).
    push_wakes: Mutex<HashSet<i64>>,
    /// Coding tasks under way, and where projects are looked for (local_backend/coding.rs).
    coding: coding::State,
    /// Every tool's models as the CLIs named them, read from the database the first time they're asked for
    /// (`all_model_options`).
    models: Mutex<Option<Arc<Vec<ModelOption>>>>,
    /// The picker's rows (`model_options`), kept with the tools signed in and the models they were taken from.
    picker: Mutex<Option<(Vec<Engine>, Arc<Vec<ModelOption>>, Arc<Vec<ModelOption>>)>>,
    /// This run has asked the CLIs about their models already, or is asking (`claim_model_refresh`).
    models_refreshing: AtomicBool,
    /// The group chat's runner (local_backend/group.rs).
    group: group::State,
    /// Checks and sign-ins under way for the user's apps, and where their secrets are (local_backend/apps.rs).
    apps: apps::State,
    /// This process is a worker running one turn or round (`filo --worker`): it starts nothing else and leaves telling
    /// conversations about their coding tasks to the app.
    in_worker: AtomicBool,
    /// Voice messages' uploads under way, and the relay that transcribes them (local_backend/voice.rs).
    voice: voice::State,
    /// A test that sends messages starts no turns, which would run the real CLI.
    #[cfg(test)]
    no_turns: AtomicBool,
}

/// Now, in milliseconds since the epoch.
fn now_ms() -> i64 {
    filo_local::now_ms() as i64
}

fn not_here(path: &str) -> String {
    format!("This isn't on this Mac yet ({path}): Filo for Mac doesn't have it.")
}

/// Whether `path` ("feed:mine") is in one of `namespaces` ("feed"): a module that owns a namespace answers all of it,
/// a function it doesn't have with `not_here`.
fn in_namespace(path: &str, namespaces: &[&str]) -> bool {
    path.split_once(':').is_some_and(|(namespace, _)| namespaces.contains(&namespace))
}

/// The routine a message is the run of: its mark, or for one saved before runs had it, read from the words
/// local_backend/scheduled.rs `routine_message` starts with ("⏰ Routine “Title” (every 30 minutes). …").
fn routine_run(message: &Message) -> Option<filo_local::store::RoutineRun> {
    if message.role != Role::User {
        return None;
    }
    if let Some(routine) = &message.routine {
        return Some(routine.clone());
    }
    let (title, rest) = message.text.strip_prefix("⏰ Routine “")?.split_once('”')?;
    let when = rest.strip_prefix(" (").and_then(|rest| rest.split_once(").")).map_or("", |(when, _)| when);
    Some(filo_local::store::RoutineRun { title: title.to_string(), when: when.to_string() })
}

/// A message's files as the chat shows them (`Attachment`): drawn and opened from the copy saved here, or an agent's
/// file where it is. One that's no longer there is `gone`, with no file to draw or open.
fn attachments_json(files: &[filo_local::store::Attachment]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|file| {
                let gone = !file.file.is_file();
                json!({
                    "storageId": file.storage_id,
                    "name": file.name,
                    "contentType": file.content_type,
                    "size": file.size as f64,
                    "localPath": file.local_path,
                    "localFile": (!gone).then_some(&file.file),
                    "gone": gone,
                })
            })
            .collect(),
    )
}

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    arg(args, key).filter(|value| !value.is_empty()).ok_or_else(|| format!("`{key}` is required"))
}

fn bool_arg(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key).and_then(Value::as_bool).ok_or_else(|| format!("`{key}` is required"))
}

fn number(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

/// How many a list asks for (`limit`): `default` without one, else from 1 to `max`.
fn limit(args: &Value, default: usize, max: usize) -> usize {
    number(args, "limit").map_or(default, |limit| limit.clamp(1., max as f64) as usize)
}

impl Local {
    pub fn open(root: PathBuf) -> Arc<Local> {
        let (version, _) = watch::channel(0);
        let db = Db::open(&root.join(filo_local::db::FILE));
        if let Ok(db) = &db {
            let _ = notes::purge_deleted(db, filo_local::now_ms() as i64);
            // In case another page's migration took the artifacts' slot (artifacts.rs `SCHEMA`).
            let _ = filo_local::artifacts::ensure(db);
            let _ = filo_local::group::ensure_asks(db);
        }
        let local = Arc::new(Local {
            db,
            agent_chats: Store::at(root.join("agents")),
            root,
            files: Mutex::new(()),
            running: Mutex::new(HashMap::new()),
            watching: AtomicBool::new(false),
            version,
            push_wakes: Mutex::new(HashSet::new()),
            coding: coding::State::default(),
            models: Mutex::new(None),
            picker: Mutex::new(None),
            models_refreshing: AtomicBool::new(false),
            group: group::State::default(),
            apps: apps::State::default(),
            in_worker: AtomicBool::new(false),
            voice: voice::State::default(),
            #[cfg(test)]
            no_turns: AtomicBool::new(false),
        });
        local.add_default_agent();
        local.ensure_main_agent();
        // Skills from when they were rows of the database move into their folders, once.
        if let Ok(db) = local.db() {
            match skills::migrate(db, &local.skills_dir()) {
                Ok(0) => {}
                Ok(moved) => {
                    eprintln!("filo skills: moved {moved} from the database into {}", local.skills_dir().display())
                }
                Err(error) => eprintln!("filo skills: {error}"),
            }
        }
        // And skills changed while the app was closed are copied again.
        local.write_skills();
        local
    }

    /// The user's skills, a folder each (filo_local skills.rs).
    fn skills_dir(&self) -> PathBuf {
        self.root.join(skills::DIR)
    }

    /// A skill was saved, removed or imported: the CLIs' copies follow, and the pages hear of it.
    fn skills_changed(&self) {
        self.write_skills();
        self.changed();
    }

    /// The user's skill folders copied whole as Claude Code's and Codex's own skills, in the agents' workspace
    /// (filo_local store.rs `write_skills`), as they are now.
    fn write_skills(&self) {
        let skills = skills::list(&self.skills_dir());
        if let Err(error) = self.agent_chats.write_skills(&skills) {
            eprintln!("filo skills: couldn't write them in {}: {error}", self.agent_chats.root().display());
        }
    }

    // --- The model picker ---

    /// Every tool's models: what Filo's database has from the last refresh (filo_local `models`), read the first time
    /// and kept here, so a conversation shows them at once without asking a CLI. For naming a model (a chip, a task
    /// row), the tool signed out or not; the pickers list `model_options`.
    pub fn all_model_options(&self) -> Arc<Vec<ModelOption>> {
        let mut rows = self.models.lock().unwrap();
        rows.get_or_insert_with(|| Arc::new(models::picker_rows(self.db.as_ref().ok()))).clone()
    }

    /// The model pickers' rows (the composer's, the Edit agent page's, the group's routing step): the models of the
    /// tools installed and signed in on this Mac, as the Coding page and Welcome have them (coding.rs
    /// `signed_in_engines`). None before the first look at the tools has answered (`refresh_models` looks).
    pub fn model_options(&self) -> Arc<Vec<ModelOption>> {
        let all = self.all_model_options();
        let Some(ready) = coding::signed_in_engines() else { return Arc::default() };
        let mut picker = self.picker.lock().unwrap();
        match &*picker {
            Some((engines, from, rows)) if *engines == ready && Arc::ptr_eq(from, &all) => rows.clone(),
            _ => {
                let rows = Arc::new(signed_in_rows(&all, &ready));
                *picker = Some((ready, all, rows.clone()));
                rows
            }
        }
    }

    /// Whether the tools signed in are known yet (`model_options` lists none before).
    pub fn knows_sign_ins(&self) -> bool {
        coding::signed_in_engines().is_some()
    }

    /// Whether to ask the CLIs about their models now (claiming it): once per run, and not while every engine's rows
    /// are newer than `MODELS_FRESH_FOR` (an app restarted soon after).
    pub fn claim_model_refresh(&self, now: i64) -> bool {
        if self.models_refreshing.swap(true, Ordering::SeqCst) {
            return false;
        }
        let fresh = |engine| {
            self.db
                .as_ref()
                .ok()
                .and_then(|db| models::refreshed_at(db, engine).ok().flatten())
                .is_some_and(|at| now - at < MODELS_FRESH_FOR)
        };
        !Engine::ALL.into_iter().all(fresh)
    }

    /// Looks at which tools are signed in unless that's known, and with `ask` asks the CLIs which models they offer and
    /// answer with by default, saves that in the database and keeps it for the picker. Blocks for a few seconds: off
    /// the UI thread. True when the picker's rows changed.
    pub fn refresh_models(&self, ask: bool) -> bool {
        let before = self.model_options();
        coding::look_at_tools();
        if ask {
            self.refresh_models_with(
                probe_claude_models,
                engine::codex_options,
                probe_session_models,
                filo_local::now_ms() as i64,
            );
        }
        *self.model_options() != *before
    }

    /// `refresh_models` with what each CLI said: Claude Code's rows (None when it couldn't be asked: the cached ones
    /// stay), Codex's, and Muse Code's and OpenCode's (each None when it couldn't be asked).
    fn refresh_models_with(
        &self,
        claude: impl FnOnce() -> Option<Vec<ModelOption>>,
        codex: impl FnOnce() -> Vec<ModelOption>,
        others: impl FnOnce() -> Vec<(Engine, Option<Vec<ModelOption>>)>,
        now: i64,
    ) -> bool {
        let before = self.all_model_options();
        let (claude, codex, others) = (claude(), codex(), others());
        if let Ok(db) = &self.db {
            if let Some(rows) = &claude {
                let _ = models::save(db, Engine::Claude, rows, now);
            }
            let _ = models::save(db, Engine::Codex, &codex, now);
        }
        let kept = |engine: Engine| before.iter().filter(|row| row.engine == engine).cloned().collect::<Vec<_>>();
        let mut rows = claude.unwrap_or_else(|| kept(Engine::Claude));
        rows.extend(codex);
        // Muse Code's and OpenCode's as they said, else as they were (none when it isn't installed), saved either way so
        // a restart soon after doesn't ask again.
        for engine in [Engine::Muse, Engine::OpenCode] {
            let said = others
                .iter()
                .find(|(other, _)| *other == engine)
                .and_then(|(_, rows)| rows.clone())
                .unwrap_or_else(|| kept(engine));
            if let Ok(db) = &self.db {
                let _ = models::save(db, engine, &said, now);
            }
            rows.extend(said);
        }
        let changed = rows != *before;
        *self.models.lock().unwrap() = Some(Arc::new(rows));
        changed
    }

    /// Fresh local data gets Filo as an agent; its conversation starts when the Agents tab first opens it
    /// (`agents:openChat`), on the engine picked in the tour. Once there are agents they're the user's (crates/filo-local
    /// `app_data::start_agents`): nothing is added again, so a removed Filo stays removed and existing agents and
    /// conversations are left alone. Nor while agents.json couldn't be copied in, whose agents may be the user's.
    fn add_default_agent(&self) {
        let Ok(db) = self.db() else { return };
        if let Err(error) = app_data::start_agents(db, &self.filo_agent()) {
            eprintln!("filo: {error}");
        }
    }

    /// The Filo agent as fresh data has it: a general assistant, the waving panda. Its name and picture are then its own,
    /// changed on the Agents tab like any agent's.
    fn filo_agent(&self) -> app_data::Agent {
        app_data::Agent {
            id: DEFAULT_AGENT_ID.to_string(),
            name: "Filo".into(),
            instructions: DEFAULT_AGENT_INSTRUCTIONS.to_string(),
            picture: crate::avatars::DEFAULT_AVATAR.into(),
            background: None,
            session_id: None,
            engine: None,
            model: None,
            earlier_chat: None,
            created_at: now_ms() as f64,
        }
    }

    /// While there are agents, one of them is the main agent: until the user picks one (or when the one picked is gone),
    /// Filo, brought back as an agent if it was removed. With none (the first run skipped), there's none until the user
    /// makes one (`create_agent`).
    fn ensure_main_agent(&self) {
        let Ok(db) = self.db() else { return };
        let picked = app_data::profile_text(db, app_data::MAIN_AGENT).ok().flatten();
        let agents = self.agents();
        if agents.is_empty() || picked.as_deref().is_some_and(|id| agents.iter().any(|agent| agent.id == id)) {
            return;
        }
        if !agents.iter().any(|agent| agent.id == DEFAULT_AGENT_ID) {
            // Back at the top of the list, where it was.
            let first = agents.iter().map(|agent| agent.created_at).reduce(f64::min);
            let filo = app_data::Agent {
                created_at: first.map_or_else(|| now_ms() as f64, |first| first - 1.),
                ..self.filo_agent()
            };
            if let Err(error) = app_data::add_agent(db, &filo) {
                eprintln!("filo: {error}");
                return;
            }
        }
        if let Err(error) = self.set_main_agent(DEFAULT_AGENT_ID) {
            eprintln!("filo: {error}");
        }
    }

    /// The main agent's id (crates/filo-local `main_agent`).
    fn main_agent_id(&self) -> String {
        self.db().map_or_else(|_| DEFAULT_AGENT_ID.to_string(), filo_local::main_agent::id)
    }

    /// Picks the main agent (none: no agent is, until `create_agent` makes one).
    fn set_main_agent(&self, id: &str) -> Result<(), String> {
        app_data::set_profile(self.db()?, app_data::MAIN_AGENT, Some(&json!(id)))?;
        self.changed();
        Ok(())
    }

    /// The main agent, while it's one of the agents (`ensure_main_agent` sees to that).
    fn main_agent(&self) -> Option<app_data::Agent> {
        let id = self.main_agent_id();
        self.agents().into_iter().find(|agent| agent.id == id)
    }

    /// Changes whenever the local data does.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    fn changed(&self) {
        self.version.send_modify(|version| *version += 1);
    }

    /// `changed`, from a drive (backend.rs `poke`).
    #[cfg(feature = "dev")]
    pub fn poke(&self) {
        self.changed();
    }

    /// Why the database can't be opened, when a newer Filo wrote it: the app stops rather than run without the user's
    /// agents (main.rs).
    pub fn newer_data(&self) -> Option<String> {
        self.db.as_ref().err().filter(|error| error.contains(filo_local::db::NEWER)).cloned()
    }

    fn db(&self) -> Result<&Db, String> {
        self.db.as_ref().map_err(|error| format!("Filo's database couldn't open: {error}"))
    }

    /// A change to the database, then the live queries hear of it.
    /// Runs a change to the database and wakes the live queries; its result as the answer.
    fn write_db<T: Serialize>(&self, write: impl FnOnce(&Db) -> Result<T, String>) -> Result<Value, String> {
        let result = write(self.db()?)?;
        self.changed();
        Ok(json!(result))
    }

    fn read_json<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> T {
        self.try_read_json(name).unwrap_or_default()
    }

    /// A JSON file as it is, the default when there's none yet, and an error when it's there but can't be read: one
    /// written by a newer Filo (an engine this build doesn't know), or damaged.
    fn try_read_json<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> Result<T, String> {
        filo_local::files::read_json(&self.root.join(name)).map(Option::unwrap_or_default)
    }

    fn write_json<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
        crate::config::write_atomic(&self.root.join(name), &bytes)
            .map_err(|error| format!("Couldn't save {name}: {error}"))
    }

    /// Reads, changes and writes one JSON file, then tells the live queries. A file that's there but can't be read is
    /// left as it is (an error), never saved over as if it were empty: on 2026-10-14 an older Filo that didn't know
    /// OpenCode read the user's agents as none and saved just Filo in their place. The agents, the profile, read markers
    /// and pins are in the database now, a row each (crates/filo-local `app_data`); this is for what's left.
    fn update_json<T: for<'de> Deserialize<'de> + Serialize + Default, R>(
        &self,
        name: &str,
        change: impl FnOnce(&mut T) -> Result<R, String>,
    ) -> Result<R, String> {
        let result = {
            let _guard = self.files.lock().unwrap();
            let mut value: T = self.try_read_json(name)?;
            let result = change(&mut value)?;
            self.write_json(name, &value)?;
            result
        };
        self.changed();
        Ok(result)
    }

    /// The store a conversation is in: the agents', while it's there.
    fn store_of(&self, session_id: &str) -> Option<&Store> {
        if !valid_id(session_id) {
            return None;
        }
        Some(&self.agent_chats).filter(|store| store.chat(session_id).ok().flatten().is_some())
    }

    /// Where a routine or check-in of no agent's (or whose conversation is gone) runs, and what an artifact whose chat
    /// is gone is asked about in: `agent`'s conversation while it's one of the agents, else the main agent's (else the
    /// first agent's).
    fn agent_or_main_chat(&self, agent: Option<&str>) -> Result<String, String> {
        let agent = agent
            .and_then(|id| self.agent(id))
            .or_else(|| self.main_agent())
            .or_else(|| self.agents().into_iter().next());
        let agent = agent.ok_or("Add an agent on the Agents tab first.")?;
        self.open_agent_chat(&agent.id)?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "Couldn't open the conversation.".into())
    }

    pub fn call(self: &Arc<Self>, path: &str, args: &Value) -> Result<Value, String> {
        // The group, and conversation functions about the group's session.
        if let Some(result) = self.group_call(path, args) {
            return result;
        }
        match path {
            "users:current" => Ok(self.profile()),
            "users:updateUserName" => {
                app_data::set_profile(self.db()?, "userName", Some(&json!(required(args, "userName")?.trim())))?;
                self.changed();
                Ok(Value::Null)
            }
            // An agent's menu on the Agents tab: who leads the group, answers Ask and owns what's no agent's.
            "users:setMainAgent" => {
                let id = required(args, "agentId")?;
                if !self.agents().iter().any(|agent| agent.id == id) {
                    return Err("This agent no longer exists.".into());
                }
                self.set_main_agent(id)?;
                Ok(Value::Null)
            }
            "agents:list" => Ok(self.agent_rows()),
            "agents:create" => self.create_agent(args),
            // Whether `agents:create` would go ahead, and why not: the New agent button explains before the form opens.
            "agents:canCreate" => Ok(match self.agent_tool_plugged() {
                Ok(()) => json!({ "allowed": true, "reason": null }),
                Err(reason) => json!({ "allowed": false, "reason": reason }),
            }),
            "agents:update" => self.update_agent(args),
            "agents:duplicate" => self.duplicate_agent(args),
            "agents:remove" => self.remove_agent(args),
            "agents:removeStandIn" => self.remove_stand_in(),
            "agents:scheduledCount" => self.scheduled_count(args),
            "agents:openChat" => self.open_agent_chat(required(args, "id")?),
            // Whose conversation this is, for opening it from a link (`AppShell::open_chat`): an agent's, the group's,
            // or null for one that's no agent's any more.
            "agents:ofSession" => {
                let session_id = required(args, "sessionId")?;
                if self.group_session().as_deref() == Some(session_id) {
                    return Ok(json!({ "agentGroup": true }));
                }
                Ok(self.agent_of(session_id).map_or(Value::Null, |agent| json!({ "agentId": agent })))
            }
            "customAvatars:list" => Ok(self.character_rows()),
            "customAvatars:remove" => self.remove_character(args),
            "customAvatars:picked" => self.character_picked(args),
            "agentPictures:list" => Ok(self.picture_rows()),
            "agentPictures:add" => self.add_picture(args),
            "agentPictures:remove" => self.remove_picture(args),
            "voiceSessions:detail" => self.detail(args),
            "voiceSessions:chatTurns" => Ok(self.chat_turns(required(args, "sessionId")?)),
            "voiceSessions:sendMessage" => self.send_message(args),
            "voiceSessions:retryChatTurn" => self.dispatch_turn(args),
            "voiceSessions:stopChatTurns" => {
                self.stop_session(required(args, "sessionId")?);
                Ok(Value::Null)
            }
            "voiceSessions:markSeen" => self.mark_seen(args),
            // The composer's reasoning picker: the conversation's turns think that hard from the next one on (`set_turn_model`).
            "voiceSessions:setReasoning" => {
                let session_id = required(args, "sessionId")?;
                let store = self.store_of(session_id).ok_or("This conversation no longer exists.")?;
                choose_reasoning(store, session_id, required(args, "reasoning")?)?;
                self.changed();
                Ok(Value::Null)
            }
            // The chat's React on a message of an agent's chat (the group's are kept with them, group.rs).
            "voiceSessions:reactToMessage" => self.react(args),
            "voiceSessions:unseen" => Ok(self.unseen()),
            "voiceSessions:working" => Ok(self.working(args)),
            "chatShared:list" => Ok(self.shared(args)),
            "notes:mine" => Ok(json!(notes::list(self.db()?)?)),
            "notes:getMine" => Ok(json!(notes::get(self.db()?, required(args, "id")?)?)),
            "notes:search" => {
                Ok(json!(notes::search(self.db()?, arg(args, "query").unwrap_or_default(), arg(args, "tag"))?))
            }
            "notes:tags" => {
                let tags = notes::tags(self.db()?)?;
                Ok(Value::Array(tags.into_iter().map(|(tag, count)| json!({ "tag": tag, "count": count })).collect()))
            }
            "notes:createMine" => {
                let fields = note_fields(args)?;
                self.write_db(|db| notes::create(db, &fields))
            }
            "notes:updateMine" => {
                let (id, fields) = (required(args, "id")?, note_fields(args)?);
                self.write_db(|db| notes::update(db, id, &fields))
            }
            "notes:setPinnedMine" => {
                let (id, pinned) =
                    (required(args, "id")?, args.get("pinned").and_then(Value::as_bool).ok_or("`pinned` is required")?);
                self.write_db(|db| notes::set_pinned(db, id, pinned))
            }
            "notes:deleteMine" => {
                let id = required(args, "id")?;
                self.write_db(|db| notes::delete(db, id))
            }
            "memories:list" => {
                let options = args.get("paginationOpts");
                let count = options
                    .and_then(|options| options.get("numItems"))
                    .and_then(Value::as_f64)
                    .map_or(30, |count| count.max(1.) as usize);
                let cursor = options.and_then(|options| options.get("cursor")).and_then(Value::as_str);
                Ok(json!(memories::page(self.db()?, count, cursor)?))
            }
            // Added and edited on the Memories page: the user's own words, so confirmed.
            "memories:create" => {
                let new = memories::New {
                    content: required(args, "content")?.to_string(),
                    kind: arg(args, "type").map(str::to_string),
                    source: "user_explicit".into(),
                    source_id: None,
                    explanation: "You added this".into(),
                    confirmation: "confirmed".into(),
                };
                self.write_db(|db| memories::create(db, &new))
            }
            "memories:update" => {
                let id = required(args, "id")?;
                let change = memories::Change {
                    content: Some(required(args, "content")?.to_string()),
                    kind: arg(args, "type").map(str::to_string),
                    explanation: None,
                    confirmation: Some("corrected".into()),
                };
                self.write_db(|db| memories::update(db, id, &change))
            }
            "memories:remove" => {
                let id = required(args, "id")?;
                self.write_db(|db| memories::remove(db, id))
            }
            "skills:list" => Ok(json!(skills::list(&self.skills_dir()))),
            // Creates the skill or changes the one with its name (its other files stay); the name it's saved under.
            "skills:save" => {
                let fields: skills::Fields =
                    serde_json::from_value(args.clone()).map_err(|error| format!("The skill isn't right: {error}"))?;
                let saved = skills::save(&self.skills_dir(), &fields)?;
                self.skills_changed();
                Ok(json!(saved))
            }
            "skills:remove" => {
                let removed = skills::remove(&self.skills_dir(), required(args, "name")?)?;
                self.skills_changed();
                Ok(json!(removed))
            }
            // Copies a skill folder picked on the Skills page, all its files with it; the name it got.
            "skills:import" => {
                let imported = skills::import(&self.skills_dir(), Path::new(required(args, "path")?))?;
                self.skills_changed();
                Ok(json!(imported))
            }
            _ => self
                .coding_call(path, args)
                .or_else(|| self.scheduled_call(path, args))
                .or_else(|| self.feed_call(path, args))
                .or_else(|| self.artifacts_call(path, args))
                .or_else(|| self.glances_call(path, args))
                .or_else(|| self.apps_call(path, args))
                .or_else(|| self.voice_call(path, args))
                .or_else(|| self.search_call(path, args))
                .unwrap_or_else(|| Err(not_here(path))),
        }
    }

    // --- Profile ---

    fn profile(&self) -> Value {
        let rows = self.db().and_then(app_data::profile).unwrap_or_default();
        let mut profile: Profile = serde_json::from_value(Value::Object(rows)).unwrap_or_default();
        // Nothing asks for it before the app opens: the Mac's user, changed in Settings.
        profile.user_name.get_or_insert_with(|| {
            let user = std::env::var("USER").unwrap_or_default();
            let mut chars = user.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_else(|| "You".into())
        });
        profile.main_agent_id = Some(self.main_agent_id());
        serde_json::to_value(profile).unwrap_or(Value::Null)
    }

    // --- Agents ---

    /// The agents, oldest first, each a row of the database: one a newer Filo saved (on an engine this build doesn't
    /// know) too, marked (`Agent::newer_engine`), which nothing here changes.
    fn agents(&self) -> Vec<app_data::Agent> {
        self.db().and_then(app_data::agents).unwrap_or_else(|error| {
            eprintln!("filo: {error}");
            Vec::new()
        })
    }

    /// One agent, by id.
    fn agent(&self, id: &str) -> Option<app_data::Agent> {
        self.db().and_then(|db| app_data::agent(db, id)).ok().flatten()
    }

    /// How far each conversation has been read (ms).
    fn seen(&self) -> HashMap<String, f64> {
        self.db().and_then(app_data::seen).unwrap_or_default()
    }

    fn agent_rows(&self) -> Value {
        let seen = self.seen();
        let (input_chats, input_agents) = self.coding_input_owners(now_ms());
        let main = self.main_agent_id();
        let mut agents = self.agents();
        agents.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        let rows: Vec<Value> = agents
            .iter()
            .map(|agent| {
                let messages = agent.session_id.as_deref().and_then(|id| self.agent_chats.shared_messages(id).ok()).unwrap_or_default();
                // What its chat shows (a group message it only checked and stayed quiet on doesn't move it).
                let last = filo_local::store::shown_of(&messages).last().map(|message| {
                    // A routine's run is the routine's name, not the user's words.
                    let routine = routine_run(message);
                    let text = routine.as_ref().map_or_else(|| message.summary(), |routine| format!("Routine · {}", routine.title));
                    // The group's ask is the group's, not the user's words, and from when it was said there, not when
                    // the agent's turn took it up.
                    let call = message.group_call.as_ref();
                    let at = call.and_then(|call| self.group_message_at(call.message_id.as_deref()?)).unwrap_or(message.at as f64);
                    json!({ "fromUser": message.role == Role::User && routine.is_none() && call.is_none() && message.asked_by.is_none() && message.group_link.is_none(), "preview": filo_local::store::preview(&text, PREVIEW_CHARS), "at": at })
                });
                let seen_until = agent.session_id.as_ref().and_then(|id| seen.get(id)).copied().unwrap_or(0.);
                // What it posted in the group is unseen there, not twice.
                let unseen = messages.iter().filter(|message| message.role == Role::Assistant && message.in_group.is_none() && message.at as f64 > seen_until).count();
                // A failure there stands until a turn there goes through (crates/filo-local `turn_state`).
                let last_ok_at = agent.session_id.as_deref().and_then(|id| self.agent_chats.chat(id).ok().flatten()).and_then(|chat| chat.last_ok_at);
                let reply_failed = turn_state::any_failed(&messages, last_ok_at);
                json!({
                    "_id": agent.id,
                    "name": agent.name,
                    "picture": agent.picture,
                    "background": agent.background,
                    "instructions": agent.instructions,
                    "sessionId": agent.session_id,
                    // The model picked for it; none for one from before there was a pick (the app's default).
                    "engine": agent.known_engine(),
                    "model": agent.model,
                    // The engine its conversation is tied to (`locked_engine`): the profile picks among its models only.
                    "lockedEngine": self.locked_engine(agent),
                    "last": last,
                    "unseen": unseen,
                    "replyFailed": reply_failed,
                    "needsInput": input_agents.contains(&agent.id) || agent.session_id.as_ref().is_some_and(|id| input_chats.contains(id)),
                    // The main agent (`users:setMainAgent`).
                    "main": agent.id == main,
                    // Saved by a newer Filo on an engine this one doesn't know: listed as it is, not changed or run.
                    "needsNewerFilo": agent.newer_engine().is_some(),
                })
            })
            .collect();
        Value::Array(rows)
    }

    /// An agent's turns run a coding tool, so making one needs at least one of them plugged in on the Coding page.
    /// Installed isn't enough: unplugged is the user's choice. Existing agents keep working either way.
    fn agent_tool_plugged(&self) -> Result<(), String> {
        let tools = filo_local::coding::tools(self.db()?)?;
        if tools.iter().any(|tool| tool.plugged) { Ok(()) } else { Err(NO_CODING_TOOL.into()) }
    }

    fn create_agent(&self, args: &Value) -> Result<Value, String> {
        self.agent_tool_plugged()?;
        let name = required(args, "name")?.trim().to_string();
        let instructions = required(args, "instructions")?.trim().to_string();
        // The model it answers with from then on; without one, the app's default engine and its model.
        let (engine, model) = model_pick(args)?.unzip();
        let agent = app_data::Agent {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            instructions,
            picture: arg(args, "picture").unwrap_or("sunny").to_string(),
            background: arg(args, "background").map(str::to_string),
            session_id: None,
            engine: engine.map(|engine| engine.key().to_string()),
            model: model.flatten(),
            earlier_chat: None,
            created_at: now_ms() as f64,
        };
        let id = agent.id.clone();
        app_data::add_agent(self.db()?, &agent)?;
        self.changed();
        // The first one made with none there (the first run skipped) is the main agent.
        if self.main_agent().is_none() {
            self.set_main_agent(&id)?;
        }
        Ok(json!(id))
    }

    /// The end of the first run (screens/welcome.rs): the Filo agent fresh data starts with goes, if it was never talked
    /// to, so the user's own first agent is the only one, or the Agents tab shows its empty state. While it was main,
    /// the user's oldest agent is now (none without one, until `create_agent` makes one). Whether it went.
    fn remove_stand_in(&self) -> Result<Value, String> {
        let untouched = |agent: &app_data::Agent| agent.session_id.is_none() && agent.earlier_chat.is_none();
        let removed = app_data::remove_agent_if(self.db()?, DEFAULT_AGENT_ID, untouched)?;
        if !removed {
            return Ok(json!(false));
        }
        self.changed();
        if self.main_agent().is_none() {
            let oldest =
                self.agents().into_iter().min_by(|a, b| a.created_at.total_cmp(&b.created_at)).map(|agent| agent.id);
            app_data::set_profile(self.db()?, app_data::MAIN_AGENT, oldest.map(Value::String).as_ref())?;
        }
        self.group_agent_removed(DEFAULT_AGENT_ID);
        self.scheduled_agent_removed(DEFAULT_AGENT_ID, None);
        Ok(json!(true))
    }

    fn update_agent(&self, args: &Value) -> Result<Value, String> {
        let id = required(args, "id")?.to_string();
        let picked = model_pick(args)?;
        // Another model of its engine, yes; another engine, no: its conversation's session stays with the CLI it's in.
        let current = self.agent(&id).ok_or("This agent no longer exists.")?;
        current.refuse_newer()?;
        if let (Some((engine, _)), Some(locked)) = (&picked, self.locked_engine(&current))
            && *engine != locked
        {
            return Err(format!(
                "{} answers with {}. To use {}, duplicate it with {} instead.",
                current.name,
                locked.label(),
                engine.label(),
                engine.label()
            ));
        }
        let change = app_data::AgentChange {
            name: arg(args, "name").map(str::trim).filter(|name| !name.is_empty()).map(str::to_string),
            instructions: arg(args, "instructions").map(str::trim).filter(|text| !text.is_empty()).map(str::to_string),
            picture: arg(args, "picture").map(str::to_string),
            // `null` resets the colour to the theme's.
            background: args.get("background").map(|_| arg(args, "background").map(str::to_string)),
            // Its next turns, here and in the group, answer with it.
            model: picked.map(|(engine, model)| (engine.key().to_string(), model)),
        };
        let updated = app_data::update_agent(self.db()?, &id, &change)?.ok_or("This agent no longer exists.")?;
        self.changed();
        // Its conversation answers as the agent it is now, and is named after it.
        if let Some(session) = &updated.session_id {
            let _ = self.agent_chats.update_chat(session, |chat| {
                chat.title = updated.name.clone();
                chat.instructions = Some(self.agent_persona(&updated));
            });
        }
        Ok(Value::Null)
    }

    /// Who the engine is in the agent's conversation, pointing to the earlier one it continues, if any.
    fn agent_persona(&self, agent: &app_data::Agent) -> String {
        let earlier = agent
            .earlier_chat
            .as_ref()
            .map(|chat| self.agent_chats.root().join("chats").join(chat).join("messages.jsonl"));
        persona(agent, earlier.as_deref())
    }

    /// The agent again on another engine (`agents:duplicate`), for when its own can't go on (a usage limit): its
    /// name, instructions, picture and apps, a new conversation that can look back at the old one, and the old agent
    /// as it was. Its reminders and routines stay with the old one.
    fn duplicate_agent(&self, args: &Value) -> Result<Value, String> {
        let id = required(args, "id")?;
        let source = self.agent(id).ok_or("This agent no longer exists.")?;
        let (engine, model) = model_pick(args)?.ok_or("Pick the engine for the copy.")?;
        let agent = app_data::Agent {
            id: uuid::Uuid::new_v4().to_string(),
            name: format!("{} ({})", source.name, engine.label()),
            instructions: source.instructions.clone(),
            picture: source.picture.clone(),
            background: source.background.clone(),
            session_id: None,
            engine: Some(engine.key().to_string()),
            model,
            earlier_chat: source.session_id.clone(),
            created_at: now_ms() as f64,
        };
        let id = agent.id.clone();
        app_data::add_agent(self.db()?, &agent)?;
        // The apps the old one may use, the copy may too.
        if let Ok(db) = self.db() {
            for app in filo_local::apps::list(db).unwrap_or_default() {
                if app.agent_scope == filo_local::apps::SOME_AGENTS && app.agents.contains(&source.id) {
                    let agents: Vec<String> = app.agents.iter().cloned().chain(std::iter::once(id.clone())).collect();
                    let _ = filo_local::apps::set_agent_access(db, &app.id, false, &agents, now_ms());
                }
            }
        }
        self.changed();
        Ok(json!(id))
    }

    fn remove_agent(&self, args: &Value) -> Result<Value, String> {
        let id = required(args, "id")?.to_string();
        // The main agent goes only once another is main (the Agents tab asks for one first).
        if id == self.main_agent_id() {
            return Err(MAIN_AGENT_STAYS.into());
        }
        self.agent(&id).ok_or("This agent no longer exists.")?.refuse_newer()?;
        let removed = app_data::remove_agent(self.db()?, &id)?.ok_or("This agent no longer exists.")?;
        if let Some(session) = removed.session_id {
            self.stop_session(&session);
            let _ = self.agent_chats.delete_chat(&session);
        }
        self.group_agent_removed(&id);
        self.scheduled_agent_removed(&id, arg(args, "scheduled"));
        self.changed();
        Ok(Value::Null)
    }

    /// The engine an agent is tied to once its conversation has a session: what that session knows can't go to the other
    /// CLI, and each switch would lose it. The one picked for it, else (one from before there was a pick) the session's.
    /// None before the first turn, while it may still take either.
    fn locked_engine(&self, agent: &app_data::Agent) -> Option<Engine> {
        let started = self.agent_chats.chat(agent.session_id.as_deref()?).ok().flatten()?.session?;
        Some(agent.known_engine().unwrap_or(started.engine))
    }

    /// The engine and model its turns run with, in its own chat and the group: as `model_choice`, except that one from
    /// before there was a pick stays on the engine its conversation started with rather than following the app's.
    fn agent_model(&self, agent: &app_data::Agent) -> (Engine, Option<String>) {
        match (agent.known_engine(), self.locked_engine(agent)) {
            (None, Some(engine)) => (engine, None),
            _ => model_choice(agent),
        }
    }

    /// The agent whose conversation `session_id` is, when it's one of theirs.
    fn session_agent(&self, store: &Store, session_id: &str) -> Option<app_data::Agent> {
        if store.root() != self.agent_chats.root() {
            return None;
        }
        self.db().and_then(|db| app_data::agent_of_chat(db, session_id)).ok().flatten()
    }

    /// The agent's conversation, made the first time.
    fn open_agent_chat(&self, id: &str) -> Result<Value, String> {
        let agent = self.agent(id).ok_or("This agent no longer exists.")?;
        if let Some(session) =
            agent.session_id.clone().filter(|session| self.agent_chats.chat(session).ok().flatten().is_some())
        {
            return Ok(json!(session));
        }
        agent.refuse_newer()?;
        let chat = self
            .agent_chats
            .create_chat(default_engine())
            .map_err(|error| format!("Couldn't create the conversation: {error}"))?;
        let mut record = agent.clone();
        record.session_id = Some(chat.id.clone());
        let _ = self.agent_chats.update_chat(&chat.id, |chat| {
            chat.title = record.name.clone();
            chat.instructions = Some(self.agent_persona(&record));
        });
        app_data::set_agent_session(self.db()?, id, &chat.id)?;
        self.changed();
        Ok(json!(chat.id))
    }

    // --- Conversations ---

    /// The reasoning levels a conversation's next turn's model takes, least first (`chat_efforts`): the agent's model in
    /// an agent's, else the one picked for it, else the app's default engine's. None when they aren't known.
    fn chat_reasoning_levels(&self, store: &Store, chat: &filo_local::store::Chat) -> Vec<String> {
        let (engine, model) = match self.session_agent(store, &chat.id) {
            Some(agent) => self.agent_model(&agent),
            None if chat.engine_chosen => (chat.engine, chat.model.clone()),
            None => (default_engine(), None),
        };
        self.chat_efforts(engine, model.as_deref())
    }

    /// `chat_reasoning_levels` for the conversation `session_id`; none for one that isn't here.
    pub(crate) fn session_reasoning_levels(&self, session_id: &str) -> Vec<String> {
        let Some(store) = self.store_of(session_id) else { return Vec::new() };
        store.chat(session_id).ok().flatten().map(|chat| self.chat_reasoning_levels(store, &chat)).unwrap_or_default()
    }

    fn detail(&self, args: &Value) -> Result<Value, String> {
        let id = required(args, "id")?;
        let Some(store) = self.store_of(id) else { return Ok(Value::Null) };
        let chat = store.chat(id).ok().flatten().ok_or("This conversation no longer exists.")?;
        let limit = limit(args, DEFAULT_LIMIT, usize::MAX);
        // What Filo's app told the engine on its own (a coding task's question or round) isn't the chat's, nor a group
        // message it only checked and stayed quiet on.
        let all = store.shared_messages(id).unwrap_or_default();
        let shown = filo_local::store::shown_of(&all);
        let has_more = shown.len() > limit;
        let mut window: Vec<Message> =
            shown[shown.len().saturating_sub(limit)..].iter().map(|message| (*message).clone()).collect();
        // An agent's files where they are now, found again when they were renamed or moved.
        store.find_moved_files(id, &mut window);
        let window = &window[..];
        // A reply that reports a coding round carries its Open proof (local_backend/coding.rs).
        let proof = self.coding_results(window);
        let reactions = store.reactions(id);
        let posts = self.posts_in_group(window);
        let mut messages: Vec<Value> = window
            .iter()
            .map(|message| {
                let mut row = json!({
                    "_id": message.id,
                    "_creationTime": message.at as f64,
                    "role": if message.role == Role::User { "user" } else { "assistant" },
                    "content": message.text,
                    // A line pointing at the group chat (local_backend/group.rs).
                    "groupLink": message.group_link,
                    "attachments": attachments_json(&message.attachments),
                    "replyToMessageId": message.reply_to,
                    // Which turn it's of: a coding round's result is an answer of its own (conversation.rs `apart`).
                    "turn": message.turn,
                    // The user's React (`react`).
                    "reaction": reactions.get(&message.id),
                });
                // What it posted in the group (local_backend/group.rs): the post itself, as the group shows it (its
                // words, files, Open proof and glance), with a label linking to it. The line's own text is only for
                // the engine, and stands in for a post that's gone.
                if let Some(posted) = &message.in_group {
                    row["groupLink"] = json!({ "kind": "posted", "who": [], "messageId": posted });
                    if let Some(post) = posts.get(posted) {
                        for key in ["content", "attachments", "codingResult", "glance", "report"] {
                            row[key] = post.get(key).cloned().unwrap_or(Value::Null);
                        }
                    }
                }
                // Another agent asked it from its chat (local_backend/group.rs `run_agent_asks`): the ask, with a chip
                // opening the asker's chat.
                if let Some(asked) = &message.asked_by {
                    row["content"] = json!(asked.ask);
                    row["askedBy"] = json!(asked);
                }
                // Where it asked another agent, or what that agent answered: the ask's state and how long it took,
                // opening the other chat at the ask or the answer.
                if let Some(ask) = message
                    .group_link
                    .as_ref()
                    .and_then(|link| link.ask.as_deref())
                    .and_then(|id| self.db().ok().and_then(|db| filo_local::agent_asks::get(db, id).ok().flatten()))
                {
                    let state = match ask.state.as_str() {
                        "queued" | "running" => "working",
                        state => state,
                    };
                    row["groupLink"]["state"] = json!(state);
                    if let (Some(started), Some(finished)) = (ask.started_at, ask.finished_at) {
                        row["groupLink"]["workedMs"] = json!((finished - started).max(0));
                    }
                    if row["groupLink"]["messageId"].is_null() {
                        row["groupLink"]["messageId"] = json!(ask.message_id);
                    }
                }
                // A turn the group gave it (local_backend/group.rs): a card with the ask and the group's latest messages.
                if let Some(call) = &message.group_call {
                    row["content"] = json!(call.ask);
                    row["groupCall"] = json!(call);
                }
                if let Some(result) = proof.get(&message.id).filter(|_| message.in_group.is_none()) {
                    row["codingResult"] = result.clone();
                }
                // A voice message: what was said is its text, and its recording plays on the bubble (local_backend/voice.rs).
                if let Some(voice) = &message.voice {
                    row["voiceRecording"] = self.voice_json(voice);
                }
                // A routine's run: a line in the chat, not its instructions; quiet once it ended with nothing sent.
                if let Some(routine) = routine_run(message) {
                    let turn = message.turn.as_deref();
                    let replied = turn.is_some()
                        && shown.iter().any(|reply| reply.role == Role::Assistant && reply.turn.as_deref() == turn);
                    let running = turn.is_some_and(|turn| self.running.lock().unwrap().contains_key(turn));
                    row["routine"] =
                        json!({ "title": routine.title, "when": routine.when, "quiet": !replied && !running });
                }
                row
            })
            .collect();
        // The glances Filo showed here, as cards above its replies (local_backend/glances.rs).
        self.with_glances(id, &mut messages);
        let seen_until = self.db().and_then(|db| app_data::seen_until(db, id)).ok().flatten();
        // The engine and model the user picked here; none follows the app's default engine.
        let reasoning_levels = self.chat_reasoning_levels(store, &chat);
        let (engine, model) = if chat.engine_chosen { (Some(chat.engine), chat.model) } else { (None, None) };
        Ok(json!({
            "session": {
                "_id": chat.id,
                "title": title_of(&chat.title),
                "seenUntil": seen_until,
                "engine": engine,
                "model": model,
                // The composer's reasoning picker, as the user left it here.
                "reasoning": chat.reasoning,
                "reasoningLevels": reasoning_levels,
            },
            "messages": messages,
            "hasMore": has_more,
            // Artifacts its turns made, changed or opened, as finished-work cards (local_backend/artifacts.rs).
            "tasks": self.chat_tasks(id),
        }))
    }

    /// The conversation's turns, read from its messages: one per user message's turn. One that failed is "settled" once
    /// a turn here went through after it (crates/filo-local `turn_state`); one that failed on a usage limit says when
    /// it's tried again on its own (`retryAt`, local_backend/limit_retry.rs).
    fn chat_turns(&self, session_id: &str) -> Value {
        let Some(store) = self.store_of(session_id) else { return json!([]) };
        let messages = store.shared_messages(session_id).unwrap_or_default();
        let chat = store.chat(session_id).ok().flatten();
        let compacted = chat.as_ref().map(|chat| chat.compacted_turns.clone()).unwrap_or_default();
        let reseeded = chat.as_ref().map(|chat| chat.reseeded_turns.clone()).unwrap_or_default();
        let ends = turn_state::ends(&messages, chat.and_then(|chat| chat.last_ok_at));
        let (retried, now) = (self.limit_retried(), now_ms());
        let running = self.running.lock().unwrap();
        let turns: Vec<Value> = ends[ends.len().saturating_sub(30)..]
            .iter()
            .map(|(turn, end)| {
                let of_turn: Vec<&Message> = messages.iter().filter(|message| message.turn.as_deref() == Some(turn)).collect();
                let user = of_turn.iter().find(|message| message.role == Role::User);
                let created_at = user.map_or(0, |message| message.at);
                let user = user.map(|message| message.id.clone());
                let (state, error) = match end {
                    _ if running.contains_key(*turn) => ("pending", None),
                    turn_state::End::Failed(error) => ("failed", Some(*error)),
                    turn_state::End::Settled(error) => ("settled", Some(*error)),
                    turn_state::End::Done => ("completed", None),
                };
                let retry_at = error.filter(|_| state == "failed").and_then(|error| self.limit_retry_at(&retried, turn, &error.text, error.at as i64, now));
                // What it's doing while it runs ("Running a command"), as its worker saves it.
                let activity = (state == "pending").then(|| chat_activity::get(store, session_id, turn)).flatten();
                // The CLI summarized the session during it (`Chat::compacted_turns`): the chat marks where.
                let compacted = compacted.iter().any(|id| id == turn);
                // It started a fresh session, the one before having gone cold (`Chat::reseeded_turns`).
                let reseeded = reseeded.iter().any(|id| id == turn);
                let error = error.map(|error| error.text.clone());
                json!({ "turnId": turn, "requestId": turn, "createdAt": created_at, "userMessageId": user, "state": state, "error": error, "retryAt": retry_at, "activity": activity, "compacted": compacted, "reseeded": reseeded })
            })
            .collect();
        Value::Array(turns)
    }

    /// Keeps a file the composer uploads (attachments.rs `upload`) for the message it goes with; its storage id.
    pub fn save_attachment(&self, content_type: &str, bytes: &[u8]) -> Result<String, String> {
        if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err("Attachments are limited to 25 MB.".into());
        }
        filo_local::attachments::save(&self.root, content_type, bytes)
            .map_err(|error| format!("Couldn't save the file: {error}"))
    }

    /// Keeps the file at `path` the composer attaches (attachments.rs `upload`) without reading it, of any size: an
    /// APFS clone, else a link to it (`link`, a file of the user's) or a copy; its storage id.
    pub fn keep_attachment(&self, content_type: &str, path: &std::path::Path, link: bool) -> Result<String, String> {
        filo_local::attachments::keep(&self.root, path, content_type, link)
            .map_err(|error| format!("Couldn't keep the file: {error}"))
    }

    /// The files a message comes with (`SentAttachment`s), each one this Mac saved when it was uploaded.
    pub(crate) fn sent_files(&self, args: &Value) -> Result<Vec<filo_local::store::Attachment>, String> {
        let Some(files) = args.get("attachments").and_then(Value::as_array) else { return Ok(Vec::new()) };
        if files.len() > MAX_ATTACHMENTS {
            return Err("Up to 5 files per message.".into());
        }
        files
            .iter()
            .map(|file| {
                let storage_id = required(file, "storageId")?;
                let saved = filo_local::attachments::file(&self.root, storage_id)
                    .ok_or("A file of this message is no longer on this Mac. Attach it again.")?;
                let size = std::fs::metadata(&saved).map(|meta| meta.len()).unwrap_or(0);
                Ok(filo_local::store::Attachment {
                    storage_id: storage_id.to_string(),
                    name: arg(file, "name").filter(|name| !name.trim().is_empty()).unwrap_or("file").to_string(),
                    content_type: arg(file, "contentType").unwrap_or("application/octet-stream").to_string(),
                    size,
                    file: saved,
                    local_path: arg(file, "localPath").map(str::to_string),
                    bookmark: None,
                })
            })
            .collect()
    }

    fn send_message(self: &Arc<Self>, args: &Value) -> Result<Value, String> {
        let session_id = required(args, "sessionId")?.to_string();
        let text = arg(args, "message").unwrap_or_default().trim().to_string();
        let files = self.sent_files(args)?;
        if text.is_empty() && files.is_empty() {
            return Err("The message is empty.".into());
        }
        let turn_id = arg(args, "requestId")
            .filter(|id| valid_id(id))
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.user_turn(&session_id, &turn_id, &text, files, None, args)?;
        self.changed();
        Ok(json!({ "turnId": turn_id, "sessionId": session_id }))
    }

    /// The user's message starting turn `turn_id` in `session_id` (an agent's chat or Filo's), typed or said (`voice`):
    /// saved with its files, what it answers (`replyToMessageId`, quoted at its top) and what it carries for the engine
    /// (`message_context`), and its turn started. Every message the user sends to a chat comes this way, so an agent
    /// saved by a newer Filo takes none (`refuse_newer`). Sent again with the same turn (a retried send), it's the
    /// message the first try made.
    pub(crate) fn user_turn(
        self: &Arc<Self>,
        session_id: &str,
        turn_id: &str,
        text: &str,
        files: Vec<filo_local::store::Attachment>,
        voice: Option<filo_local::store::Voice>,
        args: &Value,
    ) -> Result<Message, String> {
        let store = self.store_of(session_id).ok_or("This conversation no longer exists.")?.clone();
        if let Some(agent) = self.session_agent(&store, session_id) {
            agent.refuse_newer()?;
        }
        // A reasoning level picked in the composer comes with the message (an agent's chat answers with the agent's
        // own model, `start_turn_then`).
        if let Some(level) = arg(args, "reasoning") {
            choose_reasoning(&store, session_id, level)?;
        }
        let context = self.message_context(args)?;
        let messages = store.messages(session_id).unwrap_or_default();
        if let Some(sent) = messages.iter().find(|message| message.turn.as_deref() == Some(turn_id)) {
            return Ok(sent.clone());
        }
        // A reply (the chat's Reply) has what it answers quoted at its top.
        let reply_to = arg(args, "replyToMessageId").and_then(|id| {
            messages.iter().find(|message| message.id == id && message.role != Role::Error && !message.hidden)
        });
        let message = store
            .add_user_voice(session_id, text, turn_id, files, reply_to, voice)
            .map_err(|error| format!("Couldn't save the message: {error}"))?;
        if let Some(context) = &context {
            turn::save_context(&store, session_id, turn_id, context)
                .map_err(|error| format!("Couldn't save the message's context: {error}"))?;
        }
        self.start_turn(store, session_id.to_string(), turn_id.to_string());
        Ok(message)
    }

    /// What a message carries for the engine and not the chat, for its turn's prompt (turn.rs `save_context`): the Feed
    /// posts it's about (`contextNote`), the notes shared into it from the Notes tab (`sharedNotes`: each one's title and
    /// text) and the running tasks it points at (`sharedWork`: dragged from the Working panel or picked after "@").
    /// None when it carries none.
    pub(crate) fn message_context(&self, args: &Value) -> Result<Option<String>, String> {
        let items = |key: &str| args.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let mut parts: Vec<String> = arg(args, "contextNote")
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(str::to_string)
            .into_iter()
            .collect();
        let notes = items("sharedNotes");
        if notes.len() > MAX_SHARED_NOTES {
            return Err("Up to 3 notes go with one message.".into());
        }
        if notes.iter().map(|note| arg(note, "body").unwrap_or_default().trim().chars().count()).sum::<usize>()
            > MAX_SHARED_NOTE_CHARS
        {
            return Err("These notes are too long to go with one message together. Remove one first.".into());
        }
        for note in &notes {
            // A note only on the Mac it was written on has no id.
            let heading = match arg(note, "id").filter(|id| !id.trim().is_empty()) {
                Some(id) => {
                    format!("A note the user shared from their Notes (id {id}, for read_note and update_note):")
                }
                None => "A note the user shared from their Notes:".to_string(),
            };
            let title = arg(note, "title").map(str::trim).filter(|title| !title.is_empty()).unwrap_or("Untitled note");
            parts.push(format!("{heading}\nTitle: {title}\n\n{}", arg(note, "body").unwrap_or_default().trim()));
        }
        let work = items("sharedWork");
        if work.len() > MAX_SHARED_WORK {
            return Err("Up to 5 tasks go with one message.".into());
        }
        for task in &work {
            let id = required(task, "id")?;
            parts.push(match arg(task, "kind") {
                Some("coding") => match filo_local::coding::task(self.db()?, id)? {
                    Some(task) => format!(
                        "A coding task the user points at (id {id}, for watch_coding_task and tell_coding_task): \u{201c}{}\u{201d}, {} in {}, {}.",
                        task.title,
                        filo_local::coding_handoff::tool_name(&task.tool),
                        task.project_name,
                        task.status
                    ),
                    None => format!("A coding task the user points at (id {id}): it isn't on this Mac any more."),
                },
                _ => return Err("A task the message points at isn't one Filo knows.".into()),
            });
        }
        Ok((!parts.is_empty()).then(|| parts.join("\n\n")))
    }

    /// The chat's React on a message of an agent's chat: kept beside the chat's messages (`Store::reactions`),
    /// or with none (`reaction` left out) taken back.
    fn react(&self, args: &Value) -> Result<Value, String> {
        let message_id = required(args, "messageId")?;
        let reaction = arg(args, "reaction").map(str::trim).filter(|reaction| !reaction.is_empty());
        let chat = required(args, "sessionId")?;
        let has =
            |store: &&Store| store.messages(chat).unwrap_or_default().iter().any(|message| message.id == message_id);
        let store = self.store_of(chat).filter(has).ok_or("This message no longer exists.")?;
        store
            .set_reaction(chat, message_id, reaction)
            .map_err(|error| format!("Couldn't save the reaction: {error}"))?;
        self.changed();
        Ok(Value::Null)
    }

    /// Retry (`voiceSessions:retryChatTurn`): the failed turn `turnId` of the conversation `sessionId` runs again,
    /// unless it's running already. A member's chat retrying its group turn goes to the group (group.rs `group_call`).
    fn dispatch_turn(self: &Arc<Self>, args: &Value) -> Result<Value, String> {
        let turn_id = required(args, "turnId")?.to_string();
        let session_id = required(args, "sessionId")?.to_string();
        let store = self.store_of(&session_id).ok_or("This conversation no longer exists.")?.clone();
        if !self.running.lock().unwrap().contains_key(&turn_id) {
            self.start_turn(store, session_id, turn_id.clone());
        }
        self.changed();
        Ok(json!({ "turnId": turn_id }))
    }

    fn mark_seen(&self, args: &Value) -> Result<Value, String> {
        let session_id = required(args, "sessionId")?.to_string();
        let until = arg(args, "messageId")
            .and_then(|message_id| {
                let Some(store) = self.store_of(&session_id) else { return self.group_message_at(message_id) };
                store
                    .messages(&session_id)
                    .ok()?
                    .into_iter()
                    .find(|message| message.id == message_id)
                    .map(|message| message.at as f64)
            })
            .unwrap_or_else(|| now_ms() as f64);
        app_data::mark_seen(self.db()?, &session_id, until)?;
        self.changed();
        Ok(Value::Null)
    }

    /// Replies not seen yet, per conversation (the agents' and the group). Once the newest has waited `PUSH_AFTER`
    /// unseen, `pushedUntil` is its time: the app notifies about it (unseen.rs, companion.rs). Until then the live
    /// queries are woken for that moment.
    fn unseen(self: &Arc<Self>) -> Value {
        let seen = self.seen();
        let agents: HashMap<String, app_data::Agent> =
            self.agents().into_iter().filter_map(|agent| Some((agent.session_id.clone()?, agent))).collect();
        let mut chats = Vec::new();
        for row in self.agent_chats.list_chats() {
            let id = &row.chat.id;
            let seen_until = seen.get(id).copied().unwrap_or(0.);
            let messages = self.agent_chats.shared_messages(id).unwrap_or_default();
            let unseen: Vec<&Message> = messages
                .iter()
                .filter(|message| {
                    message.role == Role::Assistant && message.in_group.is_none() && message.at as f64 > seen_until
                })
                .collect();
            let Some(latest) = unseen.last() else { continue };
            let agent = agents.get(id);
            // An agent's chat goes by the agent's name now, renamed or not.
            let title = agent.map(|agent| agent.name.clone()).unwrap_or_else(|| row.chat.title.clone());
            let mut chat = unseen_row(id, &title, unseen.len(), seen_until, latest.at as f64, &latest.summary());
            chat["agentId"] = json!(agent.map(|agent| &agent.id));
            chats.push(chat);
        }
        chats.extend(self.group_unseen(&seen));
        let now = now_ms() as f64;
        for chat in &mut chats {
            let Some(latest) = chat["latestAt"].as_f64() else { continue };
            if now - latest >= PUSH_AFTER {
                chat["pushedUntil"] = json!(latest);
            } else {
                self.wake_at((latest + PUSH_AFTER) as i64);
            }
        }
        let total: u64 = chats.iter().filter_map(|chat| chat["count"].as_u64()).sum();
        json!({ "total": total, "chats": chats })
    }

    /// The live queries run again at `at` (ms), once however many ask.
    fn wake_at(self: &Arc<Self>, at: i64) {
        if !self.push_wakes.lock().unwrap().insert(at) {
            return;
        }
        let local = Arc::downgrade(self);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis((at - now_ms()).max(0) as u64));
            if let Some(local) = local.upgrade() {
                local.push_wakes.lock().unwrap().remove(&at);
                local.changed();
            }
        });
    }

    /// Conversations with a turn running, and those `include` names (the chats coding tasks work for,
    /// `coding:workingChats`) while their tasks work.
    fn working(&self, args: &Value) -> Value {
        let agents: HashMap<String, app_data::Agent> =
            self.agents().into_iter().filter_map(|agent| Some((agent.session_id.clone()?, agent))).collect();
        // An agent's chat goes by the agent's name now, renamed or not.
        let chat_row = |id: &str, chat: &filo_local::store::Chat, started: f64| {
            let agent = agents.get(id);
            let title = agent.map_or(chat.title.as_str(), |agent| agent.name.as_str());
            json!({ "_id": id, "title": title_of(title), "workStartedAt": started, "agentId": agent.map(|agent| &agent.id) })
        };
        let group = self.group_session();
        let running = self.running.lock().unwrap();
        let mut since: HashMap<String, f64> = HashMap::new();
        for (key, turn) in running.iter() {
            // The group works while a member prepares its post, not while that member's turn carries on after it.
            if group.as_deref() == Some(turn.session_id.as_str()) && self.posted_in_group(key) {
                continue;
            }
            let entry = since.entry(turn.session_id.clone()).or_insert(turn.started_at);
            *entry = entry.min(turn.started_at);
        }
        let rows: Vec<Value> = since
            .into_iter()
            .filter_map(|(id, started)| {
                let Some(store) = self.store_of(&id) else { return self.group_working(&id, started) };
                let chat = store.chat(&id).ok()??;
                Some(chat_row(&id, &chat, started))
            })
            .collect();
        let mut rows = rows;
        let coding = self.coding_working_since();
        for id in args.get("include").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if rows.iter().any(|row| row["_id"] == id) {
                continue;
            }
            let Some(started) = coding.get(id).copied() else { continue };
            let row = match self.store_of(id) {
                Some(store) => store.chat(id).ok().flatten().map(|chat| chat_row(id, &chat, started)),
                None => self.group_working(id, started),
            };
            rows.extend(row);
        }
        Value::Array(rows)
    }

    // --- Turns ---

    /// Runs a turn in a worker process (local_backend/workers.rs), else on a thread of its own; its replies and how it
    /// ended land in the conversation's file.
    fn start_turn(self: &Arc<Self>, store: Store, session_id: String, turn_id: String) {
        #[cfg(test)]
        if self.no_turns.load(Ordering::SeqCst) {
            return;
        }
        if self.detaches() {
            self.set_turn_model(&store, &session_id);
            // Counted before the worker starts, so the loop doesn't take it for one another process started.
            let running = Running {
                session_id: session_id.clone(),
                started_at: now_ms() as f64,
                detached: true,
                ..Running::default()
            };
            let stop = running.stop.clone();
            self.running.lock().unwrap().insert(turn_id.clone(), running);
            if let Some(worker) =
                self.start_worker(workers::Job::Turn { session: session_id.clone(), turn: turn_id.clone() }, &stop)
            {
                return self.wait_for_turn(worker, turn_id);
            }
            self.running.lock().unwrap().remove(&turn_id);
        }
        self.start_turn_then(store, session_id, turn_id, |_| {});
    }

    /// Work runs in worker processes: in the app (and a terminal check), not in tests or a worker itself.
    fn detaches(&self) -> bool {
        !cfg!(test) && !self.in_worker.load(Ordering::SeqCst)
    }

    /// A worker's turn this app didn't start (a previous run of the app, a terminal check): running here too, so the chat
    /// shows it working and Stop reaches it. False when it's known here already.
    fn track_turn(self: &Arc<Self>, worker: workers::Worker, session_id: String, turn_id: String) -> bool {
        {
            let mut running = self.running.lock().unwrap();
            if running.contains_key(&turn_id) {
                return false;
            }
            running.insert(
                turn_id.clone(),
                Running {
                    session_id,
                    started_at: worker.started_at,
                    stop: Stoppable::of(worker.stopper()),
                    detached: true,
                },
            );
        }
        self.wait_for_turn(worker, turn_id);
        true
    }

    /// The turn counts as running until its worker ends: the chat shows it working, and its replies as they come.
    fn wait_for_turn(self: &Arc<Self>, worker: workers::Worker, turn_id: String) {
        self.watch_turns();
        self.changed();
        let local = self.clone();
        std::thread::spawn(move || {
            worker.wait();
            local.running.lock().unwrap().remove(&turn_id);
            local.changed();
        });
    }

    /// Runs a turn here, blocking until it ends (in a worker, or on the app's thread).
    fn run_turn_here(
        self: &Arc<Self>,
        store: &Store,
        session_id: &str,
        turn_id: &str,
        stop: &Stoppable,
    ) -> turn::Report {
        // The Mac stays awake meanwhile, as for a coding task (local_backend/awake.rs); a routine's run is a turn too.
        let title =
            store.chat(session_id).ok().flatten().map(|chat| chat.title).filter(|title| !title.trim().is_empty());
        let _awake = self.keep_awake(title.map_or_else(|| "reply".into(), |title| format!("reply: {title}")));
        // Who speaks, for the user's apps it may use: the agent whose conversation it is, else Filo (local_backend/apps.rs).
        let agent = self.turn_agent(store, session_id);
        let runtime = Runtime { agent: Some(agent), ..runtime(self.root.join(filo_local::db::FILE)) };
        let report = turn::run(store, &runtime, session_id, turn_id, |started| stop.started(started));
        if let Some(error) = &report.error {
            let chat = store.chat(session_id).ok().flatten();
            self.note_failure(
                "turn",
                turn_id,
                chat.as_ref().map(|chat| chat.engine),
                chat.and_then(|chat| chat.model),
                error,
                report.stderr.clone(),
            );
        }
        report
    }

    /// Keeps a failed turn or coding round with the CLI's stderr, for a problem report (report.rs).
    pub(crate) fn note_failure(
        &self,
        kind: &str,
        id: &str,
        engine: Option<Engine>,
        model: Option<String>,
        error: &str,
        stderr: Vec<String>,
    ) {
        let failure = filo_local::failures::Failure {
            at: now_ms() as u64,
            kind: kind.into(),
            id: id.into(),
            engine,
            model,
            error: error.into(),
            stderr,
        };
        filo_local::failures::record(&self.root, failure);
    }

    /// Who answers the conversation's next turn, and how hard it thinks. An agent's conversation answers with the model
    /// picked for the agent, every turn. Elsewhere the engine the user picked in the composer answers, else the app's
    /// default. The reasoning level picked in the composer becomes one that model takes
    /// (`chat_effort`).
    fn set_turn_model(&self, store: &Store, session_id: &str) {
        let agent_model = self
            .session_agent(store, session_id)
            .map(|agent| (agent.known_engine().is_some(), self.agent_model(&agent)));
        let engine = default_engine();
        let _ = store.update_chat(session_id, |chat| {
            match agent_model {
                Some((picked, (engine, model))) => {
                    chat.engine = engine;
                    chat.model = model;
                    chat.engine_chosen = picked;
                }
                None if !chat.engine_chosen => chat.engine = engine,
                None => {}
            }
            chat.effort =
                chat.reasoning.as_deref().and_then(|level| self.chat_effort(chat.engine, chat.model.as_deref(), level));
        });
    }

    /// A turn on a thread of the app's own, then `done` with how it went (a routine's run records it). Quitting stops it,
    /// whatever "Keep coding after Filo quits" says.
    fn start_turn_then(
        self: &Arc<Self>,
        store: Store,
        session_id: String,
        turn_id: String,
        done: impl FnOnce(&turn::Report) + Send + 'static,
    ) {
        self.set_turn_model(&store, &session_id);
        let running = Running { session_id: session_id.clone(), started_at: now_ms() as f64, ..Running::default() };
        let stop = running.stop.clone();
        self.running.lock().unwrap().insert(turn_id.clone(), running);
        self.watch_turns();
        let local = self.clone();
        std::thread::spawn(move || {
            let report = local.run_turn_here(&store, &session_id, &turn_id, &stop);
            local.running.lock().unwrap().remove(&turn_id);
            local.changed();
            done(&report);
        });
    }

    fn stop_session(&self, session_id: &str) {
        for running in self.running.lock().unwrap().values().filter(|running| running.session_id == session_id) {
            running.stop.stop();
        }
    }

    /// Filo quits: the work running in the app stops. Turns and coding rounds in workers carry on while "Keep coding
    /// after Filo quits" is on (and when the updater restarts Filo); off, they stop as Stop would stop them.
    pub fn quit(&self) {
        let keep = workers::is_restarting() || self.db().and_then(filo_local::coding::keep_on_quit).unwrap_or(true);
        eprintln!("quit: turns and coding tasks in workers {}", if keep { "keep running" } else { "stop" });
        self.stop_coding(keep);
        self.halt_group();
        self.stop_app_work();
        for running in self.running.lock().unwrap().values().filter(|running| !(keep && running.detached)) {
            running.stop.stop();
        }
    }

    /// Workers this app didn't start, left running by a previous run of it (Filo quit with "Keep coding after Filo quits"
    /// on, or restarted) or started by another process (a terminal check): their turns and rounds count as running here.
    /// Returns the tasks whose rounds run in workers, so they aren't marked stopped.
    fn adopt_workers(self: &Arc<Self>) -> Vec<String> {
        let mut tasks = Vec::new();
        for worker in workers::running(&self.root) {
            let (job, pid) = (worker.job.clone(), worker.pid);
            let adopted = match job.clone() {
                workers::Job::Turn { session, turn } => self.track_turn(worker, session, turn),
                workers::Job::Round { task } => {
                    tasks.push(task.clone());
                    self.track_round(worker, task)
                }
            };
            if adopted {
                eprintln!("workers: picking up {job:?} (pid {pid})");
            }
        }
        tasks
    }

    /// While turns run: their files growing (a reply from the MCP server's process) and the database changing (a memory
    /// it saved) tell the live queries.
    fn watch_turns(self: &Arc<Self>) {
        if self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        let local = self.clone();
        std::thread::spawn(move || {
            // Each chat's messages file size and what its turn is doing (chat_activity.rs).
            let mut sizes: HashMap<String, (u64, Option<Vec<u8>>)> = HashMap::new();
            let data_version = || local.db.as_ref().ok().and_then(|db| db.data_version().ok());
            let mut db_version = data_version();
            // A skill an agent installs or changes in its folder shows on the Skills page as it lands.
            let mut skills_version = skills::version(&local.skills_dir());
            loop {
                std::thread::sleep(WATCH_EVERY);
                let sessions: Vec<String> =
                    local.running.lock().unwrap().values().map(|running| running.session_id.clone()).collect();
                // Coding rounds in workers save their steps to the database: watched too.
                let idle = || local.running.lock().unwrap().is_empty() && !local.coding_rounds_running();
                if sessions.is_empty() && idle() {
                    local.watching.store(false, Ordering::SeqCst);
                    // A turn started while this was stopping.
                    if idle() || local.watching.swap(true, Ordering::SeqCst) {
                        return;
                    }
                    continue;
                }
                let mut grew = false;
                for session in sessions {
                    let size = local.store_of(&session).map_or((0, None), |store| {
                        (store.messages_version(&session), chat_activity::version(store, &session))
                    });
                    grew |= sizes.insert(session, size.clone()).is_some_and(|before| before != size);
                }
                let version = data_version();
                grew |= version != db_version;
                db_version = version;
                let version = skills::version(&local.skills_dir());
                grew |= version != skills_version;
                skills_version = version;
                if grew {
                    local.changed();
                }
            }
        });
    }
}

/// The engine and model an agent is made or changed with (`engine`: "claude", "codex", "muse" or "opencode"; `model`: a
/// model it offers, none for its own default), when they carry one.
fn model_pick(args: &Value) -> Result<Option<(Engine, Option<String>)>, String> {
    let Some(engine) = arg(args, "engine") else { return Ok(None) };
    let engine = Engine::parse(engine).ok_or_else(|| format!("Not an engine: {engine}"))?;
    let model = arg(args, "model").map(str::trim).filter(|model| !model.is_empty());
    if let Some(model) = model.filter(|model| !filo_local::engine::valid_model(model)) {
        return Err(format!("Not a model name: {model}"));
    }
    Ok(Some((engine, model.map(str::to_string))))
}

/// Keeps how hard a conversation's engine thinks from its next turn on (`Chat::reasoning`): a level some tool names
/// (`EFFORT_ORDER`), which each turn makes one its model takes (`chat_effort`).
fn choose_reasoning(store: &Store, session_id: &str, level: &str) -> Result<(), String> {
    if !EFFORT_ORDER.contains(&level) {
        return Err(format!("Not a reasoning level: {level}"));
    }
    store
        .update_chat(session_id, |chat| chat.reasoning = Some(level.to_string()))
        .map(|_| ())
        .map_err(|error| format!("Couldn't save the reasoning level: {error}"))
}

/// A note's title, body, tags and pin from `notes:createMine` / `notes:updateMine`.
fn note_fields(args: &Value) -> Result<notes::Fields, String> {
    serde_json::from_value(args.clone()).map_err(|error| format!("The note isn't right: {error}"))
}

/// What the engine is told it is in an agent's conversation; `earlier` is the messages file of the one it was duplicated
/// from, when it was.
fn persona(agent: &app_data::Agent, earlier: Option<&std::path::Path>) -> String {
    let earlier = earlier.map(|path| {
        format!(
            "\n\nYou continue from an earlier conversation the user had with this agent on another engine. It's in `{}`: one JSON \
message per line, with `role` (user, assistant), `text` and `at` (ms since 1970); `hidden` ones came from the Filo app, not \
the user. When the user refers to earlier work, search it (grep for the words) rather than reading it all.",
            path.display()
        )
    });
    format!(
        "You are {name}, an agent the user made in the Filo desktop app on their Mac, talking with them in your own conversation. \
The user only sees what you send with the `send_message` tool (from the `filo` MCP server); your plain text output is never shown. \
Answer every message by calling `send_message` with your reply in Markdown, usually once per turn. \
If something needs a tool you don't have, say so in your message.{earlier}\n\nWho you are and how to work, in the user's words:\n\n{instructions}",
        name = agent.name,
        earlier = earlier.unwrap_or_default(),
        instructions = agent.instructions
    )
}

/// `desktop-local.json`: the engine that answers (one of the coding tools) where the composer's model picker wasn't used,
/// picked in the chat's header, or the tool the first agent runs on (screens/welcome.rs).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineSettings {
    #[serde(default = "claude")]
    engine: Engine,
}

fn claude() -> Engine {
    Engine::Claude
}

fn engine_settings_path() -> PathBuf {
    crate::config::config_dir().join("desktop-local.json")
}

/// The engine conversations and turns get.
pub fn default_engine() -> Engine {
    std::fs::read(engine_settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<EngineSettings>(&bytes).ok())
        .map_or(Engine::Claude, |settings| settings.engine)
}

pub fn set_default_engine(engine: Engine) {
    if let Ok(bytes) = serde_json::to_vec_pretty(&EngineSettings { engine }) {
        let _ = crate::config::write_atomic(&engine_settings_path(), &bytes);
    }
}

/// What Muse Code and OpenCode offer, for the composer's picker (each starts a server of its own to say); None for one
/// that isn't installed or couldn't say.
fn probe_session_models() -> Vec<(Engine, Option<Vec<ModelOption>>)> {
    let path = login_path();
    let workspace = Store::at(crate::config::config_dir().join("local/agents"))
        .workspace()
        .unwrap_or_else(|_| std::env::temp_dir());
    [Engine::Muse, Engine::OpenCode]
        .into_iter()
        .map(|engine| {
            let rows = engine::find_program(engine, Some(path)).map(|program| match engine {
                Engine::Muse => filo_local::coding_muse::models(&program, Some(path)),
                _ => filo_local::coding_opencode::models(&program, Some(path), &workspace),
            });
            (
                engine,
                rows.filter(|rows| !rows.is_empty())
                    .map(|rows| rows.into_iter().map(|model| model.option(engine)).collect()),
            )
        })
        .collect()
}

/// Claude Code's rows as it names its models: each alias as the model it is and the one it answers with by default
/// (`engine::claude_model`, all at once, in the agents' workspace so its settings are the turns'). None when it isn't
/// installed or said nothing, so the cached rows stay. Blocks for a few seconds.
fn probe_claude_models() -> Option<Vec<ModelOption>> {
    let path = login_path();
    let program = engine::find_program(Engine::Claude, Some(path))?;
    let workspace = Store::at(crate::config::config_dir().join("local/agents"))
        .workspace()
        .unwrap_or_else(|_| std::env::temp_dir());
    let (program, workspace) = (&program, &workspace);
    let (default, resolved) = std::thread::scope(|scope| {
        let default = scope.spawn(move || engine::claude_model(program, workspace, Some(path), None));
        let aliases: Vec<_> = engine::CLAUDE_MODELS
            .iter()
            .map(|(alias, _)| {
                (
                    alias.to_string(),
                    scope.spawn(move || engine::claude_model(program, workspace, Some(path), Some(alias))),
                )
            })
            .collect();
        let resolved: Vec<(String, engine::Model)> =
            aliases.into_iter().filter_map(|(alias, probe)| Some((alias, probe.join().ok()??))).collect();
        (default.join().ok().flatten(), resolved)
    });
    (default.is_some() || !resolved.is_empty()).then(|| engine::claude_options(default.as_ref(), &resolved))
}

/// Which engines this Mac has, with the login shell's PATH (blocks while the shell answers the first time).
pub fn installed(engine: Engine) -> bool {
    filo_local::engine::find_program(engine, Some(login_path())).is_some()
}

/// Where a coding tool stands on this Mac (the welcome's tool check and Set up a tool).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    NotInstalled,
    NeedsSignIn,
    Ready,
}

/// Whether the tool is installed and signed in, as the Coding page says it (`coding::signed_in`). Blocks, so off the
/// UI thread.
pub fn tool_status(engine: Engine) -> ToolStatus {
    if let Some(status) = tool_status_wanted(engine) {
        return status;
    }
    let path = login_path();
    let Some(program) = engine::find_program(engine, Some(path)) else { return ToolStatus::NotInstalled };
    let signed_in = coding::signed_in(engine, &program, path);
    // The Coding page's copy says the same from now on, not after its next look.
    coding::saw_sign_in(engine, signed_in);
    if signed_in { ToolStatus::Ready } else { ToolStatus::NeedsSignIn }
}

/// `dev` builds, `FILO_TOOL_STATUS="claude=missing codex=signin"`: those states instead of what the CLIs say, for
/// Welcome and the Coding page's look alike (a look at the welcome tour's steps, or the model picker without a tool,
/// on a Mac where all are ready).
pub(crate) fn tool_status_wanted(engine: Engine) -> Option<ToolStatus> {
    let wanted = std::env::var("FILO_TOOL_STATUS").ok().filter(|_| cfg!(feature = "dev"))?;
    let pair = wanted
        .split_whitespace()
        .find_map(|pair| pair.strip_prefix(engine.program())?.strip_prefix('=').map(str::to_string));
    match pair.as_deref() {
        Some("missing") => Some(ToolStatus::NotInstalled),
        Some("signin") => Some(ToolStatus::NeedsSignIn),
        Some("ready") => Some(ToolStatus::Ready),
        _ => None,
    }
}

/// The models of the tools in `ready` (the ones signed in), in the picker's order.
fn signed_in_rows(rows: &[ModelOption], ready: &[Engine]) -> Vec<ModelOption> {
    rows.iter().filter(|row| ready.contains(&row.engine)).cloned().collect()
}

/// How the app runs turns: itself as the MCP server (`filo --local-mcp`), with the login shell's PATH (apps opened from
/// Finder start with a bare one, without `~/.local/bin` or Homebrew, where `claude` and `codex` usually are). Blocks
/// the first time while the login shell answers, so call it off the UI thread. `db` is Filo's database, where the
/// turn reads the user's skills.
pub fn runtime(db: PathBuf) -> Runtime {
    // Tests run no app binary to serve Filo's tools: `FILO_TEST_MCP=<filo-local>` serves them (`filo-local mcp`).
    #[cfg(test)]
    if let Some(program) = std::env::var_os("FILO_TEST_MCP") {
        return Runtime {
            mcp_program: program.into(),
            mcp_args: vec!["mcp".into()],
            path_env: Some(login_path().to_string()),
            hung_after: turn::HUNG_AFTER,
            db: Some(db),
            agent: None,
        };
    }
    Runtime {
        mcp_program: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("filo")),
        mcp_args: vec!["--local-mcp".into()],
        path_env: Some(login_path().to_string()),
        hung_after: turn::HUNG_AFTER,
        db: Some(db),
        // Filo; a turn as an agent sets it (`start_turn_then`).
        agent: None,
    }
}

/// The PATH tools run with: the login shell's, read once (it takes seconds), and handed to the workers the app starts
/// (`LOGIN_PATH`, local_backend/workers.rs) so they don't read it again.
pub fn login_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::var(LOGIN_PATH).ok().filter(|path| !path.is_empty()).unwrap_or_else(|| {
            search_path(login_shell_path()).iter().map(|dir| dir.display().to_string()).collect::<Vec<_>>().join(":")
        })
    })
}

/// The environment variable a worker gets the app's `login_path` in.
const LOGIN_PATH: &str = "FILO_LOGIN_PATH";

/// The PATH a terminal gets (`$SHELL -ilc`), or None.
fn login_shell_path() -> Option<String> {
    login_shell_var("PATH")
}

/// The variables read from the login shell. FILO_TELEMETRY turns off the usage counts from ~/.zshrc (telemetry.rs).
const LOGIN_SHELL_VARS: [&str; 2] = ["PATH", "FILO_TELEMETRY"];

/// What a terminal has for one of `LOGIN_SHELL_VARS` (None when unset or empty). The shell (`$SHELL -ilc`, given
/// 5 seconds) runs once for all of them; it takes seconds, so not on the UI thread. Without Tokio: turn threads
/// aren't on its runtime.
pub fn login_shell_var(name: &str) -> Option<String> {
    static VARS: OnceLock<HashMap<String, String>> = OnceLock::new();
    VARS.get_or_init(|| login_shell_vars().unwrap_or_default()).get(name).cloned().filter(|value| !value.is_empty())
}

fn login_shell_vars() -> Option<HashMap<String, String>> {
    const MARK: &str = "__FILO_ENV__";
    let shell = std::env::var("SHELL").ok().filter(|shell| !shell.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    // One line per variable: `__FILO_ENV__NAME=value__FILO_ENV__`.
    let script = LOGIN_SHELL_VARS
        .iter()
        .map(|name| format!(r#"printf '\n{MARK}{name}=%s{MARK}\n' "${name}""#))
        .collect::<Vec<_>>()
        .join("; ");
    let ran =
        crate::command::run(std::process::Command::new(shell).args(["-ilc", &script]), None, Duration::from_secs(5))?;
    let output = String::from_utf8_lossy(&ran.stdout);
    Some(parse_login_shell_vars(&output))
}

/// The `__FILO_ENV__NAME=value__FILO_ENV__` lines in what the shell printed (a profile may print more around them).
fn parse_login_shell_vars(output: &str) -> HashMap<String, String> {
    const MARK: &str = "__FILO_ENV__";
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix(MARK)?.strip_suffix(MARK)?.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// The login shell's PATH (or the app's), then the usual places tools are installed, without repeats.
fn search_path(login: Option<String>) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let own = std::env::var("PATH").unwrap_or_default();
    let extras = [
        home.join(".local/bin"),
        home.join("Library/pnpm"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ];
    for dir in login
        .as_deref()
        .unwrap_or("")
        .split(':')
        .chain(own.split(':'))
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .chain(extras)
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// A conversation's title as every list gets it (the chat, its row, unseen replies, work going on): "New chat" while it
/// has none.
fn title_of(title: &str) -> String {
    if title.is_empty() { "New chat".to_string() } else { title.to_string() }
}

/// A conversation's row in `voiceSessions:unseen`: `latest`'s preview is its summary, so a reply of files alone shows
/// their names (the agents' chats and the group alike).
pub(crate) fn unseen_row(
    session: &str,
    title: &str,
    count: usize,
    seen_until: f64,
    latest_at: f64,
    latest: &str,
) -> Value {
    json!({
        "sessionId": session,
        "title": title_of(title),
        "count": count,
        "seenUntil": seen_until,
        "latestAt": latest_at,
        "preview": filo_local::store::preview(latest, PREVIEW_CHARS),
    })
}

/// What the local backend's tests share (each module's `temp`).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use super::Local;

    /// A `Local` on a data folder of its own, removed when the test drops it (passed or failed), so test runs don't
    /// leave folders behind in the temp folder. Reads as the `Arc<Local>` it holds.
    pub(crate) struct TempLocal(Option<Arc<Local>>);

    impl TempLocal {
        /// Filo quitting with its folder kept, for a test that opens it again (and removes it itself).
        pub(crate) fn close(mut self) {
            self.0.take();
        }
    }

    impl std::ops::Deref for TempLocal {
        type Target = Arc<Local>;

        fn deref(&self) -> &Arc<Local> {
            self.0.as_ref().expect("open until closed")
        }
    }

    impl Drop for TempLocal {
        fn drop(&mut self) {
            if let Some(local) = self.0.take() {
                let _ = std::fs::remove_dir_all(&local.root);
            }
        }
    }

    /// `filo-local-<what>-<uuid>` in the temp folder.
    pub(crate) fn temp(what: &str) -> TempLocal {
        TempLocal(Some(Local::open(std::env::temp_dir().join(format!("filo-local-{what}-{}", uuid::Uuid::new_v4())))))
    }

    /// One of the pages' functions, as the app calls it.
    pub(crate) fn call(local: &Arc<Local>, path: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
        local.call(path, &args)
    }

    /// An executable script `name` in `dir`: a stand-in for a CLI.
    pub(crate) fn script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_shell_vars_are_read_from_their_marked_lines() {
        let output = "Welcome to zsh\n\n__FILO_ENV__PATH=/opt/homebrew/bin:/usr/bin__FILO_ENV__\n\n__FILO_ENV__FILO_TELEMETRY=0__FILO_ENV__\nbye\n";
        let vars = parse_login_shell_vars(output);
        assert_eq!(vars.get("PATH").map(String::as_str), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(vars.get("FILO_TELEMETRY").map(String::as_str), Some("0"));
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn keeps_the_login_path_first_without_repeats() {
        let dirs = search_path(Some("/x/bin:/opt/homebrew/bin:/x/bin".into()));
        assert_eq!(dirs[0], PathBuf::from("/x/bin"));
        assert_eq!(dirs.iter().filter(|dir| dir.as_path() == std::path::Path::new("/opt/homebrew/bin")).count(), 1);
        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
    }

    fn temp() -> super::test_support::TempLocal {
        super::test_support::temp("backend")
    }

    #[test]
    fn a_reply_shows_what_it_answers_after_a_reload() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let answer = Message::new(Role::Assistant, "Bus or train?", Some("t-1".into()));
        local.agent_chats.append(&session, &answer).unwrap();
        let reply = local.agent_chats.add_user_reply(&session, "train", "t-2", Vec::new(), Some(&answer)).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session, "limit": 50 })).unwrap();
        let row = &detail["messages"][1];
        assert_eq!(
            (row["_id"].as_str(), row["content"].as_str()),
            (Some(reply.id.as_str()), Some("> Bus or train?\n\ntrain"))
        );
        assert_eq!(row["replyToMessageId"].as_str(), Some(answer.id.as_str()));
        assert_eq!(detail["messages"][0]["replyToMessageId"], Value::Null);
        // The engine reads it with its quote.
        assert_eq!(
            filo_local::turn::prompt_for(&local.agent_chats.messages(&session).unwrap(), "t-2", true, None),
            "> Bus or train?\n\ntrain"
        );
    }

    #[test]
    fn reactions_are_kept_in_agents_chats_and_filos_own() {
        let local = temp();
        let session =
            local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap().as_str().unwrap().to_string();
        let reply = Message::new(Role::Assistant, "Booked for 7.", Some("t-1".into()));
        local.agent_chats.append(&session, &reply).unwrap();
        let reaction = |id: &str| {
            local.call("voiceSessions:detail", &json!({ "id": id })).unwrap()["messages"][0]["reaction"].clone()
        };
        assert_eq!(reaction(&session), Value::Null);
        let mut changes = local.changes();
        changes.borrow_and_update();
        local
            .call(
                "voiceSessions:reactToMessage",
                &json!({ "messageId": reply.id, "sessionId": session, "reaction": "👍" }),
            )
            .unwrap();
        assert!(changes.has_changed().unwrap());
        assert_eq!(reaction(&session), json!("👍"));
        // Taken back.
        local.call("voiceSessions:reactToMessage", &json!({ "messageId": reply.id, "sessionId": session })).unwrap();
        assert_eq!(reaction(&session), Value::Null);
        // Another conversation.
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let answer = Message::new(Role::Assistant, "Here.", Some("t-1".into()));
        local.agent_chats.append(&chat, &answer).unwrap();
        local
            .call(
                "voiceSessions:reactToMessage",
                &json!({ "messageId": answer.id, "sessionId": chat, "reaction": "❤️" }),
            )
            .unwrap();
        assert_eq!(reaction(&chat), json!("❤️"));
        // A message in another chat than the one named, or none, isn't reacted to.
        let elsewhere = json!({ "messageId": answer.id, "sessionId": session, "reaction": "🔥" });
        assert!(local.call("voiceSessions:reactToMessage", &elsewhere).unwrap_err().contains("no longer exists"));
        assert!(
            local
                .call(
                    "voiceSessions:reactToMessage",
                    &json!({ "messageId": "gone", "sessionId": chat, "reaction": "🔥" })
                )
                .is_err()
        );
        assert_eq!(reaction(&chat), json!("❤️"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_reasoning_picker_sets_how_hard_the_chats_engine_thinks() {
        let local = temp();
        local.no_turns.store(true, Ordering::SeqCst);
        let id = local
            .call("agents:create", &json!({ "name": "Planner", "instructions": "Plans.", "engine": "claude" }))
            .unwrap();
        let chat = local.call("agents:openChat", &json!({ "id": id })).unwrap().as_str().unwrap().to_string();
        let shown = |id: &str| {
            local.call("voiceSessions:detail", &json!({ "id": id })).unwrap()["session"]["reasoning"].clone()
        };
        assert_eq!(shown(&chat), Value::Null);
        local.call("voiceSessions:setReasoning", &json!({ "sessionId": chat, "reasoning": "xhigh" })).unwrap();
        assert_eq!(shown(&chat), json!("xhigh"));
        // The composer lists the levels its model takes (`chat_efforts`).
        let offered = |id: &str| {
            local.call("voiceSessions:detail", &json!({ "id": id })).unwrap()["session"]["reasoningLevels"].clone()
        };
        assert_eq!(offered(&chat), json!(["low", "medium", "high", "xhigh", "max"]));
        assert!(
            local
                .call("voiceSessions:setReasoning", &json!({ "sessionId": chat, "reasoning": "--yolo" }))
                .unwrap_err()
                .contains("Not a reasoning level")
        );
        assert!(local.call("voiceSessions:setReasoning", &json!({ "sessionId": "gone", "reasoning": "low" })).is_err());
        // The turn's engine gets it as it starts (turn.rs reads `effort`).
        local.set_turn_model(&local.agent_chats, &chat);
        assert_eq!(local.agent_chats.chat(&chat).unwrap().unwrap().effort.as_deref(), Some("xhigh"));

        // Picked before a message is sent, it comes with it.
        let first = json!({ "sessionId": chat, "message": "Plan my week", "reasoning": "low" });
        local.call("voiceSessions:sendMessage", &first).unwrap();
        assert_eq!(shown(&chat), json!("low"));

        // An agent's chat keeps its own, with the agent's model.
        let id = local
            .call("agents:create", &json!({ "name": "Tutor", "instructions": "Teach me.", "engine": "claude" }))
            .unwrap();
        let session = local.call("agents:openChat", &json!({ "id": id })).unwrap().as_str().unwrap().to_string();
        local.call("voiceSessions:setReasoning", &json!({ "sessionId": session, "reasoning": "max" })).unwrap();
        local.set_turn_model(&local.agent_chats, &session);
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        assert_eq!(
            (chat.engine, chat.reasoning.as_deref(), chat.effort.as_deref()),
            (Engine::Claude, Some("max"), Some("max"))
        );
        assert_eq!(offered(&session), json!(["low", "medium", "high", "xhigh", "max"]));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn notes_and_tasks_sent_with_a_message_go_to_its_engine_only() {
        let local = temp();
        local.no_turns.store(true, Ordering::SeqCst);
        let session =
            local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap().as_str().unwrap().to_string();
        let context = |turn: &str| {
            let file = local.agent_chats.root().join("chats").join(&session).join("context.json");
            serde_json::from_slice::<Value>(&std::fs::read(file).unwrap()).unwrap()[turn].as_str().map(str::to_string)
        };
        let folder = local.root.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let db = local.db().unwrap();
        let project = filo_local::coding::add_project(db, &folder, 1).unwrap();
        let new = filo_local::coding::NewTask {
            brief: "Fix the header".into(),
            tool: "claude-code".into(),
            project,
            worktree: false,
            owner: "You".into(),
        };
        let task = filo_local::coding::create_task(db, &new, 1).unwrap();

        // A task dragged onto the composer goes with a typed message (`sendMessage`), the bubble only the words.
        let pointed = json!({ "sessionId": session, "requestId": "req-1", "message": "Is this one done?", "sharedWork": [{ "kind": "coding", "id": task.id }] });
        local.call("voiceSessions:sendMessage", &pointed).unwrap();
        assert_eq!(local.agent_chats.messages(&session).unwrap().last().unwrap().text, "Is this one done?");
        let said = context("req-1").unwrap();
        assert_eq!(
            said,
            format!(
                "A coding task the user points at (id {}, for watch_coding_task and tell_coding_task): \u{201c}Fix the header\u{201d}, Claude Code in site, queued.",
                task.id
            )
        );
        let prompt = filo_local::turn::with_context(Some(&said), "Is this one done?");
        assert!(
            prompt.starts_with("Is this one done?\n\n---\n\nMaterial that comes with this message")
                && prompt.ends_with("queued."),
            "{prompt}"
        );

        // Notes shared from the Notes tab go the same way, each with its title and text.
        let notes =
            json!([{ "id": "n-1", "title": "Groceries", "body": "- eggs" }, { "title": "", "body": "Call the bank" }]);
        let shared = json!({ "sessionId": session, "requestId": "req-2", "message": "Add milk", "sharedNotes": notes });
        local.call("voiceSessions:sendMessage", &shared).unwrap();
        assert_eq!(
            context("req-2").unwrap(),
            "A note the user shared from their Notes (id n-1, for read_note and update_note):\nTitle: Groceries\n\n- eggs\n\n\
A note the user shared from their Notes:\nTitle: Untitled note\n\nCall the bank"
        );
        // A plain message carries nothing.
        local
            .call(
                "voiceSessions:sendMessage",
                &json!({ "sessionId": session, "requestId": "req-3", "message": "Thanks" }),
            )
            .unwrap();
        assert_eq!(context("req-3"), None);

        // What the composer wouldn't send is refused, and nothing is kept.
        let refused = |args: Value| local.call("voiceSessions:sendMessage", &args).unwrap_err();
        let note = json!({ "title": "N", "body": "x" });
        assert!(
            refused(json!({ "sessionId": session, "message": "x", "sharedNotes": [note, note, note, note] }))
                .contains("Up to 3 notes")
        );
        assert!(refused(json!({ "sessionId": session, "message": "x", "sharedNotes": [{ "title": "Long", "body": "x".repeat(8_001) }] })).contains("too long"));
        let six = vec![json!({ "kind": "coding", "id": task.id }); 6];
        assert!(refused(json!({ "sessionId": session, "message": "x", "sharedWork": six })).contains("Up to 5 tasks"));
        assert!(
            refused(json!({ "sessionId": session, "message": "x", "sharedWork": [{ "kind": "page", "id": "p" }] }))
                .contains("isn't one Filo knows")
        );
        assert_eq!(local.agent_chats.messages(&session).unwrap().len(), 3);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_reply_left_unseen_for_a_few_seconds_is_notified_about() {
        let local = temp();
        let session =
            local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap().as_str().unwrap().to_string();
        let group_chat = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        let unseen = |id: &str| {
            let unseen = local.call("voiceSessions:unseen", &json!({})).unwrap();
            unseen["chats"].as_array().unwrap().iter().find(|chat| chat["sessionId"] == json!(id)).cloned()
        };
        // Waiting from before: notified about already (the app doesn't ring again for what was waiting at launch).
        let mut earlier = Message::new(Role::Assistant, "Earlier", Some("t-1".into()));
        earlier.at -= 60_000;
        local.agent_chats.append(&session, &earlier).unwrap();
        assert_eq!(unseen(&session).unwrap()["pushedUntil"], json!(earlier.at as f64));
        // Notified under the agent's name, renamed since its chat was made too.
        local.call("agents:update", &json!({ "id": DEFAULT_AGENT_ID, "name": "Juno" })).unwrap();
        assert_eq!(unseen(&session).unwrap()["title"], "Juno");
        let posted = filo_local::group::post(
            local.db().unwrap(),
            filo_local::apps::FILO_AGENT,
            "Pasta or rice?",
            filo_local::group::Asks::default(),
            None,
            now_ms() - 60_000,
        )
        .unwrap();
        assert_eq!(unseen(&group_chat).unwrap()["pushedUntil"], json!(posted.at as f64));

        // A new reply first waits for the user to see it, then the live queries are woken to notify about it.
        let mut changes = local.changes();
        changes.borrow_and_update();
        let reply = Message::new(Role::Assistant, "Done", Some("t-2".into()));
        local.agent_chats.append(&session, &reply).unwrap();
        let row = unseen(&session).unwrap();
        assert_eq!((row["count"].as_u64(), &row["pushedUntil"]), (Some(2), &Value::Null));
        let waiting = std::time::Instant::now();
        while !changes.has_changed().unwrap() {
            assert!(waiting.elapsed() < Duration::from_secs(10), "the live queries weren't woken");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(now_ms() as f64 >= reply.at as f64 + PUSH_AFTER);
        assert_eq!(unseen(&session).unwrap()["pushedUntil"], json!(reply.at as f64));

        // Seen on screen before then, nothing is notified.
        local.call("voiceSessions:markSeen", &json!({ "sessionId": session })).unwrap();
        assert!(unseen(&session).is_none());
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_conversation_opens_as_its_agents_with_nothing_unread() {
        let local = temp();
        let owner = |id: &str| local.call("agents:ofSession", &json!({ "sessionId": id })).unwrap();
        let session =
            local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap().as_str().unwrap().to_string();
        let group_chat = local.call("agents:openGroup", &json!({})).unwrap().as_str().unwrap().to_string();
        local.agent_chats.append(&session, &Message::new(Role::Assistant, "Hello", Some("t-1".into()))).unwrap();
        local.call("voiceSessions:markSeen", &json!({ "sessionId": session })).unwrap();
        assert!(local.call("voiceSessions:unseen", &json!({})).unwrap()["chats"].as_array().unwrap().is_empty());
        assert_eq!(owner(&session), json!({ "agentId": DEFAULT_AGENT_ID }));
        assert_eq!(owner(&group_chat), json!({ "agentGroup": true }));
        // A conversation no agent has, and one that's gone, are no agent's.
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        assert_eq!(owner(&chat), Value::Null);
        assert_eq!(owner("gone"), Value::Null);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn routine_runs_show_as_a_line_and_quiet_ones_say_so() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let mark = filo_local::store::RoutineRun { title: "Check PRs".into(), when: "every 30 minutes".into() };
        let quiet = local
            .agent_chats
            .add_routine_message(
                &session,
                "⏰ Routine “Check PRs” (every 30 minutes). Stay quiet.",
                "run-1",
                mark.clone(),
            )
            .unwrap();
        let replied = local
            .agent_chats
            .add_routine_message(&session, "⏰ Routine “Check PRs” (every 30 minutes). Stay quiet.", "run-2", mark)
            .unwrap();
        local
            .agent_chats
            .append(&session, &Message::new(Role::Assistant, "PR #4 needs you", Some("run-2".into())))
            .unwrap();
        // One from before runs were marked, read from its words; one still working isn't quiet yet.
        let older = local
            .agent_chats
            .add_user_message(&session, "⏰ Routine “Inbox” (weekdays at 9:00). Say what came in.", "run-3")
            .unwrap();
        local
            .running
            .lock()
            .unwrap()
            .insert("run-3".into(), Running { session_id: session.clone(), ..Running::default() });
        let typed = local.agent_chats.add_user_message(&session, "hello", "turn-4").unwrap();

        let detail = local.call("voiceSessions:detail", &json!({ "id": session, "limit": 50 })).unwrap();
        let routine_of = |id: &str| {
            detail["messages"].as_array().unwrap().iter().find(|message| message["_id"] == id).unwrap()["routine"]
                .clone()
        };
        assert_eq!(routine_of(&quiet.id), json!({ "title": "Check PRs", "when": "every 30 minutes", "quiet": true }));
        assert_eq!(routine_of(&replied.id)["quiet"], false);
        assert_eq!(routine_of(&older.id), json!({ "title": "Inbox", "when": "weekdays at 9:00", "quiet": false }));
        assert_eq!(routine_of(&typed.id), Value::Null);
        assert_eq!(local.agent_chats.messages(&session).unwrap()[0].summary(), "Routine · Check PRs");
    }

    #[test]
    fn hidden_failed_turns_keep_their_event_time_and_retry_target_after_new_work() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let mut trigger = Message::new(Role::User, "Coding task finished", Some("old-failure".into()));
        trigger.hidden = true;
        trigger.at = 10;
        local.agent_chats.append(&session, &trigger).unwrap();
        let mut error = Message::new(Role::Error, "Claude Code: session limit", Some("old-failure".into()));
        error.at = 20;
        local.agent_chats.append(&session, &error).unwrap();
        let mut newer = Message::new(Role::User, "Continue with Codex", Some("new-work".into()));
        newer.at = 30;
        local.agent_chats.append(&session, &newer).unwrap();
        let mut reply = Message::new(Role::Assistant, "Codex is continuing", Some("new-work".into()));
        reply.at = 40;
        local.agent_chats.append(&session, &reply).unwrap();

        let detail = local.call("voiceSessions:detail", &json!({ "id": session, "limit": 50 })).unwrap();
        let shown = detail["messages"].as_array().unwrap();
        assert_eq!(shown.len(), 2);
        assert!(shown.iter().all(|message| message["_id"] != trigger.id && message["_id"] != error.id));
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!(turns[0]["createdAt"], 10);
        assert_eq!(turns[0]["userMessageId"], trigger.id);
        // The newer work went through after it: the failure is settled (a quiet line), its error kept.
        assert_eq!(
            (turns[0]["state"].as_str(), turns[0]["error"].as_str()),
            (Some("settled"), Some("Claude Code: session limit"))
        );
        assert_eq!(turns[1]["createdAt"], 30);
        assert_eq!(turns[1]["state"], "completed");
        local.no_turns.store(true, Ordering::SeqCst);
        let retried = local
            .call("voiceSessions:retryChatTurn", &json!({ "turnId": turns[0]["turnId"], "sessionId": session }))
            .unwrap();
        assert_eq!(retried, json!({ "turnId": "old-failure" }));
        // A successful retry resolves only the old failure without changing its position.
        local
            .agent_chats
            .append(&session, &Message::new(Role::Assistant, "Recovered", Some("old-failure".into())))
            .unwrap();
        let turns = local.chat_turns(&session);
        assert_eq!(turns[0]["state"], "completed");
        assert_eq!(turns[0]["createdAt"], 10);
        assert_eq!(turns[1]["state"], "completed");
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// An agent made on the installed `FILO_REAL_ENGINE` (muse or opencode; `FILO_REAL_MODEL` for the model it's made
    /// with, `FILO_REAL_EFFORT` for the tool's reasoning level on the Coding page) answers messages sent as the app sends
    /// them: a shell command and Filo's send_message, then a follow-up in the same session. Uses the tool's sign-in, and
    /// the `filo-local` binary for Filo's tools (`cargo build -p filo-local` first):
    /// `FILO_REAL_ENGINE=muse cargo test --bin filo real_agent -- --ignored --nocapture` (`FILO_REAL_LOG=<file>` keeps
    /// the conversation).
    #[test]
    #[ignore]
    fn real_agent_answers() {
        let engine = std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "muse".into());
        // SAFETY: set before any thread of this test reads it.
        unsafe { std::env::set_var("FILO_TEST_MCP", concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/filo-local")) };
        let local = temp();
        let tool = filo_local::coding::tool_of(Engine::parse(&engine).unwrap());
        local.call("coding:setPlugged", &json!({ "tool": tool, "plugged": true })).unwrap();
        if let Ok(effort) = std::env::var("FILO_REAL_EFFORT") {
            local.call("coding:setToolEffort", &json!({ "tool": tool, "effort": effort })).unwrap();
        }
        let model = std::env::var("FILO_REAL_MODEL").ok();
        let id = local.call("agents:create", &json!({ "name": "Helper", "instructions": "You help with small jobs on this Mac.", "engine": engine, "model": model })).unwrap();
        let id = id.as_str().unwrap().to_string();
        let session = local.call("agents:openChat", &json!({ "id": id })).unwrap().as_str().unwrap().to_string();
        let mut log = format!("Agent on {engine} (model {model:?})\n\n");
        let mut ask = |text: &str| -> Vec<String> {
            let before = local.agent_chats.messages(&session).unwrap().len();
            local.call("voiceSessions:sendMessage", &json!({ "sessionId": session, "message": text })).unwrap();
            let started = std::time::Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(300));
                if local.running.lock().unwrap().is_empty()
                    && local.agent_chats.messages(&session).unwrap().len() > before + 1
                {
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(300), "no answer");
            }
            let said: Vec<String> = local.agent_chats.messages(&session).unwrap()[before + 1..]
                .iter()
                .map(|message| format!("{:?}: {}", message.role, message.text))
                .collect();
            log.push_str(&format!("> {text}\n{}\n\n", said.join("\n")));
            said
        };
        let first = ask(
            "Run the shell command `echo filo-$((6*7))` and reply with send_message, quoting exactly what it printed.",
        );
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        let first_session = chat.session.clone().expect("its session");
        let second = ask("What number did that command print? Just the number.");
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        log.push_str(&format!(
            "engine {:?}, session {} then {}\n",
            chat.engine,
            first_session.id,
            chat.session.as_ref().map(|session| session.id.as_str()).unwrap_or("none")
        ));
        println!("{log}");
        if let Ok(file) = std::env::var("FILO_REAL_LOG") {
            std::fs::write(file, &log).unwrap();
        }
        assert_eq!(chat.engine, Engine::parse(&engine).unwrap());
        assert!(first.iter().any(|line| line.starts_with("Assistant:") && line.contains("filo-42")), "{first:?}");
        assert!(second.iter().any(|line| line.starts_with("Assistant:") && line.contains("42")), "{second:?}");
        assert_eq!(chat.session.unwrap().id, first_session.id, "the follow-up carried on in the same session");
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn an_agent_answers_with_the_model_picked_for_it() {
        let local = temp();
        let agent = |id: &str| {
            local
                .call("agents:list", &json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["_id"] == id)
                .cloned()
                .unwrap()
        };
        let id = local
            .call(
                "agents:create",
                &json!({ "name": "Tutor", "instructions": "Teach me.", "engine": "codex", "model": "gpt-a" }),
            )
            .unwrap();
        let id = id.as_str().unwrap().to_string();
        assert_eq!((agent(&id)["engine"].as_str(), agent(&id)["model"].as_str()), (Some("codex"), Some("gpt-a")));
        let session = local.call("agents:openChat", &json!({ "id": id })).unwrap().as_str().unwrap().to_string();

        // Its chat takes no model of its own, and every turn answers with the agent's.
        local
            .agent_chats
            .update_chat(&session, |chat| {
                (chat.engine, chat.model, chat.engine_chosen) = (Engine::Claude, Some("opus".into()), true)
            })
            .unwrap();
        local.set_turn_model(&local.agent_chats, &session);
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        assert_eq!((chat.engine, chat.model.as_deref(), chat.engine_chosen), (Engine::Codex, Some("gpt-a"), true));

        // Changed in its profile, from the next turn on; an edit without a model keeps it.
        local.call("agents:update", &json!({ "id": id, "engine": "claude", "model": null })).unwrap();
        local.call("agents:update", &json!({ "id": id, "name": "Coach" })).unwrap();
        assert_eq!((agent(&id)["engine"].as_str(), &agent(&id)["model"]), (Some("claude"), &Value::Null));
        local.set_turn_model(&local.agent_chats, &session);
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        assert_eq!((chat.engine, chat.model.as_deref()), (Engine::Claude, None));

        // Once its conversation has a session, the engine stays: another of its models, yes; another engine, no.
        assert_eq!(agent(&id)["lockedEngine"], Value::Null);
        let started =
            filo_local::store::Session { engine: Engine::Claude, id: "s-1".into(), instructions: None, model: None };
        local.agent_chats.update_chat(&session, |chat| chat.session = Some(started)).unwrap();
        assert_eq!(agent(&id)["lockedEngine"], json!("claude"));
        local.call("agents:update", &json!({ "id": id, "engine": "claude", "model": "sonnet" })).unwrap();
        let refused =
            local.call("agents:update", &json!({ "id": id, "engine": "codex", "model": "gpt-a" })).unwrap_err();
        assert!(refused.contains("duplicate it with Codex"), "{refused}");
        assert_eq!((agent(&id)["engine"].as_str(), agent(&id)["model"].as_str()), (Some("claude"), Some("sonnet")));
        assert!(
            local
                .call(
                    "agents:create",
                    &json!({ "name": "X", "instructions": "Y", "engine": "codex", "model": "--yolo" })
                )
                .unwrap_err()
                .contains("Not a model")
        );

        // One from before there was a pick follows the app's default, whatever its chat had picked.
        let filo = local.agents().into_iter().find(|agent| agent.id == DEFAULT_AGENT_ID).unwrap();
        assert_eq!(
            (agent(DEFAULT_AGENT_ID)["engine"].clone(), model_choice(&filo)),
            (Value::Null, (default_engine(), None))
        );
        let session =
            local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap().as_str().unwrap().to_string();
        local
            .agent_chats
            .update_chat(&session, |chat| {
                (chat.engine, chat.model, chat.engine_chosen) = (Engine::Codex, Some("gpt-a".into()), true)
            })
            .unwrap();
        local.set_turn_model(&local.agent_chats, &session);
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        assert_eq!((chat.engine, chat.model.as_deref(), chat.engine_chosen), (default_engine(), None, false));
        // Until its conversation has a session: then it stays on that engine, whatever the app's default becomes.
        let other = if default_engine() == Engine::Claude { Engine::Codex } else { Engine::Claude };
        local
            .agent_chats
            .update_chat(&session, |chat| {
                chat.session = Some(filo_local::store::Session {
                    engine: other,
                    id: "s-2".into(),
                    instructions: None,
                    model: None,
                })
            })
            .unwrap();
        local.set_turn_model(&local.agent_chats, &session);
        assert_eq!(local.agent_chats.chat(&session).unwrap().unwrap().engine, other);
    }

    #[test]
    fn a_duplicate_moves_an_agent_to_the_other_engine() {
        let local = temp();
        let id = local
            .call(
                "agents:create",
                &json!({ "name": "Tutor", "instructions": "Teach me.", "picture": "mint", "engine": "claude" }),
            )
            .unwrap();
        let id = id.as_str().unwrap().to_string();
        let session = local.call("agents:openChat", &json!({ "id": id })).unwrap().as_str().unwrap().to_string();
        assert!(local.call("agents:duplicate", &json!({ "id": id })).unwrap_err().contains("Pick the engine"));

        let copy = local.call("agents:duplicate", &json!({ "id": id, "engine": "codex", "model": "gpt-a" })).unwrap();
        let copy = local.agents().into_iter().find(|agent| Some(agent.id.as_str()) == copy.as_str()).unwrap();
        assert_eq!(
            (copy.name.as_str(), copy.instructions.as_str(), copy.picture.as_str()),
            ("Tutor (Codex)", "Teach me.", "mint")
        );
        assert_eq!(
            (copy.known_engine(), copy.model.as_deref(), copy.earlier_chat.as_deref(), copy.session_id.as_deref()),
            (Some(Engine::Codex), Some("gpt-a"), Some(session.as_str()), None)
        );
        // Its conversation can look back at the old one; the old one is as it was.
        let own = local.call("agents:openChat", &json!({ "id": copy.id })).unwrap().as_str().unwrap().to_string();
        let persona = local.agent_chats.chat(&own).unwrap().unwrap().instructions.unwrap();
        assert!(persona.contains(&format!("{session}/messages.jsonl")) && persona.contains("Teach me."), "{persona}");
        let old = local.agent_chats.chat(&session).unwrap().unwrap().instructions.unwrap();
        assert!(!old.contains("messages.jsonl"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn an_agents_row_shows_the_group_message_it_was_called_with_from_when_it_was_said() {
        let local = temp();
        let session =
            local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap().as_str().unwrap().to_string();
        // Said in the group at 1000; the agent's turn took it up later and it stayed silent.
        let said = filo_local::group::post(
            local.db().unwrap(),
            filo_local::apps::FILO_AGENT,
            "that is expected",
            filo_local::group::Asks::default(),
            None,
            1_000,
        )
        .unwrap();
        let mut call = Message::new(Role::User, "[From the group chat…]", Some("t-1".into()));
        call.group_call = Some(filo_local::store::GroupCall {
            speaker: DEFAULT_AGENT_ID.into(),
            from: None,
            message_id: Some(said.id),
            ask: "that is expected".into(),
            context: String::new(),
            answers_user: false,
            after_question: false,
            checking: false,
        });
        assert!(call.at > 1_000);
        local.agent_chats.append(&session, &call).unwrap();
        let rows = local.call("agents:list", &json!({})).unwrap();
        let row = rows.as_array().unwrap().iter().find(|row| row["_id"] == DEFAULT_AGENT_ID).unwrap();
        assert_eq!(row["last"], json!({ "fromUser": false, "preview": "that is expected", "at": 1_000. }));
        // Its own reply after: its time.
        let reply = Message::new(Role::Assistant, "Noted.", Some("t-1".into()));
        local.agent_chats.append(&session, &reply).unwrap();
        let rows = local.call("agents:list", &json!({})).unwrap();
        let row = rows.as_array().unwrap().iter().find(|row| row["_id"] == DEFAULT_AGENT_ID).unwrap();
        assert_eq!(
            (row["last"]["preview"].as_str(), row["last"]["at"].as_f64()),
            (Some("Noted."), Some(reply.at as f64))
        );
        // An "@everyone" it only checked and stayed quiet on leaves its row as it was.
        let mut check = call.clone();
        check.id = uuid::Uuid::new_v4().to_string();
        check.turn = Some("t-2".into());
        check.group_call.as_mut().unwrap().checking = true;
        local.agent_chats.append(&session, &check).unwrap();
        let rows = local.call("agents:list", &json!({})).unwrap();
        let row = rows.as_array().unwrap().iter().find(|row| row["_id"] == DEFAULT_AGENT_ID).unwrap();
        assert_eq!(
            (row["last"]["preview"].as_str(), row["last"]["at"].as_f64()),
            (Some("Noted."), Some(reply.at as f64))
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn agents_round_trip_with_their_conversation() {
        let local = temp();
        let id = local
            .call("agents:create", &json!({ "name": "Tutor", "instructions": "Teach me.", "picture": "mint" }))
            .unwrap();
        let id = id.as_str().unwrap().to_string();
        // Without the Filo fresh data starts with (`fresh_data_starts_with_filo_ready_to_chat`): Tutor is main first.
        assert_eq!(local.call("agents:remove", &json!({ "id": DEFAULT_AGENT_ID })).unwrap_err(), MAIN_AGENT_STAYS);
        local.call("users:setMainAgent", &json!({ "agentId": id })).unwrap();
        assert_eq!(local.call("users:current", &json!({})).unwrap()["mainAgentId"], json!(id));
        local.call("agents:remove", &json!({ "id": DEFAULT_AGENT_ID })).unwrap();
        assert!(local.call("users:setMainAgent", &json!({ "agentId": "gone" })).is_err());
        let rows = local.call("agents:list", &json!({})).unwrap();
        assert_eq!(rows[0]["main"], true);
        assert_eq!(rows[0]["name"], "Tutor");
        assert_eq!(rows[0]["sessionId"], Value::Null);

        let session = local.call("agents:openChat", &json!({ "id": id })).unwrap();
        let session = session.as_str().unwrap().to_string();
        assert_eq!(local.call("agents:openChat", &json!({ "id": id })).unwrap(), json!(session));
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        assert!(
            chat.instructions.as_deref().unwrap().contains("You are Tutor")
                && chat.instructions.unwrap().contains("Teach me.")
        );

        local.call("agents:update", &json!({ "id": id, "name": "Coach", "instructions": "Push me.", "picture": "mint", "background": null })).unwrap();
        assert!(local.agent_chats.chat(&session).unwrap().unwrap().instructions.unwrap().contains("You are Coach"));

        // A conversation's messages and turns, read from its file.
        local.agent_chats.add_user_message(&session, "hi", "t-1").unwrap();
        local.agent_chats.append(&session, &Message::new(Role::Assistant, "hello", Some("t-1".into()))).unwrap();
        local.agent_chats.add_user_message(&session, "again", "t-2").unwrap();
        local
            .agent_chats
            .append(&session, &Message::new(Role::Error, "Claude Code isn't installed", Some("t-2".into())))
            .unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session, "limit": 50 })).unwrap();
        let roles: Vec<&str> =
            detail["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!(turns[0]["state"], "completed");
        assert_eq!(turns[1]["state"], "failed");
        assert_eq!(turns[1]["error"], "Claude Code isn't installed");
        let rows = local.call("agents:list", &json!({})).unwrap();
        assert_eq!((rows[0]["unseen"].as_u64(), rows[0]["replyFailed"].as_bool()), (Some(1), Some(true)));
        local.call("voiceSessions:markSeen", &json!({ "sessionId": session })).unwrap();
        assert_eq!(local.call("agents:list", &json!({})).unwrap()[0]["unseen"], 0);
        // A running turn says what its worker saves it's doing ("Running a command"); an ended one doesn't.
        local.agent_chats.add_user_message(&session, "list my files", "t-3").unwrap();
        filo_local::chat_activity::set(
            &local.agent_chats,
            &session,
            "t-3",
            Some(filo_local::chat_activity::RUNNING_A_COMMAND),
        );
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!((turns[2]["state"].as_str(), turns[2]["activity"].as_str()), (Some("completed"), None));
        local
            .running
            .lock()
            .unwrap()
            .insert("t-3".into(), Running { session_id: session.clone(), ..Running::default() });
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!(
            (turns[2]["state"].as_str(), turns[2]["activity"].as_str()),
            (Some("pending"), Some("Running a command"))
        );
        assert!(turns[1]["activity"].is_null());
        filo_local::chat_activity::set(&local.agent_chats, &session, "t-3", None);
        assert!(
            local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap()[2]["activity"].is_null()
        );
        local.running.lock().unwrap().remove("t-3");
        local.agent_chats.append(&session, &Message::new(Role::Assistant, "done", Some("t-3".into()))).unwrap();

        // The main agent goes once another is main.
        assert_eq!(local.call("agents:remove", &json!({ "id": id })).unwrap_err(), MAIN_AGENT_STAYS);
        let other = local.call("agents:create", &json!({ "name": "Coach", "instructions": "Push." })).unwrap();
        local.call("users:setMainAgent", &json!({ "agentId": other })).unwrap();
        local.call("agents:remove", &json!({ "id": id })).unwrap();
        assert!(local.agent_chats.chat(&session).unwrap().is_none());
        assert_eq!(local.call("agents:list", &json!({})).unwrap().as_array().unwrap().len(), 1);
        assert!(local.call("nothing:here", &json!({})).unwrap_err().contains("nothing:here"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn agents_are_made_only_with_a_coding_tool_plugged_in() {
        let local = temp();
        let plug = |tool: &str, plugged: bool| {
            local.call("coding:setPlugged", &json!({ "tool": tool, "plugged": plugged })).unwrap()
        };
        let create = |name: &str| local.call("agents:create", &json!({ "name": name, "instructions": "Teach me." }));
        let can_create = || local.call("agents:canCreate", &json!({})).unwrap();
        // Untouched, both tools are plugged in.
        assert_eq!(can_create(), json!({ "allowed": true, "reason": null }));
        let tutor = create("Tutor").unwrap();

        plug("claude-code", false);
        plug("codex", false);
        assert_eq!(can_create(), json!({ "allowed": false, "reason": NO_CODING_TOOL }));
        assert_eq!(create("Coach").unwrap_err(), NO_CODING_TOOL);
        assert!(NO_CODING_TOOL.contains("Coding page"));
        // The agents there are stay, and can still be opened and changed.
        let rows = local.call("agents:list", &json!({})).unwrap();
        let names: Vec<&str> = rows.as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Filo", "Tutor"]);
        assert!(local.call("agents:openChat", &json!({ "id": tutor })).unwrap().is_string());
        local.call("agents:update", &json!({ "id": tutor, "name": "Mentor" })).unwrap();

        // Either tool on is enough.
        for tool in ["codex", "claude-code"] {
            plug(tool, true);
            assert_eq!(can_create()["allowed"], true);
            create(tool).unwrap();
            plug(tool, false);
        }
        assert_eq!(local.call("agents:list", &json!({})).unwrap().as_array().unwrap().len(), 4);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn fresh_data_starts_with_filo_ready_to_chat() {
        let root = std::env::temp_dir().join(format!("filo-local-backend-{}", uuid::Uuid::new_v4()));
        let local = Local::open(root.clone());
        let rows = local.call("agents:list", &json!({})).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!((rows[0]["_id"].as_str(), rows[0]["name"].as_str()), (Some(DEFAULT_AGENT_ID), Some("Filo")));
        assert_eq!(rows[0]["picture"], crate::avatars::DEFAULT_AVATAR);
        assert!(rows[0]["instructions"].as_str().unwrap().contains("general assistant"));

        // Its conversation is a local agent conversation: Filo's instructions, following the engine picked in the tour.
        let session = local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap();
        let session = session.as_str().unwrap().to_string();
        let chat = local.agent_chats.chat(&session).unwrap().unwrap();
        assert_eq!(chat.title, "Filo");
        assert!(
            chat.instructions.as_deref().unwrap().contains("You are Filo")
                && chat.instructions.unwrap().contains("general assistant")
        );
        assert!(!chat.engine_chosen);
        local.agent_chats.add_user_message(&session, "hi", "t-1").unwrap();
        local.agent_chats.append(&session, &Message::new(Role::Assistant, "hello", Some("t-1".into()))).unwrap();

        // A restart keeps the one Filo and its conversation.
        drop(local);
        let local = Local::open(root.clone());
        let rows = local.call("agents:list", &json!({})).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["sessionId"], json!(session));
        assert_eq!(local.agent_chats.messages(&session).unwrap().len(), 2);

        // It's the main agent; once another is, removed, it stays removed.
        assert_eq!(local.call("users:current", &json!({})).unwrap()["mainAgentId"], DEFAULT_AGENT_ID);
        let tutor = local.call("agents:create", &json!({ "name": "Tutor", "instructions": "Teach me." })).unwrap();
        local.call("users:setMainAgent", &json!({ "agentId": tutor })).unwrap();
        local.call("agents:remove", &json!({ "id": DEFAULT_AGENT_ID })).unwrap();
        drop(local);
        let rows = Local::open(root.clone()).call("agents:list", &json!({})).unwrap();
        assert_eq!((rows.as_array().unwrap().len(), rows[0]["name"].as_str()), (1, Some("Tutor")));
        std::fs::remove_dir_all(&root).ok();

        // Data from before the main agent, with Filo removed: Filo comes back, first, as the main agent; the others stay
        // as they were.
        let root = std::env::temp_dir().join(format!("filo-local-backend-{}", uuid::Uuid::new_v4()));
        let mine = json!({
            "id": "mine",
            "name": "Tutor",
            "instructions": "Teach me.",
            "picture": "mint",
            "background": "#112233",
            "sessionId": "chat-1",
            "createdAt": 1.,
        });
        // As an older Filo kept them, in agents.json: copied into the database as it opens.
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("agents.json"), serde_json::to_vec(&vec![mine]).unwrap()).unwrap();
        let local = Local::open(root.clone());
        let rows = local.call("agents:list", &json!({})).unwrap();
        let names: Vec<&str> = rows.as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Filo", "Tutor"]);
        assert_eq!(
            (rows[0]["main"].as_bool(), rows[1]["sessionId"].as_str(), rows[1]["background"].as_str()),
            (Some(true), Some("chat-1"), Some("#112233"))
        );
        assert_eq!(local.call("users:current", &json!({})).unwrap()["mainAgentId"], DEFAULT_AGENT_ID);
        // Opened again, nothing more is added.
        drop(local);
        assert_eq!(Local::open(root.clone()).agents().len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn agents_from_a_newer_filo_are_listed_and_never_changed() {
        // An agent on an engine this build doesn't know, beside one it does: what an older Filo opened on a newer one's
        // data sees.
        let local = temp();
        let db = local.db().unwrap();
        app_data::remove_agent(db, DEFAULT_AGENT_ID).unwrap();
        let tutor = app_data::Agent {
            engine: Some("claude".into()),
            id: "tutor".into(),
            name: "Tutor".into(),
            created_at: 1.,
            ..local.filo_agent()
        };
        let later = app_data::Agent {
            engine: Some("some-future-engine".into()),
            model: Some("f-1".into()),
            session_id: Some("later-chat".into()),
            id: "later".into(),
            name: "Later".into(),
            created_at: 2.,
            ..local.filo_agent()
        };
        app_data::add_agent(db, &tutor).unwrap();
        app_data::add_agent(db, &later).unwrap();
        local.call("users:setMainAgent", &json!({ "agentId": "tutor" })).unwrap();
        // Both are listed, the newer one marked, with no engine this build would run it with.
        let rows = local.call("agents:list", &json!({})).unwrap();
        let listed: Vec<(&str, &Value, bool)> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (row["name"].as_str().unwrap(), &row["engine"], row["needsNewerFilo"].as_bool().unwrap()))
            .collect();
        assert_eq!(listed, [("Tutor", &json!("claude"), false), ("Later", &Value::Null, true)]);
        // Changing, removing, opening or messaging it says why not; the others' edits go ahead.
        for (path, args) in [
            ("agents:update", json!({ "id": "later", "name": "Renamed" })),
            ("agents:update", json!({ "id": "later", "engine": "claude" })),
            ("agents:remove", json!({ "id": "later" })),
            ("agents:openChat", json!({ "id": "later" })),
        ] {
            let error = local.call(path, &args).unwrap_err();
            assert!(error.contains("newer Filo"), "{path}: {error}");
        }
        local.call("agents:update", &json!({ "id": "tutor", "name": "Teacher" })).unwrap();
        local.call("agents:openChat", &json!({ "id": "tutor" })).unwrap();
        // Its row is as it was.
        assert_eq!(app_data::agent(db, "later").unwrap(), Some(later));
        // Its chat, made by the newer Filo, takes no message: typed, with Feed posts or notes, or said.
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        app_data::set_agent_session(db, "later", &chat).unwrap();
        for (path, args) in [
            ("voiceSessions:sendMessage", json!({ "sessionId": chat, "message": "Hi" })),
            (
                "voiceSessions:sendMessage",
                json!({ "sessionId": chat, "message": "Hi", "contextNote": "Title: Fado night" }),
            ),
            ("voiceRecordings:sendLive", json!({ "sessionId": chat, "durationMs": 1000, "transcript": "Hi" })),
        ] {
            let error = local.call(path, &args).unwrap_err();
            assert!(error.contains("newer Filo"), "{path}: {error}");
        }
        assert!(local.agent_chats.messages(&chat).unwrap().is_empty());
        assert_eq!(app_data::agent(db, "tutor").unwrap().unwrap().name, "Teacher");
        // And a restart doesn't drop it, or add Filo over the user's agents.
        let reopened = Local::open(local.root.clone());
        assert_eq!(reopened.agents().iter().map(|agent| agent.id.as_str()).collect::<Vec<_>>(), ["tutor", "later"]);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_first_run_ends_without_the_stand_in() {
        let main = |local: &Arc<Local>| local.call("users:current", &json!({})).unwrap()["mainAgentId"].clone();
        let names = |local: &Arc<Local>| {
            let rows = local.call("agents:list", &json!({})).unwrap();
            rows.as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        };
        // Skipped: no agents and no main agent, and none comes back on the next launch.
        let root = std::env::temp_dir().join(format!("filo-local-backend-{}", uuid::Uuid::new_v4()));
        let local = Local::open(root.clone());
        assert_eq!(local.call("agents:removeStandIn", &json!({})).unwrap(), json!(true));
        assert!(names(&local).is_empty() && local.main_agent().is_none());
        drop(local);
        let local = Local::open(root.clone());
        assert!(names(&local).is_empty());
        // The first agent made then is the main agent.
        let tutor = local.call("agents:create", &json!({ "name": "Tutor", "instructions": "Teach me." })).unwrap();
        local.call("agents:create", &json!({ "name": "Coach", "instructions": "Push." })).unwrap();
        assert_eq!((names(&local), main(&local)), (vec!["Tutor".to_string(), "Coach".to_string()], tutor));
        std::fs::remove_dir_all(&root).ok();

        // Made one: it's main, and it's the only agent.
        let local = temp();
        let builder = local.call("agents:create", &json!({ "name": "Builder", "instructions": "Build." })).unwrap();
        local.call("users:setMainAgent", &json!({ "agentId": builder })).unwrap();
        local.call("agents:removeStandIn", &json!({})).unwrap();
        assert_eq!((names(&local), main(&local)), (vec!["Builder".to_string()], builder));
        std::fs::remove_dir_all(&local.root).ok();

        // A Filo the user talked to stays.
        let local = temp();
        local.call("agents:openChat", &json!({ "id": DEFAULT_AGENT_ID })).unwrap();
        assert_eq!(local.call("agents:removeStandIn", &json!({})).unwrap(), json!(false));
        assert_eq!((names(&local), main(&local)), (vec!["Filo".to_string()], json!(DEFAULT_AGENT_ID)));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_sent_picture_is_kept_and_shown_from_its_copy() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let storage_id = local.save_attachment("image/png", b"png bytes").unwrap();
        let args = json!({ "attachments": [{ "storageId": storage_id, "name": "image1.png", "contentType": "image/png", "size": 9.0, "localPath": "/Users/me/shot.png" }] });
        let files = local.sent_files(&args).unwrap();
        assert_eq!((files[0].name.as_str(), files[0].size), ("image1.png", 9));
        assert_eq!(std::fs::read(&files[0].file).unwrap(), b"png bytes");
        local.agent_chats.add_user_message_with(&session, "", "t-1", files).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let shown = &detail["messages"][0]["attachments"][0];
        assert_eq!(
            (&shown["storageId"], &shown["name"], &shown["contentType"]),
            (&json!(storage_id), &json!("image1.png"), &json!("image/png"))
        );
        assert_eq!(shown["localPath"], "/Users/me/shot.png");
        assert!(std::path::Path::new(shown["localFile"].as_str().unwrap()).is_file());
        // A storage id this Mac didn't save, or too many files, is refused.
        assert!(local.sent_files(&json!({ "attachments": [{ "storageId": "../profile.json" }] })).is_err());
        let six = json!({ "attachments": vec![json!({ "storageId": storage_id }); 6] });
        assert_eq!(local.sent_files(&six).unwrap_err(), "Up to 5 files per message.");
        assert!(local.save_attachment("image/png", b"").is_err());
    }

    #[test]
    fn a_big_file_is_kept_whole_without_a_copy() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        // A 3 GB video (sparse: it takes no room on the disk), far over what a copy may be.
        let video = local.root.join("Trip.mov");
        std::fs::File::create(&video).unwrap().set_len(3 << 30).unwrap();
        let started = std::time::Instant::now();
        let storage_id = local.keep_attachment("video/quicktime", &video, true).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
        let args = json!({ "attachments": [{ "storageId": storage_id, "name": "Trip.mov", "contentType": "video/quicktime", "size": 0.0, "localPath": video }] });
        let files = local.sent_files(&args).unwrap();
        assert_eq!(files[0].size, 3 << 30);
        local.agent_chats.add_user_message_with(&session, "", "t-1", files).unwrap();
        // The original can move: the message keeps its clone.
        std::fs::rename(&video, local.root.join("Moved.mov")).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let shown = &detail["messages"][0]["attachments"][0];
        assert_eq!((&shown["contentType"], &shown["size"]), (&json!("video/quicktime"), &json!((3u64 << 30) as f64)));
        let kept = std::path::Path::new(shown["localFile"].as_str().unwrap());
        assert!(kept.is_file() && kept.extension().is_some_and(|extension| extension == "mov"), "{kept:?}");
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn an_agent_is_made_only_with_an_engine_and_model_there_are() {
        let local = temp();
        let id = local.call("agents:create", &json!({ "name": "Tutor", "instructions": "Teach me." })).unwrap();
        let bad = |pick: Value| {
            let mut args = json!({ "id": id, "name": "Coach" });
            args.as_object_mut().unwrap().extend(pick.as_object().unwrap().clone());
            local.call("agents:update", &args).unwrap_err()
        };
        assert!(bad(json!({ "engine": "gemini" })).contains("Not an engine"));
        assert!(bad(json!({ "engine": "codex", "model": "--yolo" })).contains("Not a model"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_picker_reuses_its_cache_and_a_refresh_updates_it() {
        let root = std::env::temp_dir().join(format!("filo-local-models-{}", uuid::Uuid::new_v4()));
        let row = |engine, id: &str, label: &str, default| ModelOption {
            engine,
            id: id.into(),
            label: label.into(),
            default,
            aliases: Vec::new(),
        };
        let opus = vec![
            row(Engine::Claude, "claude-opus-5-5", "Opus 5.5", true),
            row(Engine::Claude, "sonnet", "Sonnet 5.5", false),
        ];
        let codex = vec![row(Engine::Codex, "gpt-a", "GPT-A", true)];
        let now = 1_000_000_000;

        // A first run asks (nothing cached), saves what the CLIs said and shows it.
        let local = Local::open(root.clone());
        assert!(local.claim_model_refresh(now));
        assert!(!local.claim_model_refresh(now), "once per run");
        assert!(local.refresh_models_with(|| Some(opus.clone()), || codex.clone(), Vec::new, now));
        let shown: Vec<ModelOption> = opus.iter().chain(&codex).cloned().collect();
        assert_eq!(*local.all_model_options(), shown);
        drop(local);

        // Restarted: the cached rows and defaults straight away, and no asking while they're fresh.
        let local = Local::open(root.clone());
        assert_eq!(*local.all_model_options(), shown);
        assert!(!local.claim_model_refresh(now + 60_000));
        drop(local);

        // Restarted later: asked again in the background; the picker and the database get the new answer.
        let local = Local::open(root.clone());
        let later = now + MODELS_FRESH_FOR + 1;
        assert!(local.claim_model_refresh(later));
        let sonnet = vec![row(Engine::Claude, "claude-sonnet-5-5", "Sonnet 5.5", true)];
        assert!(local.refresh_models_with(|| Some(sonnet.clone()), || codex.clone(), Vec::new, later));
        assert_eq!(local.all_model_options()[0].label, "Sonnet 5.5");
        assert_eq!(models::cached(local.db().unwrap(), Engine::Claude).unwrap(), sonnet);
        // Claude Code couldn't be asked: its cached rows stay, and nothing changed.
        assert!(!local.refresh_models_with(|| None, || codex.clone(), Vec::new, later + 1));
        assert_eq!(models::cached(local.db().unwrap(), Engine::Claude).unwrap(), sonnet);
        drop(local);
        assert_eq!(Local::open(root.clone()).all_model_options()[0].label, "Sonnet 5.5");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_picker_lists_only_the_tools_signed_in() {
        let row = |engine, id: &str| ModelOption {
            engine,
            id: id.into(),
            label: id.into(),
            default: false,
            aliases: Vec::new(),
        };
        let rows = vec![row(Engine::Claude, "haiku"), row(Engine::Codex, "gpt-a"), row(Engine::OpenCode, "big")];
        let ids = |ready: &[Engine]| signed_in_rows(&rows, ready).into_iter().map(|row| row.id).collect::<Vec<_>>();
        assert_eq!(ids(&[Engine::Codex]), ["gpt-a"]);
        assert_eq!(ids(&[Engine::Claude, Engine::OpenCode]), ["haiku", "big"]);
        assert!(ids(&[]).is_empty());
    }

    /// The picker's rows from the CLIs installed here: `cargo test --bin filo installed_clis -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_installed_clis_name_their_models_and_defaults() {
        let local = temp();
        local.refresh_models(true);
        let rows = local.model_options();
        for row in rows.iter() {
            println!("{:?}\t{}\t{}{}", row.engine, row.id, row.label, if row.default { "\t(default)" } else { "" });
        }
        for engine in Engine::ALL {
            assert!(rows.iter().filter(|row| row.engine == engine && row.default).count() <= 1);
        }
    }

    #[test]
    fn every_listed_function_is_answered() {
        let local = temp();
        let lists = [
            MOVED,
            feed::FEED,
            scheduled::SCHEDULED,
            artifacts::ARTIFACTS,
            glances::GLANCES,
            apps::APPS,
            coding::CODING,
            handoff::HANDOFF,
            search::SEARCH,
        ];
        for path in lists.into_iter().flatten() {
            if let Err(error) = local.call(path, &json!({})) {
                assert!(!error.contains("isn't on this Mac yet"), "{path}: {error}");
            }
        }
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn notes_are_kept_in_the_database_across_restarts() {
        let local = temp();
        let mut changes = local.changes();
        let id = local
            .call("notes:createMine", &json!({ "title": " Groceries ", "body": "- [ ] milk", "tags": ["Home"] }))
            .unwrap();
        let id = id.as_str().unwrap().to_string();
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        let other = local
            .call("notes:createMine", &json!({ "title": "Ideas", "body": "A pet that naps", "pinned": true }))
            .unwrap();

        let note = local.call("notes:getMine", &json!({ "id": id })).unwrap();
        assert_eq!(
            (note["title"].as_str(), note["tags"].clone(), note["pinned"].as_bool()),
            (Some("Groceries"), json!(["home"]), Some(false))
        );
        assert!(note["updatedAt"].as_f64().is_some());
        assert_eq!(
            local.call("notes:updateMine", &json!({ "id": id, "title": "Groceries", "body": "- [x] milk" })).unwrap(),
            json!(true)
        );
        assert_eq!(local.call("notes:setPinnedMine", &json!({ "id": id, "pinned": true })).unwrap(), json!(true));
        assert!(changes.has_changed().unwrap());
        let found = local.call("notes:search", &json!({ "query": "MILK" })).unwrap();
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(local.call("notes:tags", &json!({})).unwrap(), json!([{ "tag": "home", "count": 1 }]));
        assert_eq!(local.call("notes:deleteMine", &json!({ "id": other })).unwrap(), json!(true));
        assert_eq!(local.call("notes:getMine", &json!({ "id": other })).unwrap(), Value::Null);
        assert_eq!(
            local.call("notes:updateMine", &json!({ "id": other, "title": "x", "body": "y" })).unwrap(),
            json!(false)
        );

        // Filo quits and opens again: the same notes.
        let root = local.root.clone();
        local.close();
        let local = Local::open(root);
        let notes = local.call("notes:mine", &json!({})).unwrap();
        let notes = notes.as_array().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0]["_id"].as_str(), notes[0]["body"].as_str(), notes[0]["pinned"].as_bool()),
            (Some(id.as_str()), Some("- [x] milk"), Some(true))
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A skill folder as a repository has one: front matter with more fields, a runnable script and references.
    fn repo_skill(at: &std::path::Path, name: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(at.join("scripts")).unwrap();
        std::fs::create_dir_all(at.join("references/deep")).unwrap();
        std::fs::write(at.join("SKILL.md"), format!("---\nname: {name}\ndescription: Record a demo video\nlicense: MIT\n---\n\n# Demo\n\nRun `scripts/record.sh`.\n")).unwrap();
        std::fs::write(at.join("scripts/record.sh"), "#!/bin/sh\necho recorded\n").unwrap();
        std::fs::set_permissions(at.join("scripts/record.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(at.join("scripts/parts.tar.gz"), [0x1f, 0x8b, 0, 0xff]).unwrap();
        std::fs::write(at.join("references/deep/notes.md"), "Notes.").unwrap();
    }

    #[test]
    fn skills_are_folders_the_page_reads_and_edits() {
        let local = temp();
        let changes = local.changes();
        assert_eq!(local.call("skills:list", &json!({})).unwrap(), json!([]));
        let saved = local.call("skills:save", &json!({ "name": "Weekly Report", "description": " When asked for a report ", "body": "1. Group by project.\n" })).unwrap();
        assert_eq!(saved, json!("weekly-report"));
        assert!(changes.has_changed().unwrap());
        local.call("skills:save", &json!({ "name": "release-notes", "description": "When writing release notes", "body": "Group by screen." })).unwrap();
        // Editing saves the same name again.
        local.call("skills:save", &json!({ "name": "weekly-report", "description": "When asked for a report", "body": "1. Group by project.\n2. Add dates." })).unwrap();
        let bad = local.call("skills:save", &json!({ "name": "x", "description": "", "body": "y" })).unwrap_err();
        assert!(bad.contains("needs a name"), "{bad}");
        // Each is a folder in the skills folder, and nothing of it in the database.
        let own = std::fs::read_to_string(local.root.join("skills/weekly-report/SKILL.md")).unwrap();
        assert!(
            own.starts_with("---\nname: weekly-report\ndescription: \"When asked for a report\"\n---\n")
                && own.ends_with("2. Add dates.\n"),
            "{own}"
        );
        let rows: i64 = local
            .db()
            .unwrap()
            .with(|conn| conn.query_row("SELECT count(*) FROM skills", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(rows, 0);
        // And a skill of Claude Code and Codex in the agents' workspace, edits included.
        let skill_file = |root: &std::path::Path, folder: &str, name: &str| {
            std::fs::read_to_string(root.join(folder).join(name).join("SKILL.md")).ok()
        };
        let workspaces = |local: &Local| [local.root.join("agents/workspace")];
        for workspace in workspaces(&local) {
            for folder in [".claude/skills", ".agents/skills"] {
                assert_eq!(skill_file(&workspace, folder, "weekly-report").as_ref(), Some(&own));
                assert!(skill_file(&workspace, folder, "release-notes").is_some());
            }
        }

        // A folder picked on the page comes in whole, its scripts with it.
        let picked = local.root.join("downloads/web-demo-video");
        repo_skill(&picked, "web-demo-video");
        assert_eq!(local.call("skills:import", &json!({ "path": picked })).unwrap(), json!("web-demo-video"));
        let again = local.call("skills:import", &json!({ "path": picked })).unwrap_err();
        assert!(again.contains("already a skill"), "{again}");
        for workspace in workspaces(&local) {
            assert!(workspace.join(".agents/skills/web-demo-video/scripts/record.sh").is_file());
        }

        // An agent copies one in with its own tools; changed while Filo was closed, the copies follow when it opens.
        repo_skill(&local.root.join("skills/installed"), "installed");
        let root = local.root.clone();
        local.close();
        std::fs::remove_dir_all(root.join("agents/workspace/.claude")).unwrap();
        let local = Local::open(root);
        assert!(skill_file(&local.root.join("agents/workspace"), ".claude/skills", "release-notes").is_some());
        assert!(local.root.join("agents/workspace/.claude/skills/installed/references/deep/notes.md").is_file());
        let list = local.call("skills:list", &json!({})).unwrap();
        let shape: Vec<(&str, &str, usize)> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|skill| {
                (
                    skill["name"].as_str().unwrap(),
                    skill["body"].as_str().unwrap(),
                    skill["files"].as_array().unwrap().len(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("installed", "# Demo\n\nRun `scripts/record.sh`.", 3),
                ("release-notes", "Group by screen.", 0),
                ("web-demo-video", "# Demo\n\nRun `scripts/record.sh`.", 3),
                ("weekly-report", "1. Group by project.\n2. Add dates.", 0),
            ]
        );
        assert_eq!(list[0]["folder"], json!(local.root.join("skills/installed")));
        assert_eq!(local.call("skills:remove", &json!({ "name": "release-notes" })).unwrap(), json!(true));
        // Removing one that's gone isn't an error (the page's smoke test clears its skill first).
        assert_eq!(local.call("skills:remove", &json!({ "name": "release-notes" })).unwrap(), json!(false));
        assert_eq!(local.call("skills:list", &json!({})).unwrap().as_array().unwrap().len(), 3);
        assert!(!local.root.join("skills/release-notes").exists());
        // Removed: gone from both CLIs, in both workspaces.
        for workspace in workspaces(&local) {
            for folder in [".claude/skills", ".agents/skills"] {
                assert!(
                    skill_file(&workspace, folder, "release-notes").is_none()
                        && skill_file(&workspace, folder, "weekly-report").is_some(),
                    "{folder}"
                );
            }
        }
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn skills_in_the_database_move_into_folders_when_filo_opens() {
        let root = std::env::temp_dir().join(format!("filo-local-backend-{}", uuid::Uuid::new_v4()));
        {
            let db = Db::open(&root.join(filo_local::db::FILE)).unwrap();
            db.write(|tx| {
                tx.execute(
                    "INSERT INTO skills (name, description, body, source, created_at, updated_at) VALUES ('use-fable', 'When coding', 'Use Fable.', 'owner', 1, 1)",
                    [],
                )
            })
            .unwrap();
        }
        let local = Local::open(root);
        assert_eq!(
            std::fs::read_to_string(local.root.join("skills/use-fable/SKILL.md")).unwrap(),
            "---\nname: use-fable\ndescription: \"When coding\"\n---\n\nUse Fable.\n"
        );
        for workspace in ["agents/workspace/.claude/skills", "agents/workspace/.agents/skills"] {
            assert!(local.root.join(workspace).join("use-fable/SKILL.md").is_file(), "{workspace}");
        }
        let rows: i64 = local
            .db()
            .unwrap()
            .with(|conn| conn.query_row("SELECT count(*) FROM skills", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(rows, 0);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn memories_are_listed_added_edited_and_forgotten_across_restarts() {
        let local = temp();
        let changes = local.changes();
        let page = |local: &Arc<Local>, cursor: Value| {
            local.call("memories:list", &json!({ "paginationOpts": { "numItems": 2, "cursor": cursor } })).unwrap()
        };
        assert_eq!(page(&local, Value::Null), json!({ "page": [], "isDone": true, "continueCursor": "" }));

        let tea = local.call("memories:create", &json!({ "content": " Likes green tea " })).unwrap();
        assert!(changes.has_changed().unwrap());
        std::thread::sleep(Duration::from_millis(3));
        local.call("memories:create", &json!({ "content": "Lives in Tashkent", "type": "fact" })).unwrap();
        std::thread::sleep(Duration::from_millis(3));
        local.call("memories:create", &json!({ "content": "Wants to run a marathon", "type": "goal" })).unwrap();
        assert!(local.call("memories:create", &json!({ "content": "" })).is_err());

        let first = page(&local, Value::Null);
        assert_eq!(first["isDone"], false);
        assert_eq!(first["page"][0]["content"], "Wants to run a marathon");
        assert_eq!(first["page"][0]["confirmation"], "confirmed");
        assert_eq!(first["page"][0]["provenance"]["explanation"], "You added this");
        let second = page(&local, first["continueCursor"].clone());
        assert_eq!(
            (second["page"][0]["content"].as_str(), second["isDone"].as_bool()),
            (Some("Likes green tea"), Some(true))
        );

        std::thread::sleep(Duration::from_millis(3));
        let edited = local.call("memories:update", &json!({ "id": tea, "content": "Likes oolong tea" })).unwrap();
        assert_eq!(edited, json!(true));
        let first = page(&local, Value::Null);
        assert_eq!(
            (first["page"][0]["content"].as_str(), first["page"][0]["confirmation"].as_str()),
            (Some("Likes oolong tea"), Some("corrected"))
        );
        let removed = first["page"][1]["_id"].clone();
        assert_eq!(local.call("memories:remove", &json!({ "id": removed })).unwrap(), json!(true));
        assert_eq!(local.call("memories:remove", &json!({ "id": removed })).unwrap(), json!(false));

        // Filo quits and opens again: the same memories.
        let root = local.root.clone();
        local.close();
        let local = Local::open(root);
        let all =
            local.call("memories:list", &json!({ "paginationOpts": { "numItems": 30, "cursor": null } })).unwrap();
        let contents: Vec<&str> =
            all["page"].as_array().unwrap().iter().map(|memory| memory["content"].as_str().unwrap()).collect();
        assert_eq!(contents, ["Likes oolong tea", "Lives in Tashkent"]);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_profile_has_defaults_and_keeps_changes() {
        let local = temp();
        let profile = local.call("users:current", &json!({})).unwrap();
        assert!(profile["userName"].as_str().is_some_and(|name| !name.is_empty()));
        local.call("users:updateUserName", &json!({ "userName": " Bex " })).unwrap();
        let profile = local.call("users:current", &json!({})).unwrap();
        assert_eq!(profile["userName"].as_str(), Some("Bex"));
        std::fs::remove_dir_all(&local.root).ok();
    }
}

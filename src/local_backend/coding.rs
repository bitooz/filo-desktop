// Coding on this Mac: the Coding page's `coding:*` functions, answered from Filo's database
// (crates/filo-local `coding`), and the tasks themselves, each round a run of the user's own coding tool.
//
// - Settings: each tool plugged in or not, its mode, model and reasoning level; where tasks work;
//   how freely Filo works on the computer; whether the Mac stays awake while tasks run (local_backend/awake.rs). Saved as they change, so they're there after a restart.
// - Projects: the git repos a walk of the home folder finds (`FILO_REPOS_ROOT` to walk another folder), done when the
//   app opens and every 10 minutes after, and folders the user adds, git or not.
// - Tasks: handed off by a conversation (start_work, local_backend/handoff.rs) on a project with a tool; the page shows
//   them and takes follow-ups. A round runs in a worker process (local_backend/workers.rs):
//   the tool in the project folder (or a new git worktree), its JSON output read into steps as it comes, each saved
//   and shown at once; Stop ends it. A follow-up on a finished task resumes the tool's session. Quitting leaves rounds
//   working while "Keep coding after Filo quits" is on, and the app picks them up when it opens again; a task left
//   running with no worker (Filo crashed with the setting off, or the Mac restarted) is marked stopped then. A
//   session that went cold since its last round (idle past the CLI's prompt cache, and big) is compacted before the
//   next round resumes it (`catch_up`, crates/filo-local `compaction`).
// - The tools: where each CLI is, its version and the account it's signed in with, read from the CLI and its own
//   files, kept a couple of minutes; how much of each one's plan is left (`coding:usage`, crates/filo-local
//   `plan_usage`), asked when the page opens and every couple of minutes while it's open; each one's newest version,
//   looked up about once a day, and its own updater run when the user clicks "Update to vX" (local_backend/tool_updates.rs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use filo_local::coding::{self, Folder, Step, Task};
use filo_local::compaction;
use filo_local::engine::{self, Engine};
use serde_json::{Value, json};

use super::{Local, Stoppable, arg, bool_arg, login_path, now_ms, required};

/// The functions answered here (checked by a test, like `MOVED`).
#[cfg(test)]
pub const CODING: &[&str] = &[
    "coding:overview",
    "coding:enabledTools",
    "coding:usage",
    "coding:tasks",
    "coding:projects",
    "coding:thread",
    "coding:setPlugged",
    "coding:updateTool",
    "coding:setToolMode",
    "coding:setToolModel",
    "coding:setToolEffort",
    "coding:setWorkspace",
    "coding:setKeepOnQuit",
    "coding:setKeepAwake",
    "coding:setKeepAwakeLid",
    "coding:keepingAwake",
    "coding:stopTask",
    "coding:continueTask",
    "coding:addProject",
    "coding:removeProject",
    "coding:refreshProjects",
];

/// How often the home folder is walked for repos again.
const RESCAN_EVERY: Duration = Duration::from_secs(10 * 60);
/// How long what the tools' CLIs said about themselves is kept.
const TOOLS_FRESH: Duration = Duration::from_secs(2 * 60);
/// Claude Code's reasoning levels (`claude --effort`).
pub(crate) const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A round under way.
#[derive(Default)]
struct Round {
    stop: Stoppable,
    /// In a worker process (local_backend/workers.rs): it carries on when Filo quits with "Keep coding after Filo quits".
    detached: Arc<AtomicBool>,
}

/// One tool's state, the same wherever it's read: the Coding page's overview, `coding:enabledTools`, and anything that
/// needs a tool before it goes on (creating an agent needs a coding tool enabled).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolState {
    /// "claude-code", "codex", "muse" or "opencode", as the Coding page names it.
    pub id: String,
    /// "claude", "codex", "muse" or "opencode", as conversations name it.
    pub engine: Engine,
    pub name: String,
    /// Plugged in on the Coding page (its switch under Tools; on until the user turns it off).
    pub enabled: bool,
    /// Its CLI is on this Mac.
    pub installed: bool,
    pub signed_in: bool,
    /// Enabled, installed, signed in, with coding on for this Mac: it can take a task now.
    pub ready: bool,
}

/// What `Local` keeps for coding.
pub struct State {
    rounds: Mutex<HashMap<String, Round>>,
    /// Held while conversations are told about their coding tasks (local_backend/handoff.rs `deliver_wakes`).
    pub(super) waking: Mutex<()>,
    /// Tests: the wake turns that would have started (conversation, turn), instead of running a coding tool.
    #[cfg(test)]
    pub(super) woken: Mutex<Vec<(String, String)>>,
    /// Where the walk looks for repos; none: it doesn't (tests).
    scan_root: Mutex<Option<PathBuf>>,
    scanning: AtomicBool,
    /// Each tool's newest version and its updates (local_backend/tool_updates.rs).
    pub(super) updates: super::tool_updates::State,
    /// The CLI each tool runs, when set (tests); otherwise found on the login shell's PATH.
    #[cfg(test)]
    programs: Mutex<HashMap<String, PathBuf>>,
}

impl Default for State {
    fn default() -> Self {
        let root = std::env::var_os("FILO_REPOS_ROOT")
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir);
        State {
            rounds: Mutex::new(HashMap::new()),
            waking: Mutex::new(()),
            #[cfg(test)]
            woken: Mutex::new(Vec::new()),
            scan_root: Mutex::new(if cfg!(test) { None } else { root }),
            scanning: AtomicBool::new(false),
            updates: Default::default(),
            #[cfg(test)]
            programs: Mutex::new(HashMap::new()),
        }
    }
}

/// The app opens: the walk for projects starts (then every 10 minutes), as does reading what the tools are. Once this
/// app holds `local/coding.lock` (at once, unless another Filo window on the same data has it), the turns and rounds
/// still running in workers are picked up, tasks left running with no worker are stopped, and the loop that starts
/// handed-off tasks runs.
pub fn start(local: Arc<Local>) {
    {
        let local = local.clone();
        std::thread::spawn(move || {
            let Some(lock) = owner_lock(&local) else { return };
            // "Keep running with the lid closed": its backstop pinned to this build (local_backend/lid.rs).
            local.lid_at_launch();
            let still_working = local.adopt_workers();
            if let Ok(db) = local.db()
                && coding::stop_interrupted(db, now_ms(), &still_working).is_ok_and(|count| count > 0)
            {
                local.changed();
            }
            // Tasks conversations hand off (local_backend/handoff.rs).
            super::handoff::start(local);
            // Held for as long as the app is open.
            std::mem::forget(lock);
        });
    }
    {
        let local = local.clone();
        std::thread::spawn(move || {
            refresh_tools();
            local.changed();
            // What Muse Code, OpenCode and pi offer, for their pickers (each starts a server of its own to say).
            for tool in ["muse", "opencode", "pi"] {
                let models = ask_tool_models(tool);
                if !models.is_empty() {
                    tool_models_cache().lock().unwrap().insert(tool.to_string(), (Instant::now(), models));
                    local.changed();
                }
            }
        });
    }
    std::thread::spawn(move || {
        loop {
            local.scan_projects();
            std::thread::sleep(RESCAN_EVERY);
        }
    });
}

/// One Filo runs this Mac's coding at a time: `local/coding.lock`, waited for while another window on the same data (a
/// scripted check) has it, so a second window never marks the first one's tasks stopped or starts their rounds again.
fn owner_lock(local: &Local) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(&local.root);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(local.root.join("coding.lock"))
        .ok()?;
    if !filo_local::lock::try_lock(&file) {
        eprintln!("coding: another Filo window runs this Mac's coding; this one waits for it to close");
        filo_local::lock::flock(&file, libc::LOCK_EX).ok()?;
    }
    Some(file)
}

/// Whether a Filo window has this data open: it holds `local/coding.lock` for as long as it is (`owner_lock`). Taken here
/// only for the moment of the look; a window opening right then waits that moment.
pub(super) fn app_open(local: &Local) -> bool {
    let Ok(file) =
        std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(local.root.join("coding.lock"))
    else {
        return false;
    };
    // Closing the file lets go.
    !filo_local::lock::try_lock(&file)
}

impl Local {
    pub(super) fn coding_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        // A conversation's tasks, their questions and round results (local_backend/handoff.rs).
        if let Some(result) = self.handoff_call(path, args) {
            return Some(result);
        }
        let now = now_ms();
        Some(match path {
            "coding:overview" => self.coding_overview(),
            // What's left of each signed-in tool's plan, by tool: `{ "claude-code": { fiveHour, week } }`.
            "coding:usage" => Ok(plan_usage()),
            // Which tools are enabled (and ready) for whatever needs one: `{ tools: [ToolState], anyEnabled, anyReady }`.
            "coding:enabledTools" => self.coding_tool_states().map(|tools| {
                let (any_enabled, any_ready) =
                    (tools.iter().any(|tool| tool.enabled), tools.iter().any(|tool| tool.ready));
                json!({ "tools": tools, "anyEnabled": any_enabled, "anyReady": any_ready })
            }),
            "coding:tasks" => {
                let limit = super::limit(args, coding::DEFAULT_LIMIT, 200);
                self.db().and_then(|db| {
                    let tasks = coding::tasks(db, limit)?;
                    let models = self.all_model_options();
                    let levels = DefaultLevels::default();
                    let pending = filo_local::coding_handoff::pending_counts(db)?;
                    let now = now_ms();
                    let row = |task: &Task| {
                        let pending = pending.get(&task.id).copied().unwrap_or(0);
                        let run_model = coding::run_model(db, &task.id).ok().flatten();
                        let mut row = task_row(task, run_model, &models, &levels, pending);
                        if let Some(worked) = filo_local::coding_handoff::worked_ms(db, &task.id)? {
                            row["workedMs"] = json!(worked as f64);
                        }
                        // Its state as its chat's row and result cards say it (a question with the user: Needs you).
                        if pending > 0 {
                            row["status"] = json!(super::handoff::task_status(db, task, now)?);
                        }
                        Ok(row)
                    };
                    Ok(Value::Array(tasks.iter().map(row).collect::<Result<_, String>>()?))
                })
            }
            "coding:projects" => self.db().and_then(|db| coding::projects(db, arg(args, "search"))).map(|projects| {
                Value::Array(
                    projects
                        .into_iter()
                        .map(|project| {
                            let online = Path::new(&project.path).is_dir();
                            let mut row = json!(project);
                            row["online"] = json!(online);
                            row
                        })
                        .collect(),
                )
            }),
            "coding:thread" => self.coding_thread(args),
            "coding:setPlugged" => (|| {
                let (tool, plugged) = (required(args, "tool")?, bool_arg(args, "plugged")?);
                self.write_db(|db| {
                    coding::update_tool(db, tool, |settings| {
                        let _: () = settings.plugged = plugged;
                        Ok(())
                    })
                })?;
                // Unplugging a tool stops its running tasks.
                if !plugged {
                    let running: Vec<String> = coding::tasks(self.db()?, 200)?
                        .into_iter()
                        .filter(|task| task.tool == tool && !coding::is_finished(&task.status))
                        .map(|task| task.id)
                        .collect();
                    for id in running {
                        self.stop_coding_task(&id)?;
                    }
                }
                Ok(Value::Null)
            })(),
            "coding:setToolMode" => (|| {
                let (tool, mode) = (required(args, "tool")?, required(args, "mode")?);
                self.write_db(|db| {
                    coding::update_tool(db, tool, |settings| {
                        let _: () = settings.mode = mode.to_string();
                        Ok(())
                    })
                })
            })(),
            "coding:setToolModel" => (|| {
                let (tool, model) = (required(args, "tool")?, arg(args, "model").unwrap_or_default());
                // A new model starts from its own reasoning level.
                self.write_db(|db| {
                    coding::update_tool(db, tool, |settings| {
                        settings.model = Some(model.to_string());
                        settings.effort = None;
                        Ok(())
                    })
                })
            })(),
            "coding:setToolEffort" => (|| {
                let (tool, effort) = (required(args, "tool")?, arg(args, "effort").unwrap_or_default());
                self.write_db(|db| {
                    coding::update_tool(db, tool, |settings| {
                        let _: () = settings.effort = Some(effort.to_string());
                        Ok(())
                    })
                })
            })(),
            "coding:setWorkspace" => required(args, "workspace")
                .and_then(|workspace| self.write_db(|db| coding::set_workspace(db, workspace))),
            "coding:setKeepOnQuit" => {
                bool_arg(args, "keep").and_then(|keep| self.write_db(|db| coding::set_keep_on_quit(db, keep)))
            }
            "coding:keepingAwake" => self.keeping_awake(),
            "coding:setKeepAwakeLid" => bool_arg(args, "keep").and_then(|keep| self.set_keep_awake_lid(keep)),
            "coding:setKeepAwake" => bool_arg(args, "keep")
                .and_then(|keep| self.write_db(|db| coding::set_keep_awake(db, keep)))
                .inspect(|_| self.keep_awake_changed()),
            "coding:stopTask" => required(args, "taskId").and_then(|id| self.stop_coding_task(id)).map(|_| Value::Null),
            "coding:continueTask" => (|| {
                let (id, message) = (required(args, "taskId")?, required(args, "message")?);
                if filo_local::coding_handoff::get(self.db()?, id)?.is_none() {
                    return Err(PAGE_TASK.into());
                }
                let task = coding::follow_up(self.db()?, id, message, now)?;
                self.changed();
                self.run_round(task.id.clone());
                Ok(json!({ "taskId": task.id }))
            })(),
            "coding:addProject" => (|| {
                let path = required(args, "path")?;
                let project = coding::add_project(self.db()?, Path::new(path), now)?;
                self.changed();
                Ok(json!(project))
            })(),
            "coding:removeProject" => {
                required(args, "path").and_then(|path| self.write_db(|db| coding::remove_project(db, path)))
            }
            // Runs the tool's own updater; the overview shows it updating, then its version or what went wrong.
            "coding:updateTool" => required(args, "tool").and_then(|tool| {
                let program = self.tool_program(tool).ok_or_else(|| {
                    let name = coding::engine_of(tool).map_or_else(|| tool.to_string(), |engine| engine.label().into());
                    format!("{name} isn't installed on this Mac.")
                })?;
                self.update_tool(tool, program)
            }),
            "coding:refreshProjects" => {
                let local = self.clone();
                std::thread::spawn(move || local.scan_projects());
                Ok(Value::Null)
            }
            _ => return None,
        })
    }

    /// The CLI a tool runs on this Mac, as last looked at.
    fn tool_program(&self, tool: &str) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(program) = self.coding.programs.lock().unwrap().get(tool) {
            return Some(program.clone());
        }
        tool_infos().into_iter().find(|info| info.id == tool)?.program
    }

    /// Each tool's state: enabled on the Coding page (saved in Filo's database), installed and signed in on this Mac.
    pub(crate) fn coding_tool_states(&self) -> Result<Vec<ToolState>, String> {
        let db = self.db()?;
        let infos = tool_infos();
        Ok(coding::tools(db)?
            .into_iter()
            .map(|settings| {
                let engine = coding::engine_of(&settings.id).unwrap_or(Engine::Claude);
                let (installed, signed_in) = installed_and_signed_in(infos.iter().find(|info| info.id == settings.id));
                ToolState {
                    id: settings.id,
                    engine,
                    name: engine.label().into(),
                    enabled: settings.plugged,
                    installed,
                    signed_in,
                    ready: settings.plugged && signed_in,
                }
            })
            .collect())
    }

    fn coding_overview(self: &Arc<Self>) -> Result<Value, String> {
        let db = self.db()?;
        let settings = coding::tools(db)?;
        let tools = tool_infos();
        let states = self.coding_tool_states()?;
        let model_rows = self.all_model_options();
        let rows: Vec<Value> = settings
            .iter()
            .map(|settings| {
                let info = tools
                    .iter()
                    .find(|info| info.id == settings.id)
                    .cloned()
                    .unwrap_or_else(|| ToolInfo::missing(&settings.id));
                let state = states.iter().find(|state| state.id == settings.id);
                let version = info.version.clone();
                let (installed, signed_in) =
                    (state.is_some_and(|state| state.installed), state.is_some_and(|state| state.signed_in));
                let mut row = json!({
                    "id": settings.id,
                    "plugged": settings.plugged,
                    "installedOn": if installed { vec!["This Mac"] } else { vec![] },
                    "signedInOn": if signed_in { vec!["This Mac"] } else { vec![] },
                    "ready": state.is_some_and(|state| state.ready),
                    "mode": settings.mode,
                    "models": match coding::engine_of(&settings.id) {
                        Some(Engine::Muse | Engine::OpenCode | Engine::Pi) => json!(tool_models(&settings.id)),
                        _ => json!(models_of(&settings.id, &model_rows)),
                    },
                });
                for (key, value) in [
                    ("account", info.account),
                    ("plan", info.plan),
                    ("version", info.version),
                    ("model", settings.model.clone()),
                    ("effort", settings.effort.clone()),
                ] {
                    if let Some(value) = value {
                        row[key] = json!(value);
                    }
                }
                if installed {
                    self.add_update_fields(&mut row, &settings.id, version.as_deref());
                }
                row
            })
            .collect();
        let project_count = coding::projects(db, None)?.len();
        let mut runner = json!({
            "name": "This Mac",
            "repoCount": project_count,
            "tools": tools.iter().map(|info| json!({ "id": info.id, "installed": info.program.is_some(), "signedIn": info.program.is_some() && info.signed_in })).collect::<Vec<_>>(),
        });
        if let Some(free) = free_disk_bytes() {
            runner["freeDiskBytes"] = json!(free);
        }
        let (lid, access) = (coding::keep_awake_lid(db)?, super::lid::access_installed());
        Ok(
            json!({ "tools": rows, "workspace": coding::workspace(db)?, "keepOnQuit": coding::keep_on_quit(db)?, "keepAwake": coding::keep_awake(db)?, "keepAwakeLid": lid, "keepAwakeLidAccess": access, "keepAwakeLidProblem": self.lid_status(lid, access), "runners": [runner] }),
        )
    }

    fn coding_thread(&self, args: &Value) -> Result<Value, String> {
        let db = self.db()?;
        let Some(task) = coding::task(db, required(args, "taskId")?)? else { return Ok(Value::Null) };
        let entries: Vec<Value> = coding::events(db, &task.id)?
            .into_iter()
            .map(|event| {
                let mut entry =
                    json!({ "key": event.id.to_string(), "at": event.at, "from": event.source, "text": event.text });
                if let Some(kind) = event.kind {
                    entry["kind"] = json!(kind);
                }
                entry
            })
            .collect();
        let run_model = coding::run_model(db, &task.id)?;
        let pending = filo_local::coding_handoff::pending_counts(db)?.get(&task.id).copied().unwrap_or(0);
        Ok(
            json!({ "task": task_row(&task, run_model, &self.all_model_options(), &DefaultLevels::default(), pending), "now": task.status, "entries": entries }),
        )
    }

    /// Stops a task: its running round ends (its process group goes), or one left waiting is marked stopped.
    pub(super) fn stop_coding_task(&self, id: &str) -> Result<(), String> {
        if let Some(round) = self.coding.rounds.lock().unwrap().get(id) {
            round.stop.stop();
            return Ok(());
        }
        let db = self.db()?;
        if coding::task(db, id)?.is_some_and(|task| !coding::is_finished(&task.status)) {
            coding::finish_round(db, id, "stopped", None, None, now_ms())?;
            self.changed();
        }
        Ok(())
    }

    /// Filo quits: every task's round stops, but those in workers when `keep` ("Keep coding after Filo quits").
    pub(super) fn stop_coding(&self, keep: bool) {
        for round in
            self.coding.rounds.lock().unwrap().values().filter(|round| !(keep && round.detached.load(Ordering::SeqCst)))
        {
            round.stop.stop();
        }
    }

    /// Runs the task's next round in a worker process (local_backend/workers.rs), else on a thread of its own, unless
    /// one is under way (the page and the hand-off loop may both start it).
    pub(super) fn run_round(self: &Arc<Self>, id: String) {
        let round = Round::default();
        let (stop, detached) = (round.stop.clone(), round.detached.clone());
        {
            let mut rounds = self.coding.rounds.lock().unwrap();
            if rounds.contains_key(&id) {
                return;
            }
            rounds.insert(id.clone(), round);
        }
        if let Some(worker) = self.start_worker(super::workers::Job::Round { task: id.clone() }, &stop) {
            detached.store(true, Ordering::SeqCst);
            return self.wait_for_round(worker, id);
        }
        let local = self.clone();
        std::thread::spawn(move || {
            local.round_here(&id, &stop);
            local.coding.rounds.lock().unwrap().remove(&id);
            local.changed();
        });
    }

    /// A worker's round this app didn't start (a previous run of the app, a terminal check): running here too, so Stop
    /// reaches it. False when it's known here already.
    pub(super) fn track_round(self: &Arc<Self>, worker: super::workers::Worker, id: String) -> bool {
        {
            let mut rounds = self.coding.rounds.lock().unwrap();
            if rounds.contains_key(&id) {
                return false;
            }
            let round = Round { stop: Stoppable::of(worker.stopper()), detached: Arc::new(AtomicBool::new(true)) };
            rounds.insert(id.clone(), round);
        }
        self.wait_for_round(worker, id);
        true
    }

    /// The round counts as running until its worker ends; then the conversation that handed it off hears how it went.
    fn wait_for_round(self: &Arc<Self>, worker: super::workers::Worker, id: String) {
        self.watch_turns();
        self.changed();
        let local = self.clone();
        std::thread::spawn(move || {
            worker.wait();
            local.coding.rounds.lock().unwrap().remove(&id);
            local.changed();
            local.deliver_wakes();
        });
    }

    /// Runs the task's next round here, blocking until it ends (in a worker, or on the app's thread); a round that
    /// couldn't start is saved as failed. The Mac stays awake meanwhile (local_backend/awake.rs).
    pub(super) fn round_here(self: &Arc<Self>, id: &str, stop: &Stoppable) {
        let title = self
            .db()
            .ok()
            .and_then(|db| coding::task(db, id).ok().flatten())
            .map(|task| task.title)
            .filter(|title| !title.is_empty());
        let _awake =
            self.keep_awake(title.map_or_else(|| "coding task".into(), |title| format!("coding task: {title}")));
        let result = self.round(id, stop);
        if let (Err(error), Ok(db)) = (result, self.db()) {
            let task = coding::task(db, id).ok().flatten();
            self.note_failure(
                "round",
                id,
                task.as_ref().and_then(|task| coding::engine_of(&task.tool)),
                task.and_then(|task| task.model),
                &error,
                Vec::new(),
            );
            let _ = coding::finish_round(db, id, "failed", Some(&error), None, now_ms());
            // A conversation that handed it off hears why.
            self.handoff_failed(id, &error);
        }
    }

    /// `coding:keepingAwake`, for the menu bar: whether "Keep Mac awake while tasks run" is on, and the work keeping it
    /// awake now from whichever process runs it (local_backend/awake.rs): coding tasks ("task:<id>") and turns
    /// ("turn:<id>": chat and agent replies, the group's, routine runs). That's this app's, every worker's (another Filo
    /// process may have started one) and the routine runs started in a process that's still there (the scheduler
    /// helper's), each once.
    fn keeping_awake(&self) -> Result<Value, String> {
        let db = self.db()?;
        let mut work: std::collections::BTreeSet<String> =
            self.coding.rounds.lock().unwrap().keys().map(|task| format!("task:{task}")).collect();
        work.extend(self.running.lock().unwrap().keys().map(|turn| format!("turn:{turn}")));
        work.extend(super::workers::running(&self.root).into_iter().map(|worker| match worker.job {
            super::workers::Job::Round { task } => format!("task:{task}"),
            super::workers::Job::Turn { turn, .. } => format!("turn:{turn}"),
        }));
        work.extend(filo_local::schedule::active_run_turns(db)?.into_iter().map(|turn| format!("turn:{turn}")));
        Ok(
            json!({ "keepAwake": coding::keep_awake(db)?, "keepAwakeLid": coding::keep_awake_lid(db)?, "running": work.len(), "work": work }),
        )
    }

    /// Rounds are under way (the turn watcher watches the database while they are).
    pub(super) fn coding_rounds_running(&self) -> bool {
        !self.coding.rounds.lock().unwrap().is_empty()
    }

    /// One round of a task a conversation handed off: makes the worktree on the first, then runs the tool
    /// (`handoff_round`). An error is the round failing before or as the tool started.
    fn round(self: &Arc<Self>, id: &str, stop: &Stoppable) -> Result<(), String> {
        let db = self.db()?;
        let mut task = coding::task(db, id)?.ok_or("This task isn't here anymore.")?;
        // Only conversations start tasks now; one an older Filo started on the Coding page can't run.
        let owner = filo_local::coding_handoff::get(db, id)?.ok_or(PAGE_TASK)?;
        let engine = coding::engine_of(&task.tool).ok_or_else(|| format!("Not a coding tool: {}", task.tool))?;
        #[cfg(test)]
        let program = self.coding.programs.lock().unwrap().get(&task.tool).cloned();
        #[cfg(not(test))]
        let program: Option<PathBuf> = None;
        let path_env = if program.is_some() { None } else { Some(login_path().to_string()) };
        let program = match program.or_else(|| engine::find_program(engine, path_env.as_deref())) {
            Some(program) => program,
            None => {
                return Err(format!(
                    "{} isn't installed on this Mac (no `{}` on PATH). Install it, then try again.",
                    engine.label(),
                    engine.program()
                ));
            }
        };
        if task.rounds == 0 && task.worktree && task.branch.is_none() {
            task = self.make_worktree(task)?;
        }
        if !Path::new(&task.work_path).is_dir() {
            return Err(format!("{} isn't there anymore.", task.work_path));
        }
        // The tool kept talking to, so it can ask (local_backend/handoff.rs).
        self.handoff_round(task, owner, engine, program, path_env, stop)
    }

    /// Before a round resumes the task's session: one that went cold since the last round (its prompt cache expired,
    /// and it's big) is compacted first, so the round doesn't send all of it again at full price
    /// (crates/filo-local `compaction`). Its thread says so, which the chat's card shows meanwhile as what it's doing.
    /// One that doesn't compact is resumed as it is; a Stop meanwhile reaches the round, which stops at once.
    pub(super) fn catch_up(
        &self,
        task: &Task,
        engine: Engine,
        program: &Path,
        path_env: Option<&str>,
        stop: &Stoppable,
    ) {
        let Some(session) = task.session.as_deref() else { return };
        let Some(last) = compaction::due(engine, session, now_ms()) else { return };
        let Ok(db) = self.db() else { return };
        let _ = coding::add_event(db, &task.id, now_ms(), &Step::marker("note", CATCHING_UP));
        self.changed();
        let spec = compaction::Spec {
            engine,
            program: program.to_path_buf(),
            session: session.to_string(),
            cwd: PathBuf::from(&task.work_path),
            path_env: path_env.map(str::to_string),
            model: task.model.clone(),
        };
        let outcome = compaction::run(&spec, |started| stop.started(started));
        let note = match (&outcome.error, outcome.stopped) {
            (_, true) => return,
            (Some(error), _) => format!("Couldn't catch up first ({error}): carrying on with the whole session"),
            (None, _) => caught_up(outcome.before.unwrap_or(last.tokens), outcome.after),
        };
        let _ = coding::add_event(db, &task.id, now_ms(), &Step::marker("note", note));
        self.changed();
    }

    /// A git project's task gets a worktree of its own on a new branch, beside Filo's data; when git can't make one (a
    /// repo with no commits yet), the task works in the project folder and says so.
    fn make_worktree(&self, task: Task) -> Result<Task, String> {
        let db = self.db()?;
        let dir = self.root.join("coding").join("worktrees").join(&task.id);
        let branch = coding::branch_name(&task.title, &task.id);
        let _ = std::fs::create_dir_all(dir.parent().unwrap_or(&self.root));
        let made = std::process::Command::new("git")
            .args(["-C", &task.project_path, "worktree", "add", "-b", &branch])
            .arg(&dir)
            .env("PATH", login_path())
            .stdin(std::process::Stdio::null())
            .output();
        let now = now_ms();
        match made {
            Ok(output) if output.status.success() => {
                coding::set_work(db, &task.id, &dir.display().to_string(), true, Some(&branch), now)?;
                coding::add_event(
                    db,
                    &task.id,
                    now,
                    &Step::marker("note", format!("Working in its own worktree, on branch {branch}")),
                )?;
            }
            other => {
                let why = match other {
                    Ok(output) => String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("git failed")
                        .trim()
                        .to_string(),
                    Err(error) => error.to_string(),
                };
                coding::set_work(db, &task.id, &task.project_path, false, None, now)?;
                coding::add_event(
                    db,
                    &task.id,
                    now,
                    &Step::marker("note", format!("Couldn't make a worktree ({why}): working in the project folder")),
                )?;
            }
        }
        self.changed();
        coding::task(db, &task.id)?.ok_or_else(|| "This task isn't here anymore.".into())
    }

    /// Walks for repos (one walk at a time) and saves what it found; the page hears when the list changed.
    fn scan_projects(&self) {
        let Some(root) = self.coding.scan_root.lock().unwrap().clone() else { return };
        if self.coding.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let found: Vec<Folder> = coding::find_repos(&root);
        if let Ok(db) = self.db() {
            match coding::replace_found(db, &found, now_ms()) {
                Ok(true) => self.changed(),
                Ok(false) => {}
                Err(error) => eprintln!("coding: couldn't save the projects: {error}"),
            }
        }
        self.coding.scanning.store(false, Ordering::SeqCst);
    }

    /// The reasoning level a chat's turn gets for `level`, the one picked in its composer: the nearest its engine's model
    /// takes (`nearest_effort`, from `chat_efforts`). None when its levels aren't known here: the tool's own then (or the
    /// Coding page's).
    pub(super) fn chat_effort(&self, engine: Engine, model: Option<&str>, level: &str) -> Option<String> {
        nearest_effort(level, &self.chat_efforts(engine, model))
    }

    /// The reasoning levels a chat's engine and model take, least first, as the Coding page learns them: Claude Code's
    /// own, Codex's catalogue, what Muse Code and OpenCode said about their models. The model is the chat's, else for Muse
    /// Code and OpenCode their tool's on the Coding page, else the one it answers with by default; for Codex without one,
    /// a level each of its models takes. None when they aren't known here (the composer then shows no picker).
    pub(crate) fn chat_efforts(&self, engine: Engine, model: Option<&str>) -> Vec<String> {
        let offered: Vec<String> = match engine {
            Engine::Claude => CLAUDE_EFFORTS.iter().map(|level| level.to_string()).collect(),
            Engine::Codex => {
                let catalogue = codex_efforts();
                match model.and_then(|model| catalogue.get(model)) {
                    Some((levels, _)) => levels.clone(),
                    None => {
                        let mut models = catalogue.values().map(|(levels, _)| levels);
                        let first = models.next().cloned().unwrap_or_default();
                        models.fold(first, |common, levels| {
                            common.into_iter().filter(|level| levels.contains(level)).collect()
                        })
                    }
                }
            }
            Engine::Muse | Engine::OpenCode | Engine::Pi => {
                let tool = coding::tool_of(engine);
                let model = model.map(str::to_string).or_else(|| {
                    self.db().ok().and_then(|db| coding::tool(db, tool).ok()).and_then(|settings| settings.model)
                });
                let models = tool_models(tool);
                let found = match &model {
                    Some(id) => models.iter().find(|row| &row.id == id),
                    None => models.iter().find(|row| row.is_default),
                };
                found.map(|row| row.efforts.clone()).unwrap_or_default()
            }
        };
        EFFORT_ORDER
            .iter()
            .filter(|level| offered.iter().any(|known| known == *level))
            .map(|level| level.to_string())
            .collect()
    }

    #[cfg(test)]
    pub(super) fn coding_round_running(&self, id: &str) -> bool {
        self.coding.rounds.lock().unwrap().contains_key(id)
    }

    #[cfg(test)]
    pub(super) fn set_coding_program(&self, tool: &str, program: PathBuf) {
        self.coding.programs.lock().unwrap().insert(tool.into(), program);
    }

    #[cfg(test)]
    fn set_scan_root(&self, root: PathBuf) {
        *self.coding.scan_root.lock().unwrap() = Some(root);
    }
}

/// A task's thread while its cold session is compacted (`Local::catch_up`).
const CATCHING_UP: &str = "Catching up on the session so far";

/// The thread's line once it's compacted: from how big to how big.
fn caught_up(before: u64, after: Option<u64>) -> String {
    match after {
        Some(after) => format!(
            "Caught up: the session went from {} to {}",
            compaction::tokens_label(before),
            compaction::tokens_label(after)
        ),
        None => format!("Caught up: the session's {} summarized", compaction::tokens_label(before)),
    }
}

/// Why a task an older Filo started on the Coding page (a "page task", from before only conversations started them)
/// can't run or take a follow-up.
const PAGE_TASK: &str = "An older Filo started this task on the Coding page, and Filo can't run those any more. Ask one of your agents to do it: it starts it again as its own task.";

/// The page's row for a task: the model's name (from the picker's rows, `Local::model_options`) and the reasoning level
/// as they read. The model is the one the tool said it started on (`run_model`), else the one chosen for the task; none
/// when neither is known, as its tool's current default may not be what the task got (`codingTaskRunsWith`). The level
/// is the task's, else the one its tool runs the model at (`DefaultLevels`), else "Default". `pending`: its requests
/// still waiting on an answer.
fn task_row(
    task: &Task,
    run_model: Option<String>,
    models: &[engine::ModelOption],
    levels: &DefaultLevels,
    pending: usize,
) -> Value {
    let engine = coding::engine_of(&task.tool);
    let model_id = run_model.or_else(|| task.model.clone());
    let reasoning = task
        .effort
        .clone()
        .or_else(|| levels.of(engine, model_id.as_deref()))
        .map_or_else(|| "Default".to_string(), |level| effort_label(&level));
    let model = model_id.map(|id| match engine {
        Some(Engine::Muse | Engine::OpenCode | Engine::Pi) => {
            tool_models(&task.tool).into_iter().find(|model| model.id == id).map_or(id, |model| model.name)
        }
        _ => models
            .iter()
            .find(|option| Some(option.engine) == engine && option.matches(&id))
            .map_or_else(|| coding::model_label(&id), |option| option.label.clone()),
    });
    coding::task_json(task, model, reasoning, pending)
}

/// The level each tool runs a model at when it's given none, its files read once for a page of tasks: Claude Code's
/// settings (the model's `modelSettings` level, else `effortLevel`), Codex's `config.toml` level, else its catalogue's
/// for the model, and what Muse Code, OpenCode and pi said of the model (else of their default one).
#[derive(Default)]
struct DefaultLevels {
    claude: std::cell::OnceCell<Value>,
    codex: std::cell::OnceCell<(Option<String>, HashMap<String, (Vec<String>, Option<String>)>)>,
}

impl DefaultLevels {
    fn of(&self, engine: Option<Engine>, model: Option<&str>) -> Option<String> {
        match engine? {
            Engine::Claude => {
                let settings = self.claude.get_or_init(|| {
                    dirs::home_dir()
                        .and_then(|home| std::fs::read(home.join(".claude/settings.json")).ok())
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                        .unwrap_or_default()
                });
                claude_level(settings, model)
            }
            Engine::Codex => {
                let (config, catalogue) = self.codex.get_or_init(|| (engine::codex_config_effort(), codex_efforts()));
                config.clone().or_else(|| catalogue.get(model?)?.1.clone())
            }
            tool @ (Engine::Muse | Engine::OpenCode | Engine::Pi) => {
                let models = tool_models(coding::tool_of(tool));
                let found = match model {
                    Some(id) => models.iter().find(|row| row.id == id),
                    None => models.iter().find(|row| row.is_default),
                };
                found?.default_effort.clone()
            }
        }
    }
}

/// How a reasoning level reads, and the levels in order (crates/filo-local coding.rs, which `start_work` takes too).
pub(crate) use filo_local::coding::{EFFORT_ORDER, effort_label};

/// What a tool offers, for its pickers: Claude Code's models with its reasoning levels, Codex's with the levels its
/// catalogue gives each.
fn models_of(tool: &str, models: &[engine::ModelOption]) -> Vec<Value> {
    let engine = coding::engine_of(tool);
    let catalogue = if engine == Some(Engine::Codex) { codex_efforts() } else { HashMap::new() };
    models
        .iter()
        .filter(|option| Some(option.engine) == engine)
        .map(|option| {
            let (efforts, default_effort) = match option.engine {
                Engine::Claude => (CLAUDE_EFFORTS.iter().map(|level| level.to_string()).collect(), None),
                Engine::Codex => catalogue.get(&option.id).cloned().unwrap_or_default(),
                Engine::Muse | Engine::OpenCode | Engine::Pi => (Vec::new(), None),
            };
            let mut row =
                json!({ "id": option.id, "name": option.label, "efforts": efforts, "isDefault": option.default });
            if let Some(level) = default_effort {
                row["defaultEffort"] = json!(level);
            }
            row
        })
        .collect()
}

/// How long what Muse Code and OpenCode said about their models is kept: asking starts a server of theirs.
const TOOL_MODELS_FRESH: Duration = Duration::from_secs(30 * 60);

fn tool_models_cache() -> &'static Mutex<HashMap<String, (Instant, Vec<coding::ToolModel>)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<coding::ToolModel>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Muse Code's or OpenCode's models, as it said last (none before it first has): asked again in the background once
/// that's half an hour old, one ask at a time.
fn tool_models(tool: &str) -> Vec<coding::ToolModel> {
    let cached = tool_models_cache().lock().unwrap().get(tool).cloned();
    if cached.as_ref().is_none_or(|(at, _)| at.elapsed() > TOOL_MODELS_FRESH) {
        static ASKING: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let mut asking = ASKING.lock().unwrap();
        if !asking.iter().any(|asked| asked == tool) {
            asking.push(tool.to_string());
            let tool = tool.to_string();
            std::thread::spawn(move || {
                let models = ask_tool_models(&tool);
                // A failed ask keeps what was said before, and asks again later.
                let at = if models.is_empty() {
                    Instant::now() - TOOL_MODELS_FRESH + Duration::from_secs(60)
                } else {
                    Instant::now()
                };
                let mut cache = tool_models_cache().lock().unwrap();
                let models = if models.is_empty() {
                    cache.get(&tool).map(|(_, models)| models.clone()).unwrap_or_default()
                } else {
                    models
                };
                cache.insert(tool.clone(), (at, models));
                ASKING.lock().unwrap().retain(|asked| *asked != tool);
            });
        }
    }
    cached.map(|(_, models)| models).unwrap_or_default()
}

fn ask_tool_models(tool: &str) -> Vec<coding::ToolModel> {
    let path = login_path();
    let Some(engine) = coding::engine_of(tool) else { return Vec::new() };
    let Some(program) = engine::find_program(engine, Some(path)) else { return Vec::new() };
    match engine {
        Engine::Muse => filo_local::coding_muse::models(&program, Some(path)),
        Engine::OpenCode => {
            filo_local::coding_opencode::models(&program, Some(path), &dirs::home_dir().unwrap_or_default())
        }
        Engine::Pi => filo_local::coding_pi::models(&program, Some(path)),
        Engine::Claude | Engine::Codex => Vec::new(),
    }
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".codex"))
}

/// Codex's reasoning levels for each of its models, and the one each starts with (`models_cache.json`).
fn codex_efforts() -> HashMap<String, (Vec<String>, Option<String>)> {
    let Ok(catalogue) = std::fs::read(codex_home().join("models_cache.json"))
        .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
    else {
        return HashMap::new();
    };
    catalogue["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let levels = model["supported_reasoning_levels"]
                .as_array()?
                .iter()
                .filter_map(|level| level["effort"].as_str().map(str::to_string))
                .filter(|level| engine::valid_model(level))
                .collect();
            Some((
                model["slug"].as_str()?.to_string(),
                (levels, model["default_reasoning_level"].as_str().map(str::to_string)),
            ))
        })
        .collect()
}

/// The level Claude Code's `settings` give `model`: its own under `modelSettings` (keyed by its full name, which its
/// dated and [1m] names start with and its alias is in), else `effortLevel`.
fn claude_level(settings: &Value, model: Option<&str>) -> Option<String> {
    let for_model = model.and_then(|model| {
        settings["modelSettings"].as_object()?.iter().find_map(|(name, set)| {
            (model.starts_with(name.as_str()) || name.contains(model)).then(|| set["effortLevel"].as_str())?
        })
    });
    for_model.or(settings["effortLevel"].as_str()).map(str::to_string)
}

/// `level` when `offered` has it, else the nearest below it there, else the lowest there; none from an empty `offered`.
pub(crate) fn nearest_effort(level: &str, offered: &[String]) -> Option<String> {
    if offered.iter().any(|known| known == level) {
        return Some(level.to_string());
    }
    let rank = |level: &str| EFFORT_ORDER.iter().position(|known| *known == level);
    let wanted = rank(level)?;
    let ranked: Vec<(usize, &String)> = offered.iter().filter_map(|known| Some((rank(known)?, known))).collect();
    let below = ranked.iter().filter(|(rank, _)| *rank < wanted).max_by_key(|(rank, _)| *rank);
    below.or_else(|| ranked.iter().min_by_key(|(rank, _)| *rank)).map(|(_, known)| known.to_string())
}

pub(crate) fn free_disk_bytes() -> Option<u64> {
    let home = std::ffi::CString::new(dirs::home_dir()?.into_os_string().into_encoded_bytes()).ok()?;
    // SAFETY: statvfs fills the struct for a NUL-terminated path.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(home.as_ptr(), &mut stats) } == 0).then(|| stats.f_bavail as u64 * stats.f_frsize)
}

// ---------------------------------------------------------------------------------------------------------------
// The tools on this Mac

/// A tool as this Mac has it.
#[derive(Clone, Debug, PartialEq)]
struct ToolInfo {
    id: String,
    program: Option<PathBuf>,
    version: Option<String>,
    signed_in: bool,
    account: Option<String>,
    plan: Option<String>,
}

impl ToolInfo {
    fn missing(id: &str) -> ToolInfo {
        ToolInfo { id: id.into(), program: None, version: None, signed_in: false, account: None, plan: None }
    }
}

fn tool_cache() -> &'static Mutex<Option<(Instant, Vec<ToolInfo>)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Vec<ToolInfo>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// What the tools are: read now the first time (it blocks while the CLIs answer), then kept, and read again in the
/// background once it's a couple of minutes old.
fn tool_infos() -> Vec<ToolInfo> {
    cached_tool_infos().unwrap_or_else(refresh_tools)
}

/// What the tools were at the last look, without waiting (an old look is read again in the background); None before
/// the first look has answered.
fn cached_tool_infos() -> Option<Vec<ToolInfo>> {
    let (at, infos) = tool_cache().lock().unwrap().clone()?;
    static REFRESHING: AtomicBool = AtomicBool::new(false);
    if at.elapsed() > TOOLS_FRESH && !REFRESHING.swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            refresh_tools();
            REFRESHING.store(false, Ordering::SeqCst);
        });
    }
    Some(infos)
}

/// How long the tools' plan usage is kept: the page asks when it opens and every couple of minutes.
const USAGE_FRESH: Duration = Duration::from_secs(30);

/// The plan windows each signed-in tool's account reports (crates/filo-local `plan_usage`), by tool id; a tool with
/// none isn't there. The tools asked at once, and kept half a minute: asks meanwhile wait for the one going.
fn plan_usage() -> Value {
    // `FILO_PLAN_USAGE_AFTER=<seconds>` (`dev` builds): the answer that much later, to see the pills while it's on its way.
    #[cfg(feature = "dev")]
    if let Some(seconds) = std::env::var("FILO_PLAN_USAGE_AFTER").ok().and_then(|text| text.parse::<f64>().ok()) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
    // `FILO_PLAN_USAGE=<json>` (`dev` builds): this answer instead, to see the Coding page's pills run low.
    #[cfg(feature = "dev")]
    if let Some(usage) = std::env::var("FILO_PLAN_USAGE").ok().and_then(|text| serde_json::from_str(&text).ok()) {
        return usage;
    }
    static KEPT: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
    let mut kept = KEPT.lock().unwrap();
    if let Some((_, usage)) = kept.as_ref().filter(|(at, _)| at.elapsed() < USAGE_FRESH) {
        return usage.clone();
    }
    let (infos, path, now) = (tool_infos(), login_path(), now_ms());
    let usage: serde_json::Map<String, Value> = std::thread::scope(|scope| {
        let asks: Vec<_> = infos
            .iter()
            .filter(|info| installed_and_signed_in(Some(info)).1)
            .filter_map(|info| {
                let (engine, program) = (coding::engine_of(&info.id)?, info.program.as_deref()?);
                let ask = scope.spawn(move || match engine {
                    Engine::Claude => filo_local::plan_usage::claude(now),
                    Engine::Codex => filo_local::plan_usage::codex(program, Some(path)),
                    Engine::Muse | Engine::OpenCode | Engine::Pi => None,
                });
                Some((info.id.clone(), ask))
            })
            .collect();
        asks.into_iter().filter_map(|(id, ask)| Some((id, json!(ask.join().ok()??)))).collect()
    });
    let usage = Value::Object(usage);
    *kept = Some((Instant::now(), usage.clone()));
    usage
}

/// Whether a tool is installed and signed in, as the Coding page shows it (`coding_tool_states`).
fn installed_and_signed_in(info: Option<&ToolInfo>) -> (bool, bool) {
    let installed = info.is_some_and(|info| info.program.is_some());
    (installed, installed && info.is_some_and(|info| info.signed_in))
}

/// The tools installed and signed in on this Mac, as the Coding page has them (Welcome's `tool_status` updates the same
/// look, `saw_sign_in`), without waiting: None before the first look has answered (`look_at_tools`). The model
/// picker lists only theirs (local_backend.rs `model_options`).
pub(crate) fn signed_in_engines() -> Option<Vec<Engine>> {
    let infos = cached_tool_infos()?;
    Some(
        Engine::ALL
            .into_iter()
            .filter(|engine| installed_and_signed_in(infos.iter().find(|info| info.id == coding::tool_of(*engine))).1)
            .collect(),
    )
}

/// Looks at the tools now unless that's been done: blocks while the CLIs answer, so off the UI thread. Looks asked for
/// at once wait for the first.
pub(crate) fn look_at_tools() {
    static LOOKING: Mutex<()> = Mutex::new(());
    let _looking = LOOKING.lock().unwrap();
    tool_infos();
}

/// Each tool's CLI, version, sign-in (`signed_in`) and the account its files name, the tools asked at once.
fn refresh_tools() -> Vec<ToolInfo> {
    let path = login_path();
    let read = |id: &'static str| {
        let engine = coding::engine_of(id).unwrap_or(Engine::Claude);
        let wanted = super::tool_status_wanted(engine);
        if wanted == Some(super::ToolStatus::NotInstalled) {
            return ToolInfo::missing(id);
        }
        let Some(program) = engine::find_program(engine, Some(path)) else { return ToolInfo::missing(id) };
        let version = crate::command::cli_version(&program, path);
        let signed_in =
            wanted.map_or_else(|| signed_in(engine, &program, path), |wanted| wanted == super::ToolStatus::Ready);
        let (_, account, plan) = match engine {
            Engine::Claude => claude_account(),
            Engine::Codex => codex_account(),
            Engine::Muse => muse_account(),
            Engine::OpenCode => opencode_account(),
            Engine::Pi => pi_account(),
        };
        ToolInfo { id: id.to_string(), program: Some(program), version, signed_in, account, plan }
    };
    let infos: Vec<ToolInfo> = std::thread::scope(|scope| {
        let reads: Vec<_> = coding::TOOLS.iter().map(|id| (*id, scope.spawn(move || read(id)))).collect();
        reads.into_iter().map(|(id, read)| read.join().unwrap_or_else(|_| ToolInfo::missing(id))).collect()
    });
    *tool_cache().lock().unwrap() = Some((Instant::now(), infos.clone()));
    infos
}

/// The installed tools' versions, by tool id, as last looked at (local_backend/tool_updates.rs looks for newer ones).
pub(super) fn tool_versions() -> Vec<(String, String)> {
    tool_infos()
        .into_iter()
        .filter(|info| info.program.is_some())
        .filter_map(|info| Some((info.id, info.version?)))
        .collect()
}

/// A tool's version read again after its update (`program` is its CLI), and what the Coding page has kept with it.
pub(super) fn tool_version_again(tool: &str, program: &Path) -> Option<String> {
    if cfg!(test) {
        return crate::command::cli_version(program, login_path());
    }
    refresh_tools().into_iter().find(|info| info.id == tool)?.version
}

/// A sign-in just checked (`tool_status`: Welcome, problem reports) goes into what the Coding page has kept.
pub(crate) fn saw_sign_in(engine: Engine, signed_in: bool) {
    if let Some((_, infos)) = tool_cache().lock().unwrap().as_mut() {
        for info in infos.iter_mut().filter(|info| info.id == coding::tool_of(engine) && info.program.is_some()) {
            info.signed_in = signed_in;
        }
    }
}

/// Whether a tool is signed in on this Mac: the one answer Welcome, the Coding page and problem reports give (they
/// used to disagree, one asking the CLI and the other reading its files). Claude Code and Codex are asked, given 10
/// seconds: `claude auth status` (JSON with `loggedIn`; an older Claude Code without it counts only on a clear yes) and
/// `codex login status` (exit 0 when signed in). Muse Code, OpenCode and pi have no status command: their files say.
pub(crate) fn signed_in(engine: Engine, program: &Path, path: &str) -> bool {
    let args: &[&str] = match engine {
        Engine::Claude => &["auth", "status"],
        Engine::Codex => &["login", "status"],
        Engine::Muse => return muse_account().0,
        Engine::OpenCode => return opencode_account().0,
        Engine::Pi => return pi_account().0,
    };
    let ran = crate::command::run(
        std::process::Command::new(program).args(args).env("PATH", path),
        None,
        Duration::from_secs(10),
    );
    match engine {
        Engine::Claude => ran
            .and_then(|ran| serde_json::from_slice::<Value>(&ran.stdout).ok())
            .and_then(|status| status.get("loggedIn")?.as_bool())
            .unwrap_or(false),
        _ => ran.is_some_and(|ran| ran.success),
    }
}

/// The account Claude Code keeps in `~/.claude.json` (and, for an older reading, an API key in the environment).
fn claude_account() -> (bool, Option<String>, Option<String>) {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()).map(PathBuf::from);
    let file = config_dir
        .map_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude.json"), |dir| dir.join(".claude.json"));
    let config: Value =
        std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
    claude_account_in(&config, std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|key| !key.is_empty()))
}

fn claude_account_in(config: &Value, api_key: bool) -> (bool, Option<String>, Option<String>) {
    let account = &config["oauthAccount"];
    let email = account["emailAddress"].as_str().filter(|email| !email.is_empty()).map(str::to_string);
    (email.is_some() || api_key, email, None)
}

/// The account and plan Codex's `auth.json` has from its ChatGPT sign-in, or an API key.
fn codex_account() -> (bool, Option<String>, Option<String>) {
    let auth: Value = std::fs::read(codex_home().join("auth.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    codex_account_in(&auth)
}

fn codex_account_in(auth: &Value) -> (bool, Option<String>, Option<String>) {
    use base64::Engine as _;
    let claims: Value = auth["tokens"]["id_token"]
        .as_str()
        .and_then(|token| token.split('.').nth(1))
        .and_then(|payload| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let email = claims["email"].as_str().map(str::to_string);
    let plan = claims["https://api.openai.com/auth"]["chatgpt_plan_type"].as_str().map(effort_label);
    let api_key = auth["OPENAI_API_KEY"].as_str().is_some_and(|key| !key.is_empty());
    (email.is_some() || api_key || auth["tokens"]["access_token"].is_string(), email, plan)
}

/// Muse Code's folder: `$MUSE_HOME`, else `$XDG_CONFIG_HOME/muse`, else `~/.config/muse`.
fn muse_home() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|dir| !dir.is_empty()).map(PathBuf::from);
    var("MUSE_HOME")
        .or_else(|| var("XDG_CONFIG_HOME").map(|dir| dir.join("muse")))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config/muse"))
}

/// Muse Code signed in: the Meta account `muse login` keeps in its `auth.json`, or `META_API_KEY`.
fn muse_account() -> (bool, Option<String>, Option<String>) {
    let auth: Value = std::fs::read(muse_home().join("auth.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    muse_account_in(&auth, std::env::var_os("META_API_KEY").is_some_and(|key| !key.is_empty()))
}

fn muse_account_in(auth: &Value, api_key: bool) -> (bool, Option<String>, Option<String>) {
    let meta = &auth["providers"]["meta"];
    let email = meta["user_email"].as_str().filter(|email| !email.is_empty()).map(str::to_string);
    (meta.is_object() || api_key, email, None)
}

/// OpenCode signed in: a provider it has credentials for (`opencode auth login`, kept in its `auth.json`), named as the
/// plan ("OpenRouter, Cline Pass").
fn opencode_account() -> (bool, Option<String>, Option<String>) {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"));
    let auth: Value = std::fs::read(data.join("opencode/auth.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    opencode_account_in(&auth)
}

fn opencode_account_in(auth: &Value) -> (bool, Option<String>, Option<String>) {
    let providers: Vec<String> = auth
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, credential)| credential.is_object())
        .map(|(id, _)| provider_label(id))
        .collect();
    (!providers.is_empty(), None, (!providers.is_empty()).then(|| providers.join(", ")))
}

/// pi signed in: a provider it has credentials for (`/login` in pi, kept in its `auth.json` in `$PI_CODING_AGENT_DIR`,
/// else `~/.pi/agent`), named as the plan ("OpenRouter, ChatGPT"). It has the same shape as OpenCode's.
fn pi_account() -> (bool, Option<String>, Option<String>) {
    let dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".pi/agent"));
    let auth: Value = std::fs::read(dir.join("auth.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    opencode_account_in(&auth)
}

/// "OpenRouter" for `openrouter`, "Cline Pass" for `cline-pass`: a provider id as it reads.
fn provider_label(id: &str) -> String {
    match id {
        "openai-codex" => "ChatGPT".into(),
        "openrouter" => "OpenRouter".into(),
        "openai" => "OpenAI".into(),
        "github-copilot" => "GitHub Copilot".into(),
        "opencode" => "OpenCode Zen".into(),
        other => other
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_backend::test_support::{call, script};

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("coding")
    }

    /// Waits for the task to be finished; its row.
    fn finished(local: &Arc<Local>, id: &str) -> Value {
        for _ in 0..200 {
            let thread = call(local, "coding:thread", json!({ "taskId": id })).unwrap();
            if coding::is_finished(thread["task"]["status"].as_str().unwrap())
                && !local.coding.rounds.lock().unwrap().contains_key(id)
            {
                return thread;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the task didn't finish");
    }

    /// A task a conversation hands off on `tool` in `folder` (start_work, coding_mcp.rs: in its own worktree when the
    /// page says so), its first round started; its id. The conversation is a chat of no agent's.
    fn hand_off(local: &Arc<Local>, tool: &str, folder: &Path, brief: &str) -> String {
        let db = local.db().unwrap();
        let project = coding::add_project(db, folder, 1).unwrap();
        let worktree = coding::workspace(db).unwrap() == "worktree";
        let task = coding::create_task(
            db,
            &coding::NewTask { brief: brief.into(), tool: tool.into(), project, worktree, owner: "Filo".into() },
            1,
        )
        .unwrap();
        let chat = local.agent_chats.create_chat(Engine::Claude).unwrap();
        let new = filo_local::coding_handoff::NewHandoff {
            chat_id: chat.id,
            agent_id: None,
            speaker: None,
            title: coding::title_of(brief),
            goal: brief.into(),
            done_when: "It's done".into(),
            notes: None,
            look_only: false,
            model: None,
            effort: None,
        };
        filo_local::coding_handoff::create(db, &task.id, &new, 1).unwrap();
        local.run_round(task.id.clone());
        task.id
    }

    /// The menu bar's count: a task counts while its round works, and not once it's done.
    #[test]
    fn a_running_task_counts_as_keeping_the_mac_awake() {
        let local = temp();
        let folder = local.root.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        let fake = script(
            &local.root,
            "claude",
            "#!/bin/sh\nread init\nread message\nsleep 2\necho '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done\"}'\nread rest\n",
        );
        local.set_coding_program("claude-code", fake);
        // Off until the user turns it on; the work counts either way.
        assert_eq!(
            call(&local, "coding:keepingAwake", json!({})).unwrap(),
            json!({ "keepAwake": false, "keepAwakeLid": false, "running": 0, "work": [] })
        );
        call(&local, "coding:setKeepAwake", json!({ "keep": true })).unwrap();
        assert_eq!(call(&local, "coding:keepingAwake", json!({})).unwrap()["keepAwake"], true);
        let id = hand_off(&local, "claude-code", &folder, "Wait");
        assert_eq!(call(&local, "coding:keepingAwake", json!({})).unwrap()["work"], json!([format!("task:{id}")]));
        call(&local, "coding:setKeepAwake", json!({ "keep": false })).unwrap();
        assert_eq!(call(&local, "coding:keepingAwake", json!({})).unwrap()["keepAwake"], false);
        finished(&local, &id);
        let deadline = Instant::now() + Duration::from_secs(5);
        while call(&local, "coding:keepingAwake", json!({})).unwrap()["running"] != 0 {
            assert!(Instant::now() < deadline, "the round still counts after it ended");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn settings_and_projects_are_kept_across_restarts() {
        let local = temp();
        let root = local.root.clone();
        let repos = root.join("home");
        std::fs::create_dir_all(repos.join("code/app/.git")).unwrap();
        std::fs::write(repos.join("code/app/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repos.join("plain")).unwrap();
        local.set_scan_root(repos.clone());
        call(&local, "coding:setPlugged", json!({ "tool": "codex", "plugged": false })).unwrap();
        call(&local, "coding:setToolMode", json!({ "tool": "claude-code", "mode": "full_access" })).unwrap();
        call(&local, "coding:setToolModel", json!({ "tool": "claude-code", "model": "opus" })).unwrap();
        call(&local, "coding:setToolEffort", json!({ "tool": "claude-code", "effort": "high" })).unwrap();
        call(&local, "coding:setWorkspace", json!({ "workspace": "checkout" })).unwrap();
        assert!(call(&local, "coding:setToolMode", json!({ "tool": "claude-code", "mode": "wild" })).is_err());
        local.scan_projects();
        call(&local, "coding:addProject", json!({ "path": repos.join("plain").display().to_string() })).unwrap();
        local.close();

        let local = Local::open(root.clone());
        let overview = call(&local, "coding:overview", json!({})).unwrap();
        let tool =
            |id: &str| overview["tools"].as_array().unwrap().iter().find(|tool| tool["id"] == id).unwrap().clone();
        assert_eq!(tool("codex")["plugged"], false);
        // The same state for whatever needs a tool enabled (creating an agent).
        let enabled = call(&local, "coding:enabledTools", json!({})).unwrap();
        let state =
            |id: &str| enabled["tools"].as_array().unwrap().iter().find(|tool| tool["id"] == id).unwrap().clone();
        assert_eq!(
            (state("codex")["enabled"].as_bool(), state("codex")["engine"].as_str(), state("codex")["ready"].as_bool()),
            (Some(false), Some("codex"), Some(false))
        );
        assert_eq!(
            (state("claude-code")["enabled"].as_bool(), state("claude-code")["name"].as_str()),
            (Some(true), Some("Claude Code"))
        );
        assert_eq!(enabled["anyEnabled"], true);
        for state in local.coding_tool_states().unwrap() {
            assert_eq!(tool(&state.id)["ready"].as_bool(), Some(state.ready));
        }
        assert_eq!(
            (
                tool("claude-code")["mode"].as_str(),
                tool("claude-code")["model"].as_str(),
                tool("claude-code")["effort"].as_str()
            ),
            (Some("full_access"), Some("opus"), Some("high"))
        );
        assert_eq!(overview["workspace"], "checkout");
        assert_eq!(overview["runners"][0]["name"], "This Mac");
        let projects = call(&local, "coding:projects", json!({})).unwrap();
        let names: Vec<(&str, bool, bool)> = projects
            .as_array()
            .unwrap()
            .iter()
            .map(|project| {
                (
                    project["name"].as_str().unwrap(),
                    project["plainFolder"].as_bool().unwrap(),
                    project["added"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(names, [("app", false, false), ("plain", true, true)]);
        assert_eq!(projects[0]["branch"], "main");
        assert_eq!(call(&local, "coding:projects", json!({ "search": "pla" })).unwrap().as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_task_runs_shows_its_steps_and_takes_a_follow_up() {
        let local = temp();
        let folder = local.root.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        // Says what Claude Code would (its stream-json session): its session, a step, a message and a result.
        let fake = script(
            &local.root,
            "claude",
            r#"#!/bin/sh
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]},"parent_tool_use_id":null}'
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Done in $(pwd)\"}]},\"parent_tool_use_id\":null}"
echo '{"type":"result","subtype":"success","is_error":false,"result":"Done","session_id":"s-1"}'
read rest
"#,
        );
        local.set_coding_program("claude-code", fake);
        let id = hand_off(&local, "claude-code", &folder, "List the files");
        let path = folder.canonicalize().unwrap().display().to_string();
        let thread = finished(&local, &id);
        assert_eq!(thread["task"]["status"], "done");
        // A plain folder: worked in as it is.
        assert_eq!(
            (thread["task"]["plainFolder"].as_bool(), thread["task"]["worktree"].as_bool()),
            (Some(true), Some(false))
        );
        let entries: Vec<(String, String)> = thread["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (entry["from"].as_str().unwrap().to_string(), entry["text"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(entries[0], ("owner".to_string(), "List the files".to_string()));
        assert!(entries.contains(&("activity".to_string(), "ls".to_string())), "{entries:?}");
        assert!(entries.contains(&("tool".to_string(), format!("Done in {path}"))), "{entries:?}");
        assert_eq!(entries.last().unwrap(), &("marker".to_string(), "Finished".to_string()));
        let tasks = call(&local, "coding:tasks", json!({})).unwrap();
        assert_eq!(
            (tasks[0]["taskId"].as_str(), tasks[0]["repoName"].as_str(), tasks[0]["owner"].as_str()),
            (Some(id.as_str()), Some("project"), Some("Filo"))
        );
        // Its questions waiting on an answer are counted on the page (the row's "N questions"), and not once answered.
        assert_eq!(tasks[0]["pending"], 0);
        let db = local.db().unwrap();
        let question =
            filo_local::coding_handoff::add_request(db, &id, 1, "question", "Which folder?", None, &[], now_ms())
                .unwrap();
        filo_local::coding_handoff::add_request(db, &id, 1, "approval", "Run `ls -a`?", None, &[], now_ms()).unwrap();
        assert_eq!(call(&local, "coding:tasks", json!({})).unwrap()[0]["pending"], 2);
        filo_local::coding_handoff::answer(db, &question, "This one", "user", now_ms()).unwrap();
        assert_eq!(call(&local, "coding:thread", json!({ "taskId": id })).unwrap()["task"]["pending"], 1);
        filo_local::coding_handoff::expire_pending(db, &id, now_ms()).unwrap();

        call(&local, "coding:continueTask", json!({ "taskId": id, "message": "And the hidden ones" })).unwrap();
        let thread = finished(&local, &id);
        assert_eq!(thread["task"]["rounds"], 2);
        let owner: Vec<&str> = thread["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["from"] == "owner")
            .map(|entry| entry["text"].as_str().unwrap())
            .collect();
        assert_eq!(owner, ["List the files", "And the hidden ones"]);
        assert_eq!(coding::task(local.db().unwrap(), &id).unwrap().unwrap().session.as_deref(), Some("s-1"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A task with Muse Code runs as a session: what it asks goes to the conversation that handed it off (a card),
    /// the answer goes back to it, and it finishes in its session with the model it said.
    #[test]
    fn a_muse_code_task_asks_and_finishes_in_its_session() {
        let local = temp();
        let folder = local.root.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        let fake = script(
            &local.root,
            "muse",
            r#"#!/bin/sh
expect() { while read -r line; do case "$line" in $1) return 0;; esac; done; exit 3; }
expect '*"method":"initialize"*'
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
expect '*"method":"session/start"*'
echo '{"jsonrpc":"2.0","id":2,"result":{"session":{"sessionId":"ms-1","modelId":"muse-spark-1.3"}}}'
expect '*"method":"turn/start"*List the files*'
echo '{"jsonrpc":"2.0","id":3,"result":{"turnId":"tu-1"}}'
echo '{"jsonrpc":"2.0","id":1,"method":"approval/request","params":{"approvalId":"ap-1","toolName":"bash","subject":{"kind":"shell","command":"rm -rf build"},"currentRequirementId":{"approvalId":"ap-1","sourceIndex":0},"availableChoices":[{"choiceId":"allow_once","decision":"approved","scope":"once"},{"choiceId":"abort","decision":"abort","scope":"once"}]}}'
expect '*"method":"approval/decide"*'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"itemId":"it-1","kind":"toolCall","tool":"bash","turnId":"tu-1","args":"{\"command\":\"ls\"}"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"itemId":"it-2","kind":"agentMessage","turnId":"tu-1","text":"Listed them; I left the build folder alone."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-1","terminal":"completed"}}'
cat > /dev/null
"#,
        );
        local.set_coding_program("muse", fake);
        call(&local, "coding:setPlugged", json!({ "tool": "muse", "plugged": true })).unwrap();
        let id = hand_off(&local, "muse", &folder, "List the files");
        let db = local.db().unwrap();
        let started = Instant::now();
        let request = loop {
            if let Some(request) = filo_local::coding_handoff::requests_for_task(db, &id)
                .unwrap()
                .into_iter()
                .find(|request| request.state == "pending")
            {
                break request;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "it didn't ask");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(request.kind, "approval");
        call(&local, "coding:answerRequest", json!({ "requestId": request.id, "answer": "deny" })).unwrap();
        let thread = finished(&local, &id);
        assert_eq!(thread["task"]["status"], "done", "{thread:#}");
        let texts: Vec<&str> =
            thread["entries"].as_array().unwrap().iter().map(|entry| entry["text"].as_str().unwrap()).collect();
        assert!(texts.contains(&"ls") && texts.contains(&"Listed them; I left the build folder alone."), "{texts:?}");
        let task = coding::task(db, &id).unwrap().unwrap();
        assert_eq!(
            (task.session.as_deref(), task.last_message.as_deref()),
            (Some("ms-1"), Some("Listed them; I left the build folder alone."))
        );
        assert_eq!(coding::run_model(db, &id).unwrap().as_deref(), Some("muse-spark-1.3"));
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_task_in_a_git_project_gets_its_own_worktree_and_can_be_stopped() {
        let local = temp();
        let repo = local.root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(["-C", &repo.display().to_string()])
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?}"
            )
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "first"]);
        let slow = script(
            &local.root,
            "claude",
            "#!/bin/sh\nread init\nread message\necho '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s-1\"}'\nsleep 30\n",
        );
        local.set_coding_program("claude-code", slow);
        let id = hand_off(&local, "claude-code", &repo, "Make it faster");
        // Working: running, in its worktree on its branch, with its session saved.
        let mut running = Value::Null;
        for _ in 0..200 {
            running = call(&local, "coding:thread", json!({ "taskId": id })).unwrap();
            if running["task"]["status"] == "running"
                && coding::task(local.db().unwrap(), &id).unwrap().unwrap().session.is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(running["task"]["status"], "running");
        assert_eq!(running["task"]["worktree"], true);
        let branch = running["task"]["branch"].as_str().unwrap().to_string();
        assert!(branch.starts_with("filo/make-it-faster-"), "{branch}");
        assert!(running["task"]["workPath"].as_str().unwrap().contains("worktrees"));
        call(&local, "coding:stopTask", json!({ "taskId": id })).unwrap();
        let thread = finished(&local, &id);
        assert_eq!(thread["task"]["status"], "stopped");
        assert_eq!(thread["entries"].as_array().unwrap().last().unwrap()["text"], "Stopped");
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A task whose session went cold since its last round has it compacted, then resumed with the follow-up (not the
    /// brief, which the compaction's notes in the thread used to hide it behind).
    #[test]
    fn a_cold_task_is_compacted_then_resumed_with_its_follow_up() {
        let local = temp();
        let folder = local.root.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        // A stand-in `claude` (its stream-json session): it notes its command lines and the message it was sent; a
        // `/compact` compacts.
        let fake = script(
            &local.root,
            "claude",
            r#"#!/bin/sh
echo "$*" >> "$0.calls"
read first
case "$first" in
*/compact*)
  printf '%s\n=====\n' "/compact" >> "$0.inputs"
  printf '%s\n' '{"type":"system","subtype":"compact_boundary","compact_metadata":{"pre_tokens":641100,"post_tokens":1973}}'
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":""}'
  exit 0
  ;;
esac
read message
printf '%s\n=====\n' "$message" >> "$0.inputs"
prev=""; for arg in "$@"; do case "$prev" in --session-id|--resume) session="$arg";; esac; prev="$arg"; done
printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session"
printf '{"type":"result","subtype":"success","is_error":false,"result":"Done.","session_id":"%s"}\n' "$session"
read rest
"#,
        );
        local.set_coding_program("claude-code", fake);
        let id = hand_off(&local, "claude-code", &folder, "Make the button blue");
        finished(&local, &id);
        let session = coding::task(local.db().unwrap(), &id).unwrap().unwrap().session.unwrap();
        compaction::pretend_last_call(
            &session,
            compaction::LastCall { at_ms: now_ms() - 2 * 3_600_000, tokens: 640_000, window: None },
        );
        call(&local, "coding:continueTask", json!({ "taskId": id, "message": "Carry on" })).unwrap();
        let thread = finished(&local, &id);
        let calls: Vec<String> =
            std::fs::read_to_string(local.root.join("claude.calls")).unwrap().lines().map(str::to_string).collect();
        let inputs: Vec<String> = std::fs::read_to_string(local.root.join("claude.inputs"))
            .unwrap()
            .split("\n=====\n")
            .map(str::to_string)
            .collect();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[1].starts_with(&format!("-p --resume {session} --input-format stream-json")), "{}", calls[1]);
        assert!(calls[2].contains(&format!("--resume {session}")), "{}", calls[2]);
        assert!(inputs[0].contains("Make the button blue") && inputs[1] == "/compact", "{inputs:?}");
        assert!(inputs[2].contains("Carry on") && !inputs[2].contains("Make the button blue"), "{}", inputs[2]);
        let notes: Vec<&str> = thread["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == "note")
            .map(|entry| entry["text"].as_str().unwrap())
            .collect();
        assert!(
            notes.contains(&CATCHING_UP)
                && notes.contains(&"Caught up: the session went from 641k tokens to 2k tokens"),
            "{notes:?}"
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_missing_tool_says_so() {
        let local = temp();
        let folder = local.root.join("p");
        std::fs::create_dir_all(&folder).unwrap();
        local.set_coding_program("codex", local.root.join("no-such-codex"));
        let id = hand_off(&local, "codex", &folder, "Hi");
        let thread = finished(&local, &id);
        assert_eq!(thread["task"]["status"], "failed");
        assert!(
            thread["entries"].as_array().unwrap().last().unwrap()["text"]
                .as_str()
                .unwrap()
                .starts_with("Failed: Couldn't start Codex"),
            "{:#}",
            thread["entries"]
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A task an older Filo started on the Coding page (no conversation handed it off) takes no follow-up, and a round
    /// of one left queued fails saying why.
    #[test]
    fn an_old_page_task_says_it_cant_run() {
        let local = temp();
        let folder = local.root.join("p");
        std::fs::create_dir_all(&folder).unwrap();
        let db = local.db().unwrap();
        let project = coding::add_project(db, &folder, 1).unwrap();
        let task = coding::create_task(
            db,
            &coding::NewTask {
                brief: "Hi".into(),
                tool: "claude-code".into(),
                project,
                worktree: false,
                owner: "You".into(),
            },
            1,
        )
        .unwrap();
        local.run_round(task.id.clone());
        let thread = finished(&local, &task.id);
        assert_eq!(thread["task"]["status"], "failed");
        assert!(
            thread["entries"].as_array().unwrap().last().unwrap()["text"].as_str().unwrap().contains("older Filo"),
            "{:#}",
            thread["entries"]
        );
        assert_eq!(
            call(&local, "coding:continueTask", json!({ "taskId": task.id, "message": "Again" })).unwrap_err(),
            PAGE_TASK
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// What the Coding page shows of each tool on this Mac (installed, signed in, version, account or providers, the
    /// models it offers): `cargo test --bin filo real_overview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_overview_lists_the_tools() {
        let local = temp();
        for tool in ["muse", "opencode"] {
            tool_models_cache().lock().unwrap().insert(tool.to_string(), (Instant::now(), ask_tool_models(tool)));
        }
        let overview = call(&local, "coding:overview", json!({})).unwrap();
        for tool in overview["tools"].as_array().unwrap() {
            let models: Vec<String> = tool["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|model| {
                    format!(
                        "{}{}",
                        model["name"].as_str().unwrap(),
                        if model["isDefault"] == true { " (default)" } else { "" }
                    )
                })
                .collect();
            println!(
                "{:<12} plugged {:<5} installed {:<5} signed in {:<5} ready {:<5} version {} · {} · {} models: {}",
                tool["id"].as_str().unwrap(),
                tool["plugged"],
                !tool["installedOn"].as_array().unwrap().is_empty(),
                !tool["signedInOn"].as_array().unwrap().is_empty(),
                tool["ready"],
                tool["version"].as_str().unwrap_or("-"),
                tool["account"].as_str().map_or("-", |_| "account shown"),
                tool["plan"].as_str().unwrap_or("-"),
                models.join(", ")
            );
        }
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_chats_reasoning_level_is_one_its_model_takes() {
        let levels = |all: &[&str]| all.iter().map(|level| level.to_string()).collect::<Vec<_>>();
        assert_eq!(nearest_effort("xhigh", &levels(&["low", "medium", "high", "xhigh"])).as_deref(), Some("xhigh"));
        // A model without the level gets the nearest below it, else its lowest.
        assert_eq!(nearest_effort("max", &levels(&["low", "medium", "high", "xhigh"])).as_deref(), Some("xhigh"));
        assert_eq!(nearest_effort("high", &levels(&["minimal", "medium"])).as_deref(), Some("medium"));
        assert_eq!(nearest_effort("low", &levels(&["medium", "high"])).as_deref(), Some("medium"));
        // Levels it doesn't know, or none to pick from: nothing.
        assert_eq!(nearest_effort("high", &[]), None);
        // What Claude Code runs a model at without --effort: its own level, else the one for all, else not known.
        let settings =
            json!({ "effortLevel": "medium", "modelSettings": { "claude-opus-5-5": { "effortLevel": "xhigh" } } });
        assert_eq!(claude_level(&settings, Some("claude-opus-5-5[1m]")).as_deref(), Some("xhigh"));
        assert_eq!(claude_level(&settings, Some("opus")).as_deref(), Some("xhigh"));
        assert_eq!(claude_level(&settings, Some("haiku")).as_deref(), Some("medium"));
        assert_eq!(claude_level(&json!({}), Some("opus")), None);
        assert_eq!(nearest_effort("high", &levels(&["turbo"])), None);
        let local = temp();
        assert_eq!(local.chat_effort(Engine::Claude, Some("opus"), "max").as_deref(), Some("max"));
        assert_eq!(local.chat_efforts(Engine::Claude, None), levels(&CLAUDE_EFFORTS));
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// Welcome and the Coding page ask the same: Claude Code's and Codex's own status commands.
    #[test]
    fn a_tool_is_signed_in_when_its_cli_says_so() {
        let dir = std::env::temp_dir().join(format!("filo-signed-in-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let claude = |answer: &str| {
            script(&dir, "claude", &format!("#!/bin/sh\n[ \"$1 $2\" = \"auth status\" ] && echo '{answer}'\n"))
        };
        let path = login_path();
        assert!(signed_in(Engine::Claude, &claude(r#"{"loggedIn":true,"authMethod":"claude.ai"}"#), path));
        assert!(!signed_in(Engine::Claude, &claude(r#"{"loggedIn":false}"#), path));
        // An older one that doesn't know `auth status`: not a clear yes.
        assert!(!signed_in(Engine::Claude, &claude("Unknown command"), path));
        let codex = |code: u8| {
            script(&dir, "codex", &format!("#!/bin/sh\n[ \"$1 $2\" = \"login status\" ] && exit {code}\nexit 9\n"))
        };
        assert!(signed_in(Engine::Codex, &codex(0), path));
        assert!(!signed_in(Engine::Codex, &codex(1), path));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_the_tools_accounts_and_versions() {
        assert_eq!(crate::command::version_in("2.1.296 (Claude Code)\n").as_deref(), Some("2.1.296"));
        assert_eq!(crate::command::version_in("codex-cli 0.162.0").as_deref(), Some("0.162.0"));
        assert_eq!(
            claude_account_in(&json!({ "oauthAccount": { "emailAddress": "me@x.com" } }), false),
            (true, Some("me@x.com".into()), None)
        );
        assert!(!claude_account_in(&json!({}), false).0);
        assert!(claude_account_in(&json!({}), true).0);
        use base64::Engine as _;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"email":"me@x.com","https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}}"#);
        let auth = json!({ "tokens": { "id_token": format!("h.{claims}.s"), "access_token": "a" } });
        assert_eq!(codex_account_in(&auth), (true, Some("me@x.com".into()), Some("Pro".into())));
        assert!(!codex_account_in(&Value::Null).0);
        let muse =
            json!({ "schema_version": 1, "providers": { "meta": { "mechanism": "oauth", "user_email": "me@x.com" } } });
        assert_eq!(muse_account_in(&muse, false), (true, Some("me@x.com".into()), None));
        assert!(!muse_account_in(&json!({}), false).0);
        assert!(muse_account_in(&json!({}), true).0);
        let opencode =
            json!({ "openrouter": { "type": "api", "key": "k" }, "cline-pass": { "type": "api", "key": "k" } });
        assert_eq!(opencode_account_in(&opencode), (true, None, Some("OpenRouter, Cline Pass".into())));
        assert!(!opencode_account_in(&json!({})).0);
        assert_eq!(crate::command::version_in("opencode v2.0.16").as_deref(), Some("2.0.16"));
        assert_eq!(crate::command::version_in("Muse Code 1.4.6 (1.4.6-R6000.1)").as_deref(), Some("1.4.6"));
    }
}

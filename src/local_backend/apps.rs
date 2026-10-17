// Extensions › Apps on this Mac: the user's MCP servers (crates/filo-local `apps`), answered from Filo's database with
// their secrets in the Keychain, and the work done for them off the UI thread: checking a server (starting or reaching
// it and listing its tools, apps_client.rs) and signing in to an account (apps_oauth.rs, which opens the browser).
// While one runs the app says so (`busy`), and when it ends the page hears of it like any other change.
//
// "Your computer" (`computer:appsOn`) is the same page's switch, kept in `local/computer.json` (filo_local `computer`,
// whose `use_computer` server reads it on every call). `computer:stop` stops Filo using the apps: each turn's helper
// closes and calls are refused for ten minutes, or until the switch is turned on again. `computer:status` says which
// apps Filo is using now, for the page's and the menu bar's Stop.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use filo_local::Engine;
use filo_local::apps::{self, App, Check, Fields};
use filo_local::computer::{self, Settings as ComputerSettings};
use filo_local::secrets::Secrets;
use filo_local::{apps_client, apps_oauth};
use serde_json::{Value, json};

use super::{Local, bool_arg, login_path, now_ms, required};

/// The functions answered here (checked by a test, like `MOVED`).
#[cfg(test)]
pub const APPS: &[&str] = &[
    "apps:list",
    "apps:connectPreset",
    "apps:add",
    "apps:update",
    "apps:setEnabled",
    "apps:setEngine",
    "apps:setAgentAccess",
    "apps:setAllAgents",
    "apps:sessionAgent",
    "apps:setToolEnabled",
    "apps:setToolsEnabled",
    "apps:setApiKey",
    "apps:connect",
    "apps:cancelConnect",
    "apps:check",
    "apps:refresh",
    "apps:signOut",
    "apps:remove",
    "computer:appsOn",
    "computer:setAppsOn",
    "computer:status",
    "computer:stop",
];

/// A remote app checked longer ago than this is checked again when the page asks (`apps:refresh`).
const RECHECK_AFTER_MS: i64 = 10 * 60 * 1000;

/// Work under way for an app.
struct Busy {
    /// `checking` or `signingIn`.
    kind: &'static str,
    cancel: Arc<AtomicBool>,
}

pub struct State {
    busy: Mutex<HashMap<String, Busy>>,
    secrets: Secrets,
}

impl Default for State {
    fn default() -> Self {
        #[cfg(test)]
        let secrets = Secrets::memory();
        #[cfg(not(test))]
        let secrets = Secrets::system();
        State { busy: Mutex::new(HashMap::new()), secrets }
    }
}

fn engine_arg(args: &Value) -> Result<Engine, String> {
    Engine::parse(required(args, "engine")?).ok_or_else(|| "`engine` is claude, codex, muse or opencode".into())
}

impl Local {
    /// Answers the Apps page's functions; `None` for any other path.
    pub(super) fn apps_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        let now = now_ms();
        Some(match path {
            "apps:list" => self.apps_list(),
            // A preset in one step, as a directory's Connect: the app with the preset's own settings, then its sign-in
            // in the browser (an account), its check (no sign-in, or the key it asks for) or, for one already added, its
            // reconnect. A key preset needs `apiKey`.
            "apps:connectPreset" => (|| {
                let preset = apps::preset(required(args, "preset")?).ok_or("That app isn't offered here.")?;
                let key = args.get("apiKey").and_then(Value::as_str).map(str::trim).filter(|key| !key.is_empty());
                let db = self.db()?;
                let secrets = &self.apps.secrets;
                let app = match apps::from_preset(db, preset.id)? {
                    Some(app) => app,
                    None => {
                        if preset.auth == apps::AUTH_KEY && key.is_none() {
                            return Err(format!("Paste your {} key to connect.", preset.name));
                        }
                        apps::add(db, &apps::preset_fields(preset), now)?
                    }
                };
                if let Some(key) = key
                    && let Err(error) = apps::set_api_key(db, secrets, &app.id, key, now)
                {
                    if app.created_at == now {
                        let _ = apps::remove(db, secrets, &app.id);
                    }
                    return Err(error);
                }
                self.changed();
                self.connect_app(&app.id);
                Ok(json!(apps::find(db, &app.id)?))
            })(),
            "apps:add" => (|| {
                let fields: Fields = serde_json::from_value(args.clone()).map_err(|error| error.to_string())?;
                let db = self.db()?;
                let app = apps::add(db, &fields, now)?;
                let secrets = &self.apps.secrets;
                let saved = (|| {
                    if let Some(key) = args.get("apiKey").and_then(Value::as_str).filter(|key| !key.trim().is_empty()) {
                        apps::set_api_key(db, secrets, &app.id, key, now)?;
                    }
                    if let Some(secret) =
                        args.get("clientSecret").and_then(Value::as_str).filter(|secret| !secret.trim().is_empty())
                    {
                        apps::set_client_secret(db, secrets, &app.id, secret, now)?;
                    }
                    Ok::<_, String>(())
                })();
                if let Err(error) = saved {
                    // Not half-added: without its key it would sit there waiting for one.
                    let _ = apps::remove(db, secrets, &app.id);
                    return Err(error);
                }
                self.changed();
                self.connect_app(&app.id);
                Ok(json!(apps::find(db, &app.id)?))
            })(),
            "apps:update" => (|| {
                let id = required(args, "id")?;
                let fields: Fields = serde_json::from_value(args.clone()).map_err(|error| error.to_string())?;
                let db = self.db()?;
                let secrets = &self.apps.secrets;
                self.cancel_app_work(id);
                let mut app = apps::update(db, secrets, id, &fields, now)?;
                if let Some(key) = args.get("apiKey").and_then(Value::as_str).filter(|key| !key.trim().is_empty()) {
                    app = apps::set_api_key(db, secrets, id, key, now)?;
                }
                if let Some(secret) = args.get("clientSecret").and_then(Value::as_str) {
                    app = apps::set_client_secret(db, secrets, id, secret, now)?;
                }
                self.changed();
                if app.status == apps::UNCHECKED && app.credentials_ready() {
                    self.check_app(id);
                } else if app.status == apps::UNCHECKED {
                    apps::record_check(db, id, &needs(&app), now)?;
                    self.changed();
                }
                Ok(json!(apps::find(db, id)?))
            })(),
            "apps:setEnabled" => (|| {
                let (id, on) = (required(args, "id")?, bool_arg(args, "enabled")?);
                self.write_db(|db| apps::set_enabled(db, id, on, now))
            })(),
            // Which coding tools may run it (engines, whoever the agent).
            "apps:setEngine" => (|| {
                let (id, engine, on) = (required(args, "id")?, engine_arg(args)?, bool_arg(args, "enabled")?);
                self.write_db(|db| apps::set_engine(db, id, engine, on, now))
            })(),
            // One Agents-tab agent (Filo among them) may use it or not. Taking one away from "every agent" leaves the
            // others that are there now; new agents then don't get it until allowed.
            "apps:setAgentAccess" => (|| {
                let (id, agent, on) = (required(args, "id")?, required(args, "agent")?, bool_arg(args, "enabled")?);
                let db = self.db()?;
                let app = apps::find(db, id)?;
                let mut allowed: Vec<String> =
                    if app.agent_scope == apps::SOME_AGENTS { app.agents.clone() } else { self.agent_ids() };
                allowed.retain(|known| known != agent);
                if on {
                    if app.agent_scope != apps::SOME_AGENTS {
                        return Ok(json!(app));
                    }
                    allowed.push(agent.to_string());
                }
                self.write_db(|db| apps::set_agent_access(db, id, false, &allowed, now))
            })(),
            // The agent a conversation is, for the composer's "@" list (Filo's chats are Filo; the group isn't asked).
            "apps:sessionAgent" => required(args, "sessionId").map(|session| match self.store_of(session) {
                Some(store) => json!(self.turn_agent(store, session)),
                None => json!(apps::FILO_AGENT),
            }),
            // Every agent, new ones too; or only those allowed now (switching to it keeps who has it).
            "apps:setAllAgents" => (|| {
                let (id, all) = (required(args, "id")?, bool_arg(args, "all")?);
                let app = apps::find(self.db()?, id)?;
                let agents =
                    if all || app.agent_scope == apps::SOME_AGENTS { app.agents.clone() } else { self.agent_ids() };
                self.write_db(|db| apps::set_agent_access(db, id, all, &agents, now))
            })(),
            "apps:setToolEnabled" => (|| {
                let (id, tool, on) = (required(args, "id")?, required(args, "tool")?, bool_arg(args, "enabled")?);
                self.write_db(|db| apps::set_tool_enabled(db, id, tool, on, now))
            })(),
            "apps:setToolsEnabled" => (|| {
                let (id, on) = (required(args, "id")?, bool_arg(args, "enabled")?);
                let tools: Vec<String> = serde_json::from_value(args.get("tools").cloned().unwrap_or(Value::Null))
                    .map_err(|_| "`tools` is a list of names".to_string())?;
                self.write_db(|db| apps::set_tools_enabled(db, id, &tools, on, now))
            })(),
            "apps:setApiKey" => (|| {
                let (id, key) = (required(args, "id")?, args.get("apiKey").and_then(Value::as_str).unwrap_or_default());
                let app = apps::set_api_key(self.db()?, &self.apps.secrets, id, key, now)?;
                self.changed();
                if app.has_secret {
                    self.check_app(id);
                }
                Ok(json!(app))
            })(),
            "apps:connect" => required(args, "id").map(|id| {
                self.connect_app(id);
                Value::Null
            }),
            "apps:cancelConnect" => required(args, "id").map(|id| {
                self.cancel_app_work(id);
                Value::Null
            }),
            "apps:check" => required(args, "id").map(|id| {
                self.check_app(id);
                Value::Null
            }),
            "apps:refresh" => self.refresh_apps(now).map(|_| Value::Null),
            "apps:signOut" => (|| {
                let id = required(args, "id")?;
                self.cancel_app_work(id);
                self.write_db(|db| apps::sign_out(db, &self.apps.secrets, id, now))
            })(),
            "apps:remove" => (|| {
                let id = required(args, "id")?;
                self.cancel_app_work(id);
                self.write_db(|db| apps::remove(db, &self.apps.secrets, id))
            })(),
            "computer:appsOn" => Ok(json!(self.read_json::<ComputerSettings>(computer::SETTINGS_FILE).apps_on)),
            "computer:setAppsOn" => (|| {
                let on = bool_arg(args, "on")?;
                self.update_json::<ComputerSettings, _>(computer::SETTINGS_FILE, |settings| {
                    settings.apps_on = on;
                    // Turning it on is asking again: a stop ends.
                    if on {
                        settings.stopped_until = 0;
                    }
                    Ok(())
                })?;
                Ok(json!(on))
            })(),
            "computer:status" => {
                let settings = self.read_json::<ComputerSettings>(computer::SETTINGS_FILE);
                let stopped_until = settings.stopped(now as u64).then_some(settings.stopped_until as f64);
                Ok(
                    json!({ "appsOn": settings.apps_on, "using": computer::using(&self.root), "stoppedUntil": stopped_until }),
                )
            }
            "computer:stop" => self
                .update_json::<ComputerSettings, _>(computer::SETTINGS_FILE, |settings| {
                    Ok(computer::stop(settings, now as u64))
                })
                .map(|until| json!(until as f64)),
            _ => return None,
        })
    }

    /// The page's data: the apps (with what's under way for each), the presets it can offer, and the two agents.
    fn apps_list(&self) -> Result<Value, String> {
        let list = apps::list(self.db()?)?;
        let busy = self.apps.busy.lock().unwrap();
        let rows: Vec<Value> = list
            .iter()
            .map(|app| {
                let mut row = json!(app);
                row["busy"] = json!(busy.get(&app.id).map(|busy| busy.kind));
                row["serverId"] = json!(app.server_name_for_cli());
                row["readTools"] = json!(app.read_tools());
                row["allowedTools"] = json!(
                    app.tools
                        .iter()
                        .filter(|tool| app.tool_enabled(&tool.name))
                        .map(|tool| tool.name.clone())
                        .collect::<Vec<_>>()
                );
                row
            })
            .collect();
        drop(busy);
        // A local preset needs its runtime (npx…) on this Mac; one already added isn't offered again.
        let presets: Vec<Value> = apps::PRESETS
            .iter()
            .filter(|preset| {
                preset.needs.is_empty() || apps_client::resolve_command(preset.needs, Some(login_path())).is_some()
            })
            .filter(|preset| !list.iter().any(|app| app.preset.as_deref() == Some(preset.id)))
            .map(|preset| json!(preset))
            .collect();
        let engines: Vec<Value> = Engine::ALL
            .iter()
            .map(|engine| json!({ "engine": engine, "name": engine.label(), "installed": super::installed(*engine) }))
            .collect();
        let agents: Vec<Value> = self
            .access_agents()
            .into_iter()
            .map(|agent| json!({ "id": agent.id, "name": agent.name, "picture": agent.picture, "background": agent.background, "isFilo": agent.id == apps::FILO_AGENT }))
            .collect();
        Ok(
            json!({ "apps": rows, "presets": presets, "engines": engines, "agents": agents, "fixedRedirect": apps_oauth::redirect_uri(apps_oauth::FIXED_PORT) }),
        )
    }

    /// The agent a turn in `session_id` speaks as, for the apps it may use: in an agent's conversation (its chats, its
    /// routines' and check-ins' runs) that agent; in Filo's own chats (a routine or check-in of no agent's) the Filo agent. A
    /// conversation whose agent is gone is no agent's: it gets only apps open to every agent.
    pub(super) fn turn_agent(&self, store: &filo_local::Store, session_id: &str) -> String {
        if store.root() != self.agent_chats.root() {
            return apps::FILO_AGENT.to_string();
        }
        self.agent_of(session_id).unwrap_or_default()
    }

    /// The agents an app's access lists: Filo first (its own chats and the group's `filo` are it, even when the user
    /// removed it from the Agents tab), then the Agents tab's in the order they were made.
    fn access_agents(&self) -> Vec<filo_local::app_data::Agent> {
        let mut agents = self.agents();
        agents.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        let filo = match agents.iter().position(|agent| agent.id == apps::FILO_AGENT) {
            Some(index) => agents.remove(index),
            None => self.filo_agent(),
        };
        agents.insert(0, filo);
        agents
    }

    fn agent_ids(&self) -> Vec<String> {
        self.access_agents().into_iter().map(|agent| agent.id).collect()
    }

    /// Signs in (an account app without its tokens) or checks the app, in the background.
    fn connect_app(self: &Arc<Self>, id: &str) {
        let Ok(Some(app)) = self.db().and_then(|db| apps::get(db, id)) else { return };
        if app.auth == apps::AUTH_OAUTH && app.transport == apps::HTTP && !(app.has_secret && app.status == apps::READY)
        {
            self.sign_in_app(id);
        } else {
            self.check_app(id);
        }
    }

    /// Claims the app for `kind` of work; false when something else runs for it.
    fn claim_app(&self, id: &str, kind: &'static str) -> Option<Arc<AtomicBool>> {
        let mut busy = self.apps.busy.lock().unwrap();
        if busy.contains_key(id) {
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        busy.insert(id.to_string(), Busy { kind, cancel: cancel.clone() });
        Some(cancel)
    }

    fn release_app(&self, id: &str) {
        self.apps.busy.lock().unwrap().remove(id);
        self.changed();
    }

    /// Stops a sign-in waiting for the browser (a check finishes on its own; its result is still recorded).
    fn cancel_app_work(&self, id: &str) {
        if let Some(busy) = self.apps.busy.lock().unwrap().get(id) {
            busy.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn check_app(self: &Arc<Self>, id: &str) {
        if self.claim_app(id, "checking").is_none() {
            return;
        }
        self.changed();
        let (local, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || {
            local.run_check(&id);
            local.release_app(&id);
        });
    }

    fn run_check(&self, id: &str) {
        let Ok(db) = self.db() else { return };
        let Ok(Some(app)) = apps::get(db, id) else { return };
        let check = apps_client::check(&app, &self.apps.secrets, &self.root, Some(login_path()));
        // Removed while it was being checked: nothing to record.
        if apps::get(db, id).ok().flatten().is_some() {
            let _ = apps::record_check(db, id, &check, now_ms());
        }
    }

    fn sign_in_app(self: &Arc<Self>, id: &str) {
        let Some(cancel) = self.claim_app(id, "signingIn") else { return };
        self.changed();
        let (local, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || {
            let open = |url: &str| {
                // Tests never open a real sign-in.
                if cfg!(test) {
                    return Err("No browser in tests.".to_string());
                }
                open::that(url).map_err(|error| format!("Couldn't open the browser: {error}"))
            };
            let result = local.db().and_then(|db| apps_oauth::sign_in(db, &local.apps.secrets, &id, &cancel, &open));
            match result {
                Ok(()) => {
                    // Signed in: now what it offers.
                    if let Some(busy) = local.apps.busy.lock().unwrap().get_mut(&id) {
                        busy.kind = "checking";
                    }
                    local.changed();
                    local.run_check(&id);
                }
                Err(error) if !cancel.load(Ordering::SeqCst) => {
                    if let Ok(db) = local.db()
                        && let Ok(Some(app)) = apps::get(db, &id)
                    {
                        let status = if app.has_secret { app.status.clone() } else { apps::NEEDS_SIGN_IN.to_string() };
                        let _ = apps::record_check(
                            db,
                            &id,
                            &Check { status, detail: error, tools: None, server_name: None },
                            now_ms(),
                        );
                    }
                }
                Err(_) => {}
            }
            local.release_app(&id);
        });
    }

    /// The page opened: remote apps not checked for a while are checked again (local ones only when asked: starting
    /// them can take a while).
    fn refresh_apps(self: &Arc<Self>, now: i64) -> Result<(), String> {
        for app in apps::list(self.db()?)? {
            // Waiting for a key or a sign-in: says so, without asking the server.
            if !app.credentials_ready() && app.status == apps::UNCHECKED {
                apps::record_check(self.db()?, &app.id, &needs(&app), now)?;
                self.changed();
                continue;
            }
            let stale = app.checked_at.is_none_or(|at| now - at > RECHECK_AFTER_MS);
            if app.enabled && app.transport == apps::HTTP && app.credentials_ready() && stale {
                self.check_app(&app.id);
            }
        }
        Ok(())
    }

    /// Every sign-in waiting for the browser stops (Filo quits).
    pub(super) fn stop_app_work(&self) {
        for busy in self.apps.busy.lock().unwrap().values() {
            busy.cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// What an app that can't be checked yet waits for.
fn needs(app: &App) -> Check {
    let detail = match app.auth.as_str() {
        apps::AUTH_OAUTH => "Sign in to your account to use it.",
        _ => "Add the API key to use it.",
    };
    Check { status: apps::NEEDS_SIGN_IN.into(), detail: detail.into(), tools: None, server_name: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_backend::test_support::call;

    #[test]
    fn turns_speak_as_the_conversations_agent_and_filos_chats_as_filo() {
        let root = std::env::temp_dir().join(format!("filo-local-apps-agents-{}", uuid::Uuid::new_v4()));
        let local = Local::open(root.clone());
        let chef = call(&local, "agents:create", json!({ "name": "Chef", "instructions": "Cooks." }));
        // Making an agent needs a coding tool plugged in on this Mac; without one, the default Filo agent shows it.
        let filo_session = call(&local, "agents:openChat", json!({ "id": apps::FILO_AGENT })).unwrap();
        assert_eq!(local.turn_agent(&local.agent_chats, filo_session.as_str().unwrap()), apps::FILO_AGENT);
        if let Ok(chef) = chef {
            let id = chef
                .as_str()
                .map(str::to_string)
                .or_else(|| chef["id"].as_str().map(str::to_string))
                .or_else(|| chef["_id"].as_str().map(str::to_string))
                .unwrap();
            let session = call(&local, "agents:openChat", json!({ "id": id })).unwrap();
            assert_eq!(local.turn_agent(&local.agent_chats, session.as_str().unwrap()), id);
        }
        // A conversation no agent has (any more) is no agent's.
        let chat = local.agent_chats.create_chat(filo_local::Engine::Claude).unwrap();
        assert_eq!(local.turn_agent(&local.agent_chats, &chat.id), "");
        assert_eq!(local.turn_agent(&local.agent_chats, "gone"), "");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn one_agent_is_taken_off_every_agent_and_put_back() {
        let root = std::env::temp_dir().join(format!("filo-local-apps-access-{}", uuid::Uuid::new_v4()));
        let local = Local::open(root.clone());
        let app = call(
            &local,
            "apps:add",
            json!({ "name": "Docs", "transport": "http", "url": "https://mcp.context7.com/mcp" }),
        )
        .unwrap();
        let id = app["id"].as_str().unwrap().to_string();
        assert_eq!(app["agentScope"], "all");
        // Off for Filo: the agents there now keep it, Filo doesn't, and a new agent won't get it.
        let app = call(&local, "apps:setAgentAccess", json!({ "id": id, "agent": apps::FILO_AGENT, "enabled": false }))
            .unwrap();
        assert_eq!(app["agentScope"], "some");
        assert!(!app["agents"].as_array().unwrap().iter().any(|agent| agent == apps::FILO_AGENT));
        let app = call(&local, "apps:setAgentAccess", json!({ "id": id, "agent": "chef", "enabled": true })).unwrap();
        assert_eq!(app["agents"], json!(["chef"]));
        let app = call(&local, "apps:setAllAgents", json!({ "id": id, "all": true })).unwrap();
        assert_eq!(app["agentScope"], "all");
        // Back to chosen agents: everyone who has it now (every agent there is) keeps it.
        let app = call(&local, "apps:setAllAgents", json!({ "id": id, "all": false })).unwrap();
        assert_eq!(app["agentScope"], "some");
        assert!(app["agents"].as_array().unwrap().iter().any(|agent| agent == apps::FILO_AGENT));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_preset_connects_in_one_step_with_its_own_settings() {
        let root = std::env::temp_dir().join(format!("filo-local-apps-presets-{}", uuid::Uuid::new_v4()));
        let local = Local::open(root.clone());
        // A key preset asks for the key and nothing else; without it nothing is added.
        let error = call(&local, "apps:connectPreset", json!({ "preset": "github" })).unwrap_err();
        assert!(error.contains("key"), "{error}");
        assert_eq!(call(&local, "apps:list", json!({})).unwrap()["apps"], json!([]));
        let github = call(&local, "apps:connectPreset", json!({ "preset": "github", "apiKey": " ghp_x " })).unwrap();
        assert_eq!(
            (
                github["auth"].as_str(),
                github["keyName"].as_str(),
                github["keyPrefix"].as_str(),
                github["hasSecret"].as_bool()
            ),
            (Some("api_key"), Some("Authorization"), Some("Bearer "), Some(true))
        );
        assert_eq!(
            local
                .apps
                .secrets
                .value(&filo_local::secrets::account(github["id"].as_str().unwrap(), apps::SECRET_KEY))
                .unwrap()
                .as_deref(),
            Some("ghp_x")
        );
        // Connecting it again is the same app, with every agent's access as it was.
        let id = github["id"].as_str().unwrap().to_string();
        call(&local, "apps:setAgentAccess", json!({ "id": id, "agent": apps::FILO_AGENT, "enabled": false })).unwrap();
        let again = call(&local, "apps:connectPreset", json!({ "preset": "github" })).unwrap();
        assert_eq!((again["id"].as_str(), again["agentScope"].as_str()), (Some(id.as_str()), Some("some")));
        // An account preset is an account sign-in at its own address (Linear's OAuth, not "No sign-in").
        let row = apps::preset_fields(apps::preset("linear").unwrap());
        assert_eq!((row.auth.as_str(), row.url.as_str()), (apps::AUTH_OAUTH, "https://mcp.linear.app/mcp"));
        assert!(call(&local, "apps:connectPreset", json!({ "preset": "nope" })).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn apps_are_listed_with_their_presets_and_agents_and_kept() {
        let root = std::env::temp_dir().join(format!("filo-local-apps-{}", uuid::Uuid::new_v4()));
        let local = Local::open(root.clone());
        let list = call(&local, "apps:list", json!({})).unwrap();
        assert_eq!(list["apps"], json!([]));
        assert!(list["presets"].as_array().unwrap().iter().any(|preset| preset["id"] == "linear"));
        let engines: Vec<&str> =
            list["engines"].as_array().unwrap().iter().map(|engine| engine["name"].as_str().unwrap()).collect();
        assert_eq!(engines, ["Claude Code", "Codex", "Muse Code", "OpenCode"]);
        // Fresh data has the Filo agent, which leads the access list.
        assert_eq!(list["agents"][0]["id"], apps::FILO_AGENT);
        assert_eq!(list["agents"][0]["isFilo"], true);

        // A key app added without its key waits for it; nothing is checked meanwhile.
        let github = call(&local, "apps:add", json!({ "name": "GitHub", "preset": "github", "transport": "http", "url": "https://api.githubcopilot.com/mcp/", "auth": "api_key" })).unwrap();
        let id = github["id"].as_str().unwrap().to_string();
        assert_eq!(github["hasSecret"], false);
        let list = call(&local, "apps:list", json!({})).unwrap();
        assert!(!list["presets"].as_array().unwrap().iter().any(|preset| preset["id"] == "github"));
        assert_eq!(list["apps"][0]["serverId"], "filo-github");

        let app = call(&local, "apps:setEngine", json!({ "id": id, "engine": "codex", "enabled": false })).unwrap();
        assert_eq!((app["claude"].as_bool(), app["codex"].as_bool()), (Some(true), Some(false)));
        let app = call(&local, "apps:setEnabled", json!({ "id": id, "enabled": false })).unwrap();
        assert_eq!(app["enabled"], false);
        assert!(call(&local, "apps:setEngine", json!({ "id": id, "engine": "gpt", "enabled": true })).is_err());

        // Kept across a restart.
        drop(local);
        let local = Local::open(root.clone());
        let list = call(&local, "apps:list", json!({})).unwrap();
        assert_eq!(
            (list["apps"][0]["name"].as_str(), list["apps"][0]["enabled"].as_bool()),
            (Some("GitHub"), Some(false))
        );
        assert_eq!(call(&local, "apps:remove", json!({ "id": id })).unwrap(), json!(true));
        assert_eq!(call(&local, "apps:list", json!({})).unwrap()["apps"], json!([]));

        assert_eq!(call(&local, "computer:appsOn", json!({})).unwrap(), json!(false));
        call(&local, "computer:setAppsOn", json!({ "on": true })).unwrap();
        assert_eq!(call(&local, "computer:appsOn", json!({})).unwrap(), json!(true));
        assert_eq!(
            call(&local, "computer:status", json!({})).unwrap(),
            json!({ "appsOn": true, "using": null, "stoppedUntil": null })
        );
        // Stop pauses the computer (the turns' servers read it) until turning it on again.
        call(&local, "computer:stop", json!({})).unwrap();
        assert!(call(&local, "computer:status", json!({})).unwrap()["stoppedUntil"].is_number());
        assert!(filo_local::computer::capability(&root, true).contains("paused"));
        call(&local, "computer:setAppsOn", json!({ "on": true })).unwrap();
        assert!(call(&local, "computer:status", json!({})).unwrap()["stoppedUntil"].is_null());
        std::fs::remove_dir_all(root).ok();
    }
}

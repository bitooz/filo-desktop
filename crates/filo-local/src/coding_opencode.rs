// A round with OpenCode (coding_session.rs): `opencode serve --stdio --port 0`, a private OpenCode 2 server for this
// round alone (https://opencode.ai/v2/docs). It prints the address it listens on (`{"url":…}`), takes HTTP Basic auth
// with the password it's given (`OPENCODE_PASSWORD`), and ends when its stdin closes. Its HTTP API:
//
// - `GET /api/event`: every event as server-sent events, read on a thread of its own; this round's carry its session.
// - `POST /api/session` in the task's folder, with its model and permission rules (or `GET /api/session/<id>` to carry
//   on in the task's session), then `POST /api/session/<id>/prompt` with the message. A level without a model of the
//   task's own goes on as the variant of the session's model (`POST /api/session/<id>/model`).
// - What it does: `session.tool.input.started` (a tool's name), `session.tool.called` (its input),
//   `session.tool.success` / `.failed`, `session.text.ended` (each message, whole).
// - A permission it needs: `permission.asked`, answered with `POST …/permission/<id>/reply` (once, always or reject).
//   A question (its `question` tool): `form.created`, answered with `POST …/form/<id>/reply`.
// - A note: one more prompt, delivered as `steer` (after its current step).
// - The turn's end: `session.execution.succeeded`, `.failed` (with why) or `.interrupted`.
//
// A chat turn's notes are the messages the user sends while it works (steer.rs), followed as every engine's are
// (coding_session.rs `Notes`). A prompt goes into the session's inbox (`session.inbox.enqueued`) and reaches the agent
// at its next step (`session.inbox.delivered`): the message is the turn's once it does something after, and an
// execution that ends before then is followed by one that takes it, which is waited for (then the prompt is taken
// back, and the message waits its own turn). A steer OpenCode keeps refusing (an error it answers with; not one that
// never reached it) interrupts the execution (`POST …/interrupt`), and a new prompt in the same session carries the
// message, after what the first one did.
//
// Not `opencode acp`: in OpenCode 2.0 it dismisses every question before anyone sees it, and refuses a second message
// while a prompt runs. Nor the user's background OpenCode service (`opencode run` without `--standalone` starts one,
// which outlives it and ignores what a round sets): a server of the round's own.
//
// OpenCode has no system prompt to give, so the instructions go in front of a new session's first message. How freely
// it works is the session's permission rules, complete for every action (one no rule matches is asked about): Full
// access allows everything; Auto allows its tools in the folder and asks before going outside it, as OpenCode does by
// default; Supervised asks before every command and edit; work that only looks can't edit. Its code mode (`execute`,
// scripts that call its other tools) is off, so what it does shows tool by tool. OpenCode keeps no todo list a round
// could show as its plan.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::ChildStdin;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

use crate::coding::{self, Step};
use crate::coding_session::{Ask, DENIED, Do, End, Host, Notes, Protocol, Spec};

/// The basic-auth user OpenCode's server takes.
const USER: &str = "opencode";

pub fn args() -> Vec<String> {
    ["serve", "--stdio", "--port", "0"].map(str::to_string).to_vec()
}

/// The server's password, made up for the round, and for a chat its MCP servers (`OPENCODE_CONFIG_CONTENT`, merged over
/// the user's own config).
pub fn env(password: &str, spec: Option<&Spec>) -> Vec<(String, String)> {
    let mut env = vec![("OPENCODE_PASSWORD".to_string(), password.to_string())];
    if let Some(config) = spec.and_then(chat_config) {
        env.push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string()));
    }
    env
}

/// A chat's MCP servers as OpenCode's config (its v2 shape, `mcp.servers`): Filo's and the user's apps, each a local
/// command. The ones the user set up in OpenCode itself load too, from their config: `keep_only_chat_servers` turns
/// them off for the round.
fn chat_config(spec: &Spec) -> Option<Value> {
    if spec.servers.is_empty() {
        return None;
    }
    let servers: serde_json::Map<String, Value> = spec
        .servers
        .iter()
        .map(|server| {
            let command: Vec<String> =
                std::iter::once(server.program.display().to_string()).chain(server.args.iter().cloned()).collect();
            (server.name.clone(), json!({ "type": "local", "command": command }))
        })
        .collect();
    Some(json!({ "mcp": { "servers": servers } }))
}

/// The actions OpenCode 2 asks permission for besides `shell` (its commands), `edit` (writing and patching too),
/// `external_directory` and `execute`.
const ACTIONS: [&str; 10] =
    ["read", "glob", "grep", "list", "webfetch", "websearch", "question", "subagent", "skill", "todowrite"];

/// The session's permission rules for the task's mode, last match winning.
fn permissions(spec: &Spec) -> Value {
    let rule = |action: &str, resource: &str, effect: &str| json!({ "action": action, "resource": resource, "effect": effect });
    let full = spec.mode == "full_access";
    let mut rules: Vec<Value> = ACTIONS.iter().map(|action| rule(action, "*", "allow")).collect();
    let (shell, edit) = match spec.mode.as_str() {
        "supervised" => ("ask", "ask"),
        "auto_accept_edits" => ("ask", "allow"),
        _ => ("allow", "allow"),
    };
    rules.push(rule("shell", "*", shell));
    rules.push(rule("edit", "*", if spec.look_only && !full { "deny" } else { edit }));
    if spec.mode == "supervised" {
        rules.push(rule("webfetch", "*", "ask"));
    }
    rules.push(rule("external_directory", "*", if full { "allow" } else { "ask" }));
    // The round's proof folder is outside its folder, and it may write there.
    for dir in &spec.writable {
        let dir = dir.display().to_string();
        for resource in [dir.clone(), format!("{dir}/*"), format!("{dir}/**")] {
            rules.push(rule("external_directory", &resource, "allow"));
        }
    }
    // Its code mode (`execute`) off for coding, so what it does shows tool by tool; a chat needs it, as its MCP
    // tools (Filo's, the apps') are only reached through it.
    rules.push(rule("execute", "*", if spec.servers.is_empty() { "deny" } else { "allow" }));
    // A chat: only Filo's skills (and the chart one and Filo's own it writes beside them), and questions go in its
    // reply, as nobody answers one mid-turn.
    if let Some(skills) = &spec.skills {
        rules.push(rule("skill", "*", "deny"));
        for name in skills
            .iter()
            .map(String::as_str)
            .chain(["dataviz"])
            .chain(crate::skills::FILO_SKILLS.iter().map(|(name, _)| *name))
        {
            rules.push(rule("skill", name, "allow"));
        }
        rules.push(rule("question", "*", "deny"));
    }
    Value::Array(rules)
}

/// A model as OpenCode names it, `provider/model` (and `#variant`), with the task's reasoning level as its variant
/// when it names none.
fn model_ref(model: &str, effort: Option<&str>) -> Option<Value> {
    let (model, variant) = match model.split_once('#') {
        Some((model, variant)) => (model, Some(variant)),
        None => (model, effort),
    };
    let (provider, id) = model.split_once('/')?;
    let mut reference = json!({ "providerID": provider, "id": id });
    if let Some(variant) = variant.filter(|variant| !variant.is_empty()) {
        reference["variant"] = json!(variant);
    }
    Some(reference)
}

/// The task's reasoning level as the variant of the model `session` (OpenCode's) has, when the task names no model of
/// its own (`model_ref` carries it then): none when the session has that variant already, or no model.
fn level_of(spec: &Spec, session: &Value) -> Option<Value> {
    let effort = spec.effort.as_deref().filter(|_| spec.model.is_none())?;
    let model = &session["model"];
    if model["variant"].as_str() == Some(effort) {
        return None;
    }
    Some(json!({ "providerID": model["providerID"].as_str()?, "id": model["id"].as_str()?, "variant": effort }))
}

/// What it asked, waiting on answers.
enum Waiting {
    Permission { id: String, key: String },
    Form { id: String, fields: Vec<(String, Vec<(String, String)>, String)>, answers: Vec<Option<String>> },
}

pub struct Talk<'a> {
    spec: &'a Spec,
    stdin: Option<ChildStdin>,
    password: String,
    http: Option<reqwest::blocking::Client>,
    url: Option<String>,
    events: Option<Receiver<Value>>,
    session: Option<String>,
    /// The message went: from now on, the session's end of execution ends the round.
    prompted: bool,
    /// Tool calls by id: their name, then their input.
    calls: HashMap<String, (String, Value)>,
    waiting: Vec<Waiting>,
    final_text: Option<String>,
    error: Option<String>,
    finished: bool,
    /// An event of the session's came since `heard` was last asked.
    heard: bool,
    /// A chat turn's notes: the messages the user sends while it works.
    notes: Option<Notes>,
    /// A chat turn's prompts in its inbox not yet delivered, by id (none when OpenCode gave none: the next delivery of a
    /// prompt not known here is its), with the notes they carry.
    inbox: Vec<(Option<String>, Vec<String>)>,
}

/// Why a call to OpenCode's API failed.
enum Failed {
    /// It answered with an error, saying why.
    Refused(String),
    /// It couldn't be reached, or its answer couldn't be read.
    Unreached(String),
}

impl<'a> Talk<'a> {
    pub fn new(spec: &'a Spec, stdin: Option<ChildStdin>, password: String) -> Self {
        Talk {
            spec,
            stdin,
            password,
            http: None,
            url: None,
            events: None,
            session: None,
            prompted: false,
            calls: HashMap::new(),
            waiting: Vec::new(),
            final_text: None,
            error: None,
            finished: false,
            heard: false,
            notes: spec.chat.then(|| Notes::new(true)),
            inbox: Vec::new(),
        }
    }

    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
        if let Some(notes) = self.notes.as_mut() {
            notes.fail();
        }
    }

    /// It did something: the notes it has are the chat turn's.
    fn acted(&mut self, host: &mut dyn Host) {
        if let Some(notes) = self.notes.as_mut() {
            notes.acted(host);
        }
    }

    /// A call to its API: the answer's `data`, or why it failed.
    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.request(method, path, body).map_err(|(Failed::Refused(why) | Failed::Unreached(why))| why)
    }

    /// `call`, saying whether OpenCode refused or wasn't reached.
    fn request(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, Failed> {
        let (Some(http), Some(url)) = (&self.http, &self.url) else {
            return Err(Failed::Unreached("OpenCode's server isn't up".into()));
        };
        let mut request = http.request(method, format!("{url}{path}")).basic_auth(USER, Some(&self.password));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().map_err(|error| Failed::Unreached(error.to_string()))?;
        let status = response.status();
        let text = response.text().map_err(|error| Failed::Unreached(error.to_string()))?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let why = value["message"]
                .as_str()
                .or(value["error"]["message"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{status} {}", text.trim()));
            return Err(Failed::Refused(why));
        }
        Ok(value.get("data").cloned().unwrap_or(value))
    }

    /// The server is up at `url`: its events start coming, and the session once they do.
    fn connect(&mut self, url: String) {
        let clients = (
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build(),
            reqwest::blocking::Client::builder().timeout(None).build(),
        );
        let (http, events) = match clients {
            (Ok(http), Ok(events)) => (http, events),
            (Err(error), _) | (_, Err(error)) => return self.fail(format!("OpenCode: {error}")),
        };
        let response = events
            .get(format!("{url}/api/event"))
            .basic_auth(USER, Some(&self.password))
            .header("Accept", "text/event-stream")
            .send();
        let response = match response {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => return self.fail(format!("OpenCode's events: {}", response.status())),
            Err(error) => return self.fail(format!("OpenCode's events: {error}")),
        };
        let (send, receive) = mpsc::channel();
        // `FILO_SESSION_TRACE=<file>`: its events go there too (coding_session.rs).
        let mut trace = crate::coding_session::session_trace();
        std::thread::spawn(move || {
            let mut data = String::new();
            for line in BufReader::new(response).lines().map_while(Result::ok) {
                if let Some(file) = trace.as_mut() {
                    let _ = std::io::Write::write_all(file, format!("{line}\n").as_bytes());
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    data.push_str(rest.trim_start());
                } else if line.is_empty()
                    && !data.is_empty()
                    && let Ok(event) = serde_json::from_str::<Value>(&std::mem::take(&mut data))
                    && send.send(event).is_err()
                {
                    return;
                }
            }
        });
        self.http = Some(http);
        self.url = Some(url);
        self.events = Some(receive);
    }

    /// A chat gets only its own MCP servers (as Claude Code's and Codex's do): once OpenCode has loaded the ones its
    /// config names, every other is disconnected, on this round's server alone (the user's config stays as it is).
    /// Waits up to 15 seconds for Filo's to connect.
    fn keep_only_chat_servers(&self) {
        let ours: Vec<&str> = self.spec.servers.iter().map(|server| server.name.as_str()).collect();
        let location = urlencode(&self.spec.cwd.display().to_string());
        let started = std::time::Instant::now();
        loop {
            let listed = self
                .call(reqwest::Method::GET, &format!("/api/mcp?location%5Bdirectory%5D={location}"), None)
                .unwrap_or(Value::Null);
            let servers = listed.as_array().cloned().unwrap_or_default();
            let status = |server: &Value| server["status"]["status"].as_str().unwrap_or_default().to_string();
            for server in &servers {
                let name = server["name"].as_str().unwrap_or_default();
                if !ours.contains(&name) && status(server) != "disabled" {
                    let _ = self.call(
                        reqwest::Method::POST,
                        &format!(
                            "/api/experimental/mcp/{}/disconnect?location%5Bdirectory%5D={location}",
                            urlencode(name)
                        ),
                        None,
                    );
                }
            }
            // Done once Filo's is up and every other is off (one still connecting comes back on if it's disconnected
            // too soon).
            let filo_ready =
                servers.iter().any(|server| server["name"] == crate::mcp::SERVER_NAME && status(server) != "pending");
            let others_off = servers.iter().all(|server| {
                ours.contains(&server["name"].as_str().unwrap_or_default()) || status(server) == "disabled"
            });
            if (filo_ready && others_off) || started.elapsed() > Duration::from_secs(15) {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Its events are coming: the session (the task's, or a new one) and the message.
    fn start(&mut self, host: &mut dyn Host) {
        if !self.spec.servers.is_empty() {
            self.keep_only_chat_servers();
        }
        let model = self.spec.model.as_deref().and_then(|model| model_ref(model, self.spec.effort.as_deref()));
        if let Some(session) = self.spec.resume.clone() {
            match self.call(reqwest::Method::GET, &format!("/api/session/{session}"), None) {
                Ok(found) => {
                    self.session = Some(session.clone());
                    host.session(&session);
                    if let Some(model) = model.or_else(|| level_of(self.spec, &found)) {
                        self.set_model(&session, model, host);
                    }
                    let message = self.spec.message.clone();
                    let _ = self.prompt(&message, None);
                    return;
                }
                // A chat starts afresh with its context instead (turn.rs).
                Err(why) if self.spec.chat => return self.fail(format!("OpenCode: session not found ({why})")),
                Err(why) => host.step(Step::marker(
                    "note",
                    format!("Couldn't carry on in its earlier OpenCode session ({why}): starting a new one"),
                )),
            }
        }
        let mut body = json!({ "location": { "directory": self.spec.cwd.display().to_string() }, "permissions": permissions(self.spec) });
        if let Some(model) = model {
            body["model"] = model;
        }
        match self.call(reqwest::Method::POST, "/api/session", Some(body)) {
            Ok(session) => match session["id"].as_str() {
                Some(id) => {
                    let id = id.to_string();
                    self.session = Some(id.clone());
                    host.session(&id);
                    if let Some(model) = level_of(self.spec, &session) {
                        self.set_model(&id, model, host);
                    }
                    let message = format!("{}\n\n{}", self.spec.instructions, self.spec.message);
                    let _ = self.prompt(&message, None);
                }
                None => self.fail("OpenCode didn't start a session.".into()),
            },
            Err(why) => self.fail(format!("OpenCode: {why}")),
        }
    }

    /// The session's model from here on; one OpenCode doesn't take leaves it on the session's.
    fn set_model(&mut self, session: &str, model: Value, host: &mut dyn Host) {
        let set = json!({ "model": model });
        if let Err(why) = self.call(reqwest::Method::POST, &format!("/api/session/{session}/model"), Some(set)) {
            host.step(Step::marker(
                "note",
                format!("OpenCode didn't take the model ({why}): it goes on with the session's"),
            ));
        }
    }

    /// Sends a prompt: its inbox id (empty when OpenCode gave none), once it's in.
    fn prompt(&mut self, text: &str, delivery: Option<&str>) -> Result<String, Failed> {
        let Some(session) = self.session.clone() else { return Err(Failed::Unreached("no session".into())) };
        let mut body = json!({ "text": text });
        if let Some(delivery) = delivery {
            body["delivery"] = json!(delivery);
        }
        match self.request(reqwest::Method::POST, &format!("/api/session/{session}/prompt"), Some(body)) {
            Ok(data) => {
                self.prompted = true;
                Ok(data["id"].as_str().unwrap_or_default().to_string())
            }
            Err(Failed::Refused(why) | Failed::Unreached(why)) if !self.prompted => {
                self.fail(format!("OpenCode: {why}"));
                Err(Failed::Unreached(why))
            }
            Err(failed) => Err(failed),
        }
    }

    /// Does what a chat turn's notes call for (`Notes`).
    fn apply(&mut self, todo: Vec<Do>) {
        let session = self.session.clone().unwrap_or_default();
        for todo in todo {
            match todo {
                // Into the inbox, delivered at its next step; refused, offered again (and the execution interrupted
                // for it if it keeps being refused); one that didn't reach it is just offered again.
                Do::Steer(note) => match self.prompt(&note, Some("steer")) {
                    Ok(id) => self.inbox.push((Some(id).filter(|id| !id.is_empty()), vec![note])),
                    Err(failed) => {
                        let todo = self
                            .notes
                            .as_mut()
                            .map(|notes| notes.refused(&note, matches!(failed, Failed::Refused(_))))
                            .unwrap_or_default();
                        self.apply(todo);
                    }
                },
                // Its end comes next; with nothing running to interrupt, the execution is over already.
                Do::Interrupt => {
                    let interrupted =
                        self.call(reqwest::Method::POST, &format!("/api/session/{session}/interrupt"), Some(json!({})));
                    if interrupted.is_ok_and(|data| data["interrupted"] != true) {
                        let todo = self.notes.as_mut().map(Notes::turn_ended).unwrap_or_default();
                        self.apply(todo);
                    }
                }
                // A prompt of its own, which the next execution takes; one that doesn't go in leaves them to their
                // own turns.
                Do::StartTurn { message, notes } => match self.prompt(&message, None) {
                    Ok(id) => self.inbox.push((Some(id).filter(|id| !id.is_empty()), notes)),
                    Err(_) => {
                        let todo = self.notes.as_mut().map(|notes| vec![notes.give_up()]).unwrap_or_default();
                        self.apply(todo);
                    }
                },
                // What's still in the inbox is taken back, so it doesn't come up in the session later.
                Do::Finish { untaken } => {
                    for (id, _) in std::mem::take(&mut self.inbox)
                        .into_iter()
                        .filter(|(_, notes)| notes.iter().any(|note| untaken.contains(note)))
                    {
                        if let Some(id) = id {
                            let _ =
                                self.call(reqwest::Method::DELETE, &format!("/api/session/{session}/inbox/{id}"), None);
                        }
                    }
                    self.finished = true;
                }
            }
        }
    }

    fn event(&mut self, event: &Value, host: &mut dyn Host) {
        let data = &event["data"];
        let kind = event["type"].as_str().unwrap_or_default();
        if kind == "server.connected" && self.session.is_none() && !self.finished {
            return self.start(host);
        }
        let session = data["sessionID"].as_str().or(data["form"]["sessionID"].as_str());
        if session.is_none() || session != self.session.as_deref() {
            return;
        }
        self.heard = true;
        let text = |value: &Value| value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
        match kind {
            "session.step.started" => {
                self.acted(host);
                let model = &data["model"];
                if let (Some(provider), Some(id)) = (text(&model["providerID"]), text(&model["id"])) {
                    host.model(&format!("{provider}/{id}"));
                }
            }
            "session.text.ended" => {
                self.acted(host);
                if let Some(said) = text(&data["text"]) {
                    self.final_text = Some(said.clone());
                    host.step(Step::said(&said));
                }
            }
            "session.tool.input.started" => {
                self.acted(host);
                if let (Some(id), Some(name)) = (text(&data["id"]), text(&data["name"])) {
                    self.calls.insert(id, (name, Value::Null));
                }
            }
            // A read or a search shows as it's called; a command once it runs (after its approval, if it needs one);
            // a change to files once it's made.
            "session.tool.called" => {
                let Some(id) = text(&data["id"]) else { return };
                let call = self.calls.entry(id).or_insert_with(|| (String::new(), Value::Null));
                call.1 = data["input"].clone();
                let (name, input) = call.clone();
                if !FILE_TOOLS.contains(&name.as_str())
                    && !SHELL_TOOLS.contains(&name.as_str())
                    && let Some(step) = tool_step(&name, &input)
                {
                    host.step(step);
                }
            }
            "session.tool.progress" => {
                let Some(id) = text(&data["id"]) else { return };
                if let Some((name, input)) =
                    self.calls.get(&id).filter(|(name, _)| SHELL_TOOLS.contains(&name.as_str())).cloned()
                {
                    self.calls.insert(id, (String::new(), Value::Null));
                    if let Some(step) = tool_step(&name, &input) {
                        host.step(step);
                    }
                }
            }
            "session.tool.success" => {
                let Some((name, input)) = text(&data["id"]).and_then(|id| self.calls.remove(&id)) else { return };
                if (FILE_TOOLS.contains(&name.as_str()) || SHELL_TOOLS.contains(&name.as_str()))
                    && let Some(step) = tool_step(&name, &input)
                {
                    host.step(step);
                }
            }
            "session.tool.failed" => {
                if let Some(id) = text(&data["id"]) {
                    self.calls.remove(&id);
                }
            }
            "permission.asked" => {
                let Some(id) = text(&data["id"]) else { return };
                // The call it's for says what it is in full (a command's resources are its parts).
                let call = text(&data["source"]["id"]).and_then(|call| self.calls.get(&call)).cloned();
                let key = host.ask(&ask_for(data, call.as_ref()));
                self.waiting.push(Waiting::Permission { id, key });
            }
            "form.created" => {
                let form = &data["form"];
                let Some(id) = text(&form["id"]) else { return };
                let mut fields = Vec::new();
                for field in form["fields"].as_array().into_iter().flatten() {
                    let options: Vec<(String, String)> = field["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|option| {
                            Some((
                                option["value"].as_str()?.to_string(),
                                option["label"].as_str().or(option["value"].as_str())?.to_string(),
                            ))
                        })
                        .collect();
                    let notes = field["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|option| option["value"].is_string())
                        .map(|option| option["description"].as_str().unwrap_or_default().to_string())
                        .collect();
                    let title = text(&field["description"])
                        .or_else(|| text(&field["title"]))
                        .unwrap_or_else(|| "A question".into());
                    let key = host.ask(&Ask {
                        kind: "question",
                        title,
                        detail: text(&field["title"]),
                        options: options.iter().map(|(_, label)| label.clone()).collect(),
                        notes,
                    });
                    fields.push((field["key"].as_str().unwrap_or_default().to_string(), options, key));
                }
                let answers = vec![None; fields.len()];
                self.waiting.push(Waiting::Form { id, fields, answers });
            }
            "session.execution.started" => {
                if let Some(notes) = self.notes.as_mut() {
                    notes.turn_started(host);
                }
            }
            // A prompt reached the agent: the notes it carries are had (one OpenCode gave no id for is the first
            // delivered that isn't known here).
            "session.inbox.delivered" => {
                let id = data["inboxID"].as_str();
                let at = self
                    .inbox
                    .iter()
                    .position(|(prompt, _)| prompt.is_some() && prompt.as_deref() == id)
                    .or_else(|| self.inbox.iter().position(|(prompt, _)| prompt.is_none()));
                if let (Some(at), Some(notes)) = (at, self.notes.as_mut()) {
                    for note in self.inbox.remove(at).1 {
                        notes.accepted(&note);
                    }
                }
            }
            "session.execution.succeeded" | "session.execution.interrupted" if self.prompted => {
                match self.notes.as_mut() {
                    // A chat turn's goes on for the notes it didn't take (`Notes`).
                    Some(notes) => {
                        let todo = notes.turn_ended();
                        self.apply(todo);
                    }
                    None => self.finished = true,
                }
            }
            "session.execution.failed" if self.prompted => {
                let why = text(&data["error"]["message"]).unwrap_or_else(|| "the turn failed".into());
                self.fail(format!("OpenCode: {why}"));
            }
            _ => {}
        }
    }

    fn deliver_answers(&mut self, host: &mut dyn Host) {
        let Some(session) = self.session.clone() else { return };
        let mut index = 0;
        while index < self.waiting.len() {
            let ready = match &mut self.waiting[index] {
                Waiting::Permission { key, .. } => host.answer(key).map(|answer| vec![answer]),
                Waiting::Form { fields, answers, .. } => {
                    for ((_, _, key), answer) in fields.iter().zip(answers.iter_mut()) {
                        if answer.is_none() {
                            *answer = host.answer(key);
                        }
                    }
                    answers.iter().all(Option::is_some).then(|| answers.iter().flatten().cloned().collect())
                }
            };
            let Some(answers) = ready else {
                index += 1;
                continue;
            };
            let result = match self.waiting.remove(index) {
                Waiting::Permission { id, .. } => {
                    let decision = match crate::coding_handoff::approval_answer(
                        answers.first().map(String::as_str).unwrap_or("deny"),
                    )
                    .unwrap_or("deny")
                    {
                        "allow" => "once",
                        "allow_always" => "always",
                        _ => "reject",
                    };
                    let mut body = json!({ "decision": decision });
                    if decision == "reject" {
                        body["message"] = json!(DENIED);
                    }
                    self.call(
                        reqwest::Method::POST,
                        &format!("/api/session/{session}/permission/{id}/reply"),
                        Some(body),
                    )
                }
                Waiting::Form { id, fields, .. } => {
                    // An option picked goes as its value; anything else in the answerer's words.
                    let answer: serde_json::Map<String, Value> = fields
                        .iter()
                        .zip(answers)
                        .map(|((key, options, _), answer)| {
                            let picked = options.iter().find(|(value, label)| {
                                label.eq_ignore_ascii_case(answer.trim()) || value.eq_ignore_ascii_case(answer.trim())
                            });
                            (key.clone(), json!(picked.map_or(answer.clone(), |(value, _)| value.clone())))
                        })
                        .collect();
                    self.call(
                        reqwest::Method::POST,
                        &format!("/api/session/{session}/form/{id}/reply"),
                        Some(json!({ "answer": answer })),
                    )
                }
            };
            if let Err(why) = result {
                host.step(Step::marker("note", format!("OpenCode didn't take the answer: {why}")));
            }
        }
    }
}

impl Protocol for Talk<'_> {
    fn begin(&mut self, _host: &mut dyn Host) {}

    /// Its stdout says only where it listens.
    fn line(&mut self, line: &str, _host: &mut dyn Host) {
        if self.url.is_some() || self.finished {
            return;
        }
        let Some(url) = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value["url"].as_str().map(|url| url.trim_end_matches('/').to_string()))
        else {
            return;
        };
        self.connect(url);
    }

    fn look(&mut self, host: &mut dyn Host) {
        let events: Vec<Value> = self.events.as_ref().map(|events| events.try_iter().collect()).unwrap_or_default();
        for event in events {
            if self.finished {
                break;
            }
            self.event(&event, host);
        }
        if self.finished {
            return;
        }
        self.deliver_answers(host);
        if let Some(notes) = self.notes.as_mut() {
            let todo = notes.look(host);
            return self.apply(todo);
        }
        // A coding round's notes, delivered at its next step.
        if self.prompted {
            for note in host.notes() {
                let _ = self.prompt(&note, Some("steer"));
            }
        }
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn heard(&mut self) -> bool {
        std::mem::take(&mut self.heard)
    }

    fn holding(&self) -> bool {
        !self.waiting.is_empty()
    }

    fn close(&mut self) {
        self.events = None;
        self.stdin = None;
    }

    fn end(&mut self) -> End {
        End {
            session_id: self.session.clone(),
            final_text: self.final_text.clone(),
            error: self.error.clone(),
            ..End::default()
        }
    }
}

/// The models OpenCode offers for the Coding page's pickers, all of them (it knows hundreds; the picker searches them):
/// its default, then the user's favourites and recent ones (OpenCode's own `model.json`), then the free ones, then the
/// rest by name, with their reasoning levels (OpenCode's variants).
/// Starts a server of its own for it (`GET /api/model`), given 20 seconds; empty when it can't say.
pub fn models(program: &Path, path_env: Option<&str>, folder: &Path) -> Vec<coding::ToolModel> {
    let password = uuid::Uuid::new_v4().simple().to_string();
    let mut command = crate::disclaim::command(program, false);
    command
        .args(args())
        .envs(env(&password, None))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    let Ok(mut child) = command.spawn() else { return Vec::new() };
    let watchdog = crate::engine::Stopper::for_group(child.id() as i32);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (watchdog, done) = (watchdog.clone(), done.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            if !done.load(std::sync::atomic::Ordering::SeqCst) {
                watchdog.stop();
            }
        });
    }
    let url = child.stdout.take().and_then(|stdout| {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| serde_json::from_str::<Value>(&line).ok()?["url"].as_str().map(str::to_string))
    });
    let (mut all, mut default) = (Value::Null, Value::Null);
    if let (Some(url), Ok(http)) = (url, reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build())
    {
        let get = |path: &str| -> Value {
            let request = http
                .get(format!("{}{path}", url.trim_end_matches('/')))
                .basic_auth(USER, Some(&password))
                .query(&[("location[directory]", folder.display().to_string())]);
            request
                .send()
                .ok()
                .and_then(|response| response.json::<Value>().ok())
                .map(|value| value["data"].clone())
                .unwrap_or(Value::Null)
        };
        // Asked about a folder for the first time, it may not know yet: an empty list, no default.
        for _ in 0..5 {
            all = get("/api/model");
            if all.as_array().is_some_and(|models| !models.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        for _ in 0..3 {
            default = get("/api/model/default");
            if !default.is_null() {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    let state =
        std::env::var_os("XDG_STATE_HOME").filter(|dir| !dir.is_empty()).map(std::path::PathBuf::from).unwrap_or_else(
            || std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(".local/state"),
        );
    let used: Value = std::fs::read(state.join("opencode/model.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    models_in(&all, &default, &used)
}

/// The pickers' rows from what OpenCode said: every model it has (`all`), its default and the models the user picked
/// before (`used`: `favorite` and `recent`, each `{providerID, modelID}`) first, then the free ones, then the rest by
/// name.
fn models_in(all: &Value, default: &Value, used: &Value) -> Vec<coding::ToolModel> {
    let key = |provider: &str, id: &str| format!("{provider}/{id}");
    let id_of =
        |model: &Value| key(model["providerID"].as_str().unwrap_or_default(), model["id"].as_str().unwrap_or_default());
    let name_of = |model: &Value| {
        model["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_lowercase)
            .unwrap_or_else(|| id_of(model))
    };
    let free = |model: &Value| {
        model["cost"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|cost| cost["input"].as_f64() == Some(0.) && cost["output"].as_f64() == Some(0.))
    };
    let mut known: Vec<&Value> =
        all.as_array().into_iter().flatten().filter(|model| model["enabled"] != false).collect();
    known.sort_by_cached_key(|model| (!free(model), name_of(model), id_of(model)));
    let by_id: std::collections::HashMap<String, &Value> = known.iter().map(|model| (id_of(model), *model)).collect();
    let default_id = default["providerID"].as_str().zip(default["id"].as_str()).map(|(provider, id)| key(provider, id));
    let mut wanted: Vec<String> = default_id.iter().cloned().collect();
    for list in ["favorite", "recent"] {
        wanted.extend(
            used[list]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|model| Some(key(model["providerID"].as_str()?, model["modelID"].as_str()?))),
        );
    }
    wanted.extend(known.iter().map(|model| id_of(model)));
    let mut rows: Vec<coding::ToolModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in wanted {
        if !seen.insert(id.clone()) || !crate::engine::valid_model(&id) {
            continue;
        }
        // One it doesn't have anymore (a provider signed out) isn't offered.
        let Some(model) = by_id.get(&id).copied() else { continue };
        let provider = model["providerID"].as_str().unwrap_or_default();
        let name = model["name"].as_str().map(str::trim).filter(|name| !name.is_empty()).unwrap_or(&id);
        let efforts = model["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|variant| variant["id"].as_str())
            .filter(|variant| *variant != "default" && crate::engine::valid_model(variant))
            .map(str::to_string)
            .collect();
        rows.push(coding::ToolModel {
            name: format!("{name} ({provider})"),
            efforts,
            default_effort: None,
            is_default: Some(&id) == default_id.as_ref(),
            id,
        });
    }
    rows
}

/// `value` for a URL's query or path.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~/".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// The tools that change files.
const FILE_TOOLS: [&str; 4] = ["edit", "write", "patch", "apply_patch"];
/// The tools that run commands.
const SHELL_TOOLS: [&str; 2] = ["shell", "bash"];

/// A tool call as a step: a command, the files it changes, what it reads or looks for. Its questions come as asks.
fn tool_step(name: &str, input: &Value) -> Option<Step> {
    let field = |key: &str| input[key].as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let path = || field("path").or_else(|| field("filePath")).or_else(|| field("file_path"));
    Some(match name {
        "shell" | "bash" => Step::did("command", coding::unwrap_shell(&field("command")?)),
        "edit" | "write" | "patch" | "apply_patch" => Step::did("files", format!("Changed {}", path()?)),
        "read" => Step::did("tool", format!("Read {}", path()?)),
        "glob" | "grep" => Step::did("search", field("pattern")?),
        "websearch" => Step::did("search", field("query")?),
        "webfetch" => Step::did("tool", format!("Read {}", field("url")?)),
        "question" | "" => return None,
        other => Step::did("tool", other.to_string()),
    })
}

/// What a permission request asks, in a line the conversation can judge: from the tool call it's for, when known.
fn ask_for(data: &Value, call: Option<&(String, Value)>) -> Ask {
    let resources: Vec<&str> = data["resources"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let what = resources.join(", ");
    let command = call
        .and_then(|(_, input)| input["command"].as_str())
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string);
    let title = match data["action"].as_str().unwrap_or_default() {
        "shell" => format!("Run `{}`", coding::unwrap_shell(&command.unwrap_or_else(|| resources.join(" && ")))),
        "edit" => format!("Change {what}"),
        "external_directory" => format!("Reach {what} outside its folder"),
        "webfetch" => format!("Fetch {what}"),
        "websearch" => format!("Search the web for \"{what}\""),
        other if what.is_empty() => format!("Use {other}"),
        other => format!("Use {other}: {what}"),
    };
    Ask { kind: "approval", title, detail: None, options: Vec::new(), notes: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec(mode: &str, look_only: bool) -> Spec {
        Spec {
            engine: crate::engine::Engine::OpenCode,
            program: PathBuf::from("/bin/opencode"),
            cwd: PathBuf::from("/w"),
            path_env: None,
            message: "Goal: x".into(),
            resume: None,
            new_session: None,
            mode: mode.into(),
            look_only,
            model: None,
            effort: None,
            instructions: "Work well.".into(),
            writable: vec![PathBuf::from("/p/proof")],
            timeout: Duration::from_secs(20),
            servers: Vec::new(),
            skills: None,
            chat: false,
            command: None,
            images: Vec::new(),
        }
    }

    /// The effect of the last rule matching `action` on `resource` (rules are globs; `*` matches anything here).
    fn effect(rules: &Value, action: &str, resource: &str) -> String {
        let matches = |pattern: &str, value: &str| {
            pattern == "*"
                || pattern == value
                || pattern
                    .strip_suffix("/**")
                    .or(pattern.strip_suffix("/*"))
                    .is_some_and(|dir| value.starts_with(&format!("{dir}/")))
        };
        rules
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|rule| {
                matches(rule["action"].as_str().unwrap(), action)
                    && matches(rule["resource"].as_str().unwrap(), resource)
            })
            .map_or("ask".into(), |rule| rule["effect"].as_str().unwrap().to_string())
    }

    #[test]
    fn permissions_follow_the_task() {
        let auto = permissions(&spec("auto", false));
        assert_eq!(
            (effect(&auto, "shell", "ls"), effect(&auto, "edit", "a.rs"), effect(&auto, "read", "a.rs")),
            ("allow".into(), "allow".into(), "allow".into())
        );
        assert_eq!(
            (
                effect(&auto, "external_directory", "/etc/hosts"),
                effect(&auto, "external_directory", "/p/proof/tests.log")
            ),
            ("ask".into(), "allow".into())
        );
        assert_eq!(effect(&auto, "execute", "x"), "deny");
        let supervised = permissions(&spec("supervised", false));
        assert_eq!(
            (effect(&supervised, "shell", "ls"), effect(&supervised, "edit", "a.rs"), effect(&supervised, "grep", "x")),
            ("ask".into(), "ask".into(), "allow".into())
        );
        assert_eq!(effect(&permissions(&spec("auto", true)), "edit", "a.rs"), "deny");
        let full = permissions(&spec("full_access", true));
        assert_eq!(
            (effect(&full, "edit", "a.rs"), effect(&full, "external_directory", "/etc")),
            ("allow".into(), "allow".into())
        );
    }

    /// A chat's session: its code mode on (its MCP tools are reached through it), only Filo's skills, no questions, and
    /// its servers in OpenCode's config.
    #[test]
    fn a_chat_gets_its_servers_and_filos_skills_only() {
        let mut chat = spec("full_access", false);
        chat.chat = true;
        chat.skills = Some(vec!["weekly-report".into()]);
        chat.servers = vec![crate::engine::McpServer {
            name: "filo".into(),
            program: PathBuf::from("/Apps/Filo/filo"),
            args: vec!["--local-mcp".into(), "--chat".into(), "c1".into()],
        }];
        let rules = permissions(&chat);
        assert_eq!(effect(&rules, "execute", "x"), "allow");
        assert_eq!(
            (
                effect(&rules, "skill", "weekly-report"),
                effect(&rules, "skill", "dataviz"),
                effect(&rules, "skill", "wrangler")
            ),
            ("allow".into(), "allow".into(), "deny".into())
        );
        assert_eq!(
            (effect(&rules, "skill", "artifacts"), effect(&rules, "skill", "desktop-pet")),
            ("allow".into(), "allow".into())
        );
        assert_eq!(effect(&rules, "question", "*"), "deny");
        let vars = env("pw", Some(&chat));
        let config: Value =
            serde_json::from_str(&vars.iter().find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT").unwrap().1).unwrap();
        assert_eq!(
            config,
            json!({ "mcp": { "servers": { "filo": { "type": "local", "command": ["/Apps/Filo/filo", "--local-mcp", "--chat", "c1"] } } } })
        );
        // A coding round: no config, and the code mode off.
        let coding = spec("auto", false);
        assert_eq!(env("pw", Some(&coding)), [("OPENCODE_PASSWORD".to_string(), "pw".to_string())]);
        assert_eq!(effect(&permissions(&coding), "execute", "x"), "deny");
        assert_eq!(effect(&permissions(&coding), "skill", "wrangler"), "allow");
    }

    #[test]
    fn models_name_their_provider_and_variant() {
        assert_eq!(
            model_ref("opencode/mimo-v2.6-flash-free", None),
            Some(json!({ "providerID": "opencode", "id": "mimo-v2.6-flash-free" }))
        );
        assert_eq!(
            model_ref("openrouter/z-ai/glm-5.3#high", Some("low")),
            Some(json!({ "providerID": "openrouter", "id": "z-ai/glm-5.3", "variant": "high" }))
        );
        assert_eq!(model_ref("cline-pass/glm", Some("max")).unwrap()["variant"], "max");
        assert_eq!(model_ref("no-provider", None), None);
        // A level without a model of the task's own: the session's model at that variant, unless it's on it already.
        let session = json!({ "model": { "providerID": "opencode", "id": "space-bunny-free", "variant": "xhigh" } });
        let mut leveled = spec("full_access", false);
        assert_eq!(level_of(&leveled, &session), None);
        leveled.effort = Some("low".into());
        assert_eq!(
            level_of(&leveled, &session),
            Some(json!({ "providerID": "opencode", "id": "space-bunny-free", "variant": "low" }))
        );
        leveled.effort = Some("xhigh".into());
        assert_eq!(level_of(&leveled, &session), None);
        assert_eq!(level_of(&leveled, &json!({ "model": null })), None);
        leveled.model = Some("opencode/space-bunny-free".into());
        leveled.effort = Some("low".into());
        assert_eq!(level_of(&leveled, &session), None);

        let model = |provider: &str, id: &str, name: &str, cost: f64| json!({ "providerID": provider, "id": id, "name": name, "enabled": true, "variants": [{ "id": "default" }, { "id": "high" }], "cost": [{ "input": cost, "output": cost }] });
        let all = json!([
            model("openrouter", "z-ai/glm-5.3", "GLM-5.3", 2.),
            model("openrouter", "meta/muse-wonder", "Muse Wonder", 4.),
            model("opencode", "mimo-free", "MiMo Free", 0.),
            model("cline-pass", "glm", "GLM", 1.),
            model("anthropic", "claude-opus", "Claude Opus", 5.),
        ]);
        let used = json!({ "recent": [{ "providerID": "cline-pass", "modelID": "glm" }, { "providerID": "gone", "modelID": "x" }] });
        let rows = models_in(&all, &json!({ "providerID": "openrouter", "id": "meta/muse-wonder" }), &used);
        let ids: Vec<(&str, bool)> = rows.iter().map(|row| (row.id.as_str(), row.is_default)).collect();
        // The default first, then what the user picked before, the free ones, and every other one by name.
        assert_eq!(
            ids,
            [
                ("openrouter/meta/muse-wonder", true),
                ("cline-pass/glm", false),
                ("opencode/mimo-free", false),
                ("anthropic/claude-opus", false),
                ("openrouter/z-ai/glm-5.3", false)
            ]
        );
        assert_eq!(
            (rows[0].name.as_str(), rows[0].efforts.clone()),
            ("Muse Wonder (openrouter)", vec!["high".to_string()])
        );
    }

    #[test]
    fn tool_calls_and_permissions_read_as_lines() {
        assert_eq!(
            tool_step("shell", &json!({ "command": "echo hi > hi.txt" })),
            Some(Step::did("command", "echo hi > hi.txt"))
        );
        assert_eq!(
            tool_step("write", &json!({ "path": "b.txt", "content": "bye" })),
            Some(Step::did("files", "Changed b.txt"))
        );
        assert_eq!(
            tool_step("webfetch", &json!({ "url": "https://x.y" })),
            Some(Step::did("tool", "Read https://x.y"))
        );
        assert_eq!(tool_step("question", &json!({})), None);
        let asked = json!({ "action": "shell", "resources": ["cat a", "printf x"], "source": { "id": "call_1" } });
        assert_eq!(ask_for(&asked, None).title, "Run `cat a && printf x`");
        assert_eq!(
            ask_for(&asked, Some(&("shell".to_string(), json!({ "command": "cat a; printf x" })))).title,
            "Run `cat a; printf x`"
        );
        assert_eq!(
            ask_for(&json!({ "action": "external_directory", "resources": ["/etc/*"] }), None).title,
            "Reach /etc/* outside its folder"
        );
    }

    /// Asks the installed OpenCode which models it offers (no model is called): `cargo test -p filo-local
    /// real_opencode_models -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_opencode_models() {
        let program =
            crate::engine::find_program(crate::engine::Engine::OpenCode, None).expect("OpenCode is installed");
        let models = models(&program, None, &std::env::temp_dir());
        println!("{models:#?}");
        assert!(!models.is_empty());
    }
}

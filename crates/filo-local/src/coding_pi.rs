// A round with pi (coding_session.rs): `pi --mode rpc`, JSON lines both ways (pi's docs/rpc.md). A command is
// `{"type": …, "id": …}` and its answer a `response` with that id; what the agent does comes as events: `agent_start`,
// each message (`message_start` / `message_end`: the user's as pi takes it in, the assistant's whole at its end),
// each tool (`tool_execution_start` with its arguments, `tool_execution_end`), and `agent_settled` once nothing more
// runs (no retry, no queued message). The message is a `prompt`; a note is a `steer`, queued (`queue_update`) until pi
// delivers it after the tool calls under way, as a user message.
//
// Filo's extension (pi_extension.ts, loaded with `-e`) brings what pi doesn't have: the chat's MCP servers (Filo's and
// the user's apps), through pi's own MCP when it has one (`registerMcpServer`), else talked to by the extension, and
// the task's mode. Full access lets everything run; Auto asks before a change outside the task's folders and, in a
// coding round, before a command that isn't plainly a look, as an `extension_ui_request` select with Allow, Allow
// always and Deny: Filo's permission card (`Host::ask`). Any other dialog (an extension of the user's) is dismissed at
// once, so nothing, project trust included, holds a turn up; pi itself asks nothing in RPC mode.
//
// A chat turn's notes are the messages the user sends while it works (steer.rs), followed as every engine's are
// (coding_session.rs `Notes`): handed over as steers, the turn's once pi delivered one (its user message) and did
// something after. One still queued when the run settles is taken back (`clear_queue`) and starts the next prompt in the
// same session; one pi refused for a while has the run aborted for it first. The chat turn is over once a run settles
// with every note the turn's.
//
// The instructions go in as `--append-system-prompt`, every run (pi builds its system prompt each time it starts); the
// session carries on with `--session <id>`, a new one's id is asked for (`get_state`). A chat runs with Filo's skills
// only and without the user's own extensions, as its MCP servers are only the ones Filo gives it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;

use serde_json::{Value, json};

use crate::coding::{self, Step};
use crate::coding_session::{Ask, Do, End, Host, Notes, Protocol, Spec};
use crate::engine::Pipe;

/// Filo's extension (pi_extension.ts).
const EXTENSION: &str = include_str!("pi_extension.ts");

/// The answers Filo's permission card gives, as the extension offers them.
const ALLOW: &str = "Allow";
const ALWAYS: &str = "Allow always";
const DENY: &str = "Deny";

/// Where Filo's extension is for pi to load: a file in the temporary folder named by its contents, written once.
pub fn extension_file() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("filo-pi");
    let file = dir.join(format!("filo-{}.ts", crate::engine::fingerprint(EXTENSION)));
    if !file.is_file() {
        std::fs::create_dir_all(&dir).map_err(|error| format!("Couldn't set pi up: {error}"))?;
        let part = dir.join(format!(".filo-{}.ts", uuid::Uuid::new_v4().simple()));
        std::fs::write(&part, EXTENSION)
            .and_then(|_| std::fs::rename(&part, &file))
            .map_err(|error| format!("Couldn't set pi up: {error}"))?;
    }
    Ok(file)
}

/// `pi --mode rpc` with Filo's extension, the instructions, the model and thinking level, the session to carry on;
/// for a chat, Filo's skills only (from the workspace, where store.rs writes them) and none of the user's extensions.
pub fn args(spec: &Spec, extension: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["--mode", "rpc", "-e"].map(str::to_string).to_vec();
    args.push(extension.display().to_string());
    if let Some(skills) = &spec.skills {
        args.extend(["--no-extensions".into(), "--no-skills".into()]);
        let folder = spec.cwd.join(".agents/skills");
        let names = skills
            .iter()
            .map(String::as_str)
            .chain(["dataviz"])
            .chain(crate::skills::FILO_SKILLS.iter().map(|(name, _)| *name));
        let mut added = Vec::new();
        for name in names {
            let dir = folder.join(name);
            if !added.contains(&name) && dir.join("SKILL.md").is_file() {
                added.push(name);
                args.extend(["--skill".into(), dir.display().to_string()]);
            }
        }
    }
    args.extend(["--append-system-prompt".into(), spec.instructions.clone()]);
    if let Some(model) = &spec.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &spec.effort {
        args.extend(["--thinking".into(), effort.clone()]);
    }
    if let Some(session) = &spec.resume {
        args.extend(["--session".into(), session.clone()]);
    }
    args
}

/// What the extension is told (`FILO_PI`), and no update checks from inside a round.
pub fn env(spec: &Spec) -> Vec<(String, String)> {
    let roots: Vec<String> =
        std::iter::once(&spec.cwd).chain(&spec.writable).map(|dir| dir.display().to_string()).collect();
    let servers: Vec<Value> = spec
        .servers
        .iter()
        .map(|server| json!({ "name": server.name, "command": server.program.display().to_string(), "args": server.args }))
        .collect();
    let config = json!({
        "mode": spec.mode,
        "lookOnly": spec.look_only,
        // Nobody answers mid-turn in a chat, so its shell isn't asked about; only a change outside its folders is,
        // and that's refused there.
        "askShell": !spec.chat,
        "roots": roots,
        "servers": servers,
    });
    vec![("FILO_PI".into(), config.to_string()), ("PI_OFFLINE".into(), "1".into())]
}

/// What a command this side sent was for.
#[derive(Clone, PartialEq)]
enum Sent {
    State,
    Prompt,
    Steer(String),
    Other,
}

pub struct Talk<'a> {
    spec: &'a Spec,
    pipe: Pipe<()>,
    next_id: u64,
    sent: HashMap<String, Sent>,
    session: Option<String>,
    /// Filo's permission card's asks, waiting on answers: the dialog's id and the ask's key.
    waiting: Vec<(String, String)>,
    /// A prompt is under way: from its acceptance to `agent_settled`.
    running: bool,
    /// The steers pi holds, not delivered yet (`queue_update`).
    queued: Vec<String>,
    final_text: Option<String>,
    /// Why the run's last call to the model failed, until a retry goes through.
    failed: Option<String>,
    error: Option<String>,
    finished: bool,
    /// A chat turn's notes: the messages the user sends while it works.
    notes: Option<Notes>,
}

impl<'a> Talk<'a> {
    pub fn new(spec: &'a Spec, stdin: Option<ChildStdin>) -> Self {
        Talk {
            spec,
            pipe: Pipe::new(stdin),
            next_id: 1,
            sent: HashMap::new(),
            session: None,
            waiting: Vec::new(),
            running: false,
            queued: Vec::new(),
            final_text: None,
            failed: None,
            error: None,
            finished: false,
            notes: spec.chat.then(|| Notes::new(false)),
        }
    }

    fn send(&mut self, mut command: Value, purpose: Sent) {
        let id = format!("filo-{}", self.next_id);
        self.next_id += 1;
        command["id"] = json!(id);
        self.sent.insert(id, purpose);
        self.pipe.write(&command);
    }

    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
        if let Some(notes) = self.notes.as_mut() {
            notes.fail();
        }
    }

    /// A prompt: the round's message (with its pictures), or a note the last run didn't take.
    fn prompt(&mut self, text: &str, with_images: bool) {
        let mut command = json!({ "type": "prompt", "message": text });
        if with_images {
            let images: Vec<Value> = self.spec.images.iter().filter_map(|path| image(path)).collect();
            if !images.is_empty() {
                command["images"] = json!(images);
            }
        }
        self.send(command, Sent::Prompt);
    }

    fn steer(&mut self, note: String) {
        self.send(json!({ "type": "steer", "message": note }), Sent::Steer(note));
    }

    /// Does what a chat turn's notes call for (`Notes`).
    fn apply(&mut self, todo: Vec<Do>) {
        for todo in todo {
            match todo {
                Do::Steer(note) => self.steer(note),
                Do::Interrupt => self.send(json!({ "type": "abort" }), Sent::Other),
                Do::StartTurn { message, .. } => self.prompt(&message, false),
                Do::Finish { .. } => self.finished = true,
            }
        }
    }

    /// Nothing more runs: the round is over, or a chat turn goes on for the notes the run didn't take.
    fn settled(&mut self) {
        self.running = false;
        if let Some(why) = self.failed.take() {
            return self.fail(format!("pi: {why}"));
        }
        let Some(notes) = self.notes.as_mut() else {
            self.finished = true;
            return;
        };
        // Steers pi still holds: taken back, for the next prompt.
        let held = notes.sent();
        for note in &held {
            notes.refused(note, false);
        }
        let todo = notes.turn_ended();
        if !held.is_empty() || !self.queued.is_empty() {
            self.queued.clear();
            self.send(json!({ "type": "clear_queue" }), Sent::Other);
        }
        self.apply(todo);
    }

    fn response(&mut self, message: &Value, host: &mut dyn Host) {
        let Some(purpose) = message["id"].as_str().and_then(|id| self.sent.remove(id)) else { return };
        let ok = message["success"] == true;
        let why = message["error"].as_str().unwrap_or("it refused").to_string();
        match (purpose, ok) {
            (Sent::State, true) => {
                let state = &message["data"];
                if let Some(id) = state["sessionId"].as_str().filter(|id| !id.is_empty()) {
                    self.session = Some(id.to_string());
                    host.session(id);
                }
                if let (Some(provider), Some(id)) = (state["model"]["provider"].as_str(), state["model"]["id"].as_str())
                {
                    host.model(&format!("{provider}/{id}"));
                }
            }
            (Sent::State, false) => self.fail(format!("pi: {why}")),
            (Sent::Prompt, true) => {
                self.running = true;
                if let Some(notes) = self.notes.as_mut() {
                    notes.turn_started(host);
                }
            }
            (Sent::Prompt, false) => self.fail(format!("pi: {why}")),
            (Sent::Steer(note), false) if self.notes.is_some() => {
                let todo = self.notes.as_mut().map(|notes| notes.refused(&note, true)).unwrap_or_default();
                self.apply(todo);
            }
            (Sent::Steer(_), false) | (Sent::Other, false) => host.step(Step::marker("note", format!("pi: {why}"))),
            (Sent::Steer(_), true) | (Sent::Other, true) => {}
        }
    }

    fn event(&mut self, kind: &str, event: &Value, host: &mut dyn Host) {
        match kind {
            "message_start" => {
                let message = &event["message"];
                match message["role"].as_str() {
                    // A note pi took in.
                    Some("user") => {
                        let said = text_of(&message["content"]);
                        if let Some(notes) = self.notes.as_mut() {
                            notes.accepted(said.trim());
                        }
                        self.queued.retain(|queued| queued.trim() != said.trim());
                    }
                    Some("assistant") => {
                        if let Some(notes) = self.notes.as_mut() {
                            notes.acted(host);
                        }
                    }
                    _ => {}
                }
            }
            "message_end" => {
                let message = &event["message"];
                if message["role"] != "assistant" {
                    return;
                }
                match message["stopReason"].as_str() {
                    Some("error") => {
                        self.failed = Some(
                            message["errorMessage"].as_str().unwrap_or("the model call failed").trim().to_string(),
                        );
                    }
                    // Stopped: by Filo for notes (the next prompt carries on), or Stop.
                    Some("aborted") => {}
                    _ => {
                        self.failed = None;
                        let said = text_of(&message["content"]);
                        if !said.trim().is_empty() {
                            self.final_text = Some(said.trim().to_string());
                            host.step(Step::said(said.trim()));
                        }
                    }
                }
            }
            "tool_execution_start" => {
                if let Some(notes) = self.notes.as_mut() {
                    notes.acted(host);
                }
                let tool = event["toolName"].as_str().unwrap_or_default();
                if !FILE_TOOLS.contains(&tool)
                    && let Some(step) = tool_step(tool, &event["args"])
                {
                    host.step(step);
                }
            }
            // A change to files shows once it's made.
            "tool_execution_end" => {
                let tool = event["toolName"].as_str().unwrap_or_default();
                if FILE_TOOLS.contains(&tool)
                    && event["isError"] != true
                    && let Some(step) = tool_step(tool, &event["args"])
                {
                    host.step(step);
                }
            }
            "queue_update" => {
                self.queued = event["steering"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
            }
            "auto_retry_start" => {
                if let Some(why) = event["errorMessage"].as_str().filter(|why| !why.trim().is_empty()) {
                    host.step(Step::marker("note", format!("pi retries: {}", why.trim())));
                }
            }
            "auto_retry_end" if event["success"] == false => {
                self.failed = Some(event["finalError"].as_str().unwrap_or("it kept failing").to_string());
            }
            "compaction_start" => host.step(Step::marker("note", "pi is summarizing the session so far")),
            "extension_error" => {
                let why = event["error"].as_str().unwrap_or_default();
                host.step(Step::marker("note", format!("pi's extension failed: {why}")));
            }
            "agent_settled" => self.settled(),
            "extension_ui_request" => self.dialog(event, host),
            _ => {}
        }
    }

    /// A dialog an extension opened: Filo's permission card goes to the host; any other is dismissed at once.
    fn dialog(&mut self, request: &Value, host: &mut dyn Host) {
        let Some(id) = request["id"].as_str().map(str::to_string) else { return };
        let method = request["method"].as_str().unwrap_or_default();
        if !matches!(method, "select" | "confirm" | "input" | "editor") {
            return;
        }
        let ours = method == "select" && request["options"] == json!([ALLOW, ALWAYS, DENY]);
        if !ours {
            self.pipe.write(&json!({ "type": "extension_ui_response", "id": id, "cancelled": true }));
            let title = request["title"].as_str().unwrap_or("a question").trim();
            return host
                .step(Step::marker("note", format!("pi asked \"{title}\": dismissed, as nobody answers it here")));
        }
        let title = request["title"].as_str().unwrap_or("Use a tool");
        let (title, detail) = match title.split_once("\n\n") {
            Some((title, detail)) => (title, Some(detail.trim().to_string()).filter(|detail| !detail.is_empty())),
            None => (title, None),
        };
        let key = host.ask(&Ask {
            kind: "approval",
            title: title.trim().to_string(),
            detail,
            options: Vec::new(),
            notes: Vec::new(),
        });
        self.waiting.push((id, key));
    }

    fn deliver_answers(&mut self, host: &mut dyn Host) {
        let mut index = 0;
        while index < self.waiting.len() {
            let Some(answer) = host.answer(&self.waiting[index].1) else {
                index += 1;
                continue;
            };
            let (id, _) = self.waiting.remove(index);
            let value = match crate::coding_handoff::approval_answer(&answer) {
                Some("allow") => ALLOW,
                Some("allow_always") => ALWAYS,
                _ => DENY,
            };
            self.pipe.write(&json!({ "type": "extension_ui_response", "id": id, "value": value }));
        }
    }
}

impl Protocol for Talk<'_> {
    fn begin(&mut self, _host: &mut dyn Host) {
        self.send(json!({ "type": "get_state" }), Sent::State);
        let message = self.spec.message.clone();
        self.prompt(&message, true);
    }

    fn line(&mut self, line: &str, host: &mut dyn Host) {
        let Ok(message) = serde_json::from_str::<Value>(line) else { return };
        match message["type"].as_str().unwrap_or_default() {
            "response" => self.response(&message, host),
            kind => self.event(kind, &message, host),
        }
    }

    fn look(&mut self, host: &mut dyn Host) {
        self.deliver_answers(host);
        if let Some(notes) = self.notes.as_mut() {
            let todo = notes.look(host);
            return self.apply(todo);
        }
        // A coding round's notes, into its running prompt.
        if self.running {
            for note in host.notes() {
                self.steer(note);
            }
        }
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn holding(&self) -> bool {
        !self.waiting.is_empty()
    }

    fn close(&mut self) {
        self.pipe.close();
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

/// The text of a message's content: a string, or its text blocks.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A picture the message comes with, as pi takes it: base64 with its type.
fn image(path: &Path) -> Option<Value> {
    use base64::Engine as _;
    let mime = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    let bytes = std::fs::read(path).ok()?;
    Some(json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(bytes), "mimeType": mime }))
}

/// The tools that change files.
const FILE_TOOLS: [&str; 2] = ["edit", "write"];

/// A tool call as a step: a command, the file it changes, what it reads or looks for; an MCP tool by its own name.
fn tool_step(tool: &str, args: &Value) -> Option<Step> {
    let field = |key: &str| args[key].as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    Some(match tool {
        "bash" => Step::did("command", coding::unwrap_shell(&field("command")?)),
        "edit" | "write" => Step::did("files", format!("Changed {}", field("path").or_else(|| field("file_path"))?)),
        "read" => Step::did("tool", format!("Read {}", field("path")?)),
        "grep" | "find" => Step::did("search", field("pattern").or_else(|| field("query"))?),
        "ls" => Step::did("tool", format!("Listed {}", field("path").unwrap_or_else(|| ".".into()))),
        "" => return None,
        other => match other.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
            Some((_, name)) => Step::did("tool", name.to_string()),
            None => Step::did("tool", other.to_string()),
        },
    })
}

/// The models pi offers (`get_available_models`: the ones of the providers it's signed in to), the one it answers with
/// when given none marked, with the thinking levels each takes. Starts `pi --mode rpc` for it with nothing of the
/// user's loaded but their logins and settings, given 20 seconds; empty when it can't say.
pub fn models(program: &Path, path_env: Option<&str>) -> Vec<coding::ToolModel> {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    let mut command = crate::disclaim::command(program, false);
    command
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
        ])
        .env("PI_OFFLINE", "1")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
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
            std::thread::sleep(std::time::Duration::from_secs(20));
            if !done.load(std::sync::atomic::Ordering::SeqCst) {
                watchdog.stop();
            }
        });
    }
    let mut stdin = child.stdin.take();
    crate::engine::write_line(&mut stdin, &json!({ "id": "state", "type": "get_state" }));
    crate::engine::write_line(&mut stdin, &json!({ "id": "models", "type": "get_available_models" }));
    let (mut state, mut available) = (Value::Null, Value::Null);
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            match message["id"].as_str() {
                Some("state") => state = message["data"].clone(),
                Some("models") => available = message["data"].clone(),
                _ => continue,
            }
            if !state.is_null() && !available.is_null() {
                break;
            }
        }
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    models_in(&available, &state)
}

/// The levels from lowest that pi takes for a model (its `getSupportedThinkingLevels`): none without reasoning; the
/// standard ones unless its map turns one off (null); `xhigh` and `max` only when it maps them. "off" isn't offered:
/// leaving the level unset is pi's default.
fn efforts(model: &Value) -> Vec<String> {
    if model["reasoning"] != true {
        return Vec::new();
    }
    let map = &model["thinkingLevelMap"];
    ["minimal", "low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .filter(|level| match map.get(*level) {
            Some(Value::Null) => false,
            Some(_) => true,
            None => !matches!(*level, "xhigh" | "max"),
        })
        .map(str::to_string)
        .collect()
}

/// `get_available_models`' answer as the pickers' rows, ids `provider/model`; a name two providers share says whose.
fn models_in(available: &Value, state: &Value) -> Vec<coding::ToolModel> {
    let models: Vec<&Value> = available["models"].as_array().into_iter().flatten().collect();
    let default = (state["model"]["provider"].as_str(), state["model"]["id"].as_str());
    let named = |model: &Value| model["name"].as_str().or(model["id"].as_str()).unwrap_or_default().to_string();
    models
        .iter()
        .filter_map(|model| {
            let (provider, id) = (model["provider"].as_str()?, model["id"].as_str()?);
            let full = format!("{provider}/{id}");
            if !crate::engine::valid_model(&full) {
                return None;
            }
            let mut name = named(model);
            if models.iter().filter(|other| named(other) == name).count() > 1 {
                name = format!("{name} ({provider})");
            }
            let is_default = default == (Some(provider), Some(id));
            let default_effort = is_default
                .then(|| state["thinkingLevel"].as_str().filter(|level| *level != "off").map(str::to_string))
                .flatten();
            Some(coding::ToolModel { id: full, name, efforts: efforts(model), default_effort, is_default })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    pub(crate) fn spec(mode: &str, chat: bool) -> Spec {
        Spec {
            engine: crate::engine::Engine::Pi,
            program: PathBuf::from("/bin/pi"),
            cwd: PathBuf::from("/w"),
            path_env: None,
            message: "Goal: x".into(),
            resume: None,
            new_session: None,
            mode: mode.into(),
            look_only: false,
            model: None,
            effort: None,
            instructions: "Work well.".into(),
            writable: vec![PathBuf::from("/proof")],
            timeout: Duration::from_secs(20),
            servers: Vec::new(),
            skills: chat.then(Vec::new),
            chat,
            command: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn the_command_line_and_what_the_extension_is_told() {
        let mut round = spec("auto", false);
        round.model = Some("openrouter/google/gemini-3.5-flash".into());
        round.effort = Some("high".into());
        round.resume = Some("01a1597b".into());
        let args = args(&round, Path::new("/tmp/filo.ts"));
        assert_eq!(
            args,
            [
                "--mode",
                "rpc",
                "-e",
                "/tmp/filo.ts",
                "--append-system-prompt",
                "Work well.",
                "--model",
                "openrouter/google/gemini-3.5-flash",
                "--thinking",
                "high",
                "--session",
                "01a1597b"
            ]
        );
        let config: Value = serde_json::from_str(&env(&round)[0].1).unwrap();
        assert_eq!(config["askShell"], true);
        assert_eq!(config["roots"], json!(["/w", "/proof"]));
        // A chat: none of the user's extensions or skills, and its shell isn't asked about.
        let mut chat = spec("auto", true);
        chat.servers = vec![crate::engine::McpServer {
            name: "filo".into(),
            program: "/F/filo".into(),
            args: vec!["--local-mcp".into()],
        }];
        let args = super::args(&chat, Path::new("/tmp/filo.ts"));
        assert!(args.contains(&"--no-extensions".to_string()) && args.contains(&"--no-skills".to_string()));
        let config: Value = serde_json::from_str(&env(&chat)[0].1).unwrap();
        assert_eq!(config["askShell"], false);
        assert_eq!(config["servers"], json!([{ "name": "filo", "command": "/F/filo", "args": ["--local-mcp"] }]));
    }

    #[test]
    fn tool_calls_read_as_lines() {
        assert_eq!(tool_step("bash", &json!({ "command": "cargo test" })), Some(Step::did("command", "cargo test")));
        assert_eq!(tool_step("edit", &json!({ "path": "src/a.rs" })), Some(Step::did("files", "Changed src/a.rs")));
        assert_eq!(tool_step("read", &json!({ "path": "README.md" })), Some(Step::did("tool", "Read README.md")));
        assert_eq!(tool_step("mcp__filo__send_message", &json!({})), Some(Step::did("tool", "send_message")));
        assert_eq!(
            text_of(&json!([{ "type": "text", "text": "a" }, { "type": "toolCall" }, { "type": "text", "text": "b" }])),
            "a\nb"
        );
    }

    #[test]
    fn models_read_as_the_pickers_rows() {
        let available = json!({ "models": [
            { "provider": "openrouter", "id": "google/gemini-3.5-flash", "name": "Gemini 3.5 Flash", "reasoning": true, "thinkingLevelMap": { "minimal": null, "xhigh": "xhigh" } },
            { "provider": "openai-codex", "id": "gpt-5.5", "name": "GPT-5.5", "reasoning": true },
            { "provider": "openrouter", "id": "openai/gpt-5.5", "name": "GPT-5.5", "reasoning": false },
            { "provider": "x", "id": "not a model", "name": "Bad" }
        ] });
        let state = json!({ "model": { "provider": "openai-codex", "id": "gpt-5.5" }, "thinkingLevel": "medium" });
        let rows = models_in(&available, &state);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].id.as_str(), rows[0].name.as_str(), rows[0].efforts.clone(), rows[0].is_default),
            (
                "openrouter/google/gemini-3.5-flash",
                "Gemini 3.5 Flash",
                vec!["low".to_string(), "medium".into(), "high".into(), "xhigh".into()],
                false
            )
        );
        assert_eq!(
            (rows[1].name.as_str(), rows[1].is_default, rows[1].default_effort.as_deref()),
            ("GPT-5.5 (openai-codex)", true, Some("medium"))
        );
        assert!(rows[2].efforts.is_empty());
    }

    /// Asks the installed pi which models it offers (no model is called): `cargo test -p filo-local real_pi_models --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_pi_models() {
        let program = crate::engine::find_program(crate::engine::Engine::Pi, None).expect("pi is installed");
        let models = models(&program, None);
        println!("{} models; default {:?}", models.len(), models.iter().find(|model| model.is_default));
        assert!(!models.is_empty());
    }
}

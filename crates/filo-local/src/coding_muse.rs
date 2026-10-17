// A round with Muse Code (coding_session.rs): `muse serve`, Meta's MSP session host, JSON-RPC 2.0 over stdio
// (https://meta-models.github.io/muse-code-sdk/1.4/). `initialize` and `initialized`, then `session/start` in the
// task's folder (or `session/resume` with its session), and `turn/start` with the message; every command carries a
// `commandId` of the client's own. What it does comes as notifications: items (`item/started` with a tool call's
// arguments, `item/completed` with its message), its todo list (`session/todoListChanged`), and the turn's end
// (`turn/completed`). A permission it needs is an `approval/request` and a question an `userInput/request`: each is
// acknowledged at once with an empty result, and answered later with `approval/decide` or `userInput/answer`. A note is
// `turn/steer`, which goes into the running turn (`expectedTurnId`) or is refused.
//
// A chat turn's notes are the messages the user sends while it works (steer.rs), steered in as they come and followed
// as every engine's are (coding_session.rs `Notes`): the turn's once Muse Code accepted one and did something after.
// One it keeps refusing mid-turn has the turn interrupted (`turn/interrupt`) for it, and once that ends (`cancelled`) a
// new turn in the same session starts with the message, after what the first one did; one refused because its turn
// had just ended ("already_terminal"), or accepted as it ended, starts the next turn the same way, without the
// interruption. The chat turn is over once its last turn ends with every note the turn's.
//
// Muse Code has no system prompt to give, so the instructions go in front of a new session's first message. How freely
// it works is the session's approval mode and permission profile (Full access: no sandbox, no prompts; Auto: its AI
// reviewer decides what it can and asks about the rest; Supervised: asks about anything no rule allows), and the host's
// sandbox flags. Its turns can't end with work left in the background, so a round ends with its turn.

use std::collections::HashMap;
use std::path::Path;
use std::process::ChildStdin;

use serde_json::{Value, json};

use crate::coding::{self, Step};
use crate::coding_session::{Ask, DENIED, Do, End, Host, Notes, Protocol, Spec};
use crate::engine::Pipe;

/// `muse serve` with the sandbox the task's mode asks for: none in Full access, no file writes for work that only
/// looks; the project's own skills and rules load, as Claude Code's do.
pub fn args(spec: &Spec) -> Vec<String> {
    let mut args = vec!["serve".to_string(), "--trust-workspace".to_string()];
    if spec.mode == "full_access" {
        args.push("--disable-sandbox".into());
    } else if spec.look_only {
        args.push("--disable-write".into());
    }
    args
}

/// Its launcher checks for an update in the background every hour: not from inside a round.
pub fn env() -> Vec<(String, String)> {
    vec![("MUSE_NO_AUTO_UPDATE".into(), "1".into())]
}

/// The session's approval mode and permission profile for the task's mode.
fn modes(spec: &Spec) -> (&'static str, &'static str) {
    match spec.mode.as_str() {
        "full_access" => ("allowAll", ":unrestricted"),
        _ if spec.look_only => ("onRequest", ":read-only"),
        "supervised" => ("promptUnmatched", ":ask-me"),
        "auto_accept_edits" => ("onRequest", ":ask-me"),
        _ => ("onRequest", ":auto-review"),
    }
}

/// A command's id: a UUIDv7, which Muse Code requires (time-ordered, the rest random).
fn command_id() -> String {
    let millis =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let random: [u8; 10] = uuid::Uuid::new_v4().as_bytes()[..10].try_into().unwrap_or_default();
    uuid::Builder::from_unix_timestamp_millis(millis, &random).into_uuid().to_string()
}

/// What a request this side sent was for.
#[derive(Clone, PartialEq)]
enum Sent {
    Initialize,
    Session {
        resumed: bool,
    },
    Turn,
    /// A note for the running turn.
    Steer(String),
    /// A chat turn's running turn, stopped for notes it refused.
    Interrupt,
    Other,
}

/// What it asked, waiting on answers.
enum Waiting {
    Approval { approval: String, requirement: Value, choices: Vec<Value>, key: String },
    Questions { input: String, questions: Vec<(String, Vec<String>, String)>, answers: Vec<Option<String>> },
}

pub struct Talk<'a> {
    spec: &'a Spec,
    pipe: Pipe<Sent>,
    session: Option<String>,
    /// The turn running now.
    turn: Option<String>,
    waiting: Vec<Waiting>,
    /// Approvals answered, with the answer: a command of several stages (`a | b && c`) is decided stage by stage, and
    /// the one answer covers them all.
    decided: HashMap<String, (&'static str, Value)>,
    /// The tool calls shown (an update of one isn't shown again).
    shown: Vec<String>,
    final_text: Option<String>,
    error: Option<String>,
    finished: bool,
    /// A turn was asked for and its answer hasn't come.
    starting: bool,
    /// Turns that ended while one was being started, before its id was known: the started one's end may be among them.
    ended_early: Vec<Value>,
    /// A chat turn's notes: the messages the user sends while it works.
    notes: Option<Notes>,
}

impl<'a> Talk<'a> {
    pub fn new(spec: &'a Spec, stdin: Option<ChildStdin>) -> Self {
        Talk {
            spec,
            pipe: Pipe::new(stdin),
            session: None,
            turn: None,
            waiting: Vec::new(),
            decided: HashMap::new(),
            shown: Vec::new(),
            final_text: None,
            error: None,
            finished: false,
            starting: false,
            ended_early: Vec::new(),
            notes: spec.chat.then(|| Notes::new(false)),
        }
    }

    fn request(&mut self, method: &str, params: Value, purpose: Sent) {
        self.pipe.request(method, params, purpose);
    }

    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
        if let Some(notes) = self.notes.as_mut() {
            notes.fail();
        }
    }

    fn start_session(&mut self, resume: bool) {
        match self.spec.resume.clone().filter(|_| resume) {
            Some(session) => {
                let mut params = json!({ "sessionId": session, "commandId": command_id(), "excludeItems": true });
                if let Some(config) = self.config() {
                    params["config"] = config;
                }
                self.request("session/resume", params, Sent::Session { resumed: true });
            }
            None => {
                let (approval, profile) = modes(self.spec);
                // Its folder first, then the ones it may also write in (the round's proof folder), as Muse Code wants
                // them: existing, canonical.
                let roots: Vec<String> = std::iter::once(self.spec.cwd.as_path())
                    .chain(self.spec.writable.iter().map(|dir| dir.as_path()))
                    .filter_map(canonical)
                    .collect();
                let mut params = json!({ "commandId": command_id(), "approvalMode": approval, "permissionProfile": profile, "workspaceRoots": roots });
                if let Some(root) = roots.first() {
                    params["workspaceRoot"] = json!(root);
                }
                if let Some(model) = &self.spec.model {
                    params["modelId"] = json!(model);
                }
                if let Some(config) = self.config() {
                    params["config"] = config;
                }
                self.request("session/start", params, Sent::Session { resumed: false });
            }
        }
    }

    /// The MCP servers it gets (a chat's), for `session/start` and `session/resume`: each over stdio; Filo's is
    /// required, an app's optional (one that doesn't start leaves the turn without it).
    fn config(&self) -> Option<Value> {
        if self.spec.servers.is_empty() {
            return None;
        }
        let servers: serde_json::Map<String, Value> = self
            .spec
            .servers
            .iter()
            .map(|server| {
                let mode = if server.name == crate::mcp::SERVER_NAME { "required" } else { "optional" };
                (server.name.clone(), json!({ "transport": "stdio", "command": server.program.display().to_string(), "args": server.args, "mode": mode }))
            })
            .collect();
        Some(json!({ "mcpServers": servers }))
    }

    fn start_turn(&mut self, text: &str) {
        let Some(session) = self.session.clone() else { return };
        let mut params =
            json!({ "sessionId": session, "commandId": command_id(), "input": [{ "type": "text", "text": text }] });
        if let Some(effort) = &self.spec.effort {
            params["reasoningEffort"] = json!(effort);
        }
        self.starting = true;
        self.request("turn/start", params, Sent::Turn);
    }

    /// `note` for the running turn.
    fn steer(&mut self, note: String) {
        let (Some(session), Some(turn)) = (self.session.clone(), self.turn.clone()) else { return };
        self.request("turn/steer", json!({ "sessionId": session, "commandId": command_id(), "expectedTurnId": turn, "input": [{ "type": "text", "text": note.clone() }] }), Sent::Steer(note));
    }

    /// Does what a chat turn's notes call for (`Notes`).
    fn apply(&mut self, todo: Vec<Do>) {
        for todo in todo {
            match todo {
                Do::Steer(note) => self.steer(note),
                Do::Interrupt => {
                    let (session, turn) = (self.session.clone(), self.turn.clone());
                    self.request(
                        "turn/interrupt",
                        json!({ "sessionId": session, "commandId": command_id(), "turnId": turn }),
                        Sent::Interrupt,
                    );
                }
                Do::StartTurn { message, .. } => self.start_turn(&message),
                Do::Finish { .. } => self.finished = true,
            }
        }
    }

    /// The running turn ended (`params`: its `turn/completed`).
    fn turn_completed(&mut self, params: &Value) {
        match params["terminal"].as_str().unwrap_or_default() {
            // A chat turn's goes on in a new turn for the notes it didn't take.
            "completed" | "cancelled" => match self.notes.as_mut() {
                Some(notes) => {
                    self.turn = None;
                    let todo = notes.turn_ended();
                    self.apply(todo);
                }
                None => self.finished = true,
            },
            _ => {
                let why = params["error"]["message"]
                    .as_str()
                    .or(params["reason"].as_str())
                    .unwrap_or("the turn failed")
                    .to_string();
                self.fail(format!("Muse Code: {why}"));
            }
        }
    }

    /// The turn being started is known by `turn`: it's the running one, and over if it has ended already.
    fn turn_known(&mut self, turn: Option<String>) {
        self.turn = turn.or(self.turn.take());
        let Some(turn) = self.turn.clone() else { return };
        let ended = std::mem::take(&mut self.ended_early).into_iter().find(|ended| ended["turnId"] == turn.as_str());
        if let Some(ended) = ended {
            self.turn_completed(&ended);
        }
    }

    fn response(&mut self, id: &Value, message: &Value, host: &mut dyn Host) {
        let Some((purpose, failed)) = self.pipe.answered(id, message) else { return };
        let result = &message["result"];
        match (purpose, failed) {
            (Sent::Initialize, None) => {
                self.pipe.notify("initialized");
                self.start_session(true);
            }
            // A chat starts afresh with its context instead (turn.rs).
            (Sent::Session { resumed: true }, Some(why)) if self.spec.chat => {
                self.fail(format!("Muse Code: session not found ({why})"))
            }
            (Sent::Session { resumed: true }, Some(why)) => {
                host.step(Step::marker(
                    "note",
                    format!("Couldn't carry on in its earlier Muse Code session ({why}): starting a new one"),
                ));
                self.start_session(false);
            }
            (Sent::Session { resumed }, None) => {
                let session = &result["session"];
                let Some(id) = session["sessionId"].as_str().map(str::to_string) else {
                    return self.fail("Muse Code didn't start a session.".into());
                };
                self.session = Some(id.clone());
                host.session(&id);
                if let Some(model) = session["modelId"].as_str().filter(|model| !model.is_empty() && !resumed) {
                    host.model(model);
                }
                // A resumed session keeps its model unless the task names one.
                if let (true, Some(model)) = (resumed, self.spec.model.clone()) {
                    self.request(
                        "session/setModel",
                        json!({ "sessionId": id, "commandId": command_id(), "model": { "modelId": model } }),
                        Sent::Other,
                    );
                }
                let message = if resumed {
                    self.spec.message.clone()
                } else {
                    format!("{}\n\n{}", self.spec.instructions, self.spec.message)
                };
                self.start_turn(&message);
            }
            (Sent::Turn, None) => {
                self.starting = false;
                if let Some(notes) = self.notes.as_mut() {
                    notes.turn_started(host);
                }
                self.turn_known(result["turnId"].as_str().map(str::to_string));
            }
            (Sent::Steer(note), None) => {
                if let Some(notes) = self.notes.as_mut() {
                    notes.accepted(&note);
                }
            }
            // A chat's turn that won't take it now, or has just ended ("already_terminal": no reason to interrupt it).
            (Sent::Steer(note), Some(why)) if self.notes.is_some() => {
                let counts = !why.contains("already_terminal");
                let todo = self.notes.as_mut().map(|notes| notes.refused(&note, counts)).unwrap_or_default();
                self.apply(todo);
            }
            (Sent::Steer(_), Some(why)) => host.step(Step::marker("note", format!("Muse Code: {why}"))),
            // The turn had ended already: its end comes, or has come.
            (Sent::Interrupt, _) => {}
            (Sent::Other, Some(why)) => host.step(Step::marker("note", format!("Muse Code: {why}"))),
            (Sent::Other, None) => {}
            (_, Some(why)) => self.fail(format!("Muse Code: {why}")),
        }
    }

    fn notification(&mut self, method: &str, params: &Value, host: &mut dyn Host) {
        match method {
            "turn/started" => self.turn_known(params["turnId"].as_str().map(str::to_string)),
            // A command shows as it starts, a change to files once it's made (one refused never is).
            "item/started" | "item/updated" | "item/completed" => {
                let item = &params["item"];
                // Something the running turn does, the notes it has taken in mind (not a message of the user's).
                let kind = item["kind"].as_str().unwrap_or_default();
                let running = self.turn.as_ref().is_some_and(|turn| item["turnId"] == turn.as_str());
                if let Some(notes) = self
                    .notes
                    .as_mut()
                    .filter(|_| method == "item/started" && running && !kind.to_ascii_lowercase().contains("user"))
                {
                    notes.acted(host);
                }
                let tool = item["tool"].as_str().unwrap_or_default();
                let made = method == "item/completed" && item["status"] == "completed";
                if item["kind"] == "toolCall" && (made || !FILE_TOOLS.contains(&tool)) {
                    let id = item["itemId"].as_str().unwrap_or_default().to_string();
                    if !self.shown.contains(&id)
                        && let Some(step) = tool_step(tool, &args_of(item))
                    {
                        self.shown.push(id);
                        host.step(step);
                    }
                }
                if method == "item/completed"
                    && item["kind"] == "agentMessage"
                    && self.turn.as_ref().is_none_or(|turn| item["turnId"] == turn.as_str())
                    && let Some(said) = item["text"].as_str().map(str::trim).filter(|said| !said.is_empty())
                {
                    self.final_text = Some(said.to_string());
                    host.step(Step::said(said));
                }
            }
            // The next stage of an approval answered already: the same answer.
            "approval/updated" => {
                let approval = params["approvalId"].as_str().unwrap_or_default().to_string();
                let requirement = &params["currentRequirementId"];
                let next = self
                    .decided
                    .get(&approval)
                    .filter(|(_, last)| !requirement.is_null() && last != requirement)
                    .map(|(wanted, _)| *wanted);
                if let Some(wanted) = next {
                    let choices = params["availableChoices"].as_array().cloned().unwrap_or_default();
                    self.decide(&approval, requirement, &choices, wanted);
                }
            }
            "approval/resolved" => {
                if let Some(approval) = params["approvalId"].as_str() {
                    self.decided.remove(approval);
                }
            }
            "session/todoListChanged" => {
                let items: Vec<String> = params["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|todo| {
                        Some(format!(
                            "{} {}",
                            if todo["status"] == "completed" { "☑" } else { "☐" },
                            todo["text"].as_str()?.trim()
                        ))
                    })
                    .collect();
                if !items.is_empty() {
                    host.step(Step::did("plan", items.join("\n")));
                }
            }
            "session/tokenUsage" => {
                if let Some(model) = params["modelId"].as_str().filter(|model| !model.is_empty()) {
                    host.model(model);
                }
            }
            "turn/retryScheduled" => {
                if let Some(reason) = params["reason"].as_str().filter(|reason| !reason.trim().is_empty()) {
                    host.step(Step::marker("note", format!("Muse Code retries: {reason}")));
                }
            }
            "turn/completed" => {
                // The running turn's end. One ending while the next is being started may be that one's, before its
                // id is known (kept for when it is); any other (one queued before the round's) doesn't end it.
                match self.turn.as_deref() {
                    Some(turn) if params["turnId"] == turn => self.turn_completed(params),
                    None if self.starting => self.ended_early.push(params.clone()),
                    None => self.turn_completed(params),
                    Some(_) => {}
                }
            }
            _ => {}
        }
    }

    fn server_request(&mut self, method: &str, id: Value, params: &Value, host: &mut dyn Host) {
        match method {
            "approval/request" => {
                // Seen: the decision is a command of its own.
                self.pipe.reply(&id, json!({}));
                let key = host.ask(&ask_for(params));
                let approval = params["approvalId"].as_str().unwrap_or_default().to_string();
                let choices = params["availableChoices"].as_array().cloned().unwrap_or_default();
                self.waiting.push(Waiting::Approval {
                    approval,
                    requirement: params["currentRequirementId"].clone(),
                    choices,
                    key,
                });
            }
            "userInput/request" => {
                self.pipe.reply(&id, json!({}));
                let mut questions = Vec::new();
                for question in params["questions"].as_array().into_iter().flatten() {
                    let text = |key: &str| {
                        question[key].as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
                    };
                    let options: Vec<String> = question["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|option| option["label"].as_str().map(str::to_string))
                        .collect();
                    let notes = question["options"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|option| option["label"].is_string())
                        .map(|option| option["description"].as_str().unwrap_or_default().to_string())
                        .collect();
                    let title = text("question").or_else(|| text("header")).unwrap_or_else(|| "A question".into());
                    let key = host.ask(&Ask {
                        kind: "question",
                        title,
                        detail: text("header"),
                        options: options.clone(),
                        notes,
                    });
                    questions.push((question["id"].as_str().unwrap_or_default().to_string(), options, key));
                }
                let answers = vec![None; questions.len()];
                let input = params["userInputId"].as_str().unwrap_or_default().to_string();
                self.waiting.push(Waiting::Questions { input, questions, answers });
            }
            other => self.pipe.refuse(&id, other),
        }
    }

    /// Decides one stage of an approval with the answer given (`allow`, `allow_always` or `deny`).
    fn decide(&mut self, approval: &str, requirement: &Value, choices: &[Value], wanted: &'static str) {
        let Some(session) = self.session.clone() else { return };
        self.decided.insert(approval.to_string(), (wanted, requirement.clone()));
        let choice = pick_choice(choices, wanted);
        let mut params = json!({ "sessionId": session, "approvalId": approval, "commandId": command_id(), "requirementId": requirement, "choiceId": choice });
        if wanted == "deny"
            && choices.iter().any(|option| option["choiceId"] == choice.as_str() && option["acceptsFeedback"] == true)
        {
            params["feedback"] = json!(DENIED);
        }
        self.request("approval/decide", params, Sent::Other);
    }

    fn deliver_answers(&mut self, host: &mut dyn Host) {
        let Some(session) = self.session.clone() else { return };
        let mut index = 0;
        while index < self.waiting.len() {
            let ready = match &mut self.waiting[index] {
                Waiting::Approval { key, .. } => host.answer(key).map(|answer| vec![answer]),
                Waiting::Questions { questions, answers, .. } => {
                    for ((_, _, key), answer) in questions.iter().zip(answers.iter_mut()) {
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
            match self.waiting.remove(index) {
                Waiting::Approval { approval, requirement, choices, .. } => {
                    let wanted =
                        crate::coding_handoff::approval_answer(answers.first().map(String::as_str).unwrap_or("deny"))
                            .unwrap_or("deny");
                    self.decide(&approval, &requirement, &choices, wanted);
                }
                Waiting::Questions { input, questions, .. } => {
                    let answers: Vec<Value> = questions
                        .iter()
                        .zip(answers)
                        .map(|((id, options, _), answer)| match options.iter().find(|option| option.eq_ignore_ascii_case(answer.trim())) {
                            Some(label) => json!({ "questionId": id, "selectedLabel": label }),
                            None => json!({ "questionId": id, "freeText": answer.chars().take(500).collect::<String>() }),
                        })
                        .collect();
                    self.request("userInput/answer", json!({ "sessionId": session, "commandId": command_id(), "userInputId": input, "answers": answers }), Sent::Other);
                }
            }
        }
    }
}

impl Protocol for Talk<'_> {
    fn begin(&mut self, _host: &mut dyn Host) {
        let mut capabilities = json!({ "experimentalApi": false });
        // A chat's MCP servers go with its session (`config`), which needs asking for.
        if !self.spec.servers.is_empty() {
            capabilities["requestedCapabilities"] = json!(["sessionMcp"]);
        }
        // Nobody answers a question mid-turn in a chat: it asks in its reply instead.
        if self.spec.chat {
            capabilities["userInputDialogs"] = json!(false);
        }
        self.request("initialize", json!({ "clientInfo": { "name": "filo_desktop", "version": env!("CARGO_PKG_VERSION") }, "capabilities": capabilities }), Sent::Initialize);
    }

    fn line(&mut self, line: &str, host: &mut dyn Host) {
        let Ok(message) = serde_json::from_str::<Value>(line) else { return };
        let method = message["method"].as_str();
        let id = message.get("id").cloned().filter(|id| !id.is_null());
        match (method, id) {
            (Some(method), Some(id)) => self.server_request(method, id, &message["params"], host),
            (Some(method), None) => self.notification(method, &message["params"], host),
            (None, Some(id)) => self.response(&id, &message, host),
            (None, None) => {}
        }
    }

    fn look(&mut self, host: &mut dyn Host) {
        self.deliver_answers(host);
        if let Some(notes) = self.notes.as_mut() {
            let todo = notes.look(host);
            return self.apply(todo);
        }
        // A coding round's notes, into its running turn.
        if self.turn.is_some() {
            for note in host.notes() {
                self.steer(note);
            }
        }
    }

    fn finished(&self) -> bool {
        self.finished
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

/// The models Muse Code offers (`model/list`, which asks nobody for anything): its catalogue, its default marked, with
/// the reasoning levels each takes. Starts `muse serve` for it, given 20 seconds; empty when it can't say.
pub fn models(program: &Path, path_env: Option<&str>) -> Vec<coding::ToolModel> {
    let mut command = crate::disclaim::command(program, false);
    command.args(["serve", "--no-session-log"]).envs(env());
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    let hello = json!({ "clientInfo": { "name": "filo_desktop", "version": env!("CARGO_PKG_VERSION") }, "capabilities": { "experimentalApi": false } });
    models_in(&crate::engine::ask_once(
        &mut command,
        hello,
        "model/list",
        json!({}),
        std::time::Duration::from_secs(20),
    ))
}

/// `model/list`'s answer as the pickers' rows.
fn models_in(answer: &Value) -> Vec<coding::ToolModel> {
    answer["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let id = model["modelId"].as_str().filter(|id| crate::engine::valid_model(id))?.to_string();
            let efforts = model["variants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|level| crate::engine::valid_model(level))
                .map(str::to_string)
                .collect();
            Some(coding::ToolModel {
                name: model_name(model["displayLabel"].as_str().unwrap_or(&id)),
                efforts,
                default_effort: model["defaultReasoningEffort"].as_str().map(str::to_string),
                is_default: model["isDefault"] == true,
                id,
            })
        })
        .collect()
}

/// "Muse Spark 1.3" for `muse-spark-1.3`: a model id as it reads.
fn model_name(id: &str) -> String {
    id.split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tools that change files.
const FILE_TOOLS: [&str; 5] = ["edit_file", "write_file", "apply_patch", "delete_file", "move_file"];

fn canonical(dir: &Path) -> Option<String> {
    dir.canonicalize().ok().map(|dir| dir.display().to_string())
}

/// A tool call's arguments: the model's JSON, as a string on the item.
fn args_of(item: &Value) -> Value {
    match &item["args"] {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// A tool call as a step: a command, the files it writes, what it reads or looks for. Its todo list comes as a plan
/// (`session/todoListChanged`), and asking the user as a question.
fn tool_step(tool: &str, args: &Value) -> Option<Step> {
    let field = |key: &str| args[key].as_str().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    Some(match tool {
        "bash" | "shell" | "bash_input" => {
            Step::did("command", coding::unwrap_shell(&field("command").or_else(|| field("input"))?))
        }
        "edit_file" | "write_file" | "apply_patch" | "delete_file" | "move_file" => {
            Step::did("files", format!("Changed {}", field("path").or_else(|| field("file_path"))?))
        }
        "read_file" => Step::did("tool", format!("Read {}", field("path")?)),
        "search" | "grep" | "glob" => Step::did("search", field("pattern").or_else(|| field("query"))?),
        "web_search" => Step::did("search", field("query")?),
        "web_fetch" => Step::did("tool", format!("Read {}", field("url")?)),
        "write_todos" | "request_user_input" => return None,
        "" => return None,
        other => Step::did("tool", other.to_string()),
    })
}

/// The choice for the answer: approved once; allowed always as a rule it keeps for the workspace (or the session) when
/// it offers one; else the rejection.
fn pick_choice(choices: &[Value], wanted: &str) -> String {
    let id = |choice: &Value| choice["choiceId"].as_str().unwrap_or_default().to_string();
    let decision = |choice: &Value| choice["decision"].as_str().unwrap_or_default().to_string();
    let once = || {
        choices
            .iter()
            .find(|choice| decision(choice) == "approved" && choice["scope"] == "once")
            .or_else(|| choices.iter().find(|choice| decision(choice) == "approved"))
            .map(id)
    };
    let always =
        || choices.iter().find(|choice| decision(choice).starts_with("approved") && choice["scope"] != "once").map(id);
    let refused = || {
        choices
            .iter()
            .find(|choice| !decision(choice).starts_with("approved"))
            .map(id)
            .unwrap_or_else(|| "abort".into())
    };
    match wanted {
        "allow_always" => always().or_else(once).unwrap_or_else(refused),
        "allow" => once().or_else(always).unwrap_or_else(refused),
        _ => refused(),
    }
}

/// What an approval request asks, in a line the conversation can judge.
fn ask_for(params: &Value) -> Ask {
    let subject = &params["subject"];
    let text = |value: &Value| value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
    let args: Value = params["rawArgs"].as_str().and_then(|raw| serde_json::from_str(raw).ok()).unwrap_or(Value::Null);
    let title = match subject["kind"].as_str().unwrap_or_default() {
        "shell" => {
            text(&subject["command"]).or_else(|| text(&args["command"])).map(|command| format!("Run `{command}`"))
        }
        "fileAccess" => text(&subject["path"]).or_else(|| text(&args["path"])).map(|path| format!("Change {path}")),
        "network" => {
            text(&subject["host"]).or_else(|| text(&subject["url"])).map(|host| format!("Reach {host} on the network"))
        }
        _ => None,
    };
    let tool = params["toolName"].as_str().unwrap_or("a tool");
    let title = title.or_else(|| tool_step(tool, &args).map(|step| step.text)).unwrap_or_else(|| format!("Use {tool}"));
    let detail = text(&args["description"]).or_else(|| text(&params["reason"]));
    Ask { kind: "approval", title, detail, options: Vec::new(), notes: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn spec(mode: &str, look_only: bool) -> Spec {
        Spec {
            engine: crate::engine::Engine::Muse,
            program: PathBuf::from("/bin/muse"),
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
            writable: Vec::new(),
            timeout: Duration::from_secs(20),
            servers: Vec::new(),
            skills: None,
            chat: false,
            command: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn modes_follow_the_task() {
        assert_eq!(modes(&spec("full_access", false)), ("allowAll", ":unrestricted"));
        assert_eq!(modes(&spec("auto", false)), ("onRequest", ":auto-review"));
        assert_eq!(modes(&spec("supervised", false)), ("promptUnmatched", ":ask-me"));
        assert_eq!(modes(&spec("auto", true)), ("onRequest", ":read-only"));
        assert_eq!(args(&spec("full_access", false)), ["serve", "--trust-workspace", "--disable-sandbox"]);
        assert_eq!(args(&spec("auto", true)), ["serve", "--trust-workspace", "--disable-write"]);
        // Muse Code takes UUIDv7 command ids only.
        assert_eq!(uuid::Uuid::parse_str(&command_id()).unwrap().get_version_num(), 7);
    }

    #[test]
    fn tool_calls_and_approvals_read_as_lines() {
        assert_eq!(tool_step("bash", &json!({ "command": "cargo test" })), Some(Step::did("command", "cargo test")));
        assert_eq!(
            tool_step("edit_file", &json!({ "path": "src/a.rs", "find": "x" })),
            Some(Step::did("files", "Changed src/a.rs"))
        );
        assert_eq!(tool_step("web_search", &json!({ "query": "muse code" })), Some(Step::did("search", "muse code")));
        assert_eq!(tool_step("write_todos", &json!({})), None);
        let item = json!({ "kind": "toolCall", "tool": "read_file", "args": "{\"path\":\"README.md\"}" });
        assert_eq!(tool_step("read_file", &args_of(&item)), Some(Step::did("tool", "Read README.md")));

        let request = json!({ "toolName": "bash", "rawArgs": "{\"command\":\"npm install\",\"description\":\"Install\"}", "subject": { "kind": "shell", "command": "npm install" } });
        assert_eq!(
            ask_for(&request),
            Ask {
                kind: "approval",
                title: "Run `npm install`".into(),
                detail: Some("Install".into()),
                options: vec![],
                notes: vec![]
            }
        );
        let choices = [
            json!({ "choiceId": "allow_once", "decision": "approved", "scope": "once" }),
            json!({ "choiceId": "allow_local_prefix", "decision": "approvedPolicyAmendment", "scope": "localPersistent" }),
            json!({ "choiceId": "abort", "decision": "abort", "scope": "once", "acceptsFeedback": true }),
        ];
        assert_eq!(
            (pick_choice(&choices, "allow"), pick_choice(&choices, "allow_always"), pick_choice(&choices, "deny")),
            ("allow_once".into(), "allow_local_prefix".into(), "abort".into())
        );
        assert_eq!(pick_choice(&choices[..1], "deny"), "abort");
        assert_eq!(pick_choice(&[choices[0].clone(), choices[2].clone()], "allow_always"), "allow_once");
    }

    #[test]
    fn models_read_as_the_pickers_rows() {
        let answer = json!({ "models": [
            { "modelId": "muse-wonder-1.0", "displayLabel": "muse-wonder-1.0", "isDefault": true, "variants": ["low", "high"], "defaultReasoningEffort": "high" },
            { "modelId": "muse-spark-1.3", "displayLabel": "muse-spark-1.3", "isDefault": false, "variants": ["minimal", "medium"] },
            { "modelId": "-bad" }
        ] });
        let rows = models_in(&answer);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].name.as_str(), rows[0].is_default, rows[0].default_effort.as_deref()),
            ("Muse Wonder 1.0", true, Some("high"))
        );
        assert_eq!(
            (rows[1].id.as_str(), rows[1].efforts.clone()),
            ("muse-spark-1.3", vec!["minimal".to_string(), "medium".into()])
        );
    }

    /// Asks the installed Muse Code which models it offers (no model is called): `cargo test -p filo-local
    /// real_muse_models -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_muse_models() {
        let program = crate::engine::find_program(crate::engine::Engine::Muse, None).expect("Muse Code is installed");
        let models = models(&program, None);
        println!("{models:#?}");
        assert!(models.iter().any(|model| model.is_default));
    }
}

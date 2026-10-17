// One round of a coding task handed off from a conversation (coding_handoff.rs), with the tool kept talking to rather
// than given its message and left alone: what it asks comes back while it waits, and a note written to it while it
// works reaches it at its next step.
//
// - Claude Code: `claude -p --input-format stream-json --output-format stream-json --verbose --permission-prompt-tool
//   stdio …`, the protocol the Claude Agent SDK speaks. The message is a JSON line on stdin, which stays open: a
//   permission it needs (or its AskUserQuestion) comes as a `control_request` (`can_use_tool`) and is answered with a
//   `control_response`; a note is one more user message. The round ends at its `result`, and closing stdin ends it.
// - Codex: `codex app-server`, JSON-RPC 2.0 over stdio: `initialize`, `thread/start` (or `thread/resume` with the
//   task's thread), `turn/start` with the message. Its approvals and questions are requests to answer
//   (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`,
//   `item/tool/requestUserInput`); a note is `turn/steer`; the round ends at `turn/completed`.
//
// A turn can end with background work still going: a command it ran in the background or a Monitor (Claude Code), a
// command still running in its terminal (Codex). Ending the CLI then would end that work too, so it's kept running:
// the host hears that its turn ended and what it waits on (`Host::background`), and the round carries on when the
// tool picks the task back up. Claude Code does that itself once the work ends (its `background_tasks_changed` and
// `task_notification` lines say what runs and what ended); Codex is started on a new turn saying how the work ended.
// A watcher that never ends on its own (a `tail -f`: `engine::endless_watcher`) isn't waited on: with only those left,
// Claude Code is asked after a while whether its work is done, and when it is they're stopped and the round ends.
//
// The caller (the app, src/local_backend/coding.rs) is the `Host`: it saves each step and what's asked, and is looked
// at every so often for answers, notes and a Stop. The CLI runs in a process group of its own, so Stop ends it all.
//
// A chat's turn runs the same way (engine.rs `start_turn`), with the chat's own command line (`Spec::command`): its
// notes are the messages the user sends while it works (steer.rs), followed the same way with every engine (`Notes`).
// Claude Code replays each user message as it takes it into the conversation (`--replay-user-messages`), and one it
// hasn't acted on by its result starts another turn of its own, which the chat's turn waits for; one Codex accepted but
// didn't act on before its turn ended, or refused as it ended, starts a new turn with the message. Muse Code's and
// OpenCode's chat turns take theirs as steers too (coding_muse.rs, coding_opencode.rs).

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::ChildStdin;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::coding::{self, Step};
use crate::engine::{BackgroundWork, Engine, EventReader, Pipe, Stopper};

/// Claude Code carries on by itself once its background work ends; if it hasn't after this long, it's told to.
const CARRY_ON_AFTER: Duration = Duration::from_secs(30);

/// A turn that ends with only watchers left in the background (a `tail -f` that never ends on its own:
/// `engine::endless_watcher`) has Claude Code asked after this long whether its work is done…
const CHECK_WATCHERS_AFTER: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 60 });

/// …and again after this long while it says it still waits on what they watch.
const CHECK_WATCHERS_AGAIN: Duration = Duration::from_secs(10 * 60);

/// Watchers it was done with get this long to stop before the round ends anyway.
const STOP_WATCHERS_FOR: Duration = Duration::from_secs(5);

/// A chat turn's CLI whose turn ended with notes it was handed and hasn't acted on (or steers it hasn't answered) is
/// given this long: Claude Code and OpenCode take a note that came as their turn ended in a turn of their own. Then the
/// chat turn is over, and those notes wait their own turns.
const TAKE_AFTER: Duration = Duration::from_secs(10);

/// A note the running turn refuses is offered again this often…
const RETRY_EVERY: Duration = Duration::from_secs(2);

/// …for this long; then the turn is interrupted for it, and the next one starts with it.
const RETRY_FOR: Duration = Duration::from_secs(6);

/// A chat turn's notes its CLI didn't take into the turn that was running, as the next turn's message: after it was
/// interrupted for them, it's told so, to carry on rather than start over.
pub(crate) fn restarted_with(notes: &[String], interrupted: bool) -> String {
    let notes = notes.join("\n\n");
    match interrupted {
        true => format!(
            "(You were stopped mid-turn so this could reach you. What you did and sent stays done: carry on from there.)\n\n{notes}"
        ),
        false => notes,
    }
}

/// Where a chat turn's note is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// Handed to the running turn: its steer not answered yet (Codex, Muse Code), or waiting in the CLI to be taken
    /// (Claude Code's stdin, OpenCode's inbox).
    Sent,
    /// The CLI has it, and hasn't done anything since.
    Accepted,
    /// Refused while the turn runs: offered again at `retry`.
    Refused { retry: Instant },
    /// For the next turn.
    Late,
    /// The next turn is being started with it.
    Starting,
}

#[derive(Debug)]
struct Note {
    text: String,
    stage: Stage,
    /// When the running turn first refused it (only refusals that count toward interrupting it).
    refused_since: Option<Instant>,
}

/// What an engine does next for its chat turn's notes (`Notes`).
#[derive(Debug, PartialEq)]
pub(crate) enum Do {
    /// Hand the running turn this note: a user message on Claude Code's stdin, `turn/steer`, a prompt delivered as
    /// `steer`.
    Steer(String),
    /// Stop the running turn: the notes it refused go into the next.
    Interrupt,
    /// Start the next turn with `message`, which carries `notes`.
    StartTurn { message: String, notes: Vec<String> },
    /// The chat turn is over. `untaken`: notes handed over that never became the turn's, to take back where the CLI
    /// keeps them (OpenCode's inbox).
    Finish { untaken: Vec<String> },
}

/// A chat turn's notes (steer.rs: the messages the user sends while it works), the same with every engine; each only
/// says what its CLI did, in its own words, and does what it's told (`Do`). A note goes to the running turn
/// (`Do::Steer`); once the CLI has it (`accepted`) and has done something since (`acted`), it's the turn's
/// (`Host::noted`). Only then: a CLI that has a note but ends its turn without acting on it hasn't answered it. One the
/// running turn refuses is offered again for a few seconds, then the turn is interrupted for it. One the turn ended
/// before taking (refused as the turn was over, or had and didn't act on) starts the next turn in the same session,
/// and is the turn's once that turn starts; Claude Code and OpenCode take such a note in a turn of their own
/// (`continues`), which is waited for (`TAKE_AFTER`). What never became the turn's when the chat turn ends, or fails,
/// waits its own turn (turn.rs), and nothing the CLI says after that counts.
pub(crate) struct Notes {
    notes: Vec<Note>,
    /// The CLI takes a note that came as its turn ended in a turn of its own (Claude Code, OpenCode).
    continues: bool,
    /// Its turn is running: notes go into it.
    running: bool,
    /// Its turn ended with notes it may still take, or steers not answered.
    waiting_since: Option<Instant>,
    /// The running turn is being interrupted for notes it refused: the next turn is told it was stopped.
    interrupted: bool,
    /// The chat turn is over.
    over: bool,
}

impl Notes {
    pub(crate) fn new(continues: bool) -> Notes {
        Notes { notes: Vec::new(), continues, running: false, waiting_since: None, interrupted: false, over: false }
    }

    /// The CLI's turn started (one it started itself, or `Do::StartTurn`'s): the notes it started with are the turn's.
    pub(crate) fn turn_started(&mut self, host: &mut dyn Host) {
        if self.over {
            return;
        }
        (self.running, self.waiting_since, self.interrupted) = (true, None, false);
        self.confirm(host, Stage::Starting);
    }

    /// Every so often: the notes come for the running turn, a refused one is offered again (or the turn is
    /// interrupted for it), and a CLI that hasn't taken what it was handed in time is given up on.
    pub(crate) fn look(&mut self, host: &mut dyn Host) -> Vec<Do> {
        if self.over {
            return Vec::new();
        }
        let now = Instant::now();
        let mut todo = Vec::new();
        if self.running && !self.interrupted {
            for note in &mut self.notes {
                match note.stage {
                    Stage::Refused { retry }
                        if now >= retry
                            && note.refused_since.is_some_and(|since| now.duration_since(since) >= RETRY_FOR) =>
                    {
                        self.interrupted = true;
                    }
                    // A refused one again, or one that was late for a turn that had started already.
                    Stage::Refused { retry } if now >= retry => {
                        note.stage = Stage::Sent;
                        todo.push(Do::Steer(note.text.clone()));
                    }
                    Stage::Late => {
                        note.stage = Stage::Sent;
                        todo.push(Do::Steer(note.text.clone()));
                    }
                    _ => {}
                }
            }
            if self.interrupted {
                todo.push(Do::Interrupt);
            } else {
                for text in host.notes() {
                    self.notes.push(Note { text: text.clone(), stage: Stage::Sent, refused_since: None });
                    todo.push(Do::Steer(text));
                }
            }
        }
        if !self.running && self.waiting_since.is_some_and(|since| since.elapsed() >= TAKE_AFTER) {
            todo.push(self.give_up());
        }
        todo
    }

    /// The notes handed over and not had yet, oldest first.
    pub(crate) fn sent(&self) -> Vec<String> {
        self.notes.iter().filter(|note| note.stage == Stage::Sent).map(|note| note.text.clone()).collect()
    }

    /// The CLI has `text` (Claude Code replayed it, a steer was accepted, OpenCode's inbox delivered it): it's the
    /// turn's once the CLI does something after.
    pub(crate) fn accepted(&mut self, text: &str) {
        if self.over {
            return;
        }
        if let Some(note) = self.notes.iter_mut().find(|note| note.stage == Stage::Sent && note.text == text) {
            note.stage = Stage::Accepted;
        }
    }

    /// The CLI did something after taking the notes it has: they're the turn's.
    pub(crate) fn acted(&mut self, host: &mut dyn Host) {
        if !self.over {
            self.confirm(host, Stage::Accepted);
        }
    }

    /// The running turn didn't take `text`: it's offered again in a moment, and once it has been refused for a few
    /// seconds (`counts`: a refusal; not one that never reached the CLI, or one because its turn was ending) the turn
    /// is interrupted for it. One whose turn is over goes into the next.
    pub(crate) fn refused(&mut self, text: &str, counts: bool) -> Vec<Do> {
        if self.over {
            return Vec::new();
        }
        let now = Instant::now();
        if let Some(note) = self.notes.iter_mut().find(|note| note.stage == Stage::Sent && note.text == text) {
            note.stage = Stage::Refused { retry: now + RETRY_EVERY };
            if counts {
                note.refused_since.get_or_insert(now);
            }
        }
        self.settle()
    }

    /// The CLI's turn ended (done, or interrupted): what it didn't take goes into the next one, or is waited for; with
    /// nothing left, the chat turn is over.
    pub(crate) fn turn_ended(&mut self) -> Vec<Do> {
        if self.over {
            return Vec::new();
        }
        self.running = false;
        self.settle()
    }

    /// The chat turn gives up on the notes it hasn't had: it's over.
    pub(crate) fn give_up(&mut self) -> Do {
        self.over = true;
        Do::Finish { untaken: std::mem::take(&mut self.notes).into_iter().map(|note| note.text).collect() }
    }

    /// The chat turn failed: it's over, and nothing the CLI says from now on counts.
    pub(crate) fn fail(&mut self) {
        self.over = true;
    }

    /// Its turn ended, and it's given time to take what it has.
    pub(crate) fn waiting(&self) -> bool {
        !self.over && !self.running && self.waiting_since.is_some()
    }

    fn confirm(&mut self, host: &mut dyn Host, stage: Stage) {
        let (confirmed, rest): (Vec<Note>, Vec<Note>) =
            std::mem::take(&mut self.notes).into_iter().partition(|note| note.stage == stage);
        self.notes = rest;
        for note in confirmed {
            host.noted(&note.text);
        }
    }

    /// Between turns: once the steers are answered, what the turn didn't take starts the next; with nothing left the
    /// chat turn is over; otherwise the CLI is given time to take what it has.
    fn settle(&mut self) -> Vec<Do> {
        if self.running || self.over {
            return Vec::new();
        }
        let now = Instant::now();
        if !self.continues && self.notes.iter().any(|note| note.stage == Stage::Sent) {
            self.waiting_since.get_or_insert(now);
            return Vec::new();
        }
        let continues = self.continues;
        for note in &mut self.notes {
            match note.stage {
                Stage::Refused { .. } => note.stage = Stage::Late,
                Stage::Accepted if !continues => note.stage = Stage::Late,
                _ => {}
            }
        }
        let starting = self.notes.iter().any(|note| note.stage == Stage::Starting);
        let late: Vec<String> =
            self.notes.iter().filter(|note| note.stage == Stage::Late).map(|note| note.text.clone()).collect();
        if !late.is_empty() && !starting {
            for note in self.notes.iter_mut().filter(|note| note.stage == Stage::Late) {
                note.stage = Stage::Starting;
            }
            self.waiting_since = None;
            let message = restarted_with(&late, std::mem::take(&mut self.interrupted));
            return vec![Do::StartTurn { message, notes: late }];
        }
        if self.notes.is_empty() {
            self.over = true;
            return vec![Do::Finish { untaken: Vec::new() }];
        }
        if self.notes.iter().any(|note| matches!(note.stage, Stage::Sent | Stage::Accepted)) {
            self.waiting_since.get_or_insert(now);
        }
        Vec::new()
    }
}

/// What a coding tool is told when its request was declined: the same for every tool.
pub(crate) const DENIED: &str = "Not allowed. Find another way, or say what you need and why.";

/// One round to run.
#[derive(Clone, Debug)]
pub struct Spec {
    pub engine: Engine,
    pub program: PathBuf,
    /// Where it works: the project folder or the task's worktree.
    pub cwd: PathBuf,
    pub path_env: Option<String>,
    /// The round's message: the brief, or the follow-ups since the last round.
    pub message: String,
    /// The task's session (Claude Code) or thread (Codex) to carry on in.
    pub resume: Option<String>,
    /// Claude Code: the id a new session gets, so it's known before it starts.
    pub new_session: Option<String>,
    /// The task's mode: the Coding page's auto or full_access, or supervised or auto_accept_edits for a task made before.
    pub mode: String,
    /// Investigate, review or research only: no edits.
    pub look_only: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// What it's told on top of the message (coding_handoff.rs `instructions`).
    pub instructions: String,
    /// Folders it may write in besides its own (the round's proof folder).
    pub writable: Vec<PathBuf>,
    /// A round is stopped after this long in all; a chat turn, which takes as long as its work does, once nothing has
    /// come from the tool for this long (it has hung).
    pub timeout: Duration,
    /// MCP servers it gets (a chat's: Filo's and the user's apps; none for a coding task). Muse Code and OpenCode only.
    pub servers: Vec<crate::engine::McpServer>,
    /// The skills it may use besides its built-in ones (a chat's: Filo's), or any (None: a coding task). OpenCode only:
    /// Muse Code has no allow-list.
    pub skills: Option<Vec<String>>,
    /// A chat turn rather than a coding round: a session it can't resume fails (the chat starts afresh with its
    /// context), rather than going on in a new one, and the turn ends with its reply, whatever runs in the background.
    pub chat: bool,
    /// A chat turn's own command line (engine.rs `invocation`), for Claude Code and Codex: its arguments and environment
    /// in place of a round's, and for Claude Code its first stdin lines in place of `message`.
    pub command: Option<crate::engine::Invocation>,
    /// Pictures the message comes with, for Codex as local images.
    pub images: Vec<PathBuf>,
}

/// Something the tool asks.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    /// "approval" (allow, allow_always or deny) or "question" (any answer).
    pub kind: &'static str,
    pub title: String,
    pub detail: Option<String>,
    pub options: Vec<String>,
    /// What each option means, by its place in `options` (empty where the tool said nothing).
    pub notes: Vec<String>,
}

/// What the round reports to, and is told by.
pub trait Host {
    fn step(&mut self, step: Step);
    /// The session or thread the round runs in.
    fn session(&mut self, id: &str);
    /// The model the tool says the round runs on (what its default or an alias came to).
    fn model(&mut self, _model: &str) {}
    /// The tool asks something; returns the key `answer` is asked with.
    fn ask(&mut self, ask: &Ask) -> String;
    /// The answer to what was asked under `key`, once there is one.
    fn answer(&mut self, key: &str) -> Option<String>;
    /// Notes to hand the tool now (they count as delivered once taken).
    fn notes(&mut self) -> Vec<String>;
    /// Note `note`, handed over, has reached it (a chat turn's: Claude Code replayed it, Codex, Muse Code or OpenCode
    /// took the steer, or a turn started with it).
    fn noted(&mut self, _note: &str) {}
    /// Each line the CLI prints, as it is.
    fn line(&mut self, _line: &str) {}
    fn stop_requested(&mut self) -> bool;
    /// Its turn ended with background work still going (named in `what`): the CLI is kept running for it. `end` has
    /// what it said at the end of the turn.
    fn background(&mut self, _end: &End, _what: &[String]) {}
    /// It picked the task back up after waiting on background work: `why` says how that work ended, when it's known.
    fn resumed(&mut self, _why: Option<&str>) {}
}

/// How a round ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct End {
    pub session_id: Option<String>,
    /// The tool's final message.
    pub final_text: Option<String>,
    pub error: Option<String>,
    pub stopped: bool,
    pub timed_out: bool,
    /// The tool's last lines on stderr when the round failed, for a problem report (failures.rs).
    pub stderr: Vec<String>,
}

/// Claude Code's command line for a round.
fn claude_args(spec: &Spec) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(str::to_string)
    .to_vec();
    match spec.mode.as_str() {
        "full_access" => args.push("--dangerously-skip-permissions".into()),
        mode => args.extend(["--permission-mode".into(), coding::claude_mode(mode).into()]),
    }
    if spec.look_only {
        args.extend(["--disallowedTools".into(), "Edit,Write,MultiEdit,NotebookEdit".into()]);
    }
    args.extend(["--append-system-prompt".into(), spec.instructions.clone()]);
    for dir in &spec.writable {
        args.extend(["--add-dir".into(), dir.display().to_string()]);
    }
    if let Some(model) = &spec.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &spec.effort {
        args.extend(["--effort".into(), effort.clone()]);
    }
    match (&spec.resume, &spec.new_session) {
        (Some(session), _) => args.extend(["--resume".into(), session.clone()]),
        (None, Some(session)) => args.extend(["--session-id".into(), session.clone()]),
        (None, None) => {}
    }
    args
}

/// Codex's settings for each of the page's modes: Supervised asks for anything but reads, Auto accept edits writes in
/// its folder and asks to go beyond it, Auto has Codex's own reviewer approve routine requests, Full access never asks
/// and has no sandbox. Returns (approval policy, sandbox, approvals reviewer). A chat turn's shell takes the same
/// (engine.rs `invocation`).
pub(crate) fn codex_mode(mode: &str) -> (&'static str, &'static str, Option<&'static str>) {
    match mode {
        "supervised" => ("untrusted", "workspace-write", None),
        "auto" => ("on-request", "workspace-write", Some("auto_review")),
        "full_access" => ("never", "danger-full-access", None),
        _ => ("on-request", "workspace-write", None),
    }
}

fn read_only(spec: &Spec) -> bool {
    spec.look_only && spec.mode != "full_access"
}

/// `thread/start`'s (and `thread/resume`'s) settings. A chat's instructions go in its first message instead, as its
/// sessions have always had them.
fn codex_thread_settings(spec: &Spec) -> Value {
    let (approval, sandbox, reviewer) = codex_mode(&spec.mode);
    let mut settings = json!({
        "cwd": spec.cwd.display().to_string(),
        "approvalPolicy": approval,
        "sandbox": if read_only(spec) { "read-only" } else { sandbox },
    });
    if spec.command.is_none() {
        settings["developerInstructions"] = json!(spec.instructions);
    }
    if let Some(model) = &spec.model {
        settings["model"] = json!(model);
    }
    if let Some(reviewer) = reviewer {
        settings["approvalsReviewer"] = json!(reviewer);
    }
    settings
}

/// `turn/start`'s parameters: the message, with its pictures.
fn codex_turn(spec: &Spec, thread: &str) -> Value {
    let mut turn = codex_turn_saying(spec, thread, &spec.message);
    if let Some(input) = turn["input"].as_array_mut() {
        input
            .extend(spec.images.iter().map(|path| json!({ "type": "localImage", "path": path.display().to_string() })));
    }
    turn
}

/// `turn/start`'s parameters for a turn with its own message (a follow-up after background work, a note).
fn codex_turn_saying(spec: &Spec, thread: &str, message: &str) -> Value {
    let (approval, sandbox, reviewer) = codex_mode(&spec.mode);
    let policy = if read_only(spec) {
        json!({ "type": "readOnly", "networkAccess": false })
    } else if sandbox == "danger-full-access" {
        json!({ "type": "dangerFullAccess" })
    } else {
        let roots: Vec<String> = spec.writable.iter().map(|dir| dir.display().to_string()).collect();
        json!({ "type": "workspaceWrite", "writableRoots": roots, "networkAccess": false, "excludeTmpdirEnvVar": false, "excludeSlashTmp": false })
    };
    let mut turn = json!({
        "threadId": thread,
        "input": [{ "type": "text", "text": message, "text_elements": [] }],
        "cwd": spec.cwd.display().to_string(),
        "approvalPolicy": approval,
        "sandboxPolicy": policy,
    });
    if let Some(model) = &spec.model {
        turn["model"] = json!(model);
    }
    if let Some(effort) = &spec.effort {
        turn["effort"] = json!(effort);
    }
    if let Some(reviewer) = reviewer {
        turn["approvalsReviewer"] = json!(reviewer);
    }
    turn
}

/// Runs a round to its end. `on_start` gets a Stopper as soon as the CLI runs.
pub fn run(spec: &Spec, host: &mut dyn Host, on_start: impl FnOnce(Stopper)) -> End {
    // OpenCode's server takes a password of the round's own.
    let password = uuid::Uuid::new_v4().simple().to_string();
    let (args, env) = match (spec.engine, &spec.command) {
        // Each user message comes back as it's taken, so a chat turn knows the messages it was handed are in.
        (Engine::Claude, Some(call)) => {
            (call.args.iter().cloned().chain(["--replay-user-messages".to_string()]).collect(), call.env.clone())
        }
        (Engine::Codex, Some(call)) => (call.args.clone(), call.env.clone()),
        (Engine::Claude, None) => (claude_args(spec), Vec::new()),
        (Engine::Codex, None) => (vec!["app-server".to_string()], Vec::new()),
        (Engine::Muse, _) => (crate::coding_muse::args(spec), crate::coding_muse::env()),
        (Engine::OpenCode, _) => (crate::coding_opencode::args(), crate::coding_opencode::env(&password, Some(spec))),
    };
    let mut started = match crate::engine::spawn_cli(
        spec.engine,
        &spec.program,
        &spec.cwd,
        spec.path_env.as_deref(),
        crate::computer::among(&spec.servers),
        &args,
        env,
        40,
    ) {
        Ok(started) => started,
        Err(error) => return End { error: Some(error), ..End::default() },
    };
    let stopper = started.stopper.clone();
    on_start(stopper.clone());
    let stdin = started.stdin.take();
    let talk: Box<dyn Protocol + '_> = match spec.engine {
        Engine::Claude | Engine::Codex => Box::new(Talk::new(spec, stdin)),
        Engine::Muse => Box::new(crate::coding_muse::Talk::new(spec, stdin)),
        Engine::OpenCode => Box::new(crate::coding_opencode::Talk::new(spec, stdin, password)),
    };
    let mut round = Round { talk, host, stopper: stopper.clone(), chat: spec.chat, trace: session_trace() };
    round.talk.begin(round.host);
    let drove = started.drive(&mut round, spec.timeout);
    let (mut talk, timed_out, stderr) = (round.talk, drove.timed_out, drove.stderr);
    let status = drove.status;

    let mut end = talk.end();
    end.timed_out = timed_out;
    end.stopped = stopper.is_stopped() && !timed_out && !talk.finished();
    if end.stopped {
        end.error = None;
    } else if timed_out && spec.chat {
        end.error = Some(crate::engine::hung_error(spec.engine, spec.timeout));
    } else if timed_out {
        end.error = Some(format!(
            "{} took longer than {} minutes and was stopped.",
            spec.engine.label(),
            spec.timeout.as_secs() / 60
        ));
    } else if end.error.is_none() && !talk.finished() {
        let code =
            status.and_then(|status| status.code()).map_or("a signal".to_string(), |code| format!("status {code}"));
        end.error = Some(match stderr.iter().rev().find(|line| !line.trim().is_empty()) {
            Some(detail) => format!("{} stopped with {code} before finishing: {}", spec.engine.label(), detail.trim()),
            None => format!("{} stopped with {code} before finishing.", spec.engine.label()),
        });
    }
    if let Some(error) = &end.error {
        let haystack = format!("{error}\n{}", stderr.join("\n"));
        if Engine::signed_out(&haystack) && !error.contains(&spec.engine.sign_in_hint()) {
            end.error = Some(format!("{error}\n\n{}", spec.engine.sign_in_hint()));
        }
        end.stderr = stderr;
    }
    end
}

/// A round as `engine::Started::drive` runs it: the CLI's protocol, the host it answers to, and Stop.
struct Round<'a, 'h> {
    talk: Box<dyn Protocol + 'a>,
    host: &'h mut dyn Host,
    stopper: Stopper,
    /// A chat turn goes on while it shows signs of life; a coding round has its time.
    chat: bool,
    /// `FILO_SESSION_TRACE=<file>`: every line the CLI prints goes there too, for looking into a round (`dev` builds).
    trace: Option<std::fs::File>,
}

impl crate::engine::Conversation for Round<'_, '_> {
    fn line(&mut self, line: &str) -> bool {
        if let Some(file) = self.trace.as_mut() {
            let _ = writeln!(file, "{line}");
        }
        self.host.line(line);
        self.talk.line(line, self.host);
        self.chat
    }

    fn look(&mut self) -> bool {
        if self.host.stop_requested() {
            self.stopper.stop();
        }
        self.talk.look(self.host);
        // It carried on after background work: the follow-up gets the round's time again.
        self.talk.carried_on() || (self.chat && self.talk.heard())
    }

    fn finished(&self) -> bool {
        self.talk.finished()
    }

    fn close(&mut self) {
        self.talk.close();
    }
}

/// The conversation with one CLI, in its own protocol: Claude Code's and Codex's here (`Talk`), Muse Code's
/// (coding_muse.rs) and OpenCode's (coding_opencode.rs).
pub(crate) trait Protocol {
    /// The first words: a handshake, or the message itself.
    fn begin(&mut self, host: &mut dyn Host);
    /// One line of its output.
    fn line(&mut self, line: &str, host: &mut dyn Host);
    /// Every so often while it works: answers to what it asked, notes, background work that ended.
    fn look(&mut self, host: &mut dyn Host);
    /// The round is over: its stdin closes.
    fn finished(&self) -> bool;
    /// It carried on after waiting on background work since last asked (the round's deadline starts again).
    fn carried_on(&mut self) -> bool {
        false
    }
    /// Something of the round's came from it since last asked other than a line of its output (OpenCode's events): a
    /// chat turn's deadline starts again.
    fn heard(&mut self) -> bool {
        false
    }
    fn close(&mut self);
    /// How the round ended, from what it said.
    fn end(&mut self) -> End;
}

impl Protocol for Talk<'_> {
    fn begin(&mut self, _host: &mut dyn Host) {
        Talk::begin(self);
    }

    fn line(&mut self, line: &str, host: &mut dyn Host) {
        Talk::line(self, line, host);
    }

    fn look(&mut self, host: &mut dyn Host) {
        self.deliver_answers(host);
        self.deliver_notes(host);
        self.background_ended(host);
        self.check_watchers();
    }

    fn finished(&self) -> bool {
        self.finished
    }

    fn carried_on(&mut self) -> bool {
        std::mem::take(&mut self.carried_on)
    }

    fn close(&mut self) {
        Talk::close(self);
    }

    fn end(&mut self) -> End {
        Talk::end(self)
    }
}

/// How to answer a protocol request once its asks have answers.
enum Reply {
    ClaudeTool { request_id: String, tool_use_id: Value, input: Value, suggestions: Value },
    ClaudeQuestions { request_id: String, tool_use_id: Value, input: Value, questions: Vec<String> },
    CodexDecision { id: Value },
    CodexPermissions { id: Value, permissions: Value },
    CodexInput { id: Value, question_ids: Vec<String> },
}

/// A protocol request waiting on its asks.
struct Waiting {
    reply: Reply,
    keys: Vec<String>,
    answers: Vec<Option<String>>,
}

/// What a Codex request this side sent was for.
#[derive(Clone, Copy, PartialEq)]
enum Sent {
    Initialize,
    Thread {
        resumed: bool,
    },
    Turn,
    /// A note for the turn under way (`turn/steer`).
    Steer,
    /// A chat turn's running turn, stopped for notes it refused.
    Interrupt,
}

/// The conversation with Claude Code or Codex.
struct Talk<'a> {
    spec: &'a Spec,
    /// Its stdin: Claude Code's JSON lines, Codex's JSON-RPC.
    pipe: Pipe<Sent>,
    waiting: Vec<Waiting>,
    finished: bool,
    session: Option<String>,
    final_text: Option<String>,
    error: Option<String>,
    /// Background work it started and hasn't heard the end of (Claude Code's tasks, Codex's commands by item id), and
    /// how what ended while it waited went, for the host (and Codex's follow-up turn).
    background: BackgroundWork,
    /// Its turn ended with background work still going: it's kept running until it carries on.
    monitoring: bool,
    /// When the last of the background work ended while it waited (Claude Code is told to carry on after a while).
    quiet_since: Option<Instant>,
    /// Since when only watchers run while it waits, and how long till it's asked whether its work is done.
    watched_since: Option<Instant>,
    check_after: Duration,
    /// It was asked whether its work is done: its next turn answers.
    checking: bool,
    /// It was done, and the watchers left are being stopped: since when (the round ends once they're gone).
    stopping: Option<Instant>,
    /// What it said as its turn ended with background work going: the round's report when it ends by stopping the
    /// watchers left.
    report: Option<String>,
    /// It carried on after waiting: the round's deadline starts again.
    carried_on: bool,
    /// A chat turn's notes: the messages the user sends while it works.
    notes: Option<Notes>,
    // Claude Code
    events: EventReader,
    // Codex
    thread: Option<String>,
    turn: Option<String>,
    /// A chat turn's notes sent with `turn/steer` whose answer hasn't come, by request.
    steering: HashMap<i64, String>,
}

impl<'a> Talk<'a> {
    fn new(spec: &'a Spec, stdin: Option<ChildStdin>) -> Self {
        Talk {
            spec,
            pipe: Pipe::new(stdin),
            waiting: Vec::new(),
            finished: false,
            session: None,
            final_text: None,
            error: None,
            background: BackgroundWork::default(),
            monitoring: false,
            quiet_since: None,
            watched_since: None,
            check_after: CHECK_WATCHERS_AFTER,
            checking: false,
            stopping: None,
            report: None,
            carried_on: false,
            // Claude Code takes a note that came as its turn ended in a turn of its own.
            notes: spec.chat.then(|| Notes::new(spec.engine == Engine::Claude)),
            events: EventReader::new(Engine::Claude),
            thread: None,
            turn: None,
            steering: HashMap::new(),
        }
    }

    fn write(&mut self, message: &Value) {
        self.pipe.write(message);
    }

    fn close(&mut self) {
        self.pipe.close();
    }

    fn begin(&mut self) {
        match self.spec.engine {
            Engine::Claude => match self.spec.command.as_ref().map(|call| call.stdin.clone()) {
                // A chat turn's own: its skills request, then the message with its pictures.
                Some(first) => {
                    for line in first.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
                        self.write(&line);
                    }
                }
                // The SDK's handshake (a CLI that doesn't know it carries on), then the message.
                None => {
                    self.write(&json!({ "type": "control_request", "request_id": "filo-initialize", "request": { "subtype": "initialize", "hooks": {} } }));
                    let message = self.spec.message.clone();
                    self.write(&claude_user(&message));
                }
            },
            _ => {
                let params = json!({
                    "clientInfo": { "name": "filo_desktop", "title": "Filo", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": {
                        "experimentalApi": true,
                        "requestAttestation": false,
                        "optOutNotificationMethods": ["turn/diff/updated", "item/agentMessage/delta", "item/reasoning/textDelta", "item/reasoning/summaryTextDelta", "item/commandExecution/outputDelta"],
                    },
                });
                self.request("initialize", params, Sent::Initialize);
            }
        }
    }

    fn request(&mut self, method: &str, params: Value, purpose: Sent) {
        self.pipe.request(method, params, purpose);
    }

    fn line(&mut self, line: &str, host: &mut dyn Host) {
        let Ok(message) = serde_json::from_str::<Value>(line) else { return };
        match self.spec.engine {
            Engine::Claude => self.claude(line, &message, host),
            _ => self.codex(&message, host),
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Claude Code

    fn claude(&mut self, line: &str, message: &Value, host: &mut dyn Host) {
        self.events.line(line);
        // What runs in the background, and what ended.
        self.background.claude_tool_calls(message);
        if self.background.claude_event(message) {
            self.settle();
            return;
        }
        // Its work was done and the watchers it left are being stopped: whatever comes now isn't the round's.
        if self.stopping.is_some() {
            return;
        }
        match message["type"].as_str().unwrap_or_default() {
            "system" if message["subtype"] == "init" => {
                if let Some(id) = message["session_id"].as_str().filter(|id| !id.is_empty())
                    && self.session.as_deref() != Some(id)
                {
                    self.session = Some(id.to_string());
                    host.session(id);
                }
                if let Some(model) = coding::claude_init_model(message) {
                    host.model(&model);
                }
                // A new turn: after background work ended (or a note), it carries on.
                if let Some(notes) = self.notes.as_mut() {
                    notes.turn_started(host);
                }
                // Answering whether its work is done isn't carrying on with it.
                if !self.checking {
                    self.carry_on(host, None);
                }
            }
            // A note it was handed, replayed as it takes it into the conversation: the first replay is the message
            // itself.
            "user" if message["isReplay"] == true => self.taken(&message["message"]["content"]),
            // A subagent's own steps aren't the round's.
            "assistant" if message["parent_tool_use_id"].is_null() => {
                if let Some(notes) = self.notes.as_mut() {
                    notes.acted(host);
                }
                // Asked whether its work is done: its answer isn't a step of the work, and stopping a watcher itself
                // isn't carrying on with it; anything else it does is.
                if self.checking {
                    let acts = message["message"]["content"].as_array().into_iter().flatten().any(|block| {
                        block["type"] == "tool_use"
                            && !matches!(block["name"].as_str(), Some("TaskStop" | "KillShell" | "KillBash"))
                    });
                    if !acts {
                        return;
                    }
                    self.checking = false;
                }
                self.carry_on(host, None);
                for step in coding::steps(Engine::Claude, line) {
                    host.step(step);
                }
            }
            // Its turn ended. With background work still going it's kept running, else the round is over. A chat turn
            // ends with its reply, unless a note it was handed is still to be acted on: its next turn takes it.
            "result" => {
                let failed = message["is_error"] == true
                    || message["subtype"].as_str().is_some_and(|subtype| subtype != "success");
                if self.checking && !failed {
                    let said = message["result"].as_str().map(str::trim).unwrap_or_default().to_string();
                    self.checked(host, &said);
                    return;
                }
                if let (Some(notes), false) = (self.notes.as_mut(), failed) {
                    let todo = notes.turn_ended();
                    self.apply(todo);
                } else if failed || self.spec.chat || self.background.is_empty() {
                    if let Some(notes) = self.notes.as_mut() {
                        notes.fail();
                    }
                    self.finished = true;
                } else {
                    let said =
                        message["result"].as_str().map(str::trim).filter(|said| !said.is_empty()).map(str::to_string);
                    self.wait_for_background(host, said);
                }
            }
            "control_request" => self.claude_request(message, host),
            _ => {}
        }
    }

    fn claude_request(&mut self, message: &Value, host: &mut dyn Host) {
        let request_id = message["request_id"].as_str().unwrap_or_default().to_string();
        let request = &message["request"];
        if request["subtype"] != "can_use_tool" {
            let why = format!("Filo doesn't handle {}", request["subtype"].as_str().unwrap_or("that"));
            self.write(&json!({ "type": "control_response", "response": { "subtype": "error", "request_id": request_id, "error": why } }));
            return;
        }
        let tool = request["tool_name"].as_str().unwrap_or("a tool").to_string();
        let input = request["input"].clone();
        let tool_use_id = request["tool_use_id"].clone();
        if tool == "AskUserQuestion" {
            let questions: Vec<Ask> = input["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|question| {
                    let title = question["question"].as_str()?.trim().to_string();
                    let (options, notes) = labelled_options(&question["options"]);
                    let detail =
                        question["header"].as_str().map(str::to_string).filter(|header| !header.trim().is_empty());
                    Some(Ask { kind: "question", title, detail, options, notes })
                })
                .collect();
            let keys: Vec<String> = questions.iter().map(|ask| host.ask(ask)).collect();
            let answers = vec![None; keys.len()];
            let questions = questions.into_iter().map(|ask| ask.title).collect();
            self.waiting.push(Waiting {
                reply: Reply::ClaudeQuestions { request_id, tool_use_id, input, questions },
                keys,
                answers,
            });
        } else {
            let key = host.ask(&ask_for(&tool, &input, request));
            let suggestions = request["permission_suggestions"].clone();
            self.waiting.push(Waiting {
                reply: Reply::ClaudeTool { request_id, tool_use_id, input, suggestions },
                keys: vec![key],
                answers: vec![None],
            });
        }
        // A request with nothing to ask (no questions) is answered at once.
        self.deliver_answers(host);
    }

    // ---------------------------------------------------------------------------------------------------------
    // Codex

    fn codex(&mut self, message: &Value, host: &mut dyn Host) {
        let method = message["method"].as_str();
        let id = message.get("id").cloned().filter(|id| !id.is_null());
        match (method, id) {
            (Some(method), Some(id)) => self.codex_request(method, id, &message["params"], host),
            (Some(method), None) => self.codex_notification(method, &message["params"], host),
            (None, Some(id)) => self.codex_response(&id, message, host),
            (None, None) => {}
        }
    }

    fn codex_response(&mut self, id: &Value, message: &Value, host: &mut dyn Host) {
        let Some((purpose, failed)) = self.pipe.answered(id, message) else { return };
        let result = &message["result"];
        match (purpose, failed) {
            (Sent::Initialize, None) => {
                self.pipe.notify("initialized");
                self.start_thread(host);
            }
            // A chat's turn fails, saying why: the chat starts afresh with its context (turn.rs).
            (Sent::Thread { resumed: true }, Some(why)) if self.spec.chat => self.fail(format!("Codex: {why}")),
            (Sent::Thread { resumed: true }, Some(why)) => {
                host.step(Step::marker(
                    "note",
                    format!("Couldn't carry on in its earlier Codex thread ({why}): starting a new one"),
                ));
                self.request("thread/start", codex_thread_settings(self.spec), Sent::Thread { resumed: false });
            }
            (Sent::Thread { .. }, None) => match result["thread"]["id"].as_str() {
                Some(thread) => {
                    let thread = thread.to_string();
                    self.session = Some(thread.clone());
                    host.session(&thread);
                    if let Some(model) = result["model"].as_str().filter(|model| !model.is_empty()) {
                        host.model(model);
                    }
                    let turn = codex_turn(self.spec, &thread);
                    self.thread = Some(thread);
                    self.request("turn/start", turn, Sent::Turn);
                }
                None => self.fail("Codex didn't start a thread.".into()),
            },
            (Sent::Turn, None) => {
                self.turn = result["turn"]["id"].as_str().map(str::to_string);
                if let Some(notes) = self.notes.as_mut() {
                    notes.turn_started(host);
                }
            }
            (Sent::Steer, failed) => {
                let note = id.as_i64().and_then(|id| self.steering.remove(&id)).unwrap_or_default();
                let Some(notes) = self.notes.as_mut() else { return };
                match failed {
                    // The turn has it.
                    None => notes.accepted(&note),
                    // Its turn had ended ("no active turn to steer"), or won't take it now.
                    Some(_) => {
                        let todo = notes.refused(&note, true);
                        self.apply(todo);
                    }
                }
            }
            // The turn had ended already: its end comes, or has come.
            (Sent::Interrupt, _) => {}
            (_, Some(why)) => self.fail(format!("Codex: {why}")),
        }
    }

    fn start_thread(&mut self, _host: &mut dyn Host) {
        match self.spec.resume.clone() {
            Some(thread) => {
                let mut settings = codex_thread_settings(self.spec);
                settings["threadId"] = json!(thread);
                settings["excludeTurns"] = json!(true);
                self.request("thread/resume", settings, Sent::Thread { resumed: true });
            }
            None => self.request("thread/start", codex_thread_settings(self.spec), Sent::Thread { resumed: false }),
        }
    }

    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
        if let Some(notes) = self.notes.as_mut() {
            notes.fail();
        }
    }

    fn codex_notification(&mut self, method: &str, params: &Value, host: &mut dyn Host) {
        let text = |value: &Value| value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
        match method {
            "item/started" => {
                let item = &params["item"];
                // Something it does, the notes it has taken in mind (not their own `userMessage` items).
                if let Some(notes) = self.notes.as_mut().filter(|_| item["type"] != "userMessage") {
                    notes.acted(host);
                }
                if item["type"] == "commandExecution"
                    && let Some(command) = text(&item["command"])
                {
                    let command = coding::unwrap_shell(&command);
                    // Running until its item completes: one still running when the turn ends is background work.
                    if let Some(id) = text(&item["id"]).filter(|_| item["status"] == "inProgress") {
                        self.background.running.push((id, command.clone()));
                    }
                    host.step(Step::did("command", command));
                }
            }
            "item/completed" => {
                let item = &params["item"];
                if item["type"] == "commandExecution" {
                    let id = text(&item["id"]).unwrap_or_default();
                    if let Some(at) = self.background.running.iter().position(|(running, _)| *running == id) {
                        let (_, command) = self.background.running.remove(at);
                        if self.monitoring {
                            self.background.ended.push(command_ended(&command, item));
                        }
                    }
                }
                let step = match item["type"].as_str().unwrap_or_default() {
                    "agentMessage" => text(&item["text"]).map(|said| {
                        self.final_text = Some(said.clone());
                        Step::said(&said)
                    }),
                    "fileChange" => {
                        let paths: Vec<String> = item["changes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|change| text(&change["path"]))
                            .collect();
                        (!paths.is_empty()).then(|| Step::did("files", format!("Changed {}", paths.join(", "))))
                    }
                    "webSearch" => text(&item["query"]).map(|query| Step::did("search", query)),
                    "mcpToolCall" => {
                        let name = [text(&item["server"]), text(&item["tool"])]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(".");
                        (!name.is_empty()).then(|| Step::did("tool", name))
                    }
                    "plan" => text(&item["text"]).map(|plan| Step::did("plan", plan)),
                    _ => None,
                };
                if let Some(step) = step {
                    host.step(step);
                }
            }
            "turn/completed" => {
                let turn = &params["turn"];
                if turn["status"] == "failed" {
                    let why = text(&turn["error"]["message"]).unwrap_or_else(|| "the turn failed".into());
                    self.fail(format!("Codex: {why}"));
                } else if let Some(notes) = self.notes.as_mut() {
                    // A chat turn ends with its reply, once what the user sent meanwhile is the turn's (`Notes`).
                    self.turn = None;
                    let todo = notes.turn_ended();
                    self.apply(todo);
                } else if self.background.is_empty() {
                    self.finished = true;
                } else {
                    // Commands still run in its terminals: it waits for them, then gets a turn saying how they ended.
                    self.turn = None;
                    let said = self.final_text.clone();
                    self.wait_for_background(host, said);
                }
            }
            "error" => {
                if params["willRetry"] != true
                    && let Some(message) = text(&params["error"]["message"])
                {
                    host.step(Step::marker("note", message));
                }
            }
            _ => {}
        }
    }

    fn codex_request(&mut self, method: &str, id: Value, params: &Value, host: &mut dyn Host) {
        let text = |value: &Value| value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
        let approval = |title: String, detail: Option<String>| Ask {
            kind: "approval",
            title,
            detail,
            options: Vec::new(),
            notes: Vec::new(),
        };
        match method {
            "item/commandExecution/requestApproval" => {
                let command = text(&params["command"])
                    .map(|command| coding::unwrap_shell(&command))
                    .unwrap_or_else(|| "a command".into());
                let key = host.ask(&approval(format!("Run `{command}`"), text(&params["reason"])));
                self.waiting.push(Waiting { reply: Reply::CodexDecision { id }, keys: vec![key], answers: vec![None] });
            }
            "item/fileChange/requestApproval" => {
                let title = text(&params["grantRoot"]).map_or_else(
                    || "Change files outside its folder".to_string(),
                    |root| format!("Write files in {root}"),
                );
                let key = host.ask(&approval(title, text(&params["reason"])));
                self.waiting.push(Waiting { reply: Reply::CodexDecision { id }, keys: vec![key], answers: vec![None] });
            }
            "item/permissions/requestApproval" => {
                let wanted = &params["permissions"];
                let mut granted = json!({});
                let mut what = Vec::new();
                if !wanted["network"].is_null() {
                    granted["network"] = wanted["network"].clone();
                    what.push("use the network".to_string());
                }
                if !wanted["fileSystem"].is_null() {
                    granted["fileSystem"] = wanted["fileSystem"].clone();
                    what.push(format!("reach more files ({})", cut(&wanted["fileSystem"].to_string(), 200)));
                }
                let what = if what.is_empty() { "get more permissions".to_string() } else { what.join(" and ") };
                let key = host.ask(&approval(format!("Let it {what}"), text(&params["reason"])));
                self.waiting.push(Waiting {
                    reply: Reply::CodexPermissions { id, permissions: granted },
                    keys: vec![key],
                    answers: vec![None],
                });
            }
            "item/tool/requestUserInput" => {
                let mut keys = Vec::new();
                let mut question_ids = Vec::new();
                for question in params["questions"].as_array().into_iter().flatten() {
                    let title = text(&question["question"])
                        .or_else(|| text(&question["header"]))
                        .unwrap_or_else(|| "A question".into());
                    let (options, notes) = labelled_options(&question["options"]);
                    keys.push(host.ask(&Ask { kind: "question", title, detail: None, options, notes }));
                    question_ids.push(question["id"].as_str().unwrap_or_default().to_string());
                }
                let answers = vec![None; keys.len()];
                self.waiting.push(Waiting { reply: Reply::CodexInput { id, question_ids }, keys, answers });
                self.deliver_answers(host);
            }
            // An MCP server asking for a form: nobody fills one in here.
            "mcpServer/elicitation/request" => self.pipe.reply(&id, json!({ "action": "decline" })),
            other => self.pipe.refuse(&id, other),
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Answers and notes

    fn deliver_answers(&mut self, host: &mut dyn Host) {
        let mut index = 0;
        while index < self.waiting.len() {
            let waiting = &mut self.waiting[index];
            for (key, answer) in waiting.keys.iter().zip(waiting.answers.iter_mut()) {
                if answer.is_none() {
                    *answer = host.answer(key);
                }
            }
            if waiting.answers.iter().all(Option::is_some) {
                let waiting = self.waiting.remove(index);
                let answers: Vec<String> = waiting.answers.into_iter().flatten().collect();
                let reply = respond(waiting.reply, &answers);
                self.write(&reply);
            } else {
                index += 1;
            }
        }
    }

    fn deliver_notes(&mut self, host: &mut dyn Host) {
        if let Some(notes) = self.notes.as_mut() {
            let todo = notes.look(host);
            return self.apply(todo);
        }
        // Codex between turns, waiting on background work: notes start its next turn.
        if self.spec.engine == Engine::Codex && self.monitoring {
            let Some(thread) = self.thread.clone() else { return };
            let notes = host.notes();
            if !notes.is_empty() {
                self.request("turn/start", codex_turn_saying(self.spec, &thread, &notes.join("\n\n")), Sent::Turn);
                self.carry_on(host, None);
            }
            return;
        }
        let ready = match self.spec.engine {
            Engine::Claude => self.pipe.is_open(),
            _ => self.thread.is_some() && self.turn.is_some(),
        };
        if !ready {
            return;
        }
        for note in host.notes() {
            match self.spec.engine {
                Engine::Claude => self.write(&claude_user(&note)),
                _ => {
                    self.steer(note);
                }
            }
        }
    }

    /// `note` for Codex's running turn (`turn/steer`).
    fn steer(&mut self, note: String) {
        let params = json!({
            "threadId": self.thread,
            "input": [{ "type": "text", "text": note, "text_elements": [] }],
            "expectedTurnId": self.turn,
        });
        let id = self.pipe.request("turn/steer", params, Sent::Steer);
        self.steering.insert(id, note);
    }

    /// Does what a chat turn's notes call for (`Notes`).
    fn apply(&mut self, todo: Vec<Do>) {
        for todo in todo {
            match (todo, self.spec.engine) {
                // One more user message: into the running turn, or a turn of its own once it's over.
                (Do::Steer(text) | Do::StartTurn { message: text, .. }, Engine::Claude) => {
                    self.write(&claude_user(&text))
                }
                (Do::Steer(text), _) => self.steer(text),
                (Do::StartTurn { message, .. }, _) => match self.thread.clone() {
                    Some(thread) => {
                        self.request("turn/start", codex_turn_saying(self.spec, &thread, &message), Sent::Turn)
                    }
                    None => self.fail("Codex has no thread to go on in.".into()),
                },
                (Do::Interrupt, Engine::Claude) => {
                    self.write(&json!({ "type": "control_request", "request_id": "filo-interrupt", "request": { "subtype": "interrupt" } }));
                }
                (Do::Interrupt, _) => {
                    let params = json!({ "threadId": self.thread, "turnId": self.turn });
                    self.request("turn/interrupt", params, Sent::Interrupt);
                }
                (Do::Finish { .. }, _) => self.finished = true,
            }
        }
    }

    /// Claude Code replayed a user message (`content`) as it took it into the conversation: the chat turn's notes in it
    /// are had, and the turn's once it acts after.
    fn taken(&mut self, content: &Value) {
        let Some(notes) = self.notes.as_mut() else { return };
        let said = match content {
            Value::String(text) => text.clone(),
            blocks => blocks
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        for note in notes.sent() {
            if said.contains(note.trim()) {
                notes.accepted(&note);
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Background work

    /// Its turn ended with background work going: the host hears what, and the CLI is kept running.
    fn wait_for_background(&mut self, host: &mut dyn Host, said: Option<String>) {
        self.monitoring = true;
        self.quiet_since = None;
        self.background.ended.clear();
        self.report = said.clone();
        self.settle();
        host.background(
            &End { session_id: self.session.clone(), final_text: said, ..End::default() },
            &self.background.what(),
        );
    }

    /// Nothing runs in the background anymore while it waits: from now on it's due to carry on. Only watchers run:
    /// it's due to be asked whether its work is done.
    fn settle(&mut self) {
        if self.monitoring && self.background.is_empty() && self.quiet_since.is_none() {
            self.quiet_since = Some(Instant::now());
        }
        if !self.monitoring || self.background.waits() || self.background.is_empty() {
            self.watched_since = None;
        } else if self.watched_since.is_none() && !self.checking {
            self.watched_since = Some(Instant::now());
        }
    }

    /// While it waits with only watchers running (a `tail -f` that never ends on its own), Claude Code is asked after
    /// a while whether its work is done; once it was, the round ends when they've stopped.
    fn check_watchers(&mut self) {
        if let Some(since) = self.stopping {
            if self.background.is_empty() || since.elapsed() >= STOP_WATCHERS_FOR {
                self.finished = true;
            }
            return;
        }
        if self.spec.engine != Engine::Claude
            || !self.monitoring
            || self.checking
            || self.watched_since.is_none_or(|since| since.elapsed() < self.check_after)
        {
            return;
        }
        self.watched_since = None;
        let watching = self.background.watching();
        if watching.is_empty() || self.background.waits() {
            return;
        }
        let what = watching.iter().map(|(_, what)| format!("\"{what}\"")).collect::<Vec<_>>().join(", ");
        self.write(&claude_user(&format!(
            "(Filo, not the user.) Your turn ended with only {what} still running in the background, which doesn't end on its own. \
If your work on the task is done, reply with just DONE: Filo stops it, and your last report stands as the result. \
If you're still waiting on something it shows you, reply in one line with what, and leave it running."
        )));
        self.checking = true;
    }

    /// Its answer to whether its work is done (`said`): DONE stops the watchers left and ends the round with the report
    /// it gave before; otherwise it's asked again after a while.
    fn checked(&mut self, host: &mut dyn Host, said: &str) {
        self.checking = false;
        let done = said
            .split_whitespace()
            .next()
            .is_some_and(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).eq_ignore_ascii_case("done"));
        if !done || self.background.waits() {
            self.check_after = CHECK_WATCHERS_AGAIN;
            self.watched_since = None;
            self.settle();
            return;
        }
        let watching = self.background.watching();
        for (task, _) in &watching {
            self.write(&json!({ "type": "control_request", "request_id": format!("filo-stop-{task}"), "request": { "subtype": "stop_task", "task_id": task } }));
        }
        let what = watching.iter().map(|(_, what)| what.as_str()).collect::<Vec<_>>().join("; ");
        host.step(Step::marker(
            "note",
            match what.is_empty() {
                true => "Its work was done".to_string(),
                false => format!("Its work was done: stopped what it left watching ({what})"),
            },
        ));
        self.monitoring = false;
        self.stopping = Some(Instant::now());
    }

    /// It started a new turn while it waited: the round carries on.
    fn carry_on(&mut self, host: &mut dyn Host, why: Option<String>) {
        if !self.monitoring {
            return;
        }
        self.monitoring = false;
        self.quiet_since = None;
        self.watched_since = None;
        self.check_after = CHECK_WATCHERS_AFTER;
        self.carried_on = true;
        let ended = std::mem::take(&mut self.background.ended);
        let why = why.or_else(|| (!ended.is_empty()).then(|| ended.join("\n")));
        host.resumed(why.as_deref());
    }

    /// While it waits, once its background work has all ended: Codex gets a turn saying how; Claude Code, which
    /// carries on by itself, is told to only if it hasn't after a while.
    fn background_ended(&mut self, host: &mut dyn Host) {
        if !self.monitoring || !self.background.is_empty() {
            return;
        }
        let message = if self.background.ended.is_empty() {
            "Your background work has ended. Carry on with the task from here, and end with your report.".to_string()
        } else {
            format!(
                "Your background work has ended:\n{}\n\nCarry on with the task from here, and end with your report.",
                self.background.ended.join("\n")
            )
        };
        match self.spec.engine {
            Engine::Codex => {
                let Some(thread) = self.thread.clone() else { return };
                self.request("turn/start", codex_turn_saying(self.spec, &thread, &message), Sent::Turn);
                self.carry_on(host, None);
            }
            _ => {
                if self.quiet_since.is_some_and(|since| since.elapsed() >= CARRY_ON_AFTER) {
                    self.write(&claude_user(&message));
                    // Once: its next turn is what ends the wait.
                    self.quiet_since = Some(Instant::now() + Duration::from_secs(24 * 60 * 60));
                }
            }
        }
    }

    fn end(&mut self) -> End {
        // Its turn had ended when it exited, while it was given time to take a note: it finished.
        if self.notes.as_ref().is_some_and(Notes::waiting) {
            self.finished = true;
        }
        let outcome = std::mem::replace(&mut self.events, EventReader::new(Engine::Claude)).finish();
        match self.spec.engine {
            Engine::Claude => End {
                session_id: self.session.clone().or(outcome.session_id),
                // Ended by stopping the watchers it left: its report is what it said before it was asked.
                final_text: if self.stopping.is_some() {
                    self.report.clone().or(outcome.final_text)
                } else {
                    outcome.final_text
                },
                error: self.error.clone().or(outcome.error),
                ..End::default()
            },
            _ => End {
                session_id: self.session.clone(),
                final_text: self.final_text.clone(),
                error: self.error.clone(),
                ..End::default()
            },
        }
    }
}

/// How a Codex command that ran past its turn ended: its exit code and the end of its output.
fn command_ended(command: &str, item: &Value) -> String {
    let code = item["exitCode"].as_i64().map_or_else(
        || format!("ended ({})", item["status"].as_str().unwrap_or("done")),
        |code| format!("exited with code {code}"),
    );
    let output = item["aggregatedOutput"].as_str().map(str::trim_end).unwrap_or_default();
    let skip = output.chars().count().saturating_sub(1_500);
    let tail: String = output.chars().skip(skip).collect();
    if tail.trim().is_empty() {
        format!("`{command}` {code}.")
    } else {
        format!("`{command}` {code}. The end of its output:\n```\n{tail}\n```")
    }
}

/// A user message for Claude Code's stream-json input.
fn claude_user(text: &str) -> Value {
    json!({ "type": "user", "message": { "role": "user", "content": text }, "parent_tool_use_id": null })
}

/// The reply a protocol request gets, from its asks' answers.
fn respond(reply: Reply, answers: &[String]) -> Value {
    let first = answers.first().map(String::as_str).unwrap_or("deny");
    let approval = crate::coding_handoff::approval_answer(first).unwrap_or("deny");
    match reply {
        Reply::ClaudeTool { request_id, tool_use_id, input, suggestions } => {
            let response = if approval == "deny" {
                json!({ "behavior": "deny", "message": DENIED, "toolUseID": tool_use_id })
            } else {
                let mut allow = json!({ "behavior": "allow", "updatedInput": input, "toolUseID": tool_use_id });
                if approval == "allow_always" && suggestions.as_array().is_some_and(|list| !list.is_empty()) {
                    allow["updatedPermissions"] = suggestions;
                }
                allow
            };
            json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id, "response": response } })
        }
        Reply::ClaudeQuestions { request_id, tool_use_id, mut input, questions } => {
            let answers: serde_json::Map<String, Value> =
                questions.into_iter().zip(answers.iter()).map(|(question, answer)| (question, json!(answer))).collect();
            if input.is_object() {
                input["answers"] = Value::Object(answers);
            }
            json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id, "response": { "behavior": "allow", "updatedInput": input, "toolUseID": tool_use_id } } })
        }
        Reply::CodexDecision { id } => {
            let decision = match approval {
                "allow" => "accept",
                "allow_always" => "acceptForSession",
                _ => "decline",
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": { "decision": decision } })
        }
        Reply::CodexPermissions { id, permissions } => {
            let result = match approval {
                "deny" => json!({ "permissions": {}, "scope": "turn" }),
                "allow_always" => json!({ "permissions": permissions, "scope": "session" }),
                _ => json!({ "permissions": permissions, "scope": "turn" }),
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        }
        Reply::CodexInput { id, question_ids } => {
            let answers: serde_json::Map<String, Value> = question_ids
                .into_iter()
                .zip(answers.iter())
                .map(|(question, answer)| (question, json!({ "answers": [answer] })))
                .collect();
            json!({ "jsonrpc": "2.0", "id": id, "result": { "answers": answers } })
        }
    }
}

/// What a Claude Code permission request asks for, in a line the conversation can judge.
fn ask_for(tool: &str, input: &Value, request: &Value) -> Ask {
    let field = |value: &Value, key: &str| {
        value[key].as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
    };
    let detail = field(request, "description").or_else(|| field(request, "decision_reason"));
    let title = match tool {
        "Bash" => format!(
            "Run `{}`{}",
            field(input, "command").unwrap_or_default(),
            field(request, "blocked_path").map(|path| format!(" (touches {path})")).unwrap_or_default()
        ),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => format!(
            "Change {}",
            field(input, "file_path").or_else(|| field(input, "notebook_path")).unwrap_or_else(|| "a file".into())
        ),
        "Read" | "Glob" | "Grep" => format!(
            "Read {} outside its folder",
            field(input, "file_path")
                .or_else(|| field(input, "path"))
                .or_else(|| field(input, "pattern"))
                .unwrap_or_else(|| "files".into())
        ),
        "WebFetch" => format!("Fetch {}", field(input, "url").unwrap_or_default()),
        "WebSearch" => format!("Search the web for \"{}\"", field(input, "query").unwrap_or_default()),
        other => {
            let shown = input
                .as_object()
                .filter(|object| !object.is_empty())
                .map(|_| format!(": {}", cut(&input.to_string(), 300)))
                .unwrap_or_default();
            format!("Use {other}{shown}")
        }
    };
    Ask { kind: "approval", title, detail, options: Vec::new(), notes: Vec::new() }
}

/// A question's options as the tools send them (`{label, description}`): the labels, and what each means.
fn labelled_options(options: &Value) -> (Vec<String>, Vec<String>) {
    options
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| {
            let label = option["label"].as_str().map(str::trim).filter(|label| !label.is_empty())?;
            Some((label.to_string(), option["description"].as_str().map(str::trim).unwrap_or_default().to_string()))
        })
        .unzip()
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>())
    }
}

/// `FILO_SESSION_TRACE`'s file, opened to append, in `dev` builds: the packaged app never writes a tool's output
/// anywhere it's told to.
pub(crate) fn session_trace() -> Option<std::fs::File> {
    if !cfg!(feature = "dev") {
        return None;
    }
    std::env::var_os("FILO_SESSION_TRACE")
        .and_then(|file| std::fs::OpenOptions::new().create(true).append(true).open(file).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::BufReader;
    use std::path::Path;
    use std::sync::{Arc, Mutex, mpsc};

    /// Records what the round reports and answers as told.
    #[derive(Default)]
    struct Recorder {
        steps: Vec<Step>,
        sessions: Vec<String>,
        asks: Vec<Ask>,
        answers: HashMap<String, String>,
        notes: Vec<String>,
        stop: bool,
        /// Set once something is asked.
        on_ask: Option<Arc<std::sync::atomic::AtomicBool>>,
        /// Each turn that ended with background work going: what it said, and what it waited on.
        background: Vec<(Option<String>, Vec<String>)>,
        /// Each time it carried on after waiting, with why.
        resumed: Vec<Option<String>>,
        /// The notes that reached the tool.
        noted: Vec<String>,
    }

    impl Host for Recorder {
        fn step(&mut self, step: Step) {
            self.steps.push(step);
        }
        fn session(&mut self, id: &str) {
            self.sessions.push(id.into());
        }
        fn ask(&mut self, ask: &Ask) -> String {
            self.asks.push(ask.clone());
            if let Some(flag) = &self.on_ask {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            format!("ask-{}", self.asks.len())
        }
        fn answer(&mut self, key: &str) -> Option<String> {
            self.answers.get(key).cloned()
        }
        fn notes(&mut self) -> Vec<String> {
            std::mem::take(&mut self.notes)
        }
        fn noted(&mut self, note: &str) {
            self.noted.push(note.to_string());
        }
        fn stop_requested(&mut self) -> bool {
            self.stop
        }
        fn background(&mut self, end: &End, what: &[String]) {
            self.background.push((end.final_text.clone(), what.to_vec()));
        }
        fn resumed(&mut self, why: Option<&str>) {
            self.resumed.push(why.map(str::to_string));
        }
    }

    use crate::test_support::script;

    fn spec(engine: Engine, program: PathBuf, cwd: &Path) -> Spec {
        Spec {
            engine,
            program,
            cwd: cwd.to_path_buf(),
            path_env: None,
            message: "Goal: list the files".into(),
            resume: None,
            new_session: Some("s-new".into()),
            mode: "auto_accept_edits".into(),
            look_only: false,
            model: Some("haiku".into()),
            effort: None,
            instructions: "Work well.".into(),
            writable: vec![cwd.join("proof")],
            timeout: Duration::from_secs(20),
            servers: Vec::new(),
            skills: None,
            chat: false,
            command: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn claude_command_line_and_codex_settings_follow_the_task() {
        let dir = Path::new("/w");
        let mut spec = spec(Engine::Claude, "/bin/claude".into(), dir);
        let args = claude_args(&spec);
        let has = |args: &[String], pair: [&str; 2]| args.windows(2).any(|window| window == pair);
        assert!(
            has(&args, ["--input-format", "stream-json"])
                && has(&args, ["--permission-prompt-tool", "stdio"])
                && has(&args, ["--permission-mode", "acceptEdits"])
        );
        assert!(
            has(&args, ["--add-dir", "/w/proof"])
                && has(&args, ["--session-id", "s-new"])
                && has(&args, ["--model", "haiku"])
        );
        assert!(!args.contains(&"--disallowedTools".to_string()));
        spec.look_only = true;
        spec.resume = Some("s-1".into());
        let args = claude_args(&spec);
        assert!(
            has(&args, ["--disallowedTools", "Edit,Write,MultiEdit,NotebookEdit"])
                && has(&args, ["--resume", "s-1"])
                && !args.contains(&"--session-id".to_string())
        );

        let thread = codex_thread_settings(&spec);
        assert_eq!(
            (thread["sandbox"].as_str(), thread["approvalPolicy"].as_str()),
            (Some("read-only"), Some("on-request"))
        );
        assert_eq!(codex_turn(&spec, "th")["sandboxPolicy"]["type"], "readOnly");
        spec.look_only = false;
        let turn = codex_turn(&spec, "th");
        assert_eq!(
            (turn["sandboxPolicy"]["type"].as_str(), turn["sandboxPolicy"]["writableRoots"][0].as_str()),
            (Some("workspaceWrite"), Some("/w/proof"))
        );
        spec.mode = "full_access".into();
        assert_eq!(codex_turn(&spec, "th")["sandboxPolicy"]["type"], "dangerFullAccess");
        spec.mode = "auto".into();
        assert_eq!(codex_thread_settings(&spec)["approvalsReviewer"], "auto_review");
    }

    #[test]
    fn asks_read_as_lines() {
        assert_eq!(
            ask_for("Bash", &json!({ "command": "npm install" }), &json!({ "description": "Install" })),
            Ask {
                kind: "approval",
                title: "Run `npm install`".into(),
                detail: Some("Install".into()),
                options: vec![],
                notes: vec![]
            }
        );
        assert_eq!(ask_for("Edit", &json!({ "file_path": "/etc/hosts" }), &json!({})).title, "Change /etc/hosts");
        assert_eq!(ask_for("mcp__x__y", &json!({}), &json!({})).title, "Use mcp__x__y");
        let deny = respond(Reply::CodexDecision { id: json!(4) }, &["deny".into()]);
        assert_eq!(deny["result"]["decision"], "decline");
        let always = respond(
            Reply::ClaudeTool {
                request_id: "r".into(),
                tool_use_id: json!("t"),
                input: json!({ "command": "ls" }),
                suggestions: json!([{ "type": "addRules" }]),
            },
            &["allow_always".into()],
        );
        assert_eq!(always["response"]["response"]["behavior"], "allow");
        assert_eq!(always["response"]["response"]["updatedPermissions"][0]["type"], "addRules");
    }

    /// A fake Claude Code that speaks the stream-json control protocol: it asks for permission to run a command, waits
    /// for the answer and a note (in whichever order they come), and reports what it heard.
    #[test]
    fn claude_code_asks_waits_for_the_answer_and_reads_a_note() {
        let dir = crate::test_support::temp_dir("session-claude");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/sh
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"npm install"}}]},"parent_tool_use_id":null}'
echo '{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"npm install"},"tool_use_id":"tu-1"}}'
read first
read second
both="$first$second"
behavior=$(echo "$both" | sed 's/.*"behavior":"\([a-z]*\)".*/\1/')
heard=$(echo "$both" | sed 's/.*"content":"\([^"]*\)".*/\1/')
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"$behavior; $heard\"}]},\"parent_tool_use_id\":null}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Done: $behavior, $heard\",\"session_id\":\"s-new\"}"
read rest
"#,
        );
        let spec = spec(Engine::Claude, fake, &dir);
        let mut host = Recorder::default();
        host.answers.insert("ask-1".into(), "allow".into());
        host.notes.push("keep the lockfile".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(end.error, None, "{end:?}");
        assert_eq!(end.final_text.as_deref(), Some("Done: allow, keep the lockfile"));
        assert_eq!(end.session_id.as_deref(), Some("s-new"));
        assert!(!end.stopped);
        assert_eq!(host.asks[0].title, "Run `npm install`");
        assert_eq!(host.steps[0], Step::did("command", "npm install"));
        assert_eq!(host.sessions, ["s-new"]);
    }

    /// A fake `codex app-server`: the handshake, a thread and a turn, an approval it waits on, and the turn's end.
    #[test]
    fn codex_app_server_asks_and_finishes() {
        let dir = crate::test_support::temp_dir("session-codex");
        let fake = script(
            &dir,
            "codex",
            r#"#!/bin/sh
read initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read initialized
read thread
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th-1"}}}'
read turn
case "$turn" in *'"sandboxPolicy":{"excludeSlashTmp"'*|*'"type":"workspaceWrite"'*) ;; *) echo "bad turn: $turn" >&2; exit 3;; esac
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"tu-1"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"type":"commandExecution","command":"/bin/zsh -lc '"'"'cargo test'"'"'"}}}'
echo '{"jsonrpc":"2.0","id":"srv-1","method":"item/commandExecution/requestApproval","params":{"command":"curl example.com","reason":"needs the network"}}'
read decision
case "$decision" in *'"decision":"acceptForSession"'*) word=always;; *) word=other;; esac
echo "{\"jsonrpc\":\"2.0\",\"method\":\"item/completed\",\"params\":{\"item\":{\"type\":\"agentMessage\",\"text\":\"Fetched it ($word)\"}}}"
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-1","status":"completed"}}}'
read rest
"#,
        );
        let mut spec = spec(Engine::Codex, fake, &dir);
        spec.new_session = None;
        let mut host = Recorder::default();
        host.answers.insert("ask-1".into(), "allow_always".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(end.error, None, "{end:?}");
        assert_eq!(end.final_text.as_deref(), Some("Fetched it (always)"));
        assert_eq!(end.session_id.as_deref(), Some("th-1"));
        assert_eq!(
            host.asks[0],
            Ask {
                kind: "approval",
                title: "Run `curl example.com`".into(),
                detail: Some("needs the network".into()),
                options: vec![],
                notes: vec![]
            }
        );
        assert_eq!(host.steps[0], Step::did("command", "cargo test"));
    }

    /// A chat turn's own command line for `program`: Claude Code's first stdin lines, or Codex's first message.
    fn chat_spec(engine: Engine, program: PathBuf, cwd: &Path) -> Spec {
        let first = match engine {
            Engine::Claude => format!(
                "{}\n{}\n",
                json!({ "type": "control_request", "request_id": "filo-init", "request": { "subtype": "initialize", "skills": [] } }),
                claude_user("Fill in the shoutouts")
            ),
            _ => "Fill in the shoutouts".to_string(),
        };
        let command = crate::engine::Invocation {
            program: program.clone(),
            args: vec!["--chat-args".into()],
            stdin: first.clone(),
            env: Vec::new(),
        };
        Spec { chat: true, message: first, new_session: None, command: Some(command), ..spec(engine, program, cwd) }
    }

    /// A chat turn's Claude Code that ended its turn before taking the message it was handed: it takes it as a turn of its
    /// own right after (as Claude Code does with a message that comes too late), and the chat turn waits for that one.
    #[test]
    fn a_chat_turns_claude_code_takes_a_late_message_in_one_more_turn() {
        let dir = crate::test_support::temp_dir("session-claude-steer");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/sh
case "$*" in *"--chat-args --replay-user-messages"*) ;; *) echo "args: $*" >&2; exit 3;; esac
read init
read message
case "$init" in *'"request_id":"filo-init"'*) ;; *) exit 4;; esac
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fill in the shoutouts"}]},"isReplay":true}'
sleep 1
echo '{"type":"result","subtype":"success","is_error":false,"result":"Filled them in.","session_id":"s-1"}'
read note
said=$(echo "$note" | sed 's/.*"content":"\([^"]*\)".*/\1/')
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"$said\"},\"isReplay\":true}"
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Holding: $said\"}]},\"parent_tool_use_id\":null}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Holding: $said\",\"session_id\":\"s-1\"}"
read rest
"#,
        );
        let spec = chat_spec(Engine::Claude, fake, &dir);
        let mut host = Recorder::default();
        host.notes.push("Don't send without my approval".into());
        let began = Instant::now();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref()),
            (None, Some("Holding: Don't send without my approval")),
            "{end:?}"
        );
        assert_eq!(host.noted.len(), 1);
        assert!(began.elapsed() < TAKE_AFTER, "{:?}", began.elapsed());
    }

    /// A chat turn's Claude Code that ends its turn without taking the message and starts none for it: the chat turn ends
    /// a moment later, the message not taken (it then waits its own turn, turn.rs).
    #[test]
    fn a_chat_turn_ends_when_its_claude_code_never_takes_the_message() {
        let dir = crate::test_support::temp_dir("session-claude-untaken");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/sh
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
sleep 1
echo '{"type":"result","subtype":"success","is_error":false,"result":"Filled them in.","session_id":"s-1"}'
read note
read rest
"#,
        );
        let spec = chat_spec(Engine::Claude, fake, &dir);
        let mut host = Recorder::default();
        host.notes.push("Don't send without my approval".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Filled them in.")), "{end:?}");
        assert_eq!(host.noted.len(), 0);
    }

    /// A chat turn's Claude Code that replays the message it was handed, then ends its turn before acting on it, has
    /// it but hasn't answered it: the chat turn waits for the turn it starts with it, and the message is the turn's
    /// once it acts there. One that never does is given up on.
    #[test]
    fn a_chat_turns_claude_code_that_ends_its_turn_before_acting_on_a_message_is_waited_for() {
        let dir = crate::test_support::temp_dir("session-claude-replayed");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/sh
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Filling them in."}]},"parent_tool_use_id":null}'
read note
said=$(echo "$note" | sed 's/.*"content":"\([^"]*\)".*/\1/')
echo "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"$said\"},\"isReplay\":true}"
echo '{"type":"result","subtype":"success","is_error":false,"result":"Filled them in.","session_id":"s-1"}'
case "$0" in *never*) read rest; exit 0;; esac
sleep 1
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Holding: $said\"}]},\"parent_tool_use_id\":null}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Holding: $said\",\"session_id\":\"s-1\"}"
read rest
"#,
        );
        let mut host = Recorder::default();
        host.notes.push("Don't send without my approval".into());
        let end = run(&chat_spec(Engine::Claude, fake.clone(), &dir), &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref()),
            (None, Some("Holding: Don't send without my approval")),
            "{end:?}"
        );
        assert_eq!(host.noted, ["Don't send without my approval"]);

        // It never takes it up: the chat turn ends without it.
        let never = dir.join("claude-never");
        std::fs::copy(&fake, &never).unwrap();
        let mut host = Recorder::default();
        host.notes.push("Don't send without my approval".into());
        let began = Instant::now();
        let end = run(&chat_spec(Engine::Claude, never, &dir), &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Filled them in.")), "{end:?}");
        assert!(host.noted.is_empty(), "{:?}", host.noted);
        assert!(
            began.elapsed() >= TAKE_AFTER && began.elapsed() < TAKE_AFTER + Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
    }

    /// A chat turn on Codex whose steer was accepted, but whose turn ended before it did anything after: the message
    /// starts the next turn, and is the turn's once Codex has started it (not when it's asked to); a turn that won't
    /// start fails the chat turn with the message not the turn's.
    #[test]
    fn a_chat_turn_on_codex_restarts_for_a_message_its_turn_ended_without_acting_on() {
        let dir = crate::test_support::temp_dir("session-codex-unacted");
        let fake = script(
            &dir,
            "codex",
            r#"#!/bin/sh
read initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read initialized
read thread
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th-1"}}}'
read turn
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"tu-1"}}}'
read steer
echo '{"jsonrpc":"2.0","id":4,"result":{"turnId":"tu-1"}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"agentMessage","text":"Filled them in."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-1","status":"completed"}}}'
read next
case "$next" in *'"method":"turn/start"'*'"text":"Hold them"'*) ;; *) echo "bad next turn: $next" >&2; exit 7;; esac
case "$0" in *refused*) echo '{"jsonrpc":"2.0","id":5,"error":{"code":-32600,"message":"the thread is busy"}}'; read rest; exit 0;; esac
echo '{"jsonrpc":"2.0","id":5,"result":{"turn":{"id":"tu-2"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"agentMessage","text":"Holding them."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-2","status":"completed"}}}'
read rest
"#,
        );
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let end = run(&chat_spec(Engine::Codex, fake.clone(), &dir), &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);

        let refused = dir.join("codex-refused");
        std::fs::copy(&fake, &refused).unwrap();
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let end = run(&chat_spec(Engine::Codex, refused, &dir), &mut host, |_| {});
        assert_eq!(end.error.as_deref(), Some("Codex: the thread is busy"), "{end:?}");
        assert!(host.noted.is_empty(), "{:?}", host.noted);
    }

    /// The shared steps every engine's chat turn takes its notes through (`Notes`), on their own.
    #[test]
    fn notes_count_once_the_cli_has_them_and_acts() {
        let mut host = Recorder::default();
        // Muse Code's way: steers answered, the next turn started for what the turn ended without acting on.
        let mut notes = Notes::new(false);
        assert!(notes.look(&mut host).is_empty(), "nothing goes before the turn runs");
        notes.turn_started(&mut host);
        host.notes.extend(["a".to_string(), "b".to_string()]);
        assert_eq!(notes.look(&mut host), [Do::Steer("a".into()), Do::Steer("b".into())]);
        notes.accepted("a");
        notes.acted(&mut host);
        notes.accepted("b");
        assert_eq!(host.noted, ["a"]);
        assert_eq!(notes.turn_ended(), [Do::StartTurn { message: "b".into(), notes: vec!["b".into()] }]);
        notes.turn_started(&mut host);
        assert_eq!(host.noted, ["a", "b"]);
        assert_eq!(notes.turn_ended(), [Do::Finish { untaken: Vec::new() }]);
        // Over: nothing counts from now on.
        notes.accepted("c");
        notes.acted(&mut host);
        assert!(notes.look(&mut host).is_empty() && notes.turn_ended().is_empty());

        // A steer still unanswered as the turn ends holds up what comes next.
        let mut host = Recorder::default();
        let mut notes = Notes::new(false);
        notes.turn_started(&mut host);
        host.notes.push("a".into());
        notes.look(&mut host);
        assert!(notes.turn_ended().is_empty());
        // Refused as its turn was over: the next turn starts with it, not told it was stopped.
        assert_eq!(notes.refused("a", false), [Do::StartTurn { message: "a".into(), notes: vec!["a".into()] }]);

        // Claude Code's way: what it has and ended its turn on is waited for, not started again.
        let mut host = Recorder::default();
        let mut notes = Notes::new(true);
        notes.turn_started(&mut host);
        host.notes.push("a".into());
        notes.look(&mut host);
        assert_eq!(notes.sent(), ["a"]);
        notes.accepted("a");
        assert!(notes.turn_ended().is_empty() && notes.waiting());
        notes.turn_started(&mut host);
        notes.acted(&mut host);
        assert_eq!(host.noted, ["a"]);
        assert_eq!(notes.turn_ended(), [Do::Finish { untaken: Vec::new() }]);
    }

    /// A chat turn on Muse Code that refuses a message's steer once, then takes it when it's offered again a moment
    /// later: no interruption.
    #[test]
    fn a_chat_turn_on_muse_code_offers_a_refused_message_again_before_interrupting() {
        let dir = crate::test_support::temp_dir("session-muse-retry");
        let fake = script(
            &dir,
            "muse",
            r#"#!/bin/sh
expect() { while read -r line; do echo "$line" >> "$0.heard"; case "$line" in $1) return 0;; esac; done; echo "never: $1" >&2; exit 3; }
id() { printf '%s\n' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/'; }
expect '*"method":"initialize"*'
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
expect '*"method":"session/start"*'
echo '{"jsonrpc":"2.0","id":2,"result":{"session":{"sessionId":"ms-1"}}}'
expect '*"method":"turn/start"*'
echo '{"jsonrpc":"2.0","id":3,"result":{"status":"accepted","turnId":"tu-1"}}'
expect '*"method":"turn/steer"*Hold them*'
echo "{\"jsonrpc\":\"2.0\",\"id\":$(id),\"error\":{\"code\":-32030,\"message\":\"turn/steer rejected: not steerable now\"}}"
expect '*"method":"turn/steer"*Hold them*'
echo "{\"jsonrpc\":\"2.0\",\"id\":$(id),\"result\":{\"status\":\"accepted\",\"turnId\":\"tu-1\"}}"
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"itemId":"c-2","kind":"toolCall","tool":"bash","turnId":"tu-1","args":"{}"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"itemId":"m-2","kind":"agentMessage","turnId":"tu-1","status":"completed","text":"Holding them."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-1","terminal":"completed"}}'
cat >> "$0.heard"
"#,
        );
        let spec = Spec { chat: true, new_session: None, ..spec(Engine::Muse, fake, &dir) };
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let began = Instant::now();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);
        assert!(began.elapsed() >= RETRY_EVERY, "{:?}", began.elapsed());
        assert!(!std::fs::read_to_string(dir.join("muse.heard")).unwrap().contains("turn/interrupt"));
    }

    /// A chat turn on Muse Code whose next turn (for a message that came as the first ended) ends before Muse Code has
    /// answered the `turn/start` that started it: that end is the round's, kept until the turn is known.
    #[test]
    fn a_chat_turn_on_muse_code_hears_a_restarted_turn_end_before_its_start_is_answered() {
        let dir = crate::test_support::temp_dir("session-muse-early-end");
        let fake = script(
            &dir,
            "muse",
            r#"#!/bin/sh
expect() { while read -r line; do echo "$line" >> "$0.heard"; case "$line" in $1) return 0;; esac; done; echo "never: $1" >&2; exit 3; }
expect '*"method":"initialize"*'
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
expect '*"method":"session/start"*'
echo '{"jsonrpc":"2.0","id":2,"result":{"session":{"sessionId":"ms-1"}}}'
expect '*"method":"turn/start"*'
echo '{"jsonrpc":"2.0","id":3,"result":{"status":"accepted","turnId":"tu-1"}}'
expect '*"method":"turn/steer"*Hold them*'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-1","terminal":"completed"}}'
echo '{"jsonrpc":"2.0","id":4,"error":{"code":-32030,"message":"turn/steer command x rejected: already_terminal"}}'
expect '*"method":"turn/start"*Hold them*'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"itemId":"m-2","kind":"agentMessage","turnId":"tu-2","status":"completed","text":"Holding them."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-2","terminal":"completed"}}'
sleep 1
echo '{"jsonrpc":"2.0","id":5,"result":{"status":"accepted","turnId":"tu-2"}}'
cat >> "$0.heard"
"#,
        );
        let spec = Spec { chat: true, new_session: None, ..spec(Engine::Muse, fake, &dir) };
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let began = Instant::now();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);
        assert!(began.elapsed() < Duration::from_secs(10), "{:?}", began.elapsed());
    }

    /// A chat turn on Codex's app-server: its first message and picture go in a turn on a thread without Filo's
    /// developer instructions (they're in the message); a message steered in mid-turn is taken, and one whose steer
    /// came after the turn ended starts the next turn, which the chat turn waits for.
    #[test]
    fn a_chat_turn_on_codex_steers_and_starts_a_turn_for_a_late_message() {
        let dir = crate::test_support::temp_dir("session-codex-steer");
        let fake = script(
            &dir,
            "codex",
            r#"#!/bin/sh
case "$*" in "--chat-args") ;; *) echo "args: $*" >&2; exit 3;; esac
read initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read initialized
read thread
case "$thread" in *developerInstructions*) echo "instructions in the thread" >&2; exit 4;; esac
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th-1"}}}'
read turn
case "$turn" in *'"text":"Fill in the shoutouts"'*'"/pics/a.png"'*) ;; *) echo "bad turn: $turn" >&2; exit 5;; esac
case "$turn" in *'"localImage"'*) ;; *) echo "no picture: $turn" >&2; exit 5;; esac
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"tu-1"}}}'
read steer
case "$steer" in *'"method":"turn/steer"'*'"expectedTurnId":"tu-1"'*) ;; *) echo "bad steer: $steer" >&2; exit 6;; esac
echo '{"jsonrpc":"2.0","id":4,"result":{"turnId":"tu-1"}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"u-1","type":"userMessage"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"m-1","type":"agentMessage","text":""}}}'
read late
echo '{"jsonrpc":"2.0","id":5,"error":{"code":-32600,"message":"no active turn to steer"}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"agentMessage","text":"Filled them in."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-1","status":"completed"}}}'
read next
case "$next" in *'"method":"turn/start"'*'"text":"Hold them"'*) ;; *) echo "bad next turn: $next" >&2; exit 7;; esac
echo '{"jsonrpc":"2.0","id":6,"result":{"turn":{"id":"tu-2"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"agentMessage","text":"Holding them."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-2","status":"completed"}}}'
read rest
"#,
        );
        let spec = Spec { images: vec!["/pics/a.png".into()], ..chat_spec(Engine::Codex, fake, &dir) };
        let mut host = Recorder::default();
        host.notes.extend(["Don't send without my approval".to_string(), "Hold them".to_string()]);
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref(), end.session_id.as_deref()),
            (None, Some("Holding them."), Some("th-1")),
            "{end:?}"
        );
        assert_eq!(host.noted.len(), 2);
    }

    /// A chat's Codex thread that can't be resumed fails the turn, saying why, rather than going on in a new one: the
    /// chat starts afresh with its context (turn.rs).
    #[test]
    fn a_chat_turns_codex_thread_that_is_gone_fails_the_turn() {
        let dir = crate::test_support::temp_dir("session-codex-gone");
        let fake = script(
            &dir,
            "codex",
            r#"#!/bin/sh
read initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read initialized
read thread
echo '{"jsonrpc":"2.0","id":2,"error":{"code":-32600,"message":"no rollout found for thread id th-gone"}}'
read rest
"#,
        );
        let spec = Spec { resume: Some("th-gone".into()), ..chat_spec(Engine::Codex, fake, &dir) };
        let end = run(&spec, &mut Recorder::default(), |_| {});
        assert_eq!(end.error.as_deref(), Some("Codex: no rollout found for thread id th-gone"));
    }

    /// A fake `muse serve`: the handshake, a session and a turn, a command it asks to run, a question it asks, a note
    /// steered in, and the turn's end.
    #[test]
    fn muse_code_asks_hears_a_note_and_finishes() {
        let dir = crate::test_support::temp_dir("session-muse");
        let fake = script(
            &dir,
            "muse",
            r#"#!/bin/sh
# Reads until a line matches the pattern (what Filo sends may come in any order), else fails.
expect() { while read -r line; do echo "$line" >> "$0.heard"; case "$line" in $1) return 0;; esac; done; echo "never: $1" >&2; exit 3; }
expect '*"method":"initialize"*'
echo '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"muse"}}}'
expect '*"method":"session/start"*"permissionProfile":":ask-me"*'
echo '{"jsonrpc":"2.0","id":2,"result":{"session":{"sessionId":"ms-1","modelId":"muse-spark-1.3"}}}'
expect '*"method":"turn/start"*Work well.*Goal: list the files*'
echo '{"jsonrpc":"2.0","id":3,"result":{"status":"accepted","turnId":"tu-1"}}'
echo '{"jsonrpc":"2.0","id":1,"method":"approval/request","params":{"approvalId":"ap-1","toolName":"bash","rawArgs":"{\"command\":\"npm install\"}","subject":{"kind":"shell","command":"npm install"},"currentRequirementId":{"approvalId":"ap-1","sourceIndex":0},"availableChoices":[{"choiceId":"allow_once","decision":"approved","scope":"once"},{"choiceId":"abort","decision":"abort","scope":"once"}]}}'
expect '*"method":"approval/decide"*"choiceId":"allow_once"*'
echo '{"jsonrpc":"2.0","method":"approval/updated","params":{"approvalId":"ap-1","currentRequirementId":{"approvalId":"ap-1","sourceIndex":1},"availableChoices":[{"choiceId":"allow_once","decision":"approved","scope":"once"},{"choiceId":"allow_local_prefix","decision":"approvedPolicyAmendment","scope":"localPersistent"},{"choiceId":"abort","decision":"abort","scope":"once"}]}}'
expect '*"method":"approval/decide"*"sourceIndex":1*"choiceId":"allow_once"*'
echo '{"jsonrpc":"2.0","method":"approval/updated","params":{"approvalId":"ap-1","currentRequirementId":{"approvalId":"ap-1","sourceIndex":1},"availableChoices":[]}}'
echo '{"jsonrpc":"2.0","method":"approval/resolved","params":{"approvalId":"ap-1","decision":"approved"}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"itemId":"it-1","kind":"toolCall","tool":"bash","turnId":"tu-1","args":"{\"command\":\"npm install\"}"}}}'
echo '{"jsonrpc":"2.0","id":2,"method":"userInput/request","params":{"userInputId":"ui-1","questions":[{"id":"word","header":"Word","question":"Which word?","options":[{"label":"hi"},{"label":"hello"}]}]}}'
expect '*"method":"userInput/answer"*"selectedLabel":"hello"*'
echo '{"jsonrpc":"2.0","method":"session/todoListChanged","params":{"items":[{"text":"Install","status":"completed"},{"text":"Test","status":"pending"}]}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"itemId":"it-2","kind":"agentMessage","turnId":"tu-1","status":"completed","text":"Installed, said hello, kept the lockfile."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-1","terminal":"completed"}}'
cat >> "$0.heard"
"#,
        );
        let mut spec = spec(Engine::Muse, fake, &dir);
        spec.mode = "supervised".into();
        spec.new_session = None;
        let mut host = Recorder::default();
        host.answers.insert("ask-1".into(), "allow".into());
        host.answers.insert("ask-2".into(), "Hello".into());
        host.notes.push("keep the lockfile".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref(), end.session_id.as_deref()),
            (None, Some("Installed, said hello, kept the lockfile."), Some("ms-1")),
            "{end:?}"
        );
        assert_eq!(host.asks[0].title, "Run `npm install`");
        assert_eq!(
            (host.asks[1].kind, host.asks[1].title.as_str(), host.asks[1].options.clone()),
            ("question", "Which word?", vec!["hi".to_string(), "hello".into()])
        );
        assert_eq!(
            host.steps,
            [
                Step::did("command", "npm install"),
                Step::did("plan", "☑ Install\n☐ Test"),
                Step::said("Installed, said hello, kept the lockfile.")
            ]
        );
        // The note went in as a steer of the running turn.
        let heard = std::fs::read_to_string(dir.join("muse.heard")).unwrap();
        // Each stage decided once.
        assert_eq!(heard.lines().filter(|line| line.contains("approval/decide")).count(), 2, "{heard}");
        assert!(
            heard.lines().any(|line| line.contains("\"method\":\"turn/steer\"")
                && line.contains("\"expectedTurnId\":\"tu-1\"")
                && line.contains("keep the lockfile")),
            "{heard}"
        );
    }

    /// A chat turn on Muse Code: one message it takes as a steer of the running turn; one it keeps refusing mid-turn,
    /// offered again for a few seconds and then interrupting the turn, and once it's cancelled a new turn in the same
    /// session starts with the message, told it was stopped; the chat turn ends with that one.
    #[test]
    fn a_chat_turn_on_muse_code_steers_and_restarts_for_a_refused_message() {
        let dir = crate::test_support::temp_dir("session-muse-steer");
        let fake = script(
            &dir,
            "muse",
            r#"#!/bin/sh
expect() { while read -r line; do echo "$line" >> "$0.heard"; case "$line" in $1) return 0;; esac; done; echo "never: $1" >&2; exit 3; }
expect '*"method":"initialize"*'
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
expect '*"method":"session/start"*'
echo '{"jsonrpc":"2.0","id":2,"result":{"session":{"sessionId":"ms-1"}}}'
expect '*"method":"turn/start"*Fill in the shoutouts*'
echo '{"jsonrpc":"2.0","id":3,"result":{"status":"accepted","turnId":"tu-1"}}'
id() { printf '%s\n' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/'; }
expect '*"method":"turn/steer"*"expectedTurnId":"tu-1"*Keep them short*'
echo '{"jsonrpc":"2.0","id":4,"result":{"status":"accepted","turnId":"tu-1"}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"itemId":"c-1","kind":"toolCall","tool":"bash","turnId":"tu-1","args":"{}"}}}'
# Refused each time it's offered, until the turn is interrupted.
while read -r line; do
  echo "$line" >> "$0.heard"
  case "$line" in
    *'"method":"turn/interrupt"'*'"turnId":"tu-1"'*) break;;
    *'"method":"turn/steer"'*'Hold them'*) echo "{\"jsonrpc\":\"2.0\",\"id\":$(id),\"error\":{\"code\":-32030,\"message\":\"turn/steer rejected: not steerable now\"}}";;
  esac
done
echo "{\"jsonrpc\":\"2.0\",\"id\":$(id),\"result\":{}}"
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-1","terminal":"cancelled"}}'
expect '*"method":"turn/start"*stopped mid-turn*Hold them*'
echo "{\"jsonrpc\":\"2.0\",\"id\":$(id),\"result\":{\"status\":\"accepted\",\"turnId\":\"tu-2\"}}"
echo '{"jsonrpc":"2.0","method":"turn/started","params":{"turnId":"tu-2"}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"itemId":"m-2","kind":"agentMessage","turnId":"tu-2","status":"completed","text":"Holding them."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-2","terminal":"completed"}}'
cat >> "$0.heard"
"#,
        );
        let spec = Spec {
            chat: true,
            message: "Fill in the shoutouts".into(),
            new_session: None,
            ..spec(Engine::Muse, fake, &dir)
        };
        let mut host = Recorder::default();
        host.notes.extend(["Keep them short".to_string(), "Hold them".to_string()]);
        let began = Instant::now();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref(), end.session_id.as_deref()),
            (None, Some("Holding them."), Some("ms-1")),
            "{end:?}"
        );
        assert_eq!(host.noted, ["Keep them short", "Hold them"]);
        assert!(began.elapsed() >= RETRY_FOR, "{:?}", began.elapsed());
    }

    /// A chat turn on Muse Code whose turn ended just before a message's steer: refused ("already_terminal"), it starts
    /// the next turn as it is, which the chat turn waits for.
    #[test]
    fn a_chat_turn_on_muse_code_starts_a_turn_for_a_late_message() {
        let dir = crate::test_support::temp_dir("session-muse-late");
        let fake = script(
            &dir,
            "muse",
            r#"#!/bin/sh
expect() { while read -r line; do echo "$line" >> "$0.heard"; case "$line" in $1) return 0;; esac; done; echo "never: $1" >&2; exit 3; }
expect '*"method":"initialize"*'
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
expect '*"method":"session/start"*'
echo '{"jsonrpc":"2.0","id":2,"result":{"session":{"sessionId":"ms-1"}}}'
expect '*"method":"turn/start"*'
echo '{"jsonrpc":"2.0","id":3,"result":{"status":"accepted","turnId":"tu-1"}}'
expect '*"method":"turn/steer"*Hold them*'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-1","terminal":"completed"}}'
echo '{"jsonrpc":"2.0","id":4,"error":{"code":-32030,"message":"turn/steer command x rejected: already_terminal"}}'
expect '*"method":"turn/start"*'
case "$line" in *stopped*) echo "told it was stopped: $line" >&2; exit 4;; *Hold\ them*) ;; *) exit 5;; esac
echo '{"jsonrpc":"2.0","id":5,"result":{"status":"accepted","turnId":"tu-2"}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"itemId":"m-2","kind":"agentMessage","turnId":"tu-2","status":"completed","text":"Holding them."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"tu-2","terminal":"completed"}}'
cat >> "$0.heard"
"#,
        );
        let spec = Spec { chat: true, new_session: None, ..spec(Engine::Muse, fake, &dir) };
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);
        assert!(!std::fs::read_to_string(dir.join("muse.heard")).unwrap().contains("turn/interrupt"));
    }

    /// A chat turn on OpenCode: a message steered in reaches the agent once its inbox delivers it (here in the execution
    /// after the one running, which ended first), and the chat turn ends with that execution.
    #[test]
    fn a_chat_turn_on_opencode_waits_for_its_steer_to_be_delivered() {
        let (port, heard) = opencode_server(|path, body, send| {
            let reply = match path {
                "POST /api/session" => r#"{"data":{"id":"ses-1"}}"#,
                "POST /api/session/ses-1/prompt" if body.contains("\"delivery\":\"steer\"") => {
                    send(&[
                        r#"{"type":"session.inbox.enqueued","data":{"sessionID":"ses-1","inboxID":"msg-2"}}"#,
                        r#"{"type":"session.text.ended","data":{"sessionID":"ses-1","text":"Filled them in."}}"#,
                        r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses-1"}}"#,
                        r#"{"type":"session.execution.started","data":{"sessionID":"ses-1"}}"#,
                        r#"{"type":"session.inbox.delivered","data":{"sessionID":"ses-1","inboxID":"msg-2"}}"#,
                        r#"{"type":"session.text.ended","data":{"sessionID":"ses-1","text":"Holding them."}}"#,
                        r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses-1"}}"#,
                    ]);
                    r#"{"data":{"id":"msg-2","delivery":"steer"}}"#
                }
                "POST /api/session/ses-1/prompt" => {
                    send(&[
                        r#"{"type":"session.execution.started","data":{"sessionID":"ses-1"}}"#,
                        r#"{"type":"session.inbox.delivered","data":{"sessionID":"ses-1","inboxID":"msg-1"}}"#,
                    ]);
                    r#"{"data":{"id":"msg-1"}}"#
                }
                _ => "{}",
            };
            (200, reply.to_string())
        });
        let dir = crate::test_support::temp_dir("session-opencode-steer");
        let spec = Spec { chat: true, ..spec(Engine::OpenCode, opencode_script(&dir, port), &dir) };
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);
        assert!(!heard.lock().unwrap().iter().any(|line| line.contains("/interrupt")));
    }

    /// A chat turn on OpenCode whose steer's prompt came back without an id: the next delivery of a prompt it doesn't
    /// know is that one, and the message is the turn's once the agent does something after (once: it isn't taken back
    /// and run again).
    #[test]
    fn a_chat_turn_on_opencode_knows_a_steer_given_no_id_by_its_delivery() {
        let (port, heard) = opencode_server(|path, body, send| {
            let reply = match path {
                "POST /api/session" => r#"{"data":{"id":"ses-1"}}"#,
                "POST /api/session/ses-1/prompt" if body.contains("\"delivery\":\"steer\"") => {
                    send(&[
                        r#"{"type":"session.inbox.delivered","data":{"sessionID":"ses-1","inboxID":"msg-2"}}"#,
                        r#"{"type":"session.step.started","data":{"sessionID":"ses-1","model":{}}}"#,
                        r#"{"type":"session.text.ended","data":{"sessionID":"ses-1","text":"Holding them."}}"#,
                        r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses-1"}}"#,
                    ]);
                    r#"{"data":{"delivery":"steer"}}"#
                }
                "POST /api/session/ses-1/prompt" => {
                    send(&[r#"{"type":"session.execution.started","data":{"sessionID":"ses-1"}}"#]);
                    r#"{"data":{"id":"msg-1"}}"#
                }
                _ => "{}",
            };
            (200, reply.to_string())
        });
        let dir = crate::test_support::temp_dir("session-opencode-no-id");
        let spec = Spec { chat: true, ..spec(Engine::OpenCode, opencode_script(&dir, port), &dir) };
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let began = Instant::now();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);
        assert!(began.elapsed() < TAKE_AFTER, "{:?}", began.elapsed());
        assert!(!heard.lock().unwrap().iter().any(|line| line.starts_with("DELETE")));
    }

    /// A chat turn on OpenCode that keeps refusing a message's steer: it's offered again for a few seconds, then the
    /// execution is interrupted, and once it's over a new prompt in the same session carries the message, told it was
    /// stopped; the chat turn ends with the execution that takes it.
    #[test]
    fn a_chat_turn_on_opencode_restarts_for_a_refused_message() {
        let (port, heard) = opencode_server(|path, body, send| match path {
            "POST /api/session" => (200, r#"{"data":{"id":"ses-1"}}"#.into()),
            "POST /api/session/ses-1/prompt" if body.contains("\"delivery\":\"steer\"") => {
                (400, r#"{"message":"steering isn't available"}"#.into())
            }
            "POST /api/session/ses-1/interrupt" => {
                send(&[r#"{"type":"session.execution.interrupted","data":{"sessionID":"ses-1"}}"#]);
                (200, r#"{"data":{"interrupted":true}}"#.into())
            }
            "POST /api/session/ses-1/prompt" if body.contains("stopped mid-turn") => {
                send(&[
                    r#"{"type":"session.execution.started","data":{"sessionID":"ses-1"}}"#,
                    r#"{"type":"session.inbox.delivered","data":{"sessionID":"ses-1","inboxID":"msg-2"}}"#,
                    r#"{"type":"session.text.ended","data":{"sessionID":"ses-1","text":"Holding them."}}"#,
                    r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses-1"}}"#,
                ]);
                (200, r#"{"data":{"id":"msg-2"}}"#.into())
            }
            "POST /api/session/ses-1/prompt" => {
                send(&[r#"{"type":"session.execution.started","data":{"sessionID":"ses-1"}}"#]);
                (200, r#"{"data":{"id":"msg-1"}}"#.into())
            }
            _ => (200, "{}".into()),
        });
        let dir = crate::test_support::temp_dir("session-opencode-restart");
        let spec = Spec { chat: true, ..spec(Engine::OpenCode, opencode_script(&dir, port), &dir) };
        let mut host = Recorder::default();
        host.notes.push("Hold them".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Holding them.")), "{end:?}");
        assert_eq!(host.noted, ["Hold them"]);
        let heard = heard.lock().unwrap().join("\n");
        assert!(heard.contains("POST /api/session/ses-1/interrupt") && heard.contains(r#"Hold them"}"#), "{heard}");
    }

    /// What a stand-in OpenCode server answers a request with (`opencode_server`): the path and body it got, and a way
    /// to send events; the status and the body ("" for none).
    type OpenCodeReply = fn(&str, &str, &dyn Fn(&[&str])) -> (u16, String);

    /// A stand-in for OpenCode's server, here on a port of its own: its events stream (`server.connected` first), every
    /// other request answered by `reply`. Its port, and what it was told ("<METHOD> <path> <body>").
    fn opencode_server(reply: OpenCodeReply) -> (u16, Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead as _, Read as _};
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // The events stream's sender, once it's open, and what the server was told.
        let events: Arc<Mutex<Option<mpsc::Sender<String>>>> = Arc::default();
        let heard: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let (events, heard) = (events.clone(), heard.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (events, heard) = (events.clone(), heard.clone());
                    std::thread::spawn(move || serve(stream, &events, &heard, reply));
                }
            });
        }
        fn serve(
            stream: TcpStream,
            events: &Mutex<Option<mpsc::Sender<String>>>,
            heard: &Mutex<Vec<String>>,
            reply: OpenCodeReply,
        ) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut out = stream;
            loop {
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    return;
                }
                let (mut length, mut authorized) = (0, false);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end().to_lowercase();
                    if header.is_empty() {
                        break;
                    }
                    if let Some(value) = header.strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    authorized |= header.starts_with("authorization: basic ");
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let path = request.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                heard.lock().unwrap().push(format!("{path} {body}"));
                if !authorized {
                    let _ = out.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                    continue;
                }
                if path == "GET /api/event" {
                    let (send, receive) = mpsc::channel::<String>();
                    *events.lock().unwrap() = Some(send);
                    let _ = out
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
                    let _ = out.write_all(b"data: {\"type\":\"server.connected\",\"data\":{}}\n\n");
                    for event in receive {
                        if out.write_all(format!("data: {event}\n\n").as_bytes()).is_err() {
                            return;
                        }
                    }
                    return;
                }
                let send = |list: &[&str]| {
                    if let Some(sender) = events.lock().unwrap().as_ref() {
                        for event in list {
                            let _ = sender.send(event.to_string());
                        }
                    }
                };
                let (status, reply) = reply(&path, &body, &send);
                let status = match status {
                    204 => "204 No Content",
                    400 => "400 Bad Request",
                    _ => "200 OK",
                };
                let _ = out.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                );
            }
        }
        (port, heard)
    }

    /// `opencode serve --stdio` for `opencode_server` on `port`: it says where it listens, and waits.
    fn opencode_script(dir: &Path, port: u16) -> PathBuf {
        script(
            dir,
            "opencode",
            &format!(
                "#!/bin/sh\n[ -n \"$OPENCODE_PASSWORD\" ] || exit 3\necho '{{\"url\":\"http://127.0.0.1:{port}\"}}'\ncat > /dev/null\n"
            ),
        )
    }

    /// A fake `opencode serve --stdio`: a script that says where it listens, and a small HTTP server here that serves
    /// its API and its events. A command it asks to run, a question it asks, a note steered in, and the turn's end.
    #[test]
    fn opencode_asks_hears_a_note_and_finishes() {
        let (port, heard) = opencode_server(|path, body, send| {
            let reply = match path {
                "POST /api/session" => r#"{"data":{"id":"ses-1"}}"#,
                "POST /api/session/ses-1/prompt" if body.contains("\"delivery\":\"steer\"") => {
                    r#"{"data":{"delivery":"steer"}}"#
                }
                "POST /api/session/ses-1/prompt" => {
                    send(&[
                        r#"{"type":"session.step.started","data":{"sessionID":"ses-1","model":{"providerID":"opencode","id":"mimo-free"}}}"#,
                        r#"{"type":"session.tool.input.started","data":{"sessionID":"ses-1","id":"call-1","name":"shell"}}"#,
                        r#"{"type":"session.tool.called","data":{"sessionID":"ses-1","id":"call-1","input":{"command":"npm install"}}}"#,
                        r#"{"type":"session.tool.called","data":{"sessionID":"other","id":"call-9","input":{"command":"not ours"}}}"#,
                        r#"{"type":"permission.asked","data":{"id":"per-1","sessionID":"ses-1","action":"shell","resources":["npm install"],"source":{"id":"call-1"}}}"#,
                    ]);
                    r#"{"data":{"id":"msg-1"}}"#
                }
                "POST /api/session/ses-1/permission/per-1/reply" => {
                    send(&[
                        r#"{"type":"session.tool.progress","data":{"sessionID":"ses-1","id":"call-1","metadata":{"shellID":"sh-1"}}}"#,
                        r#"{"type":"session.tool.success","data":{"sessionID":"ses-1","id":"call-1"}}"#,
                        r#"{"type":"session.tool.input.started","data":{"sessionID":"ses-1","id":"call-2","name":"write"}}"#,
                        r#"{"type":"session.tool.called","data":{"sessionID":"ses-1","id":"call-2","input":{"path":"color.txt","content":"Blue"}}}"#,
                        r#"{"type":"form.created","data":{"form":{"id":"frm-1","sessionID":"ses-1","metadata":{"kind":"question"},"fields":[{"key":"q0","title":"Color","description":"Which color do you prefer?","options":[{"value":"Red","label":"Red"},{"value":"Blue","label":"Blue"}],"custom":true}]}}}"#,
                    ]);
                    ""
                }
                "POST /api/session/ses-1/form/frm-1/reply" => {
                    send(&[
                        r#"{"type":"session.tool.success","data":{"sessionID":"ses-1","id":"call-2"}}"#,
                        r#"{"type":"session.text.ended","data":{"sessionID":"ses-1","text":"Installed, wrote Blue, kept the lockfile."}}"#,
                        r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses-1"}}"#,
                    ]);
                    ""
                }
                _ => "{}",
            };
            (if reply.is_empty() { 204 } else { 200 }, reply.to_string())
        });
        let dir = crate::test_support::temp_dir("session-opencode");
        let fake = opencode_script(&dir, port);
        let mut spec = spec(Engine::OpenCode, fake, &dir);
        spec.mode = "supervised".into();
        spec.model = Some("opencode/mimo-free".into());
        let mut host = Recorder::default();
        host.answers.insert("ask-1".into(), "allow".into());
        host.answers.insert("ask-2".into(), "blue".into());
        host.notes.push("keep the lockfile".into());
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref(), end.session_id.as_deref()),
            (None, Some("Installed, wrote Blue, kept the lockfile."), Some("ses-1")),
            "{end:?}"
        );
        assert_eq!(host.asks[0].title, "Run `npm install`");
        assert_eq!(
            (host.asks[1].kind, host.asks[1].title.as_str(), host.asks[1].options.clone()),
            ("question", "Which color do you prefer?", vec!["Red".to_string(), "Blue".into()])
        );
        assert_eq!(
            host.steps,
            [
                Step::did("command", "npm install"),
                Step::did("files", "Changed color.txt"),
                Step::said("Installed, wrote Blue, kept the lockfile.")
            ]
        );
        let heard = heard.lock().unwrap().join("\n");
        assert!(
            heard.contains(r#"POST /api/session {"location":{"directory":"#)
                && heard.contains(r#""model":{"providerID":"opencode","id":"mimo-free"}"#),
            "{heard}"
        );
        assert!(heard.contains(r#"{"action":"shell","resource":"*","effect":"ask"}"#), "{heard}");
        assert!(
            heard.contains(r#"permission/per-1/reply {"decision":"once"}"#)
                && heard.contains(r#"form/frm-1/reply {"answer":{"q0":"Blue"}}"#),
            "{heard}"
        );
        assert!(heard.contains(r#"prompt {"text":"keep the lockfile","delivery":"steer"}"#), "{heard}");
    }

    /// A fake Claude Code whose turn ends with a build (and a Monitor on it) still running in the background: it's kept
    /// running rather than closed, and when the work ends it carries on by itself, as Claude Code does, in a turn of
    /// its own. Like Claude Code, it goes on only while its stdin is open: closed at its first result, it just exits.
    #[test]
    fn claude_code_with_background_work_is_kept_running_until_it_carries_on() {
        let dir = crate::test_support::temp_dir("session-background");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/sh
read init
read message
( cat > /dev/null; touch "$0.closed" ) <&0 &
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"scripts/package.sh","run_in_background":true}}]},"parent_tool_use_id":null}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"Build the app"}]}'
echo '{"type":"system","subtype":"task_started","task_id":"m1","description":"Watch the build log","is_backgrounded":true,"task_type":"monitor"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"The build runs in the background.","session_id":"s-new"}'
sleep 1
[ -f "$0.closed" ] && exit 0
echo '{"type":"system","subtype":"task_notification","task_id":"m1","status":"completed","summary":"Monitor \"Watch the build log\" ended"}'
echo '{"type":"system","subtype":"task_notification","task_id":"b1","status":"completed","summary":"Background command \"Build the app\" completed (exit code 0)"}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"The build passed."}]},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Built and signed: dist/Filo.zip","session_id":"s-new"}'
wait
"#,
        );
        let spec = spec(Engine::Claude, fake, &dir);
        let mut host = Recorder::default();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref()),
            (None, Some("Built and signed: dist/Filo.zip")),
            "{end:?}"
        );
        assert_eq!(
            host.background,
            [(
                Some("The build runs in the background.".to_string()),
                vec!["Build the app".to_string(), "Watch the build log".to_string()]
            )]
        );
        assert_eq!(
            host.resumed,
            [Some(
                "Monitor \"Watch the build log\" ended\nBackground command \"Build the app\" completed (exit code 0)"
                    .to_string()
            )]
        );
        assert!(host.steps.contains(&Step::said("The build passed.")), "{:?}", host.steps);
    }

    /// A fake Claude Code whose turn ends with its report and only a `tail -f` it ran in the background left: after a
    /// while it's asked whether its work is done, answers DONE, and the watcher is stopped (`stop_task`); the round
    /// ends with the report it gave, not its answer.
    #[test]
    fn claude_code_done_but_for_a_watcher_is_asked_and_the_watcher_stopped() {
        let dir = crate::test_support::temp_dir("session-watcher");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/bash
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Monitor","input":{"command":"tail -n 0 -f proof/dev-restart.log | grep --line-buffered -E \"done|FAILED\"","description":"Watch the restart log"}}]},"parent_tool_use_id":null}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"local_bash","description":"Watch the restart log"}]}'
echo '{"type":"system","subtype":"task_started","task_id":"w1","tool_use_id":"t1","description":"Watch the restart log","is_backgrounded":true,"task_type":"local_bash"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Restarted and checked: all good.","session_id":"s-new"}'
read -r check
echo "$check" > "$0.check"
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"DONE"}]},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"DONE","session_id":"s-new"}'
read -r stop
echo "$stop" > "$0.stop"
echo '{"type":"system","subtype":"task_updated","task_id":"w1","patch":{"status":"killed"}}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
cat > /dev/null
"#,
        );
        let spec = spec(Engine::Claude, fake, &dir);
        let mut host = Recorder::default();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!(
            (end.error.as_deref(), end.final_text.as_deref()),
            (None, Some("Restarted and checked: all good.")),
            "{end:?}"
        );
        assert_eq!(
            host.background,
            [(Some("Restarted and checked: all good.".to_string()), vec!["Watch the restart log".to_string()])]
        );
        assert!(host.resumed.is_empty(), "answering isn't carrying on: {:?}", host.resumed);
        let check = std::fs::read_to_string(dir.join("claude.check")).unwrap();
        assert!(
            check.contains(r#"\"Watch the restart log\" still running"#) && check.contains("reply with just DONE"),
            "{check}"
        );
        let stop: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("claude.stop")).unwrap()).unwrap();
        assert_eq!(
            (stop["request"]["subtype"].as_str(), stop["request"]["task_id"].as_str()),
            (Some("stop_task"), Some("w1"))
        );
        assert!(!host.steps.contains(&Step::said("DONE")), "{:?}", host.steps);
        assert!(
            host.steps.contains(&Step::marker(
                "note",
                "Its work was done: stopped what it left watching (Watch the restart log)"
            )),
            "{:?}",
            host.steps
        );
    }

    /// A watcher next to real work: nothing is asked while the build runs; once only the watcher is left it's asked,
    /// and an answer that it still waits on what it watches keeps the round going until the watcher ends.
    #[test]
    fn claude_code_waiting_on_real_work_or_on_what_it_watches_stays_monitoring() {
        let dir = crate::test_support::temp_dir("session-watcher-waits");
        let fake = script(
            &dir,
            "claude",
            r#"#!/bin/bash
read init
read message
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"tail -f build.log","run_in_background":true}},{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo build","run_in_background":true}}]},"parent_tool_use_id":null}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"local_bash","description":"Watch the log"},{"task_id":"b1","task_type":"local_bash","description":"Build"}]}'
echo '{"type":"system","subtype":"task_started","task_id":"w1","tool_use_id":"t1","description":"Watch the log","is_backgrounded":true,"task_type":"local_bash"}'
echo '{"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"t2","description":"Build","is_backgrounded":true,"task_type":"local_bash"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Building; I'"'"'ll check when it ends.","session_id":"s-new"}'
if read -r -t 2.5 early; then echo "$early" > "$0.early"; fi
echo '{"type":"system","subtype":"task_notification","task_id":"b1","status":"completed","summary":"Background command \"Build\" completed (exit code 0)"}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"w1","task_type":"local_bash","description":"Watch the log"}]}'
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Built; the restart helper runs.","session_id":"s-new"}'
read -r check
echo "$check" > "$0.check"
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"Waiting for the helper to log done."}]},"parent_tool_use_id":null}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Waiting for the helper to log done.","session_id":"s-new"}'
if read -r -t 1.5 late; then echo "$late" > "$0.late"; fi
echo '{"type":"system","subtype":"task_notification","task_id":"w1","status":"completed","summary":"Background command \"Watch the log\" completed (exit code 0)"}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
echo '{"type":"system","subtype":"init","session_id":"s-new"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"All done: restarted.","session_id":"s-new"}'
cat > /dev/null
"#,
        );
        let spec = spec(Engine::Claude, fake, &dir);
        let mut host = Recorder::default();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("All done: restarted.")), "{end:?}");
        assert!(!dir.join("claude.early").exists(), "nothing is asked while the build runs");
        assert!(std::fs::read_to_string(dir.join("claude.check")).unwrap().contains("reply with just DONE"));
        assert!(!dir.join("claude.late").exists(), "not asked again at once, nor stopped");
        assert_eq!(
            host.background.iter().map(|(said, _)| said.as_deref().unwrap_or_default()).collect::<Vec<_>>(),
            ["Building; I'll check when it ends.", "Built; the restart helper runs."]
        );
        assert_eq!(
            host.resumed.len(),
            2,
            "after the build and after the watcher, not for its answer: {:?}",
            host.resumed
        );
        assert!(!host.steps.contains(&Step::said("Waiting for the helper to log done.")), "{:?}", host.steps);
    }

    /// A fake `codex app-server` whose turn ends with its command still running in a terminal: it's kept running, and
    /// once the command's item completes it gets a turn saying how the command ended.
    #[test]
    fn codex_with_a_command_still_running_gets_a_turn_when_it_ends() {
        let dir = crate::test_support::temp_dir("session-codex-background");
        let fake = script(
            &dir,
            "codex",
            r#"#!/bin/sh
read initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read initialized
read thread
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"th-1"}}}'
read turn
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"tu-1"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"item":{"type":"commandExecution","id":"exec-1","command":"/bin/zsh -lc '"'"'make release'"'"'","status":"inProgress","processId":"4242"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"agentMessage","text":"The release build runs in its terminal."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-1","status":"completed"}}}'
sleep 1
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"commandExecution","id":"exec-1","command":"/bin/zsh -lc '"'"'make release'"'"'","status":"completed","exitCode":0,"aggregatedOutput":"BUILD-OK"}}}'
read next
case "$next" in *'"method":"turn/start"'*BUILD-OK*) ;; *) echo "bad follow-up: $next" >&2; exit 3;; esac
echo '{"jsonrpc":"2.0","id":4,"result":{"turn":{"id":"tu-2"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"type":"agentMessage","text":"Release built."}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"tu-2","status":"completed"}}}'
read rest
"#,
        );
        let mut spec = spec(Engine::Codex, fake, &dir);
        spec.new_session = None;
        let mut host = Recorder::default();
        let end = run(&spec, &mut host, |_| {});
        assert_eq!((end.error.as_deref(), end.final_text.as_deref()), (None, Some("Release built.")), "{end:?}");
        assert_eq!(
            host.background,
            [(Some("The release build runs in its terminal.".to_string()), vec!["make release".to_string()])]
        );
        let why = host.resumed[0].clone().unwrap();
        assert!(why.contains("`make release` exited with code 0") && why.contains("BUILD-OK"), "{why}");
    }

    #[test]
    fn stop_ends_a_round_that_waits_on_an_answer() {
        let dir = crate::test_support::temp_dir("session-stop");
        let fake = script(
            &dir,
            "claude",
            "#!/bin/sh\nread init\nread message\necho '{\"type\":\"control_request\",\"request_id\":\"r\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"input\":{\"command\":\"rm -rf build\"},\"tool_use_id\":\"t\"}}'\nsleep 30\n",
        );
        let spec = spec(Engine::Claude, fake, &dir);
        // Stop comes from elsewhere (the Stop button) once the question is out; macOS may take a while to first run a
        // new script, so the stop waits for the question rather than a set time.
        let stopper: Arc<Mutex<Option<Stopper>>> = Arc::default();
        let slot = stopper.clone();
        let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = asked.clone();
        std::thread::spawn(move || {
            while !seen.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if let Some(stopper) = slot.lock().unwrap().as_ref() {
                stopper.stop();
            }
        });
        let mut host = Recorder { on_ask: Some(asked), ..Recorder::default() };
        let started = Instant::now();
        let end = run(&spec, &mut host, |stop| *stopper.lock().unwrap() = Some(stop));
        assert!(end.stopped && end.error.is_none(), "{end:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(host.asks.len(), 1);
    }

    /// The installed CLI doing a small task in a scratch folder in Supervised mode, so it has to ask: every ask is
    /// allowed. Uses the user's sign-in and usage, so it runs only when asked:
    /// `cargo test -p filo-local real_ -- --ignored --nocapture` (`FILO_REAL=claude`, `codex`, `muse` or `opencode` for one
    /// of them; `FILO_REAL_MODEL` for the model of the last three).
    fn real_round(engine: Engine) {
        let only = std::env::var("FILO_REAL").unwrap_or_default();
        if !only.is_empty() && Engine::parse(&only) != Some(engine) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("filo-real-session-{}-{}", engine.program(), uuid::Uuid::new_v4()));
        let proof = dir.join("proof");
        std::fs::create_dir_all(&proof).unwrap();
        let program = crate::engine::find_program(engine, None).expect("the CLI is installed");
        let mut spec = spec(engine, program, &dir);
        spec.mode = "supervised".into();
        spec.model = match engine {
            Engine::Claude => Some("haiku".into()),
            _ => std::env::var("FILO_REAL_MODEL").ok(),
        };
        spec.effort =
            std::env::var("FILO_REAL_EFFORT").ok().filter(|_| matches!(engine, Engine::Muse | Engine::OpenCode));
        spec.new_session = (engine == Engine::Claude).then(|| uuid::Uuid::new_v4().to_string());
        spec.message = "Create a file named hello.txt in the current folder containing exactly the word hi, using a shell command. Then reply with one short sentence.".into();
        spec.instructions = format!("You work in {}. Save nothing else.", dir.display());
        spec.timeout = Duration::from_secs(300);
        struct Allow(Recorder);
        impl Host for Allow {
            fn step(&mut self, step: Step) {
                println!("step: {step:?}");
                self.0.step(step)
            }
            fn session(&mut self, id: &str) {
                self.0.session(id)
            }
            fn ask(&mut self, ask: &Ask) -> String {
                println!("asks: {ask:?}");
                let key = self.0.ask(ask);
                self.0.answers.insert(key.clone(), "allow".into());
                key
            }
            fn answer(&mut self, key: &str) -> Option<String> {
                self.0.answer(key)
            }
            fn notes(&mut self) -> Vec<String> {
                Vec::new()
            }
            fn stop_requested(&mut self) -> bool {
                false
            }
        }
        let mut host = Allow(Recorder::default());
        let end = run(&spec, &mut host, |_| {});
        println!("{engine:?} ended: {end:?}\nasked: {:?}", host.0.asks);
        assert_eq!(end.error, None);
        assert!(!host.0.asks.is_empty(), "Supervised: it had to ask");
        assert!(end.session_id.is_some());
        assert_eq!(std::fs::read_to_string(dir.join("hello.txt")).unwrap_or_default().trim(), "hi");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore]
    fn real_claude_code_asks_and_carries_on() {
        real_round(Engine::Claude);
    }

    #[test]
    #[ignore]
    fn real_codex_asks_and_carries_on() {
        real_round(Engine::Codex);
    }

    /// The installed CLI asking the user a question mid-task (its own tool for it), answered "hello": the answer reaches
    /// it and it carries on. `cargo test -p filo-local real_question -- --ignored --nocapture` (`FILO_REAL`,
    /// `FILO_REAL_MODEL` and `FILO_REAL_EFFORT` as for `real_round`).
    fn real_question(engine: Engine) {
        let only = std::env::var("FILO_REAL").unwrap_or_default();
        if !only.is_empty() && Engine::parse(&only) != Some(engine) {
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("filo-real-question-{}-{}", engine.program(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("proof")).unwrap();
        let program = crate::engine::find_program(engine, None).expect("the CLI is installed");
        let mut spec = spec(engine, program, &dir);
        spec.mode = "full_access".into();
        spec.model = std::env::var("FILO_REAL_MODEL").ok();
        spec.effort = std::env::var("FILO_REAL_EFFORT").ok();
        spec.new_session = None;
        spec.message = "First ask me, with your tool for asking the user a question (offer the options hi and hello), which word to write. Then create word.txt in the current folder containing exactly the word I picked, and reply with one short sentence.".into();
        spec.instructions = format!("You work in {}. Save nothing else.", dir.display());
        spec.timeout = Duration::from_secs(300);
        struct Answer(Recorder);
        impl Host for Answer {
            fn step(&mut self, step: Step) {
                println!("step: {step:?}");
                self.0.step(step)
            }
            fn session(&mut self, id: &str) {
                self.0.session(id)
            }
            fn ask(&mut self, ask: &Ask) -> String {
                println!("asks: {ask:?}");
                let key = self.0.ask(ask);
                self.0
                    .answers
                    .insert(key.clone(), if ask.kind == "question" { "hello".into() } else { "allow".into() });
                key
            }
            fn answer(&mut self, key: &str) -> Option<String> {
                self.0.answer(key)
            }
            fn notes(&mut self) -> Vec<String> {
                Vec::new()
            }
            fn stop_requested(&mut self) -> bool {
                false
            }
        }
        let mut host = Answer(Recorder::default());
        let end = run(&spec, &mut host, |_| {});
        println!("{engine:?} ended: {end:?}");
        assert_eq!(end.error, None);
        assert!(host.0.asks.iter().any(|ask| ask.kind == "question"), "it asked a question");
        assert_eq!(std::fs::read_to_string(dir.join("word.txt")).unwrap_or_default().trim(), "hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore]
    fn real_question_muse_code() {
        real_question(Engine::Muse);
    }

    #[test]
    #[ignore]
    fn real_question_opencode() {
        real_question(Engine::OpenCode);
    }

    #[test]
    #[ignore]
    fn real_muse_code_asks_and_carries_on() {
        real_round(Engine::Muse);
    }

    #[test]
    #[ignore]
    fn real_opencode_asks_and_carries_on() {
        real_round(Engine::OpenCode);
    }

    /// The installed Claude Code ending its turn with a command still running in the background: kept running, it
    /// carries on by itself once the command ends. Uses the user's sign-in and usage:
    /// `cargo test -p filo-local real_claude_code_monitors -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_claude_code_monitors_its_background_command() {
        let dir = crate::test_support::temp_dir("real-background");
        std::fs::create_dir_all(dir.join("proof")).unwrap();
        let program = crate::engine::find_program(Engine::Claude, None).expect("Claude Code is installed");
        let mut spec = spec(Engine::Claude, program, &dir);
        spec.mode = "full_access".into();
        spec.new_session = Some(uuid::Uuid::new_v4().to_string());
        spec.message = "Run `sleep 15; echo BUILD-OK > built.txt` with the Bash tool and run_in_background set to true. Then end your turn at once with one short sentence, without waiting or checking on it. When you're told it finished, check built.txt and reply with its contents.".into();
        spec.timeout = Duration::from_secs(300);
        let mut host = Recorder::default();
        let end = run(&spec, &mut host, |_| {});
        println!("ended: {end:?}\nbackground: {:?}\nresumed: {:?}", host.background, host.resumed);
        assert_eq!(end.error, None);
        assert_eq!(host.background.len(), 1, "its turn ended once with the command still running");
        assert_eq!(host.resumed.len(), 1, "it carried on once the command ended");
        assert!(end.final_text.unwrap_or_default().contains("BUILD-OK"));
    }

    /// The installed Claude Code ending its turn with its report and a `tail -f` it left in the background: asked
    /// whether its work is done, it says DONE, the watcher is stopped (its process too) and the round ends with the
    /// report. Uses the user's sign-in and usage:
    /// `cargo test -p filo-local real_claude_code_left_watching -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_claude_code_left_watching_a_log_ends_its_round() {
        let dir = crate::test_support::temp_dir("real-watcher");
        std::fs::create_dir_all(dir.join("proof")).unwrap();
        let log = dir.join("helper.log");
        std::fs::write(&log, "").unwrap();
        let program = crate::engine::find_program(Engine::Claude, None).expect("Claude Code is installed");
        let mut spec = spec(Engine::Claude, program, &dir);
        spec.mode = "full_access".into();
        spec.new_session = Some(uuid::Uuid::new_v4().to_string());
        spec.message = format!(
            "Run `tail -n 0 -f {}` with the Bash tool and run_in_background set to true, to watch it while you work. Then write OK to result.txt, and end your turn with a one-line report saying result.txt is written.",
            log.display()
        );
        spec.timeout = Duration::from_secs(120);
        let mut host = Recorder::default();
        let end = run(&spec, &mut host, |_| {});
        println!(
            "ended: {end:?}\nbackground: {:?}\nresumed: {:?}\nsteps: {:?}",
            host.background, host.resumed, host.steps
        );
        assert_eq!(end.error, None);
        assert_eq!(host.background.len(), 1, "its turn ended once with the watcher left");
        assert!(end.final_text.unwrap_or_default().contains("result.txt"));
        let left = std::process::Command::new("pgrep")
            .args(["-f", &format!("tail -n 0 -f {}", log.display())])
            .output()
            .unwrap();
        assert!(left.stdout.is_empty(), "the watcher's tail still runs");
    }

    #[test]
    fn a_cli_that_dies_early_is_a_failure_with_its_words() {
        let dir = crate::test_support::temp_dir("session-dies");
        let fake = script(&dir, "claude", "#!/bin/sh\necho 'Not logged in. Please run /login' >&2\nexit 1\n");
        let end = run(&spec(Engine::Claude, fake, &dir), &mut Recorder::default(), |_| {});
        let error = end.error.unwrap();
        assert!(
            error.contains("before finishing")
                && error.contains("Please run /login")
                && error.contains(&Engine::Claude.sign_in_hint()),
            "{error}"
        );
        let missing = run(&spec(Engine::Codex, dir.join("nope"), &dir), &mut Recorder::default(), |_| {});
        assert!(missing.error.unwrap().starts_with("Couldn't start Codex"));
    }
}

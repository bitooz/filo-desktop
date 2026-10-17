// Runs one turn of the user's own coding tool (Claude Code, Codex, Muse Code or OpenCode) with Filo's MCP server
// plugged in.
//
// Claude Code: `claude -p --output-format stream-json --verbose --mcp-config <json> --strict-mcp-config
//   --tools Read,Grep,Glob,WebSearch,WebFetch,ToolSearch,Skill,Write,Edit,Bash --allowedTools <those and
//   mcp__filo__<each of Filo's tools>> --permission-mode dontAsk --append-system-prompt <instructions>
//   (--session-id <new> | --resume <id>)`, the prompt on stdin. Of its built-in tools it gets web search, reading and
//   writing files (Write and Edit build the pages it publishes as artifacts, file first), tool search, the user's
//   skills and the shell, all allowed without asking, and nobody is there to approve anything else (`dontAsk` refuses
//   it, whatever the user's settings say). That's with Claude Code in Full access on the Coding page (the turn's
//   `mode`); in Auto the shell isn't allowed outright but goes to `--permission-mode auto`, whose reviewer runs
//   routine commands and refuses the rest, and the file editors are allowed in the working folder only; reading is
//   allowed there and in Filo's own folders (the chats, the files sent, the pages), and any other file or web page
//   fetched goes to the reviewer (`TurnSpec::readable`). Only the MCP
//   servers Filo names load: Filo's, the computer's (`use_computer`, computer.rs, while "Your computer" is on) and the user's apps that are on for this
//   agent. Their tools load through tool search when needed, except the ones Filo pins (mcp.rs `PINNED`).
//   Stdin is `--input-format stream-json`: first the SDK's `initialize` request, whose `skills` allow-list leaves the
//   turn only Filo's skills (the workspace's `.claude/skills`, store.rs) and `skills::BUILT_IN`, then the prompt as one
//   JSON user message, with the pictures (and PDFs) the user sent as content blocks.
// Codex: `codex app-server -c sandbox_mode="danger-full-access" -c approval_policy="never" -c web_search="live"
//   -c mcp_servers.filo_core.… -c mcp_servers.filo.… -c features.code_mode.direct_only_tool_namespaces=["mcp__filo_core"]
//   -c features.<…>=false …`, then over its JSON-RPC (coding_session.rs) a thread started in the workspace or resumed,
//   and a turn with the prompt (with the instructions on a new session) and each picture as a local image. With Codex in
//   Full access its shell runs anything, with network; in Auto it's sandboxed to the workspace without network and its
//   reviewer approves routine requests to go further (`sandbox_mode="workspace-write"`, `approval_policy="on-request"`,
//   `approvals_reviewer="auto_review"`). Its live web search is on. Codex keeps MCP tools out of the prompt, inside its
//   code mode's `exec` (`ALL_TOOLS`), and has no per-tool pin, so Filo's server runs twice: `filo_core` with the pinned
//   tools, which Codex gives the model directly, and `filo` with the rest. The app-server reads the user's own
//   `config.toml` (it has no `--ignore-user-config`), so the MCP servers and the `notify` program it sets are turned off
//   (`codex_user_config_off`, however the file names them), and so are ChatGPT's connectors and plugins
//   (`CODEX_FEATURES_OFF`): no MCP server joins a chat past the Apps page's per-agent switches. A server of the chat's
//   own that the file names too goes by another name (`codex_own_name`), as Codex would merge the two. The model and reasoning effort it sets still apply. Codex has no
//   skills allow-list, so its own skills are turned off and every other skill folder's are named off
//   (`codex_skill_args`), leaving Filo's (the workspace's `.agents/skills`, store.rs) and the four document plugins of
//   its primary runtime, which hold only skills.
//
// A chat turn runs as a session the CLI is kept talking to (`start_turn`, coding_session.rs), so a message the user
// sends while it works reaches it at its next step (`Steer`, turn.rs `Steering`): one more user message on Claude Code's
// stdin, `turn/steer` on Codex's app-server and Muse Code's, a prompt delivered as `steer` on OpenCode's server
// (coding_session.rs `Notes`, the same for each). A turn that keeps refusing the steer is interrupted, and goes on in a
// new turn with the message.
//
// A chat turn ends once the agent has replied, whatever still runs in its shell: Claude Code's stdin closes at its
// result, and Codex's app-server at the end of its turn. Work a CLI runs in the background ends with it, so Claude
// Code's background tasks are off in a chat (`NO_BACKGROUND_TASKS`), and the instructions say to start what should keep
// running after the reply (a dev server) detached, with a log (`capabilities`): nothing of Filo's stops it once the turn
// is over. Waiting on background work is for coding tasks (coding_session.rs).
//
// A chat turn takes as long as its work does: it's stopped only once nothing at all has come from the CLI for
// `turn::HUNG_AFTER`, when it has hung. A page shared to claude.ai gets a time in all (`start_invocation`).
//
// Each CLI runs in its own process group in the store's workspace folder, so Stop ends it and the MCP server it
// started. Their JSON events give the session to resume next time and how the turn ended; the reply itself arrives
// through `send_message`, not their output.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mcp;
use crate::pages::Pages;

/// A coding CLI Filo runs: it answers chats (Filo, the user's agents, the group, routines) and takes coding tasks.
/// A chat's turn runs each as a session (`start_turn`, coding_session.rs); Claude Code and Codex take messages sent while
/// it works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Claude,
    Codex,
    Muse,
    OpenCode,
}

impl Engine {
    /// Every engine, in the order pickers list them (the Coding page's too).
    pub const ALL: [Engine; 4] = [Engine::Claude, Engine::Codex, Engine::Muse, Engine::OpenCode];

    pub fn label(self) -> &'static str {
        match self {
            Engine::Claude => "Claude Code",
            Engine::Codex => "Codex",
            Engine::Muse => "Muse Code",
            Engine::OpenCode => "OpenCode",
        }
    }

    pub fn program(self) -> &'static str {
        match self {
            Engine::Claude => "claude",
            Engine::Codex => "codex",
            Engine::Muse => "muse",
            Engine::OpenCode => "opencode",
        }
    }

    pub fn parse(value: &str) -> Option<Engine> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude code" => Some(Engine::Claude),
            "codex" => Some(Engine::Codex),
            "muse" | "muse-code" | "muse code" => Some(Engine::Muse),
            "opencode" | "open-code" | "open code" => Some(Engine::OpenCode),
            _ => None,
        }
    }

    /// Claude Code and Codex: a chat turn runs their own command line (`invocation`), whose JSON lines the chat reads as
    /// they come, and they see the message's pictures themselves and keep their own models. Muse Code and OpenCode get
    /// the turn as a session of Filo's making (coding_session.rs), whose steps come as lines of Filo's own.
    pub fn runs_invocation(self) -> bool {
        matches!(self, Engine::Claude | Engine::Codex)
    }

    /// How Filo's data names it (`"claude"`): models.rs's and apps.rs's columns, the problem report's checks.
    pub fn key(self) -> &'static str {
        match self {
            Engine::Claude => "claude",
            Engine::Codex => "codex",
            Engine::Muse => "muse",
            Engine::OpenCode => "opencode",
        }
    }

    /// What follows the program to sign in from a terminal (`claude auth login`): the welcome's Sign in, the problem
    /// report's fix, the Coding page's advice and `sign_in_hint`.
    pub fn sign_in_args(self) -> &'static str {
        match self {
            Engine::Claude | Engine::OpenCode => "auth login",
            Engine::Codex | Engine::Muse => "login",
        }
    }

    /// The whole command that signs it in (`codex login`).
    pub fn sign_in_command(self) -> String {
        format!("{} {}", self.program(), self.sign_in_args())
    }

    /// What to run in a terminal when the CLI says it isn't signed in.
    pub fn sign_in_hint(self) -> String {
        match self {
            Engine::OpenCode => {
                format!("Run `{}` in Terminal to add a provider, then try again.", self.sign_in_command())
            }
            _ => format!("Run `{}` in Terminal, then try again.", self.sign_in_command()),
        }
    }

    /// Whether a CLI's error or output says it isn't signed in, in any of the four tools' words.
    pub fn signed_out(text: &str) -> bool {
        let text = text.to_lowercase();
        [
            "not logged in",
            "please run /login",
            "invalid api key",
            "login required",
            "not signed in",
            "401 unauthorized",
            "authentication_error",
            "codex login",
            "muse login",
            "opencode auth login",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    }
}

/// A model as a CLI names it: what `--model` / `-m` takes, and how it reads ("Opus 5.5").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub label: String,
}

/// A row of the composer's model picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub engine: Engine,
    /// What a pick saves and the CLI gets.
    pub id: String,
    pub label: String,
    /// The model the CLI answers with when it isn't given one, as the CLI itself reports it.
    pub default: bool,
    /// Other names the CLI takes for the same model (Claude Code's alias and the full name it resolves to).
    pub aliases: Vec<String>,
}

impl ModelOption {
    pub fn matches(&self, id: &str) -> bool {
        self.id == id || self.aliases.iter().any(|alias| alias == id)
    }
}

/// Claude Code's aliases for the latest model of each family (`claude --model opus`), so the list doesn't go stale, with
/// how each reads until Claude Code says which model it is.
pub const CLAUDE_MODELS: [(&str, &str); 4] =
    [("fable", "Fable"), ("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")];

/// A coding tool's command, as every run of one starts: in `cwd`, stdin and stdout piped, in a process group of its own
/// (so stopping it reaches what it started), without the variables that make Claude Code think it's nested inside
/// another session (started from a terminal or a check), and on `path_env` when given. It doesn't use Filo's macOS
/// permissions (disclaim.rs) unless `uses_computer` (a turn with "Your computer" on). The caller adds the arguments, the
/// environment and where stderr goes.
pub(crate) fn cli_command(program: &Path, cwd: &Path, path_env: Option<&str>, uses_computer: bool) -> Command {
    let mut command = crate::disclaim::command(program, uses_computer);
    command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .process_group(0)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("CLAUDE_CODE_SSE_PORT");
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    command
}

/// The last `keep` lines a CLI prints on stderr, read on a thread of its own as they come.
pub(crate) fn stderr_tail(stderr: Option<std::process::ChildStderr>, keep: usize) -> Arc<Mutex<Vec<String>>> {
    let tail = Arc::new(Mutex::new(Vec::new()));
    if let Some(stderr) = stderr {
        let tail = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut tail = tail.lock().unwrap();
                tail.push(line);
                let excess = tail.len().saturating_sub(keep);
                tail.drain(..excess);
            }
        });
    }
    tail
}

/// How a run with a deadline went (`run_for`): its exit status, None when the deadline killed it, and all it printed.
pub struct Ran {
    pub status: Option<std::process::ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs `command` for its answer, `stdin` written to it, for `timeout` at most: in a process group of its own, so the
/// deadline kills everything it started, with both pipes read as they fill (a chatty program can't block on a full
/// pipe). The caller sets the arguments and environment; an error only when it can't start. The app's checks run
/// through it too (src/command.rs).
pub fn run_for(command: &mut Command, stdin: Option<&str>, timeout: Duration) -> std::io::Result<Ran> {
    let mut child = command
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = reader(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = reader(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(30)),
            _ => {
                // SAFETY: the child's own process group.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break None;
            }
        }
    };
    Ok(Ran { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

/// One JSON message on a line of a CLI's stdin. A pipe that broke (the CLI ended) is dropped, and nothing more is tried.
pub(crate) fn write_line(stdin: &mut Option<std::process::ChildStdin>, message: &Value) {
    if let Some(pipe) = stdin.as_mut() {
        let mut line = message.to_string();
        line.push('\n');
        if pipe.write_all(line.as_bytes()).and_then(|_| pipe.flush()).is_err() {
            *stdin = None;
        }
    }
}

/// One question to a JSON-RPC server a CLI starts on stdio (`muse serve`, `codex app-server`): `initialize` with
/// `initialize`'s params, `initialized`, then `method` with `params`; its result, or null when it errs, ends or takes
/// longer than `timeout`. The caller sets the arguments and environment; the server is stopped after. It runs in the
/// temporary folder: a tool started disclaimed (disclaim.rs) in a folder macOS guards (the app's own, in a checkout on
/// the Desktop) has macOS ask the user for that folder, and waits on the answer.
pub(crate) fn ask_once(
    command: &mut Command,
    initialize: Value,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Value {
    let Ok(mut child) = command
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return Value::Null;
    };
    let watchdog = Stopper::for_group(child.id() as i32);
    let done = Arc::new(AtomicBool::new(false));
    {
        let (watchdog, done) = (watchdog.clone(), done.clone());
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            if !done.load(Ordering::SeqCst) {
                watchdog.stop();
            }
        });
    }
    let mut answer = Value::Null;
    let mut stdin = child.stdin.take();
    if let Some(stdout) = child.stdout.take() {
        write_line(&mut stdin, &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize }));
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            match message["id"].as_i64() {
                Some(1) => {
                    write_line(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "initialized" }));
                    write_line(&mut stdin, &json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params }));
                }
                Some(2) => {
                    answer = message["result"].clone();
                    break;
                }
                _ => {}
            }
        }
    }
    done.store(true, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    answer
}

/// A CLI's stdin as Filo writes to it: JSON lines (`write`), and for a JSON-RPC peer (Codex's app-server, Muse Code)
/// requests numbered in order, each with what it was for until its answer comes (`answered`).
pub(crate) struct Pipe<P> {
    stdin: Option<std::process::ChildStdin>,
    next_id: i64,
    sent: std::collections::HashMap<i64, P>,
}

impl<P> Pipe<P> {
    pub(crate) fn new(stdin: Option<std::process::ChildStdin>) -> Self {
        Pipe { stdin, next_id: 1, sent: std::collections::HashMap::new() }
    }

    pub(crate) fn write(&mut self, message: &Value) {
        write_line(&mut self.stdin, message);
    }

    /// Sends request `method`; its id.
    pub(crate) fn request(&mut self, method: &str, params: Value, purpose: P) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.insert(id, purpose);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    pub(crate) fn notify(&mut self, method: &str) {
        self.write(&json!({ "jsonrpc": "2.0", "method": method }));
    }

    /// The answer to the peer's request `id`.
    pub(crate) fn reply(&mut self, id: &Value, result: Value) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// A request of the peer's Filo doesn't handle (`method`).
    pub(crate) fn refuse(&mut self, id: &Value, method: &str) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Filo can't answer {method}") } }));
    }

    /// What request `id` was for (once), and why it failed when `message`, its answer, says it did.
    pub(crate) fn answered(&mut self, id: &Value, message: &Value) -> Option<(P, Option<String>)> {
        let purpose = id.as_i64().and_then(|id| self.sent.remove(&id))?;
        let failed = message["error"]["message"]
            .as_str()
            .map(str::to_string)
            .or_else(|| message.get("error").filter(|error| !error.is_null()).map(|error| error.to_string()));
        Some((purpose, failed))
    }

    pub(crate) fn is_open(&self) -> bool {
        self.stdin.is_some()
    }

    /// Closing its stdin ends the CLI.
    pub(crate) fn close(&mut self) {
        self.stdin = None;
    }
}

/// Each line a CLI prints on stdout, read on a thread of its own until it closes.
pub(crate) fn stdout_lines(stdout: Option<std::process::ChildStdout>) -> std::sync::mpsc::Receiver<String> {
    let (lines, incoming) = std::sync::mpsc::channel();
    if let Some(stdout) = stdout {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
    }
    incoming
}

/// A CLI Filo keeps talking to over its stdin and stdout until it's done (a coding round: coding_session.rs; a
/// compaction: compaction.rs), as `Started::drive` runs it.
pub(crate) trait Conversation {
    /// A line it printed; whether it counts as a sign of life (the deadline starts again).
    fn line(&mut self, line: &str) -> bool;
    /// Every `LOOK_EVERY` while it runs, until it's finished; whether it's alive (the deadline starts again).
    fn look(&mut self) -> bool {
        false
    }
    /// It's done: its stdin closes, and one that lingers past `EXIT_GRACE` is ended.
    fn finished(&self) -> bool;
    fn close(&mut self);
}

/// How often a driven CLI's conversation is looked at (and its deadline).
const LOOK_EVERY: Duration = Duration::from_millis(250);
/// Once its conversation is finished, how long a CLI gets to exit on its own after its stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// A CLI started in a process group of its own (`cli_command`): its stdin for the conversation to take, what stops it
/// (Stop reaches everything it started), its lines as they come and the end of its stderr.
pub(crate) struct Started {
    child: Child,
    pub stdin: Option<std::process::ChildStdin>,
    pub stopper: Stopper,
    lines: std::sync::mpsc::Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

/// How a driven CLI ended: its exit status, whether its deadline passed (it was stopped then), and the last lines of
/// its stderr.
pub(crate) struct Drove {
    pub status: Option<std::process::ExitStatus>,
    pub timed_out: bool,
    pub stderr: Vec<String>,
}

/// Starts `engine`'s CLI at `program` with `args` and `env`, keeping the last `keep_stderr` lines of its stderr.
pub(crate) fn spawn_cli(
    engine: Engine,
    program: &Path,
    cwd: &Path,
    path_env: Option<&str>,
    uses_computer: bool,
    args: &[String],
    env: Vec<(String, String)>,
    keep_stderr: usize,
) -> Result<Started, String> {
    let mut child = cli_command(program, cwd, path_env, uses_computer)
        .args(args)
        .stderr(Stdio::piped())
        .envs(env)
        .spawn()
        .map_err(|error| format!("Couldn't start {} ({}): {error}", engine.label(), program.display()))?;
    let stopper = Stopper::for_group(child.id() as i32);
    let (lines, stderr) = (stdout_lines(child.stdout.take()), stderr_tail(child.stderr.take(), keep_stderr));
    Ok(Started { stdin: child.stdin.take(), child, stopper, lines, stderr })
}

impl Started {
    /// Gives `conversation` each line until the CLI exits. It's stopped once `timeout` goes by without a sign of life
    /// (from the start, for a conversation that gives none).
    pub(crate) fn drive(mut self, conversation: &mut dyn Conversation, timeout: Duration) -> Drove {
        let mut deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let mut looked = Instant::now() - LOOK_EVERY;
        let mut closed_at: Option<Instant> = None;
        loop {
            match self.lines.recv_timeout(LOOK_EVERY) {
                Ok(line) => {
                    if conversation.line(line.trim_end()) {
                        deadline = Instant::now() + timeout;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if looked.elapsed() >= LOOK_EVERY {
                looked = Instant::now();
                if !conversation.finished() && conversation.look() {
                    deadline = Instant::now() + timeout;
                }
                if !timed_out && Instant::now() > deadline {
                    timed_out = true;
                    self.stopper.stop();
                }
            }
            // Done: closing stdin ends the CLI; one that lingers is ended.
            if conversation.finished() && closed_at.is_none() {
                conversation.close();
                closed_at = Some(Instant::now());
            }
            if closed_at.is_some_and(|at| at.elapsed() > EXIT_GRACE) {
                // SAFETY: a signal to the CLI's own process group.
                unsafe {
                    libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                }
                closed_at = Some(Instant::now() + Duration::from_secs(3600));
            }
        }
        conversation.close();
        let status = self.child.wait().ok();
        self.stopper.exited();
        // The stderr thread reads what's left.
        std::thread::sleep(Duration::from_millis(50));
        let stderr = self.stderr.lock().unwrap().clone();
        Drove { status, timed_out, stderr }
    }
}

/// What Claude Code answers with for `model` (none: its own default, from the user's settings or their plan): `/model`
/// in print mode, which Claude Code answers itself without calling a model ("Current model: `Opus 5.5 (default)`"),
/// after an init event naming the model in full. Blocks for a couple of seconds; None when it can't tell.
pub fn claude_model(program: &Path, workspace: &Path, path_env: Option<&str>, model: Option<&str>) -> Option<Model> {
    let mut args: Vec<&str> = vec![
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--tools",
        "",
    ];
    if let Some(model) = model {
        args.extend(["--model", model]);
    }
    let mut child = cli_command(program, workspace, path_env, false).args(&args).stderr(Stdio::null()).spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"/model");
    }
    let pid = child.id() as i32;
    let done = Arc::new(AtomicBool::new(false));
    {
        let done = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(30));
            if !done.load(Ordering::SeqCst) {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        });
    }
    let (mut id, mut label) = (None, None);
    for line in BufReader::new(child.stdout.take()?).lines().map_while(Result::ok) {
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        match (event.get("type").and_then(Value::as_str), event.get("subtype").and_then(Value::as_str)) {
            (Some("system"), Some("init")) => {
                id = event.get("model").and_then(Value::as_str).filter(|id| valid_model(id)).map(str::to_string)
            }
            (Some("result"), _) => {
                label = event.get("result").and_then(Value::as_str).and_then(current_model_label);
                break;
            }
            _ => {}
        }
    }
    done.store(true, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    let id = id?;
    Some(Model { label: label.unwrap_or_else(|| id.clone()), id })
}

/// "Opus 5.5" from Claude Code's answer to `/model` ("Current model: `Opus 5.5 (default)` (effort: high)").
fn current_model_label(answer: &str) -> Option<String> {
    let rest = &answer[answer.find("Current model:")? + "Current model:".len()..];
    let name = rest.split('`').nth(1)?.trim();
    let name = name.strip_suffix("(default)").unwrap_or(name).trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Claude Code's rows: its aliases, read as the models they are (`resolved`, alias by alias), with its default in
/// place of the alias that names the same model (or first, when none does). Without what Claude Code said, the
/// aliases under their families' names and no default.
pub fn claude_options(default: Option<&Model>, resolved: &[(String, Model)]) -> Vec<ModelOption> {
    let mut options: Vec<ModelOption> = Vec::new();
    let mut default_row: Option<usize> = None;
    for (alias, family) in CLAUDE_MODELS {
        let model = resolved.iter().find(|(name, _)| name == alias).map(|(_, model)| model);
        if let (Some(default), Some(model)) = (default, model)
            && model.id == default.id
        {
            match default_row {
                Some(row) => options[row].aliases.push(alias.to_string()),
                None => {
                    default_row = Some(options.len());
                    options.push(ModelOption {
                        engine: Engine::Claude,
                        id: default.id.clone(),
                        label: default.label.clone(),
                        default: true,
                        aliases: vec![alias.to_string()],
                    });
                }
            }
            continue;
        }
        options.push(ModelOption {
            engine: Engine::Claude,
            id: alias.to_string(),
            label: model.map_or_else(|| family.to_string(), |model| model.label.clone()),
            default: false,
            aliases: model.map(|model| vec![model.id.clone()]).unwrap_or_default(),
        });
    }
    if let (Some(default), None) = (default, default_row) {
        options.insert(
            0,
            ModelOption {
                engine: Engine::Claude,
                id: default.id.clone(),
                label: default.label.clone(),
                default: true,
                aliases: Vec::new(),
            },
        );
    }
    options
}

pub(crate) fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".codex"))
}

/// Codex's rows: the models its own picker lists, from the catalogue it keeps (`$CODEX_HOME/models_cache.json`), in its
/// order, and its default, the `model` its `config.toml` sets (none when it sets none). A default it doesn't list goes
/// first. Reads two small files.
pub fn codex_options() -> Vec<ModelOption> {
    let home = codex_home();
    let catalogue = std::fs::read(home.join("models_cache.json")).unwrap_or_default();
    let default = std::fs::read_to_string(home.join("config.toml")).ok().and_then(|config| codex_config_model(&config));
    codex_options_from(&catalogue, default.as_deref())
}

fn codex_options_from(catalogue: &[u8], default: Option<&str>) -> Vec<ModelOption> {
    let option = |model: Model| ModelOption {
        engine: Engine::Codex,
        default: Some(model.id.as_str()) == default,
        id: model.id,
        label: model.label,
        aliases: Vec::new(),
    };
    let mut options: Vec<ModelOption> = codex_models(catalogue, true).into_iter().map(option).collect();
    if let Some(default) =
        default.filter(|default| valid_model(default) && !options.iter().any(|option| option.id == *default))
    {
        let model = codex_models(catalogue, false)
            .into_iter()
            .find(|model| model.id == default)
            .unwrap_or_else(|| Model { id: default.into(), label: default.into() });
        options.insert(0, option(model));
    }
    options
}

/// Codex's model catalogue, by its `priority`: the models it lists (`visibility: "list"`), or all of them.
fn codex_models(bytes: &[u8], listed_only: bool) -> Vec<Model> {
    let Ok(catalogue) = serde_json::from_slice::<Value>(bytes) else { return Vec::new() };
    let mut models: Vec<(i64, Model)> = catalogue
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| !listed_only || model.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|model| {
            let id = model.get("slug").and_then(Value::as_str).filter(|id| valid_model(id))?;
            let label = model
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .unwrap_or(id);
            Some((
                model.get("priority").and_then(Value::as_i64).unwrap_or(i64::MAX),
                Model { id: id.into(), label: label.into() },
            ))
        })
        .collect();
    models.sort_by_key(|(priority, _)| *priority);
    models.into_iter().map(|(_, model)| model).collect()
}

/// The model Codex's `config.toml` sets: the `model` of the `profile` it picks, else its own `model`.
fn codex_config_model(config: &str) -> Option<String> {
    codex_config_value(config, "model")
}

/// The MCP servers the user's Codex `config.toml` sets up (`mcp_servers`, however it's written: tables, inline tables,
/// dotted keys, quoted names), by name. None when it can't be read (nor can Codex).
fn codex_user_servers(config: &str) -> Vec<String> {
    let Ok(config) = config.parse::<toml::Table>() else { return Vec::new() };
    config
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

/// What of the user's Codex `config.toml` a chat turn turns off, as `-c` options: the MCP servers it sets up
/// (`servers`, `codex_user_servers`), off in one table Codex merges over the file's, as a name may hold what a dotted
/// key can't; and the program it runs at the end of each turn (`notify`). The app-server reads the file, which `codex
/// exec` could leave out.
fn codex_user_config_off(servers: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    if !servers.is_empty() {
        let off: Vec<String> =
            servers.iter().map(|name| format!("{} = {{ enabled = false }}", toml_string(name))).collect();
        args.extend(["-c".into(), format!("mcp_servers={{ {} }}", off.join(", "))]);
    }
    args.extend(["-c".into(), "notify=[]".into()]);
    args
}

/// The name one of a chat turn's own Codex servers goes by: `name`, unless the user's `config.toml` has a server of that
/// name (`taken`), which Codex would merge with it (its settings over Filo's, off with the rest): then `name_2`, `name_3`…
fn codex_own_name(name: &str, taken: &[String]) -> String {
    std::iter::once(name.to_string())
        .chain((2..).map(|n| format!("{name}_{n}")))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| name.to_string())
}

/// The `key` Codex's `config.toml` sets: the profile's it picks, else its own. Only what that takes: `key = "string"`
/// lines, top level or under `[profiles.<name>]`.
fn codex_config_value(config: &str, wanted: &str) -> Option<String> {
    let mut table = String::new();
    let (mut model, mut profile) = (None, None);
    let mut profile_models: Vec<(String, String)> = Vec::new();
    for line in config.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            table = header.split(']').next().unwrap_or_default().trim().replace('"', "");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        let Some(quote) = value.chars().next().filter(|quote| *quote == '"' || *quote == '\'') else { continue };
        let Some(value) = value[1..].split(quote).next() else { continue };
        match (table.as_str(), key.trim()) {
            ("", "profile") => profile = Some(value.to_string()),
            ("", key) if key == wanted => model = Some(value.to_string()),
            (table, key) if key == wanted => {
                if let Some(name) = table.strip_prefix("profiles.") {
                    profile_models.push((name.to_string(), value.to_string()));
                }
            }
            _ => {}
        }
    }
    let from_profile = profile
        .and_then(|profile| profile_models.into_iter().find(|(name, _)| *name == profile).map(|(_, model)| model));
    from_profile.or(model).filter(|model| !model.trim().is_empty())
}

/// A model name a CLI could take: letters, digits and `.-_:/[]@`, not starting with a dash (it would read as a flag).
pub fn valid_model(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('-')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:/[]@".contains(c))
}

/// What Filo tells the CLI about the chat. Claude Code gets it as a system prompt, Codex at the top of a new session.
pub const INSTRUCTIONS: &str = "You are Filo, the user's assistant, chatting with them in the Filo desktop app on their Mac. \
The user only sees what you send with Filo's `send_message` tool (from Filo's MCP server); your plain text output is never shown to them. \
Answer every message by calling `send_message` with your reply in Markdown, usually once per turn. \
Be warm, direct and concise. If a request needs a tool you don't have, say so in your message.";

/// Codex keeps the instructions in its session; later turns get this reminder in front of the message.
const CODEX_REMINDER: &str = "(Reply with Filo's send_message tool.)";

/// Claude Code's own tools a chat turn gets: web search, reading files, tool search (for the tools that load when
/// needed), skills (Filo's and `skills::BUILT_IN`), writing and editing files (for the pages it builds and publishes
/// as artifacts; coding work goes to a coding task) and the shell. All but the shell and the file editors are allowed
/// outright; those are too in Full access, and otherwise the editors are allowed in the working folder (`invocation`).
const CLAUDE_NATIVE_TOOLS: &str = "Read,Grep,Glob,WebSearch,WebFetch,ToolSearch,Skill,Write,Edit,Bash";

/// Codex features on by default that would give a chat turn tools past Filo's: ChatGPT's connectors (`codex_apps`)
/// and plugins (their own MCP servers) outside the Apps page's switches, the browser and the computer, image making,
/// subagents and installing tools. Off as `-c features.<name>=false`, which a Codex without one of them ignores
/// (`--disable` would stop it).
const CODEX_FEATURES_OFF: [&str; 11] = [
    "apps",
    "plugins",
    "remote_plugin",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "image_generation",
    "multi_agent",
    "skill_mcp_dependency_install",
    "tool_suggest",
];

/// What a chat turn can use, as `invocation` gives it to `engine` with the user's apps in `servers`, what `computer`
/// says of `use_computer` (computer.rs `capability`) and the optional pages that are on (`mcp::pages`): for the end of
/// its instructions, so they promise no more and no less than the turn has.
fn capabilities(engine: Engine, mode: &str, servers: &[McpServer], computer: Option<&str>, pages: Pages) -> String {
    let full = mode == "full_access";
    // Claude Code's file editors work in its working folder unless it has Full access (`invocation`).
    let claude_native = format!(
        "web search and opening web pages (WebSearch, WebFetch); reading files on this Mac (Read, Grep, Glob) and writing them{} (Write, Edit); skills (Skill): the user's own from Filo and ones for PDFs, Word, Excel, PowerPoint, charts, making a desktop pet (desktop-pet) and making artifacts (artifacts)",
        if full { "" } else { " in your working folder and the user's skills folder" }
    );
    let (filo, native, shell, on_demand) = match engine {
        Engine::Claude => (
            "Filo's tools (the `filo` MCP server)",
            claude_native.as_str(),
            if full {
                "a shell (Bash) that runs any command, with network"
            } else {
                "a shell (Bash), with network, whose commands a reviewer approves: routine ones run, risky ones are refused, and nobody can approve them while you work"
            },
            "load when you search for them with tool search",
        ),
        Engine::Muse | Engine::OpenCode => (
            "Filo's tools (the `filo` MCP server)",
            "web search and opening web pages; reading and searching files on this Mac; skills: the user's own from Filo and ones for charts (dataviz), making a desktop pet (desktop-pet) and making artifacts (artifacts)",
            match (full, engine) {
                (true, _) => "a shell that runs any command on this Mac, with network",
                (false, Engine::Muse) => {
                    "a shell on this Mac whose commands Muse Code's reviewer approves: routine ones run, risky ones are refused, and nobody can approve them while you work"
                }
                _ => {
                    "a shell that runs commands in your working folder and the user's skills folder; going outside them is refused, since nobody can approve it while you work"
                }
            },
            "are there among your tools",
        ),
        Engine::Codex => (
            "Filo's tools (the `filo_core` and `filo` MCP servers)",
            "web search; skills: the user's own from Filo and the ones listed for documents, charts, making a desktop pet (desktop-pet) and making artifacts (artifacts)",
            if full {
                "a shell that runs any command on this Mac, with network"
            } else {
                "a shell on this Mac, sandboxed to your working folder without network: ask to go beyond it and a reviewer approves routine requests and refuses risky ones, since nobody can approve them while you work"
            },
            "are there when you look for them among your tools",
        ),
    };
    let native = format!("{native}; and {shell}");
    let apps = if servers.is_empty() {
        "No other apps are connected for you here.".to_string()
    } else {
        format!(
            "The user's apps connected for you here (MCP servers): {}.",
            servers.iter().map(|server| format!("`{}`", server.name)).collect::<Vec<_>>().join(", ")
        )
    };
    let computer = computer.map(|computer| format!(" {computer}")).unwrap_or_default();
    let on_demand_tools = ["reminders, routines, check-ins"]
        .into_iter()
        .chain(pages.notes.then_some("finding and changing notes"))
        .chain(["artifacts", "the browser"])
        .chain(pages.feed.then_some("the Feed"))
        .collect::<Vec<_>>()
        .join(", ");
    // Without Notes, a note asked for isn't kept as a memory instead: the agent says Notes is off.
    let notes = if pages.notes {
        "When the user asks you to take, make or save a note, or to note something down for later, even inside an answer to a coding tool's question, \
write it on Filo's Notes tab with create_note: a note isn't a memory (save_memory is for lasting facts about them), and a coding tool doesn't keep it for them."
    } else {
        "Notes is off. If the user asks for a note, say so and that it can be turned on in Settings › Pages."
    };
    format!(
        "What you can use here: {filo}; {native}. {apps}{computer} \
Filo's main tools are loaded; the rest of Filo's tools ({on_demand_tools}) and the apps' tools {on_demand}. \
Filo's browser is always yours: its tools (`browser_open`, then `browser_read`, `browser_click`, `browser_type`, `browser_drag` and the rest) work web pages in the background \
in a browser of Filo's own that keeps the user's logins, for sites that need their account or a real browser, and for what the user asks with @browser; \
they click, type and drag there without the user's screen or mouse, so never work a page of Filo's browser on screen. \
Use its tool for each part of a page: `browser_select` for menus, `browser_upload` for files, `browser_hover`, `browser_dialog` for a page's alert or confirm, \
`browser_wait` rather than reading again and again; what a page downloads lands in Filo's downloads folder, and answers say where. \
The user sees you work there and can pause you or take a tab over: your next call then waits, and once they hand it back, read the page again. \
{notes} \
Tell them something is noted, saved or remembered only after the tool that does it succeeded in this conversation. \
Search the web for anything current or that you aren't sure of, and read the files the user attaches or points you to. \
When a small visual would say it faster than sentences (the shape of a day, a comparison, progress, a trend, where time or money went, a timeline), \
call show_glance before your reply, then keep the reply to what matters. \
Pictures are drawn by the user's Codex through Filo's generate_images, which shows them in the chat as Codex drew them: \
show those originals before any version of your own, and call one you trace, vectorize or redraw from them a traced or redrawn version, never Codex-drawn. \
Use the shell for quick look-ups and small jobs the user asks for; hand coding work to a coding tool with start_work rather than doing it here, \
and ask before deleting, moving or overwriting the user's files or doing anything else hard to undo. \
When the user hasn't said what they want and a guess would mean a lot of work they may throw away (designs, code, research), \
a short question about what's bothering them first saves their time; when the request is clear, or quick to redo, just do it. \
Your turn ends as soon as you've replied, and nothing wakes you after that unless something brings you back: the user's next message, \
news from a coding task you started with start_work, a check-in you planned with schedule_check_in, or a routine's run. \
So a reply that says what you'll do next (\"Next I'll draft the listing text\") only comes true if one of those is in place: \
do the work in this turn before you reply when it fits (a draft for the user to approve can come in the same reply), \
hand it to a coding task when it's coding, or schedule a check-in for when it can be done (once something has finished, once the user has had time). \
Without one, the user waits on work that never comes, since you won't be back to do it. \
When the next step waits on the user's go-ahead, offer it as a question (\"Want me to draft the listing text?\"), and their answer brings you back. \
Commands still running in your shell stop with the turn too. \
To leave something running after your reply (a dev server, a file watcher, a long build), start it detached with its output in a log file, \
like `nohup npm run dev > /tmp/<name>.log 2>&1 < /dev/null & echo $!` (which prints its process ID); check that it came up (its log, its address), \
then tell the user where it runs, where its log is and how to stop it (`kill` with its process ID). \
To make something for the user's Artifacts library (a report, a page, a diagram, a small tool), build it as an HTML file with the artifacts skill \
and publish the file with publish_artifact; publishing the same file again saves its next version. \
A page a coding task built is published the same way, by its file's path, once its round has finished. \
When a request needs something you don't have, say so in your message."
    )
}

/// The instructions a chat turn runs with: the chat's own, with what it can use after them (`capabilities`).
pub fn with_capabilities(
    instructions: &str,
    engine: Engine,
    mode: &str,
    servers: &[McpServer],
    computer: Option<&str>,
    pages: Pages,
) -> String {
    format!("{}\n\n{}", instructions.trim_end(), capabilities(engine, mode, servers, computer, pages))
}

/// A short, stable mark of a turn's instructions (FNV-1a), kept with the session to tell when they've changed since.
pub fn fingerprint(text: &str) -> String {
    let hash = text
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    format!("{hash:016x}")
}

/// What goes in front of a resumed session's message when the chat's instructions changed since it started (the agent
/// edited, apps switched): the current ones, to follow from now on. Both CLIs keep a session's first instructions
/// (Claude Code's `--system-prompt-snapshot`), so they're sent again rather than lost.
pub fn updated_instructions(instructions: &str) -> String {
    format!(
        "[Your instructions in this chat have changed. From now on, follow these in place of the ones you were given earlier:]\n\n{}\n\n[End of the updated instructions.]",
        instructions.trim()
    )
}

/// The program that serves Filo's tools: the app itself with `--local-mcp`, or `filo-local mcp`.
#[derive(Clone, Debug)]
pub struct McpLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Another MCP server a turn plugs in besides Filo's: one of the user's apps (apps.rs), through Filo's proxy. Claude
/// Code allows all of its tools (`mcp__<name>`), Codex runs them without asking; the proxy hides those turned off.
#[derive(Clone, Debug, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// One turn to run.
#[derive(Clone, Debug)]
pub struct TurnSpec {
    pub engine: Engine,
    /// The CLI to run (found with `find_program`).
    pub program: PathBuf,
    /// The text for the CLI (the message, with earlier messages in front on a new session).
    pub prompt: String,
    /// Resume this session; `None` starts one.
    pub resume: Option<String>,
    /// Claude Code: the id a new session gets, so it's known even if the turn fails early.
    pub new_session_id: Option<String>,
    pub workspace: PathBuf,
    /// The MCP server command, chat and turn included.
    pub mcp: McpLaunch,
    /// PATH for the CLI (apps opened from Finder start with a bare one).
    pub path_env: Option<String>,
    pub model: Option<String>,
    /// The chat's own instructions (an agent's), else `INSTRUCTIONS`.
    pub instructions: Option<String>,
    /// Files the user sent with the message that go to the CLI as they are: pictures for both, PDFs for Claude Code.
    pub inputs: Vec<Input>,
    /// The user's apps for this engine (apps.rs `servers_for`).
    pub servers: Vec<McpServer>,
    /// A session nobody resumes (a group turn of a speaker with no chat of its own): Claude Code keeps its prompt cache
    /// 5 minutes rather than the hour it picks on a subscription, whose cache writes cost more (`ONE_OFF_CACHE`).
    pub one_off: bool,
    /// The names of the user's skills (skills.rs), copied into the workspace (store.rs `write_skills`): with Filo's own
    /// and `skills::BUILT_IN`, the only skills the turn sees.
    pub skills: Vec<String>,
    /// Folders besides the workspace its file tools and shell may write in whatever its mode: the user's skills, which
    /// it installs and changes there (skills.rs `install_line`).
    pub writable: Vec<PathBuf>,
    /// Folders Claude Code's file-reading tools read without asking short of Full access, besides the workspace and
    /// `writable`: the chats' files its instructions point to and the files the user sent (turn.rs `readable`). Any
    /// other file, and any web page fetched, goes to the mode's reviewer.
    pub readable: Vec<PathBuf>,
    /// How freely its shell works: its engine's tool's mode on the Coding page (coding.rs `turn_mode`).
    pub mode: String,
    /// The reasoning level: the one picked in the chat's composer (`Chat::effort`), else for Muse Code and OpenCode their
    /// tool's on the Coding page; none: Claude Code's own, and Codex's from the user's config.
    pub effort: Option<String>,
    /// The optional pages that are on (`mcp::pages`): Claude Code is allowed only their tools.
    pub pages: Pages,
}

/// Claude Code's setting for how long its prompt cache lasts ("5m" or "1h"), and what a one-off session sets it to:
/// all of its calls come within seconds of each other, so an hour's cache is paid for and never used.
const ONE_OFF_CACHE: (&str, &str) = ("CLAUDE_CODE_PROMPT_CACHE_TTL", "5m");

/// Claude Code's background tasks, off in a chat turn: they'd end with it once it has replied. Without them its Bash
/// has no `run_in_background`, and a command that outlasts its timeout is stopped rather than moved to the background.
const NO_BACKGROUND_TASKS: (&str, &str) = ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");

/// A file sent with the message, for the CLI to see itself rather than read about.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub path: PathBuf,
    pub content_type: String,
}

impl Input {
    fn is_image(&self) -> bool {
        crate::attachments::is_image(&self.content_type)
    }
}

/// Claude Code's stdin with `--input-format stream-json`: the prompt and each picture or PDF as content blocks, in one
/// user message. A file that can't be read is left out (the prompt still names it and where it's saved).
fn claude_user_message(prompt: &str, inputs: &[Input]) -> String {
    use base64::Engine as _;
    let mut content = vec![json!({ "type": "text", "text": prompt })];
    for input in inputs {
        let kind = if input.is_image() {
            "image"
        } else if input.content_type == "application/pdf" {
            "document"
        } else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&input.path) else { continue };
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        content.push(
            json!({ "type": kind, "source": { "type": "base64", "media_type": input.content_type, "data": data } }),
        );
    }
    let mut line = json!({ "type": "user", "message": { "role": "user", "content": content } }).to_string();
    line.push('\n');
    line
}

/// Claude Code's first stdin line with `--input-format stream-json`: the Agent SDK's `initialize` control request, whose
/// `skills` allow-list is all of Claude Code's skills the turn sees (the Skill tool refuses the rest), wherever they
/// come from: Filo's and `skills::BUILT_IN` (a bare name also matches a synced one, `anthropic-skills:pdf`).
fn claude_skills_request(filo: &[String]) -> String {
    let mut names: Vec<&str> = filo.iter().map(String::as_str).collect();
    names.extend(
        crate::skills::BUILT_IN
            .iter()
            .chain(crate::skills::FILO_SKILLS.iter().map(|(name, _)| name))
            .filter(|name| !filo.iter().any(|own| own == *name)),
    );
    let mut line = json!({ "type": "control_request", "request_id": "filo-init", "request": { "subtype": "initialize", "skills": names } }).to_string();
    line.push('\n');
    line
}

/// The marketplace with Codex's document plugins: its primary runtime's, where the user's `config.toml` says, else
/// where Codex puts it. None when it isn't on this Mac (a chat then goes without them).
fn codex_documents_marketplace() -> Option<PathBuf> {
    let config = std::fs::read_to_string(codex_home().join("config.toml")).unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    codex_table_value(&config, &format!("marketplaces.{CODEX_DOCUMENTS_MARKETPLACE}"), "source")
        .map(PathBuf::from)
        .into_iter()
        .chain([home.join(".cache/codex-runtimes/codex-primary-runtime/plugins").join(CODEX_DOCUMENTS_MARKETPLACE)])
        .find(|dir| dir.is_dir())
}

/// A `key = "string"` in `[table]` of a Codex `config.toml`.
fn codex_table_value(config: &str, wanted_table: &str, wanted: &str) -> Option<String> {
    let mut table = String::new();
    for line in config.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[') {
            table = header.split(']').next().unwrap_or_default().trim().replace('"', "");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        let Some(quote) = value.chars().next().filter(|quote| *quote == '"' || *quote == '\'') else { continue };
        if table == wanted_table && key.trim() == wanted {
            return value[1..].split(quote).next().map(str::to_string).filter(|value| !value.is_empty());
        }
    }
    None
}

/// Codex's primary-runtime marketplace and the plugins in it a chat turn gets: skills for PDFs, Word documents,
/// spreadsheets and presentations (no MCP servers or apps).
const CODEX_DOCUMENTS_MARKETPLACE: &str = "openai-primary-runtime";
const CODEX_DOCUMENT_PLUGINS: [&str; 4] = ["pdf", "documents", "spreadsheets", "presentations"];
/// Skills of those plugins a chat turn doesn't get: driving Excel itself.
const CODEX_PLUGIN_SKILLS_OFF: [&str; 1] = ["spreadsheets:excel-live-control"];

/// The plugins Codex has installed (`$CODEX_HOME/plugins/cache/<marketplace>/<plugin>`), as `<plugin>@<marketplace>`:
/// with plugins on, those installed from the user's account load even without their config, so each but the document
/// ones is named off.
fn codex_installed_plugins() -> Vec<String> {
    let cache = codex_home().join("plugins/cache");
    let mut plugins = Vec::new();
    for marketplace in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
        let market = marketplace.file_name().to_string_lossy().into_owned();
        for plugin in
            std::fs::read_dir(marketplace.path()).into_iter().flatten().flatten().filter(|entry| entry.path().is_dir())
        {
            plugins.push(format!("{}@{market}", plugin.file_name().to_string_lossy()));
        }
    }
    plugins.sort();
    plugins
}

/// The skill folders Codex reads besides the workspace's own and its bundled ones: the user's, its legacy one and the
/// admin's.
fn codex_skill_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    vec![home.join(".agents/skills"), codex_home().join("skills"), PathBuf::from("/etc/codex/skills")]
}

/// Every skill in `roots` (a `SKILL.md` at most a few folders down): the name its front matter gives (else its
/// folder's) and its file, as found and as it resolves.
fn codex_skills_on_disk(roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, depth: usize, found: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let file = path.join("SKILL.md");
            if file.is_file() {
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                let name = front_matter_name(&text).unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
                if let Some(real) = file.canonicalize().ok().filter(|real| *real != file) {
                    found.push((name.clone(), real));
                }
                found.push((name, file));
            } else if depth > 1 {
                walk(&path, depth - 1, found);
            }
        }
    }
    let mut found = Vec::new();
    for root in roots {
        walk(root, 4, &mut found);
    }
    found
}

/// The `name:` of a `SKILL.md`'s front matter.
fn front_matter_name(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix("---")?;
    let front = rest.split("\n---").next()?;
    let value = front.lines().find_map(|line| line.trim().strip_prefix("name:"))?.trim();
    Some(value.trim_matches(|c| c == '"' || c == '\'').to_string()).filter(|name| !name.is_empty())
}

/// Codex's `-c` options for a chat turn's skills: its bundled ones off, each skill found elsewhere (`found`) off by its
/// file and (unless Filo has one by its name) by its name, and the document plugins of `marketplace` on, but for
/// `CODEX_PLUGIN_SKILLS_OFF`, with every other plugin it has `installed` off. What's left: the workspace's
/// `.agents/skills` (Filo's) and those plugins' skills.
fn codex_skill_args(
    filo: &[String],
    found: &[(String, PathBuf)],
    marketplace: Option<&Path>,
    installed: &[String],
) -> Vec<String> {
    let off = |selector: &str, value: &str| format!("{{{selector}={},enabled=false}}", toml_string(value));
    let mut rules: Vec<String> = Vec::new();
    for (name, path) in found {
        rules.push(off("path", &path.display().to_string()));
        if name != "dataviz"
            && !crate::skills::is_filo_skill(name)
            && !filo.contains(name)
            && !rules.contains(&off("name", name))
        {
            rules.push(off("name", name));
        }
    }
    rules.extend(CODEX_PLUGIN_SKILLS_OFF.iter().map(|name| off("name", name)));
    let mut args = vec![
        "-c".to_string(),
        "skills.bundled.enabled=false".to_string(),
        "-c".to_string(),
        format!("skills.config=[{}]", rules.join(",")),
    ];
    if let Some(dir) = marketplace {
        // Inline tables: Codex ignores `plugins."<name>".enabled` given as a dotted key.
        let on: Vec<String> =
            CODEX_DOCUMENT_PLUGINS.iter().map(|plugin| format!("{plugin}@{CODEX_DOCUMENTS_MARKETPLACE}")).collect();
        let plugins = on
            .iter()
            .map(|plugin| format!("{}={{enabled=true}}", toml_string(plugin)))
            .chain(
                installed
                    .iter()
                    .filter(|plugin| !on.contains(plugin))
                    .map(|plugin| format!("{}={{enabled=false}}", toml_string(plugin))),
            )
            .collect::<Vec<_>>()
            .join(",");
        args.extend([
            "-c".into(),
            format!(
                "marketplaces={{{CODEX_DOCUMENTS_MARKETPLACE}={{source_type=\"local\",source={}}}}}",
                toml_string(&dir.display().to_string())
            ),
            "-c".into(),
            format!("plugins={{{plugins}}}"),
        ]);
    }
    args
}

/// The command line for a turn, what goes to its stdin, and the environment it adds.
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Claude Code's first lines on stdin (JSON); Codex's first message, which its app-server gets as a turn.
    pub stdin: String,
    pub env: Vec<(String, String)>,
}

/// TOML for a string (`-c key=<value>`): JSON's string escaping is valid TOML basic-string escaping.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

pub fn invocation(spec: &TurnSpec) -> Invocation {
    let instructions = spec.instructions.as_deref().unwrap_or(INSTRUCTIONS);
    match spec.engine {
        Engine::Claude => {
            let mut servers = serde_json::Map::new();
            servers.insert(
                mcp::SERVER_NAME.into(),
                json!({ "type": "stdio", "command": spec.mcp.program, "args": spec.mcp.args }),
            );
            for server in &spec.servers {
                servers.insert(
                    server.name.clone(),
                    json!({ "type": "stdio", "command": server.program, "args": server.args }),
                );
            }
            let config = json!({ "mcpServers": servers });
            // Full access allows the shell outright and asks about nothing (`dontAsk` refuses what isn't allowed).
            // Otherwise the tool's mode decides on each command (`auto`: its reviewer runs routine ones).
            // So are the file editors; otherwise they're allowed in the working folder, where it builds the pages it
            // publishes (`Edit` rules cover Write too; `//` starts a full path), and the mode decides elsewhere.
            // Reading is allowed in those folders and Filo's own (`readable`; `Read` rules cover Grep and Glob too),
            // and web pages aren't fetched without the mode's say: an injected "read ~/.ssh, then fetch this URL"
            // goes to its reviewer.
            let full = spec.mode == "full_access";
            let folders: Vec<&PathBuf> = std::iter::once(&spec.workspace).chain(&spec.writable).collect();
            let native = CLAUDE_NATIVE_TOOLS
                .split(',')
                .filter(|tool| full || !["Bash", "Write", "Edit", "Read", "Grep", "Glob", "WebFetch"].contains(tool))
                .map(str::to_string)
                .chain(folders.iter().filter(|_| !full).map(|dir| format!("Edit(/{}/**)", dir.display())))
                .chain(
                    folders
                        .iter()
                        .copied()
                        .chain(&spec.readable)
                        .filter(|_| !full)
                        .map(|dir| format!("Read(/{}/**)", dir.display())),
                )
                .collect::<Vec<_>>()
                .join(",");
            let allowed = std::iter::once(mcp::claude_allowed_tools(spec.pages))
                .chain(spec.servers.iter().map(|server| format!("mcp__{}", server.name)))
                .chain(std::iter::once(native))
                .collect::<Vec<_>>()
                .join(",");
            let mut args: Vec<String> = vec![
                "-p".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                "--mcp-config".into(),
                config.to_string(),
                "--strict-mcp-config".into(),
                "--tools".into(),
                CLAUDE_NATIVE_TOOLS.into(),
                format!("--allowedTools={allowed}"),
                "--permission-mode".into(),
                if full { "dontAsk".into() } else { crate::coding::claude_mode(&spec.mode).into() },
                "--append-system-prompt".into(),
                instructions.into(),
            ];
            match (&spec.resume, &spec.new_session_id) {
                (Some(session), _) => args.extend(["--resume".into(), session.clone()]),
                (None, Some(session)) => args.extend(["--session-id".into(), session.clone()]),
                (None, None) => {}
            }
            if let Some(model) = &spec.model {
                args.extend(["--model".into(), model.clone()]);
            }
            if let Some(effort) = &spec.effort {
                args.extend(["--effort".into(), effort.clone()]);
            }
            let env = std::iter::once(NO_BACKGROUND_TASKS)
                .chain(spec.one_off.then_some(ONE_OFF_CACHE))
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            args.extend(["--input-format".into(), "stream-json".into()]);
            let stdin =
                format!("{}{}", claude_skills_request(&spec.skills), claude_user_message(&spec.prompt, &spec.inputs));
            Invocation { program: spec.program.clone(), args, stdin, env }
        }
        Engine::Codex => {
            let mut args: Vec<String> = vec!["app-server".into(), "-c".into(), "web_search=\"live\"".into()];
            // The user's config.toml loads: its own MCP servers and its notify program go off, so a chat's servers are
            // Filo's and the apps on for this agent, under names of their own.
            let user_servers =
                codex_user_servers(&std::fs::read_to_string(codex_home().join("config.toml")).unwrap_or_default());
            args.extend(codex_user_config_off(&user_servers));
            // The shell as the tool's mode has it: Full access runs anything, with network; Auto is sandboxed and has
            // Codex's reviewer approve going further. Nobody is at a terminal to approve either way.
            let (approval, sandbox, reviewer) = crate::coding_session::codex_mode(&spec.mode);
            args.extend([
                "-c".into(),
                format!("sandbox_mode={}", toml_string(sandbox)),
                "-c".into(),
                format!("approval_policy={}", toml_string(approval)),
            ]);
            if let Some(reviewer) = reviewer {
                args.extend(["-c".into(), format!("approvals_reviewer={}", toml_string(reviewer))]);
            }
            // Its sandbox lets it write there too.
            if !spec.writable.is_empty() {
                let roots = spec
                    .writable
                    .iter()
                    .map(|dir| toml_string(&dir.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(", ");
                args.extend(["-c".into(), format!("sandbox_workspace_write.writable_roots=[{roots}]")]);
            }
            // Filo's server twice: its pinned tools on `filo_core`, which Codex loads directly rather than through
            // its tool search, and the rest on `filo`. Both run without asking.
            for (name, part) in [(mcp::CORE_SERVER_NAME, "pinned"), (mcp::SERVER_NAME, "deferred")] {
                let server = format!("mcp_servers.{}", codex_own_name(name, &user_servers));
                let mcp_args = spec
                    .mcp
                    .args
                    .iter()
                    .map(String::as_str)
                    .chain(["--tools", part])
                    .map(toml_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                args.extend([
                    "-c".into(),
                    format!("{server}.command={}", toml_string(&spec.mcp.program.display().to_string())),
                    "-c".into(),
                    format!("{server}.args=[{mcp_args}]"),
                    "-c".into(),
                    format!("{server}.default_tools_approval_mode=\"approve\""),
                    // Making pictures (characters_mcp.rs) takes minutes; Codex's default gives a tool one.
                    "-c".into(),
                    format!("{server}.tool_timeout_sec=900"),
                    "-c".into(),
                    format!("{server}.enabled=true"),
                ]);
            }
            // Codex names a server's tools' namespace `mcp__<server>`.
            let core = codex_own_name(mcp::CORE_SERVER_NAME, &user_servers);
            args.extend([
                "-c".into(),
                format!("features.code_mode.direct_only_tool_namespaces=[{}]", toml_string(&format!("mcp__{core}"))),
            ]);
            args.extend(codex_server_args(&spec.servers, &user_servers));
            // Plugins stay off but for the document ones, when they're on this Mac.
            let marketplace = codex_documents_marketplace();
            for feature in
                CODEX_FEATURES_OFF.into_iter().filter(|feature| marketplace.is_none() || *feature != "plugins")
            {
                args.extend(["-c".into(), format!("features.{feature}=false")]);
            }
            args.extend(codex_skill_args(
                &spec.skills,
                &codex_skills_on_disk(&codex_skill_roots()),
                marketplace.as_deref(),
                &codex_installed_plugins(),
            ));
            // Over the user's config.toml's, which gives them when the chat picks none. They go with the thread and the
            // turn too (coding_session.rs), as do the workspace, the thread to resume and the pictures.
            if let Some(effort) = &spec.effort {
                args.extend(["-c".into(), format!("model_reasoning_effort={}", toml_string(effort))]);
            }
            if let Some(model) = &spec.model {
                args.extend(["-c".into(), format!("model={}", toml_string(model))]);
            }
            let first = if spec.resume.is_none() {
                format!("{instructions}\n\n{}", spec.prompt)
            } else {
                format!("{CODEX_REMINDER}\n\n{}", spec.prompt)
            };
            Invocation { program: spec.program.clone(), args, stdin: first, env: Vec::new() }
        }
        // Their turns run as sessions (`start_turn`), which take the spec itself: the command line is just the CLI.
        Engine::Muse | Engine::OpenCode => {
            Invocation { program: spec.program.clone(), args: Vec::new(), stdin: String::new(), env: Vec::new() }
        }
    }
}

/// Codex's `-c` options for the user's apps: each a stdio server run without asking, given time to start (a local
/// one may download itself first), under a name none of the user's own servers (`taken`) has.
fn codex_server_args(servers: &[McpServer], taken: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    for server in servers {
        let key = format!("mcp_servers.{}", codex_own_name(&server.name, taken));
        let server_args =
            format!("[{}]", server.args.iter().map(|arg| toml_string(arg)).collect::<Vec<_>>().join(", "));
        args.extend([
            "-c".into(),
            format!("{key}.command={}", toml_string(&server.program.display().to_string())),
            "-c".into(),
            format!("{key}.args={server_args}"),
            "-c".into(),
            format!("{key}.default_tools_approval_mode=\"approve\""),
            "-c".into(),
            format!("{key}.startup_timeout_sec=60"),
            "-c".into(),
            format!("{key}.tool_timeout_sec=600"),
            "-c".into(),
            format!("{key}.enabled=true"),
        ]);
    }
    args
}

/// The CLI on PATH (or `path_env`), else where installers usually put it.
pub fn find_program(engine: Engine, path_env: Option<&str>) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let own = std::env::var("PATH").unwrap_or_default();
    let dirs =
        path_env.unwrap_or("").split(':').chain(own.split(':')).filter(|dir| !dir.is_empty()).map(PathBuf::from).chain(
            [
                home.join(".local/bin"),
                home.join(".claude/local"),
                home.join(".opencode/bin"),
                home.join(".npm-global/bin"),
                home.join(".bun/bin"),
                "/opt/homebrew/bin".into(),
                "/usr/local/bin".into(),
            ],
        );
    for dir in dirs {
        let candidate = dir.join(engine.program());
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// How a turn's CLI run ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunOutcome {
    /// The session to resume next turn.
    pub session_id: Option<String>,
    /// Why the turn failed, in words for the chat.
    pub error: Option<String>,
    /// The CLI's own final text (not shown: replies come through send_message), for when it never called the tool.
    pub final_text: Option<String>,
    /// Stopped by the user.
    pub stopped: bool,
    pub timed_out: bool,
    /// The session it was to resume isn't there (deleted, another Mac's): the one failure a fresh session fixes.
    pub session_gone: bool,
    /// The CLI's last lines on stderr when it failed, for a problem report (failures.rs).
    pub stderr: Vec<String>,
}

/// Stops a running turn (from any thread).
#[derive(Clone)]
pub struct Stopper {
    pid: Arc<Mutex<Option<i32>>>,
    stopped: Arc<AtomicBool>,
    /// A process of Filo's own that ends its work itself (`for_worker`): SIGTERM to it alone, nothing after.
    worker: bool,
}

impl Stopper {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if self.worker {
            if let Some(pid) = *self.pid.lock().unwrap() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            return;
        }
        if let Some(pid) = *self.pid.lock().unwrap() {
            // The whole group: the CLI and the MCP server it started.
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
            let pid_slot = self.pid.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if let Some(pid) = *pid_slot.lock().unwrap() {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
            });
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Stops the process group `pid` leads (a coding task's round kept talking to, coding_session.rs).
    pub(crate) fn for_group(pid: i32) -> Stopper {
        Stopper { pid: Arc::new(Mutex::new(Some(pid))), stopped: Arc::new(AtomicBool::new(false)), worker: false }
    }

    /// Stops what hasn't started yet (a turn waiting for the chat's turn before it, turn.rs): only the flag.
    pub(crate) fn before_start() -> Stopper {
        Stopper { pid: Arc::new(Mutex::new(None)), stopped: Arc::new(AtomicBool::new(false)), worker: false }
    }

    /// Stops a turn or coding round running in a process of its own (the app's `filo --worker`): SIGTERM asks it to end
    /// its CLI and save how the work ended, so it's never killed outright.
    pub fn for_worker(pid: i32) -> Stopper {
        Stopper { pid: Arc::new(Mutex::new(Some(pid))), stopped: Arc::new(AtomicBool::new(false)), worker: true }
    }

    /// From now on stops what `started` stops (a session's CLI, which starts after this Stopper is handed out); one
    /// stopped already stops it at once.
    pub(crate) fn follow(&self, started: &Stopper) {
        *self.pid.lock().unwrap() = *started.pid.lock().unwrap();
        if self.is_stopped() {
            started.stop();
        }
    }

    /// Its process ended: nothing is left to signal.
    pub(crate) fn exited(&self) {
        *self.pid.lock().unwrap() = None;
    }
}

/// Claude Code's background work, from its stream-json events: what runs now (a command its Bash runs in the
/// background, a Monitor, a subagent sent off), by task id with what it is, and how the work that ended went. A coding
/// task's round waits on it (coding_session.rs). `background_tasks_changed` lists all that runs; `task_started`
/// (backgrounded), `task_notification` and `task_updated` say what started and ended. A task whose command never ends
/// on its own (`endless_watcher`: a `tail -f`, run in the background or as a Monitor) is a watcher, which nothing waits
/// on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackgroundWork {
    pub running: Vec<(String, String)>,
    pub ended: Vec<String>,
    /// The tool calls (Bash, Monitor) whose command never ends on its own, by tool_use id, till their task starts.
    watcher_calls: std::collections::HashSet<String>,
    /// The tasks those started, by task id.
    watchers: std::collections::HashSet<String>,
}

impl BackgroundWork {
    /// Reads the tool calls in one of Claude Code's assistant messages: a Bash or Monitor command that never ends on
    /// its own is a watcher once its task starts.
    pub fn claude_tool_calls(&mut self, message: &Value) {
        if message["type"] != "assistant" {
            return;
        }
        for block in
            message["message"]["content"].as_array().into_iter().flatten().filter(|block| block["type"] == "tool_use")
        {
            let (Some(id), Some(command)) = (block["id"].as_str(), block["input"]["command"].as_str()) else {
                continue;
            };
            if matches!(block["name"].as_str(), Some("Bash" | "Monitor")) && endless_watcher(command) {
                self.watcher_calls.insert(id.to_string());
            }
        }
    }

    /// Something runs that the work waits on (not only watchers).
    pub fn waits(&self) -> bool {
        self.running.iter().any(|(id, _)| !self.watchers.contains(id))
    }

    /// The watchers still running: their task ids and what they are.
    pub fn watching(&self) -> Vec<(String, String)> {
        self.running.iter().filter(|(id, _)| self.watchers.contains(id)).cloned().collect()
    }

    /// Reads one of Claude Code's events; true when it was about background work.
    pub fn claude_event(&mut self, event: &Value) -> bool {
        if event["type"] != "system" {
            return false;
        }
        let id = |value: &Value| value.as_str().unwrap_or_default().to_string();
        let task = id(&event["task_id"]);
        match event["subtype"].as_str().unwrap_or_default() {
            "background_tasks_changed" => {
                self.running = event["tasks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|task| (id(&task["task_id"]), background_label(task)))
                    .collect();
            }
            "task_started" if event["is_backgrounded"] == true => {
                if event["tool_use_id"].as_str().is_some_and(|call| self.watcher_calls.remove(call)) {
                    self.watchers.insert(task.clone());
                }
                if !self.running.iter().any(|(id, _)| *id == task) {
                    self.running.push((task, background_label(event)));
                }
            }
            "task_notification" => {
                self.running.retain(|(id, _)| *id != task);
                if let Some(summary) = event["summary"].as_str().map(str::trim).filter(|summary| !summary.is_empty()) {
                    self.ended.push(summary.to_string());
                }
            }
            "task_updated"
                if matches!(event["patch"]["status"].as_str(), Some("completed" | "failed" | "killed" | "stopped")) =>
            {
                self.running.retain(|(id, _)| *id != task);
            }
            _ => return false,
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// What runs, in words.
    pub fn what(&self) -> Vec<String> {
        self.running.iter().map(|(_, what)| what.clone()).collect()
    }
}

/// What a Claude Code background task is, from its `description` (and its kind when it has none).
fn background_label(task: &Value) -> String {
    let field = |key: &str| task[key].as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_string);
    field("description").unwrap_or_else(|| match task["task_type"].as_str() {
        Some(kind) if kind.contains("monitor") => "a monitor".into(),
        Some(kind) if kind.contains("agent") => "a subagent".into(),
        _ => "a background command".into(),
    })
}

/// A shell command that follows something forever and never ends on its own: `tail -f` (or `-F`, `--follow`),
/// `log stream`, `journalctl -f`, `docker logs -f`, `kubectl logs -f`, `watch …`, `sleep infinity`. One with a way to end
/// (`timeout`, `head`, `grep -q` or `-m`, `tail --pid`, an `exit` or `break`) waits on something, so it isn't.
pub fn endless_watcher(command: &str) -> bool {
    let segments = shell_segments(command);
    let programs = || segments.iter().filter_map(|words| program(words));
    let has = |words: &[String], options: &[&str], letter: char| {
        words.iter().any(|word| {
            options.contains(&word.split('=').next().unwrap_or_default())
                || (word.starts_with('-') && !word.starts_with("--") && word.contains(letter))
        })
    };
    let ends = programs().any(|(name, args)| match name {
        "timeout" | "gtimeout" | "head" => true,
        "grep" | "egrep" | "fgrep" | "rg" => args.iter().any(|word| {
            ["--quiet", "--silent", "--max-count", "--files-with-matches"]
                .contains(&word.split('=').next().unwrap_or_default())
                || (word.starts_with('-')
                    && !word.starts_with("--")
                    && word.chars().any(|c| matches!(c, 'q' | 'm' | 'l')))
        }),
        "awk" | "gawk" => args.iter().any(|word| word.contains("exit")),
        "tail" => args.iter().any(|word| word.starts_with("--pid")),
        _ => false,
    }) || segments.iter().flatten().any(|word| word == "exit" || word == "break");
    let follows = programs().any(|(name, args)| match name {
        "tail" => has(args, &["--follow"], 'f') || has(args, &[], 'F'),
        "journalctl" => has(args, &["--follow"], 'f'),
        "docker" | "podman" | "kubectl" => {
            args.first().is_some_and(|sub| sub == "logs") && has(args, &["--follow"], 'f')
        }
        "log" => args.first().is_some_and(|sub| sub == "stream"),
        "watch" => true,
        "sleep" => args.first().is_some_and(|time| time == "infinity" || time == "inf"),
        _ => false,
    });
    follows && !ends
}

/// A command's program (its name, without a folder) and its arguments, past variable assignments and wrappers (`nohup`, `exec`, `stdbuf -oL`).
fn program(words: &[String]) -> Option<(&str, &[String])> {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let wrapper = matches!(
            word.as_str(),
            "nohup" | "exec" | "command" | "stdbuf" | "unbuffer" | "env" | "do" | "then" | "else"
        );
        let skipped = wrapper
            || (word.contains('=') && !word.starts_with('-'))
            || (at > 0 && word.starts_with('-') && words[at - 1] == "stdbuf");
        if !skipped {
            return Some((word.rsplit('/').next().unwrap_or(word), &words[at + 1..]));
        }
        at += 1;
    }
    None
}

/// A shell command's simple commands (split at unquoted `|`, `;`, `&`, newlines and parentheses), each as its words
/// with quotes taken off.
fn shell_segments(command: &str) -> Vec<Vec<String>> {
    let (mut segments, mut words, mut word) = (Vec::new(), Vec::new(), String::new());
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    let end_word = |words: &mut Vec<String>, word: &mut String| {
        if !word.is_empty() {
            words.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => word.extend(chars.next()),
            (None, c) if c.is_whitespace() && c != '\n' => end_word(&mut words, &mut word),
            (None, '|' | ';' | '&' | '\n' | '(' | ')' | '`') => {
                end_word(&mut words, &mut word);
                if !words.is_empty() {
                    segments.push(std::mem::take(&mut words));
                }
            }
            (None, c) => word.push(c),
        }
    }
    end_word(&mut words, &mut word);
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

/// A turn under way: a chat's session run on a thread of its own (`start_turn`), whose lines come as the CLI printed them
/// (Claude Code, Codex) or as steps of Filo's own (Muse Code, OpenCode); or a CLI whose JSON lines are read here (a page
/// shared to claude.ai, `start_invocation`).
pub enum Running {
    Cli(CliRun),
    Session(SessionRun),
}

pub struct CliRun {
    child: Child,
    stopper: Stopper,
    stdout: Option<std::process::ChildStdout>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
    engine: Engine,
    /// It's stopped after this long in all, however busy it is.
    timeout: Duration,
}

/// What a turn that went quiet for `quiet` and was stopped says in its chat.
pub(crate) fn hung_error(engine: Engine, quiet: Duration) -> String {
    let count = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    let quiet = match quiet.as_secs() {
        secs if secs >= 3600 && secs % 3600 == 0 => count(secs / 3600, "hour"),
        secs if secs >= 60 => count(secs / 60, "minute"),
        secs => count(secs, "second"),
    };
    format!("{} seemed stuck: nothing came from it for {quiet}, so it was stopped.", engine.label())
}

/// Starts a chat turn with nothing steering it, stopped once nothing has come from it for `hung_after`. Fails only when
/// the CLI can't be started at all.
#[cfg(test)]
fn start(spec: &TurnSpec, hung_after: Duration) -> Result<Running, String> {
    start_turn(spec, &invocation(spec), hung_after, None)
}

/// Starts `call` in `workspace` in a process group of its own, reading it as `engine`'s JSON events (a page shared to
/// claude.ai), stopped after `timeout` in all. Fails only when the CLI can't be started at all.
pub fn start_invocation(
    call: &Invocation,
    engine: Engine,
    workspace: &Path,
    path_env: Option<&str>,
    timeout: Duration,
) -> Result<Running, String> {
    // Its own group, so Stop reaches everything it started.
    let mut child = cli_command(&call.program, workspace, path_env, false)
        .args(&call.args)
        .stderr(Stdio::piped())
        .envs(call.env.iter().map(|(key, value)| (key, value)))
        .spawn()
        .map_err(|error| format!("Couldn't start {} ({}): {error}", engine.label(), call.program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let text = call.stdin.clone();
        // A thread, so a large prompt can't block on a full pipe while nobody reads stdout yet. Closed once written:
        // Claude Code then exits at its result, whatever it left in the background.
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let stderr_tail = stderr_tail(child.stderr.take(), 40);
    let stopper = Stopper {
        pid: Arc::new(Mutex::new(Some(child.id() as i32))),
        stopped: Arc::new(AtomicBool::new(false)),
        worker: false,
    };
    let stdout = child.stdout.take();
    Ok(Running::Cli(CliRun { child, stopper, stdout, stderr_tail, engine, timeout }))
}

impl Running {
    pub fn stopper(&self) -> Stopper {
        match self {
            Running::Cli(run) => run.stopper.clone(),
            Running::Session(run) => run.stopper.clone(),
        }
    }

    /// Reads the CLI's events until it exits.
    pub fn wait(self) -> RunOutcome {
        self.wait_lines(|_| {})
    }

    /// `wait`, handing each line of the CLI's output to `on_line` as it comes (a coding task's live steps; a session's
    /// steps as `{"filo_step": <kind>}` lines, for chat_activity.rs).
    pub fn wait_lines(self, on_line: impl FnMut(&str)) -> RunOutcome {
        match self {
            Running::Cli(run) => run.wait_lines(on_line),
            Running::Session(run) => run.wait_lines(on_line),
        }
    }
}

impl CliRun {
    fn wait_lines(mut self, mut on_line: impl FnMut(&str)) -> RunOutcome {
        let timed_out = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + self.timeout;
        {
            let (stopper, timed_out, done) = (self.stopper.clone(), timed_out.clone(), done.clone());
            std::thread::spawn(move || {
                let step = Duration::from_millis(200);
                while Instant::now() < deadline {
                    if done.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(step);
                }
                if !done.load(Ordering::SeqCst) {
                    timed_out.store(true, Ordering::SeqCst);
                    stopper.stop();
                }
            });
        }
        let mut events = EventReader::new(self.engine);
        if let Some(stdout) = self.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        events.line(line.trim_end());
                        on_line(line.trim_end());
                    }
                }
            }
            // Whatever is left (a line with no newline at the end).
            let mut rest = String::new();
            let _ = reader.read_to_string(&mut rest);
            events.line(rest.trim());
            if !rest.trim().is_empty() {
                on_line(rest.trim());
            }
        }
        let status = self.child.wait();
        done.store(true, Ordering::SeqCst);
        *self.stopper.pid.lock().unwrap() = None;
        // Let the stderr reader catch the last lines.
        std::thread::sleep(Duration::from_millis(50));
        let stderr = self.stderr_tail.lock().unwrap().clone();

        let mut outcome = events.finish();
        outcome.stopped = self.stopper.is_stopped() && !timed_out.load(Ordering::SeqCst);
        outcome.timed_out = timed_out.load(Ordering::SeqCst);
        if outcome.stopped {
            outcome.error = None;
            return outcome;
        }
        if outcome.timed_out {
            outcome.error = Some(format!(
                "{} took longer than {} minutes and was stopped.",
                self.engine.label(),
                self.timeout.as_secs() / 60
            ));
            return outcome;
        }
        let failed = !status.as_ref().is_ok_and(|status| status.success());
        if outcome.error.is_none() && failed {
            let code = status
                .ok()
                .and_then(|status| status.code())
                .map_or("a signal".to_string(), |code| format!("status {code}"));
            let detail = stderr.iter().rev().find(|line| !line.trim().is_empty()).cloned();
            outcome.error = Some(match detail {
                Some(detail) => format!("{} stopped with {code}: {detail}", self.engine.label()),
                None => format!("{} stopped with {code}.", self.engine.label()),
            });
        }
        if let Some(error) = &outcome.error {
            outcome.session_gone = session_gone(error, &stderr);
            outcome.error = Some(explain(self.engine, error, &stderr));
            outcome.stderr = stderr;
        }
        outcome
    }
}

/// Messages the user sends a chat turn while it works (turn.rs `Steering`), for its session to hand its CLI.
pub trait Steer: Send {
    /// Messages to hand over now, as the CLI reads them: each is the turn's from here.
    fn take(&mut self) -> Vec<String>;
    /// Message `note`, handed over, has reached the CLI: it acts on it from its next step.
    fn seen(&mut self, note: &str);
}

/// Starts a chat turn: a session with the tool (coding_session.rs) on a thread of its own, with Filo's MCP server and
/// the user's apps plugged in, the turn's skills and its mode. Claude Code and Codex run `call` (`invocation`), and
/// `steer`, when there is one, gives them the messages the user sends meanwhile. It's stopped once nothing has come from
/// it for `hung_after`. Fails only when the CLI can't be started at all.
pub fn start_turn(
    spec: &TurnSpec,
    call: &Invocation,
    hung_after: Duration,
    steer: Option<Box<dyn Steer>>,
) -> Result<Running, String> {
    let mut servers = vec![McpServer {
        name: mcp::SERVER_NAME.into(),
        program: spec.mcp.program.clone(),
        args: spec.mcp.args.clone(),
    }];
    servers.extend(spec.servers.iter().cloned());
    let session = crate::coding_session::Spec {
        engine: spec.engine,
        program: spec.program.clone(),
        cwd: spec.workspace.clone(),
        path_env: spec.path_env.clone(),
        // Codex's first message: the prompt, with the instructions on a new session (`invocation`).
        message: if spec.engine == Engine::Codex { call.stdin.clone() } else { spec.prompt.clone() },
        resume: spec.resume.clone(),
        new_session: None,
        mode: spec.mode.clone(),
        look_only: false,
        model: spec.model.clone(),
        effort: spec.effort.clone(),
        instructions: spec.instructions.clone().unwrap_or_else(|| INSTRUCTIONS.into()),
        writable: spec.writable.clone(),
        timeout: hung_after,
        servers,
        skills: Some(spec.skills.clone()),
        chat: true,
        command: spec.engine.runs_invocation().then(|| call.clone()),
        // One that's gone would stop Codex, so it's left out (the prompt still names it).
        images: spec
            .inputs
            .iter()
            .filter(|input| input.is_image() && input.path.is_file())
            .map(|input| input.path.clone())
            .collect(),
    };
    if !session.program.is_file() {
        return Err(format!("Couldn't start {} ({}): it isn't there", spec.engine.label(), session.program.display()));
    }
    let stopper = Stopper::before_start();
    let (send, lines) = std::sync::mpsc::channel::<String>();
    let thread = {
        let stopper = stopper.clone();
        let engine = spec.engine;
        std::thread::spawn(move || {
            let mut host = ChatHost { lines: send, stopper: stopper.clone(), engine, steer };
            let end = crate::coding_session::run(&session, &mut host, |started| stopper.follow(&started));
            stopper.exited();
            end
        })
    };
    Ok(Running::Session(SessionRun { stopper, lines, thread, engine: spec.engine }))
}

/// A chat turn run as a session on a thread of its own (`start_turn`).
pub struct SessionRun {
    stopper: Stopper,
    lines: std::sync::mpsc::Receiver<String>,
    thread: std::thread::JoinHandle<crate::coding_session::End>,
    engine: Engine,
}

impl SessionRun {
    fn wait_lines(self, mut on_line: impl FnMut(&str)) -> RunOutcome {
        for line in self.lines.iter() {
            on_line(&line);
        }
        let end = self.thread.join().unwrap_or_else(|_| crate::coding_session::End {
            error: Some(format!("{} stopped unexpectedly.", self.engine.label())),
            ..Default::default()
        });
        let stopped = self.stopper.is_stopped() && !end.timed_out;
        let session_gone = end.error.as_deref().is_some_and(|error| session_gone(error, &end.stderr));
        let error =
            (!stopped).then(|| end.error.as_deref().map(|error| explain(self.engine, error, &end.stderr))).flatten();
        RunOutcome {
            session_id: end.session_id,
            final_text: end.final_text,
            stopped,
            timed_out: end.timed_out,
            session_gone,
            stderr: if error.is_some() { end.stderr } else { Vec::new() },
            error,
        }
    }
}

/// What a chat turn's session reports to: the CLI's lines as it prints them (Claude Code, Codex), else its steps as
/// lines of Filo's own (`{"filo_step":"command"}`), for the chat's activity and a page shared to claude.ai; the messages
/// the user sends meanwhile (`steer`); and nobody to ask while it works: approvals are refused, and a question is
/// answered that it should ask in its reply.
struct ChatHost {
    lines: std::sync::mpsc::Sender<String>,
    stopper: Stopper,
    engine: Engine,
    steer: Option<Box<dyn Steer>>,
}

impl crate::coding_session::Host for ChatHost {
    fn line(&mut self, line: &str) {
        if self.engine.runs_invocation() {
            let _ = self.lines.send(line.to_string());
        }
    }

    fn step(&mut self, step: crate::coding::Step) {
        if !self.engine.runs_invocation() {
            let kind = step.kind.unwrap_or(step.source);
            let _ = self.lines.send(json!({ "filo_step": kind }).to_string());
        }
    }

    fn session(&mut self, _id: &str) {}

    fn ask(&mut self, ask: &crate::coding_session::Ask) -> String {
        ask.kind.to_string()
    }

    fn answer(&mut self, key: &str) -> Option<String> {
        Some(if key == "approval" {
            "deny".into()
        } else {
            "Nobody can answer here while you work. Ask the user in your reply (send_message) if you need to, or make the sensible choice and say which.".into()
        })
    }

    fn notes(&mut self) -> Vec<String> {
        self.steer.as_mut().map(|steer| steer.take()).unwrap_or_default()
    }

    fn noted(&mut self, note: &str) {
        if let Some(steer) = self.steer.as_mut() {
            steer.seen(note);
        }
    }

    fn stop_requested(&mut self) -> bool {
        self.stopper.is_stopped()
    }
}

/// Adds what to do about errors with a known fix.
fn explain(engine: Engine, error: &str, stderr: &[String]) -> String {
    let haystack = format!("{error}\n{}", stderr.join("\n")).to_lowercase();
    if Engine::signed_out(&haystack) && !error.contains(&engine.sign_in_hint()) {
        return format!("{error}\n\n{}", engine.sign_in_hint());
    }
    let limited = ["session limit", "usage limit", "weekly limit"];
    if limited.iter().any(|needle| haystack.contains(needle)) {
        return format!("{error}\n\n{}", limit_hint(engine));
    }
    error.to_string()
}

/// What to do about a plan's usage limit: wait, or go on with the other CLI. The session stays for when it lifts.
fn limit_hint(engine: Engine) -> String {
    let other = match engine {
        Engine::Claude => Engine::Codex,
        Engine::Codex | Engine::Muse | Engine::OpenCode => Engine::Claude,
    };
    format!(
        "That's your {} plan's limit, not Filo's: this chat picks up where it left off once it lifts. To keep going before then, use {}: for an agent, Duplicate with {} in its profile.",
        engine.label(),
        other.label(),
        other.label()
    )
}

/// Whether a failed run's words say the session it was to resume is gone (Claude Code's "No conversation found with
/// session ID", Codex's "No saved session found" and kin). A usage limit, a sign-in or the network isn't that: the
/// session is still there, and the next turn resumes it.
fn session_gone(error: &str, stderr: &[String]) -> bool {
    let haystack = format!("{error}\n{}", stderr.join("\n")).to_lowercase();
    [
        "no conversation found with session id",
        "no saved session found",
        "no rollout found",
        "thread not found",
        "session not found",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

/// Reads a CLI's JSON lines into how the run went.
pub struct EventReader {
    engine: Engine,
    outcome: RunOutcome,
}

impl EventReader {
    pub fn new(engine: Engine) -> Self {
        EventReader { engine, outcome: RunOutcome::default() }
    }

    pub fn line(&mut self, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else { return };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        let text = |value: Option<&Value>| {
            value.and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
        };
        match self.engine {
            Engine::Claude => {
                if let Some(session) = text(event.get("session_id")) {
                    self.outcome.session_id = Some(session);
                }
                if kind == "result" {
                    let is_error = event.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                        || event.get("subtype").and_then(Value::as_str).is_some_and(|subtype| subtype != "success");
                    let result = text(event.get("result"));
                    if is_error {
                        let subtype = event.get("subtype").and_then(Value::as_str).unwrap_or("error");
                        let detail = result.clone().or_else(|| {
                            event
                                .get("errors")
                                .and_then(Value::as_array)
                                .map(|errors| errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "))
                                .filter(|joined| !joined.is_empty())
                        });
                        self.outcome.error = Some(match (subtype, detail) {
                            ("error_max_turns", _) => "Claude Code reached its turn limit before finishing.".into(),
                            (_, Some(detail)) => format!("Claude Code: {detail}"),
                            (subtype, None) => format!("Claude Code ended with {subtype}."),
                        });
                    } else {
                        self.outcome.final_text = result;
                    }
                }
            }
            Engine::Codex => match kind {
                "thread.started" => {
                    self.outcome.session_id = text(event.get("thread_id")).or(self.outcome.session_id.take())
                }
                "turn.failed" => {
                    let message = text(event.pointer("/error/message")).unwrap_or_else(|| "the turn failed".into());
                    self.outcome.error = Some(format!("Codex: {message}"));
                }
                "error" => {
                    if let Some(message) = text(event.get("message")) {
                        // Codex reports reconnects as errors and carries on; only the last one counts if the turn fails.
                        self.outcome.error = Some(format!("Codex: {message}"));
                    }
                }
                "turn.completed" => self.outcome.error = None,
                "item.completed" => {
                    let item = event.get("item").cloned().unwrap_or(Value::Null);
                    if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                        self.outcome.final_text = text(item.get("text"));
                    }
                }
                _ => {}
            },
            // They run as sessions (coding_session.rs), never through here: chats are Claude Code's or Codex's.
            Engine::Muse | Engine::OpenCode => {}
        }
    }

    pub fn finish(self) -> RunOutcome {
        self.outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(engine: Engine, resume: Option<&str>) -> TurnSpec {
        TurnSpec {
            engine,
            program: PathBuf::from("/bin/cli"),
            prompt: "-hello \"there\"".into(),
            resume: resume.map(str::to_string),
            new_session_id: Some("11111111-2222-3333-4444-555555555555".into()),
            workspace: PathBuf::from("/tmp/ws"),
            mcp: McpLaunch {
                program: PathBuf::from("/Apps/Filo \"x\"/filo"),
                args: vec!["--local-mcp".into(), "--chat".into(), "c1".into()],
            },
            path_env: None,
            model: None,
            instructions: None,
            inputs: Vec::new(),
            servers: Vec::new(),
            one_off: false,
            skills: vec!["weekly-report".into()],
            writable: vec![PathBuf::from("/tmp/skills")],
            readable: vec![PathBuf::from("/tmp/chats")],
            mode: "full_access".into(),
            effort: None,
            pages: Pages::default(),
        }
    }

    #[test]
    fn pictures_go_to_the_cli_as_images() {
        let dir = crate::test_support::temp_dir("inputs");
        let picture = dir.join("a.png");
        std::fs::write(&picture, b"\x89PNG").unwrap();
        let pdf = dir.join("b.pdf");
        std::fs::write(&pdf, b"%PDF").unwrap();
        let inputs = vec![
            Input { path: picture.clone(), content_type: "image/png".into() },
            Input { path: pdf.clone(), content_type: "application/pdf".into() },
            Input { path: dir.join("gone.png"), content_type: "image/png".into() },
        ];
        let with = |engine, resume| TurnSpec { inputs: inputs.clone(), ..spec(engine, resume) };

        let claude = invocation(&with(Engine::Claude, None));
        assert!(claude.args.windows(2).any(|pair| pair == ["--input-format", "stream-json"]));
        assert!(claude.stdin.ends_with('\n') && claude.stdin.lines().count() == 2);
        let message: Value = serde_json::from_str(claude.stdin.lines().nth(1).unwrap()).unwrap();
        assert_eq!(message["type"], "user");
        let content = message["message"]["content"].as_array().unwrap();
        assert_eq!(content[0], json!({ "type": "text", "text": "-hello \"there\"" }));
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"], json!({ "type": "base64", "media_type": "image/png", "data": "iVBORw==" }));
        assert_eq!(content[2]["type"], "document");
        // The file that's gone is left out.
        assert_eq!(content.len(), 3);
        // Without files the prompt is the message's only block.
        let plain = invocation(&spec(Engine::Claude, None));
        let message: Value = serde_json::from_str(plain.stdin.lines().nth(1).unwrap()).unwrap();
        assert_eq!(message["message"]["content"], json!([{ "type": "text", "text": "-hello \"there\"" }]));

        // Codex's go with its turn (coding_session.rs), not on its command line.
        for resume in [None, Some("thread-7")] {
            let codex = invocation(&with(Engine::Codex, resume));
            assert!(!codex.args.iter().any(|arg| arg == "-i" || arg.contains("a.png")), "{:?}", codex.args);
            assert!(codex.stdin.ends_with("-hello \"there\""));
        }
    }

    #[test]
    fn claude_command_line() {
        let call = invocation(&spec(Engine::Claude, None));
        // First on stdin, the skills it may see: Filo's and the document and chart ones, nothing else.
        let first: Value = serde_json::from_str(call.stdin.lines().next().unwrap()).unwrap();
        assert_eq!(
            first,
            json!({ "type": "control_request", "request_id": "filo-init", "request": { "subtype": "initialize", "skills": ["weekly-report", "pdf", "docx", "xlsx", "pptx", "dataviz", "desktop-pet", "artifacts"] } })
        );
        assert!(call.args.windows(2).any(|pair| pair == ["--input-format", "stream-json"]));
        // A skill of Filo's named like one of those is listed once.
        let own_pdf = invocation(&TurnSpec { skills: vec!["pdf".into()], ..spec(Engine::Claude, None) });
        let first: Value = serde_json::from_str(own_pdf.stdin.lines().next().unwrap()).unwrap();
        assert_eq!(
            first["request"]["skills"],
            json!(["pdf", "docx", "xlsx", "pptx", "dataviz", "desktop-pet", "artifacts"])
        );
        let at = |flag: &str| call.args.iter().position(|arg| arg == flag).map(|index| call.args[index + 1].clone());
        assert_eq!(at("--session-id").as_deref(), Some("11111111-2222-3333-4444-555555555555"));
        // In Full access: web search, reading and writing files, tool search, skills and the shell (any command, `Bash`
        // with no pattern), allowed without asking, and nothing else gets asked about, whatever the user's settings say.
        let native = ["Read", "Grep", "Glob", "WebSearch", "WebFetch", "ToolSearch", "Skill", "Write", "Edit", "Bash"];
        assert_eq!(at("--tools").as_deref(), Some(native.join(",").as_str()));
        assert_eq!(at("--permission-mode").as_deref(), Some("dontAsk"));
        let allowed = call.args.iter().find_map(|arg| arg.strip_prefix("--allowedTools=")).unwrap();
        assert_eq!(allowed.split(',').next(), Some("mcp__filo__send_message"));
        assert!(!allowed.split(',').any(|tool| tool == "mcp__filo__use_skill"), "{allowed}");
        for tool in native {
            assert!(allowed.split(',').any(|allowed| allowed == tool), "{allowed}");
        }
        assert!(
            !allowed.split(',').any(|tool| ["MultiEdit", "NotebookEdit"].contains(&tool)
                || tool.starts_with("Bash(")
                || tool.starts_with("Edit(")),
            "{allowed}"
        );
        // One Filo server: Claude Code reads which of its tools are pinned from the tools themselves.
        assert!(
            !at("--mcp-config").unwrap().contains("filo_core")
                && !call.args.iter().any(|arg| arg.contains("code_mode"))
        );
        assert!(call.args.contains(&"--strict-mcp-config".to_string()));
        let config: Value = serde_json::from_str(&at("--mcp-config").unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["filo"]["command"], "/Apps/Filo \"x\"/filo");
        assert_eq!(config["mcpServers"]["filo"]["args"][2], "c1");

        let resumed = invocation(&spec(Engine::Claude, Some("sess-9")));
        assert!(resumed.args.windows(2).any(|pair| pair == ["--resume", "sess-9"]));
        assert!(!resumed.args.contains(&"--session-id".to_string()));
        assert!(
            resumed.args.contains(&"--strict-mcp-config".to_string())
                && resumed.args.windows(2).any(|pair| pair == ["--permission-mode", "dontAsk"])
        );

        // In Auto the same tools, but the shell isn't allowed outright: Claude Code's auto mode decides on each command.
        // The file editors are allowed in the working folder, where the pages it publishes are built.
        let auto = invocation(&TurnSpec { mode: "auto".into(), ..spec(Engine::Claude, None) });
        let at = |flag: &str| auto.args.iter().position(|arg| arg == flag).map(|index| auto.args[index + 1].clone());
        assert_eq!(at("--tools").as_deref(), Some(native.join(",").as_str()));
        assert_eq!(at("--permission-mode").as_deref(), Some("auto"));
        let allowed = auto.args.iter().find_map(|arg| arg.strip_prefix("--allowedTools=")).unwrap();
        assert!(
            !allowed.split(',').any(|tool| tool.starts_with("Bash")
                || ["Write", "Edit", "Read", "Grep", "Glob", "WebFetch"].contains(&tool)),
            "{allowed}"
        );
        // And in the user's skills' folder, where it installs and changes them. It reads there and in Filo's folders
        // without asking; anywhere else, and any web page, goes to the mode's reviewer.
        let wanted = [
            "WebSearch",
            "Skill",
            "mcp__filo__send_message",
            "Edit(//tmp/ws/**)",
            "Edit(//tmp/skills/**)",
            "Read(//tmp/ws/**)",
            "Read(//tmp/skills/**)",
            "Read(//tmp/chats/**)",
        ];
        assert!(wanted.iter().all(|tool| allowed.split(',').any(|allowed| allowed == *tool)), "{allowed}");
    }

    #[test]
    fn codex_command_line() {
        let call = invocation(&spec(Engine::Codex, None));
        // Its app-server, which a chat turn keeps talking to; the rest is configuration.
        assert_eq!(call.args[0], "app-server");
        assert!(call.args[1..].chunks(2).all(|pair| pair[0] == "-c"), "{:?}", call.args);
        // Filo's server twice: the pinned tools on `filo_core`, loaded directly, the rest on `filo`.
        for server in ["filo", "filo_core"] {
            assert!(call.args.contains(&format!(r#"mcp_servers.{server}.command="/Apps/Filo \"x\"/filo""#)));
            assert!(call.args.contains(&format!(r#"mcp_servers.{server}.default_tools_approval_mode="approve""#)));
        }
        assert!(call.args.contains(
            &r#"mcp_servers.filo_core.args=["--local-mcp", "--chat", "c1", "--tools", "pinned"]"#.to_string()
        ));
        assert!(
            call.args.contains(
                &r#"mcp_servers.filo.args=["--local-mcp", "--chat", "c1", "--tools", "deferred"]"#.to_string()
            )
        );
        assert!(
            call.args
                .windows(2)
                .any(|pair| pair == ["-c", r#"features.code_mode.direct_only_tool_namespaces=["mcp__filo_core"]"#])
        );
        // Its first message: the instructions, then the prompt.
        assert!(call.stdin.starts_with(INSTRUCTIONS));
        assert!(call.stdin.ends_with("-hello \"there\""));

        // The thread to resume goes with the session (coding_session.rs); its first message has only the reminder.
        let resumed = invocation(&spec(Engine::Codex, Some("thread-7")));
        assert_eq!(resumed.args, call.args);
        assert!(resumed.stdin.starts_with(CODEX_REMINDER));

        // New or resumed: the user's own notify program stays off, the shell runs anything with network and asks nobody
        // (Full access), and live web search is on.
        for call in [&call, &resumed] {
            let has = |flag: &str| call.args.iter().any(|arg| arg == flag);
            let set = |value: &str| call.args.windows(2).any(|pair| pair[0] == "-c" && pair[1] == value);
            assert!(set("notify=[]") && !has("--ignore-user-config"), "{:?}", call.args);
            assert!(
                set(r#"sandbox_mode="danger-full-access""#)
                    && set(r#"approval_policy="never""#)
                    && set(r#"web_search="live""#),
                "{:?}",
                call.args
            );
            assert!(!call.args.iter().any(|arg| arg.starts_with("approvals_reviewer")), "{:?}", call.args);
            assert!(
                set(r#"features.code_mode.direct_only_tool_namespaces=["mcp__filo_core"]"#)
                    && !set("features.code_mode=true"),
                "{:?}",
                call.args
            );
            assert!(!has("-s") && !has("--dangerously-bypass-approvals-and-sandbox"));
            // ChatGPT's connectors, the browser and the computer stay off; plugins too, but for the document ones
            // when this Mac has them.
            for feature in ["apps", "browser_use", "computer_use", "remote_plugin"] {
                assert!(set(&format!("features.{feature}=false")), "{feature}");
            }
            let documents = codex_documents_marketplace();
            assert_eq!(set("features.plugins=false"), documents.is_none());
            assert_eq!(call.args.iter().any(|arg| arg.starts_with("plugins={")), documents.is_some());
            // Codex's own skills off, and every other skill folder's named off.
            assert!(
                set("skills.bundled.enabled=false") && call.args.iter().any(|arg| arg.starts_with("skills.config=[")),
                "{:?}",
                call.args
            );
        }

        // In Auto, new or resumed: sandboxed to the workspace, with Codex's reviewer approving requests to go further.
        for resume in [None, Some("thread-7")] {
            let auto = invocation(&TurnSpec { mode: "auto".into(), ..spec(Engine::Codex, resume) });
            let set = |value: &str| auto.args.windows(2).any(|pair| pair[0] == "-c" && pair[1] == value);
            assert!(
                set(r#"sandbox_mode="workspace-write""#)
                    && set(r#"approval_policy="on-request""#)
                    && set(r#"approvals_reviewer="auto_review""#),
                "{:?}",
                auto.args
            );
            assert!(!set(r#"sandbox_mode="danger-full-access""#) && set(r#"web_search="live""#), "{:?}", auto.args);
            // Its sandbox writes in the user's skills' folder too.
            assert!(set(r#"sandbox_workspace_write.writable_roots=["/tmp/skills"]"#), "{:?}", auto.args);
        }
    }

    #[test]
    fn codex_sees_only_filos_skills_and_the_document_plugins() {
        let dir = crate::test_support::temp_dir("codex-skills");
        let skill = |path: &str, front: &str| {
            std::fs::create_dir_all(dir.join(path)).unwrap();
            std::fs::write(dir.join(path).join("SKILL.md"), front).unwrap();
        };
        skill("user/wrangler", "---\nname: wrangler\ndescription: Deploy\n---\nBody");
        skill("user/nested/group/deep", "---\nname: \"deep-one\"\n---\n");
        skill("legacy/weekly-report", "No front matter.");
        skill("legacy/dataviz", "---\nname: dataviz\n---\n");
        skill("legacy/artifacts", "---\nname: artifacts\n---\n");
        let found = codex_skills_on_disk(&[dir.join("user"), dir.join("legacy"), dir.join("missing")]);
        let mut names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names, ["artifacts", "dataviz", "deep-one", "weekly-report", "wrangler"]);

        let filo = vec!["weekly-report".to_string()];
        let installed = ["convex@openai-curated-remote".to_string(), "pdf@openai-primary-runtime".to_string()];
        let args = codex_skill_args(&filo, &found, Some(Path::new("/m/openai-primary-runtime")), &installed);
        let config = args.iter().find(|arg| arg.starts_with("skills.config=")).unwrap();
        // Every skill found is off by its file; by name too, but for Filo's own and dataviz, which the workspace has.
        let wrangler = dir.join("user/wrangler/SKILL.md");
        assert!(
            config.contains(&format!("{{path={},enabled=false}}", toml_string(&wrangler.display().to_string()))),
            "{config}"
        );
        assert!(
            config.contains(r#"{name="wrangler",enabled=false}"#)
                && config.contains(r#"{name="deep-one",enabled=false}"#),
            "{config}"
        );
        assert!(
            !config.contains(r#"{name="weekly-report""#)
                && !config.contains(r#"{name="dataviz""#)
                && !config.contains(r#"{name="artifacts""#),
            "{config}"
        );
        assert!(config.contains(r#"{name="spreadsheets:excel-live-control",enabled=false}"#), "{config}");
        assert_eq!(args[..2], ["-c", "skills.bundled.enabled=false"]);
        assert!(
            args.contains(
                &r#"marketplaces={openai-primary-runtime={source_type="local",source="/m/openai-primary-runtime"}}"#
                    .to_string()
            ),
            "{args:?}"
        );
        assert!(args.contains(
            &r#"plugins={"pdf@openai-primary-runtime"={enabled=true},"documents@openai-primary-runtime"={enabled=true},"spreadsheets@openai-primary-runtime"={enabled=true},"presentations@openai-primary-runtime"={enabled=true},"convex@openai-curated-remote"={enabled=false}}"#.to_string()
        ));
        // Without the marketplace, no plugins.
        assert!(
            !codex_skill_args(&filo, &found, None, &installed)
                .iter()
                .any(|arg| arg.starts_with("plugins=") || arg.starts_with("marketplaces="))
        );

        let config = "model = \"x\"\n[marketplaces.openai-bundled]\nsource = \"/b\"\n[marketplaces.openai-primary-runtime]\nsource_type = \"local\"\nsource = \"/p\"\n";
        assert_eq!(codex_table_value(config, "marketplaces.openai-primary-runtime", "source").as_deref(), Some("/p"));
        assert_eq!(codex_table_value(config, "marketplaces.nope", "source"), None);
    }

    #[test]
    fn instructions_say_what_the_turn_can_use() {
        let app = McpServer { name: "filo-linear".into(), program: PathBuf::from("/x"), args: Vec::new() };
        let claude = capabilities(Engine::Claude, "full_access", &[], None, Pages::default());
        assert!(
            claude.contains("WebSearch, WebFetch")
                && claude.contains("Read, Grep, Glob")
                && claude.contains("No other apps"),
            "{claude}"
        );
        assert!(
            claude.contains("(Bash) that runs any command, with network")
                && claude.contains("(Skill)")
                && claude.contains("`filo` MCP server"),
            "{claude}"
        );
        assert!(
            claude.contains("load when you search for them with tool search")
                && claude.contains("ask before deleting")
                && claude.contains("call show_glance before your reply")
        );
        assert!(
            claude.contains("generate_images") && claude.contains("a traced or redrawn version, never Codex-drawn"),
            "{claude}"
        );
        assert!(
            claude.contains("build it as an HTML file with the artifacts skill")
                && claude.contains("publish_artifact")
                && claude.contains("once its round has finished"),
            "{claude}"
        );
        assert!(
            claude.contains("(Read, Grep, Glob) and writing them (Write, Edit)")
                && claude.contains("making artifacts (artifacts)"),
            "{claude}"
        );
        // What should outlive the turn is started detached, with a log, and the user hears how to stop it.
        assert!(
            claude.contains("Your turn ends as soon as you've replied")
                && claude.contains("nohup npm run dev > /tmp/<name>.log 2>&1 < /dev/null &"),
            "{claude}"
        );
        assert!(claude.contains("where its log is and how to stop it"), "{claude}");
        // Work it says comes next is done now, handed off or scheduled: nothing wakes it after its reply otherwise.
        assert!(
            claude.contains("nothing wakes you after that unless")
                && claude.contains("start_work")
                && claude.contains("schedule_check_in"),
            "{claude}"
        );
        // An open-ended request that a guess would cost a lot on: a short question first; a clear one: just done.
        assert!(claude.contains("a short question about what's bothering them first saves their time; when the request is clear, or quick to redo, just do it."), "{claude}");
        assert!(
            claude.contains("offer it as a question")
                && claude.contains("Commands still running in your shell stop with the turn too"),
            "{claude}"
        );
        let codex = capabilities(
            Engine::Codex,
            "full_access",
            std::slice::from_ref(&app),
            Some("The user's computer is on for you."),
            Pages::default(),
        );
        assert!(
            codex.contains("a shell that runs any command on this Mac, with network")
                && codex.contains("web search")
                && codex.contains("`filo-linear`")
                && !codex.contains("WebFetch"),
            "{codex}"
        );
        assert!(
            codex.contains("the `filo_core` and `filo` MCP servers")
                && codex.contains("there when you look for them")
                && !codex.contains("tool search"),
            "{codex}"
        );
        assert!(
            codex.contains("`filo-linear`. The user's computer is on for you. Filo's main tools")
                && !claude.contains("computer"),
            "{codex}"
        );
        // The browser, on every engine.
        for engine in [Engine::Claude, Engine::Codex, Engine::Muse, Engine::OpenCode] {
            let text = capabilities(engine, "auto", &[], None, Pages { feed: false, notes: false });
            assert!(
                text.contains("Filo's browser is always yours")
                    && text.contains("`browser_open`")
                    && text.contains("`browser_drag`")
                    && text.contains("artifacts, the browser)"),
                "{text}"
            );
        }
        let full = with_capabilities("You are Pip.\n", Engine::Claude, "full_access", &[], None, Pages::default());
        assert!(full.starts_with("You are Pip.\n\nWhat you can use here:"));
        // In Auto they don't promise a shell that runs anything.
        let claude = capabilities(Engine::Claude, "auto", &[], None, Pages::default());
        assert!(
            claude.contains("a shell (Bash), with network, whose commands a reviewer approves")
                && !claude.contains("runs any command"),
            "{claude}"
        );
        assert!(
            claude.contains("writing them in your working folder and the user's skills folder (Write, Edit)"),
            "{claude}"
        );
        let codex = capabilities(Engine::Codex, "auto", &[], None, Pages::default());
        assert!(
            codex.contains("sandboxed to your working folder without network") && !codex.contains("runs any command"),
            "{codex}"
        );
    }

    #[test]
    fn an_off_page_leaves_the_instructions_and_the_allowed_tools() {
        let on = capabilities(Engine::Claude, "auto", &[], None, Pages::default());
        assert!(
            on.contains(
                "(reminders, routines, check-ins, finding and changing notes, artifacts, the browser, the Feed)"
            ) && on.contains("with create_note"),
            "{on}"
        );
        let off = capabilities(Engine::Claude, "auto", &[], None, Pages { feed: false, notes: false });
        assert!(
            off.contains("(reminders, routines, check-ins, artifacts, the browser)")
                && !off.contains("create_note")
                && !off.contains("the Feed"),
            "{off}"
        );
        // Not dropped silently, or a note asked for would be kept as a memory.
        assert!(
            off.contains(
                "Notes is off. If the user asks for a note, say so and that it can be turned on in Settings › Pages."
            ),
            "{off}"
        );
        let feed_only = capabilities(Engine::Codex, "auto", &[], None, Pages { feed: true, notes: false });
        assert!(
            feed_only.contains("(reminders, routines, check-ins, artifacts, the browser, the Feed)"),
            "{feed_only}"
        );

        let allowed = |pages| {
            invocation(&TurnSpec { pages, ..spec(Engine::Claude, None) })
                .args
                .iter()
                .find(|arg| arg.contains("mcp__filo__send_message"))
                .cloned()
                .unwrap()
        };
        assert!(
            allowed(Pages::default()).contains("mcp__filo__create_note,")
                && allowed(Pages::default()).contains("mcp__filo__publish_feed_post,")
        );
        let without_notes = allowed(Pages { feed: true, notes: false });
        assert!(
            !without_notes.contains("note")
                && without_notes.contains("mcp__filo__publish_feed_post,")
                && without_notes.contains("mcp__filo__show_glance")
        );
    }

    #[test]
    fn fingerprints_tell_changed_instructions() {
        assert_eq!(fingerprint("You are Pip."), fingerprint("You are Pip."));
        assert_ne!(fingerprint("You are Pip."), fingerprint("You are Pip!"));
        assert_eq!(fingerprint("").len(), 16);
        // Stable across builds: it's saved with the session.
        assert_eq!(fingerprint("a"), "af63dc4c8601ec8c");
        let updated = updated_instructions("  Be brief.  ");
        assert!(updated.contains("have changed") && updated.contains("\n\nBe brief.\n\n"));
    }

    #[test]
    fn only_a_missing_session_counts_as_gone() {
        assert!(session_gone("Claude Code stopped with status 1: No conversation found with session ID: s-1", &[]));
        assert!(session_gone("Codex stopped with status 1.", &["Error: No saved session found with ID th-1".into()]));
        assert!(!session_gone("Claude Code: You've hit your session limit · resets 3:50am (Asia/Tashkent)", &[]));
        assert!(!session_gone("Codex: usage limit reached", &[]));
        assert!(!session_gone("Claude Code: Invalid API key · Please run /login", &[]));
    }

    #[test]
    fn a_usage_limit_says_what_to_do() {
        let limited =
            explain(Engine::Claude, "Claude Code: You've hit your session limit · resets 3:50am (Asia/Tashkent)", &[]);
        assert!(
            limited.starts_with("Claude Code: You've hit your session limit · resets 3:50am")
                && limited.contains("Duplicate with Codex"),
            "{limited}"
        );
        assert!(explain(Engine::Codex, "Codex: usage limit reached", &[]).contains("Duplicate with Claude Code"));
        assert_eq!(
            explain(Engine::Codex, "Codex: workspace routing discovery timed out", &[]),
            "Codex: workspace routing discovery timed out"
        );
    }

    #[test]
    fn the_users_own_codex_servers_and_notify_program_stay_off() {
        // However the file sets them up: tables, a quoted name with a dot, dotted keys, an inline table.
        let config = "model = \"gpt-a\"\nnotify = [\"/x/Notify.app\"]\nmcp_servers.dotted.command = \"cat\"\nmcp_servers.inline = { command = \"ls\", env = { A = \"1\" } }\n[mcp_servers.context7]\ncommand = \"npx\"\n[mcp_servers.node_repl.env]\nA = \"1\"\n[mcp_servers.\"odd.name\"]\nurl = \"https://y\"\n[mcp_servers.filo]\nurl = \"https://mine\"\nenabled = false\n";
        let mut servers = codex_user_servers(config);
        servers.sort();
        assert_eq!(servers, ["context7", "dotted", "filo", "inline", "node_repl", "odd.name"]);
        assert_eq!(
            codex_user_config_off(&servers),
            [
                "-c",
                r#"mcp_servers={ "context7" = { enabled = false }, "dotted" = { enabled = false }, "filo" = { enabled = false }, "inline" = { enabled = false }, "node_repl" = { enabled = false }, "odd.name" = { enabled = false } }"#,
                "-c",
                "notify=[]"
            ]
        );
        // What it says parses as the table it means.
        let table: toml::Table =
            codex_user_config_off(&servers)[1].replacen("mcp_servers=", "mcp_servers = ", 1).parse().unwrap();
        assert_eq!(table["mcp_servers"]["odd.name"]["enabled"].as_bool(), Some(false));
        assert_eq!(codex_user_config_off(&[]), ["-c", "notify=[]"]);
        assert!(codex_user_servers("not = [toml").is_empty());

        // A server of the chat's own the user has one of the same name of goes by another, which takes over all it
        // says (`filo` here: the user's `filo_2` too).
        let taken = vec!["filo".to_string(), "filo_2".to_string(), "filo-linear".to_string()];
        assert_eq!(
            (
                codex_own_name("filo", &taken),
                codex_own_name("filo_core", &taken),
                codex_own_name("filo-linear", &taken)
            ),
            ("filo_3".into(), "filo_core".into(), "filo-linear_2".into())
        );
        let app = McpServer { name: "filo-linear".into(), program: "/x/filo".into(), args: vec!["--app".into()] };
        let args = codex_server_args(&[app], &taken);
        assert!(
            args.contains(&r#"mcp_servers.filo-linear_2.command="/x/filo""#.to_string())
                && args.contains(&"mcp_servers.filo-linear_2.enabled=true".to_string()),
            "{args:?}"
        );
    }

    #[test]
    fn claude_rows_read_as_the_models_claude_code_names() {
        let model = |id: &str, label: &str| Model { id: id.into(), label: label.into() };
        // Before Claude Code has said anything: its aliases, no default.
        let rows: Vec<(String, String, bool)> =
            claude_options(None, &[]).into_iter().map(|row| (row.id, row.label, row.default)).collect();
        assert_eq!(rows[1], ("opus".to_string(), "Opus".to_string(), false));
        assert_eq!(rows.len(), 4);
        // Its default takes the place of the alias naming the same model.
        let resolved = vec![
            ("fable".to_string(), model("claude-fable-5-1", "Fable 5.1")),
            ("opus".to_string(), model("claude-opus-5-5", "Opus 5.5")),
        ];
        let rows = claude_options(Some(&model("claude-opus-5-5", "Opus 5.5")), &resolved);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["fable", "claude-opus-5-5", "sonnet", "haiku"]);
        assert!(rows[1].default && rows[1].label == "Opus 5.5" && rows[1].matches("opus"));
        assert!(rows[0].label == "Fable 5.1" && rows[0].matches("claude-fable-5-1") && !rows[0].default);
        assert_eq!(rows.iter().filter(|row| row.default).count(), 1);
        // A default no alias names (pinned in settings) goes first.
        let rows = claude_options(Some(&model("claude-opus-4-8", "Opus 4.8")), &resolved);
        assert_eq!((rows[0].id.as_str(), rows[0].default, rows.len()), ("claude-opus-4-8", true, 5));
    }

    #[test]
    fn claude_codes_answer_to_slash_model() {
        assert_eq!(
            current_model_label("Current model: `Opus 5.5 (default)` (effort: high)\nUsage: /model <name>.").as_deref(),
            Some("Opus 5.5")
        );
        assert_eq!(current_model_label("Current model: `Sonnet 5.5`").as_deref(), Some("Sonnet 5.5"));
        assert_eq!(current_model_label("/model isn't available in this environment."), None);
    }

    #[test]
    fn codex_rows_and_its_default() {
        let catalogue = br#"{"models":[
            {"slug":"gpt-b","display_name":"GPT-B","visibility":"list","priority":2},
            {"slug":"hidden","display_name":"Hidden","visibility":"hide","priority":0},
            {"slug":"gpt-a","display_name":"","visibility":"list","priority":1},
            {"slug":"--evil","visibility":"list","priority":3}
        ]}"#;
        let rows = |default| {
            codex_options_from(catalogue, default)
                .into_iter()
                .map(|row| (row.id, row.label, row.default))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rows(None),
            [("gpt-a".to_string(), "gpt-a".to_string(), false), ("gpt-b".into(), "GPT-B".into(), false)]
        );
        assert_eq!(rows(Some("gpt-b"))[1], ("gpt-b".to_string(), "GPT-B".to_string(), true));
        // A default the picker doesn't list goes first, named as the catalogue names it.
        assert_eq!(rows(Some("hidden"))[0], ("hidden".to_string(), "Hidden".to_string(), true));
        assert_eq!(rows(Some("gpt-z"))[0], ("gpt-z".to_string(), "gpt-z".to_string(), true));
        assert!(codex_options_from(b"not json", None).is_empty());
        assert!(valid_model("claude-opus-5-5[1m]") && !valid_model("") && !valid_model("-x") && !valid_model("a b"));
    }

    #[test]
    fn codex_config_model_and_profiles() {
        assert_eq!(
            codex_config_model(
                "model = \"gpt-6.1-sol\" # mine\nmodel_reasoning_effort = \"high\"\n[mcp_servers.x]\nmodel = \"other\""
            )
            .as_deref(),
            Some("gpt-6.1-sol")
        );
        let profiled = "model = 'gpt-a'\nprofile = \"fast\"\n[profiles.slow]\nmodel = \"gpt-s\"\n[profiles.fast]\nmodel = \"gpt-f\"\n";
        assert_eq!(codex_config_model(profiled).as_deref(), Some("gpt-f"));
        assert_eq!(codex_config_model("[profiles.fast]\nmodel = \"gpt-f\"\n"), None);
        assert_eq!(codex_config_model("approvals_reviewer = \"user\""), None);
    }

    #[test]
    fn the_model_goes_on_the_command_line() {
        let with_model = |engine, resume| TurnSpec { model: Some("m-1".into()), ..spec(engine, resume) };
        assert!(
            invocation(&with_model(Engine::Claude, Some("s"))).args.windows(2).any(|pair| pair == ["--model", "m-1"])
        );
        let codex = invocation(&with_model(Engine::Codex, Some("thread-7")));
        assert!(codex.args.windows(2).any(|pair| pair == ["-c", "model=\"m-1\""]));
        assert!(!invocation(&spec(Engine::Codex, None)).args.iter().any(|arg| arg.starts_with("model=")));
        assert!(!invocation(&spec(Engine::Claude, None)).args.contains(&"--model".to_string()));
    }

    #[test]
    fn the_chats_reasoning_level_goes_on_the_command_line() {
        let with_effort = |engine, resume| TurnSpec { effort: Some("xhigh".into()), ..spec(engine, resume) };
        assert!(
            invocation(&with_effort(Engine::Claude, Some("s")))
                .args
                .windows(2)
                .any(|pair| pair == ["--effort", "xhigh"])
        );
        assert!(!invocation(&spec(Engine::Claude, None)).args.contains(&"--effort".to_string()));
        // Codex takes it over the level in the user's own config.
        let codex = invocation(&with_effort(Engine::Codex, None));
        let levels: Vec<&String> = codex.args.iter().filter(|arg| arg.starts_with("model_reasoning_effort=")).collect();
        assert_eq!(levels, ["model_reasoning_effort=\"xhigh\""]);
    }

    #[test]
    fn claude_events() {
        let mut reader = EventReader::new(Engine::Claude);
        reader.line(r#"{"type":"system","subtype":"init","session_id":"s-1","tools":[]}"#);
        reader.line("garbage");
        reader.line(r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1"}"#);
        let outcome = reader.finish();
        assert_eq!(outcome.session_id.as_deref(), Some("s-1"));
        assert_eq!(outcome.final_text.as_deref(), Some("done"));
        assert_eq!(outcome.error, None);

        let mut reader = EventReader::new(Engine::Claude);
        reader.line(r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login","session_id":"s-2"}"#);
        let outcome = reader.finish();
        let error = explain(Engine::Claude, outcome.error.as_deref().unwrap(), &[]);
        assert!(error.starts_with("Claude Code: Invalid API key"));
        assert!(error.ends_with("Run `claude auth login` in Terminal, then try again."), "{error}");
    }

    #[test]
    fn endless_watchers_are_told_from_work_that_ends() {
        for endless in [
            "tail -n 0 -f /tmp/round-1/dev-restart.log",
            "tail -n +2 -f run.log | grep --line-buffered -E 'rebuilding|FAILED|done|head'",
            "tail -F app.log",
            "cd /w && nohup tail --follow=name x.log",
            "/usr/bin/log stream --predicate 'process == \"filo\"'",
            "journalctl -f -u filo",
            "kubectl logs -f pod/web",
            "watch -n 5 ls",
            "sleep infinity",
        ] {
            assert!(endless_watcher(endless), "{endless}");
        }
        for ends in [
            "cargo build",
            "scripts/app.sh",
            "until grep -qE ' done$' run.log; do sleep 2; done",
            "tail -f run.log | grep -m1 ready",
            "tail -f run.log | grep -q ready",
            "timeout 600 tail -f run.log",
            "tail -f run.log | head -n 5",
            "tail --pid=42 -f run.log",
            "tail -f run.log | while read line; do case $line in *done*) break;; esac; done",
            "tail -n 50 run.log",
            "grep -e 'tail -f' notes.md",
            "sleep 30",
        ] {
            assert!(!endless_watcher(ends), "{ends}");
        }
    }

    #[test]
    fn a_hung_turn_says_how_long_it_was_quiet() {
        let said = |quiet: u64| hung_error(Engine::Codex, Duration::from_secs(quiet));
        assert_eq!(said(30 * 60), "Codex seemed stuck: nothing came from it for 30 minutes, so it was stopped.");
        assert!(
            said(3 * 60 * 60).contains("for 3 hours,")
                && said(60 * 60).contains("for 1 hour,")
                && said(90 * 60).contains("for 90 minutes,")
        );
    }

    #[test]
    fn codex_events() {
        let mut reader = EventReader::new(Engine::Codex);
        reader.line(r#"{"type":"thread.started","thread_id":"th-1"}"#);
        reader.line(r#"{"type":"error","message":"Reconnecting... 1/5"}"#);
        reader.line(r#"{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"hi"}}"#);
        reader.line(r#"{"type":"turn.completed","usage":{}}"#);
        let outcome = reader.finish();
        assert_eq!(outcome.session_id.as_deref(), Some("th-1"));
        assert_eq!(outcome.final_text.as_deref(), Some("hi"));
        assert_eq!(outcome.error, None);

        let mut reader = EventReader::new(Engine::Codex);
        reader.line(r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#);
        assert_eq!(reader.finish().error.as_deref(), Some("Codex: usage limit reached"));
    }

    /// A Claude Code chat turn ends at its result, with a dev server it left in the background still running: its stdin
    /// closes once the message is written, and a fake that, like Claude Code, goes on while its stdin is open (and
    /// exits at once when it's closed) is over at its first result. It runs with Claude Code's background tasks off.
    #[test]
    fn a_claude_chat_turn_ends_at_its_result_whatever_runs_in_the_background() {
        let dir = crate::test_support::temp_dir("local-engine-background");
        let fake = dir.join("claude.sh");
        std::fs::write(
            &fake,
            r#"#!/bin/bash
read -r init
read -r message
echo '{"type":"system","subtype":"init","session_id":"fake-1"}'
echo '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","task_type":"local_bash","description":"Start the dev server"}]}'
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Serving. Background tasks off: $CLAUDE_CODE_DISABLE_BACKGROUND_TASKS\"}"
read -r -t 30 more
[ $? -gt 128 ] || exit 0
echo '{"type":"system","subtype":"init","session_id":"fake-1"}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Still waiting on the dev server."}'
"#,
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let spec = TurnSpec {
            program: fake,
            workspace: dir.to_path_buf(),
            prompt: "start the dev server".into(),
            ..spec(Engine::Claude, None)
        };
        let began = Instant::now();
        let outcome = start(&spec, Duration::from_secs(20)).unwrap().wait();
        assert_eq!(
            (outcome.final_text.as_deref(), outcome.error.as_deref(), outcome.timed_out),
            (Some("Serving. Background tasks off: 1"), None, false),
            "{outcome:?}"
        );
        // Well under the 30 s the background work would take, with room for a busy machine.
        assert!(began.elapsed() < Duration::from_secs(15), "{:?}", began.elapsed());
    }

    #[test]
    fn running_a_fake_cli_and_stopping_it() {
        let dir = crate::test_support::temp_dir("local-engine");
        // Echoes a session and a result like Claude Code, after reading the skills request and then the prompt.
        let ok = dir.join("ok.sh");
        std::fs::write(
            &ok,
            r#"#!/bin/sh
read -r init
read -r message
case "$init" in *'"subtype":"initialize"'*) ;; *) message=;; esac
case "$message" in *'"text":"ping\n"'*) said=ping;; *) said=other;; esac
echo '{"type":"system","subtype":"init","session_id":"fake-1"}'
echo '{"type":"control_response","response":{"subtype":"success","request_id":"filo-init"}}'
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"$said\"}"
"#,
        )
        .unwrap();
        let slow = dir.join("slow.sh");
        std::fs::write(&slow, "#!/bin/sh\nsleep 30\n").unwrap();
        // Busy for 4 seconds, saying so every 0.2, then done.
        let busy = dir.join("busy.sh");
        std::fs::write(&busy, "#!/bin/sh\nfor i in $(seq 20); do echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fake-1\"}'; sleep 0.2; done\necho '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n").unwrap();
        let fails = dir.join("fails.sh");
        std::fs::write(&fails, "#!/bin/sh\necho 'Error: not logged in' >&2\nexit 3\n").unwrap();
        // Answers with how long Claude Code would keep its prompt cache.
        let ttl = dir.join("ttl.sh");
        std::fs::write(&ttl, "#!/bin/sh\nread prompt\necho \"{\\\"type\\\":\\\"result\\\",\\\"subtype\\\":\\\"success\\\",\\\"is_error\\\":false,\\\"result\\\":\\\"ttl=$CLAUDE_CODE_PROMPT_CACHE_TTL\\\"}\"\n").unwrap();
        for script in [&ok, &slow, &busy, &fails, &ttl] {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let base = |program: &Path| TurnSpec {
            program: program.to_path_buf(),
            workspace: dir.to_path_buf(),
            prompt: "ping\n".into(),
            ..spec(Engine::Claude, None)
        };

        let outcome = start(&base(&ok), Duration::from_secs(20)).unwrap().wait();
        assert_eq!(outcome.session_id.as_deref(), Some("fake-1"));
        assert_eq!(outcome.final_text.as_deref(), Some("ping"));
        assert_eq!(outcome.error, None);

        // A one-off session's CLI gets the 5-minute cache; a chat's keeps Claude Code's own choice.
        let one_off = TurnSpec { one_off: true, ..base(&ttl) };
        assert_eq!(start(&one_off, Duration::from_secs(20)).unwrap().wait().final_text.as_deref(), Some("ttl=5m"));
        if std::env::var_os("CLAUDE_CODE_PROMPT_CACHE_TTL").is_none() {
            assert_eq!(start(&base(&ttl), Duration::from_secs(20)).unwrap().wait().final_text.as_deref(), Some("ttl="));
        }
        assert!(invocation(&TurnSpec { one_off: true, ..spec(Engine::Codex, None) }).env.is_empty());

        let outcome = start(&base(&fails), Duration::from_secs(20)).unwrap().wait();
        let error = outcome.error.unwrap();
        assert!(
            error.contains("status 3") && error.contains("not logged in") && error.contains("`claude auth login`"),
            "{error}"
        );

        let running = start(&base(&slow), Duration::from_secs(20)).unwrap();
        let stopper = running.stopper();
        let began = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            stopper.stop();
        });
        let outcome = running.wait();
        assert!(outcome.stopped && outcome.error.is_none(), "{outcome:?}");
        assert!(began.elapsed() < Duration::from_secs(10));

        // A chat turn goes on for as long as it's busy, and one that goes quiet has hung. 1.5 s without a line, where
        // busy.sh says something every 0.2 s: a busy machine can be slow to start each `sleep`.
        let outcome = start(&base(&busy), Duration::from_millis(1500)).unwrap().wait();
        assert_eq!(
            (outcome.final_text.as_deref(), outcome.error.as_deref(), outcome.timed_out),
            (Some("done"), None, false),
            "{outcome:?}"
        );
        let began = std::time::Instant::now();
        let outcome = start(&base(&slow), Duration::from_secs(1)).unwrap().wait();
        assert!(outcome.timed_out && !outcome.stopped, "{outcome:?}");
        assert_eq!(
            outcome.error.as_deref(),
            Some("Claude Code seemed stuck: nothing came from it for 1 second, so it was stopped.")
        );
        assert!(began.elapsed() < Duration::from_secs(10));
        // A coding task's command line has its time in all, busy or not.
        let outcome =
            start_invocation(&invocation(&base(&busy)), Engine::Claude, &dir, None, Duration::from_millis(800))
                .unwrap()
                .wait();
        assert!(
            outcome.timed_out && outcome.error.as_deref().is_some_and(|error| error.contains("took longer than")),
            "{outcome:?}"
        );

        assert!(start(&base(&dir.join("missing")), Duration::from_secs(1)).is_err());
    }

    #[test]
    fn the_users_apps_go_to_both_clis_as_more_servers() {
        let app = McpServer {
            name: "filo-linear".into(),
            program: PathBuf::from("/Apps/Filo/filo"),
            args: vec!["--local-mcp".into(), "--app".into(), "a1".into()],
        };
        let with_app = |engine| TurnSpec { servers: vec![app.clone()], ..spec(engine, None) };
        let claude = invocation(&with_app(Engine::Claude));
        let config: Value =
            serde_json::from_str(&claude.args[claude.args.iter().position(|arg| arg == "--mcp-config").unwrap() + 1])
                .unwrap();
        assert_eq!(
            config["mcpServers"]["filo-linear"],
            json!({ "type": "stdio", "command": "/Apps/Filo/filo", "args": ["--local-mcp", "--app", "a1"] })
        );
        assert!(config["mcpServers"]["filo"].is_object());
        let allowed = claude.args.iter().find(|arg| arg.starts_with("--allowedTools=")).unwrap();
        assert!(
            allowed.split(',').any(|tool| tool == "mcp__filo-linear") && allowed.contains("mcp__filo__send_message")
        );
        let codex = invocation(&with_app(Engine::Codex));
        let has = |value: &str| codex.args.iter().any(|arg| arg == value);
        assert!(has(r#"mcp_servers.filo-linear.command="/Apps/Filo/filo""#));
        assert!(has(r#"mcp_servers.filo-linear.args=["--local-mcp", "--app", "a1"]"#));
        assert!(has(r#"mcp_servers.filo-linear.default_tools_approval_mode="approve""#));
        // Without apps, the command lines are as before.
        assert!(!invocation(&spec(Engine::Codex, None)).args.iter().any(|arg| arg.contains("filo-linear")));
    }
}

// Report a problem (docs/problem-reports.md): checks run on this Mac, then what went wrong, sent to Filo's backend
// at `ENDPOINT` as one versioned JSON body. Opened from a chat's failed reply, a coding task that failed, or Settings ›
// Help (screens/report.rs, which shows the checks, a one-line preview and Send).
//
// The checks: Filo's build, macOS, the chip, the coding tools (where each is and its version), whether the engine
// in question is signed in and answers a tiny test turn, free disk and the network. A red check with a known fix
// offers one (`Fix`): update an old Claude Code, sign in, install, free up disk.
//
// What goes with them: the failed turn's or round's exact error with the CLI's last lines on stderr (filo-local
// failures.rs keeps those), the recent failures, and the end of the workers' log. Never chat text: no messages, and
// what a CLI wrote instead of replying is cut from errors. Paths under the home folder, other users' folders, keys,
// tokens and email addresses are hidden (`redact`).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use filo_local::engine::{self, Engine};
use filo_local::failures::{self, Failure};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};

use crate::local_backend::{self, ToolStatus};

pub const ENDPOINT: &str = "https://dev.filo.im/desktop/reports";
/// The body's version: a change to its fields is a new one (docs/problem-reports.md).
pub const SCHEMA: u32 = 1;
/// Claude Code older than this lacks what Filo's turns use (stream-json input with skills, `auth status`).
pub const CLAUDE_MIN: &str = "2.1.286";

const SEND_TIMEOUT: Duration = Duration::from_secs(20);
const TEST_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);
const LOG_LINES: usize = 60;
const RECENT_FAILURES: usize = 5;
/// No line or error goes longer.
const LINE_MAX: usize = 600;
/// Below this much free disk, Claude Code and Codex can't save their sessions.
const DISK_LOW: u64 = 2 * 1024 * 1024 * 1024;
const DISK_TIGHT: u64 = 10 * 1024 * 1024 * 1024;

/// Where the report was opened from.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Settings,
    /// A chat's failed reply: its turn, and the error the chat shows.
    Turn {
        turn_id: String,
        error: Option<String>,
    },
    /// A coding task that failed.
    Task {
        task_id: String,
        error: Option<String>,
    },
}

impl Origin {
    fn source(&self) -> &'static str {
        match self {
            Origin::Settings => "settings",
            Origin::Turn { .. } => "chat",
            Origin::Task { .. } => "task",
        }
    }

    fn id(&self) -> Option<&str> {
        match self {
            Origin::Settings => None,
            Origin::Turn { turn_id: id, .. } | Origin::Task { task_id: id, .. } => Some(id),
        }
    }

    fn shown_error(&self) -> Option<&str> {
        match self {
            Origin::Settings => None,
            Origin::Turn { error, .. } | Origin::Task { error, .. } => error.as_deref(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    /// Not needed here (Codex when Claude Code answers and Codex isn't installed).
    Skip,
}

/// What a red check's one-click fix does.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    /// `claude update` in Terminal.
    UpdateClaude(PathBuf),
    /// `claude auth login` / `codex login` in Terminal.
    SignIn(Engine, PathBuf),
    /// The official install command in Terminal.
    Install(Engine),
    /// System Settings › Storage.
    FreeDisk,
}

impl Fix {
    pub fn label(&self) -> &'static str {
        match self {
            Fix::UpdateClaude(_) => "Update",
            Fix::SignIn(..) => "Sign in…",
            Fix::Install(_) => "Install",
            Fix::FreeDisk => "Free up space",
        }
    }

    /// The fix's name in the report.
    fn id(&self) -> &'static str {
        match self {
            Fix::UpdateClaude(_) => "update-claude",
            Fix::SignIn(Engine::Claude, _) => "sign-in-claude",
            Fix::SignIn(Engine::Codex, _) => "sign-in-codex",
            Fix::Install(Engine::Claude) => "install-claude",
            Fix::Install(Engine::Codex) => "install-codex",
            Fix::SignIn(Engine::Muse, _) => "sign-in-muse",
            Fix::SignIn(Engine::OpenCode, _) => "sign-in-opencode",
            Fix::Install(Engine::Muse) => "install-muse",
            Fix::Install(Engine::OpenCode) => "install-opencode",
            Fix::SignIn(Engine::Pi, _) => "sign-in-pi",
            Fix::Install(Engine::Pi) => "install-pi",
            Fix::FreeDisk => "free-disk",
        }
    }

    /// Runs it: a Terminal window with the command, or System Settings.
    pub fn run(&self) -> Result<(), String> {
        use crate::screens::welcome::{install_command, run_in_terminal, shell_quote};
        let program = |path: &Path| shell_quote(&path.display().to_string());
        match self {
            Fix::UpdateClaude(path) => {
                run_in_terminal("update-claude", "Updating Claude Code", &format!("{} update", program(path)))
            }
            Fix::SignIn(engine, path) => {
                let args = engine.sign_in_args();
                run_in_terminal(
                    &format!("sign-in-{}", engine.program()),
                    &format!("Signing in to {}", engine.label()),
                    &format!("{} {args}", program(path)),
                )
            }
            Fix::Install(engine) => run_in_terminal(
                &format!("install-{}", engine.program()),
                &format!("Installing {}", engine.label()),
                install_command(*engine),
            ),
            Fix::FreeDisk => {
                let status = Command::new("/usr/bin/open")
                    .arg("x-apple.systempreferences:com.apple.settings.Storage")
                    .status()
                    .map_err(|error| error.to_string())?;
                if status.success() { Ok(()) } else { Err(format!("open exited with {status}")) }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub id: &'static str,
    pub label: String,
    pub status: Status,
    pub detail: String,
    pub fix: Option<Fix>,
}

impl Check {
    fn new(id: &'static str, label: impl Into<String>, status: Status, detail: impl Into<String>) -> Check {
        Check { id, label: label.into(), status, detail: detail.into(), fix: None }
    }

    fn fix(mut self, fix: Fix) -> Check {
        self.fix = Some(fix);
        self
    }
}

/// The checks, in the order they show; each runs on its own (`run`), the slow ones (the test reply, the network)
/// alongside the rest.
pub const CHECKS: [(&str, &str); 12] = [
    ("filo", "Filo"),
    ("macos", "macOS"),
    ("chip", "Chip"),
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("muse", "Muse Code"),
    ("opencode", "OpenCode"),
    ("pi", "pi"),
    ("sign-in", "Signed in"),
    ("test-reply", "Test reply"),
    ("disk", "Disk space"),
    ("network", "Network"),
];

/// The engine the report is about: the failure's, else the one that answers by default.
pub fn engine_for(origin: &Origin) -> Engine {
    origin
        .id()
        .and_then(|id| failures::find(&local_dir(), id))
        .and_then(|failure| failure.engine)
        .unwrap_or_else(local_backend::default_engine)
}

fn local_dir() -> PathBuf {
    crate::config::config_dir().join("local")
}

/// Runs check `id` for `engine`. Blocks (seconds for the CLIs, up to a minute for the test reply).
pub fn run(id: &str, engine: Engine) -> Check {
    let label = CHECKS.iter().find(|(check, _)| *check == id).map_or(id, |(_, label)| label);
    let id: &'static str = CHECKS.iter().find(|(check, _)| *check == id).map_or("unknown", |(check, _)| check);
    match id {
        "filo" => Check::new(id, label, Status::Ok, crate::updater::version_label()),
        "macos" => Check::new(id, label, Status::Ok, macos_version().unwrap_or_else(|| "Unknown".into())),
        "chip" => match (std::env::consts::ARCH, rosetta()) {
            (_, true) => Check::new(
                id,
                label,
                Status::Warn,
                "Apple silicon, with Filo for Intel under Rosetta: download the Apple silicon Filo",
            ),
            ("aarch64", false) => Check::new(id, label, Status::Ok, "Apple silicon (aarch64)"),
            (arch, false) => Check::new(id, label, Status::Ok, format!("Intel ({arch})")),
        },
        "claude" => cli_check(Engine::Claude, engine),
        "codex" => cli_check(Engine::Codex, engine),
        "muse" => cli_check(Engine::Muse, engine),
        "opencode" => cli_check(Engine::OpenCode, engine),
        "pi" => cli_check(Engine::Pi, engine),
        "sign-in" => sign_in_check(engine),
        "test-reply" => test_reply(engine),
        "disk" => disk_check(),
        "network" => network_check(engine),
        _ => Check::new(id, label, Status::Skip, ""),
    }
}

fn macos_version() -> Option<String> {
    let read = |flag: &str| {
        let output = Command::new("/usr/bin/sw_vers").arg(flag).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string()).filter(|text| !text.is_empty())
    };
    let version = read("-productVersion")?;
    Some(match read("-buildVersion") {
        Some(build) => format!("{version} ({build})"),
        None => version,
    })
}

/// An Intel build running on Apple silicon.
fn rosetta() -> bool {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let name = c"sysctl.proc_translated";
    // SAFETY: a known sysctl name and an int-sized buffer.
    unsafe {
        libc::sysctlbyname(name.as_ptr(), (&mut value as *mut libc::c_int).cast(), &mut size, std::ptr::null_mut(), 0)
            == 0
            && value == 1
    }
}

/// Where the CLI is and its version; red when it's the one in question and missing, or Claude Code is too old.
fn cli_check(cli: Engine, engine: Engine) -> Check {
    let id = cli.key();
    let Some(program) = engine::find_program(cli, Some(local_backend::login_path())) else {
        return if cli == engine {
            Check::new(id, cli.label(), Status::Fail, "Not installed").fix(Fix::Install(cli))
        } else {
            Check::new(id, cli.label(), Status::Skip, "Not installed")
        };
    };
    let version = crate::command::cli_version(&program, local_backend::login_path());
    let place = program.display().to_string();
    let detail = match &version {
        Some(version) => format!("{version} · {place}"),
        None => format!("version unknown · {place}"),
    };
    if cli == Engine::Claude
        && let Some(version) = version.as_deref().filter(|version| older(version, CLAUDE_MIN))
    {
        return Check::new(
            id,
            cli.label(),
            Status::Fail,
            format!("{version} is too old: Filo needs {CLAUDE_MIN} or later · {place}"),
        )
        .fix(Fix::UpdateClaude(program));
    }
    Check::new(id, cli.label(), if version.is_some() { Status::Ok } else { Status::Warn }, detail)
}

/// Whether dotted version `version` comes before `min` ("2.1.99" < "2.1.286").
pub fn older(version: &str, min: &str) -> bool {
    let parts = |text: &str| text.split(['.', '-', '+']).map_while(|part| part.parse::<u64>().ok()).collect::<Vec<_>>();
    let (version, min) = (parts(version), parts(min));
    !version.is_empty() && version < min
}

fn sign_in_check(engine: Engine) -> Check {
    let label = format!("Signed in to {}", engine.label());
    let Some(program) = engine::find_program(engine, Some(local_backend::login_path())) else {
        return Check::new("sign-in", label, Status::Skip, format!("{} isn't installed", engine.label()));
    };
    match local_backend::tool_status(engine) {
        ToolStatus::Ready => Check::new("sign-in", label, Status::Ok, "Yes"),
        ToolStatus::NeedsSignIn => {
            Check::new("sign-in", label, Status::Fail, "Not signed in").fix(Fix::SignIn(engine, program))
        }
        ToolStatus::NotInstalled => {
            Check::new("sign-in", label, Status::Skip, format!("{} isn't installed", engine.label()))
        }
    }
}

/// A one-line turn with no tools and no session kept: does the CLI answer at all?
fn test_reply(engine: Engine) -> Check {
    let label = format!("Test reply from {}", engine.label());
    let Some(program) = engine::find_program(engine, Some(local_backend::login_path())) else {
        return Check::new("test-reply", label, Status::Skip, format!("{} isn't installed", engine.label()));
    };
    let mut command = Command::new(&program);
    match engine {
        Engine::Claude => command.args([
            "-p",
            "--output-format",
            "json",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--tools",
            "",
        ]),
        Engine::Codex => command.args(["exec", "--skip-git-repo-check", "--ignore-user-config", "--json", "-"]),
        // Reports check the chat engines (`Engine::ALL`); these answer a prompt headless all the same.
        Engine::Muse => command.args(["exec", "--json", "--no-session-log", "Reply with the word ok."]),
        Engine::OpenCode => command.args(["run", "--format", "json", "Reply with the word ok."]),
        Engine::Pi => command.args(["-p", "--no-session", "--no-tools", "--no-extensions", "Reply with the word ok."]),
    };
    command
        .current_dir(std::env::temp_dir())
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .env_remove("CLAUDE_CODE_SSE_PORT");
    let started = Instant::now();
    let Some(ran) = run_command(&mut command, Some("Reply with just the word ok."), TEST_TIMEOUT) else {
        return Check::new(
            "test-reply",
            label,
            Status::Fail,
            format!("No answer in {} seconds", TEST_TIMEOUT.as_secs()),
        );
    };
    let seconds = started.elapsed().as_secs_f32();
    let stdout = String::from_utf8_lossy(&ran.stdout).to_string();
    let stderr = String::from_utf8_lossy(&ran.stderr).to_string();
    let said = match engine {
        // `{"type":"result","is_error":false,"result":"ok",…}`
        Engine::Claude => serde_json::from_str::<Value>(stdout.trim())
            .ok()
            .map(|result| (result["is_error"] != true, result["result"].as_str().unwrap_or_default().to_string())),
        Engine::Codex | Engine::Muse | Engine::OpenCode | Engine::Pi => None,
    };
    let worked = ran.success && said.as_ref().is_none_or(|(ok, _)| *ok);
    if worked {
        return Check::new("test-reply", label, Status::Ok, format!("Answered in {seconds:.1} s"));
    }
    let why = said
        .map(|(_, text)| text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| last_line(&stderr))
        .or_else(|| last_line(&stdout))
        .unwrap_or_else(|| "It stopped without saying why".into());
    let check = Check::new("test-reply", label, Status::Fail, clip(&why));
    if signed_out(&format!("{stdout}\n{stderr}")) {
        return check.fix(Fix::SignIn(engine, program));
    }
    check
}

fn last_line(text: &str) -> Option<String> {
    text.lines().rev().find(|line| !line.trim().is_empty()).map(|line| line.trim().to_string())
}

/// The words a CLI uses when it isn't signed in, as a turn's failure is read (crates/filo-local engine.rs).
fn signed_out(text: &str) -> bool {
    Engine::signed_out(text)
}

fn disk_check() -> Check {
    let Some(free) = local_backend::free_disk_bytes() else {
        return Check::new("disk", "Disk space", Status::Warn, "Couldn't tell");
    };
    let detail = format!("{} free", gigabytes(free));
    match free {
        free if free < DISK_LOW => {
            Check::new("disk", "Disk space", Status::Fail, format!("Only {detail}")).fix(Fix::FreeDisk)
        }
        free if free < DISK_TIGHT => Check::new("disk", "Disk space", Status::Warn, detail),
        _ => Check::new("disk", "Disk space", Status::Ok, detail),
    }
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.)
}

/// Filo's backend and the engine's API answer at all (any HTTP status counts).
fn network_check(engine: Engine) -> Check {
    let api = match engine {
        Engine::Claude => "api.anthropic.com",
        Engine::Codex => "api.openai.com",
        Engine::Muse => "api.meta.ai",
        Engine::OpenCode => "opencode.ai",
        // Its providers are the user's; pi.dev is its own.
        Engine::Pi => "pi.dev",
    };
    let missed: Vec<&str> = ["dev.filo.im", api].into_iter().filter(|host| !reachable(host)).collect();
    match missed.len() {
        0 => Check::new("network", "Network", Status::Ok, format!("Reached dev.filo.im and {api}")),
        2 => Check::new("network", "Network", Status::Fail, format!("Offline: couldn't reach dev.filo.im or {api}")),
        _ => Check::new("network", "Network", Status::Fail, format!("Couldn't reach {}", missed.join(", "))),
    }
}

/// `curl` (on every Mac) asks `host` for the headers only: any answer means it's reachable. On a plain thread, so no
/// Tokio.
fn reachable(host: &str) -> bool {
    let timeout = NETWORK_TIMEOUT.as_secs().to_string();
    let mut command = Command::new("/usr/bin/curl");
    command.args(["-sS", "-I", "-o", "/dev/null", "--max-time", &timeout, &format!("https://{host}")]);
    run_command(&mut command, None, NETWORK_TIMEOUT + Duration::from_secs(2)).is_some_and(|ran| ran.success)
}

/// Runs `command` with the login shell's PATH (command.rs).
fn run_command(command: &mut Command, stdin: Option<&str>, timeout: Duration) -> Option<crate::command::Ran> {
    crate::command::run(command.env("PATH", local_backend::login_path()), stdin, timeout)
}

// ---------------------------------------------------------------------------------------------------------------
// What goes with the checks

/// The failure, recent failures and log, gathered once the page opens.
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub failure: Option<Failure>,
    pub recent: Vec<Failure>,
    pub log: Vec<String>,
}

/// Reads them from Filo's data on this Mac.
pub fn gather(origin: &Origin) -> Details {
    let dir = local_dir();
    let all = failures::recent(&dir);
    let mut failure = origin.id().and_then(|id| all.iter().find(|failure| failure.id == id).cloned());
    // A failure from before this version (or a group's reply): the error the chat shows, without stderr.
    if failure.is_none()
        && let (Some(id), Some(error)) = (origin.id(), origin.shown_error())
    {
        let kind = if matches!(origin, Origin::Task { .. }) { "round" } else { "turn" };
        failure = Some(Failure {
            at: 0,
            kind: kind.into(),
            id: id.into(),
            engine: None,
            model: None,
            error: error.into(),
            stderr: Vec::new(),
        });
    }
    let recent =
        all.into_iter().filter(|recent| Some(recent.id.as_str()) != origin.id()).take(RECENT_FAILURES).collect();
    let log = tail(&dir.join("workers").join("workers.log"), LOG_LINES);
    Details { failure, recent, log }
}

/// The last `lines` lines of the file at `path`, reading only its end.
fn tail(path: &Path, lines: usize) -> Vec<String> {
    use std::io::{Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = length.saturating_sub(256 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        // Cut mid-line.
        all.remove(0);
    }
    all[all.len().saturating_sub(lines)..].iter().map(|line| line.to_string()).collect()
}

/// The report's body, as Filo's backend takes it (docs/problem-reports.md): the build and system, the CLIs, the checks
/// with the first red one, the error, the user's note and the log lines, and the rest under `extra` (with `schema`).
/// Everything in it redacted.
pub fn payload(
    report_id: &str,
    origin: &Origin,
    engine: Engine,
    checks: &[Check],
    details: &Details,
    note: &str,
) -> Value {
    let cli = |id: &str| {
        checks.iter().find(|check| check.id == id && check.detail != "Not installed").map(|check| redact(&check.detail))
    };
    let mut body = json!({
        "build": crate::updater::build_number(),
        "version": crate::updater::VERSION,
        "channel": crate::updater::CHANNEL.unwrap_or("local"),
        "macos": checks.iter().find(|check| check.id == "macos").map_or_else(|| "unknown".to_string(), |check| check.detail.clone()),
        "arch": std::env::consts::ARCH,
        "claudeCode": cli("claude"),
        "codex": cli("codex"),
        "checks": checks.iter().map(|check| json!({ "name": check.label, "ok": check.status != Status::Fail, "detail": redact(&check.detail) })).collect::<Vec<_>>(),
        "logs": logs(details),
        "extra": {
            "schema": SCHEMA,
            "reportId": report_id,
            "source": origin.source(),
            "engine": engine,
            "museCode": cli("muse"),
            "opencode": cli("opencode"),
            "pi": cli("pi"),
            "appLabel": crate::updater::version_label(),
            "checks": checks.iter().map(|check| json!({ "id": check.id, "status": check.status, "fix": check.fix.as_ref().map(Fix::id) })).collect::<Vec<_>>(),
            "failure": details.failure.as_ref().map(|failure| json!({
                "kind": failure.kind,
                "engine": failure.engine,
                "model": failure.model,
                "at": (failure.at > 0).then_some(failure.at),
                "stderrLines": failure.stderr.len(),
            })),
            "recentFailures": details.recent.iter().map(|recent| json!({
                "kind": recent.kind,
                "engine": recent.engine,
                "at": recent.at,
                "error": clip(&redact(&recent.error)),
            })).collect::<Vec<_>>(),
        },
    });
    if let Some(check) = checks.iter().find(|check| check.status == Status::Fail) {
        body["failedCheck"] = json!({ "name": check.label, "detail": redact(&check.detail) });
    }
    if let Some(failure) = &details.failure {
        body["error"] = json!(redact(&failure.error));
    }
    let note = note.trim();
    if !note.is_empty() {
        body["note"] = json!(redact(note));
    }
    body
}

/// The failure's stderr, then the end of the workers' log, each under a line saying which.
fn logs(details: &Details) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(failure) = details.failure.as_ref().filter(|failure| !failure.stderr.is_empty()) {
        lines.push(format!("[stderr of the failed {}]", if failure.kind == "round" { "round" } else { "reply" }));
        lines.extend(failure.stderr.iter().map(|line| clip(&redact(line))));
    }
    if !details.log.is_empty() {
        lines.push("[workers.log]".to_string());
        lines.extend(details.log.iter().map(|line| clip(&redact(line))));
    }
    lines
}

/// The one line under the checks that says what Send sends.
pub fn preview(checks: &[Check], details: &Details) -> String {
    let red = checks.iter().filter(|check| check.status == Status::Fail).count();
    let mut parts = vec![match red {
        0 => count(checks.len(), "check"),
        red => format!("{} ({red} red)", count(checks.len(), "check")),
    }];
    if let Some(failure) = &details.failure {
        let what = if failure.kind == "round" { "the task's error" } else { "the reply's error" };
        parts.push(match failure.stderr.len() {
            0 => what.to_string(),
            lines => format!("{what} with {} of output", count(lines, "line")),
        });
    }
    if !details.recent.is_empty() {
        parts.push(count(details.recent.len(), "recent failure"));
    }
    if !details.log.is_empty() {
        parts.push(count(details.log.len(), "log line"));
    }
    format!("Sends {}. No chat text; paths and keys hidden.", join_words(&parts))
}

fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn join_words(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [first, last] => format!("{first} and {last}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(LINE_MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// Hides what's private in `text`: what a CLI wrote instead of replying (chat text), keys and tokens, email
/// addresses, and paths in the home folder (the folder's name and the folders in between; the file's name stays,
/// and so do the hidden folders tools install in, like `~/.local`) and in other users' folders.
pub fn redact(text: &str) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            // Long random strings that aren't ids Filo uses (uuids are 36).
            (r"\b[A-Za-z0-9_\-]{48,}\b", "[hidden]"),
            (r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b", "[email]"),
        ]
        .into_iter()
        .map(|(pattern, with)| (Regex::new(pattern).expect("a valid pattern"), with))
        .collect()
    });
    // What the CLI wrote instead of replying is the reply's text: cut.
    let mut text = match text.find("What it wrote instead:") {
        Some(at) => format!("{}What it wrote instead: [left out]", &text[..at]),
        None => text.to_string(),
    };
    text = hide_keys(&text);
    for (pattern, with) in patterns {
        text = pattern.replace_all(&text, *with).into_owned();
    }
    redact_paths(&text, dirs::home_dir().as_deref())
}

/// Keys and tokens in `text`, by their shape, hidden (a shared coding result's too, local_backend/result_share.rs).
pub fn hide_keys(text: &str) -> String {
    hide_keys_in(text, &[]).0
}

/// `hide_keys`, with `known` (secrets this Mac holds: connector keys, share tokens) hidden wherever they are, whatever
/// their shape; and how many it hid. The values of secret-looking variables in Filo's own environment are hidden too.
/// The patterns cover the common formats (security review H1): every provider prefix below, a private key's whole PEM
/// block, a password in a URL, a header that carries a key, and a value after a name that says it's secret, with any
/// prefix (`DB_PASSWORD=`, `AWS_SECRET_ACCESS_KEY=`, `client_secret:`), which a word boundary before "password" missed.
pub fn hide_keys_in(text: &str, known: &[String]) -> (String, usize) {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"(?s)-----BEGIN[A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END[A-Z0-9 ]*PRIVATE KEY-----|\z)", "[private key]"),
            (r"(?i)\b([a-z][a-z0-9+.\-]*://)[^\s/?#@:'\x22]+:[^\s/?#@'\x22]+@", "$1[hidden]@"),
            (r"sk-ant-[A-Za-z0-9_\-]{8,}", "[key]"),
            (r"\bsk-[A-Za-z0-9_\-]{16,}", "[key]"),
            (r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{10,}", "[key]"),
            (r"\bwhsec_[A-Za-z0-9]{10,}", "[key]"),
            (r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}", "[key]"),
            (r"\bgithub_pat_[A-Za-z0-9_]{20,}", "[key]"),
            (r"\bglpat-[A-Za-z0-9_\-]{20,}", "[key]"),
            (r"\bxox[abeprs]-[A-Za-z0-9\-]{10,}", "[key]"),
            (r"\bxapp-[A-Za-z0-9\-]{10,}", "[key]"),
            (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", "[key]"),
            (r"\bAIza[0-9A-Za-z_\-]{30,}", "[key]"),
            (r"\bya29\.[A-Za-z0-9_\-]{20,}", "[token]"),
            (r"\bGOCSPX-[A-Za-z0-9_\-]{10,}", "[key]"),
            (r"\bhf_[A-Za-z0-9]{20,}", "[key]"),
            (r"\bnpm_[A-Za-z0-9]{20,}", "[key]"),
            (r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{5,}", "[token]"),
            (r"(?i)\b(bearer|basic|token)\s+[A-Za-z0-9._~+/\-]{12,}=*", "$1 [token]"),
            (r"(?i)\b(AccountKey|SharedAccessKey|sig)=[^;\s&'\x22\[][^;\s&'\x22]{7,}", "$1=[hidden]"),
            (r"(?i)\b(authorization|proxy-authorization|x-api-key|api-key|x-auth-token|ocp-apim-subscription-key)(\s*:\s*)(?:(?:bearer|basic|token)\s+)?[^\s,;'\x22\[][^\s,;'\x22]{7,}", "$1$2[hidden]"),
            // A name that says it's secret, then its value: `DB_PASSWORD=…`, `"client_secret": "…"`, `SECRET_KEY_BASE: …`.
            (r#"\b([A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIALS?)[A-Z0-9_]*|[A-Z][A-Z0-9_]*_(?:PWD|AUTH))\b(["']?\s*[:=]\s*["']?)[^\s"',;}\[][^\s"',;}]{3,}"#, "$1$2[hidden]"),
            (r#"(?i)\b([a-z0-9]+(?:[_\-.][a-z0-9]+)*[_\-.](?:key|token|secret|password|passwd|pwd|credentials?))\b(["']?\s*[:=]\s*["']?)[^\s"',;}\[][^\s"',;}]{3,}"#, "$1$2[hidden]"),
            (r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|token|secret|password|passwd)(["']?\s*[:=]\s*["']?)[^\s"',;}\[][^\s"',;}]{3,}"#, "$1$2[hidden]"),
        ]
        .into_iter()
        .map(|(pattern, with)| (Regex::new(pattern).expect("a valid pattern"), with))
        .collect()
    });
    let mut text = text.to_string();
    let mut hidden = 0;
    // Exact values first, the longest first (one may hold another).
    let mut values: Vec<&str> = known
        .iter()
        .map(String::as_str)
        .chain(env_secrets().iter().map(String::as_str))
        .filter(|value| value.len() >= 8)
        .collect();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in values {
        let found = text.matches(value).count();
        if found > 0 {
            hidden += found;
            text = text.replace(value, "[hidden]");
        }
    }
    for (pattern, with) in patterns {
        let found = pattern.find_iter(&text).count();
        if found > 0 {
            hidden += found;
            text = pattern.replace_all(&text, *with).into_owned();
        }
    }
    (text, hidden)
}

/// The values of secret-looking variables in Filo's own environment (`OPENAI_API_KEY`, `DB_PASSWORD`…), read once.
fn env_secrets() -> &'static [String] {
    static VALUES: OnceLock<Vec<String>> = OnceLock::new();
    VALUES.get_or_init(|| {
        let name = Regex::new(r"(?i)(KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL)").expect("a valid pattern");
        std::env::vars()
            .filter(|(key, value)| name.is_match(key) && value.len() >= 8 && !value.contains('/'))
            .map(|(_, value)| value)
            .collect()
    })
}

/// Long random strings hidden too, for what leaves this Mac for other people (a shared result or artifact): 32 or more
/// letters and digits (or base64) mixing upper and lower case and digits, or 32 or more hex digits, whatever's around
/// them (an AWS secret key, an Azure key). A uuid stays, and so does a git commit's 40 hex digits; and how many it hid.
pub fn hide_long_secrets(text: &str) -> (String, usize) {
    static LONG: OnceLock<Regex> = OnceLock::new();
    let long =
        LONG.get_or_init(|| Regex::new(r"(^|[^A-Za-z0-9+_\-])([A-Za-z0-9+_\-]{32,}={0,2})").expect("a valid pattern"));
    let mut hidden = 0;
    let text = long.replace_all(text, |caps: &regex::Captures| {
        if random_looking(&caps[2]) {
            hidden += 1;
            format!("{}[hidden]", &caps[1])
        } else {
            caps[0].to_string()
        }
    });
    (text.into_owned(), hidden)
}

fn random_looking(word: &str) -> bool {
    let word = word.trim_end_matches('=');
    let count = |test: fn(&char) -> bool| word.chars().filter(test).count();
    let uuid = word.len() == 36
        && word
            .chars()
            .enumerate()
            .all(|(at, c)| if [8, 13, 18, 23].contains(&at) { c == '-' } else { c.is_ascii_hexdigit() });
    let hex = word.chars().all(|c| c.is_ascii_hexdigit());
    if uuid || (hex && word.len() == 40) {
        return false;
    }
    hex || (count(char::is_ascii_uppercase) >= 2
        && count(char::is_ascii_lowercase) >= 2
        && count(char::is_ascii_digit) >= 2)
}

/// Paths in `home` and other users' folders, and in the private temp folders, cut to their file's name.
pub fn redact_paths(text: &str, home: Option<&Path>) -> String {
    static PATHS: OnceLock<Regex> = OnceLock::new();
    let mut text = text.to_string();
    if let Some(home) = home.map(|home| home.display().to_string()).filter(|home| home.len() > 1) {
        text = text.replace(&home, "~");
    }
    // Other users' folders, and the private temp folders.
    let paths = PATHS.get_or_init(|| {
        Regex::new(r#"(~|/Users/[^/\s"']+|/private/var/folders|/var/folders)((?:/[^/\s"'():,]+)+)"#)
            .expect("a valid pattern")
    });
    paths
        .replace_all(&text, |caps: &regex::Captures| {
            let root = match &caps[1] {
                "~" => "~",
                root if root.starts_with("/Users/") => "/Users/…",
                _ => "…",
            };
            let parts: Vec<&str> = caps[2].split('/').filter(|part| !part.is_empty()).collect();
            // A tool's own folder (`~/.local/bin/claude`) says how it was installed; a file's name says what it was.
            let kept_head = parts.first().filter(|part| part.starts_with('.') && parts.len() > 1);
            match (kept_head, parts.as_slice()) {
                (_, [only]) => format!("{root}/{only}"),
                (Some(head), [.., last]) if parts.len() == 2 => format!("{root}/{head}/{last}"),
                (Some(head), [.., last]) => format!("{root}/{head}/…/{last}"),
                (None, [.., last]) => format!("{root}/…/{last}"),
                (_, []) => root.to_string(),
            }
        })
        .into_owned()
}

// ---------------------------------------------------------------------------------------------------------------
// Sending

/// How a send went wrong, in words for the page.
#[derive(Clone, Debug, PartialEq)]
pub enum SendError {
    Offline,
    /// Too many reports from here (5 a minute): try again after this many seconds.
    TooMany(Option<u64>),
    /// The server didn't take it: its status, and what it said was wrong (400's `error` and `field`).
    Refused(u16, Option<String>),
    Other(String),
}

impl SendError {
    pub fn message(&self) -> String {
        match self {
            SendError::Offline => "Couldn't reach Filo's server. Check your connection and send again.".into(),
            SendError::TooMany(Some(seconds)) => format!("Too many reports just now. Try again in {seconds} seconds."),
            SendError::TooMany(None) => "Too many reports just now. Try again in a minute.".into(),
            SendError::Refused(status, _) if *status >= 500 => {
                format!("Filo's server had a problem ({status}). Try again in a few minutes.")
            }
            SendError::Refused(status, Some(said)) => {
                format!("Filo's server didn't take the report ({status}: {said}).")
            }
            SendError::Refused(status, None) => format!("Filo's server didn't take the report ({status})."),
            SendError::Other(error) => format!("Couldn't send it: {error}"),
        }
    }
}

/// The body stays under the server's 512 KB: log lines go first, oldest first.
const BODY_MAX: usize = 500 * 1024;

fn fit(mut body: Value) -> Value {
    while body.to_string().len() > BODY_MAX {
        let Some(logs) = body["logs"].as_array_mut().filter(|logs| !logs.is_empty()) else { break };
        let drop = logs.len().div_ceil(4);
        logs.drain(..drop);
    }
    body
}

/// Posts `body` to `endpoint`; the report's id from the answer (`201 { "id", "url" }`), when it has one.
pub async fn send(http: reqwest::Client, endpoint: &str, body: Value) -> Result<Option<String>, SendError> {
    let response = http.post(endpoint).timeout(SEND_TIMEOUT).json(&fit(body)).send().await.map_err(|error| {
        if error.is_connect() || error.is_timeout() {
            SendError::Offline
        } else {
            SendError::Other(error.without_url().to_string())
        }
    })?;
    let status = response.status();
    if status.as_u16() == 429 {
        let wait = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        return Err(SendError::TooMany(wait));
    }
    let answer: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let said = answer["error"].as_str().map(|error| match answer["field"].as_str() {
            Some(field) => format!("{error}, {field}"),
            None => error.to_string(),
        });
        return Err(SendError::Refused(status.as_u16(), said));
    }
    Ok(answer.get("id").and_then(|id| id.as_str().map(str::to_string).or_else(|| id.as_i64().map(|id| id.to_string()))))
}

/// Where reports go: Filo's backend, or in `dev` builds `FILO_REPORT_URL` (a test server).
pub fn endpoint() -> String {
    crate::test_run::dev_override("FILO_REPORT_URL").unwrap_or_else(|| ENDPOINT.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(older("2.1.99", CLAUDE_MIN));
        assert!(older("2.0.300", CLAUDE_MIN));
        assert!(older("1.9.0", CLAUDE_MIN));
        assert!(!older("2.1.286", CLAUDE_MIN));
        assert!(!older("2.1.296", CLAUDE_MIN));
        assert!(!older("2.2.0", CLAUDE_MIN));
        assert!(!older("unknown", CLAUDE_MIN));
    }

    /// Every format the security review found leaking (H1, hide-keys-gaps.log), with fake values: none stays.
    #[test]
    fn hides_the_common_secret_formats() {
        let cases = [
            ("env-style *_PASSWORD", "DB_PASSWORD=Tr0ub4dor-and-3", "Tr0ub4dor"),
            ("AWS secret key (env)", "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "wJalrXUtnFEMI"),
            ("AWS temporary key id", "aws_access_key_id = ASIAIOSFODNN7EXAMPLE", "ASIAIOSFODNN7EXAMPLE"),
            ("Stripe secret key", "charge with sk_live_51Habcdefghijklmnop", "sk_live_51H"),
            ("Cloudflare token (env)", "export CLOUDFLARE_API_TOKEN=abcd1234efgh5678ijkl", "abcd1234efgh"),
            ("Azure key header", "Ocp-Apim-Subscription-Key: 0123456789abcdef0123456789abcdef", "0123456789abcdef"),
            ("Azure key (env)", "AZURE_SPEECH_KEY=0123456789abcdef0123456789abcdef", "0123456789abcdef"),
            ("Password in a URL", "git clone https://bob:hunter2pass@github.com/x/y.git", "hunter2pass"),
            (
                "PEM private key",
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAabcdef\n-----END RSA PRIVATE KEY-----",
                "MIIEowIBAAKCAQEA",
            ),
            ("Google OAuth access token", "token ya29.a0AfH6SMBabcdefghijklmnopqrstu", "ya29.a0AfH6SMB"),
            ("Google client secret", "GOCSPX-abcdefghijklmnop1234", "GOCSPX-abcdefgh"),
            ("Hugging Face token", "using hf_abcdefghijklmnopqrstuvwxyz12", "hf_abcdefghij"),
            ("npm token", "//registry.npmjs.org/:_authToken=npm_abcdefghijklmnopqrstuvwxyz0123", "npm_abcdefghij"),
            ("GitLab token", "glpat-abcdefghijklmnopqrst", "glpat-abcdefgh"),
            ("client_secret=", "client_secret=s3cr3t-value-123", "s3cr3t-value"),
            (
                "Azure storage conn string",
                "DefaultEndpointsProtocol=https;AccountName=x;AccountKey=abcd1234efgh5678==;EndpointSuffix=core",
                "abcd1234efgh",
            ),
            ("Authorization: token", "Authorization: token ghx0123456789abcdefghij", "ghx0123456789"),
            ("Slack app token", "xapp-1-A0123-4567-abcdef", "xapp-1-A0123"),
            ("JSON secret", r#"{"client_secret": "abcd-efgh-ijkl", "name": "x"}"#, "abcd-efgh"),
            ("Rails secret", "SECRET_KEY_BASE: 9f8e7d6c5b4a3f2e1d0c", "9f8e7d6c5b4a"),
        ];
        for (case, text, secret) in cases {
            let (hidden, count) = hide_keys_in(text, &[]);
            assert!(!hidden.contains(secret) && count > 0, "{case}: {hidden}");
        }
        // What stays: ids, commits, prose.
        for kept in [
            "turn 0b5dd17b-3c72-4f81-8ac6-7faa5df2ded1",
            "commit 31129ed3f1a7c2b0e9d8f7a6b5c4d3e2f1a0b9c8",
            "The primary key is the id.",
            "Passwords: use a manager.",
        ] {
            assert_eq!(hide_keys(kept), kept);
            assert_eq!(hide_long_secrets(kept).0, kept);
        }
        // A secret this Mac holds, whatever its shape.
        assert_eq!(
            hide_keys_in("the key is plainwords-long", &["plainwords-long".into()]),
            ("the key is [hidden]".to_string(), 1)
        );
        // For shares, long random strings too.
        let (shared, count) =
            hide_long_secrets("aws wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY1 and 0123456789abcdef0123456789abcdef");
        assert_eq!((shared.as_str(), count), ("aws [hidden] and [hidden]", 2));
    }

    #[test]
    fn hides_paths_keys_and_reply_text() {
        let home = Path::new("/Users/sam");
        assert_eq!(
            redact_paths("at /Users/sam/Desktop/secret-project/src/main.rs:12", Some(home)),
            "at ~/…/main.rs:12"
        );
        assert_eq!(redact_paths("/Users/sam/.local/bin/claude", Some(home)), "~/.local/…/claude");
        assert_eq!(redact_paths("/Users/sam/.claude.json", Some(home)), "~/.claude.json");
        assert_eq!(redact_paths("cd /Users/other/work/repo failed", Some(home)), "cd /Users/…/…/repo failed");
        assert_eq!(redact_paths("/opt/homebrew/bin/claude", Some(home)), "/opt/homebrew/bin/claude");
        assert_eq!(redact_paths("/var/folders/ab/xyz/T/filo-1.command", Some(home)), "…/…/filo-1.command");

        let said = redact(
            "Invalid API key sk-ant-api03-abcdefghijklmnop · Authorization: Bearer abcdefghijklmnopqrstuv · token=hunter2hunter2 · write to sam@example.com",
        );
        assert!(
            !said.contains("abcdefghijklmnop") && !said.contains("hunter2") && !said.contains("sam@example.com"),
            "{said}"
        );
        assert!(
            said.contains("[key]")
                && said.contains("Bearer [token]")
                && said.contains("token=[hidden]")
                && said.contains("[email]"),
            "{said}"
        );
        // Ids stay: they say which turn or task it was.
        assert!(redact("turn 0b5dd17b-3c72-4f81-8ac6-7faa5df2ded1").contains("0b5dd17b-3c72-4f81-8ac6-7faa5df2ded1"));

        let error = "Claude Code finished without sending a message with Filo's send_message tool. What it wrote instead:\n\nHere's your private plan for Monday";
        let said = redact(error);
        assert!(!said.contains("Monday") && said.ends_with("What it wrote instead: [left out]"), "{said}");
    }

    fn sample() -> (Vec<Check>, Details) {
        let checks = vec![
            Check::new("filo", "Filo", Status::Ok, "0.1.0 · local build"),
            Check::new("claude", "Claude Code", Status::Fail, "2.1.10 is too old · /Users/sam/.local/bin/claude")
                .fix(Fix::UpdateClaude(PathBuf::from("/x/claude"))),
        ];
        let failure = Failure {
            at: 5,
            kind: "turn".into(),
            id: "t1".into(),
            engine: Some(Engine::Claude),
            model: Some("opus".into()),
            error: "Claude Code stopped with status 1: boom".into(),
            stderr: vec!["Error at /Users/sam/code/x.js".into(), "token: abcdefgh1234".into()],
        };
        let recent = vec![Failure { stderr: vec!["kept out".into()], id: "t0".into(), ..failure.clone() }];
        (checks, Details { failure: Some(failure), recent, log: vec!["worker 1: turn ended".into()] })
    }

    #[test]
    fn the_body_matches_the_backend_and_is_redacted() {
        let (checks, details) = sample();
        let body = payload(
            "r1",
            &Origin::Turn { turn_id: "t1".into(), error: None },
            Engine::Claude,
            &checks,
            &details,
            "  It just stopped  ",
        );
        assert!(body["build"].is_u64());
        assert_eq!(body["version"], crate::updater::VERSION);
        assert!(body["channel"].is_string() && body["macos"].is_string() && body["arch"].is_string());
        assert_eq!(body["claudeCode"], "2.1.10 is too old · /Users/…/.local/…/claude");
        assert!(body["codex"].is_null());
        assert_eq!(
            body["checks"][1],
            json!({ "name": "Claude Code", "ok": false, "detail": "2.1.10 is too old · /Users/…/.local/…/claude" })
        );
        assert_eq!(
            body["failedCheck"],
            json!({ "name": "Claude Code", "detail": "2.1.10 is too old · /Users/…/.local/…/claude" })
        );
        assert_eq!(body["error"], "Claude Code stopped with status 1: boom");
        assert_eq!(body["note"], "It just stopped");
        let logs: Vec<&str> = body["logs"].as_array().unwrap().iter().map(|line| line.as_str().unwrap()).collect();
        assert_eq!(
            logs,
            [
                "[stderr of the failed reply]",
                "Error at /Users/…/…/x.js",
                "token: [hidden]",
                "[workers.log]",
                "worker 1: turn ended"
            ]
        );
        assert_eq!(body["extra"]["schema"], SCHEMA);
        assert_eq!(body["extra"]["reportId"], "r1");
        assert_eq!(body["extra"]["source"], "chat");
        assert_eq!(body["extra"]["engine"], "claude");
        assert_eq!(body["extra"]["checks"][1], json!({ "id": "claude", "status": "fail", "fix": "update-claude" }));
        // Recent failures carry their error, not their output.
        assert!(body["extra"]["recentFailures"][0].get("stderr").is_none());
        let text = body.to_string();
        assert!(!text.contains("/Users/sam") && !text.contains("abcdefgh1234") && !text.contains("kept out"), "{text}");
        // No note, failure or red check: those fields are left out rather than null.
        let fine = vec![checks[0].clone()];
        let body = payload("r1", &Origin::Settings, Engine::Claude, &fine, &Details::default(), " ");
        let object = body.as_object().unwrap();
        assert!(!object.contains_key("note") && !object.contains_key("error") && !object.contains_key("failedCheck"));
        assert_eq!(body["logs"], json!([]));
    }

    #[test]
    fn a_big_body_drops_old_log_lines() {
        let lines: Vec<String> = (0..2000).map(|n| format!("{n} {}", "x".repeat(500))).collect();
        let body = fit(json!({ "build": 1, "logs": lines }));
        assert!(body.to_string().len() <= BODY_MAX);
        assert!(body["logs"].as_array().unwrap().last().unwrap().as_str().unwrap().starts_with("1999 "));
    }

    #[test]
    fn previews_in_one_line() {
        let (checks, details) = sample();
        assert_eq!(
            preview(&checks, &details),
            "Sends 2 checks (1 red), the reply's error with 2 lines of output, 1 recent failure, and 1 log line. No chat text; paths and keys hidden."
        );
        assert_eq!(preview(&checks[..1], &Details::default()), "Sends 1 check. No chat text; paths and keys hidden.");
        let log = Details { log: vec!["a".into(), "b".into()], ..Default::default() };
        assert_eq!(preview(&checks[..1], &log), "Sends 1 check and 2 log lines. No chat text; paths and keys hidden.");
    }

    #[test]
    fn every_tool_signed_out_is_recognised() {
        for said in [
            "Error: Not logged in. Please run /login",
            "Run `codex login` first",
            "Run muse login to sign in",
            "No credentials: run opencode auth login",
        ] {
            assert!(signed_out(said), "{said}");
        }
        assert!(!signed_out("rate limited"));
        assert_eq!(crate::command::version_in("opencode v2.0.16\n").as_deref(), Some("2.0.16"));
    }

    #[test]
    fn a_failure_shown_without_a_record_still_goes() {
        let origin = Origin::Task { task_id: "no-such-task".into(), error: Some("Failed: it broke".into()) };
        let details = gather(&origin);
        let failure = details.failure.unwrap();
        assert_eq!((failure.kind.as_str(), failure.error.as_str()), ("round", "Failed: it broke"));
        assert!(failure.stderr.is_empty());
    }

    /// Runs every check on this Mac, the test reply included, and prints them with the body:
    /// `cargo test --bin filo report::tests::checks_this_mac -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn checks_this_mac() {
        let origin = Origin::Settings;
        let engine = engine_for(&origin);
        let checks: Vec<Check> = std::thread::scope(|scope| {
            let runs: Vec<_> = CHECKS.iter().map(|(id, _)| scope.spawn(move || run(id, engine))).collect();
            runs.into_iter().map(|run| run.join().unwrap()).collect()
        });
        for check in &checks {
            println!(
                "{:<5} {:<28} {}{}",
                format!("{:?}", check.status),
                check.label,
                check.detail,
                check.fix.as_ref().map(|fix| format!("  [{}]", fix.label())).unwrap_or_default()
            );
        }
        let details = gather(&origin);
        println!("\n{}\n", preview(&checks, &details));
        let body = payload("live", &origin, engine, &checks, &details, "");
        println!("{}", serde_json::to_string_pretty(&body).unwrap());
        assert_eq!(checks.len(), CHECKS.len());
    }

    /// A stand-in for Filo's backend on localhost: answers with `status`, `headers` and `body`.
    async fn server(status: u16, headers: &'static str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/desktop/reports", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|n| n.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if rest.len() >= length || read == 0 {
                            break;
                        }
                    }
                }
                let reply = format!(
                    "HTTP/1.1 {status} X\r\n{headers}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        url
    }

    #[tokio::test]
    async fn sends_and_reads_the_id_or_says_what_went_wrong() {
        let http = reqwest::Client::new();
        let url = server(201, "", r#"{"id":"rep_42","url":"https://dev.filo.im/r/rep_42"}"#).await;
        assert_eq!(send(http.clone(), &url, json!({ "build": 1 })).await, Ok(Some("rep_42".into())));
        let url = server(400, "", r#"{"error":"expected an integer","field":"build"}"#).await;
        let refused = send(http.clone(), &url, json!({})).await.unwrap_err();
        assert_eq!(refused, SendError::Refused(400, Some("expected an integer, build".into())));
        assert_eq!(refused.message(), "Filo's server didn't take the report (400: expected an integer, build).");
        let url = server(429, "retry-after: 42\r\n", "{}").await;
        assert_eq!(send(http.clone(), &url, json!({})).await, Err(SendError::TooMany(Some(42))));
        let url = server(503, "", "{}").await;
        assert_eq!(send(http.clone(), &url, json!({})).await, Err(SendError::Refused(503, None)));
        // Nothing listening: offline.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/desktop/reports", closed.local_addr().unwrap());
        drop(closed);
        assert_eq!(send(http, &url, json!({})).await, Err(SendError::Offline));
    }
}

// Running a program for its answer, with a deadline: the problem report's checks, a tool's version and sign-in status,
// the login shell's variables, Quick Look's still of a video. One way for all of them, crates/filo-local
// `engine::run_for`: in a process group of its own (a deadline kills everything it started), both pipes read as they
// fill (a chatty shell profile can't block on a full pipe), and stdin written on a thread.

use std::process::Command;
use std::time::Duration;

/// How a run ended in time.
pub struct Ran {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs `command` with `stdin` written to it, for `timeout` at most (then it's killed, with what it started: None),
/// as crates/filo-local `engine::run_for` does. The caller sets its arguments and environment (`local_backend::login_path`
/// for the user's PATH).
pub fn run(command: &mut Command, stdin: Option<&str>, timeout: Duration) -> Option<Ran> {
    let ran = filo_local::engine::run_for(command, stdin, timeout).ok()?;
    ran.status.map(|status| Ran { success: status.success(), stdout: ran.stdout, stderr: ran.stderr })
}

/// `claude --version` ("2.1.296 (Claude Code)"), `codex --version` ("codex-cli 0.162.0"), `muse --version` ("Muse Code
/// 1.4.6 (1.4.6-R6000.1)") or `opencode --version` ("opencode v2.0.16"): the version in it, given 10 seconds on `path`.
pub fn cli_version(program: &std::path::Path, path: &str) -> Option<String> {
    let ran = run(Command::new(program).arg("--version").env("PATH", path), None, Duration::from_secs(10))?;
    version_in(&String::from_utf8_lossy(&ran.stdout))
}

/// The version in a CLI's `--version` line: its first dotted number, "v2.0.16" read as 2.0.16.
pub fn version_in(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|word| word.strip_prefix('v').unwrap_or(word))
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()) && word.contains('.'))
        .map(str::to_string)
}

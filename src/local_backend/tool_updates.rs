// The coding tools' updates, the Coding page's "Update to vX" pill: each tool's newest version, looked up about once a
// day where its own updater looks (`latest_url`), and the update itself, only when the user clicks it: the tool's own
// updater (`claude update`, `codex update`, `opencode upgrade`, `pi update`, Muse Code's installer again), given 10
// minutes. Nothing updates by itself. A task's round or a chat's turn already running keeps the process it started
// with: an updater only swaps the files on disk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use filo_local::coding;
use filo_local::engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Local, login_path, now_ms};

/// How often the newest versions are looked up.
const CHECK_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
/// After a look that found nothing (offline, say), how long until the next try.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
/// How long a tool's updater is given.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The last look, in `local/`, so a restart doesn't look again.
const SAVED: &str = "tool-updates.json";
/// Muse Code's installer, as Welcome gives it.
const MUSE_INSTALLER: &str = "curl -fsSL https://dev.meta.ai/install.sh | bash";

/// `SAVED`: when the newest versions were looked up (ms), and what they were, by tool id.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Saved {
    checked_at: i64,
    latest: HashMap<String, String>,
}

/// What `Local` keeps for the updates.
#[derive(Default)]
pub(super) struct State {
    /// `SAVED`, read the first time it's needed.
    saved: Mutex<Option<Saved>>,
    checking: AtomicBool,
    /// The last look that found nothing, this run.
    failed_at: Mutex<Option<Instant>>,
    /// The tools whose updater is running.
    updating: Mutex<HashSet<String>>,
    /// What went wrong with a tool's last update, in its own words, until the next one.
    errors: Mutex<HashMap<String, String>>,
}

impl Local {
    /// A tool's row on the Coding page gets `latestVersion` when a newer version than `version` is out, `updating`
    /// while its updater runs, and `updateError` when the last update failed. Starts the day's look when it's due.
    pub(super) fn add_update_fields(self: &Arc<Self>, row: &mut Value, tool: &str, version: Option<&str>) {
        let latest = self.latest_versions();
        if let (Some(latest), Some(version)) = (latest.get(tool), version)
            && newer(latest, version)
        {
            row["latestVersion"] = json!(latest);
        }
        if self.coding.updates.updating.lock().unwrap().contains(tool) {
            row["updating"] = json!(true);
        }
        if let Some(error) = self.coding.updates.errors.lock().unwrap().get(tool) {
            row["updateError"] = json!(error);
        }
    }

    /// The newest versions last looked up, by tool; a look in the background when the last is a day old.
    fn latest_versions(self: &Arc<Self>) -> HashMap<String, String> {
        let state = &self.coding.updates;
        let saved = state.saved.lock().unwrap().get_or_insert_with(|| self.read_json(SAVED)).clone();
        let retry = state.failed_at.lock().unwrap().is_none_or(|at| at.elapsed() >= RETRY_AFTER);
        if now_ms() - saved.checked_at >= CHECK_EVERY_MS && retry && !state.checking.swap(true, Ordering::SeqCst) {
            let local = self.clone();
            std::thread::spawn(move || {
                local.look_for_updates();
                local.coding.updates.checking.store(false, Ordering::SeqCst);
            });
        }
        saved.latest
    }

    /// Looks up every installed tool's newest version, all at once, and saves what it found.
    fn look_for_updates(&self) {
        // Tests never ask the internet.
        if cfg!(test) {
            return;
        }
        let installed = super::coding::tool_versions();
        let found: Vec<(String, String)> = std::thread::scope(|scope| {
            let looks: Vec<_> = installed
                .iter()
                .filter_map(|(tool, version)| {
                    let engine = coding::engine_of(tool)?;
                    Some((tool.clone(), scope.spawn(move || latest(engine, version))))
                })
                .collect();
            looks.into_iter().filter_map(|(tool, look)| Some((tool, look.join().ok()??))).collect()
        });
        if found.is_empty() {
            *self.coding.updates.failed_at.lock().unwrap() = Some(Instant::now());
            return;
        }
        let saved = {
            let mut kept = self.coding.updates.saved.lock().unwrap();
            let saved = kept.get_or_insert_with(Saved::default);
            saved.checked_at = now_ms();
            saved.latest.extend(found);
            saved.clone()
        };
        if let Err(error) = self.write_json(SAVED, &saved) {
            eprintln!("filo coding: {error}");
        }
        self.changed();
    }

    /// `coding:updateTool`: runs the tool's own updater in the background, the row's pill a spinner meanwhile; then
    /// the row shows the version it has, or what the updater said went wrong.
    pub(super) fn update_tool(self: &Arc<Self>, tool: &str, program: PathBuf) -> Result<Value, String> {
        let engine = coding::engine_of(tool).ok_or_else(|| format!("There's no coding tool called {tool}."))?;
        if !self.coding.updates.updating.lock().unwrap().insert(tool.to_string()) {
            return Ok(Value::Null);
        }
        self.coding.updates.errors.lock().unwrap().remove(tool);
        self.changed();
        let (local, tool) = (self.clone(), tool.to_string());
        std::thread::spawn(move || {
            let path = login_path();
            let ran = run_update(engine, &program, path);
            let version = super::coding::tool_version_again(&tool, &program);
            let latest =
                local.coding.updates.saved.lock().unwrap().as_ref().and_then(|saved| saved.latest.get(&tool).cloned());
            let error = match ran {
                Err(error) => Some(error),
                Ok(said) => match (latest, version) {
                    (Some(latest), Some(version)) if newer(&latest, &version) => Some(format!(
                        "Still v{version} after `{}`.{}",
                        update_command(engine),
                        said.map(|said| format!(" {said}")).unwrap_or_default()
                    )),
                    _ => None,
                },
            };
            if let Some(error) = error {
                local.coding.updates.errors.lock().unwrap().insert(tool.clone(), error);
            }
            local.coding.updates.updating.lock().unwrap().remove(&tool);
            local.changed();
        });
        Ok(Value::Null)
    }
}

/// The updater a person would type for it.
pub(crate) fn update_command(engine: Engine) -> &'static str {
    match engine {
        Engine::Claude => "claude update",
        Engine::Codex => "codex update",
        Engine::OpenCode => "opencode upgrade",
        Engine::Pi => "pi update",
        Engine::Muse => MUSE_INSTALLER,
    }
}

/// Runs the tool's updater (`program` is its CLI) from the home folder, with the user's PATH: Ok with its last line
/// when it says it worked, else what it said went wrong.
fn run_update(engine: Engine, program: &Path, path: &str) -> Result<Option<String>, String> {
    let mut command = match engine {
        Engine::Muse => {
            let mut command = std::process::Command::new("/bin/bash");
            command.args(["-o", "pipefail", "-c", MUSE_INSTALLER]);
            command
        }
        _ => {
            let mut command = std::process::Command::new(program);
            command.arg(if engine == Engine::OpenCode { "upgrade" } else { "update" });
            command
        }
    };
    command.env("PATH", path).env("NO_COLOR", "1");
    if let Some(home) = dirs::home_dir() {
        command.current_dir(home);
    }
    let name = update_command(engine);
    let ran = filo_local::engine::run_for(&mut command, None, UPDATE_TIMEOUT)
        .map_err(|error| format!("Couldn't run `{name}`: {error}"))?;
    let said = |bytes: &[u8]| last_lines(&String::from_utf8_lossy(bytes));
    match ran.status {
        None => Err(format!("`{name}` didn't finish in {} minutes.", UPDATE_TIMEOUT.as_secs() / 60)),
        Some(status) if status.success() => Ok(said(&ran.stdout).or_else(|| said(&ran.stderr))),
        Some(status) => Err(said(&ran.stderr)
            .or_else(|| said(&ran.stdout))
            .unwrap_or_else(|| format!("`{name}` failed ({status})."))),
    }
}

/// The last few lines a program printed, without its colors, cut to a few hundred characters: what the row shows.
fn last_lines(text: &str) -> Option<String> {
    let plain = strip_ansi(text);
    let lines: Vec<&str> = plain.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let said = lines[lines.len().saturating_sub(3)..].join(" ");
    let said = said.trim();
    match said.char_indices().nth(300) {
        _ if said.is_empty() => None,
        Some((cut, _)) => Some(format!("{}…", &said[..cut])),
        None => Some(said.to_string()),
    }
}

/// `text` without terminal escapes (colors, cursor moves).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            // A spinner's carriage returns start the line again: keep what it ended on.
            if c == '\r' && chars.peek() != Some(&'\n') {
                out.truncate(out.rfind('\n').map_or(0, |at| at + 1));
            } else {
                out.push(c);
            }
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            while chars.next().is_some_and(|c| !c.is_ascii_alphabetic()) {}
        } else {
            chars.next();
        }
    }
    out
}

/// Where the tool's own updater finds its newest version. `installed` picks OpenCode's line: 1.x updates within it.
fn latest_url(engine: Engine, installed: &str) -> String {
    match engine {
        // `claude update` follows the release channel in its settings ("latest" unless set to "stable").
        Engine::Claude => format!("https://downloads.claude.ai/claude-code-releases/{}", claude_channel()),
        Engine::Codex => "https://releases.openai.com/codex/channels/latest".into(),
        Engine::Muse => format!(
            "https://api.meta.ai/muse-code/channels/{}",
            std::env::var("MUSE_CHANNEL")
                .ok()
                .filter(|channel| !channel.is_empty())
                .as_deref()
                .unwrap_or("muse-stable")
        ),
        Engine::OpenCode if installed.starts_with("1.") => "https://registry.npmjs.org/opencode-ai/latest".into(),
        Engine::OpenCode => "https://opencode.ai/update/api/latest/cli/npm".into(),
        Engine::Pi => "https://pi.dev/api/installer/releases/latest".into(),
    }
}

/// Claude Code's release channel (`autoUpdatesChannel` in ~/.claude/settings.json).
fn claude_channel() -> &'static str {
    let settings = dirs::home_dir()
        .and_then(|home| std::fs::read(home.join(".claude/settings.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    match settings.as_ref().and_then(|settings| settings["autoUpdatesChannel"].as_str()) {
        Some("stable") => "stable",
        _ => "latest",
    }
}

/// The tool's newest version, given 15 seconds; None when it can't be had.
fn latest(engine: Engine, installed: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("filo-desktop")
        .timeout(Duration::from_secs(15))
        .build()
        .ok()?;
    let response = client.get(latest_url(engine, installed)).send().ok()?.error_for_status().ok()?;
    latest_in(engine, &response.text().ok()?)
}

/// The version in what `latest_url` answers: Claude Code's is the version alone, Codex's a GitHub-style release
/// ("rust-v0.163.0"), Muse Code's carries its build ("1.4.11-R6667.1", shown as 1.4.11), the others' a `version`.
fn latest_in(engine: Engine, body: &str) -> Option<String> {
    let version = match engine {
        Engine::Claude => body.trim().to_string(),
        _ => {
            let answer: Value = serde_json::from_str(body).ok()?;
            match engine {
                Engine::Codex => answer["tag_name"].as_str()?.trim_start_matches("rust-v").to_string(),
                Engine::Muse => answer["version"].as_str()?.split("-R").next()?.to_string(),
                _ => answer["version"].as_str()?.to_string(),
            }
        }
    };
    let version = version.trim_start_matches('v');
    (version.starts_with(|c: char| c.is_ascii_digit()) && version.contains('.') && version.len() < 40)
        .then(|| version.to_string())
}

/// Whether `latest` comes after `installed`: their numbers compared in turn ("1.10.0" after "1.9.3"), and a release
/// after a pre-release of the same numbers ("1.3.0" after "1.3.0-beta.2").
pub(super) fn newer(latest: &str, installed: &str) -> bool {
    fn parts(version: &str) -> (Vec<u64>, bool) {
        let version = version.trim().trim_start_matches('v');
        let (numbers, pre) = version.split_once('-').map_or((version, false), |(numbers, _)| (numbers, true));
        let numbers = numbers
            .split('.')
            .map(|part| part.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
            .collect();
        (numbers, !pre)
    }
    let ((mut latest, latest_release), (mut installed, installed_release)) = (parts(latest), parts(installed));
    let len = latest.len().max(installed.len());
    latest.resize(len, 0);
    installed.resize(len, 0);
    (latest, latest_release) > (installed, installed_release)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_backend::test_support::{call, script, temp};

    #[test]
    fn a_newer_version_is_one_with_a_bigger_number() {
        assert!(newer("1.3.0", "0.84.4"));
        assert!(newer("2.1.304", "2.1.302"));
        assert!(newer("1.10.0", "1.9.3"));
        assert!(newer("0.163.0", "0.162.1"));
        assert!(newer("1.3.0", "1.3.0-beta.2"));
        assert!(newer("1.3.1", "1.3"));
        assert!(!newer("1.3.0", "1.3.0"));
        assert!(!newer("1.3", "1.3.0"));
        assert!(!newer("2.0.16", "2.1.1"));
        assert!(!newer("1.3.0-beta.2", "1.3.0"));
    }

    #[test]
    fn reads_each_tools_newest_version() {
        assert_eq!(latest_in(Engine::Claude, "2.1.304\n").as_deref(), Some("2.1.304"));
        assert_eq!(latest_in(Engine::Claude, "<html>Not found</html>"), None);
        let codex = r#"{"tag_name":"rust-v0.163.0","assets":[{"name":"codex"}]}"#;
        assert_eq!(latest_in(Engine::Codex, codex).as_deref(), Some("0.163.0"));
        let muse = r#"{"channel":"muse-stable","version":"1.4.11-R6667.1","urgency":"none"}"#;
        assert_eq!(latest_in(Engine::Muse, muse).as_deref(), Some("1.4.11"));
        let opencode = r#"{"channel":"latest","name":"cli","distribution":"npm","version":"2.1.1"}"#;
        assert_eq!(latest_in(Engine::OpenCode, opencode).as_deref(), Some("2.1.1"));
        assert_eq!(latest_in(Engine::Pi, r#"{"schemaVersion":1,"version":"1.3.0"}"#).as_deref(), Some("1.3.0"));
        assert_eq!(latest_in(Engine::Pi, r#"{"error":"down"}"#), None);
        // OpenCode 1.x stays on its own line.
        assert!(latest_url(Engine::OpenCode, "1.19.0").contains("registry.npmjs.org/opencode-ai"));
        assert!(latest_url(Engine::OpenCode, "2.0.16").contains("opencode.ai/update"));
    }

    /// Each tool's newest version, asked where its updater asks: `cargo test --bin filo real_latest -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn real_latest_versions() {
        for (engine, installed) in [
            (Engine::Claude, "2.1.0"),
            (Engine::Codex, "0.1.0"),
            (Engine::Muse, "1.0.0"),
            (Engine::OpenCode, "2.0.0"),
            (Engine::OpenCode, "1.0.0"),
            (Engine::Pi, "0.1.0"),
        ] {
            let found = latest(engine, installed);
            println!("{:<12} from {installed}: {found:?}", engine.label());
            assert!(found.is_some_and(|found| newer(&found, installed)), "{engine:?}");
        }
    }

    #[test]
    fn an_update_runs_the_tools_own_updater() {
        let dir = std::env::temp_dir().join(format!("filo-tool-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pi = script(&dir, "pi", "#!/bin/sh\n[ \"$1\" = update ] && echo 'Updated pi to 1.3.0' && exit 0\nexit 9\n");
        assert_eq!(run_update(Engine::Pi, &pi, "/usr/bin:/bin").unwrap().as_deref(), Some("Updated pi to 1.3.0"));
        let opencode = script(&dir, "opencode", "#!/bin/sh\n[ \"$1\" = upgrade ] && exit 0\nexit 9\n");
        assert_eq!(run_update(Engine::OpenCode, &opencode, "/usr/bin:/bin").unwrap(), None);
        // A failure shows the tool's own words, without its colors.
        let codex = script(
            &dir,
            "codex",
            "#!/bin/sh\necho 'Checking for updates' \nprintf '\\033[31merror:\\033[0m could not reach releases.openai.com\\n' >&2\nexit 1\n",
        );
        assert_eq!(
            run_update(Engine::Codex, &codex, "/usr/bin:/bin").unwrap_err(),
            "error: could not reach releases.openai.com"
        );
        let silent = script(&dir, "claude", "#!/bin/sh\nexit 3\n");
        assert_eq!(
            run_update(Engine::Claude, &silent, "/usr/bin:/bin").unwrap_err(),
            "`claude update` failed (exit status: 3)."
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_spinners_lines_keep_what_they_ended_on() {
        assert_eq!(last_lines("Downloading 10%\rDownloading 100%\nDone\n").as_deref(), Some("Downloading 100% Done"));
        assert_eq!(last_lines("\n \n"), None);
    }

    /// The row shows the pill only for a newer version, a spinner while the updater runs, then the version it has or
    /// the updater's error.
    #[test]
    fn the_row_shows_an_update_until_the_tool_has_it() {
        let local = temp("tool-updates");
        let dir = local.root.join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        local
            .write_json(SAVED, &Saved { checked_at: now_ms(), latest: HashMap::from([("pi".into(), "1.3.0".into())]) })
            .unwrap();
        let fields = |version: &str| {
            let mut row = json!({});
            local.add_update_fields(&mut row, "pi", Some(version));
            row
        };
        assert_eq!(fields("0.84.4")["latestVersion"], "1.3.0");
        assert_eq!(fields("1.3.0").get("latestVersion"), None);
        // An updater that works: the version it reads after is the new one.
        let marker = dir.join("updated");
        let pi = script(
            &dir,
            "pi",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  update) sleep 0.3; touch '{0}' ;;\n  --version) [ -f '{0}' ] && echo 1.3.0 || echo 0.84.4 ;;\nesac\n",
                marker.display()
            ),
        );
        local.set_coding_program("pi", pi.clone());
        call(&local, "coding:updateTool", json!({ "tool": "pi" })).unwrap();
        assert_eq!(fields("0.84.4")["updating"], true);
        let settled = || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while local.coding.updates.updating.lock().unwrap().contains("pi") && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        settled();
        assert_eq!(fields("1.3.0"), json!({}));
        // One that fails says why, until the next try.
        script(
            &dir,
            "pi",
            "#!/bin/sh\n[ \"$1\" = update ] && echo 'npm ERR! EACCES: permission denied' >&2 && exit 1\necho 0.84.4\n",
        );
        call(&local, "coding:updateTool", json!({ "tool": "pi" })).unwrap();
        settled();
        assert_eq!(fields("0.84.4")["updateError"], "npm ERR! EACCES: permission denied");
        assert_eq!(fields("0.84.4")["latestVersion"], "1.3.0");
        // A tool Filo doesn't know isn't updated.
        assert!(call(&local, "coding:updateTool", json!({ "tool": "vim" })).is_err());
    }
}

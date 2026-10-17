// How much of a coding tool's plan is left, as its account says (the Coding page shows it under each tool): the 5-hour
// window and the week, each with the share used and when it resets. Read from the account, never estimated:
// - Claude Code: its account's usage (`GET api.anthropic.com/api/oauth/usage`: `five_hour` and `seven_day`), asked with
//   the sign-in Claude Code keeps in the login Keychain ("Claude Code-credentials", read with `/usr/bin/security`, as
//   Claude Code reads it itself, so macOS doesn't ask) or, without one, in `~/.claude/.credentials.json`. An expired
//   token is left alone (refreshing it here would sign Claude Code out): nothing shows until Claude Code refreshes it.
// - Codex: `account/rateLimits/read` on `codex app-server`, signed in with ChatGPT: its windows by their length (300
//   minutes, a week).
// - Muse Code's host has `usage/read`, but it answers only for a session that made a model call (a new host has
//   nothing), and OpenCode's providers report no plan windows: neither shows any.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

/// How long an ask may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The windows' lengths, in minutes.
const FIVE_HOURS: i64 = 5 * 60;
const WEEK: i64 = 7 * 24 * 60;

/// A usage window: the share of it used (0–100, over 100 past the limit) and when it resets (ms), when it says.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub used_percent: f64,
    pub resets_at: Option<i64>,
}

/// A tool's plan windows, those its account reports.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub week: Option<Window>,
}

impl Usage {
    fn some(self) -> Option<Usage> {
        (self.five_hour.is_some() || self.week.is_some()).then_some(self)
    }
}

/// Claude Code's plan usage, from its account; none when it isn't signed in with a Claude plan or can't be asked.
pub fn claude(now_ms: i64) -> Option<Usage> {
    let token = claude_token(now_ms)?;
    let client = reqwest::blocking::Client::builder().timeout(TIMEOUT).build().ok()?;
    let answer: Value = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("user-agent", concat!("filo-desktop/", env!("CARGO_PKG_VERSION")))
        .send()
        .ok()
        .filter(|answer| answer.status().is_success())?
        .json()
        .ok()?;
    claude_usage_in(&answer).some()
}

/// `oauth/usage`'s answer: `five_hour` and `seven_day`, each `{utilization, resets_at}` (RFC 3339, null for a window
/// nothing has started).
pub fn claude_usage_in(answer: &Value) -> Usage {
    let window = |window: &Value| {
        Some(Window {
            used_percent: window["utilization"].as_f64()?,
            resets_at: window["resets_at"]
                .as_str()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.timestamp_millis()),
        })
    };
    Usage { five_hour: window(&answer["five_hour"]), week: window(&answer["seven_day"]) }
}

/// Claude Code's folder: `$CLAUDE_CONFIG_DIR`, else `~/.claude`; and whether it was set.
fn claude_dir() -> (PathBuf, bool) {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => (PathBuf::from(dir), true),
        None => (std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".claude"), false),
    }
}

/// The access token of Claude Code's own sign-in, while it's good: its Keychain item (named for its folder when
/// `CLAUDE_CONFIG_DIR` is set), else its `.credentials.json`.
fn claude_token(now_ms: i64) -> Option<String> {
    use sha2::Digest as _;
    let (dir, custom) = claude_dir();
    let service = if custom {
        let hash = sha2::Sha256::digest(dir.to_string_lossy().as_bytes());
        format!("Claude Code-credentials-{}", &hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>()[..8])
    } else {
        "Claude Code-credentials".to_string()
    };
    let keychain = crate::engine::run_for(
        Command::new("/usr/bin/security").args(["find-generic-password", "-s", &service, "-w"]),
        None,
        TIMEOUT,
    )
    .ok()
    .filter(|ran| ran.status.is_some_and(|status| status.success()))
    .and_then(|ran| serde_json::from_slice::<Value>(&ran.stdout).ok());
    let credentials = keychain.or_else(|| {
        std::fs::read(dir.join(".credentials.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
    })?;
    claude_token_in(&credentials, now_ms)
}

/// `{claudeAiOauth: {accessToken, expiresAt}}`: the token, unless it has expired (or will within a minute).
fn claude_token_in(credentials: &Value, now_ms: i64) -> Option<String> {
    let oauth = &credentials["claudeAiOauth"];
    let expires_at = oauth["expiresAt"].as_i64().unwrap_or(i64::MAX);
    oauth["accessToken"].as_str().filter(|token| !token.is_empty() && expires_at > now_ms + 60_000).map(str::to_string)
}

/// Codex's plan windows, from `codex app-server` (`program`, on `path_env`); none when it's signed in with an API key or
/// can't say.
pub fn codex(program: &Path, path_env: Option<&str>) -> Option<Usage> {
    let mut command = crate::disclaim::command(program, false);
    // Without the user's MCP servers: nothing here uses them.
    command.args(["app-server", "-c", "mcp_servers={}"]);
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    let hello = json!({ "clientInfo": { "name": "filo_desktop", "version": env!("CARGO_PKG_VERSION") } });
    codex_usage_in(&crate::engine::ask_once(&mut command, hello, "account/rateLimits/read", Value::Null, TIMEOUT))
        .some()
}

/// `account/rateLimits/read`'s result: `rateLimits.primary` and `.secondary`, each `{usedPercent, windowDurationMins,
/// resetsAt}` (seconds), taken by their length.
pub fn codex_usage_in(result: &Value) -> Usage {
    let mut usage = Usage::default();
    for window in ["primary", "secondary"].map(|key| &result["rateLimits"][key]) {
        let Some(used_percent) = window["usedPercent"].as_f64() else { continue };
        let read = Some(Window { used_percent, resets_at: window["resetsAt"].as_i64().map(|at| at * 1000) });
        match window["windowDurationMins"].as_i64() {
            Some(FIVE_HOURS) => usage.five_hour = read,
            Some(WEEK) => usage.week = read,
            _ => {}
        }
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code's answer, as its account gave it on Oct 16: both windows with their resets, and one not started.
    #[test]
    fn reads_claude_codes_windows() {
        let answer = json!({
            "five_hour": { "utilization": 9.0, "resets_at": "2026-10-16T01:19:59.762960+00:00" },
            "seven_day": { "utilization": 6.0, "resets_at": null },
            "seven_day_opus": null,
        });
        assert_eq!(
            claude_usage_in(&answer),
            Usage {
                five_hour: Some(Window { used_percent: 9., resets_at: Some(1_792_113_599_762) }),
                week: Some(Window { used_percent: 6., resets_at: None })
            }
        );
        assert_eq!(claude_usage_in(&json!({ "error": "unauthorized" })).some(), None);
    }

    /// Claude Code's token is used only while it's good: an expired one is left for Claude Code to refresh.
    #[test]
    fn an_expired_claude_token_isnt_used() {
        let credentials =
            |expires_at: i64| json!({ "claudeAiOauth": { "accessToken": "sk-ant-oat", "expiresAt": expires_at } });
        assert_eq!(claude_token_in(&credentials(10_000_000), 1_000_000).as_deref(), Some("sk-ant-oat"));
        assert_eq!(claude_token_in(&credentials(1_030_000), 1_000_000), None);
        assert_eq!(claude_token_in(&json!({}), 1_000_000), None);
    }

    /// Codex's windows by their length (a plan with only a weekly one has it as `primary`).
    #[test]
    fn reads_codexs_windows() {
        let result = json!({ "rateLimits": {
            "primary": { "usedPercent": 38, "windowDurationMins": 300, "resetsAt": 1_792_121_614 },
            "secondary": { "usedPercent": 70, "windowDurationMins": 10080, "resetsAt": 1_792_677_567 },
        } });
        assert_eq!(
            codex_usage_in(&result),
            Usage {
                five_hour: Some(Window { used_percent: 38., resets_at: Some(1_792_121_614_000) }),
                week: Some(Window { used_percent: 70., resets_at: Some(1_792_677_567_000) }),
            }
        );
        let weekly = json!({ "rateLimits": { "primary": { "usedPercent": 5, "windowDurationMins": 10080, "resetsAt": 1 }, "secondary": null } });
        assert_eq!(
            codex_usage_in(&weekly),
            Usage { five_hour: None, week: Some(Window { used_percent: 5., resets_at: Some(1000) }) }
        );
        assert_eq!(codex_usage_in(&Value::Null).some(), None);
    }
}

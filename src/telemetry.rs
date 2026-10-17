// Anonymous usage counts (docs/telemetry.md): how many Macs installed Filo and how many open it each day, by country.
// Modelled on T3 Code's apps/server/src/telemetry, with only a random install ID (never a Claude or Codex account).
//
// Two events go to PostHog's /batch/ endpoint: `app_installed` once per install, and `app_opened` once on each day
// Filo is open. Each carries the install ID, `platform: "desktop"` (the project is shared with the phone and web apps),
// the build's channel, the app version, the OS and the architecture, and nothing else. PostHog works out the country
// from the request's IP (its GeoIP lookup); Filo doesn't send or keep the IP.
//
// On by default, like T3 Code's. Nothing is sent, and no install ID is made, when FILO_TELEMETRY=0 is set in Filo's
// environment or in the login shell's (`export FILO_TELEMETRY=0` in ~/.zshrc, read when Filo opens), in debug builds
// (unless FILO_TELEMETRY=1), in smoke and fixture runs, or when the build has no PostHog key: FILO_POSTHOG_KEY and
// FILO_POSTHOG_HOST are read when building (scripts/package.sh).
//
// What it remembers is in telemetry.json in the config folder. A send that fails is tried again an hour later.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config;

const POSTHOG_KEY: Option<&str> = option_env!("FILO_POSTHOG_KEY");
const POSTHOG_HOST: Option<&str> = option_env!("FILO_POSTHOG_HOST");
const DEFAULT_HOST: &str = "https://us.i.posthog.com";

/// `0` turns the counts off; `1` turns them on in a debug build.
const SWITCH: &str = "FILO_TELEMETRY";
/// Set in smoke and fixture runs, which send nothing.
const TEST_RUN: [&str; 4] = ["FILO_SMOKE", "FILO_OPEN", "FILO_FIXTURE_SHOTS", "FILO_FIXTURE_DRIVE"];

/// How often an open Filo checks whether a new day began (it can stay open for days in the menu bar).
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// The first check waits for the window, so starting up isn't slowed by it.
const FIRST_CHECK: Duration = Duration::from_secs(5);
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

const INSTALLED: &str = "app_installed";
const OPENED: &str = "app_opened";

/// Reads and writes of telemetry.json in this process, one at a time.
static STATE: Mutex<()> = Mutex::new(());

/// telemetry.json.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct State {
    /// A random UUID, made the first time something is sent.
    install_id: Option<String>,
    install_sent: bool,
    /// The local day (2026-10-12) `app_opened` was last sent for.
    last_opened: Option<String>,
}

fn path() -> PathBuf {
    config::config_dir().join("telemetry.json")
}

fn load_from(path: &Path) -> State {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save_to(path: &Path, state: &State) {
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = config::write_atomic(path, format!("{json}\n").as_bytes());
    }
}

/// Why this run sends nothing: None when it may send. FILO_TELEMETRY comes from Filo's environment, else the login
/// shell's (which takes seconds the first time, so not on the UI thread).
fn blocked() -> Option<&'static str> {
    blocked_by(POSTHOG_KEY, |name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| (name == SWITCH).then(|| crate::local_backend::login_shell_var(name)).flatten())
    })
}

fn blocked_by(key: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    if key.is_none_or(|key| key.trim().is_empty()) {
        return Some("this build has no analytics key");
    }
    if TEST_RUN.iter().any(|name| env(name).is_some_and(|value| !value.is_empty())) {
        return Some("this is a test run");
    }
    let forced = match env(SWITCH).map(|value| value.trim().to_ascii_lowercase()).as_deref() {
        Some("0" | "false" | "off" | "no") => return Some("FILO_TELEMETRY=0 is set"),
        Some("1" | "true" | "on" | "yes") => true,
        _ => false,
    };
    if cfg!(debug_assertions) && !forced {
        return Some("this is a development build");
    }
    None
}

/// Sends what's due now and checks again every hour while Filo is open.
pub fn start(runtime: &tokio::runtime::Handle, http: reqwest::Client) {
    // No key: no need to ask the login shell.
    let Some(key) = POSTHOG_KEY.filter(|key| !key.trim().is_empty()) else { return };
    runtime.spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        match tokio::task::spawn_blocking(blocked).await {
            Ok(None) => {}
            Ok(Some(reason)) => {
                eprintln!("telemetry: off, {reason}");
                return;
            }
            Err(_) => return,
        }
        let host = POSTHOG_HOST.filter(|host| !host.trim().is_empty()).unwrap_or(DEFAULT_HOST);
        loop {
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            if let Err(error) = send_due(&http, key, host, &path(), &today).await {
                eprintln!("telemetry: couldn't send: {error}");
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// The events due today: `app_installed` if it was never sent, `app_opened` if it wasn't sent today.
fn due(state: &State, today: &str) -> Vec<&'static str> {
    let mut events = Vec::new();
    if !state.install_sent {
        events.push(INSTALLED);
    }
    if state.last_opened.as_deref() != Some(today) {
        events.push(OPENED);
    }
    events
}

/// Sends the events due `today` and notes them in the state file at `path` once PostHog took them.
async fn send_due(http: &reqwest::Client, key: &str, host: &str, path: &Path, today: &str) -> reqwest::Result<()> {
    let (events, install_id) = {
        let _lock = STATE.lock();
        let mut state = load_from(path);
        let events = due(&state, today);
        if events.is_empty() {
            return Ok(());
        }
        let install_id = state.install_id.get_or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone();
        save_to(path, &state);
        (events, install_id)
    };
    let body = batch(key, &install_id, &events, &chrono::Utc::now().to_rfc3339());
    let url = format!("{}/batch/", host.trim_end_matches('/'));
    http.post(url).timeout(SEND_TIMEOUT).json(&body).send().await?.error_for_status()?;
    let _lock = STATE.lock();
    let mut state = load_from(path);
    if events.contains(&INSTALLED) {
        state.install_sent = true;
    }
    state.last_opened = Some(today.to_string());
    save_to(path, &state);
    Ok(())
}

/// Tells these events from the phone's and the web app's in the shared PostHog project.
const PLATFORM: &str = "desktop";

/// `stable` from the build's FILO_CHANNEL (scripts/package.sh); `dev` for a plain cargo build.
fn channel_of(built: Option<&str>) -> &str {
    built.filter(|channel| !channel.trim().is_empty()).unwrap_or("dev")
}

/// PostHog's batch body. `$process_person_profile: false` keeps PostHog from making a person profile for the ID.
fn batch(key: &str, install_id: &str, events: &[&str], timestamp: &str) -> serde_json::Value {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        other => other,
    };
    json!({
        "api_key": key,
        "batch": events.iter().map(|event| json!({
            "uuid": uuid::Uuid::new_v4().to_string(),
            "event": event,
            "distinct_id": install_id,
            "timestamp": timestamp,
            "properties": {
                "platform": PLATFORM,
                "channel": channel_of(crate::updater::CHANNEL),
                "app_version": crate::updater::VERSION,
                "os": os,
                "arch": std::env::consts::ARCH,
                "$process_person_profile": false,
            },
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
    }

    #[test]
    fn nothing_without_a_key_with_it_turned_off_or_in_test_runs() {
        assert!(blocked_by(None, env(&[("FILO_TELEMETRY", "1")])).is_some());
        assert!(blocked_by(Some(" "), env(&[("FILO_TELEMETRY", "1")])).is_some());
        assert_eq!(blocked_by(Some("phc_x"), env(&[("FILO_TELEMETRY", "0")])), Some("FILO_TELEMETRY=0 is set"));
        assert_eq!(blocked_by(Some("phc_x"), env(&[("FILO_TELEMETRY", "off")])), Some("FILO_TELEMETRY=0 is set"));
        assert!(blocked_by(Some("phc_x"), env(&[("FILO_TELEMETRY", "1"), ("FILO_SMOKE", "1")])).is_some());
        assert!(blocked_by(Some("phc_x"), env(&[("FILO_TELEMETRY", "1"), ("FILO_OPEN", "coding-cards")])).is_some());
        // Debug builds only with FILO_TELEMETRY=1; release builds by default.
        assert_eq!(blocked_by(Some("phc_x"), env(&[])).is_some(), cfg!(debug_assertions));
        assert_eq!(blocked_by(Some("phc_x"), env(&[("FILO_TELEMETRY", "1")])), None);
    }

    #[test]
    fn install_once_and_opened_once_a_day() {
        let mut state = State::default();
        assert_eq!(due(&state, "2026-10-12"), vec![INSTALLED, OPENED]);
        state.install_sent = true;
        state.last_opened = Some("2026-10-12".into());
        assert!(due(&state, "2026-10-12").is_empty());
        assert_eq!(due(&state, "2026-10-13"), vec![OPENED]);
    }

    #[test]
    fn events_carry_only_the_id_platform_channel_version_os_and_arch() {
        let body = batch("phc_x", "id-1", &[INSTALLED, OPENED], "2026-10-12T09:00:00+00:00");
        assert_eq!(body["api_key"], "phc_x");
        let events = body["batch"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        for event in events {
            assert_eq!(event["distinct_id"], "id-1");
            let mut keys: Vec<&str> = event["properties"].as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, ["$process_person_profile", "app_version", "arch", "channel", "os", "platform"]);
            assert_eq!(event["properties"]["platform"], "desktop");
            assert_eq!(event["properties"]["channel"], channel_of(crate::updater::CHANNEL));
        }
        assert_ne!(events[0]["uuid"], events[1]["uuid"]);
    }

    #[test]
    fn the_channel_is_the_builds_or_dev() {
        assert_eq!(channel_of(Some("stable")), "stable");
        assert_eq!(channel_of(None), "dev");
        assert_eq!(channel_of(Some("")), "dev");
    }

    /// A stand-in PostHog on localhost: answers each request with `status` and hands back what it was sent.
    async fn posthog(status: u16) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let (sent, received) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                // Headers, then a body of content-length bytes.
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|n| n.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= length || read == 0 {
                            sent.send(text).unwrap();
                            break;
                        }
                    }
                }
                let reply = format!("HTTP/1.1 {status} X\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}");
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (host, received)
    }

    #[tokio::test]
    async fn sends_install_then_one_open_a_day_and_retries_after_a_failure() {
        let dir = std::env::temp_dir().join(format!("filo-telemetry-{}", uuid::Uuid::new_v4()));
        let path = dir.join("telemetry.json");
        let http = reqwest::Client::new();

        // PostHog down: nothing is marked sent, so the next check tries again.
        let (down, mut tried) = posthog(503).await;
        assert!(send_due(&http, "phc_x", &down, &path, "2026-10-12").await.is_err());
        assert!(tried.recv().await.unwrap().starts_with("POST /batch/ "));
        let state = load_from(&path);
        assert!(!state.install_sent && state.last_opened.is_none());
        let install_id = state.install_id.clone().unwrap();

        let (host, mut received) = posthog(200).await;
        send_due(&http, "phc_x", &host, &path, "2026-10-12").await.unwrap();
        let request = received.recv().await.unwrap();
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        let events: Vec<&str> =
            body["batch"].as_array().unwrap().iter().map(|event| event["event"].as_str().unwrap()).collect();
        assert_eq!(events, [INSTALLED, OPENED]);
        assert_eq!(body["batch"][0]["distinct_id"], install_id.as_str());
        assert_eq!(body["batch"][0]["properties"]["platform"], "desktop");
        // No IP, account or anything else in the request.
        assert!(!request.contains("$ip") && !request.contains("geoip"));

        // Again the same day: nothing. The next day: app_opened alone, with the same ID.
        send_due(&http, "phc_x", &host, &path, "2026-10-12").await.unwrap();
        send_due(&http, "phc_x", &host, &path, "2026-10-13").await.unwrap();
        let request = received.recv().await.unwrap();
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["batch"].as_array().unwrap().len(), 1);
        assert_eq!(body["batch"][0]["event"], OPENED);
        assert_eq!(body["batch"][0]["distinct_id"], install_id.as_str());
        assert!(received.try_recv().is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

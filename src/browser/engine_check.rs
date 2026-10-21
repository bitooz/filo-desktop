// `FILO_OPEN=browser-engine-check` (`dev` builds, on a data folder of its own): Settings › Browser's switch between the
// engines end to end (browser/engine.rs), from an engine feed served here: `FILO_CHECK_ENGINES=<dir>` holds `v1` and
// `v2`, each a folder chromium/package.sh made with FILES_URL="" (the zip and chromium-<protocol>.json), v1 the feed's
// first and v2 its update. Run it with FILO_CHROMIUM_APP=none, so no engine built in target/cef stands in for the
// download. It opens two tabs in WebKit as an agent does, one signed in on the test site (browser/check.rs), switches to
// Chromium as the page's row does, and checks that each step showed (looking, downloading, checking, logins,
// restarting); that the engine went in, checked, with its version; that both tabs are Chromium's with their ids and
// owner, and the agent hears of it; that the sign-in came along (the lasting cookie and the session one); that the
// engine's helper processes run in a sandbox; that a parked page draws at 1x and one in the browser window at the
// window's scale; that the feed's newer engine downloads in the background and waits for the engine's next start; and
// back on WebKit, that a cookie Chromium got meanwhile came back and the engine stopped; then Remove. Each check prints
// a line; the process ends with 1 when any failed. `FILO_FIXTURE_SHOTS=<dir>`: the browser window on Chromium.

use std::ffi::c_char;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp};
use regex::Regex;
use serde_json::json;

use super::check::{ask, text};
use super::engine::{self, Busy, Kind};

unsafe extern "C" {
    /// libsandbox: non-zero when `pid` runs in a sandbox (no operation asked about).
    fn sandbox_check(pid: libc::pid_t, operation: *const c_char, kind: libc::c_int, ...) -> libc::c_int;
}

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        println!(
            "browser engine check: {} {name}: {}",
            if ok { "ok" } else { "FAILED" },
            detail.as_ref().replace('\n', " | ")
        );
        if !ok {
            self.failed += 1;
        }
    }
}

fn sleep(cx: &AsyncApp, ms: u64) -> gpui::Task<()> {
    cx.background_executor().timer(Duration::from_millis(ms))
}

/// Serves `dir`'s `v1` or `v2` (whichever `stage` says) at /feed/: the engine feed and its zip.
fn serve_feed(dir: PathBuf, stage: Arc<Mutex<&'static str>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("feed");
    let port = listener.local_addr().expect("port").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (dir, stage) = (dir.clone(), stage.clone());
            std::thread::spawn(move || {
                let mut first = String::new();
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                if reader.read_line(&mut first).is_err() {
                    return;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/");
                let name = path.strip_prefix("/feed/").filter(|name| !name.contains('/') && !name.contains(".."));
                let file = name.map(|name| dir.join(*stage.lock().unwrap()).join(name));
                match file.and_then(|file| std::fs::File::open(file).ok()) {
                    Some(mut file) => {
                        let size = file.metadata().map_or(0, |meta| meta.len());
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = std::io::copy(&mut file, &mut stream);
                    }
                    None => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            });
        }
    });
    port
}

/// The engine state's step, as a word.
fn step(busy: &Busy) -> &'static str {
    match busy {
        Busy::Looking => "looking",
        Busy::Downloading { .. } => "downloading",
        Busy::Checking => "checking",
        Busy::CopyingLogins => "logins",
        Busy::Restarting => "restarting",
    }
}

/// Switches to `kind` as Settings › Browser's row does, and waits for it: the steps it showed, the error and the note.
async fn switch(cx: &mut AsyncApp, kind: Kind) -> (Vec<&'static str>, Option<String>, Option<String>, Duration) {
    let started = Instant::now();
    let before = cx.update(|cx| engine::engines(cx).map_or(0, |engines| engines.read(cx).steps.len())).unwrap_or(0);
    cx.update(|cx| engine::choose(kind, cx)).ok();
    loop {
        let state = cx
            .update(|cx| {
                engine::engines(cx).map(|engines| {
                    let engines = engines.read(cx);
                    let mut seen: Vec<&'static str> = Vec::new();
                    for busy in &engines.steps[before.min(engines.steps.len())..] {
                        if seen.last() != Some(&step(busy)) {
                            seen.push(step(busy));
                        }
                    }
                    (seen, engines.busy.is_some(), engines.error.clone(), engines.note.clone())
                })
            })
            .ok()
            .flatten();
        let Some((seen, busy, error, note)) = state else {
            break (Vec::new(), Some("no engine state".into()), None, started.elapsed());
        };
        if !busy && (!seen.is_empty() || started.elapsed() > Duration::from_secs(3)) {
            break (seen, error, note, started.elapsed());
        }
        if started.elapsed() > Duration::from_secs(600) {
            break (Vec::new(), Some("still going after 10 minutes".into()), None, started.elapsed());
        }
        sleep(cx, 50).await;
    }
}

/// The opened tab's id in an answer to `open` ("t3").
fn opened(answer: &str) -> String {
    Regex::new(r"\b(t\d+)\b")
        .ok()
        .and_then(|found| found.captures(answer).map(|found| found[1].to_string()))
        .unwrap_or_default()
}

/// The engine's processes started from this run's data folder: pid and whether each runs in a sandbox.
fn engine_processes() -> Vec<(u32, String, bool)> {
    let folder = crate::config::config_dir().join("local/chromium-engine");
    // The engine has its folder as Filo named it (/tmp/…), its helpers by its real path (/private/tmp/…).
    let folders =
        [folder.display().to_string(), std::fs::canonicalize(&folder).unwrap_or(folder).display().to_string()];
    let Ok(listing) = std::process::Command::new("/bin/ps").args(["-axo", "pid=,command="]).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|line| folders.iter().any(|folder| line.contains(folder.as_str())))
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            let pid: u32 = pid.parse().ok()?;
            let kind =
                command.split("--type=").nth(1).and_then(|rest| rest.split_whitespace().next()).unwrap_or("main");
            // SAFETY: a libsandbox query about another process.
            let sandboxed = unsafe { sandbox_check(pid as libc::pid_t, std::ptr::null(), 0) } != 0;
            Some((pid, kind.to_string(), sandboxed))
        })
        .collect()
}

pub fn open(cx: &mut App) {
    let Some(engines) = std::env::var("FILO_CHECK_ENGINES").ok().filter(|dir| !dir.is_empty()).map(PathBuf::from)
    else {
        println!("browser engine check: FAILED FILO_CHECK_ENGINES isn't set");
        std::process::exit(1);
    };
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    let stage = Arc::new(Mutex::new("v1"));
    let feed_port = serve_feed(engines.clone(), stage.clone());
    engine::use_feed(&format!("http://127.0.0.1:{feed_port}/feed"));
    let site = format!("http://127.0.0.1:{}", super::check::serve());
    let feed_version = |stage: &str| {
        let feed = std::fs::read_dir(engines.join(stage)).ok().and_then(|entries| {
            entries.flatten().map(|entry| entry.path()).find(|path| path.extension().is_some_and(|ext| ext == "json"))
        });
        feed.and_then(|feed| std::fs::read(feed).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|feed| Some((feed["version"].as_str()?.to_string(), feed["chromium"].as_str()?.to_string())))
            .unwrap_or_default()
    };
    let (first, second) = (feed_version("v1"), feed_version("v2"));
    cx.spawn(async move |cx| {
        let mut checks = Checks { failed: 0 };
        sleep(cx, 1500).await;
        checks.check(
            "WebKit to begin with",
            engine::current() == Kind::WebKit && engine::installed().is_none(),
            format!("engine {:?}, installed {:?}", engine::current(), engine::installed()),
        );

        // Two of an agent's tabs in WebKit, one signed in.
        let signed =
            opened(&text(&ask(cx, json!({ "op": "open", "url": format!("{site}/login"), "agent": "Check" })).await));
        let other =
            opened(&text(&ask(cx, json!({ "op": "open", "url": format!("{site}/form"), "agent": "Check" })).await));
        sleep(cx, 1000).await;
        let engines_of = |ids: &[&str]| ids.iter().map(|id| super::tab(id).map(|tab| tab.engine())).collect::<Vec<_>>();
        checks.check(
            "two tabs open in WebKit",
            engines_of(&[&signed, &other]) == vec![Some(Kind::WebKit); 2],
            format!("{signed} {other}"),
        );

        // Chromium, as the page's row picks it: downloaded, checked, the logins brought over, the tabs opened again.
        let (seen, error, note, took) = switch(cx, Kind::Chromium).await;
        checks.check(
            "the switch to Chromium went through",
            error.is_none(),
            format!("{error:?}; note {note:?}; took {:.1} s", took.as_secs_f64()),
        );
        checks.check(
            "it showed each step",
            ["looking", "downloading", "checking", "logins", "restarting"].iter().all(|step| seen.contains(step)),
            seen.join(" → "),
        );
        let installed = engine::installed();
        checks.check(
            "the feed's engine went in",
            installed.as_ref().is_some_and(|installed| installed.version == first.0 && installed.chromium == first.1),
            format!("{installed:?}, feed {first:?}"),
        );
        checks.check(
            "Chromium is the engine now",
            engine::current() == Kind::Chromium && engine::chosen() == Kind::Chromium,
            format!("{:?}", engine::current()),
        );
        let tabs = super::tabs();
        checks.check(
            "both tabs opened again in Chromium, with their ids and owner",
            engines_of(&[&signed, &other]) == vec![Some(Kind::Chromium); 2]
                && tabs.len() == 2
                && tabs.iter().all(|tab| tab.owner.as_ref().is_some_and(|owner| owner.agent == "Check")),
            tabs.iter()
                .map(|tab| format!("{} {:?} {:?}", tab.id, tab.engine(), tab.state().1))
                .collect::<Vec<_>>()
                .join(", "),
        );
        sleep(cx, 1500).await;
        let read = text(&ask(cx, json!({ "op": "read", "tab": signed })).await);
        checks.check(
            "the agent hears its tab restarted",
            read.contains("restarted on Chromium"),
            read.lines().find(|line| line.contains("restarted")).unwrap_or(&read[..read.len().min(200)]),
        );
        ask(cx, json!({ "op": "navigate", "tab": signed, "url": format!("{site}/whoami") })).await.ok();
        let whoami = text(&ask(cx, json!({ "op": "read", "tab": signed })).await);
        checks.check(
            "the sign-in came over (lasting and session cookies)",
            whoami.contains("filo_lasting=yes") && whoami.contains("filo_session=yes"),
            whoami.lines().find(|line| line.contains("cookies:")).unwrap_or("no cookies line"),
        );

        let processes = engine_processes();
        let helpers: Vec<_> = processes.iter().filter(|(_, kind, _)| kind != "main").collect();
        checks.check(
            "the engine's helpers run in a sandbox",
            !helpers.is_empty() && helpers.iter().all(|(_, _, sandboxed)| *sandboxed),
            processes
                .iter()
                .map(|(pid, kind, sandboxed)| {
                    format!("{pid} {kind} {}", if *sandboxed { "sandboxed" } else { "NOT sandboxed" })
                })
                .collect::<Vec<_>>()
                .join(", "),
        );

        // Parked at 1x; in the browser window at the window's scale; parked again at 1x.
        let parked = super::chromium_drawn(&other);
        checks.check(
            "a parked page draws at 1x",
            parked.is_some_and(|(width, scale, frame)| scale == 1. && frame == width as usize),
            format!("{parked:?}"),
        );
        cx.update(|cx| super::window::open(Some(other.clone()), cx)).ok();
        sleep(cx, 2500).await;
        let shown = super::chromium_drawn(&other);
        let window_scale = super::tab(&other).map(|tab| {
            // SAFETY: AppKit on the main thread; the page's view is in the browser window.
            unsafe {
                let window: *mut objc2::runtime::AnyObject = objc2::msg_send![tab.view(), window];
                if window.is_null() { 0. } else { objc2::msg_send![window, backingScaleFactor] }
            }
        });
        checks.check(
            "in the browser window it draws at the window's scale",
            shown.zip(window_scale).is_some_and(|((width, scale, frame), window)| {
                scale == window && frame == (width * scale).round() as usize
            }),
            format!("{shown:?}, window {window_scale:?}"),
        );
        if let Some(dir) = &shots {
            crate::window_shot::save_largest(format!("{dir}/engine-chromium-window.png")).await;
        }
        cx.update(super::window::close).ok();
        sleep(cx, 1500).await;
        let back = super::chromium_drawn(&other);
        checks.check(
            "parked again, 1x again",
            back.is_some_and(|(_, scale, frame)| scale == 1. && frame == 1280),
            format!("{back:?}"),
        );

        // The feed's next engine: downloaded and checked in the background, in when the engine next starts.
        *stage.lock().unwrap() = "v2";
        cx.update(engine::look_for_update).ok();
        let started = Instant::now();
        let mut updated = None;
        while started.elapsed() < Duration::from_secs(600) {
            sleep(cx, 500).await;
            updated =
                cx.update(|cx| engine::engines(cx).and_then(|engines| engines.read(cx).updated.clone())).ok().flatten();
            if updated.is_some() {
                break;
            }
        }
        let installed = engine::installed();
        checks.check(
            "a newer engine downloads in the background",
            updated.is_some() && installed.as_ref().is_some_and(|installed| installed.version == second.0),
            format!(
                "updated {updated:?}, installed {installed:?}, feed {second:?}, {:.1} s",
                started.elapsed().as_secs_f64()
            ),
        );
        let running = engine_processes().into_iter().find(|(_, kind, _)| kind == "main").map(|(pid, _, _)| pid);
        let running_from = running
            .and_then(|pid| {
                std::process::Command::new("/bin/ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()
            })
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default();
        checks.check(
            "the running engine carries on until it next starts",
            running_from.contains(&format!("chromium-engine/{}/", first.0))
                && engine::app_path().is_some_and(|app| app.display().to_string().contains(&second.0)),
            format!("running {running_from:?}, next {:?}", engine::app_path()),
        );

        // A sign-in in Chromium, then back to WebKit: it comes along, and the engine stops.
        let again =
            text(&ask(cx, json!({ "op": "navigate", "tab": signed, "url": format!("{site}/login-again") })).await);
        checks.check(
            "signed in again in Chromium",
            again.contains("Signed in again"),
            again.lines().next().unwrap_or_default(),
        );
        let (seen, error, note, took) = switch(cx, Kind::WebKit).await;
        checks.check(
            "the switch back to WebKit went through",
            error.is_none() && engine::current() == Kind::WebKit,
            format!("{} · {error:?} · {note:?} · {:.1} s", seen.join(" → "), took.as_secs_f64()),
        );
        checks.check(
            "both tabs are WebKit's again",
            engines_of(&[&signed, &other]) == vec![Some(Kind::WebKit); 2],
            format!("{:?}", engines_of(&[&signed, &other])),
        );
        ask(cx, json!({ "op": "navigate", "tab": signed, "url": format!("{site}/whoami") })).await.ok();
        let whoami = text(&ask(cx, json!({ "op": "read", "tab": signed })).await);
        checks.check(
            "Chromium's new sign-in came back to WebKit",
            whoami.contains("filo_again=yes") && whoami.contains("filo_lasting=yes"),
            whoami.lines().find(|line| line.contains("cookies:")).unwrap_or("no cookies line"),
        );
        let stopping = Instant::now();
        let mut left = engine_processes();
        while !left.is_empty() && stopping.elapsed() < Duration::from_secs(30) {
            sleep(cx, 500).await;
            left = engine_processes();
        }
        checks.check(
            "the engine stopped",
            left.is_empty(),
            format!("{left:?} after {:.1} s", stopping.elapsed().as_secs_f64()),
        );
        let removed = cx.update(engine::remove).ok().map(|result| result.map(|()| engine::installed().is_none()));
        checks.check("Remove deletes it", removed == Some(Ok(true)), format!("{removed:?}"));

        println!(
            "browser-engine-check: {}",
            if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) }
        );
        crate::window_shot::flush(cx.background_executor()).await;
        super::remove_test_store(cx).await;
        cx.update(|_| super::quit()).ok();
        sleep(cx, 1500).await;
        std::process::exit(if checks.failed == 0 { 0 } else { 1 });
    })
    .detach();
}

// Updates for packaged builds (scripts/package.sh), the way Zed, Arc and Slack do them. A build knows its build number
// and the feed it came from: the newest GitHub Release's downloads (.github/workflows/release.yml), whose latest.json
// names that build and links its zip. Filo checks it at launch, every hour, after the Mac wakes and when
// its window becomes active (at most every 10 minutes); the sidebar button above Settings checks it too. A newer build
// downloads in the background into a folder next to the app, where its SHA-256 and its signature (the same key as this
// app's) are checked; then the button turns into "Restart to update" and a silent banner says so, with a Restart button
// (notifications.rs `show_update`). Restarting swaps the app bundle in place and opens the new one; quit
// first and it goes in once Filo has quit (`install_on_quit`, a `filo --install-update` helper). Both wait for chat
// replies under way to finish; coding tasks carry on in their workers and the new Filo picks them up. Settings ›
// Updates turns the background download off: the button then says "Update", the banner says the build is available,
// with an Update button, and either downloads, installs and restarts at once. One banner per build either way
// (`announce`).
// Local builds have no feed, so no button, unless FILO_UPDATE_FEED points at one (`dev` builds read it at run time).

use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, Global, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window,
    div, prelude::FluentBuilder as _, px,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::backend::{Backend, backend};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// `stable` in packaged builds; None in local ones.
pub const CHANNEL: Option<&str> = option_env!("FILO_CHANNEL");
const BUILD: Option<&str> = option_env!("FILO_BUILD");
const BUILT_FEED: Option<&str> = option_env!("FILO_UPDATE_FEED");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const FIRST_CHECK: Duration = Duration::from_secs(8);
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// The window becoming active checks again once the last check is this old.
const CHECK_WHEN_ACTIVE: Duration = Duration::from_secs(10 * 60);
/// How often the clock is looked at: a jump past this (and a minute) is the Mac waking from sleep.
const TICK: Duration = Duration::from_secs(30);
/// How often a restart waiting for chat replies looks again.
const REPLIES_POLL: Duration = Duration::from_secs(2);
/// The longest the install on quit waits for chat replies before it goes in anyway.
const REPLIES_WAIT: Duration = Duration::from_secs(2 * 60 * 60);
/// Staging folders next to the app, cleaned up at the next launch.
const STAGING_PREFIX: &str = ".filo-update-";
/// What Filo keeps about updates, in its config folder.
const SETTINGS: &str = "updates.json";
/// The helper that installs a downloaded build once Filo has quit (`install_main`).
pub const INSTALL_ARG: &str = "--install-update";

/// This build's number; 0 for local builds, so any feed build is newer.
pub fn build_number() -> u64 {
    BUILD.and_then(|build| build.parse().ok()).unwrap_or(0)
}

/// Where this build looks for updates: `https://github.com/bitooz/filo-desktop/releases/latest/download`, as it was
/// built (or, in `dev` builds, `FILO_UPDATE_FEED` at run time).
pub fn feed() -> Option<String> {
    crate::test_run::dev_override("FILO_UPDATE_FEED")
        .or_else(|| BUILT_FEED.map(str::to_string))
        .map(|feed| feed.trim_end_matches('/').to_string())
}

/// "0.1.0 (42)", or "0.1.0 · local build".
pub fn version_label() -> String {
    match CHANNEL {
        Some(_) => format!("{VERSION} ({})", build_number()),
        None => format!("{VERSION} · local build"),
    }
}

/// `updates.json`: Settings › Updates' switch, and the last build a banner announced (one per build).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    auto_download: bool,
    announced_build: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { auto_download: true, announced_build: 0 }
    }
}

/// Whether new builds download on their own (Settings › Updates; on unless switched off).
pub fn auto_download() -> bool {
    crate::config::load_settings::<Settings>(SETTINGS).auto_download
}

/// Settings › Updates' switch: turned on, a build already found starts downloading.
pub fn set_auto_download(on: bool, cx: &mut App) -> Result<(), String> {
    let mut settings = crate::config::load_settings::<Settings>(SETTINGS);
    settings.auto_download = on;
    crate::config::save_settings(SETTINGS, &settings)?;
    eprintln!("update: automatic downloads {}", if on { "on" } else { "off" });
    if let Some(updater) = updater(cx).filter(|_| on) {
        updater.update(cx, |updater, cx| {
            if let State::Available(release) = updater.state.clone() {
                updater.fetch(release, false, cx);
            }
        });
    }
    Ok(())
}

/// The feed's latest.json.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub build: u64,
    /// What `filo --update` tells (`dev` builds).
    #[cfg(feature = "dev")]
    #[serde(default)]
    pub commit: String,
    #[cfg(feature = "dev")]
    #[serde(default)]
    pub published_at: String,
    /// The zip: its URL (a release's own download, so a newer release published meanwhile can't change it), or its
    /// name next to latest.json.
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

impl Release {
    pub fn label(&self) -> String {
        format!("Filo {} ({})", self.version, self.build)
    }

    /// Where the zip downloads from.
    fn zip_url(&self, feed: &str) -> String {
        if self.file.contains("://") { self.file.clone() } else { format!("{feed}/{}", self.file) }
    }

    /// The zip's own name, for the staging folder.
    fn zip_name(&self) -> &str {
        self.file.rsplit('/').next().unwrap_or(&self.file)
    }
}

pub async fn fetch_release(http: reqwest::Client, feed: String) -> Result<Release> {
    let response = http.get(format!("{feed}/latest.json")).header("cache-control", "no-cache").send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("nothing has been released yet");
    }
    Ok(response.error_for_status()?.json().await?)
}

/// The .app this binary runs from; None for a bare binary (cargo run).
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors().find(|path| path.extension().is_some_and(|ext| ext == "app")).map(Path::to_path_buf)
}

/// Removes what an earlier update left next to the app.
pub fn cleanup() {
    let Some(parent) = current_bundle().and_then(|bundle| bundle.parent().map(Path::to_path_buf)) else { return };
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(STAGING_PREFIX) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Downloads the release's zip into `staging`, checking its size and SHA-256 as it arrives.
pub async fn download(
    http: reqwest::Client,
    feed: String,
    release: Release,
    staging: PathBuf,
    progress: Arc<AtomicU64>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("Filo can’t write to {}", staging.parent().unwrap_or(&staging).display()))?;
    let path = staging.join(release.zip_name());
    fetch_checked(http, &release.zip_url(&feed), release.size, &release.sha256, &path, progress).await?;
    Ok(path)
}

/// Downloads `url` into `path`, checking its size and SHA-256 as it arrives; `progress` is the bytes so far. Filo's
/// updates and the Chromium engine's (browser/engine.rs).
pub async fn fetch_checked(
    http: reqwest::Client,
    url: &str,
    size: u64,
    sha256: &str,
    path: &Path,
    progress: Arc<AtomicU64>,
) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    let mut response = http.get(url).send().await?.error_for_status()?;
    let mut hash = Sha256::new();
    let mut received = 0u64;
    while let Some(chunk) = response.chunk().await? {
        hash.update(&chunk);
        file.write_all(&chunk)?;
        received += chunk.len() as u64;
        progress.store(received, Ordering::Relaxed);
    }
    file.flush()?;
    if received != size {
        bail!("the download was cut short ({received} of {size} bytes)");
    }
    let got = hex::encode(hash.finalize());
    if !got.eq_ignore_ascii_case(sha256) {
        bail!("the download doesn’t match the release (SHA-256 {got})");
    }
    Ok(())
}

pub(crate) fn run(command: &mut Command) -> Result<String> {
    let output = command.output().with_context(|| format!("couldn’t run {command:?}"))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    // codesign prints what it displays on stderr, and some of it on stdout.
    Ok(format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

/// The code requirement updates must meet: this app's designated requirement when it is signed with a key
/// (ad-hoc local builds have none worth keeping, as theirs names this exact binary).
fn designated_requirement(bundle: &Path) -> Option<String> {
    let shown = run(Command::new("codesign").args(["-d", "-r-"]).arg(bundle)).ok()?;
    let requirement = shown.lines().find_map(|line| line.trim().strip_prefix("designated => "))?.to_string();
    requirement.contains("certificate").then_some(requirement)
}

/// Unzips the download in `staging` and checks the app in it; returns the new .app.
pub fn unpack(zip: &Path, staging: &Path, current: Option<&Path>) -> Result<PathBuf> {
    let unpacked = staging.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    run(Command::new("ditto").args(["-x", "-k"]).arg(zip).arg(&unpacked))?;
    let app = std::fs::read_dir(&unpacked)?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or_else(|| anyhow!("the download has no app in it"))?;
    run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&app))
        .context("the new app’s signature is broken")?;
    if let Some(requirement) = current.and_then(designated_requirement) {
        run(Command::new("codesign").arg("--verify").arg(format!("-R={requirement}")).arg(&app))
            .context("the new app isn’t signed by the same key as this one")?;
    }
    Ok(app)
}

/// Puts `new_app` where `bundle` is; the old one goes into `staging`, removed at the next launch.
pub fn swap(bundle: &Path, new_app: &Path, staging: &Path) -> Result<()> {
    let old = staging.join("previous.app");
    std::fs::rename(bundle, &old).with_context(|| format!("couldn’t move {} aside", bundle.display()))?;
    if let Err(error) = std::fs::rename(new_app, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(error).context("couldn’t put the new version in place");
    }
    Ok(())
}

/// Opens `bundle` once this process has quit, with the `FILO_*` variables this run has (none in a normal launch; a test
/// run's own data folder) and `FILO_LOG` getting its output.
fn relaunch(bundle: &Path) -> Result<()> {
    let script = format!("while kill -0 {} 2>/dev/null; do sleep 0.1; done; exec open \"$@\"", std::process::id());
    let mut args: Vec<String> = Vec::new();
    for (name, value) in std::env::vars().filter(|(name, _)| name.starts_with("FILO_")) {
        // The new version starts like any launch, without the debug settings this one may have had.
        if !matches!(name.as_str(), "FILO_SMOKE" | "FILO_OPEN") {
            args.extend(["--env".into(), format!("{name}={value}")]);
        }
        if name == "FILO_LOG" {
            args.extend(["--stdout".into(), value.clone(), "--stderr".into(), value]);
        }
    }
    args.push(bundle.display().to_string());
    // `open` hands its own environment on too.
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("filo-relaunch")
        .args(args)
        .env_remove("FILO_SMOKE")
        .env_remove("FILO_OPEN")
        .spawn()?;
    Ok(())
}

/// A build downloaded and checked, waiting next to the app to go in: on a restart, or once Filo has quit.
#[derive(Clone, Debug)]
struct Staged {
    release: Release,
    /// The new .app, in `staging`.
    app: PathBuf,
    staging: PathBuf,
    /// The app it replaces.
    bundle: PathBuf,
}

/// The build waiting to go in; the quit hook reads it (`install_on_quit`).
static STAGED: Mutex<Option<Staged>> = Mutex::new(None);

fn staged() -> Option<Staged> {
    STAGED.lock().unwrap().clone()
}

/// Filo quits with a build waiting: a helper puts it in place once this process is gone and the chat replies under way
/// have finished. Nothing when the updater itself restarts Filo (it took the build).
pub fn install_on_quit() {
    let Some(staged) = STAGED.lock().unwrap().take() else { return };
    match spawn_installer(&staged) {
        Ok(()) => eprintln!("update: {} goes in once Filo has quit", staged.release.label()),
        Err(error) => eprintln!("update: couldn’t start installing {} on quit: {error:#}", staged.release.label()),
    }
}

fn spawn_installer(staged: &Staged) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(INSTALL_ARG)
        .arg("--pid")
        .arg(std::process::id().to_string())
        .arg("--app")
        .arg(&staged.app)
        .arg("--staging")
        .arg(&staged.staging)
        .arg("--bundle")
        .arg(&staged.bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // In a session of its own, so the app quitting (or its terminal closing) doesn't take it along.
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn()?;
    Ok(())
}

/// `filo --install-update --pid <app> --app <new .app> --staging <dir> --bundle <.app>`: waits for the app to quit and
/// for the chat replies running in workers to finish, then swaps the bundle. If Filo opens again first, it leaves it
/// (that Filo finds the build again). Logs to `updates.log` in the config folder.
pub fn install_main(args: &[String]) -> i32 {
    let value = |name: &str| args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).cloned();
    let (Some(pid), Some(app), Some(staging), Some(bundle)) =
        (value("--pid").and_then(|pid| pid.parse::<i32>().ok()), value("--app"), value("--staging"), value("--bundle"))
    else {
        eprintln!("filo {INSTALL_ARG}: needs --pid, --app, --staging and --bundle");
        return 2;
    };
    let log = |line: String| {
        let path = crate::config::config_dir().join("updates.log");
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    };
    let quitting = Instant::now();
    // SAFETY: signal 0 only asks whether the process is there.
    while unsafe { libc::kill(pid, 0) } == 0 {
        if quitting.elapsed() > Duration::from_secs(120) {
            log(format!("Filo (pid {pid}) didn’t quit; {app} stays in {staging}"));
            return 1;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let root = crate::config::config_dir().join("local");
    let waiting = Instant::now();
    let mut said = false;
    loop {
        if crate::local_backend::window_open(&root) {
            log("Filo opened again before the update went in: it’ll find the build again".into());
            return 0;
        }
        let replies = crate::local_backend::replies_running(&root);
        if replies == 0 || waiting.elapsed() > REPLIES_WAIT {
            break;
        }
        if !said {
            log(format!("waiting for {replies} chat {} to finish", if replies == 1 { "reply" } else { "replies" }));
            said = true;
        }
        std::thread::sleep(REPLIES_POLL);
    }
    match swap(Path::new(&bundle), Path::new(&app), Path::new(&staging)) {
        Ok(()) => {
            log(format!("installed {app} as {bundle} after Filo quit"));
            0
        }
        Err(error) => {
            log(format!("couldn’t install {app}: {error:#}"));
            1
        }
    }
}

/// Chat replies running now, in the app or in workers (`coding:keepingAwake`'s "turn:" work).
async fn replies_running(backend: &Backend) -> usize {
    let status = backend.call_raw("coding:keepingAwake", json!({})).await.unwrap_or_default();
    let work = status.get("work").and_then(Value::as_array).cloned().unwrap_or_default();
    work.iter().filter(|work| work.as_str().is_some_and(|work| work.starts_with("turn:"))).count()
}

#[derive(Clone, Debug)]
enum State {
    Idle,
    Checking,
    /// A newer build, not downloaded: automatic downloads are off, or the last one failed.
    Available(Release),
    /// `asked`: clicked (it then restarts into it); else in the background, where the button stays as it was.
    Downloading {
        release: Release,
        received: u64,
        asked: bool,
    },
    /// Downloaded and checked (`STAGED`): "Restart to update".
    Ready(Release),
    /// Restart clicked while chat replies run: it restarts once they've finished.
    Waiting(Release),
    Restarting,
}

pub enum Checked {
    UpToDate,
    Available(Release),
    Busy,
    Failed(String),
}

struct UpdaterGlobal(WeakEntity<Updater>);

impl Global for UpdaterGlobal {}

/// The app's updater, in builds with a `feed()`.
pub fn updater(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<UpdaterGlobal>().and_then(|global| global.0.upgrade())
}

/// The update button, and the state behind it. One lives in the sidebar.
pub struct Updater {
    feed: String,
    state: State,
    /// The window its messages show in.
    window: AnyWindowHandle,
    last_check: Option<SystemTime>,
    /// The build downloading (in the background or not).
    downloading: Option<u64>,
    _checks: Task<()>,
    _banner_actions: Option<Task<()>>,
    _activation: gpui::Subscription,
    _download: Option<Task<()>>,
    _restart: Option<Task<()>>,
}

impl Updater {
    /// For builds with a `feed()`.
    pub fn new(feed: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let this = cx.entity().downgrade();
        cx.set_global(UpdaterGlobal(this));
        eprintln!("update: this is {} from {:?}; feed {feed}", version_label(), current_bundle());
        cx.background_spawn(async { cleanup() }).detach();
        // At launch, then every hour and after the Mac wakes: the timer doesn't run while it sleeps, the clock does.
        let _checks = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            let mut reason = "at launch";
            loop {
                let Ok(check) = this.update(cx, |this, cx| {
                    eprintln!("update: checking {reason}");
                    this.check(cx)
                }) else {
                    break;
                };
                check.await;
                let mut ticked = SystemTime::now();
                reason = loop {
                    cx.background_executor().timer(TICK).await;
                    let now = SystemTime::now();
                    let woke = now.duration_since(ticked).unwrap_or_default() > TICK + Duration::from_secs(60);
                    ticked = now;
                    let Ok(due) = this.update(cx, |this, _| this.since_check() >= CHECK_EVERY) else { return };
                    if woke {
                        break "after the Mac woke";
                    }
                    if due {
                        break "hourly";
                    }
                };
            }
        });
        // After the launch check.
        let _activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && this.last_check.is_some() && this.since_check() >= CHECK_WHEN_ACTIVE {
                eprintln!("update: checking as Filo became active");
                this.check(cx).detach();
            }
        });
        // Restart or Update on the update's banner.
        let _banner_actions = crate::notifications::update_actions().map(|mut actions| {
            cx.spawn(async move |this, cx| {
                use futures::StreamExt as _;
                while let Some(action) = actions.next().await {
                    eprintln!("update: {action:?} clicked on the banner");
                    if this.update(cx, |this, cx| this.update_now(cx)).is_err() {
                        break;
                    }
                }
            })
        });
        Updater {
            feed,
            state: State::Idle,
            window: window.window_handle(),
            last_check: None,
            downloading: None,
            _checks,
            _banner_actions,
            _activation,
            _download: None,
            _restart: None,
        }
    }

    fn since_check(&self) -> Duration {
        self.last_check.map(|at| SystemTime::now().duration_since(at).unwrap_or_default()).unwrap_or(Duration::MAX)
    }

    /// A newer build found and not downloaded (`dev` checks).
    #[cfg(feature = "dev")]
    pub fn available(&self) -> Option<Release> {
        match &self.state {
            State::Available(release) => Some(release.clone()),
            _ => None,
        }
    }

    /// A downloaded build is waiting to go in (`dev` checks).
    #[cfg(feature = "dev")]
    pub fn ready(&self) -> Option<Release> {
        match &self.state {
            State::Ready(release) | State::Waiting(release) => Some(release.clone()),
            _ => None,
        }
    }

    pub fn check(&mut self, cx: &mut Context<Self>) -> Task<Checked> {
        if !matches!(self.state, State::Idle | State::Available(_) | State::Ready(_)) {
            return Task::ready(Checked::Busy);
        }
        self.last_check = Some(SystemTime::now());
        let previous = self.state.clone();
        // A build ready to go in stays on the button while the feed is asked again.
        if !matches!(previous, State::Ready(_)) {
            self.state = State::Checking;
            cx.notify();
        }
        let backend = backend(cx);
        let fetch = backend.run(fetch_release(backend.http(), self.feed.clone()));
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                let checked = match result {
                    Ok(release) if release.build > build_number() => {
                        let have = match &previous {
                            State::Ready(ready) => ready.build,
                            _ => this.downloading.unwrap_or(0),
                        };
                        if !matches!(previous, State::Ready(_)) {
                            this.state = State::Available(release.clone());
                        }
                        if release.build > have && auto_download() {
                            this.fetch(release.clone(), false, cx);
                        } else if release.build <= have {
                            eprintln!("update: {} already downloaded", release.label());
                        } else {
                            eprintln!("update: {} is available (automatic downloads are off)", release.label());
                            announce(&release, crate::notifications::UpdateAction::Update);
                        }
                        Checked::Available(release)
                    }
                    Ok(_) => {
                        if !matches!(previous, State::Ready(_)) {
                            this.state = State::Idle;
                        }
                        eprintln!("update: up to date");
                        Checked::UpToDate
                    }
                    Err(error) => {
                        // Offline for a moment: keep offering what was found before.
                        this.state = match previous {
                            State::Available(_) | State::Ready(_) => previous,
                            _ => State::Idle,
                        };
                        eprintln!("update: check failed: {error:#}");
                        Checked::Failed(format!("{error:#}"))
                    }
                };
                cx.notify();
                checked
            })
            .unwrap_or(Checked::Busy)
        })
    }

    pub fn click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.state.clone() {
            State::Available(_) | State::Ready(_) => self.update_now(cx),
            State::Downloading { release, received, asked: false } => {
                let percent = received as f64 / release.size.max(1) as f64 * 100.;
                ui::toast(
                    window,
                    cx,
                    format!("Downloading {} ({percent:.0}%). Filo says when it’s ready.", release.label()),
                );
            }
            State::Idle => {
                let check = self.check(cx);
                window
                    .spawn(cx, async move |cx| {
                        let checked = check.await;
                        cx.update(|window, cx| match checked {
                            Checked::UpToDate => {
                                ui::toast(window, cx, format!("Filo is up to date ({})", version_label()))
                            }
                            Checked::Available(release) if auto_download() => ui::toast(
                                window,
                                cx,
                                format!("Downloading {}. Filo says when it’s ready.", release.label()),
                            ),
                            Checked::Available(release) => ui::toast(
                                window,
                                cx,
                                format!("{} is out. Click Update to install it.", release.label()),
                            ),
                            Checked::Failed(error) => ui::toast_error(window, cx, "Couldn’t check for updates", error),
                            Checked::Busy => {}
                        })
                        .ok();
                    })
                    .detach();
            }
            _ => {}
        }
    }

    /// The Update or Restart button (the sidebar's, the banner's): a build found is downloaded, installed and restarted
    /// into; one downloaded already is restarted into.
    fn update_now(&mut self, cx: &mut Context<Self>) {
        match self.state.clone() {
            State::Available(release) => self.fetch(release, true, cx),
            State::Ready(_) => self.restart(cx),
            _ => {}
        }
    }

    /// Downloads `release` and checks it, into a folder next to the app; then it's ready to go in. `asked` (a click with
    /// automatic downloads off): shows the progress and restarts into it.
    fn fetch(&mut self, release: Release, asked: bool, cx: &mut Context<Self>) {
        let Some(bundle) = current_bundle() else {
            // A bare binary (cargo run) has no app to replace: hand over the disk image.
            if asked {
                open::that(format!("{}/Filo.dmg", self.feed)).ok();
            }
            return;
        };
        let Some(parent) = bundle.parent() else { return };
        if self.downloading.is_some_and(|build| build >= release.build) {
            return;
        }
        let staging = parent.join(format!("{STAGING_PREFIX}{}", release.build));
        if !matches!(self.state, State::Ready(_) | State::Waiting(_)) {
            self.state = State::Downloading { release: release.clone(), received: 0, asked };
            cx.notify();
        }
        self.downloading = Some(release.build);
        eprintln!("update: downloading {}{}", release.label(), if asked { "" } else { " in the background" });
        let backend = backend(cx);
        let progress = Arc::new(AtomicU64::new(0));
        let job = {
            let (http, feed, release, staging, progress, bundle) =
                (backend.http(), self.feed.clone(), release.clone(), staging.clone(), progress.clone(), bundle.clone());
            backend.run(async move {
                let _ = std::fs::remove_dir_all(&staging);
                let zip = download(http, feed, release, staging.clone(), progress).await?;
                tokio::task::spawn_blocking(move || unpack(&zip, &staging, Some(&bundle))).await?
            })
        };
        self._download = Some(cx.spawn(async move |this, cx| {
            let mut job = Box::pin(job);
            let result = loop {
                let tick = Box::pin(cx.background_executor().timer(Duration::from_millis(200)));
                match futures::future::select(job, tick).await {
                    futures::future::Either::Left((result, _)) => break result,
                    futures::future::Either::Right((_, pending)) => {
                        job = pending;
                        let received = progress.load(Ordering::Relaxed);
                        this.update(cx, |this, cx| {
                            if let State::Downloading { received: shown, .. } = &mut this.state {
                                *shown = received;
                                cx.notify();
                            }
                        })
                        .ok();
                    }
                }
            };
            this.update(cx, |this, cx| {
                this.downloading = None;
                match result {
                    Ok(app) => {
                        eprintln!(
                            "update: {} downloaded and checked (size, SHA-256, signed by the same key): {}",
                            release.label(),
                            app.display()
                        );
                        let staged = Staged { release: release.clone(), app, staging: staging.clone(), bundle };
                        if let Some(old) = STAGED.lock().unwrap().replace(staged).filter(|old| old.staging != staging) {
                            let _ = std::fs::remove_dir_all(old.staging);
                        }
                        this.state = match this.state {
                            State::Waiting(_) => State::Waiting(release.clone()),
                            _ => State::Ready(release.clone()),
                        };
                        cx.notify();
                        announce(&release, crate::notifications::UpdateAction::Restart);
                        if asked {
                            this.restart(cx);
                        }
                    }
                    Err(error) => {
                        eprintln!("update: couldn’t download {}: {error:#}", release.label());
                        let _ = std::fs::remove_dir_all(&staging);
                        if matches!(this.state, State::Downloading { .. }) {
                            this.state = State::Available(release);
                            cx.notify();
                        }
                        if asked {
                            this.window
                                .update(cx, |_, window, cx| {
                                    ui::toast_error(window, cx, "Couldn’t download the update", format!("{error:#}"))
                                })
                                .ok();
                        }
                    }
                }
            })
            .ok();
        }));
    }

    /// Puts the waiting build in place and opens it, once no chat reply is running; coding tasks carry on in their
    /// workers and the new Filo picks them up.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, State::Waiting(_) | State::Restarting) {
            return;
        }
        let Some(release) = staged().map(|staged| staged.release) else { return };
        self.state = State::Waiting(release);
        cx.notify();
        let backend = backend(cx);
        self._restart = Some(cx.spawn(async move |this, cx| {
            let mut said = false;
            loop {
                let replies = replies_running(&backend).await;
                if replies == 0 {
                    break;
                }
                if !said {
                    eprintln!(
                        "update: restarting once {replies} chat {} finished",
                        if replies == 1 { "reply has" } else { "replies have" }
                    );
                    said = true;
                }
                cx.background_executor().timer(REPLIES_POLL).await;
            }
            // Quitting meanwhile took the build (`install_on_quit`).
            let Some(staged) = STAGED.lock().unwrap().take() else { return };
            this.update(cx, |this, cx| {
                this.state = State::Restarting;
                cx.notify();
            })
            .ok();
            let (bundle, app, staging) = (staged.bundle.clone(), staged.app.clone(), staged.staging.clone());
            let swapped = cx.background_spawn(async move { swap(&bundle, &app, &staging) }).await;
            let result = swapped
                .and_then(|()| relaunch(&staged.bundle).context("the new version is in place, but couldn’t open it"));
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    eprintln!("update: restarting into {}", staged.release.label());
                    // The new Filo picks up the turns and coding tasks under way.
                    crate::local_backend::restarting();
                    cx.quit();
                }
                Err(error) => {
                    eprintln!("update: couldn’t restart into {}: {error:#}", staged.release.label());
                    // When the swap failed the old app is back and the new one still waits in its folder.
                    if std::fs::exists(&staged.app).unwrap_or(false) {
                        this.state = State::Ready(staged.release.clone());
                        *STAGED.lock().unwrap() = Some(staged);
                    } else {
                        this.state = State::Idle;
                    }
                    cx.notify();
                    this.window
                        .update(cx, |_, window, cx| {
                            ui::toast_error(window, cx, "Couldn’t restart into the update", format!("{error:#}"))
                        })
                        .ok();
                }
            })
            .ok();
        }));
    }
}

/// One silent banner per build (`updates.json` remembers which): ready, with Restart, or available, with Update (automatic
/// downloads off). A build announced as available isn't announced again once it's downloaded.
fn announce(release: &Release, action: crate::notifications::UpdateAction) {
    let mut settings = crate::config::load_settings::<Settings>(SETTINGS);
    if settings.announced_build >= release.build {
        eprintln!("update: {} was announced already", release.label());
        return;
    }
    settings.announced_build = release.build;
    if let Err(error) = crate::config::save_settings(SETTINGS, &settings) {
        eprintln!("update: {error}");
    }
    eprintln!("update: announcing {} ({action:?})", release.label());
    let (title, body) = match action {
        crate::notifications::UpdateAction::Restart => {
            (format!("{} is ready", release.label()), "Restart Filo to use it. Quit instead and it goes in then.")
        }
        crate::notifications::UpdateAction::Update => {
            (format!("{} is available", release.label()), "Update downloads it and restarts Filo into it.")
        }
    };
    crate::notifications::show_update(release.build, &title, body, action);
}

impl Render for Updater {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let (icon, label, tooltip, busy, ready): (&str, SharedString, String, bool, bool) = match &self.state {
            State::Idle => {
                ("refresh-cw", "Updates".into(), format!("Check for updates · {}", version_label()), false, false)
            }
            State::Checking => ("refresh-cw", "Checking".into(), "Checking for updates…".into(), true, false),
            State::Available(release) => (
                "download",
                "Update".into(),
                format!("Download and install {} (now {})", release.label(), version_label()),
                false,
                true,
            ),
            State::Downloading { release, asked: false, .. } => (
                "refresh-cw",
                "Updates".into(),
                format!("Downloading {} in the background · {}", release.label(), version_label()),
                false,
                false,
            ),
            State::Downloading { release, received, asked: true } => {
                let percent = (*received as f64 / release.size.max(1) as f64 * 100.).min(100.);
                ("download", format!("{percent:.0}%").into(), format!("Downloading {}…", release.label()), true, true)
            }
            State::Ready(release) => (
                "rotate-cw",
                "Restart to update".into(),
                format!("Restart into {} (now {}). Coding tasks carry on.", release.label(), version_label()),
                false,
                true,
            ),
            State::Waiting(release) => (
                "rotate-cw",
                "Restarting soon".into(),
                format!("Restarts into {} once the chat replies under way have finished", release.label()),
                true,
                true,
            ),
            State::Restarting => {
                ("rotate-cw", "Restarting".into(), "Restarting into the new version…".into(), true, true)
            }
        };
        let color = if ready { colors.accent } else { colors.text_secondary };
        div()
            .id("sidebar-updates")
            .w_full()
            .py(px(space::SM))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::XXS))
            .rounded(px(radius::MD))
            .when(!busy, |this| {
                this.cursor_pointer()
                    .hover(|style| style.bg(colors.selected_fill.opacity(0.6)))
                    .on_click(cx.listener(|this, _, window, cx| this.click(window, cx)))
            })
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
            .child(div().relative().map(|this| {
                if busy {
                    this.child(ui::spinner(22., cx))
                } else {
                    this.child(ui::icon(icon).size(px(22.)).text_color(color)).when(ready, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top(px(-1.))
                                .right(px(-3.))
                                .size(px(9.))
                                .rounded(px(radius::FULL))
                                .bg(colors.accent)
                                .border_1()
                                .border_color(colors.sunken),
                        )
                    })
                }
            }))
            // "Restart to update" takes two lines in the sidebar.
            .child(
                div().w_full().text_center().micro().text_color(color).when(ready, |this| this.semibold()).child(label),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(file: &str) -> Release {
        serde_json::from_value(
            json!({ "channel": "stable", "version": "0.1.0", "build": 42, "file": file, "size": 1, "sha256": "" }),
        )
        .unwrap()
    }

    /// A GitHub Release's latest.json links the zip in that release; an older feed names it next to latest.json.
    #[test]
    fn the_zip_comes_from_its_link_or_beside_latest_json() {
        let feed = "https://github.com/bitooz/filo-desktop/releases/latest/download";
        let linked = release("https://github.com/bitooz/filo-desktop/releases/download/v0.1.0-42/Filo-42.zip");
        assert_eq!(
            linked.zip_url(feed),
            "https://github.com/bitooz/filo-desktop/releases/download/v0.1.0-42/Filo-42.zip"
        );
        assert_eq!(linked.zip_name(), "Filo-42.zip");
        let beside = release("Filo-42.zip");
        assert_eq!(beside.zip_url(feed), format!("{feed}/Filo-42.zip"));
        assert_eq!(beside.zip_name(), "Filo-42.zip");
    }
}

// The engine Filo's browser runs its pages in (Settings › Browser): WebKit, macOS's own and the default, or Chromium,
// marked Beta, an engine of Filo's own downloaded once and kept up to date apart from Filo's updates. Either way the
// browser is the same (browser.rs, native.rs); only what drives a page differs (browser/chromium.rs for Chromium).
//
// The Chromium engine is Filo Chromium.app (chromium/), from the engine feed: `chromium-<PROTOCOL>.json`, the newest
// engine that speaks this Filo's protocol, with its zip for each processor and the zip's size and SHA-256
// (chromium/package.sh makes them, chromium/publish.sh puts them in GitHub Releases). Choosing Chromium downloads it into
// the local data folder's `chromium-engine/<version>` (`engine.json` there names the one in use) and checks it: size,
// SHA-256, a signature that holds, and one by the same developer as Filo itself (a Filo signed ad hoc, a local build,
// skips that part, as its updater does). Then the logins move over: the cookies WebKit has that Chromium hasn't, read
// from WebKit's store and never changed there; and the browser restarts on Chromium, each tab opening again at its
// address with its id, owner and agent (browser.rs `reopen_tabs`), hidden as before. Choosing WebKit again copies
// Chromium's newer logins back the same way and stops the engine, which stays installed for next time (Remove deletes
// it). What sites keep besides cookies (localStorage, IndexedDB, service workers) doesn't move.
//
// While Chromium is chosen, Filo looks at the feed a minute after launch and every 6 hours: a newer engine downloads in
// the background, is checked the same way, and goes in the next time the engine starts (at once when no tab is open in
// it); the older one is removed at the next launch. `dev` builds: FILO_CHROMIUM_FEED names another feed (a local
// folder served over HTTP), FILO_BROWSER_ENGINE=webkit|chromium picks the engine for the run, FILO_CHROMIUM_APP an
// engine to run, and a build in target/cef/<arch> is used when none is installed.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use block2::RcBlock;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Global, Task, WeakEntity};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backend::backend;

/// What Filo and its Chromium engine say to each other (chromium/filo_chromium.mm `kProtocol`).
pub const PROTOCOL: u64 = 1;
/// The engine feed's folder: chromium/publish.sh's release.
const FEED: &str = "https://github.com/bitooz/filo-desktop/releases/download/chromium-engine";
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// The engines' folder in the local data folder.
const ENGINES: &str = "chromium-engine";
/// The app in an engine's folder.
const APP: &str = "Filo Chromium.app";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    WebKit,
    Chromium,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::WebKit => "WebKit",
            Kind::Chromium => "Chromium",
        }
    }
}

/// The feed's `chromium-<protocol>.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    /// CEF's version and the engine's build number: 154.0.34.1.
    pub version: String,
    /// Chromium's own: 154.0.8037.98.
    pub chromium: String,
    pub protocol: u64,
    /// By processor ("arm64", "x86_64").
    pub builds: HashMap<String, Build>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Build {
    /// The zip's URL, or its name next to the feed.
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

/// The engine in use: `chromium-engine/engine.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    pub chromium: String,
    pub protocol: u64,
}

impl Installed {
    fn app(&self) -> PathBuf {
        engines_dir().join(&self.version).join(APP)
    }
}

thread_local! {
    /// The engine a check picked for its run (`force`).
    static FORCED: Cell<Option<Kind>> = const { Cell::new(None) };
    /// The feed a check serves (`dev` builds).
    static CHECK_FEED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The engine feed for this run: a check's own (`dev` builds).
#[cfg(feature = "dev")]
pub fn use_feed(url: &str) {
    CHECK_FEED.with(|feed| *feed.borrow_mut() = Some(url.to_string()));
}

/// The engine new pages open in: the one picked in Settings › Browser, when it's there to run.
pub fn current() -> Kind {
    let wanted = FORCED.with(Cell::get).unwrap_or_else(|| {
        match crate::test_run::dev_override("FILO_BROWSER_ENGINE").as_deref() {
            Some("chromium") => Kind::Chromium,
            Some("webkit") => Kind::WebKit,
            _ => chosen(),
        }
    });
    if wanted == Kind::Chromium && app_path().is_none() { Kind::WebKit } else { wanted }
}

/// New pages in `kind` whatever Settings says, or as it says again (None): a check's (`dev` builds).
#[cfg(feature = "dev")]
pub fn force(kind: Option<Kind>) {
    FORCED.with(|forced| forced.set(kind));
}

/// What Settings › Browser has picked (`local/browser.json`'s `engine`).
pub fn chosen() -> Kind {
    super::browser_json()
        .get("engine")
        .and_then(|engine| serde_json::from_value(engine.clone()).ok())
        .unwrap_or_default()
}

fn engines_dir() -> PathBuf {
    super::local_dir().join(ENGINES)
}

/// The engine installed, when it speaks this Filo's protocol.
pub fn installed() -> Option<Installed> {
    let saved = std::fs::read(engines_dir().join("engine.json")).ok()?;
    let installed: Installed = serde_json::from_slice(&saved).ok()?;
    (installed.protocol == PROTOCOL && installed.app().join("Contents/MacOS/Filo Chromium").exists())
        .then_some(installed)
}

/// The Chromium engine to run: FILO_CHROMIUM_APP (`dev` builds; `none`, only one installed), the one installed, else
/// (`dev` builds) the one chromium/build.sh built in target/cef.
pub fn app_path() -> Option<PathBuf> {
    let named = crate::test_run::dev_override("FILO_CHROMIUM_APP");
    if named.as_deref() == Some("none") {
        return installed().map(|installed| installed.app());
    }
    if let Some(app) = named {
        return Some(PathBuf::from(app));
    }
    if let Some(installed) = installed() {
        return Some(installed.app());
    }
    built_here()
}

#[cfg(feature = "dev")]
fn built_here() -> Option<PathBuf> {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/cef").join(arch).join(APP);
    app.join("Contents/MacOS/Filo Chromium").exists().then_some(app)
}

#[cfg(not(feature = "dev"))]
fn built_here() -> Option<PathBuf> {
    None
}

/// This Mac's key in the feed's builds.
fn processor() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" }
}

fn feed() -> String {
    CHECK_FEED
        .with(|feed| feed.borrow().clone())
        .or_else(|| crate::test_run::dev_override("FILO_CHROMIUM_FEED"))
        .unwrap_or_else(|| FEED.into())
        .trim_end_matches('/')
        .to_string()
}

/// "154.0.34.10" after "154.0.34.9".
fn newer(version: &str, than: &str) -> bool {
    let parts = |text: &str| text.split('.').map(|part| part.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(version) > parts(than)
}

/// What Settings › Browser shows going on.
#[derive(Clone, Debug, PartialEq)]
pub enum Busy {
    Looking,
    Downloading { received: u64, total: u64 },
    Checking,
    CopyingLogins,
    Restarting,
}

/// The engines' state: one, a global, that Settings › Browser watches.
pub struct Engines {
    pub busy: Option<Busy>,
    /// Why the last switch didn't happen.
    pub error: Option<String>,
    /// What the last switch said (logins copied).
    pub note: Option<String>,
    /// A newer engine downloaded in the background, going in when the engine next starts.
    pub updated: Option<String>,
    /// Bytes downloaded so far, read while `Downloading`.
    progress: Arc<AtomicU64>,
    /// Every step shown so far, for a check (`dev` builds).
    #[cfg(feature = "dev")]
    pub steps: Vec<Busy>,
    _switch: Option<Task<()>>,
    _updates: Option<Task<()>>,
    _update: Option<Task<()>>,
}

struct EnginesGlobal(Entity<Engines>);

impl Global for EnginesGlobal {}

pub fn engines(cx: &App) -> Option<Entity<Engines>> {
    cx.try_global::<EnginesGlobal>().map(|global| global.0.clone())
}

/// Sets up the engines' state, removes engines no longer in use, and looks for newer ones while Chromium is chosen.
pub fn init(cx: &mut App) {
    if cx.try_global::<EnginesGlobal>().is_some() {
        return;
    }
    let engines = cx.new(|cx| {
        let mut engines = Engines {
            busy: None,
            error: None,
            note: None,
            updated: None,
            progress: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "dev")]
            steps: Vec::new(),
            _switch: None,
            _updates: None,
            _update: None,
        };
        engines.watch_for_updates(cx);
        engines
    });
    cx.set_global(EnginesGlobal(engines));
    cx.background_spawn(async { remove_unused() }).detach();
}

/// Engines other than the one in use, and downloads left half done (none runs yet: Filo just started).
fn remove_unused() {
    let keep = installed().map(|installed| installed.version);
    let Ok(entries) = std::fs::read_dir(engines_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && keep.as_deref() != Some(name.as_str()) {
            eprintln!("browser: removing the unused Chromium engine {name}");
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Looks for a newer engine now, as Filo does every 6 hours while Chromium is chosen (a check's, `dev` builds).
#[cfg(feature = "dev")]
pub fn look_for_update(cx: &mut App) {
    if let Some(engines) = engines(cx) {
        engines.update(cx, |engines, cx| engines.look_for_update(cx));
    }
}

/// Switches the browser to `kind` (Settings › Browser).
pub fn choose(kind: Kind, cx: &mut App) {
    if let Some(engines) = engines(cx) {
        engines.update(cx, |engines, cx| engines.choose(kind, cx));
    }
}

/// Deletes the Chromium engine (not while it's the browser's).
pub fn remove(cx: &mut App) -> Result<(), String> {
    if current() == Kind::Chromium || chosen() == Kind::Chromium {
        return Err("Chromium is the browser's engine: pick WebKit first.".into());
    }
    super::chromium::stop();
    std::fs::remove_dir_all(engines_dir()).map_err(|error| format!("Couldn't remove it: {error}"))?;
    eprintln!("browser: removed the Chromium engine");
    if let Some(engines) = engines(cx) {
        engines.update(cx, |engines, cx| {
            engines.updated = None;
            cx.notify();
        });
    }
    Ok(())
}

impl Engines {
    /// The bytes downloaded so far.
    pub fn received(&self) -> u64 {
        self.progress.load(Ordering::Relaxed)
    }

    fn set(&mut self, busy: Option<Busy>, cx: &mut Context<Self>) {
        #[cfg(feature = "dev")]
        self.steps.extend(busy.clone());
        self.busy = busy;
        cx.notify();
    }

    fn choose(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.error = None;
        self.note = None;
        let from = current();
        eprintln!("browser: switching the engine from {} to {}", from.label(), kind.label());
        // Its first step shows at once, and the rows wait.
        let download = kind == Kind::Chromium && app_path().is_none();
        self.set(Some(if download { Busy::Looking } else { Busy::CopyingLogins }), cx);
        self._switch = Some(cx.spawn(async move |this, cx| {
            let result = switch(&this, from, kind, cx).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(note) => this.note = note,
                    Err(error) => {
                        eprintln!("browser: couldn't switch to {}: {error}", kind.label());
                        this.error = Some(error);
                    }
                }
                this.set(None, cx);
                if kind == Kind::Chromium {
                    this.watch_for_updates(cx);
                }
            })
            .ok();
        }));
    }

    /// Looks at the feed now and then while Chromium is chosen, and downloads a newer engine.
    fn watch_for_updates(&mut self, cx: &mut Context<Self>) {
        if self._updates.is_some() || chosen() != Kind::Chromium {
            return;
        }
        self._updates = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            loop {
                this.update(cx, |this, cx| this.look_for_update(cx)).ok();
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        }));
    }

    /// A newer engine from the feed, while Chromium is chosen and nothing else is going on.
    fn look_for_update(&mut self, cx: &mut Context<Self>) {
        if chosen() != Kind::Chromium || self.busy.is_some() {
            return;
        }
        self._update = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = update(&this, cx).await {
                eprintln!("browser: couldn't update the Chromium engine: {error}");
            }
        }));
    }
}

/// The feed's newest engine for this Filo.
async fn look(cx: &AsyncApp) -> Result<Release, String> {
    let (http, runtime) =
        cx.update(|cx| (backend(cx).http(), backend(cx).runtime())).map_err(|_| "Filo is quitting.")?;
    let url = format!("{}/chromium-{PROTOCOL}.json", feed());
    let job = runtime.spawn(async move {
        let response = http.get(&url).header("cache-control", "no-cache").send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("there's no Chromium engine for this version of Filo yet");
        }
        Ok::<Release, anyhow::Error>(response.error_for_status()?.json().await?)
    });
    let release = job
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("Couldn't reach the engine feed: {error:#}"))?;
    if release.protocol != PROTOCOL {
        return Err(format!("The feed's engine speaks protocol {}, this Filo {PROTOCOL}.", release.protocol));
    }
    Ok(release)
}

/// Downloads `release`'s engine for this Mac, checks it and makes it the one in use.
async fn install(this: &WeakEntity<Engines>, release: Release, cx: &mut AsyncApp) -> Result<Installed, String> {
    let build =
        release.builds.get(processor()).cloned().ok_or("There's no Chromium engine for this Mac's processor yet.")?;
    let url = if build.file.contains("://") { build.file.clone() } else { format!("{}/{}", feed(), build.file) };
    // The zip, the engine unpacked from it (about 2.3 times its size) and room to spare.
    let needed = build.size * 4;
    let _ = std::fs::create_dir_all(engines_dir());
    if let Some(free) = free_space(&engines_dir()).filter(|free| *free < needed) {
        return Err(format!(
            "Chromium needs about {} MB free to install, and this Mac has {} MB: free some space and pick it again.",
            needed / 1_000_000,
            free / 1_000_000
        ));
    }
    let staging = engines_dir().join(format!(".download-{}", release.version));
    let progress = this.read_with(cx, |this, _| this.progress.clone()).map_err(|_| "Filo is quitting.")?;
    progress.store(0, Ordering::Relaxed);
    this.update(cx, |this, cx| this.set(Some(Busy::Downloading { received: 0, total: build.size }), cx)).ok();
    eprintln!(
        "browser: downloading the Chromium engine {} ({} MB) from {url}",
        release.version,
        build.size / 1_000_000
    );
    let (http, runtime) =
        cx.update(|cx| (backend(cx).http(), backend(cx).runtime())).map_err(|_| "Filo is quitting.")?;
    let job = {
        let (staging, progress) = (staging.clone(), progress.clone());
        runtime.spawn(async move {
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(&staging).context("Filo can't write to its data folder")?;
            let zip = staging.join("engine.zip");
            crate::updater::fetch_checked(http, &url, build.size, &build.sha256, &zip, progress).await?;
            Ok::<PathBuf, anyhow::Error>(zip)
        })
    };
    let mut job = Box::pin(job);
    let zip = loop {
        let tick = Box::pin(cx.background_executor().timer(Duration::from_millis(200)));
        match futures::future::select(job, tick).await {
            futures::future::Either::Left((result, _)) => break result,
            futures::future::Either::Right((_, pending)) => {
                job = pending;
                this.update(cx, |_, cx| cx.notify()).ok();
            }
        }
    };
    let zip = zip.map_err(anyhow::Error::from).and_then(|zip| zip).map_err(|error| {
        let _ = std::fs::remove_dir_all(&staging);
        format!("Couldn't download Chromium: {error:#}")
    })?;
    this.update(cx, |this, cx| this.set(Some(Busy::Checking), cx)).ok();
    let version = release.version.clone();
    let checked = cx.background_spawn(async move { unpack(&zip, &staging, &version) }).await;
    let app = checked.map_err(|error| format!("The Chromium download didn't pass its checks: {error:#}"))?;
    let installed =
        Installed { version: release.version.clone(), chromium: release.chromium.clone(), protocol: release.protocol };
    let folder = engines_dir().join(&installed.version);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    std::fs::rename(&app, folder.join(APP)).map_err(|error| format!("Couldn't put the engine in place: {error}"))?;
    let _ = std::fs::remove_dir_all(engines_dir().join(format!(".download-{}", installed.version)));
    let json = serde_json::to_vec_pretty(&installed).map_err(|error| error.to_string())?;
    crate::config::write_atomic(&engines_dir().join("engine.json"), &json).map_err(|error| error.to_string())?;
    eprintln!(
        "browser: the Chromium engine {} (Chromium {}) is installed and checked",
        installed.version, installed.chromium
    );
    Ok(installed)
}

/// The bytes free on the disk `path` is on.
fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the struct it's given, for a path that's a C string.
    unsafe {
        let mut stats: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(path.as_ptr(), &mut stats) == 0).then(|| stats.f_bavail as u64 * stats.f_frsize as u64)
    }
}

/// Unzips the engine in `staging` and checks it: its version, a signature that holds, by Filo's own developer.
fn unpack(zip: &Path, staging: &Path, version: &str) -> Result<PathBuf> {
    let unpacked = staging.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    crate::updater::run(Command::new("ditto").args(["-x", "-k"]).arg(zip).arg(&unpacked))?;
    let app = unpacked.join(APP);
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).context("the download has no engine in it")?;
    let said = plist
        .split("<key>CFBundleShortVersionString</key>")
        .nth(1)
        .and_then(|after| after.split("<string>").nth(1)?.split("</string>").next())
        .unwrap_or_default();
    if said != version {
        bail!("the engine says it's {said}, the feed {version}");
    }
    crate::updater::run(Command::new("codesign").args(["--verify", "--deep", "--strict"]).arg(&app))
        .context("its signature is broken")?;
    if let Some(team) = developer() {
        let requirement = format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"");
        crate::updater::run(Command::new("codesign").arg("--verify").arg(format!("-R{requirement}")).arg(&app))
            .with_context(|| format!("it isn't signed by Filo's developer ({team})"))?;
    }
    Ok(app)
}

/// The developer (team) this Filo is signed by: what the engine must be signed by too. None for a Filo signed ad hoc
/// or a bare binary (local builds).
fn developer() -> Option<String> {
    let bundle = crate::updater::current_bundle()?;
    let shown = crate::updater::run(Command::new("codesign").arg("-dv").arg(bundle)).ok()?;
    let team = shown.lines().find_map(|line| line.strip_prefix("TeamIdentifier="))?.trim().to_string();
    (!team.is_empty() && team != "not set").then_some(team)
}

/// A newer engine from the feed, downloaded and checked in the background; it goes in when the engine next starts.
async fn update(this: &WeakEntity<Engines>, cx: &mut AsyncApp) -> Result<(), String> {
    let Some(have) = installed() else { return Ok(()) };
    let release = look(cx).await?;
    if !newer(&release.version, &have.version) {
        return Ok(());
    }
    let installed = install(this, release, cx).await;
    this.update(cx, |this, cx| {
        this.set(None, cx);
        if let Ok(installed) = &installed {
            this.updated = Some(installed.chromium.clone());
        }
    })
    .ok();
    let installed = installed?;
    // No page in the old one: the next tab starts the new one.
    if !super::tabs().iter().any(|tab| tab.engine() == Kind::Chromium) {
        super::chromium::stop();
    }
    eprintln!("browser: the Chromium engine {} goes in when the engine next starts", installed.version);
    Ok(())
}

/// Downloads the engine when there's none, copies the logins over, saves the choice and opens the tabs again in it.
async fn switch(this: &WeakEntity<Engines>, from: Kind, to: Kind, cx: &mut AsyncApp) -> Result<Option<String>, String> {
    if to == Kind::Chromium && app_path().is_none() {
        let release = look(cx).await?;
        install(this, release, cx).await?;
    }
    let mut note = None;
    if from != to {
        this.update(cx, |this, cx| this.set(Some(Busy::CopyingLogins), cx)).ok();
        match copy_logins(to, cx).await {
            Ok((copied, there)) => {
                eprintln!("browser: copied {copied} logins (cookies) to {}, {there} it had already", to.label());
                note = Some(match copied {
                    0 => format!("{} had your logins already.", to.label()),
                    1 => "Brought 1 login cookie over.".to_string(),
                    _ => format!("Brought {copied} login cookies over."),
                });
            }
            // The switch goes on: the user signs in again where it matters.
            Err(error) => {
                eprintln!("browser: couldn't copy the logins to {}: {error}", to.label());
                note = Some(format!("Couldn't bring your logins over ({error}): sign in again where you need to."));
            }
        }
    }
    super::set_browser_json("engine", json!(to))?;
    this.update(cx, |this, cx| this.set(Some(Busy::Restarting), cx)).ok();
    cx.update(|cx| {
        let reopened = super::reopen_tabs(false);
        eprintln!("browser: the browser runs on {} now ({reopened} tabs opened again)", to.label());
        if to == Kind::WebKit {
            super::chromium::stop();
        }
        cx.refresh_windows();
    })
    .ok();
    Ok(note)
}

/// Copies the cookies the other engine has and `to` hasn't into `to`: how many, and how many it had already.
async fn copy_logins(to: Kind, cx: &mut AsyncApp) -> Result<(usize, usize), String> {
    // WebKit serves its store's cookies only while a page of it is open: one of its own for the copy.
    let page = cx.update(|_| webkit_page()).ok().flatten().ok_or("WebKit's store didn't open.")?;
    for _ in 0..30 {
        // SAFETY: WebKit on the main thread.
        let loading = cx.update(|_| unsafe { msg_send![&*page, isLoading] }).unwrap_or(false);
        if !loading {
            break;
        }
        cx.background_executor().timer(Duration::from_millis(100)).await;
    }
    let copied = copy_logins_with(to, cx).await;
    drop(page);
    copied
}

async fn copy_logins_with(to: Kind, cx: &mut AsyncApp) -> Result<(usize, usize), String> {
    let webkit = webkit_cookies(cx).await?;
    match to {
        Kind::Chromium => {
            let total = webkit.len();
            let answer = super::chromium::set_cookies(webkit, true).await?;
            Ok((answer.0, total.saturating_sub(answer.0)))
        }
        Kind::WebKit => {
            let have: HashSet<String> = webkit.iter().map(cookie_key).collect();
            let chromium = super::chromium::cookies().await?;
            let total = chromium.len();
            let missing: Vec<Value> =
                chromium.into_iter().filter(|cookie| !have.contains(&cookie_key(cookie))).collect();
            let copied = set_webkit_cookies(missing, cx).await?;
            Ok((copied, total.saturating_sub(copied)))
        }
    }
}

/// A cookie's place in a store: name, domain and path.
fn cookie_key(cookie: &Value) -> String {
    let text = |key: &str| cookie[key].as_str().unwrap_or_default().to_string();
    let path = text("path");
    format!("{}\t{}\t{}", text("name"), text("domain"), if path.is_empty() { "/".into() } else { path })
}

/// The browser's WebKit data store (browser.rs `store_id`).
fn webkit_data_store() -> Option<Retained<AnyObject>> {
    let id = super::store_id();
    // SAFETY: Foundation and WebKit on the main thread; the store is returned retained.
    unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class!(NSUUID), alloc];
        let uuid: Option<Retained<AnyObject>> = msg_send![allocated, initWithUUIDBytes: id.as_ptr()];
        msg_send![class!(WKWebsiteDataStore), dataStoreForIdentifier: &*uuid?]
    }
}

/// Its cookie store.
fn webkit_store() -> Option<Retained<AnyObject>> {
    // SAFETY: WebKit on the main thread.
    unsafe { msg_send![&*webkit_data_store()?, httpCookieStore] }
}

/// An empty page of WebKit's on the browser's store, in no window: while it's open, WebKit serves the store's cookies.
fn webkit_page() -> Option<Retained<AnyObject>> {
    let store = webkit_data_store()?;
    // SAFETY: WebKit on the main thread; the configuration and the view are retained here, the view returned.
    unsafe {
        let configuration: Retained<AnyObject> = msg_send![class!(WKWebViewConfiguration), new];
        let _: () = msg_send![&*configuration, setWebsiteDataStore: &*store];
        let allocated: Allocated<AnyObject> = msg_send![class!(WKWebView), alloc];
        let frame = objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(0., 0.),
            objc2_foundation::NSSize::new(10., 10.),
        );
        let view: Option<Retained<AnyObject>> =
            msg_send![allocated, initWithFrame: frame, configuration: &*configuration];
        let view = view?;
        let html = NSString::from_str("<!doctype html><title>Filo</title>");
        let _: *mut AnyObject = msg_send![&*view, loadHTMLString: &*html, baseURL: std::ptr::null_mut::<AnyObject>()];
        Some(view)
    }
}

fn text(object: *mut AnyObject) -> String {
    if object.is_null() {
        return String::new();
    }
    // SAFETY: an NSString, read on the main thread.
    unsafe {
        let text: *const std::ffi::c_char = msg_send![object, UTF8String];
        if text.is_null() { String::new() } else { std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned() }
    }
}

/// Every cookie in WebKit's store, as the engine takes them (chromium/filo_chromium.mm). Read only.
async fn webkit_cookies(cx: &mut AsyncApp) -> Result<Vec<Value>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Vec<Value>>();
    cx.update(|_| {
        let Some(store) = webkit_store() else { return };
        let tx = std::cell::RefCell::new(Some(tx));
        let handler = RcBlock::new(move |cookies: *mut AnyObject| {
            let Some(tx) = tx.borrow_mut().take() else { return };
            let mut all = Vec::new();
            // SAFETY: WebKit's NSArray of NSHTTPCookie, read on the main thread inside its callback.
            unsafe {
                let count: usize = if cookies.is_null() { 0 } else { msg_send![cookies, count] };
                for index in 0..count {
                    let cookie: *mut AnyObject = msg_send![cookies, objectAtIndex: index];
                    let expires: *mut AnyObject = msg_send![cookie, expiresDate];
                    let same_site: *mut AnyObject = msg_send![cookie, sameSitePolicy];
                    let secure: bool = msg_send![cookie, isSecure];
                    let http_only: bool = msg_send![cookie, isHTTPOnly];
                    let mut entry = json!({
                        "name": text(msg_send![cookie, name]),
                        "value": text(msg_send![cookie, value]),
                        "domain": text(msg_send![cookie, domain]),
                        "path": text(msg_send![cookie, path]),
                        "secure": secure,
                        "httpOnly": http_only,
                    });
                    if !expires.is_null() {
                        let seconds: f64 = msg_send![expires, timeIntervalSince1970];
                        entry["expires"] = json!(seconds.round() as i64);
                    }
                    match text(same_site).to_ascii_lowercase().as_str() {
                        "lax" => entry["sameSite"] = json!("Lax"),
                        "strict" => entry["sameSite"] = json!("Strict"),
                        _ => {}
                    }
                    all.push(entry);
                }
            }
            let _ = tx.send(all);
        });
        // SAFETY: WebKit on the main thread; it calls the handler once, on the main thread.
        unsafe {
            let _: () = msg_send![&*store, getAllCookies: &*handler];
        }
    })
    .map_err(|_| "Filo is quitting.")?;
    rx.await.map_err(|_| "WebKit didn't give its cookies.".to_string())
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSHTTPCookieName: &'static NSString;
    static NSHTTPCookieValue: &'static NSString;
    static NSHTTPCookieDomain: &'static NSString;
    static NSHTTPCookiePath: &'static NSString;
    static NSHTTPCookieSecure: &'static NSString;
    static NSHTTPCookieExpires: &'static NSString;
    static NSHTTPCookieSameSitePolicy: &'static NSString;
}

/// Puts `cookies` (the engine's) in WebKit's store: how many it took.
async fn set_webkit_cookies(cookies: Vec<Value>, cx: &mut AsyncApp) -> Result<usize, String> {
    if cookies.is_empty() {
        return Ok(0);
    }
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<bool>();
    let sent = cx
        .update(|_| {
            let Some(store) = webkit_store() else { return 0 };
            let mut sent = 0;
            for cookie in &cookies {
                let Some(made) = webkit_cookie(cookie) else { continue };
                let tx = tx.clone();
                let done = RcBlock::new(move || drop(tx.unbounded_send(true)));
                // SAFETY: WebKit on the main thread; it calls the handler once the cookie is in.
                unsafe {
                    let _: () = msg_send![&*store, setCookie: &*made, completionHandler: &*done];
                }
                sent += 1;
            }
            sent
        })
        .map_err(|_| "Filo is quitting.")?;
    drop(tx);
    let mut taken = 0;
    while taken < sent {
        use futures::StreamExt as _;
        if rx.next().await.is_none() {
            break;
        }
        taken += 1;
    }
    Ok(taken)
}

/// An NSHTTPCookie of one of the engine's cookies.
fn webkit_cookie(cookie: &Value) -> Option<Retained<AnyObject>> {
    let text = |key: &str| cookie[key].as_str().map(NSString::from_str);
    let (name, value, domain) =
        (text("name")?, text("value").unwrap_or_else(|| NSString::from_str("")), text("domain")?);
    let path = text("path").unwrap_or_else(|| NSString::from_str("/"));
    // SAFETY: Foundation on the main thread; the dictionary and the cookie are retained here.
    unsafe {
        let properties: Retained<AnyObject> = msg_send![class!(NSMutableDictionary), dictionary];
        let set = |key: &AnyObject, value: &AnyObject| {
            let _: () = msg_send![&*properties, setObject: value, forKey: key];
        };
        set(NSHTTPCookieName, &name);
        set(NSHTTPCookieValue, &value);
        set(NSHTTPCookieDomain, &domain);
        set(NSHTTPCookiePath, &path);
        let yes = NSString::from_str("TRUE");
        if cookie["secure"] == true {
            set(NSHTTPCookieSecure, &yes);
        }
        if cookie["httpOnly"] == true {
            set(&NSString::from_str("HttpOnly"), &yes);
        }
        match cookie["sameSite"].as_str() {
            Some("Lax") => set(NSHTTPCookieSameSitePolicy, &NSString::from_str("lax")),
            Some("Strict") => set(NSHTTPCookieSameSitePolicy, &NSString::from_str("strict")),
            _ => {}
        }
        if let Some(seconds) = cookie["expires"].as_f64() {
            let date: Retained<AnyObject> = msg_send![class!(NSDate), dateWithTimeIntervalSince1970: seconds];
            set(NSHTTPCookieExpires, &date);
        }
        msg_send![class!(NSHTTPCookie), cookieWithProperties: &*properties]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_versions_compare_by_number() {
        assert!(newer("154.0.34.10", "154.0.34.9"));
        assert!(newer("155.0.1.1", "154.9.99.99"));
        assert!(!newer("154.0.34.1", "154.0.34.1"));
        assert!(!newer("154.0.34", "154.0.34.1"));
    }

    #[test]
    fn a_feed_reads() {
        let release: Release = serde_json::from_str(
            r#"{"version":"154.0.34.1","chromium":"154.0.8037.98","protocol":1,
                "builds":{"arm64":{"file":"Filo-Chromium-154.0.34.1-arm64.zip","size":10,"sha256":"ab"}}}"#,
        )
        .unwrap();
        assert_eq!(release.builds["arm64"].size, 10);
        assert_eq!(serde_json::to_value(Kind::Chromium).unwrap(), json!("chromium"));
        assert_eq!(cookie_key(&json!({ "name": "a", "domain": ".x.com" })), "a\t.x.com\t/");
    }
}

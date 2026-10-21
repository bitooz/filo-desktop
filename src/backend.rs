// The backend: the functions the pages call by name ("notes:mine"), answered on this Mac (local_backend.rs) with no
// account or server. Views get results through `subscribe` / `call`, which hop back onto the GPUI thread; the work
// itself runs on a Tokio runtime's blocking threads. A live query runs again whenever the local data changes.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{App, AppContext as _, Context, Global, Task, Window};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::config;
use crate::local_backend::Local;

/// The one person using this Mac's Filo, for the folders kept per user (offline drafts, saved answers).
const LOCAL_USER: &str = "local";

#[derive(Clone)]
pub struct Backend {
    rt: tokio::runtime::Handle,
    http: reqwest::Client,
    local: Arc<Local>,
    shared: Shared,
}

/// The latest answer to each live query, by its path and arguments: several screens subscribe to the same one (the
/// agents' list, their unseen replies), and after a change the first to ask runs it while the others wait for its
/// answer, instead of each running it again. While a turn runs, every live query runs again whenever anything changes.
type Shared = Arc<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Answered>>>>>>;

/// A live query's answer after change `version`. Taken by another asker only at the same change and soon after, so a
/// screen subscribing later reads the data afresh.
struct Answered {
    version: u64,
    at: std::time::Instant,
    answer: Result<Value, String>,
}

/// How long after it's given an answer can be shared (`Answered`).
const SHARE_FOR: std::time::Duration = std::time::Duration::from_secs(1);

pub struct BackendGlobal(pub Backend);

impl Global for BackendGlobal {}

pub fn backend(cx: &App) -> Backend {
    cx.global::<BackendGlobal>().0.clone()
}

impl Backend {
    pub fn new(rt: tokio::runtime::Handle) -> Result<Self> {
        let http = reqwest::Client::builder().user_agent("filo-desktop").build()?;
        let local = Local::open(config::config_dir().join("local"));
        Ok(Backend { rt, http, local, shared: Shared::default() })
    }

    /// Runs a future on the Tokio runtime and awaits it from any executor.
    pub fn run<F, T>(&self, future: F) -> impl Future<Output = T> + 'static
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.rt.spawn(async move {
            let _ = tx.send(future.await);
        });
        async move { rx.await.expect("backend task dropped") }
    }

    pub fn http(&self) -> reqwest::Client {
        self.http.clone()
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.rt.clone()
    }

    pub fn user_id(&self) -> &'static str {
        LOCAL_USER
    }

    /// Why the local data can't be opened, when a newer Filo wrote it (local_backend.rs `Local::newer_data`).
    pub fn newer_data(&self) -> Option<String> {
        self.local.newer_data()
    }

    /// Fires reminders, routines and check-ins while the app is open (local_backend/scheduled.rs).
    pub fn start_schedule(&self) {
        crate::local_backend::start_schedule(self.local.clone(), false);
    }

    /// Finds projects and stops the coding tasks a closed Filo left working (local_backend/coding.rs).
    pub fn start_coding(&self) {
        crate::local_backend::start_coding(self.local.clone());
    }

    /// Notes an older Filo kept waiting to be saved (~/.config/filo/offline/local/local/notes.json) go into Filo's
    /// database once, in the background (local_backend/note_carry_over.rs).
    pub fn carry_over_old_notes(&self) {
        let (local, file) = (self.local.clone(), config::offline_dir(LOCAL_USER).join("notes.json"));
        self.rt.spawn_blocking(move || local.carry_over_old_notes(&file));
    }

    /// Runs the agents' group chat's turns while the app is open (local_backend/group.rs).
    pub fn start_group(&self) {
        crate::local_backend::start_group(self.local.clone());
    }

    /// Saves the web pictures of artifacts made before Filo did that, once (filo_local artifact_images.rs).
    pub fn save_artifact_pictures(&self) {
        crate::local_backend::save_artifact_pictures(self.local.clone());
    }

    /// Feed and Notes, on or off, as saved (pages.rs).
    pub fn pages(&self) -> filo_local::pages::Pages {
        self.local.pages()
    }

    /// The first welcome on this Mac: Feed and Notes start off unless they have something in them (pages.rs).
    pub fn start_pages_off(&self) -> filo_local::pages::Pages {
        self.local.start_pages_off()
    }

    /// Turns Feed or Notes on or off (pages.rs).
    pub fn set_page(
        &self,
        page: filo_local::pages::Page,
        on: bool,
    ) -> impl Future<Output = Result<(), String>> + 'static {
        let (local, rt) = (self.local.clone(), self.rt.clone());
        self.run(async move {
            rt.spawn_blocking(move || local.set_page(page, on)).await.unwrap_or_else(|error| Err(error.to_string()))
        })
    }

    /// The model pickers' rows: the signed-in tools' models (local_backend `model_options`). Every few seconds while a
    /// picker draws, pi's sign-ins are looked at in the background, so a /login or a sign-in that stopped working
    /// shows without a restart (`look_at_pi_logins`); the pages hear of a change.
    pub fn model_options(&self) -> Arc<Vec<filo_local::engine::ModelOption>> {
        if self.local.pi_look_due() {
            let local = self.local.clone();
            self.rt.spawn_blocking(move || local.look_at_pi_logins());
        }
        self.local.model_options()
    }

    /// Every tool's models, for naming one whose tool is signed out (local_backend `all_model_options`).
    pub fn all_model_options(&self) -> Arc<Vec<filo_local::engine::ModelOption>> {
        self.local.all_model_options()
    }

    /// The reasoning levels a chat's engine and model take, least first; none when they aren't known (local_backend
    /// `chat_efforts`, as the Coding page has them). Reads Codex's catalogue: not on every frame.
    pub fn reasoning_levels(&self, engine: filo_local::engine::Engine, model: Option<&str>) -> Vec<String> {
        self.local.chat_efforts(engine, model)
    }

    /// The reasoning levels the conversation `session_id`'s next turn takes (local_backend `session_reasoning_levels`).
    pub fn session_reasoning_levels(&self, session_id: &str) -> Vec<String> {
        self.local.session_reasoning_levels(session_id)
    }

    /// Asks the CLIs about their models in the background, once per run unless the cache is fresh, and looks at which
    /// tools are signed in while that isn't known; resolves to whether the picker's rows changed. None when there's
    /// nothing to ask.
    pub fn refresh_models(&self) -> Option<impl Future<Output = bool> + 'static> {
        let ask = self.local.claim_model_refresh(filo_local::now_ms() as i64);
        if !ask && self.local.knows_sign_ins() {
            return None;
        }
        let (local, rt) = (self.local.clone(), self.rt.clone());
        Some(self.run(async move { rt.spawn_blocking(move || local.refresh_models(ask)).await.unwrap_or(false) }))
    }

    /// Filo quits: what runs in the app stops, and turns and coding tasks in workers stop unless "Keep coding after Filo
    /// quits" is on (local_backend/workers.rs).
    pub fn quit(&self) {
        self.local.quit();
    }

    // --- Functions ---

    pub fn call_raw(&self, path: &str, args: Value) -> impl Future<Output = Result<Value, String>> + 'static {
        let (local, path) = (self.local.clone(), path.to_string());
        let rt = self.rt.clone();
        self.run(async move {
            rt.spawn_blocking(move || {
                #[cfg(feature = "dev")]
                let started = query_times::Started::now();
                let answer = local.call(&path, &args);
                #[cfg(feature = "dev")]
                query_times::record(&format!("call {path}"), &args, started);
                answer
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()))
        })
    }

    /// The query's answers as they change: one now, then a new one whenever the local data changes and the answer
    /// with it.
    pub fn subscribe_raw(&self, path: &str, args: Value) -> mpsc::UnboundedReceiver<Result<Value, String>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (local, path, rt) = (self.local.clone(), path.to_string(), self.rt.clone());
        let mut changes = local.changes();
        // Screens asking the same thing share its answer (`Shared`).
        let key = format!("{path} {args}");
        let shared = self.shared.clone();
        let slot = shared.lock().unwrap().entry(key.clone()).or_default().clone();
        self.rt.spawn(async move {
            let mut last: Option<Result<Value, String>> = None;
            loop {
                let version = *changes.borrow_and_update();
                let answer = {
                    let mut slot = slot.lock().await;
                    match slot.as_ref() {
                        Some(answered) if answered.version == version && answered.at.elapsed() < SHARE_FOR => {
                            answered.answer.clone()
                        }
                        _ => {
                            let (query, path_now, args_now) = (local.clone(), path.clone(), args.clone());
                            let answer = rt
                                .spawn_blocking(move || {
                                    #[cfg(feature = "dev")]
                                    let started = query_times::Started::now();
                                    let answer = query.call(&path_now, &args_now);
                                    #[cfg(feature = "dev")]
                                    query_times::record(&path_now, &args_now, started);
                                    answer
                                })
                                .await
                                .unwrap_or_else(|error| Err(error.to_string()));
                            *slot = Some(Answered { version, at: std::time::Instant::now(), answer: answer.clone() });
                            answer
                        }
                    }
                };
                if last.as_ref() != Some(&answer) {
                    if tx.send(answer.clone()).is_err() {
                        break;
                    }
                    last = Some(answer);
                }
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() { break },
                    // The view dropped its receiver: unsubscribe.
                    _ = tx.closed() => break,
                }
            }
            // The last one asking: its answer goes.
            let mut shared = shared.lock().unwrap();
            if Arc::strong_count(&slot) == 2 {
                shared.remove(&key);
            }
        });
        rx
    }

    /// Says the local data changed, as a turn's new message does: every live query runs again (`FILO_OPEN=chat-timing`).
    #[cfg(feature = "dev")]
    pub fn poke(&self) {
        self.local.poke();
    }

    /// Runs query `path` once, alone, for how long it takes: (wall, this thread's CPU) (`FILO_OPEN=chat-timing`).
    #[cfg(feature = "dev")]
    pub fn time_query(
        &self,
        path: &str,
        args: Value,
    ) -> impl Future<Output = (std::time::Duration, std::time::Duration)> + 'static {
        let (local, path, rt) = (self.local.clone(), path.to_string(), self.rt.clone());
        self.run(async move {
            rt.spawn_blocking(move || {
                let started = query_times::Started::now();
                let _ = local.call(&path, &args);
                started.took()
            })
            .await
            .unwrap_or_default()
        })
    }

    /// The voice upload route (`/voice-recording-upload?token=`), answered here: the recording at `path` kept for the
    /// message or upload its ticket was given for (local_backend/voice.rs).
    pub fn upload_voice(&self, token: String, path: PathBuf) -> impl Future<Output = Result<(), String>> + 'static {
        let (local, rt) = (self.local.clone(), self.rt.clone());
        self.run(async move {
            rt.spawn_blocking(move || {
                let bytes = std::fs::read(&path).map_err(|_| "The recording could not be read.".to_string())?;
                local.upload_voice(&token, &bytes)
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()))
        })
    }

    /// Keeps a file sent with a message on this Mac (local_backend `save_attachment`); resolves to its storage id.
    pub fn save_attachment(
        &self,
        content_type: String,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<String>> + 'static {
        let (local, rt) = (self.local.clone(), self.rt.clone());
        self.run(async move {
            rt.spawn_blocking(move || local.save_attachment(&content_type, &bytes))
                .await
                .map_err(|error| anyhow!(error))
                .and_then(|saved| saved.map_err(|error| anyhow!(error)))
        })
    }

    /// Keeps a file of any size sent with a message without reading it (local_backend `keep_attachment`); resolves to
    /// its storage id.
    pub fn keep_attachment(
        &self,
        content_type: String,
        path: std::path::PathBuf,
        link: bool,
    ) -> impl Future<Output = Result<String>> + 'static {
        let (local, rt) = (self.local.clone(), self.rt.clone());
        self.run(async move {
            rt.spawn_blocking(move || local.keep_attachment(&content_type, &path, link))
                .await
                .map_err(|error| anyhow!(error))
                .and_then(|kept| kept.map_err(|error| anyhow!(error)))
        })
    }
}

/// Subscribes a view to a query. The subscription lives as long as the
/// returned task; drop it (or replace it) to unsubscribe.
pub fn subscribe<V: 'static, T: DeserializeOwned + 'static>(
    cx: &mut Context<V>,
    path: &str,
    args: Value,
    mut on_update: impl FnMut(&mut V, Result<T, String>, &mut Context<V>) + 'static,
) -> Task<()> {
    let mut rx = backend(cx).subscribe_raw(path, args);
    let path = path.to_string();
    cx.spawn(async move |this, cx| {
        while let Some(item) = rx.recv().await {
            let parsed = item.and_then(|value| {
                serde_json::from_value::<T>(value).map_err(|error| format!("Unexpected reply from {path}: {error}"))
            });
            if this.update(cx, |view, cx| on_update(view, parsed, cx)).is_err() {
                break;
            }
        }
    })
}

/// Runs a backend function and hands the result back to the view.
pub fn call<V: 'static, T: DeserializeOwned + 'static>(
    cx: &mut Context<V>,
    path: &str,
    args: Value,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Context<V>) + 'static,
) {
    let future = backend(cx).call_raw(path, args);
    let path = path.to_string();
    cx.spawn(async move |this, cx| {
        let result = future.await.and_then(|value| {
            serde_json::from_value::<T>(value).map_err(|error| format!("Unexpected reply from {path}: {error}"))
        });
        this.update(cx, |view, cx| done(view, result, cx)).ok();
    })
    .detach();
}

/// Like `call`, with the window available in the callback (dialogs, focus, toasts).
pub fn call_in<V: 'static, T: DeserializeOwned + 'static>(
    window: &Window,
    cx: &mut Context<V>,
    path: &str,
    args: Value,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Window, &mut Context<V>) + 'static,
) {
    let future = backend(cx).call_raw(path, args);
    let path = path.to_string();
    cx.spawn_in(window, async move |this, cx| {
        let result = future.await.and_then(|value| {
            serde_json::from_value::<T>(value).map_err(|error| format!("Unexpected reply from {path}: {error}"))
        });
        this.update_in(cx, |view, window, cx| done(view, result, window, cx)).ok();
    })
    .detach();
}

/// A backend function whose result the view doesn't need.
pub fn fire(cx: &mut App, path: &str, args: Value) {
    let future = backend(cx).call_raw(path, args);
    cx.background_spawn(async move {
        let _ = future.await;
    })
    .detach();
}

/// How long each live query took, while a drive measures them (`FILO_OPEN=chat-timing`, smoke/drives.rs).
#[cfg(feature = "dev")]
pub mod query_times {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use serde_json::Value;

    /// A query run: its path and arguments, when it started, how long it took and how much CPU its thread spent on it
    /// (the rest it waited: on the database, which answers one query at a time, or for a core).
    #[derive(Clone)]
    pub struct Run {
        pub path: String,
        pub args: Value,
        pub at: Instant,
        pub took: Duration,
        pub cpu: Duration,
    }

    static ON: AtomicBool = AtomicBool::new(false);
    static RUNS: Mutex<Vec<Run>> = Mutex::new(Vec::new());

    #[derive(Clone, Copy)]
    pub struct Started(Instant, Duration);

    impl Started {
        pub fn now() -> Self {
            Started(Instant::now(), thread_cpu())
        }

        /// (wall, CPU) since.
        pub fn took(self) -> (Duration, Duration) {
            (self.0.elapsed(), thread_cpu().saturating_sub(self.1))
        }
    }

    fn thread_cpu() -> Duration {
        let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid timespec to fill.
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
        Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
    }

    pub(super) fn record(path: &str, args: &Value, started: Started) {
        if ON.load(Ordering::Relaxed) {
            let (took, cpu) = started.took();
            RUNS.lock().unwrap().push(Run { path: path.to_string(), args: args.clone(), at: started.0, took, cpu });
        }
    }

    /// Starts keeping them, from none.
    pub fn start() {
        RUNS.lock().unwrap().clear();
        ON.store(true, Ordering::Relaxed);
    }

    /// The queries run since `start` or the last `take`.
    pub fn take() -> Vec<Run> {
        std::mem::take(&mut *RUNS.lock().unwrap())
    }
}

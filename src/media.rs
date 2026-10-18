// Media plumbing shared by screens:
// - `ReqwestHttp`: GPUI's HTTP client, so `img("https://…")` loads remote images
//   (feed covers, artifact previews, chat attachments) through the Tokio runtime. A download gives up after
//   `PICTURE_TIMEOUT`, refuses bodies over `PICTURE_MAX_BYTES`, and stops when nobody waits for it any more.
//   A `file://localhost/…` url (avatars.rs `file_url`: the pets the agents drew) is read from disk, only inside the
//   characters' folder.
// - `Audio`: one sound at a time across the app (voice previews, voice messages, audio files), played from its file on
//   this Mac by the video player's AVPlayer (video_player.rs `open_audio`), with seeking and a speed for every sound.
//   A video opening in the viewer stops it.

use std::time::Duration;

use anyhow::anyhow;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use gpui::{App, Global, SharedString};
use gpui_http_client::http::{HeaderValue, Response};
use gpui_http_client::{AsyncBody, HttpClient, Url};

const PICTURE_TIMEOUT: Duration = Duration::from_secs(30);
const PICTURE_MAX_BYTES: usize = 64 << 20;

/// Stops the download when its response future is dropped (a picture given up on); a no-op once it has finished.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct ReqwestHttp {
    rt: tokio::runtime::Handle,
    client: reqwest::Client,
    user_agent: HeaderValue,
}

impl ReqwestHttp {
    pub fn new(rt: tokio::runtime::Handle) -> Self {
        ReqwestHttp {
            rt,
            client: reqwest::Client::builder().user_agent("filo-desktop").build().expect("http client"),
            user_agent: HeaderValue::from_static("filo-desktop"),
        }
    }
}

impl HttpClient for ReqwestHttp {
    fn type_name(&self) -> &'static str {
        "ReqwestHttp"
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        req: gpui_http_client::Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        // Only GETs come through here (image loading); request bodies are ignored.
        let url = req.uri().to_string();
        if let Some(path) = crate::avatars::file_of_url(&url) {
            let task = self.rt.spawn_blocking(move || {
                let folder = crate::config::config_dir().join("local").join("characters");
                anyhow::ensure!(path.starts_with(&folder), "not a picture Filo keeps");
                anyhow::Ok(std::fs::read(&path)?)
            });
            return async move {
                let bytes = task.await.map_err(|error| anyhow!(error))??;
                Ok(Response::builder().status(200).body(AsyncBody::from(bytes))?)
            }
            .boxed();
        }
        let client = self.client.clone();
        let task = self.rt.spawn(async move {
            let mut response = client.get(url).timeout(PICTURE_TIMEOUT).send().await?;
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > PICTURE_MAX_BYTES {
                    anyhow::bail!("larger than {} MB", PICTURE_MAX_BYTES >> 20);
                }
                bytes.extend_from_slice(&chunk);
            }
            anyhow::Ok((status, bytes))
        });
        let abort = AbortOnDrop(task.abort_handle());
        async move {
            let _abort = abort;
            let (status, bytes) = task.await.map_err(|error| anyhow!(error))??;
            Ok(Response::builder().status(status).body(AsyncBody::from(bytes))?)
        }
        .boxed()
    }
}

/// How fast a sound can play, as its speed button steps through them.
pub const SPEEDS: [f32; 3] = [1., 1.5, 2.];

/// The one sound playing or paused (`current`), and the speed every sound plays at.
#[derive(Default)]
pub struct Audio {
    current: Option<Sound>,
    /// Into `SPEEDS`.
    speed: usize,
    generation: u64,
}

impl Global for Audio {}

struct Sound {
    id: SharedString,
    player: std::rc::Rc<crate::video_player::Player>,
    /// Where to go once the file is ready (a press on a sound's waveform before it played): it plays from there.
    pending: Option<f64>,
    /// Played, not paused: AVFoundation's player stops by itself (its rate 0) only at the end, which can come before
    /// the length it gives (a FLAC's).
    wanted: bool,
    /// The window that started it, drawn again while it plays.
    window: Option<gpui::AnyWindowHandle>,
}

/// Where the sound playing or paused under some id is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playing {
    pub time: f64,
    /// Once AVFoundation knows.
    pub duration: Option<f64>,
    pub playing: bool,
}

fn current<'a>(id: &str, cx: &'a App) -> Option<&'a Sound> {
    cx.try_global::<Audio>().and_then(|audio| audio.current.as_ref()).filter(|sound| sound.id == id)
}

/// Whether the sound under `id` is playing now (not paused).
pub fn is_playing(id: &str, cx: &App) -> bool {
    state(id, cx).is_some_and(|state| state.playing)
}

/// The sound under `id` when it's the one playing or paused.
pub fn state(id: &str, cx: &App) -> Option<Playing> {
    let sound = current(id, cx)?;
    Some(match sound.pending {
        Some(time) => Playing { time, duration: sound.player.duration(), playing: true },
        None => {
            Playing { time: sound.player.time(), duration: sound.player.duration(), playing: sound.player.playing() }
        }
    })
}

/// The sound playing or paused, by its id, and where it is (fixture checks).
#[cfg(feature = "dev")]
pub fn now_playing(cx: &App) -> Option<(SharedString, Playing, f32)> {
    let sound = cx.try_global::<Audio>()?.current.as_ref()?;
    Some((sound.id.clone(), state(&sound.id, cx)?, speed(cx)))
}

/// How fast sounds play.
pub fn speed(cx: &App) -> f32 {
    SPEEDS[cx.try_global::<Audio>().map_or(0, |audio| audio.speed)]
}

/// The next speed (1×, 1.5×, 2×, then 1× again), for every sound and at once for the one playing.
pub fn next_speed(cx: &mut App) {
    let audio = cx.default_global::<Audio>();
    audio.speed = (audio.speed + 1) % SPEEDS.len();
    let speed = SPEEDS[audio.speed];
    if let Some(sound) = audio.current.as_ref().filter(|sound| sound.player.playing()) {
        sound.player.play_at(speed);
    }
    cx.refresh_windows();
}

pub fn stop(cx: &mut App) {
    let audio = cx.default_global::<Audio>();
    // Dropping its player stops it.
    audio.current = None;
    audio.generation += 1;
    cx.refresh_windows();
}

/// The file `url` names: a `file://` address or a path (every recording and audio file is on this Mac).
fn file_of(url: &str) -> Option<std::path::PathBuf> {
    let path = url.strip_prefix("file://").unwrap_or(url);
    path.starts_with('/').then(|| std::path::PathBuf::from(path))
}

/// Plays the file at `url` under `id`, pausing it if it's playing, or playing on where it was paused (from the start
/// once it ended). Whatever else played stops: one sound at a time.
pub fn toggle(id: impl Into<SharedString>, url: String, cx: &mut App) {
    let id = id.into();
    let speed = speed(cx);
    if let Some(sound) = cx.default_global::<Audio>().current.as_mut().filter(|sound| sound.id == id) {
        if sound.player.playing() {
            sound.player.pause();
            sound.wanted = false;
        } else if sound.pending.is_none() {
            sound.player.play_at(speed);
            sound.wanted = true;
        }
        cx.refresh_windows();
        return;
    }
    start(id, &url, None, cx);
}

/// Goes `seconds` into the sound under `id`: at once when it's the one playing or paused (`exact` lands on that
/// sample, else near it, while a drag goes on), else it starts playing from there.
pub fn seek(id: impl Into<SharedString>, url: &str, seconds: f64, exact: bool, cx: &mut App) {
    let id = id.into();
    let audio = cx.default_global::<Audio>();
    let Some(sound) = audio.current.as_mut().filter(|sound| sound.id == id) else {
        return start(id, url, Some(seconds), cx);
    };
    if sound.player.duration().is_some() {
        sound.player.seek(seconds, exact);
    } else {
        sound.pending = Some(seconds);
    }
    cx.refresh_windows();
}

/// Stops whatever played and plays `url` under `id`, from `from` seconds in once the file is ready.
fn start(id: SharedString, url: &str, from: Option<f64>, cx: &mut App) {
    stop(cx);
    let Some(player) = file_of(url).and_then(|path| crate::video_player::Player::open_audio(&path)) else { return };
    // An ended sound is let go (`stop`), so it plays from the start the next time.
    // A test window makes no sound.
    player.set_muted(crate::test_run::active());
    if from.is_none() {
        player.play_at(speed(cx));
    }
    let window = cx.active_window();
    let generation = {
        let audio = cx.default_global::<Audio>();
        audio.current = Some(Sound { id, player, pending: from, wanted: true, window });
        audio.generation
    };
    // Ten times a second while it's there: a pending seek once the file is ready, the window drawn again while it
    // plays (its time and waveform move), and it goes once it ends or can't play.
    cx.spawn(async move |cx| {
        let mut was = None;
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let going = cx.update(|cx| {
                let speed = speed(cx);
                let audio = cx.default_global::<Audio>();
                if audio.generation != generation {
                    return false;
                }
                let Some(sound) = audio.current.as_mut() else { return false };
                let (player, window) = (sound.player.clone(), sound.window);
                if let Some(at) = sound.pending
                    && player.duration().is_some()
                {
                    player.seek(at, true);
                    player.play_at(speed);
                    sound.pending = None;
                }
                let ended = sound.pending.is_none() && sound.wanted && !player.playing();
                if player.failed() || ended {
                    stop(cx);
                    return false;
                }
                let now = (player.playing(), player.duration().is_some());
                if now.0 || was != Some(now) {
                    match window {
                        Some(window) => {
                            window.update(cx, |_, window, _| window.refresh()).ok();
                        }
                        None => cx.refresh_windows(),
                    }
                }
                was = Some(now);
                true
            });
            if !going.unwrap_or(false) {
                break;
            }
        }
    })
    .detach();
    cx.refresh_windows();
}

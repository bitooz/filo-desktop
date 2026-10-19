// Media plumbing shared by screens:
// - `ReqwestHttp`: GPUI's HTTP client, so `img("https://…")` loads remote images
//   (feed covers, artifact previews, chat attachments) through the Tokio runtime. A download gives up after
//   `PICTURE_TIMEOUT`, refuses bodies over `PICTURE_MAX_BYTES`, and stops when nobody waits for it any more.
//   A `file://localhost/…` url (avatars.rs `file_url`: the pets the agents drew) is read from disk, only inside the
//   characters' folder.
// - `Audio`: one sound at a time across the app (voice previews, voice messages, audio files), played from its file on
//   this Mac by the video player's AVPlayer (video_player.rs `open_audio`), with seeking and a speed for every sound.
//   A video playing in the viewer pauses it (where it is) and a sound starting pauses the video; nothing plays while
//   the microphone records. Which files are sounds is `is_audio`, by filo-local's one table of types.
// - `file_key` and `kept_folder`: where what's made from a file (a video's poster, a sound's waveform) is kept.

use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use anyhow::anyhow;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use gpui::{App, EntityId, Global, SharedString, Window};

use crate::video_player::Player;
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

/// Whether a file is a sound the app plays: its content type says so, or its name does by filo-local's one table (a
/// FLAC an older Filo sent as `application/octet-stream`).
pub fn is_audio(content_type: &str, name: &str) -> bool {
    content_type.starts_with("audio/")
        || filo_local::attachments::content_type_for(Path::new(name)).starts_with("audio/")
}

/// A name for what's made from `file` and kept (a video's poster, a sound's waveform), from its path, size and time,
/// so a changed file gets a new one; None when the file isn't there.
pub fn file_key(file: &Path) -> Option<String> {
    use sha2::Digest as _;
    let meta = std::fs::metadata(file).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_millis());
    let id = sha2::Sha256::digest(format!("{}\n{}\n{modified}", file.display(), meta.len()).as_bytes());
    Some(id[..10].iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The temporary folder what's made from files of one `kind` ("posters", "waves") is kept in, by `file_key`.
pub fn kept_folder(kind: &str) -> PathBuf {
    std::env::temp_dir().join(format!("filo-desktop-{kind}"))
}

/// How fast a sound can play, as its speed button steps through them.
pub const SPEEDS: [f32; 3] = [1., 1.5, 2.];

/// The one sound playing or paused (`current`), the speed every sound plays at, and the viewer's video.
#[derive(Default)]
pub struct Audio {
    current: Option<Sound>,
    /// Into `SPEEDS`.
    speed: usize,
    generation: u64,
    /// The video playing in the viewer (media_viewer.rs, `video_plays`), paused when a sound starts.
    video: Option<Weak<Player>>,
}

impl Global for Audio {}

struct Sound {
    id: SharedString,
    player: Rc<Player>,
    /// Where to go once the file is ready (a press on a sound's waveform before it played): it plays from there.
    pending: Option<f64>,
    /// Played, not paused (also while it waits for its file).
    wanted: bool,
    /// How long it is when its row knows (its samples', audio_row.rs), else AVFoundation's length, which can run past
    /// the sound's end (a FLAC's). The player stopping by itself within a moment of it, or saying it played to its end
    /// (which a FLAC can do sooner), is the sound ending; stopped anywhere else, it's kept there, paused.
    length: Option<f64>,
    /// The views that drew it since it last changed (`shown_in`), drawn again when what they show changes.
    views: Vec<EntityId>,
}

impl Sound {
    fn end(&self) -> Option<f64> {
        self.length.or(self.player.duration())
    }
}

/// Where the sound playing or paused under some id is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playing {
    pub time: f64,
    /// Its length, once known.
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
        Some(time) => Playing { time, duration: sound.end(), playing: sound.wanted },
        None => Playing { time: sound.player.time(), duration: sound.end(), playing: sound.player.playing() },
    })
}

/// The view being drawn shows the sound under `id`: it's drawn again when the sound's time or played part changes.
pub fn shown_in(id: &str, window: &Window, cx: &mut App) {
    let view = window.current_view();
    if let Some(sound) = cx.default_global::<Audio>().current.as_mut().filter(|sound| sound.id == id)
        && !sound.views.contains(&view)
    {
        sound.views.push(view);
    }
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

/// The viewer's video plays (media_viewer.rs): a sound playing pauses where it is, and the video pauses when a sound
/// starts.
pub fn video_plays(video: &Rc<Player>, cx: &mut App) {
    let audio = cx.default_global::<Audio>();
    audio.video = Some(Rc::downgrade(video));
    if let Some(sound) = audio.current.as_mut().filter(|sound| sound.wanted) {
        sound.player.pause();
        sound.wanted = false;
        cx.refresh_windows();
    }
}

/// A sound is about to play: the viewer's video pauses. False while the microphone records, when nothing may play
/// (it would be in the recording).
fn may_play(audio: &Audio) -> bool {
    if crate::recorder::recording() {
        return false;
    }
    if let Some(video) = audio.video.as_ref().and_then(Weak::upgrade) {
        video.pause();
    }
    true
}

/// The file `url` names: a `file://` address or a path (every recording and audio file is on this Mac).
fn file_of(url: &str) -> Option<PathBuf> {
    let path = url.strip_prefix("file://").unwrap_or(url);
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// Plays the file at `url` under `id` (`length` long when its row knows), pausing it if it's playing, or playing on
/// where it was paused (from the start once it ended). Whatever else played stops: one sound at a time.
pub fn toggle(id: impl Into<SharedString>, url: String, length: Option<f64>, cx: &mut App) {
    let id = id.into();
    let speed = speed(cx);
    let audio = cx.default_global::<Audio>();
    let allowed = audio.current.as_ref().is_some_and(|sound| sound.id == id && !sound.wanted) && may_play(audio);
    if let Some(sound) = audio.current.as_mut().filter(|sound| sound.id == id) {
        if sound.wanted {
            // Paused, or no longer to start once its file is ready.
            sound.player.pause();
            sound.wanted = false;
        } else if allowed {
            if sound.pending.is_none() {
                // Paused just as it ended: from the start.
                if sound.player.ended() {
                    sound.player.seek(0., true);
                }
                sound.player.play_at(speed);
            }
            sound.wanted = true;
        }
        cx.refresh_windows();
        return;
    }
    start(id, &url, length, None, cx);
}

/// Goes `seconds` into the sound under `id` (`length` long when its row knows): at once when it's the one playing or
/// paused (`exact` lands on that sample, else near it, while a drag goes on), else it starts playing from there.
pub fn seek(id: impl Into<SharedString>, url: &str, length: Option<f64>, seconds: f64, exact: bool, cx: &mut App) {
    let id = id.into();
    let audio = cx.default_global::<Audio>();
    let Some(sound) = audio.current.as_mut().filter(|sound| sound.id == id) else {
        return start(id, url, length, Some(seconds), cx);
    };
    sound.length = length.or(sound.length);
    if sound.player.duration().is_some() {
        sound.player.seek(seconds, exact);
    } else {
        sound.pending = Some(seconds);
    }
    cx.refresh_windows();
}

/// Stops whatever played and plays `url` under `id`, from `from` seconds in once the file is ready.
fn start(id: SharedString, url: &str, length: Option<f64>, from: Option<f64>, cx: &mut App) {
    stop(cx);
    if !may_play(cx.default_global::<Audio>()) {
        return;
    }
    let Some(player) = file_of(url).and_then(|path| Player::open_audio(&path)) else { return };
    // A test window makes no sound.
    player.set_muted(crate::test_run::active());
    if from.is_none() {
        player.play_at(speed(cx));
    }
    let generation = {
        let audio = cx.default_global::<Audio>();
        audio.current = Some(Sound { id, player, pending: from, wanted: true, length, views: Vec::new() });
        audio.generation
    };
    // Ten times a second while it's there: a pending seek once the file is ready, the views that show it drawn again
    // when its clock or its played part moves, and it goes once it ends (let go, so it plays from the start the next
    // time) or can't play.
    cx.spawn(async move |cx| {
        let mut shown = None;
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let going = cx.update(|cx| {
                let speed = speed(cx);
                let audio = cx.default_global::<Audio>();
                if audio.generation != generation {
                    return false;
                }
                let Some(sound) = audio.current.as_mut() else { return false };
                let player = sound.player.clone();
                if let Some(at) = sound.pending
                    && player.duration().is_some()
                {
                    player.seek(at, true);
                    if sound.wanted {
                        player.play_at(speed);
                    }
                    sound.pending = None;
                }
                if player.failed() {
                    stop(cx);
                    return false;
                }
                let end = sound.end();
                let mut stopped_short = false;
                if sound.pending.is_none() && sound.wanted && !player.playing() {
                    if player.ended() || end.is_none_or(|end| player.time() >= end - 0.25) {
                        stop(cx);
                        return false;
                    }
                    // Stopped short of its end (not by Filo): kept where it is, paused. Rare, so every window draws
                    // again: voice messages' and Shared's buttons show it too, and they don't say they do
                    // (`shown_in`).
                    sound.wanted = false;
                    stopped_short = true;
                }
                let time = sound.pending.unwrap_or_else(|| player.time());
                let now = (
                    sound.wanted,
                    time.floor() as i64,
                    end.map(|end| (time / end.max(0.001) * BAR_STEPS).floor() as i64),
                );
                if shown != Some(now) {
                    shown = Some(now);
                    for view in std::mem::take(&mut sound.views) {
                        cx.notify(view);
                    }
                }
                if stopped_short {
                    cx.refresh_windows();
                }
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

/// The steps a sound's played part moves in: finer than any row's bars.
const BAR_STEPS: f64 = 200.;

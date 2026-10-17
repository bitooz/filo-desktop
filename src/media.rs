// Media plumbing shared by screens:
// - `ReqwestHttp`: GPUI's HTTP client, so `img("https://…")` loads remote images
//   (feed covers, artifact previews, chat attachments) through the Tokio runtime. A download gives up after
//   `PICTURE_TIMEOUT`, refuses bodies over `PICTURE_MAX_BYTES`, and stops when nobody waits for it any more.
//   A `file://localhost/…` url (avatars.rs `file_url`: the pets the agents drew) is read from disk, only inside the
//   characters' folder.
// - `audio`: one sound at a time (voice previews, voice messages, audio files),
//   played with macOS `afplay` from its file on this Mac.

use std::sync::{Arc, Mutex};
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

/// Which sound is playing; observe it with `cx.observe_global::<Audio>`.
#[derive(Default)]
pub struct Audio {
    pub playing: Option<SharedString>,
    child: Option<Arc<Mutex<std::process::Child>>>,
    generation: u64,
}

impl Global for Audio {}

pub fn is_playing(id: &str, cx: &App) -> bool {
    cx.try_global::<Audio>().and_then(|audio| audio.playing.as_ref()).is_some_and(|playing| playing == id)
}

pub fn stop(cx: &mut App) {
    let audio = cx.default_global::<Audio>();
    if let Some(child) = audio.child.take() {
        let _ = child.lock().unwrap().kill();
    }
    audio.playing = None;
    audio.generation += 1;
    cx.refresh_windows();
}

/// Plays the file at `url` (a `file://` address: every recording and audio file is on this Mac) under `id`, or stops
/// it if `id` is already playing.
pub fn toggle(id: impl Into<SharedString>, url: String, cx: &mut App) {
    let id = id.into();
    let was_playing = is_playing(&id, cx);
    stop(cx);
    if was_playing {
        return;
    }
    let generation = {
        let audio = cx.default_global::<Audio>();
        audio.playing = Some(id.clone());
        audio.generation
    };
    let file = url.strip_prefix("file://").map(std::path::PathBuf::from);
    cx.spawn(async move |cx| {
        let finish = |cx: &mut gpui::AsyncApp| {
            cx.update(|cx| {
                let audio = cx.default_global::<Audio>();
                if audio.generation == generation {
                    audio.playing = None;
                    audio.child = None;
                    cx.refresh_windows();
                }
            })
            .ok();
        };
        let Some(path) = file else {
            finish(cx);
            return;
        };
        let Ok(child) = std::process::Command::new("afplay").arg(&path).spawn() else {
            finish(cx);
            return;
        };
        let child = Arc::new(Mutex::new(child));
        let still_current = cx
            .update(|cx| {
                let audio = cx.default_global::<Audio>();
                if audio.generation == generation {
                    audio.child = Some(child.clone());
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if !still_current {
            let _ = child.lock().unwrap().kill();
            return;
        }
        // Poll for exit without blocking the UI thread.
        loop {
            cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            let done = matches!(child.lock().unwrap().try_wait(), Ok(Some(_)) | Err(_));
            if done {
                break;
            }
        }
        finish(cx);
    })
    .detach();
}

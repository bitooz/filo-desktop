// Videos played inside the app (media_viewer.rs): AVFoundation's player drawing into a plain native view set into the
// window over the box its element lays out. The viewer draws its own controls (play, the time and the scrubber, mute,
// full screen) in its bar, as it does a picture's, so the view has none: clicks and the wheel go on through it to the
// window (an NSView passes what it doesn't handle to its superview, GPUI's), and the keyboard stays with the app, so
// Esc, Space and the arrows work as they do over a picture. A native view draws above everything GPUI paints, so
// nothing of the viewer's goes over that box. The view follows the box each frame the element is drawn, and leaves the
// window with the player. It plays the file on this Mac (file_preview `download`). AVFoundation decodes and draws it, so
// playing costs GPUI no frames. A sound (an audio file, a voice message: media.rs) plays through the same player, with
// no view (`open_audio`).
//
// Before it plays, a video shows as its poster (`poster`): the still a shared result's page shows for it
// (result_share.rs) and its length, made once from the file and kept in a temporary folder, with the play button over
// it (`badge`).
// Chat tiles, a coding result's recordings and the shared-media grid show it where nothing gave them a thumbnail.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    App, Bounds, Element, ElementId, Global, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    ParentElement as _, Pixels, Size, Style, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use crate::file_preview::Item;
use crate::theme::{ActiveTheme as _, Typography as _, radius};
use crate::ui;

/// One video, playing once its element is first drawn; it stops and leaves the window when dropped.
pub struct Player {
    native: native::NativePlayer,
}

impl Player {
    /// The video at `path`; `None` when AVFoundation can't open it.
    pub fn open(path: &Path) -> Option<Rc<Self>> {
        native::NativePlayer::new(path).map(|native| Rc::new(Player { native }))
    }

    /// The sound at `path`, with no picture (media.rs `toggle`: an audio file in a chat or on a result page); it plays
    /// once told to. `None` when AVFoundation can't open it.
    pub fn open_audio(path: &Path) -> Option<Rc<Self>> {
        native::NativePlayer::audio(path).map(|native| Rc::new(Player { native }))
    }

    /// The box the video shows in: it takes all the room its parent gives it, the picture fitted inside.
    pub fn element(self: &Rc<Self>) -> PlayerElement {
        PlayerElement { player: self.clone() }
    }

    pub fn playing(&self) -> bool {
        self.native.rate() != 0.
    }

    /// Plays, from the start once it has ended, or pauses.
    pub fn toggle(&self) {
        if self.playing() {
            self.native.pause();
            return;
        }
        if self.duration().is_some_and(|duration| self.time() >= duration - 0.05) {
            self.native.seek(0., true);
        }
        self.native.play();
    }

    pub fn pause(&self) {
        self.native.pause();
    }

    pub fn play(&self) {
        self.native.play();
    }

    /// Plays at `rate` times its speed (1.5 and 2 for a sound's speed button), keeping its pitch.
    pub fn play_at(&self, rate: f32) {
        self.native.set_rate(rate);
    }

    /// Where it is, in seconds.
    pub fn time(&self) -> f64 {
        self.native.time()
    }

    /// How long it is, in seconds, once AVFoundation knows.
    pub fn duration(&self) -> Option<f64> {
        self.native.duration()
    }

    /// To `seconds` in: `exact` lands on that frame, else on the nearest one that's quick to show (while scrubbing).
    pub fn seek(&self, seconds: f64, exact: bool) {
        let seconds = match self.duration() {
            Some(end) => seconds.clamp(0., end),
            None => seconds.max(0.),
        };
        self.native.seek(seconds, exact);
    }

    pub fn muted(&self) -> bool {
        self.native.muted()
    }

    pub fn set_muted(&self, muted: bool) {
        self.native.set_muted(muted);
    }

    /// Couldn't play it (a file AVFoundation can't read): the player says so once it has tried.
    pub fn failed(&self) -> bool {
        self.native.failed()
    }

    /// A sound (`open_audio`) played to its end, as AVFoundation tells it: sometimes before the length it gives, or the
    /// one its samples make (a FLAC's). Always false for a video.
    pub fn ended(&self) -> bool {
        self.native.ended()
    }

    fn place(&self, bounds: Bounds<Pixels>, window: &Window) {
        self.native.place(bounds, window);
    }
}

pub struct PlayerElement {
    player: Rc<Player>,
}

impl IntoElement for PlayerElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PlayerElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style { size: Size::full(), flex_grow: 1., flex_shrink: 1., ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        self.player.place(bounds, window);
    }

    /// What GPUI paints after the video, over it (a notification, a toast, a dialog: gpui's `Window::painted_over`), shows
    /// through holes cut in its view once the frame is drawn (web_holes.rs); clicks go through the view anyway.
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        let index = window.scene_index();
        let player = self.player.clone();
        window.on_next_frame(move |window, _| {
            let over = window.painted_over(index, bounds);
            let view = player.native.view();
            if !view.is_null() {
                // SAFETY: on the main thread; `player` holds its view.
                unsafe { crate::web_holes::cut_view(view, bounds, &over) };
            }
        });
    }
}

/// A video's frame to show before it plays, and its length.
#[derive(Clone, Debug, PartialEq)]
pub struct Poster {
    /// The frame, a JPEG in a temporary folder.
    pub picture: PathBuf,
    /// Its size in pixels, turned as the video plays.
    pub size: (f32, f32),
    pub duration_ms: Option<f64>,
}

/// Posters by video (its file on this Mac, else its URL): `None` while one is being made, or when none could be.
#[derive(Default)]
struct Posters(HashMap<String, Option<Poster>>);

impl Global for Posters {}

/// `item`'s poster once it's made; the first ask starts making it (from the downloaded copy, file_preview's cache, so
/// playing it later fetches nothing), and the windows draw again when it's there.
pub fn poster(item: &Item, cx: &mut App) -> Option<Poster> {
    let key = item.file.as_ref().map_or_else(|| item.url.clone(), |file| file.display().to_string());
    if key.is_empty() {
        return None;
    }
    let posters = cx.default_global::<Posters>();
    if let Some(known) = posters.0.get(&key) {
        return known.clone();
    }
    posters.0.insert(key.clone(), None);
    let saving = crate::file_preview::download(item);
    cx.spawn(async move |cx| {
        let made = match saving.await {
            Ok(video) => cx.background_executor().spawn(async move { make_poster(&video) }).await,
            Err(_) => None,
        };
        cx.update(|cx| {
            if made.is_some() {
                cx.default_global::<Posters>().0.insert(key, made);
                cx.refresh_windows();
            }
        })
        .ok();
    })
    .detach();
    None
}

/// The poster for the video file at `video` (result_share.rs `poster`, Quick Look's still of it) and its length: made
/// once per file (by its path, size and time), then read back.
fn make_poster(video: &Path) -> Option<Poster> {
    let name = crate::media::file_key(video)?;
    let folder = crate::media::kept_folder("posters");
    let picture = folder.join(format!("{name}.jpg"));
    let facts = folder.join(format!("{name}.json"));
    if let Some(poster) = std::fs::read(&facts)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|facts| {
            let number = |key: &str| facts.get(key).and_then(serde_json::Value::as_f64);
            Some(Poster {
                picture: picture.clone(),
                size: (number("width")? as f32, number("height")? as f32),
                duration_ms: number("durationMs"),
            })
        })
        && picture.is_file()
    {
        return Some(poster);
    }
    std::fs::create_dir_all(&folder).ok()?;
    // The same still a shared result's page shows for it.
    std::fs::write(&picture, crate::local_backend::result_share::poster(video)?).ok()?;
    let (width, height) = image::image_dimensions(&picture).ok()?;
    let duration_ms = native::length_ms(video);
    let written = serde_json::json!({ "width": width, "height": height, "durationMs": duration_ms });
    std::fs::write(&facts, written.to_string()).ok()?;
    Some(Poster { picture, size: (width as f32, height as f32), duration_ms })
}

/// Over a video's poster (in a `relative` box): the play button in the middle, and its length in the bottom right
/// corner once it's known.
pub fn badge(duration_ms: Option<f64>, cx: &App) -> gpui::Div {
    let colors = cx.colors();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .size(px(44.))
                .rounded(px(radius::FULL))
                .bg(colors.media.opacity(0.62))
                .flex()
                .items_center()
                .justify_center()
                .child(ui::icon("play").size(px(20.)).text_color(colors.on_media)),
        )
        .when_some(duration_ms, |this, duration| {
            this.child(
                div()
                    .absolute()
                    .bottom(px(6.))
                    .right(px(6.))
                    .px(px(6.))
                    .rounded(px(radius::XS))
                    .bg(colors.media.opacity(0.62))
                    .micro()
                    .text_color(colors.on_media)
                    .child(crate::format::duration_clock(duration)),
            )
        })
}

mod native {
    use std::path::Path;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{Bounds, Pixels, Window};
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{Encode, Encoding};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}

    /// Core Media's time: `value / timescale` seconds.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    // SAFETY: the struct as Core Media declares it (an unnamed struct: `{?=qiIq}`).
    unsafe impl Encode for CMTime {
        const ENCODING: Encoding = Encoding::Struct("?", &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING]);
    }

    const VALID: u32 = 1;
    const INDEFINITE: u32 = 16;

    impl CMTime {
        fn seconds(seconds: f64) -> Self {
            CMTime { value: (seconds * 600.).round() as i64, timescale: 600, flags: VALID, epoch: 0 }
        }

        fn as_seconds(&self) -> Option<f64> {
            (self.flags & VALID != 0 && self.flags & INDEFINITE == 0 && self.timescale > 0)
                .then(|| self.value as f64 / self.timescale as f64)
        }
    }

    /// AVPlayerItemStatusFailed.
    const ITEM_FAILED: isize = 2;

    pub struct NativePlayer {
        /// The picture's view; a sound has none.
        view: Option<Retained<AnyObject>>,
        player: Retained<AnyObject>,
        /// A sound's end: set by AVPlayerItemDidPlayToEndTimeNotification, through the observer kept here.
        end: Option<(Arc<AtomicBool>, Retained<AnyObject>)>,
    }

    fn file_url(path: &Path) -> Option<Retained<AnyObject>> {
        let string = NSString::from_str(path.to_str()?);
        // SAFETY: a Foundation class method, any thread.
        unsafe { msg_send![AnyClass::get(c"NSURL")?, fileURLWithPath: &*string] }
    }

    impl NativePlayer {
        pub fn new(path: &Path) -> Option<Self> {
            let (view_class, player_class, layer_class) =
                (AnyClass::get(c"NSView")?, AnyClass::get(c"AVPlayer")?, AnyClass::get(c"AVPlayerLayer")?);
            let url = file_url(path)?;
            // SAFETY: AppKit and AVFoundation messages on the main thread, where GPUI builds elements.
            unsafe {
                let player: Option<Retained<AnyObject>> = msg_send![player_class, playerWithURL: &*url];
                let player = player?;
                let layer: Option<Retained<AnyObject>> = msg_send![layer_class, playerLayerWithPlayer: &*player];
                let view: Option<Retained<AnyObject>> = msg_send![view_class, new];
                let view = view?;
                // A layer-hosting view: the layer is the player's picture, fitted in (AVLayerVideoGravityResizeAspect,
                // its default), with nothing behind it, so the viewer's backdrop shows either side.
                let _: () = msg_send![&*view, setLayer: &*layer?];
                let _: () = msg_send![&*view, setWantsLayer: true];
                Some(NativePlayer { view: Some(view), player, end: None })
            }
        }

        pub fn audio(path: &Path) -> Option<Self> {
            let url = file_url(path)?;
            // SAFETY: as in `new`.
            let player: Option<Retained<AnyObject>> =
                unsafe { msg_send![AnyClass::get(c"AVPlayer")?, playerWithURL: &*url] };
            let player = player?;
            let ended = Arc::new(AtomicBool::new(false));
            let told = ended.clone();
            let block = block2::RcBlock::new(move |_: NonNull<AnyObject>| told.store(true, Ordering::Relaxed));
            let name = NSString::from_str("AVPlayerItemDidPlayToEndTimeNotification");
            // SAFETY: as in `new`; the center keeps the block, and `Drop` takes the observer away.
            let observer: Option<Retained<AnyObject>> = unsafe {
                let center: Retained<AnyObject> = msg_send![AnyClass::get(c"NSNotificationCenter")?, defaultCenter];
                let item: Option<Retained<AnyObject>> = msg_send![&*player, currentItem];
                msg_send![&*center, addObserverForName: &*name, object: item.as_deref(), queue: std::ptr::null::<AnyObject>(), usingBlock: &*block]
            };
            Some(NativePlayer { view: None, player, end: observer.map(|observer| (ended, observer)) })
        }

        /// Its view, which `place` puts in the window.
        pub fn view(&self) -> *const AnyObject {
            self.view.as_ref().map_or(std::ptr::null(), Retained::as_ptr)
        }

        /// Into the window on the first call (and plays), then onto `bounds` (window points from the top left).
        pub fn place(&self, bounds: Bounds<Pixels>, window: &Window) {
            let (Some(view), Some(parent)) = (&self.view, content_view(window)) else { return };
            let (x, y) = (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y)));
            let (width, height) = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
            // SAFETY: as in `new`; `parent` is GPUI's view, alive while its window is.
            unsafe {
                let superview: *mut AnyObject = msg_send![&**view, superview];
                if superview != parent {
                    if !superview.is_null() {
                        let _: () = msg_send![&**view, removeFromSuperview];
                    }
                    let _: () = msg_send![parent, addSubview: &**view];
                    let _: () = msg_send![&*self.player, play];
                }
                let flipped: bool = msg_send![parent, isFlipped];
                let parent_bounds: NSRect = msg_send![parent, bounds];
                let y = if flipped { y } else { parent_bounds.size.height - y - height };
                let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
                let current: NSRect = msg_send![&**view, frame];
                if current != frame {
                    let _: () = msg_send![&**view, setFrame: frame];
                }
            }
        }

        pub fn rate(&self) -> f32 {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, rate] }
        }

        pub fn play(&self) {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, play] }
        }

        pub fn pause(&self) {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, pause] }
        }

        /// Plays at `rate` (AVPlayer keeps the pitch).
        pub fn set_rate(&self, rate: f32) {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, setRate: rate] }
        }

        pub fn time(&self) -> f64 {
            // SAFETY: as in `new`.
            let time: CMTime = unsafe { msg_send![&*self.player, currentTime] };
            time.as_seconds().unwrap_or(0.).max(0.)
        }

        fn item(&self) -> Option<Retained<AnyObject>> {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, currentItem] }
        }

        pub fn duration(&self) -> Option<f64> {
            let item = self.item()?;
            // SAFETY: as in `new`.
            let duration: CMTime = unsafe { msg_send![&*item, duration] };
            duration.as_seconds().filter(|seconds| *seconds > 0.)
        }

        pub fn ended(&self) -> bool {
            self.end.as_ref().is_some_and(|(ended, _)| ended.load(Ordering::Relaxed))
        }

        pub fn failed(&self) -> bool {
            self.item().is_some_and(|item| {
                // SAFETY: as in `new`.
                let status: isize = unsafe { msg_send![&*item, status] };
                status == ITEM_FAILED
            })
        }

        /// Also takes back a sound's end: it's somewhere else now.
        pub fn seek(&self, seconds: f64, exact: bool) {
            if let Some((ended, _)) = &self.end {
                ended.store(false, Ordering::Relaxed);
            }
            let tolerance = if exact { CMTime::seconds(0.) } else { CMTime::seconds(0.25) };
            // SAFETY: as in `new`.
            unsafe {
                msg_send![&*self.player, seekToTime: CMTime::seconds(seconds), toleranceBefore: tolerance, toleranceAfter: tolerance]
            }
        }

        pub fn muted(&self) -> bool {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, isMuted] }
        }

        pub fn set_muted(&self, muted: bool) {
            // SAFETY: as in `new`.
            unsafe { msg_send![&*self.player, setMuted: muted] }
        }
    }

    impl Drop for NativePlayer {
        fn drop(&mut self) {
            // SAFETY: as in `new`.
            unsafe {
                let _: () = msg_send![&*self.player, pause];
                if let Some(view) = &self.view {
                    let _: () = msg_send![&**view, removeFromSuperview];
                }
                if let Some((_, observer)) = &self.end
                    && let Some(class) = AnyClass::get(c"NSNotificationCenter")
                {
                    let center: Retained<AnyObject> = msg_send![class, defaultCenter];
                    let _: () = msg_send![&*center, removeObserver: &**observer];
                }
            }
        }
    }

    fn content_view(window: &Window) -> Option<*mut AnyObject> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
        Some(handle.ns_view.as_ptr() as *mut AnyObject)
    }

    /// How long the video at `video` is, in milliseconds. Blocks while AVFoundation reads the file: call it off the
    /// main thread.
    pub fn length_ms(video: &Path) -> Option<f64> {
        objc2::rc::autoreleasepool(|_| {
            let url = file_url(video)?;
            // SAFETY: an AVFoundation asset made and read on this one thread.
            unsafe {
                let asset: Option<Retained<AnyObject>> = msg_send![AnyClass::get(c"AVURLAsset")?, URLAssetWithURL: &*url, options: std::ptr::null::<AnyObject>()];
                let duration: CMTime = msg_send![&*asset?, duration];
                duration.as_seconds().map(|seconds| seconds * 1000.)
            }
        })
    }
}

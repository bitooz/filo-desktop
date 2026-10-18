// Pictures and videos opened inside the app (file_preview::open), like Telegram's viewer: over the whole window in the
// dialog layer the app shell draws, on a dark backdrop, the picture fitted to the window above a bar of controls
// (through the picture cache, like every remote picture; zoomed in, a copy as sharp as it's shown, and past its own
// pixels the part on screen drawn on the CPU at the screen's pixels, resample.rs), or the video playing there
// (video_player.rs) with its controls in the same bar: play and pause, the time and a scrubber, mute and full screen.
// Space (or a click on the video) plays and pauses it, M mutes it, F or a double-click puts the window in full screen
// and Esc takes it out again before it closes the viewer. A picture zooms with ⌘ and the wheel (about the pointer),
// ⌘= ⌘- ⌘0, the bar's buttons or a double-click, up to filling the window, and pans by dragging or scrolling once
// it's bigger than the window. Arrows and ← → step through the pictures and videos shown together (a message's); Esc,
// the close button or a click on the backdrop closes it. ⌘C or the bar's Copy button puts the picture itself on the clipboard (copy_pictures), and
// "Copied" shows once it's there. In a chat a picture can go back to Filo, so there its pictures also get "Draw on
// it" (the image editor). A file it can show
// (file_preview::preview_of) opens by itself in the same frame, as a page where the picture would be
// (file_view.rs); other files open in their own app.

mod file_view;
mod resample;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, CursorStyle, FocusHandle, ImageSource, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement as _, Pixels,
    Point, Render, RenderImage, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Task, Window, canvas, div, img, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::WindowExt as _;

use crate::file_preview::{self, Item};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, IconButton};
use crate::video_player::Player;

/// The band along the top kept clear: the window's title bar, where the traffic lights draw over everything, and the
/// close button.
pub(crate) const TOP: f32 = 56.;
/// The bar along the bottom with the name and the buttons.
pub(crate) const BAR: f32 = 72.;
/// The columns either side of the picture: the arrows', when there's more than one item. Kept clear of the video: its
/// native view draws over anything in its box.
const GUTTER: f32 = 72.;
/// The room either side when there's one item.
pub(crate) const EDGE: f32 = 24.;
/// How far the dialog layer slides down as it opens, and stays (gpui-component's dialog.rs, "slide-down").
pub(crate) const DIALOG_SLIDE: f32 = 30.;
/// One step of the zoom buttons and ⌘= ⌘-.
const ZOOM_STEP: f32 = 1.25;
/// How far a picture zooms out, against it fitted to the window.
const ZOOM_MIN: f32 = 0.25;
/// How far it zooms in: this many times its own size.
const ZOOM_MAX_NATURAL: f32 = 8.;
/// Past this many screen pixels to one of the picture's, the part on screen is drawn on the CPU (resample.rs).
const SHARPEN_PAST: f64 = 1.01;
/// How long "Copied" shows after the picture went on the clipboard.
pub(crate) const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

gpui::actions!(media_viewer, [ZoomIn, ZoomOut, ZoomToFit, CopyPicture]);

/// ⌘= (or ⌘+), ⌘- and ⌘0 zoom the shown picture, ⌘C copies it. Bound as actions: a ⌘ key no binding takes reaches the
/// view twice on macOS (as a key equivalent, then as a key down), so a key listener zoomed two steps a press. Only in
/// the viewer's context, so ⌘C copies text everywhere else.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("cmd-=", ZoomIn, Some("MediaViewer")),
        gpui::KeyBinding::new("cmd-+", ZoomIn, Some("MediaViewer")),
        gpui::KeyBinding::new("cmd--", ZoomOut, Some("MediaViewer")),
        gpui::KeyBinding::new("cmd-0", ZoomToFit, Some("MediaViewer")),
        gpui::KeyBinding::new("cmd-c", CopyPicture, Some("MediaViewer")),
    ]);
}

/// Opens a picture from the viewer in the image editor (a chat's, where the marked-up copy goes back to Filo).
pub type Draw = Rc<dyn Fn(Item, &mut Window, &mut App)>;

pub fn is_picture(item: &Item) -> bool {
    item.content_type.starts_with("image/")
}

pub fn is_video(item: &Item) -> bool {
    item.content_type.starts_with("video/")
}

/// Puts the pictures among `items` on the clipboard, the pictures themselves (PNG and their own format), not their
/// links: downloaded once (file_preview's cache), converted off the main thread. Gives how many once the pasteboard has
/// them all.
pub fn copy_pictures(items: Vec<Item>, cx: &mut App) -> Task<anyhow::Result<usize>> {
    let downloads: Vec<_> = items.iter().filter(|item| is_picture(item)).map(file_preview::download).collect();
    cx.spawn(async move |cx| {
        anyhow::ensure!(!downloads.is_empty(), "There's no picture to copy.");
        let paths = futures::future::try_join_all(downloads).await?;
        let pictures = cx
            .background_executor()
            .spawn(async move {
                paths
                    .iter()
                    .map(|path| crate::pasteboard::picture_for_clipboard(&std::fs::read(path)?))
                    .collect::<anyhow::Result<Vec<_>>>()
            })
            .await?;
        // The pasteboard is AppKit's: written on the main thread.
        let written = cx.update(|_| crate::pasteboard::copy_pictures(&pictures))?;
        anyhow::ensure!(written, "The clipboard didn't take it.");
        Ok(pictures.len())
    })
}

/// copy_pictures outside the viewer (a chat's picture or message): a toast says it worked once it has, or why not.
pub fn copy_pictures_and_tell(items: Vec<Item>, window: &mut Window, cx: &mut App) {
    let copying = copy_pictures(items, cx);
    window
        .spawn(cx, async move |cx| {
            let copied = copying.await;
            cx.update(|window, cx| match copied {
                Ok(1) => ui::toast(window, cx, "Image copied"),
                Ok(count) => ui::toast(window, cx, format!("{count} images copied")),
                Err(error) => ui::toast_error(window, cx, "Couldn’t copy", error.to_string()),
            })
            .ok();
        })
        .detach();
}

/// Shows `items` (pictures and videos) from `index` over the window.
pub fn open(items: Vec<Item>, index: usize, draw: Option<Draw>, window: &mut Window, cx: &mut App) {
    if items.is_empty() {
        return;
    }
    let index = index.min(items.len() - 1);
    let viewer = cx.new(|cx| {
        let mut viewer = MediaViewer {
            items,
            index,
            video: Video::None,
            file: None,
            draw,
            focus: cx.focus_handle(),
            focused: false,
            zoom: 1.,
            pan: (0., 0.),
            drag: None,
            bounds: Rc::default(),
            copy: Copying::Idle,
            sharpened: Sharpened::default(),
            _pinches: None,
            track: Rc::default(),
            scrub: None,
            full_screen: false,
        };
        cx.on_release(|viewer: &mut MediaViewer, cx| viewer.sharpened.release(cx)).detach();
        cx.on_release_in(window, |viewer: &mut MediaViewer, window, _| {
            if viewer.full_screen && window.is_fullscreen() {
                window.toggle_fullscreen();
            }
        })
        .detach();
        viewer.load(window, cx);
        viewer
    });
    #[cfg(feature = "dev")]
    SHOWN.with(|shown| *shown.borrow_mut() = Some(viewer.downgrade()));
    let pinches = pinch::listen();
    viewer.update(cx, |viewer, cx| {
        viewer._pinches = Some(cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            let mut pinches = pinches;
            while let Some(gesture) = pinches.next().await {
                let applied = this.update_in(cx, |this, window, cx| {
                    let anchor = |x: f32, y: f32| this.local(gpui::point(px(x), px(y)));
                    match gesture {
                        pinch::Gesture::By { magnification, x, y } => {
                            let anchor = anchor(x, y);
                            this.zoom_to(this.zoom * (1. + magnification), anchor, window, cx);
                        }
                        // Two fingers tapped twice: like a double-click on the picture.
                        pinch::Gesture::Smart { x, y } => {
                            let anchor = anchor(x, y);
                            this.toggle_zoom(anchor, window, cx);
                        }
                    }
                });
                if applied.is_err() {
                    break;
                }
            }
        }));
    });
    // The whole window: the viewer draws its own backdrop, so the dialog's box has no colour, border or corners.
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .w(window.viewport_size().width)
            .margin_top(px(-DIALOG_SLIDE))
            .close_button(false)
            .p_0()
            .border_0()
            .rounded(px(0.))
            .bg(gpui::transparent_black())
            .child(viewer.clone())
    });
}

#[cfg(feature = "dev")]
thread_local! {
    /// The viewer last opened, for test drives (`smoke_state`).
    static SHOWN: std::cell::RefCell<Option<gpui::WeakEntity<MediaViewer>>> = const { std::cell::RefCell::new(None) };
}

/// What the last viewer opened shows, for test drives: gone once it has closed; for a video, how it plays.
#[cfg(feature = "dev")]
pub(crate) fn smoke_state(window: &Window, cx: &App) -> String {
    let Some(viewer) = SHOWN.with(|shown| shown.borrow().as_ref().and_then(|viewer| viewer.upgrade())) else {
        return "closed".into();
    };
    let viewer = viewer.read(cx);
    let item = &viewer.items[viewer.index];
    let video = match &viewer.video {
        Video::None => String::new(),
        Video::Loading { .. } => " video=loading".into(),
        Video::Failed(error) => format!(" video=failed({error})"),
        Video::Playing { player, .. } => format!(
            " video=open playing={} time={:.2} duration={:?} muted={}",
            player.playing(),
            player.time(),
            player.duration().map(|duration| (duration * 100.).round() / 100.),
            player.muted()
        ),
    };
    let file = viewer.file.as_ref().map(|file| file.smoke_state(cx)).unwrap_or_default();
    format!(
        "{} of {}: {} ({}){video}{file} full_screen={}",
        viewer.index + 1,
        viewer.items.len(),
        item.name,
        item.content_type,
        window.is_fullscreen()
    )
}

/// Where things go, in points from the viewer's top left, for the shown item.
#[derive(Clone, Copy)]
struct Layout {
    /// The whole viewer: the window.
    area: (f32, f32),
    /// The room a picture fits into and a video plays in (between the top band, the bar and the side columns), its
    /// top left and size.
    origin: (f32, f32),
    room: (f32, f32),
    /// A picture fitted to the room, and that size against its own (at most 1: a small picture isn't blown up).
    fitted: Option<((f32, f32), f32)>,
    /// Pixels to a point.
    scale: f32,
}

impl Layout {
    fn middle(&self) -> (f32, f32) {
        (self.origin.0 + self.room.0 / 2., self.origin.1 + self.room.1 / 2.)
    }
}

/// The shown video: downloading (file_preview's cache, so it's fetched once), then playing.
enum Video {
    None,
    /// Held, so a download finishing after the viewer moved on doesn't take the screen.
    Loading {
        _download: Task<()>,
    },
    /// It stops and leaves the window when the viewer moves on or closes. The ticker draws the viewer again while it
    /// plays, for the time and the scrubber.
    Playing {
        player: Rc<Player>,
        _ticker: Task<()>,
    },
    Failed(SharedString),
}

/// Copying the shown picture to the clipboard.
enum Copying {
    Idle,
    /// Downloading it (file_preview's cache) and making the clipboard's PNG.
    Busy {
        _copy: Task<()>,
    },
    /// On the clipboard: "Copied" shows until the timer ends.
    Copied {
        _hide: Task<()>,
    },
}

pub struct MediaViewer {
    items: Vec<Item>,
    index: usize,
    video: Video,
    /// The shown file, when it's neither a picture nor a video.
    file: Option<file_view::FileView>,
    draw: Option<Draw>,
    focus: FocusHandle,
    focused: bool,
    /// The picture's zoom against it fitted to the room (1), and how far its middle sits off the room's, in points.
    zoom: f32,
    pan: (f32, f32),
    /// Where a drag to pan started: the pointer, and the pan then.
    drag: Option<(Point<Pixels>, (f32, f32))>,
    /// The viewer on screen, for zooming about the pointer.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    copy: Copying,
    sharpened: Sharpened,
    /// Takes the trackpad's pinches while the viewer is open (`pinch`).
    _pinches: Option<Task<()>>,
    /// The video's scrubber on screen, and a drag along it.
    track: Rc<Cell<Option<Bounds<Pixels>>>>,
    scrub: Option<Scrub>,
    /// The viewer put the window in full screen: Esc and closing the viewer take it out again.
    full_screen: bool,
}

/// A drag along the video's scrubber: where it is, in seconds, and whether the video plays again when it ends.
#[derive(Clone, Copy)]
struct Scrub {
    at: f64,
    resume: bool,
}

/// Zoomed in past the picture's own pixels, the part of it on screen drawn on the CPU at the screen's pixels
/// (resample.rs): GPUI's linear sampler left text soft. One is drawn at a time, in the background; until it's
/// ready the picture shows as before, and while panning the last one shows moved with it.
#[derive(Default)]
struct Sharpened {
    /// The part on screen as last drawn.
    shown: Option<(Region, Arc<RenderImage>)>,
    drawing: Option<Task<()>>,
    /// What the latest frame wants drawn.
    wanted: Option<Region>,
    /// Drawn parts no frame shows any more, dropped from the GPU after the next frame (one still showing them must
    /// find their textures).
    retired: Vec<Arc<RenderImage>>,
}

/// A part of the picture drawn on the CPU, in the window's screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Region {
    index: usize,
    /// The copy it's drawn from (pictures::sharp), by its size.
    source: (u32, u32),
    /// Screen pixels to one of the copy's, and where its top left is.
    scale: f64,
    origin: (f64, f64),
    /// The drawn part's top left and size.
    at: (i64, i64),
    size: (u32, u32),
}

impl Region {
    /// Drawn at the same size from the same copy: it shows moved where `other` would be.
    fn same_scale(&self, other: &Region) -> bool {
        self.index == other.index && self.source == other.source && (self.scale - other.scale).abs() < 1e-6
    }
}

impl Sharpened {
    fn retire(&mut self) {
        if let Some((_, image)) = self.shown.take() {
            self.retired.push(image);
        }
    }

    /// Everything still on the GPU, dropped once the viewer has left the screen.
    fn release(&mut self, cx: &mut App) {
        self.retire();
        self.drawing = None;
        let images = std::mem::take(&mut self.retired);
        if images.is_empty() {
            return;
        }
        // After the frame without the viewer has been drawn.
        cx.spawn(async move |cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
            cx.update(|cx| images.into_iter().for_each(|image| cx.drop_image(image, None))).ok();
        })
        .detach();
    }
}

impl MediaViewer {
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.items[self.index].clone();
        self.file = (!is_picture(&item) && !is_video(&item)).then(|| file_view::FileView::load(&item, window, cx));
        if !is_video(&item) {
            self.video = Video::None;
            return;
        }
        let saving = file_preview::download(&item);
        let download = cx.spawn(async move |this, cx| {
            let saved = saving.await;
            this.update(cx, |this, cx| {
                this.video = match saved.map(|path| Player::open(&path)) {
                    Ok(Some(player)) => {
                        // One sound at a time: a sound playing in a chat stops for the video.
                        crate::media::stop(cx);
                        Video::Playing { _ticker: Self::tick(player.clone(), cx), player }
                    }
                    Ok(None) => Video::Failed("This video can’t play here.".into()),
                    Err(error) => Video::Failed(format!("Couldn’t load the video: {error}").into()),
                };
                cx.notify();
            })
            .ok();
        });
        self.video = Video::Loading { _download: download };
    }

    /// Draws the viewer again four times a second while `player` plays (and once when it stops), and says when it
    /// couldn't.
    fn tick(player: Rc<Player>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut was = (false, None);
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let now = (player.playing(), player.duration());
                let failed = player.failed();
                let changed = now.0 || now != was || failed;
                was = now;
                let updated = this.update(cx, |this, cx| {
                    if failed {
                        this.video = Video::Failed("This video can’t play here.".into());
                    }
                    if changed {
                        cx.notify();
                    }
                });
                if updated.is_err() || failed {
                    break;
                }
            }
        })
    }

    fn player(&self) -> Option<&Rc<Player>> {
        match &self.video {
            Video::Playing { player, .. } => Some(player),
            _ => None,
        }
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = self.player() {
            player.toggle();
            cx.notify();
        }
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if let Some(player) = self.player() {
            player.set_muted(!player.muted());
            cx.notify();
        }
    }

    /// The window in or out of full screen (only for a video, which then plays as large as the screen allows).
    fn toggle_full_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.player().is_none() {
            return;
        }
        self.full_screen = !window.is_fullscreen();
        window.toggle_fullscreen();
        cx.notify();
    }

    /// Seconds into the video at `x` (in the window) along the scrubber.
    fn time_at(&self, x: Pixels) -> Option<f64> {
        let track = self.track.get()?;
        let duration = self.player()?.duration()?;
        let fraction = f32::from(x - track.origin.x) / f32::from(track.size.width).max(1.);
        Some(f64::from(fraction.clamp(0., 1.)) * duration)
    }

    /// A press on the scrubber: the video goes there at once, paused while the drag goes on.
    fn start_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let (Some(at), Some(player)) = (self.time_at(x), self.player()) else { return };
        let resume = player.playing();
        player.pause();
        player.seek(at, false);
        self.scrub = Some(Scrub { at, resume });
        cx.notify();
    }

    fn move_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let (Some(scrub), Some(at), Some(player)) = (self.scrub, self.time_at(x), self.player()) else { return };
        player.seek(at, false);
        self.scrub = Some(Scrub { at, ..scrub });
        cx.notify();
    }

    /// The drag let go: on the very frame it ended on, playing again if it was.
    fn end_scrub(&mut self, cx: &mut Context<Self>) {
        let Some(scrub) = self.scrub.take() else { return };
        if let Some(player) = self.player() {
            player.seek(scrub.at, true);
            if scrub.resume {
                player.play();
            }
        }
        cx.notify();
    }

    /// Puts the shown picture, not its link, on the clipboard; "Copied" shows only once the pasteboard has it.
    fn copy_picture(&mut self, cx: &mut Context<Self>) {
        let item = self.items[self.index].clone();
        if !is_picture(&item) || matches!(self.copy, Copying::Busy { .. }) {
            return;
        }
        let copying = copy_pictures(vec![item], cx);
        let copy = cx.spawn(async move |this, cx| {
            let copied = copying.await;
            this.update(cx, |this, cx| {
                match copied {
                    Ok(_) => {
                        let hide = cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(COPIED_FOR).await;
                            this.update(cx, |this, cx| {
                                this.copy = Copying::Idle;
                                cx.notify();
                            })
                            .ok();
                        });
                        this.copy = Copying::Copied { _hide: hide };
                    }
                    Err(error) => {
                        this.copy = Copying::Idle;
                        if let Some(window) = cx.active_window() {
                            window
                                .update(cx, |_, window, cx| {
                                    ui::toast_error(window, cx, "Couldn’t copy the picture", error.to_string())
                                })
                                .ok();
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.copy = Copying::Busy { _copy: copy };
        cx.notify();
    }

    fn step(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.index as isize + by;
        if next < 0 || next >= self.items.len() as isize {
            return;
        }
        self.index = next as usize;
        self.zoom = 1.;
        self.pan = (0., 0.);
        self.drag = None;
        self.scrub = None;
        self.load(window, cx);
        cx.notify();
    }

    fn layout(&self, window: &mut Window, cx: &mut App) -> Layout {
        let viewport = window.viewport_size();
        let area = (f32::from(viewport.width), f32::from(viewport.height));
        let side = if self.items.len() > 1 { GUTTER } else { EDGE };
        let room = ((area.0 - 2. * side).max(1.), (area.1 - TOP - BAR).max(1.));
        let item = &self.items[self.index];
        let scale = window.scale_factor();
        let fitted = is_picture(item)
            .then(|| crate::pictures::natural_size_of(&item.picture(), window, cx))
            .flatten()
            .map(|(width, height)| {
                let (width, height) = (width / scale, height / scale);
                let fit = (room.0 / width).min(room.1 / height).min(1.);
                ((width * fit, height * fit), fit)
            });
        Layout { area, origin: (side, TOP), room, fitted, scale }
    }

    /// `position` (in the window) in the viewer, if it has been drawn.
    fn local(&self, position: Point<Pixels>) -> Option<(f32, f32)> {
        let bounds = self.bounds.get()?;
        Some((f32::from(position.x - bounds.origin.x), f32::from(position.y - bounds.origin.y)))
    }

    /// The shown picture's size and middle, the pan kept so it never leaves a gap it could cover.
    fn placed(&self, layout: &Layout) -> Option<((f32, f32), (f32, f32))> {
        let ((width, height), _) = layout.fitted?;
        let shown = (width * self.zoom, height * self.zoom);
        let middle = layout.middle();
        let centre = (
            place(middle.0, middle.0 + self.pan.0, shown.0, layout.area.0),
            place(middle.1, middle.1 + self.pan.1, shown.1, layout.area.1),
        );
        Some((shown, centre))
    }

    /// Zooms the picture to `zoom`, keeping what's under `anchor` (in the viewer; the room's middle without one) in
    /// place.
    fn zoom_to(&mut self, zoom: f32, anchor: Option<(f32, f32)>, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window, cx);
        let Some((_, fit)) = layout.fitted else { return };
        let Some((_, centre)) = self.placed(&layout) else { return };
        let zoom = zoom.clamp(ZOOM_MIN, (ZOOM_MAX_NATURAL / fit).max(1.));
        let (x, y) = anchor.unwrap_or(layout.middle());
        // The picture's point under the anchor, from the picture's middle, scales with it.
        let by = zoom / self.zoom;
        let centre = (x - (x - centre.0) * by, y - (y - centre.1) * by);
        let middle = layout.middle();
        self.zoom = zoom;
        self.pan = (centre.0 - middle.0, centre.1 - middle.1);
        if let Some((_, centre)) = self.placed(&layout) {
            self.pan = (centre.0 - middle.0, centre.1 - middle.1);
        }
        cx.notify();
    }

    /// Fitted, the picture goes to its own size (twice that if it's shown at its own size); zoomed, back to fit.
    fn toggle_zoom(&mut self, anchor: Option<(f32, f32)>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, fit)) = self.layout(window, cx).fitted else { return };
        let zoom = if (self.zoom - 1.).abs() > 0.01 {
            1.
        } else if fit < 0.99 {
            1. / fit
        } else {
            2.
        };
        self.zoom_to(zoom, anchor, window, cx);
    }

    fn pan_by(&mut self, dx: f32, dy: f32, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout(window, cx);
        let middle = layout.middle();
        self.pan = (self.pan.0 + dx, self.pan.1 + dy);
        if let Some((_, centre)) = self.placed(&layout) {
            self.pan = (centre.0 - middle.0, centre.1 - middle.1);
        }
        cx.notify();
    }

    /// The picture, placed and zoomed, or the video in the room.
    fn render_media(
        &mut self,
        item: &Item,
        layout: &Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let note = move |text: SharedString| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .callout()
                .text_color(colors.on_media.opacity(0.7))
                .child(text)
        };
        let in_room = || {
            div()
                .absolute()
                .left(px(layout.origin.0))
                .top(px(layout.origin.1))
                .w(px(layout.room.0))
                .h(px(layout.room.1))
        };
        if is_video(item) || self.placed(layout).is_none() {
            // Nothing drawn on the CPU for a video, or a picture not loaded yet.
            self.sharpened.wanted = None;
            self.sharpened.retire();
        }
        if is_video(item) {
            let video = match &self.video {
                // A click on the video plays or pauses it, a double-click puts it in full screen; neither is one on
                // the backdrop. They come through its native view, which passes them on to the window.
                Video::Playing { player, .. } => div()
                    .id("media-viewer-video")
                    .size_full()
                    .flex()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if event.click_count == 2 {
                                // The first click of the two paused it.
                                this.toggle_play(cx);
                                this.toggle_full_screen(window, cx);
                            } else {
                                this.toggle_play(cx);
                            }
                        }),
                    )
                    .child(player.element())
                    .into_any_element(),
                Video::Failed(error) => note(error.clone()).into_any_element(),
                Video::Loading { .. } | Video::None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::spinner(28., cx))
                    .into_any_element(),
            };
            return in_room().child(video).into_any_element();
        }
        let Some(((width, height), (x, y))) = self.placed(layout) else {
            // Until the picture is in the cache its size isn't known: nothing yet, then the picture.
            return in_room()
                .child(
                    img(crate::pictures::source(gpui::ImageSource::Resource(item.picture())))
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .with_loading(|| div().size_full().into_any_element())
                        .with_fallback(move || note("Couldn’t load the picture.".into()).into_any_element()),
                )
                .into_any_element();
        };
        let pannable = width > layout.area.0 + 0.5 || height > layout.area.1 + 0.5;
        // Zoomed in, a copy with as many pixels as it's shown at: the cache keeps a picture 1600 pixels across, and a
        // long screenshot's text blurred.
        let edge = layout.fitted.map_or(0, |((fitted_width, fitted_height), fit)| {
            let natural = (fitted_width / fit * layout.scale, fitted_height / fit * layout.scale);
            crate::pictures::sharp_edge(natural, width.max(height) * layout.scale)
        });
        let (left, top) = (x - width / 2., y - height / 2.);
        let tiles = crate::pictures::sharp(&item.picture(), edge, window, cx);
        let sharpened = tiles.as_ref().and_then(|tiles| self.sharpen(tiles, (left, top, width, height), layout, cx));
        div()
            .id("media-viewer-picture")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .h(px(height))
            .when(pannable, |this| {
                this.cursor(if self.drag.is_some() { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            })
            .map(|this| match &tiles {
                // The picture's own box, placed here: GPUI's Contain drew a tall screenshot across the whole width
                // and cut it off. Only the tiles on screen.
                Some(tiles) => this.children((0..tiles.images.len()).filter_map(|index| {
                    let ((tile_x, tile_y), (tile_width, tile_height)) = tiles.bounds(index);
                    let along = |at: u32, of: u32, size: f32| at as f32 / of.max(1) as f32 * size;
                    let (tile_left, tile_top) =
                        (along(tile_x, tiles.size.0, width), along(tile_y, tiles.size.1, height));
                    let (tile_w, tile_h) =
                        (along(tile_width, tiles.size.0, width), along(tile_height, tiles.size.1, height));
                    let on_screen = left + tile_left < layout.area.0
                        && left + tile_left + tile_w > 0.
                        && top + tile_top < layout.area.1
                        && top + tile_top + tile_h > 0.;
                    on_screen.then(|| {
                        div().absolute().left(px(tile_left)).top(px(tile_top)).w(px(tile_w)).h(px(tile_h)).child(
                            img(ImageSource::Render(tiles.images[index].clone()))
                                .size_full()
                                .object_fit(ObjectFit::Fill),
                        )
                    })
                })),
                None => this.child(
                    img(crate::pictures::source(ImageSource::Resource(item.picture())))
                        .size_full()
                        .object_fit(ObjectFit::Fill)
                        // Shown in the chat already, so it's usually in the picture cache.
                        .with_loading(|| div().size_full().into_any_element())
                        .with_fallback(move || note("Couldn’t load the picture.".into()).into_any_element()),
                ),
            })
            .children(sharpened)
            // A click on the picture isn't one on the backdrop, which closes the viewer.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if event.click_count == 2 {
                        let anchor = this.local(event.position);
                        this.toggle_zoom(anchor, window, cx);
                    } else {
                        this.drag = Some((event.position, this.pan));
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    /// The part of the picture on screen drawn on the CPU, when it's shown past its own pixels (`Sharpened`): the last
    /// one drawn at this size, placed in the picture's box (`placed`: left, top, width, height), and the next one
    /// started when it isn't this frame's.
    fn sharpen(
        &mut self,
        tiles: &crate::pictures::Tiles,
        (left, top, width, _): (f32, f32, f32, f32),
        layout: &Layout,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let pixels = layout.scale as f64;
        let viewer = self
            .bounds
            .get()
            .map(|bounds| (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y))));
        let scale = width as f64 * pixels / tiles.size.0.max(1) as f64;
        let wanted = viewer.filter(|_| scale > SHARPEN_PAST).and_then(|viewer| {
            let origin = ((viewer.0 + left as f64) * pixels, (viewer.1 + top as f64) * pixels);
            let end = (origin.0 + tiles.size.0 as f64 * scale, origin.1 + tiles.size.1 as f64 * scale);
            let area = (
                viewer.0 * pixels,
                viewer.1 * pixels,
                (viewer.0 + layout.area.0 as f64) * pixels,
                (viewer.1 + layout.area.1 as f64) * pixels,
            );
            let at = (origin.0.max(area.0).floor() as i64, origin.1.max(area.1).floor() as i64);
            let to = (end.0.min(area.2).ceil() as i64, end.1.min(area.3).ceil() as i64);
            (to.0 > at.0 && to.1 > at.1).then(|| Region {
                index: self.index,
                source: tiles.size,
                scale,
                origin,
                at,
                size: ((to.0 - at.0) as u32, (to.1 - at.1) as u32),
            })
        });
        self.sharpened.wanted = wanted;
        let Some(wanted) = wanted else {
            self.sharpened.retire();
            return None;
        };
        if self.sharpened.shown.as_ref().is_some_and(|(shown, _)| !shown.same_scale(&wanted)) {
            self.sharpened.retire();
        }
        let current = self.sharpened.shown.as_ref().is_some_and(|(shown, _)| *shown == wanted);
        // One at a time: the next starts from where the picture is when this one is done.
        if !current && self.sharpened.drawing.is_none() {
            self.draw_sharpened(tiles.clone(), wanted, cx);
        }
        let (shown, image) = self.sharpened.shown.as_ref()?;
        // Where it goes now the picture may have moved, in the picture's box.
        let place = |at: i64, then: f64, now: f64| ((at as f64 - then + now) / pixels) as f32;
        let (x, y) =
            (place(shown.at.0, shown.origin.0, wanted.origin.0), place(shown.at.1, shown.origin.1, wanted.origin.1));
        let viewer = viewer.unwrap_or_default();
        Some(
            div()
                .absolute()
                .left(px(x - viewer.0 as f32 - left))
                .top(px(y - viewer.1 as f32 - top))
                .w(px((shown.size.0 as f64 / pixels) as f32))
                .h(px((shown.size.1 as f64 / pixels) as f32))
                .child(img(ImageSource::Render(image.clone())).size_full().object_fit(ObjectFit::Fill))
                .into_any_element(),
        )
    }

    /// Draws `region` of the picture in the background; once it's done, it shows if it's still at the wanted size,
    /// and the next one starts if the picture moved meanwhile.
    fn draw_sharpened(&mut self, tiles: crate::pictures::Tiles, region: Region, cx: &mut Context<Self>) {
        let frame = resample::Frame {
            size: tiles.size,
            scale: region.scale,
            origin: (region.origin.0 - region.at.0 as f64, region.origin.1 - region.at.1 as f64),
            out: region.size,
        };
        let drawing = cx.background_executor().spawn(async move { resample::render(&tiles, frame).map(Arc::new) });
        let task = cx.spawn(async move |this, cx| {
            let image = drawing.await;
            this.update(cx, |this, cx| {
                let sharpened = &mut this.sharpened;
                sharpened.drawing = None;
                let Some(image) = image else { return };
                if sharpened.wanted.is_some_and(|wanted| wanted.same_scale(&region)) {
                    sharpened.retire();
                    sharpened.shown = Some((region, image));
                } else {
                    sharpened.retired.push(image);
                }
                cx.notify();
            })
            .ok();
        });
        self.sharpened.drawing = Some(task);
    }

    /// A video's controls in the bar: play or pause, where it is, the scrubber, how long it is, mute and full screen.
    /// No tooltips, as for every button over a video.
    fn render_controls(&self, player: &Rc<Player>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let duration = player.duration();
        let at = self.scrub.map_or_else(|| player.time(), |scrub| scrub.at);
        let fraction = duration.map_or(0., |duration| (at / duration).clamp(0., 1.)) as f32;
        let playing = player.playing() || self.scrub.is_some_and(|scrub| scrub.resume);
        let clock = |seconds: f64| {
            div()
                .flex_none()
                .footnote()
                .text_color(colors.on_media.opacity(0.8))
                .child(crate::format::duration_clock(seconds * 1000.))
        };
        let track = self.track.clone();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                IconButton::new("media-viewer-play", if playing { "pause" } else { "play" })
                    .color(colors.on_media)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx))),
            )
            .child(clock(at.floor()))
            .child(
                // A press anywhere along it goes there; a drag goes on over the whole viewer (its mouse move).
                div()
                    .id("media-viewer-scrubber")
                    .flex_1()
                    .min_w(px(80.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.start_scrub(event.position.x, cx);
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .rounded(px(radius::FULL))
                            .bg(colors.on_media.opacity(0.25))
                            .child(
                                canvas(move |bounds, _, _| track.set(Some(bounds)), |_, _, _, _| {})
                                    .absolute()
                                    .inset_0(),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .bottom_0()
                                    .w(relative(fraction))
                                    .rounded(px(radius::FULL))
                                    .bg(colors.on_media),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top(px(-4.))
                                    .left(relative(fraction))
                                    .ml(px(-6.))
                                    .size(px(12.))
                                    .rounded(px(radius::FULL))
                                    .bg(colors.on_media),
                            ),
                    ),
            )
            .child(clock(duration.unwrap_or(0.)))
            .child(
                IconButton::new("media-viewer-mute", if player.muted() { "volume-x" } else { "volume-2" })
                    .color(colors.on_media)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
            )
            .child(
                IconButton::new(
                    "media-viewer-full-screen",
                    if window.is_fullscreen() { "minimize" } else { "maximize" },
                )
                .color(colors.on_media)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_full_screen(window, cx))),
            )
            .into_any_element()
    }

    /// The bar along the bottom: the name and where it is in the set, a video's controls, then the buttons.
    fn render_bar(&self, item: &Item, layout: &Layout, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let video = is_video(item);
        // No tooltips over a video: one would open into its box, which draws above it.
        let button = |id: &'static str, icon: &'static str, tip: &'static str| {
            let button = IconButton::new(id, icon).color(colors.on_media);
            if video { button } else { button.tooltip(tip) }
        };
        let (count, index) = (self.items.len(), self.index);
        let kind = if video { "Video" } else { "Picture" };
        let place = match &self.file {
            // A file is shown by itself: what it is and how big.
            Some(file) => file.place(),
            None if count > 1 => format!("{kind} {} of {count}", index + 1),
            None => kind.to_string(),
        };
        let zoom = layout.fitted.map(|(_, fit)| ((fit * self.zoom * 100.).round(), self.zoom));
        let draw = self.draw.clone().filter(|_| is_picture(item));
        let (copying, copied) =
            (matches!(self.copy, Copying::Busy { .. }), matches!(self.copy, Copying::Copied { .. }));
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(space::XS))
            // Clicks on the buttons aren't ones on the backdrop.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when_some(zoom, |this, (percent, zoom)| {
                this.child(
                    button("media-viewer-zoom-out", "minus", "Zoom out (⌘-)")
                        .disabled(zoom <= ZOOM_MIN + 0.001)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.zoom_to(this.zoom / ZOOM_STEP, None, window, cx)),
                        ),
                )
                .child(
                    // How big it's shown against its own size; a click fits it again.
                    div()
                        .id("media-viewer-zoom-fit")
                        .min_w(px(52.))
                        .py(px(space::XS))
                        .rounded(px(radius::SM))
                        .cursor_pointer()
                        .hover(move |style| style.bg(colors.on_media.opacity(0.12)))
                        .footnote()
                        .text_center()
                        .text_color(colors.on_media.opacity(0.8))
                        .child(format!("{percent}%"))
                        .on_click(cx.listener(|this, _, window, cx| this.zoom_to(1., None, window, cx))),
                )
                .child(
                    button("media-viewer-zoom-in", "plus", "Zoom in (⌘+)").on_click(
                        cx.listener(|this, _, window, cx| this.zoom_to(this.zoom * ZOOM_STEP, None, window, cx)),
                    ),
                )
                .child(div().w(px(space::MD)))
            })
            .when(is_picture(item), |this| {
                this.child(
                    button(
                        "media-viewer-copy",
                        if copied { "check" } else { "copy" },
                        if copied { "Copied" } else { "Copy (⌘C)" },
                    )
                    .spinning(copying)
                    .on_click(cx.listener(|this, _, _, cx| this.copy_picture(cx))),
                )
            })
            .when_some(draw, |this, draw| {
                let item = item.clone();
                this.child(button("media-viewer-draw", "pencil", "Draw on it").on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    draw(item.clone(), window, cx);
                }))
            })
            .child({
                let item = item.clone();
                button("media-viewer-save", "download", "Save to Downloads")
                    .on_click(move |_, _, cx| file_preview::save_to_downloads(item.clone(), cx))
            })
            .child({
                let item = item.clone();
                if self.file.is_some() {
                    button("media-viewer-open", "external-link", "Open in its app").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        file_preview::open_in_app(item.clone(), cx);
                    })
                } else {
                    button("media-viewer-quick-look", "eye", "Open in Quick Look").on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        file_preview::quick_look(item.clone(), cx);
                    })
                }
            });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(BAR))
            .px(px(space::XL))
            .flex()
            .items_center()
            .gap(px(space::LG))
            // Over a zoomed picture the bar keeps a dark ground to read on.
            .bg(gpui::linear_gradient(
                180.,
                gpui::linear_color_stop(colors.media.opacity(0.), 0.),
                gpui::linear_color_stop(colors.media.opacity(0.75), 0.6),
            ))
            .child(
                div()
                    // Beside a video's controls the name takes less of the bar.
                    .map(|this| if video { this.flex_shrink().max_w(px(220.)) } else { this.flex_1() })
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .callout()
                            .semibold()
                            .text_color(colors.on_media)
                            .truncate()
                            .child(SharedString::from(item.name.clone())),
                    )
                    .child(div().footnote().text_color(colors.on_media.opacity(0.6)).child(place)),
            )
            .children(self.player().filter(|_| video).map(|player| self.render_controls(player, window, cx)))
            .child(buttons)
            .into_any_element()
    }
}

/// A middle for something `size` long on an `area` long axis: in the `wanted` place if it covers the area edge to edge
/// from there, else as near it as keeps it covering; something shorter stays at `home`, moved only to stay whole.
fn place(home: f32, wanted: f32, size: f32, area: f32) -> f32 {
    let (low, high) = (size / 2., area - size / 2.);
    if size <= area { home.clamp(low, high) } else { wanted.clamp(high, low) }
}

impl Render for MediaViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Arrows step from the start; Esc reaches the dialog around it from here too.
        if !self.focused {
            self.focused = true;
            self.focus.focus(window);
        }
        if self.full_screen && window.is_fullscreen() {
            crate::escape::dialog_layer(window, cx, |this, window, cx| this.toggle_full_screen(window, cx));
        }
        let colors = *cx.colors();
        let item = self.items[self.index].clone();
        let layout = self.layout(window, cx);
        let media = match &self.file {
            Some(file) => file.render(&item, &layout, window, cx),
            None => self.render_media(&item, &layout, window, cx),
        };
        // Parts drawn on the CPU this frame no longer shows: gone from the GPU once it has been drawn.
        let retired = std::mem::take(&mut self.sharpened.retired);
        if !retired.is_empty() {
            window.on_next_frame(move |window, _| retired.into_iter().for_each(|image| _ = window.drop_image(image)));
        }
        let (count, index) = (self.items.len(), self.index);
        // No tooltips: one would open over the video, which draws above it.
        let arrow = |id: &'static str, icon: &'static str, by: isize, enabled: bool, cx: &mut Context<Self>| {
            div()
                .absolute()
                .top(px(layout.origin.1))
                .w(px(GUTTER))
                .h(px(layout.room.1))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .rounded(px(radius::FULL))
                        .bg(colors.media.opacity(0.5))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            IconButton::new(id, icon)
                                .color(colors.on_media)
                                .disabled(!enabled)
                                .on_click(cx.listener(move |this, _, window, cx| this.step(by, window, cx))),
                        ),
                )
        };
        let bounds = self.bounds.clone();
        div()
            .id("media-viewer")
            .track_focus(&self.focus)
            .key_context("MediaViewer")
            .on_action(
                cx.listener(|this, _: &ZoomIn, window, cx| this.zoom_to(this.zoom * ZOOM_STEP, None, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ZoomOut, window, cx| this.zoom_to(this.zoom / ZOOM_STEP, None, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ZoomToFit, window, cx| this.zoom_to(1., None, window, cx)))
            .on_action(cx.listener(|this, _: &CopyPicture, _, cx| this.copy_picture(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "left" => this.step(-1, window, cx),
                    "right" => this.step(1, window, cx),
                    "space" => this.toggle_play(cx),
                    "m" => this.toggle_mute(cx),
                    "f" => this.toggle_full_screen(window, cx),
                    _ => {}
                }
            }))
            .relative()
            .w(px(layout.area.0))
            .h(px(layout.area.1))
            .overflow_hidden()
            .bg(colors.media.opacity(0.92))
            // A click on the backdrop closes the viewer, like Telegram's; the picture and the buttons keep theirs.
            .on_mouse_down(MouseButton::Left, |_, window, cx| window.close_dialog(cx))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                // A file's page scrolls by itself.
                if this.file.is_some() {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height());
                let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                if event.modifiers.platform || event.modifiers.control {
                    let anchor = this.local(event.position);
                    this.zoom_to(this.zoom * (dy * 0.01).exp(), anchor, window, cx);
                } else {
                    this.pan_by(dx, dy, window, cx);
                }
            }))
            // The drag goes on over the bar and the backdrop.
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.scrub.is_some() {
                    return if event.pressed_button == Some(MouseButton::Left) {
                        this.move_scrub(event.position.x, cx)
                    } else {
                        this.end_scrub(cx)
                    };
                }
                let Some((start, from)) = this.drag else { return };
                if event.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    return cx.notify();
                }
                this.pan = from;
                this.pan_by(f32::from(event.position.x - start.x), f32::from(event.position.y - start.y), window, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    this.end_scrub(cx);
                    cx.notify();
                }),
            )
            .child(
                canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(media)
            .child(self.render_bar(&item, &layout, window, cx))
            // "Copied", over the bottom of the picture, for the button and ⌘C alike.
            .when(matches!(self.copy, Copying::Copied { .. }), |this| {
                this.child(
                    div().absolute().left_0().right_0().bottom(px(BAR + space::SM)).flex().justify_center().child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::XS))
                            .px(px(space::MD))
                            .py(px(space::XS))
                            .rounded(px(radius::FULL))
                            .bg(colors.media.opacity(0.75))
                            .child(ui::icon_sized("check", 14., colors.on_media))
                            .child(div().footnote().semibold().text_color(colors.on_media).child("Copied")),
                    ),
                )
            })
            .when(count > 1, |this| {
                this.child(arrow("media-viewer-previous", "chevron-left", -1, index > 0, cx).left_0())
                    .child(arrow("media-viewer-next", "chevron-right", 1, index + 1 < count, cx).right_0())
            })
            .child(
                div()
                    .absolute()
                    .top(px(space::MD))
                    .right(px(space::MD))
                    .rounded(px(radius::FULL))
                    .bg(colors.media.opacity(0.5))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        IconButton::new("media-viewer-close", "x")
                            .color(colors.on_media)
                            .tooltip("Close (Esc)")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    }
}

/// The trackpad's pinch, which GPUI doesn't pass on: AppKit sends `magnifyWithEvent:` (and `smartMagnifyWithEvent:`
/// for a two-finger double tap) to GPUI's view, which has no such method, so they're added to its class. While a
/// viewer listens they go to it; otherwise on up the responder chain, as before.
#[allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare
mod pinch {
    use std::cell::RefCell;

    use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

    #[derive(Clone, Copy)]
    pub enum Gesture {
        /// Zoom by `1 + magnification` about (x, y), in points from the window's top left.
        By {
            magnification: f32,
            x: f32,
            y: f32,
        },
        Smart {
            x: f32,
            y: f32,
        },
    }

    thread_local! {
        /// The open viewer's (main thread only, like AppKit's events).
        static LISTENER: RefCell<Option<UnboundedSender<Gesture>>> = const { RefCell::new(None) };
    }

    /// Pinches from now on, for the viewer opening; the one before stops getting them.
    pub fn listen() -> UnboundedReceiver<Gesture> {
        install();
        let (sender, receiver) = unbounded();
        LISTENER.with(|listener| *listener.borrow_mut() = Some(sender));
        receiver
    }

    /// Hands `gesture` to the viewer, if one listens.
    fn deliver(gesture: Gesture) -> bool {
        LISTENER.with(|listener| {
            let mut listener = listener.borrow_mut();
            let sent = listener.as_ref().is_some_and(|sender| sender.unbounded_send(gesture).is_ok());
            if !sent {
                // Its viewer closed.
                *listener = None;
            }
            sent
        })
    }

    fn install() {
        use std::ffi::CString;
        use std::sync::Once;

        use objc::runtime::{Class, Imp, Object, Sel, class_addMethod};
        use objc::{msg_send, sel, sel_impl};

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct NSPoint {
            x: f64,
            y: f64,
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct NSRect {
            origin: NSPoint,
            size: NSPoint,
        }

        /// The event's place in the view, from its top left (the view isn't flipped).
        unsafe fn place(view: &Object, event: *mut Object) -> (f32, f32) {
            unsafe {
                let point: NSPoint = msg_send![event, locationInWindow];
                let point: NSPoint = msg_send![view, convertPoint: point fromView: std::ptr::null_mut::<Object>()];
                let bounds: NSRect = msg_send![view, bounds];
                (point.x as f32, (bounds.size.y - point.y) as f32)
            }
        }

        extern "C" fn magnify(view: &Object, _: Sel, event: *mut Object) {
            unsafe {
                let magnification: f64 = msg_send![event, magnification];
                let (x, y) = place(view, event);
                if !deliver(Gesture::By { magnification: magnification as f32, x, y }) {
                    let _: () = msg_send![super(view, objc::class!(NSView)), magnifyWithEvent: event];
                }
            }
        }

        extern "C" fn smart_magnify(view: &Object, _: Sel, event: *mut Object) {
            unsafe {
                let (x, y) = place(view, event);
                if !deliver(Gesture::Smart { x, y }) {
                    let _: () = msg_send![super(view, objc::class!(NSView)), smartMagnifyWithEvent: event];
                }
            }
        }

        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // GPUI declares its view class with its first window, which is open by the time anything opens.
            let Some(class) = Class::get("GPUIView") else { return };
            let types = CString::new("v@:@").expect("types");
            // SAFETY: adding methods with matching signatures to a live class, on the main thread; class_addMethod
            // leaves one the class already has.
            unsafe {
                let class = class as *const Class as *mut Class;
                let magnify: Imp = std::mem::transmute(magnify as extern "C" fn(&Object, Sel, *mut Object));
                let smart: Imp = std::mem::transmute(smart_magnify as extern "C" fn(&Object, Sel, *mut Object));
                class_addMethod(class, sel!(magnifyWithEvent:), magnify, types.as_ptr());
                class_addMethod(class, sel!(smartMagnifyWithEvent:), smart, types.as_ptr());
            }
        });
    }
}

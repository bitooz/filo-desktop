// The desktop pet: the main agent's picture (or the character picked in Settings › Desktop pet) living on the desktop,
// above every app (like the Codex app's pets). It takes a pose for what the agents are doing (companion.rs: replying or
// working at its laptop, waving when a reminder comes due, a heart when a reply lands, asleep late at night), glances
// at the pointer, and says things in a speech bubble: replies nobody has seen while Filo is in the background,
// reminders as they come due (Done, Snooze), "Listening…" while the open shortcut is held, and on hover what Filo is
// up to. A click opens Filo (the newest unseen chat, if any), a drag moves it (where it's left is kept), a right-click
// opens the menu bar's menu.
//
// The window is a borderless, non-activating panel (GPUI's PopUp), so using the pet never takes the focus from the
// app in front. It always has room for a bubble (above the pet, or below it near the top of the screen; toward the
// middle of the screen) and for the tray of chats Filo is working in (under the pet, or above it near the bottom of
// the screen), and never resizes: resizing under the pointer made the hover come and go, and the pet shook. Clicks go
// through it everywhere but the pet, its bubble and its tray, by watching the pointer (which also gives the hover and
// the glances; AppKit's mouse-move monitors also point clicks the right way as the pointer moves, so a quick click
// doesn't wait for the next look). It moves through AppKit outside GPUI's event handling: AppKit reports frame changes back
// into GPUI at once.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, Bounds, BoxShadow, Context, Entity, InteractiveElement as _,
    IntoElement, MouseButton, ObjectFit, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, canvas, div, img, point, prelude::FluentBuilder as _, px,
    size,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::avatars::Look;
use crate::companion::{self, Command, Companion, CompanionEvent, Glance, Mood};

#[cfg(feature = "dev")]
mod drive;
use crate::screens::chat::working_panel::{one_line, two_lines, up_to_lines};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};

/// The pet's square, in points.
const PET: f64 = 140.;
/// The picture inside it, with room to hop and a badge.
const PICTURE: f32 = 116.;
/// Under the picture, room for the pet's shadow.
const PICTURE_BOTTOM: f32 = 6.;
/// How far up its picture a pose's feet are, as a share of its height, until it's measured (or for a picture of the
/// person's own): about the animals' average.
const FEET_RISE: f32 = 0.08;
/// The shadow under the feet: its size, and how far it blurs out.
const SHADOW_W: f32 = PICTURE * 0.5;
const SHADOW_H: f32 = 6.;
const SHADOW_BLUR: f32 = 6.;
/// The bubble's area beside the pet.
const BUBBLE_W: f64 = 300.;
const BUBBLE_H: f64 = 172.;
/// Room around the bubble for its shadow.
const BUBBLE_MARGIN: f32 = 10.;
/// A reply's bubble shows this many lines of it at most.
const REPLY_LINES: usize = 3;
/// The tray of what Filo and the agents are working on: its width, a row's height (two lines; one waiting on the user
/// has its buttons under them; the "N more" row is one line), the rows it lists before "N more", and the room kept
/// for it beside the pet (every row waiting on the user, the "more" row, padding and the gap to the pet).
const TRAY_W: f32 = 256.;
const TRAY_ROW_H: f32 = 44.;
const TRAY_ASK_H: f32 = 74.;
const TRAY_MORE_H: f32 = 26.;
/// Between rows, so two highlighted ones don't run together.
const TRAY_GAP: f32 = 2.;
/// The tray's header (what's at work, and the control that folds it into a pill), and where its first row starts:
/// past the border, the padding, the header and a gap.
const TRAY_HEAD_H: f32 = 22.;
const TRAY_TOP: f32 = 1. + space::XS + TRAY_HEAD_H + TRAY_GAP;
/// The folded tray's pictures, and how many it stacks.
const PILL_FACE: f32 = 20.;
const PILL_FACES: usize = 3;
const TRAY_ROWS: usize = 3;
/// A row's picture, and the ring around it (a task waiting on the user, or how it finished).
const TRAY_FACE: f32 = 24.;
const TRAY_RING: f32 = 32.;
/// A row's text, beside its picture: the tray less its border and padding, the row's padding, the picture and the gap.
/// The text is set to this width (less the time's, on the first line) and cut with "…" there, rather than left to the
/// flex row, which can lay unwrapped text out at its whole width and clip it mid-letter under the time.
const TRAY_TEXT_W: f32 = TRAY_W - 2. - space::XS * 2. - space::SM * 2. - TRAY_RING - space::SM;
/// A row waiting on the user has its title in up to two lines; the room kept for the second.
const TRAY_WRAP_ROOM: f32 = 20.;
const TRAY_H: f64 = (TRAY_TOP
    + (TRAY_ASK_H + TRAY_WRAP_ROOM) * TRAY_ROWS as f32
    + TRAY_MORE_H
    + TRAY_GAP * TRAY_ROWS as f32
    + space::XS
    + 1.
    + 4.) as f64;
/// Room beside the pet, on the side away from the bubble, for the tray (and its shadow) centred under the pet.
const TRAY_SIDE: f64 = (TRAY_W / 2. + BUBBLE_MARGIN) as f64 - PET / 2.;
/// The window's width: the bubble's area and the tray's room beside the pet.
const WINDOW_W: f64 = BUBBLE_W + TRAY_SIDE;
/// Above the pet, the tray comes down this far into the pet's square, to the top of its head.
const TRAY_TUCK: f32 = 12.;
/// Where the pet starts: this far in from the bottom-right corner of the screen.
const START_INSET: f64 = 28.;
/// A press that travels this far is a drag, not a click.
const DRAG_SLOP: f64 = 4.;
/// A reply's bubble stays this long (longer while the pointer is on the pet).
const REPLY_FOR: Duration = Duration::from_secs(14);
/// Resting on the pet this long shows what Filo is up to.
const HOVER_DELAY: Duration = Duration::from_millis(450);
/// How often the pet checks where the pointer is: often while it's near (clicks, hover), less when it's far.
const POINTER_NEAR: Duration = Duration::from_millis(40);
const POINTER_FAR: Duration = Duration::from_millis(150);
const NEAR: f64 = 160.;
/// The pet looks the new way only once the pointer has stayed on that side this long.
const GLANCE_SETTLE: Duration = Duration::from_millis(500);
/// The pointer this close (horizontally, points) catches the pet's eye; too close is "ahead".
const GLANCE_RANGE: f64 = 520.;
const GLANCE_DEAD_ZONE: f64 = 48.;
/// A resting pet breathes this often.
const BREATH_EVERY: Duration = Duration::from_secs(9);

pub struct PetWindow(pub WindowHandle<Pet>);

impl gpui::Global for PetWindow {}

/// Whether the pet says replies nobody has seen: it's out and Filo is in the background (companion.rs). Then the
/// in-app card and the macOS banner stay quiet, so a reply doesn't arrive twice.
pub fn speaks_replies(cx: &App) -> bool {
    cx.has_global::<PetWindow>() && !companion::app_active()
}

/// The pose the desktop pet shows (`companion::mood_key`), while it's out (smoke.rs).
#[cfg(feature = "dev")]
pub fn showing(cx: &App) -> Option<String> {
    cx.try_global::<PetWindow>().and_then(|pet| pet.0.read(cx).ok()).and_then(|pet| pet.pose.clone())
}

/// Opens the pet when it's on and the companion is up, and closes it otherwise.
pub fn sync(cx: &mut App) {
    let wanted = companion::settings(cx).pet && companion::companion(cx).is_some();
    let open = cx.try_global::<PetWindow>().map(|pet| pet.0);
    match (wanted, open) {
        (true, None) => {
            let Some(companion) = companion::companion(cx) else { return };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(gpui::Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(WINDOW_W as f32), px((PET + BUBBLE_H) as f32)),
                ))),
                titlebar: None,
                focus: false,
                // Shown once it's in place (Pet::new).
                show: false,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            };
            match cx.open_window(options, |window, cx| cx.new(|cx| Pet::new(companion, window, cx))) {
                Ok(handle) => cx.set_global(PetWindow(handle)),
                Err(error) => eprintln!("pet: {error:#}"),
            }
        }
        (false, Some(handle)) => {
            cx.remove_global::<PetWindow>();
            handle.update(cx, |_, window, _| window.remove_window()).ok();
        }
        _ => {}
    }
}

/// The end of what's been heard, as much as two lines of the bubble hold: the newest words show.
fn last_words(words: &str) -> String {
    const KEEP: usize = 70;
    let words = words.trim();
    let count = words.chars().count();
    if count <= KEEP {
        return words.to_string();
    }
    let tail: String = words.chars().skip(count - KEEP).collect();
    // From a word's start.
    let tail = tail.split_once(' ').map_or(tail.as_str(), |(_, rest)| rest);
    format!("…{tail}")
}

/// What the bubble says.
enum Bubble {
    Listening,
    Due(companion::Reminder),
    Reply(companion::Reply),
    Status,
}

/// Where things sit in the window, in AppKit screen points (y goes up).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// The pet's bottom-left corner.
    origin: (f64, f64),
    /// The bubble reaches left from the pet (it's on the right half of the screen).
    toward_left: bool,
    /// The bubble goes under the pet (no room above).
    below: bool,
    /// The tray goes under the pet; near the bottom of the screen it goes above, between the pet and the bubble.
    tray_below: bool,
    /// The tray's left edge in the window: centred on the pet, moved in only as far as it takes to stay on the
    /// screen at its edge.
    tray_left: f32,
    /// The window's frame: the pet and room for a bubble and the tray.
    frame: (f64, f64, f64, f64),
}

impl Layout {
    fn new(origin: (f64, f64), screen: NSRect) -> Layout {
        let (x, y) = origin;
        let screen_mid = screen.origin.x + screen.size.width / 2.;
        let toward_left = x + PET / 2. > screen_mid;
        let tray_below = y - TRAY_H >= screen.origin.y;
        // With the tray above, the bubble goes above it.
        let below = tray_below && y + PET + BUBBLE_H > screen.origin.y + screen.size.height;
        let left = if toward_left { x + PET - BUBBLE_W } else { x - TRAY_SIDE };
        let bottom = match (tray_below, below) {
            (true, false) => y - TRAY_H,
            (true, true) => y - TRAY_H - BUBBLE_H,
            (false, _) => y,
        };
        let tray_w = f64::from(TRAY_W);
        let screen_right = screen.origin.x + screen.size.width;
        let tray_x = (x + PET / 2. - tray_w / 2.).min(screen_right - tray_w).max(screen.origin.x);
        let tray_left = ((tray_x - left) as f32).clamp(BUBBLE_MARGIN, WINDOW_W as f32 - BUBBLE_MARGIN - TRAY_W);
        Layout {
            origin,
            toward_left,
            below,
            tray_below,
            tray_left,
            frame: (left, bottom, WINDOW_W, PET + BUBBLE_H + TRAY_H),
        }
    }

    /// The pet's top-left inside the window, in GPUI's coordinates (y goes down).
    fn pet_offset(&self) -> (f32, f32) {
        let left = self.origin.0 - self.frame.0;
        let top = self.frame.1 + self.frame.3 - self.origin.1 - PET;
        (left as f32, top as f32)
    }

    /// The left edge of the bubble's area in the window: the pet's edge on the side it opens to.
    fn bubble_left(&self) -> f32 {
        if self.toward_left { 0. } else { TRAY_SIDE as f32 }
    }

    fn over_pet(&self, point: NSPoint) -> bool {
        inside(point, (self.origin.0, self.origin.1, PET, PET))
    }

    /// A point on the screen in the window (GPUI's coordinates).
    fn to_window(&self, point: NSPoint) -> (f32, f32) {
        ((point.x - self.frame.0) as f32, (self.frame.1 + self.frame.3 - point.y) as f32)
    }

    /// A rectangle in the window (GPUI's coordinates) on the screen.
    fn to_screen(&self, bounds: Bounds<Pixels>) -> (f64, f64, f64, f64) {
        let (x, y) = (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y)));
        let (width, height) = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
        (self.frame.0 + x, self.frame.1 + self.frame.3 - y - height, width, height)
    }
}

/// How far up its picture a pose's feet are, as a share of its height: from the bottom to the lowest row that's at
/// least half opaque (a picture's own faint shadow under it doesn't count). None for a picture that can't be read.
fn feet_rise(bytes: &[u8]) -> Option<f32> {
    let picture = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = picture.dimensions();
    let lowest = (0..height).rev().find(|&row| (0..width).any(|column| picture.get_pixel(column, row)[3] >= 128))?;
    Some((height - 1 - lowest) as f32 / height as f32)
}

/// Where the shadow's pill sits, up from the bottom of the pet's square: its middle a point above the feet, so the
/// feet stand on it.
fn shadow_bottom(feet_rise: f32) -> f32 {
    PICTURE_BOTTOM + feet_rise * PICTURE + 1. - SHADOW_H / 2.
}

/// How far up the pose's picture its feet are (`feet_rise`), measured once per pose.
fn pose_feet(avatar: &str) -> f32 {
    static MEASURED: std::sync::Mutex<Option<HashMap<String, f32>>> = std::sync::Mutex::new(None);
    let mut measured = MEASURED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *measured
        .get_or_insert_with(Default::default)
        .entry(avatar.to_string())
        .or_insert_with(|| crate::assets::avatar_bytes(avatar).and_then(|bytes| feet_rise(&bytes)).unwrap_or(FEET_RISE))
}

/// The pet at rest as it stands on the desktop, `scale` times its size there: its resting picture breathing on its
/// shadow (Settings › Desktop pet).
pub fn resting(look: &Look, scale: f32) -> gpui::Div {
    let picture = PICTURE * scale;
    let feet = match look {
        Look::Animal(id, _) => pose_feet(&companion::mood_avatar(id, Mood::Idle, Glance::Ahead)),
        Look::Custom(_) => FEET_RISE,
    };
    let image = img(companion::mood_image(look, Mood::Idle, Glance::Ahead))
        .w(px(picture))
        .h(px(picture))
        .object_fit(ObjectFit::Contain);
    let figure = if crate::ui::reduce_motion() {
        image.into_any_element()
    } else {
        // A slow stretch from the feet, as the pet breathes, only without the pauses between breaths.
        image
            .with_animation(
                "pet-preview-breath",
                Animation::new(Duration::from_millis(3200)).repeat().with_easing(gpui::ease_in_out),
                move |image, t| {
                    let swell = (t * std::f32::consts::PI).sin();
                    image
                        .w(px(picture * (1. - 0.012 * swell)))
                        .h(px(picture * (1. + 0.035 * swell)))
                        .object_fit(ObjectFit::Fill)
                },
            )
            .into_any_element()
    };
    div()
        .relative()
        .size(px(PET as f32 * scale))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_end()
        .pb(px(PICTURE_BOTTOM * scale))
        .child(
            div()
                .absolute()
                .bottom(px(shadow_bottom(feet) * scale))
                .w(px(SHADOW_W * scale))
                .h(px(SHADOW_H * scale))
                .rounded(px(radius::FULL))
                .shadow(vec![BoxShadow {
                    color: gpui::black().opacity(0.22),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(SHADOW_BLUR * scale),
                    spread_radius: px(0.),
                }]),
        )
        .child(div().flex().items_end().justify_center().w(px(picture)).h(px(picture)).child(figure))
}

/// The heights of the tray's rows for `rows`: those it lists (one waiting on the user a line taller when its title
/// takes two, as `title_lines` says), then "N more" when there are more.
fn tray_heights(rows: &[companion::TrayRow], title_lines: impl Fn(&companion::TrayRow) -> usize) -> Vec<f32> {
    let line = tray_title_line();
    let mut heights: Vec<f32> = rows
        .iter()
        .take(TRAY_ROWS)
        .map(|row| match row.ask {
            Some(_) => TRAY_ASK_H + title_lines(row).saturating_sub(1) as f32 * line,
            None => TRAY_ROW_H,
        })
        .collect();
    if rows.len() > TRAY_ROWS {
        heights.push(TRAY_MORE_H);
    }
    heights
}

/// A row title's line height, at the chosen text scale (`Typography::footnote`'s 17).
fn tray_title_line() -> f32 {
    (17. * crate::theme::text_scale()).round()
}

/// Which of the tray's rows (of these `heights`) is at `at` (window coordinates), past its border, padding and header.
fn tray_row(tray: Bounds<Pixels>, heights: &[f32], (x, y): (f32, f32)) -> Option<usize> {
    let (left, top) = (f32::from(tray.origin.x), f32::from(tray.origin.y));
    if x < left || x >= left + f32::from(tray.size.width) {
        return None;
    }
    let mut offset = y - top - TRAY_TOP;
    if offset < 0. {
        return None;
    }
    for (index, height) in heights.iter().enumerate() {
        if offset < *height {
            return Some(index);
        }
        offset -= height + TRAY_GAP;
        if offset < 0. {
            return None;
        }
    }
    None
}

fn inside(point: NSPoint, (x, y, width, height): (f64, f64, f64, f64)) -> bool {
    point.x >= x && point.x < x + width && point.y >= y && point.y < y + height
}

/// Whether a point on the screen is on something the pet draws that takes clicks: the pet, its bubble, its tray
/// (the bounds are where they were last drawn, in the window).
fn takes_clicks(
    layout: &Layout,
    bubble: Option<Bounds<Pixels>>,
    tray: Option<Bounds<Pixels>>,
    pointer: NSPoint,
) -> bool {
    layout.over_pet(pointer)
        || bubble.is_some_and(|bubble| inside(pointer, layout.to_screen(bubble)))
        || tray.is_some_and(|tray| inside(pointer, layout.to_screen(tray)))
}

/// Times the event monitors pointed clicks a new way (`FILO_PET_DRIVE` reports it).
#[cfg(feature = "dev")]
static MONITOR_SWITCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Where the window's clicks go, shared by the pointer poll and the AppKit event monitors.
#[derive(Clone)]
struct HitTest {
    ns_window: usize,
    layout: Rc<Cell<Layout>>,
    bubble: Rc<Cell<Option<Bounds<Pixels>>>>,
    tray: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The window takes clicks (else they go to the app below).
    interactive: Rc<Cell<bool>>,
}

impl HitTest {
    fn set(&self, interactive: bool) -> bool {
        if interactive == self.interactive.get() {
            return false;
        }
        self.interactive.set(interactive);
        set_ignores_mouse(self.ns_window, !interactive);
        true
    }

    /// A mouse move: clicks go to the pet where it draws something, through it elsewhere. Not while the button is
    /// held: a press (a drag) keeps the window it began in.
    fn moved(&self) {
        if mouse_down() {
            return;
        }
        let interactive = takes_clicks(&self.layout.get(), self.bubble.get(), self.tray.get(), mouse_location());
        if self.set(interactive) {
            #[cfg(feature = "dev")]
            MONITOR_SWITCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// AppKit monitors for mouse moves, over other apps (global) and over Filo's own windows (local), which point the
/// pet's clicks the right way as the pointer moves: a click straight after moving onto the pet, or off it, goes to
/// the right app without waiting for the pointer poll. Watching the mouse needs no permission (only keys do). They
/// go with the pet.
struct Monitors(Vec<Retained<AnyObject>>);

impl Monitors {
    fn install(hit: HitTest) -> Monitors {
        // NSEventMaskMouseMoved
        const MOUSE_MOVED: u64 = 1 << 5;
        let global_hit = hit.clone();
        let global = block2::RcBlock::new(move |_event: std::ptr::NonNull<AnyObject>| global_hit.moved());
        let local = block2::RcBlock::new(move |event: std::ptr::NonNull<AnyObject>| -> *mut AnyObject {
            hit.moved();
            event.as_ptr()
        });
        // SAFETY: AppKit messages on the main thread; AppKit copies the blocks and calls them on the main thread,
        // outside GPUI's updates, and they only message the pet's window.
        unsafe {
            let global: Option<Retained<AnyObject>> =
                msg_send![class!(NSEvent), addGlobalMonitorForEventsMatchingMask: MOUSE_MOVED, handler: &*global];
            let local: Option<Retained<AnyObject>> =
                msg_send![class!(NSEvent), addLocalMonitorForEventsMatchingMask: MOUSE_MOVED, handler: &*local];
            Monitors(global.into_iter().chain(local).collect())
        }
    }
}

impl Drop for Monitors {
    fn drop(&mut self) {
        for monitor in &self.0 {
            // SAFETY: removing a monitor AppKit gave us, on the main thread.
            unsafe {
                let _: () = msg_send![class!(NSEvent), removeMonitor: &**monitor];
            }
        }
    }
}

/// Where a saved spot puts the pet now: on the screen holding its middle (or its corner), inside that screen's
/// usable part, since the display may have shrunk or the Dock moved since it was left there. On no screen, None
/// (it starts in the corner). The saved spot itself stays as it was.
fn restored_origin(
    saved: Option<(f64, f64)>,
    visible_at: impl Fn((f64, f64)) -> Option<NSRect>,
) -> Option<((f64, f64), NSRect)> {
    let saved = saved?;
    let screen = visible_at((saved.0 + PET / 2., saved.1 + PET / 2.)).or_else(|| visible_at(saved))?;
    Some((clamp_onto(saved, screen), screen))
}

/// The pet's corner kept so all of it is on the screen's usable part.
fn clamp_onto((x, y): (f64, f64), screen: NSRect) -> (f64, f64) {
    (
        x.clamp(screen.origin.x, (screen.origin.x + screen.size.width - PET).max(screen.origin.x)),
        y.clamp(screen.origin.y, (screen.origin.y + screen.size.height - PET).max(screen.origin.y)),
    )
}

pub struct Pet {
    companion: Entity<Companion>,
    ns_window: usize,
    layout: Layout,
    reply: Option<companion::Reply>,
    _reply_timer: Option<Task<()>>,
    hovered: bool,
    status: bool,
    _hover_timer: Option<Task<()>>,
    /// Where the bubble and the tray were last drawn, for letting clicks through around them.
    bubble_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    tray_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The tray row under the pointer: worked out here, since GPUI's hover goes stale while clicks go through.
    tray_hover: Option<usize>,
    /// The requests waiting on the user last seen, to open a folded tray for a new one.
    asks_seen: HashSet<String>,
    /// Where clicks go (the pet, its bubble and tray take them; elsewhere they go to the app below), and the
    /// monitors that keep it up to date as the mouse moves.
    hit: HitTest,
    _monitors: Monitors,
    dragging: bool,
    glance: Glance,
    /// A new way to look, and since when the pointer has been there.
    glance_next: Option<(Glance, Instant)>,
    /// The picture shown before the current one, kept up while the new one loads.
    last_picture: Option<gpui::ImageSource>,
    /// The picture last shown, so a change of pose hops.
    pose: Option<String>,
    shown_picture: Option<gpui::ImageSource>,
    /// The animal's pose showing (`fox-left`), and how far up its picture each of the animal's poses has its feet,
    /// for the shadow under them (measured once, off the main thread).
    pose_avatar: Option<String>,
    feet: HashMap<String, f32>,
    hop: usize,
    breath: usize,
    reduce_motion: bool,
    _drag: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui::Subscription>,
}

impl Pet {
    fn new(companion: Entity<Companion>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ns_window = ns_window(window).unwrap_or(0);
        prepare_window(ns_window);
        let (origin, screen) = restored_origin(companion::settings(cx).pet_origin, screen_at).unwrap_or_else(|| {
            let origin = start_origin();
            (origin, screen_at(origin).unwrap_or_else(main_screen))
        });
        let layout = Layout::new(origin, screen);
        let (bubble_bounds, tray_bounds) = (Rc::new(Cell::new(None)), Rc::new(Cell::new(None)));
        let hit = HitTest {
            ns_window,
            layout: Rc::new(Cell::new(layout)),
            bubble: Rc::clone(&bubble_bounds),
            tray: Rc::clone(&tray_bounds),
            interactive: Rc::new(Cell::new(true)),
        };
        let monitors = Monitors::install(hit.clone());
        // AppKit reports the frame to GPUI at once, which can't happen while GPUI is opening the window.
        let frame = layout.frame;
        window
            .spawn(cx, async move |_| {
                set_frame(ns_window, frame);
                order_front(ns_window);
            })
            .detach();

        let subs = vec![
            cx.observe_in(&companion, window, |this: &mut Self, _, window, cx| this.refresh(window, cx)),
            cx.subscribe_in(&companion, window, |this: &mut Self, _, event: &CompanionEvent, window, cx| match event {
                CompanionEvent::Reply(notice) => this.show_reply(notice.clone(), window, cx),
            }),
        ];
        let pointer = cx.spawn_in(window, async move |this, cx| {
            let mut wait = POINTER_NEAR;
            loop {
                cx.background_executor().timer(wait).await;
                match this.update_in(cx, |this, window, cx| this.follow_pointer(window, cx)) {
                    Ok(next) => wait = next,
                    Err(_) => break,
                }
            }
        });
        let breathe = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(BREATH_EVERY).await;
                let breathed = this.update(cx, |this, cx| {
                    let resting = matches!(this.companion.read(cx).mood(), Mood::Idle | Mood::Night);
                    if resting && !this.reduce_motion {
                        this.breath += 1;
                        cx.notify();
                    }
                });
                if breathed.is_err() {
                    break;
                }
            }
        });
        let tasks = vec![pointer, breathe];
        // `FILO_PET_DRIVE` (`dev` builds) drives the pet.
        #[cfg(feature = "dev")]
        let tasks: Vec<_> =
            tasks.into_iter().chain(drive::steps().map(|steps| drive::start(steps, window, cx))).collect();
        Pet {
            companion,
            ns_window,
            layout,
            reply: None,
            _reply_timer: None,
            hovered: false,
            status: false,
            _hover_timer: None,
            bubble_bounds,
            tray_bounds,
            tray_hover: None,
            asks_seen: HashSet::new(),
            hit,
            _monitors: monitors,
            dragging: false,
            glance: Glance::Ahead,
            glance_next: None,
            last_picture: None,
            pose: None,
            shown_picture: None,
            pose_avatar: None,
            feet: HashMap::new(),
            hop: 0,
            breath: 0,
            reduce_motion: crate::ui::reduce_motion(),
            _drag: None,
            _tasks: tasks,
            _subs: subs,
        }
    }

    fn bubble(&self, cx: &App) -> Option<Bubble> {
        let companion = self.companion.read(cx);
        if companion.listening() {
            return Some(Bubble::Listening);
        }
        if let Some(reminder) = companion.due() {
            return Some(Bubble::Due(reminder.clone()));
        }
        if let Some(reply) = &self.reply {
            return Some(Bubble::Reply(reply.clone()));
        }
        self.status.then_some(Bubble::Status)
    }

    /// After a change: a hop for a new pose (not for glancing about), keeping the old picture up while it loads.
    fn refresh(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_pose(true, cx);
        // Something new needs the user: a folded tray opens again (the user may fold it once more).
        let asks: HashSet<String> = self
            .companion
            .read(cx)
            .tray()
            .iter()
            .filter_map(|row| row.ask.as_ref().map(|ask| ask.request_id.clone()))
            .collect();
        if asks.iter().any(|ask| !self.asks_seen.contains(ask)) && companion::settings(cx).tray_collapsed {
            cx.defer(|cx| companion::update_settings(cx, |settings| settings.tray_collapsed = false));
        }
        self.asks_seen = asks;
        cx.notify();
    }

    fn set_pose(&mut self, hop: bool, cx: &mut Context<Self>) {
        let companion = self.companion.read(cx);
        let Some(look) = companion.pet_look() else { return };
        let mood = companion.mood();
        let pose = Some(companion::mood_key(look, mood, self.glance));
        if pose == self.pose {
            return;
        }
        if self.pose.is_some() {
            // What was showing, as it was showing (the glance may have just changed).
            self.last_picture = self.shown_picture.clone();
            if hop && !self.reduce_motion {
                self.hop += 1;
            }
        }
        self.shown_picture = Some(companion::mood_image(look, mood, self.glance));
        self.pose = pose;
        let Look::Animal(id, _) = look else {
            self.pose_avatar = None;
            return;
        };
        let avatar = companion::mood_avatar(id, mood, self.glance);
        if !self.feet.contains_key(&avatar) {
            // Every pose of this animal at once, so the shadow doesn't jump as it changes pose.
            let moods = [Mood::Idle, Mood::Night, Mood::Working, Mood::Replying, Mood::Celebrating, Mood::Due];
            let glances = [Glance::Ahead, Glance::Left, Glance::Right];
            let mut poses: Vec<String> = moods
                .into_iter()
                .flat_map(|mood| glances.map(|glance| companion::mood_avatar(id, mood, glance)))
                .collect();
            poses.retain(|pose| self.feet.insert(pose.clone(), FEET_RISE).is_none());
            let measured = cx.background_spawn(async move {
                poses
                    .into_iter()
                    .filter_map(|pose| {
                        crate::assets::avatar_bytes(&pose).and_then(|bytes| feet_rise(&bytes)).map(|rise| (pose, rise))
                    })
                    .collect::<Vec<_>>()
            });
            cx.spawn(async move |this, cx| {
                let measured = measured.await;
                this.update(cx, |this, cx| {
                    this.feet.extend(measured);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.pose_avatar = Some(avatar);
    }

    fn show_reply(&mut self, notice: companion::Reply, window: &mut Window, cx: &mut Context<Self>) {
        self.reply = Some(notice);
        self.start_reply_timer(window, cx);
        self.refresh(window, cx);
    }

    fn start_reply_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._reply_timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(REPLY_FOR).await;
            this.update_in(cx, |this, window, cx| {
                if !this.hovered {
                    this.reply = None;
                    this.refresh(window, cx);
                }
            })
            .ok();
        }));
    }

    fn dismiss_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reply = None;
        self._reply_timer = None;
        self.refresh(window, cx);
    }

    fn hover(&mut self, hovered: bool, window: &mut Window, cx: &mut Context<Self>) {
        if hovered == self.hovered {
            return;
        }
        self.hovered = hovered;
        if hovered {
            self._hover_timer = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(HOVER_DELAY).await;
                this.update_in(cx, |this, window, cx| {
                    if this.hovered {
                        this.status = true;
                        this.refresh(window, cx);
                    }
                })
                .ok();
            }));
        } else {
            self._hover_timer = None;
            self.status = false;
            // The reply waited while the pointer was on it.
            if self.reply.is_some() {
                self.start_reply_timer(window, cx);
            }
            self.refresh(window, cx);
        }
    }

    /// Where the pointer is: whether the window takes clicks, the hover, and where the pet looks. Returns when to
    /// look again.
    fn follow_pointer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Duration {
        let pointer = mouse_location();
        let bubble = self.bubble_bounds.get().map(|bounds| self.layout.to_screen(bounds));
        let on_pet = self.layout.over_pet(pointer);
        let on_bubble = bubble.is_some_and(|bubble| inside(pointer, bubble));
        let on_tray = self.tray_bounds.get().is_some_and(|tray| inside(pointer, self.layout.to_screen(tray)));
        let heights = self.tray_row_heights(self.companion.read(cx).tray(), cx);
        let collapsed = companion::settings(cx).tray_collapsed;
        let tray_hover = self.tray_bounds.get().filter(|_| on_tray && !self.dragging).and_then(|tray| {
            if collapsed { Some(0) } else { tray_row(tray, &heights, self.layout.to_window(pointer)) }
        });
        if tray_hover != self.tray_hover {
            self.tray_hover = tray_hover;
            cx.notify();
        }
        // The monitors keep this up as the mouse moves; this catches what moves without it (the pet's own drop, a
        // bubble or tray coming up under a still pointer).
        self.hit.set(on_pet || on_bubble || on_tray || self.dragging);
        if !self.dragging {
            // Over the bubble counts too, so a reply being read stays up.
            self.hover(on_pet || on_bubble, window, cx);
        }

        let companion = self.companion.read(cx);
        let can_glance =
            companion.pet_look().is_some_and(companion::can_glance) && companion.mood() == Mood::Idle && !self.hovered;
        let center = (self.layout.origin.0 + PET / 2., self.layout.origin.1 + PET / 2.);
        let (dx, dy) = (pointer.x - center.0, pointer.y - center.1);
        let wanted = if !can_glance || dx.abs() < GLANCE_DEAD_ZONE || dx.abs() > GLANCE_RANGE || dy.abs() > GLANCE_RANGE
        {
            Glance::Ahead
        } else if dx < 0. {
            Glance::Left
        } else {
            Glance::Right
        };
        // Only once the pointer has stayed: no flicking back and forth.
        if wanted == self.glance {
            self.glance_next = None;
        } else {
            match self.glance_next {
                Some((next, since)) if next == wanted && since.elapsed() >= GLANCE_SETTLE => {
                    self.glance = wanted;
                    self.glance_next = None;
                    self.set_pose(false, cx);
                    cx.notify();
                }
                Some((next, _)) if next == wanted => {}
                _ => self.glance_next = Some((wanted, Instant::now())),
            }
        }

        let (x, y, width, height) = self.layout.frame;
        let near = pointer.x > x - NEAR
            && pointer.x < x + width + NEAR
            && pointer.y > y - NEAR
            && pointer.y < y + height + NEAR;
        if near || self.dragging { POINTER_NEAR } else { POINTER_FAR }
    }

    /// A press on the pet: dragged, it moves (and stays there); let go in place, it's a click.
    fn press(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ns_window = self.ns_window;
        let start_mouse = mouse_location();
        let start_origin = self.layout.origin;
        let start_frame = self.layout.frame;
        self.dragging = true;
        self._drag = Some(cx.spawn_in(window, async move |this, cx| {
            let mut dragged = false;
            loop {
                cx.background_executor().timer(Duration::from_millis(8)).await;
                let pointer = mouse_location();
                let (dx, dy) = (pointer.x - start_mouse.x, pointer.y - start_mouse.y);
                if !dragged && dx.hypot(dy) > DRAG_SLOP {
                    dragged = true;
                }
                if dragged {
                    // Outside any GPUI update: AppKit tells GPUI about the move straight away.
                    move_frame(ns_window, (start_frame.0 + dx, start_frame.1 + dy));
                }
                if !mouse_down() {
                    break;
                }
            }
            let pointer = mouse_location();
            let (dx, dy) = (pointer.x - start_mouse.x, pointer.y - start_mouse.y);
            this.update_in(cx, |this, window, cx| {
                this.dragging = false;
                if dragged {
                    this.dropped((start_origin.0 + dx, start_origin.1 + dy), window, cx);
                } else {
                    this.click(cx);
                }
            })
            .ok();
        }));
    }

    /// Settles where it was dropped (kept on screen) and remembers it.
    fn dropped(&mut self, origin: (f64, f64), window: &mut Window, cx: &mut Context<Self>) {
        let screen = screen_at((origin.0 + PET / 2., origin.1 + PET / 2.))
            .or_else(|| screen_at(origin))
            .unwrap_or_else(main_screen);
        let origin = clamp_onto(origin, screen);
        self.layout = Layout::new(origin, screen);
        self.hit.layout.set(self.layout);
        let (ns_window, frame) = (self.ns_window, self.layout.frame);
        window.spawn(cx, async move |_| set_frame(ns_window, frame)).detach();
        companion::update_settings(cx, |settings| settings.pet_origin = Some(origin));
        cx.notify();
    }

    /// Opens what the pet is showing: the reply, the newest unseen chat, or Filo.
    fn click(&mut self, cx: &mut Context<Self>) {
        let command = match (&self.reply, self.companion.read(cx).newest_unseen()) {
            (Some(reply), _) => Command::OpenChat(reply.session_id.clone()),
            (None, Some(session_id)) => Command::OpenChat(session_id),
            (None, None) => Command::Open,
        };
        self.reply = None;
        self._reply_timer = None;
        cx.notify();
        companion::run(command, cx);
    }

    fn render_pet(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let (mood, unseen, needs_you, poses) = {
            let companion = self.companion.read(cx);
            (
                companion.mood(),
                companion.unseen_total(),
                companion.needs_you(),
                companion.pet_look().map(companion::all_mood_images).unwrap_or_default(),
            )
        };
        let picture = self.shown_picture.clone();
        let previous = self.last_picture.clone();
        let (left, top) = self.layout.pet_offset();
        let reduce_motion = self.reduce_motion;

        let feet = self.pose_avatar.as_ref().and_then(|avatar| self.feet.get(avatar).copied()).unwrap_or(FEET_RISE);

        // Breathing: a slow stretch from the feet.
        let breath = self.breath;
        let figure = picture.map(|picture| {
            // The last picture stays up while a new one loads, so a change of pose never blinks.
            let image =
                img(picture).w(px(PICTURE)).h(px(PICTURE)).object_fit(ObjectFit::Contain).with_loading(move || {
                    match previous.clone() {
                        Some(previous) => img(previous)
                            .w(px(PICTURE))
                            .h(px(PICTURE))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                        None => div().into_any_element(),
                    }
                });
            if reduce_motion || breath == 0 {
                image.into_any_element()
            } else {
                image
                    .with_animation(
                        SharedString::from(format!("pet-breath-{breath}")),
                        Animation::new(Duration::from_millis(1800)).with_easing(gpui::ease_in_out),
                        |image, t| {
                            let swell = (t * std::f32::consts::PI).sin();
                            image
                                .w(px(PICTURE * (1. - 0.012 * swell)))
                                .h(px(PICTURE * (1. + 0.035 * swell)))
                                .object_fit(ObjectFit::Fill)
                        },
                    )
                    .into_any_element()
            }
        });

        // A hop for a new pose; a busy bob while Filo works.
        let busy = matches!(mood, Mood::Working | Mood::Replying) && !reduce_motion;
        let body = div().relative().flex().items_end().justify_center().w(px(PICTURE)).h(px(PICTURE)).children(figure);
        let body = if busy {
            // On the spinners' stepped clock (ui::ticker), not a repeating animation: the pet stays up over every app
            // and would draw every frame for as long as a coding task runs.
            let t = (crate::ui::ticker::seconds() / 0.9).fract();
            body.top(px(-2.5 * (t * std::f32::consts::PI * 2.).sin().abs()))
                .child(canvas(|_, window, cx| crate::ui::ticker::keep(window, cx), |_, _, _, _| {}).absolute().size_0())
                .into_any_element()
        } else if self.hop > 0 && !reduce_motion {
            body.with_animation(
                SharedString::from(format!("pet-hop-{}", self.hop)),
                Animation::new(Duration::from_millis(520)),
                |body, t| body.top(px(-12. * (t * std::f32::consts::PI).sin())),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        };

        let badge = match mood {
            Mood::Due => Some(("!".to_string(), colors.warning, colors.on_warning)),
            Mood::Listening => None,
            // A coding tool waits on a permission or an answer (the tray has it).
            _ if needs_you => Some(("!".to_string(), colors.warning, colors.on_warning)),
            _ if unseen > 0 => Some((crate::unseen::format_count(unseen), colors.accent, colors.on_accent)),
            _ => None,
        };

        div()
            .id("pet")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(PET as f32))
            .h(px(PET as f32))
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .pb(px(PICTURE_BOTTOM))
            .cursor_pointer()
            // Every pose of this look, loaded ahead and kept, so changing pose is instant.
            .child(
                div()
                    .absolute()
                    .size(px(1.))
                    .opacity(0.)
                    .children(poses.into_iter().map(|pose| img(pose).size(px(1.)))),
            )
            // A soft shadow the feet stand on: blurred out from a pill tucked just behind them.
            .child(
                div()
                    .absolute()
                    .bottom(px(shadow_bottom(feet)))
                    .w(px(SHADOW_W))
                    .h(px(SHADOW_H))
                    .rounded(px(radius::FULL))
                    .shadow(vec![BoxShadow {
                        color: gpui::black().opacity(0.22),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(SHADOW_BLUR),
                        spread_radius: px(0.),
                    }]),
            )
            .when(mood == Mood::Listening && !reduce_motion, |this| {
                this.child(
                    div()
                        .absolute()
                        .bottom(px(10.))
                        .size(px(PICTURE))
                        .rounded(px(radius::FULL))
                        .border_2()
                        .border_color(colors.accent)
                        .with_animation(
                            "pet-listening",
                            Animation::new(Duration::from_millis(1300)).repeat(),
                            |ring, t| ring.opacity(1. - t).size(px(PICTURE * (0.7 + 0.35 * t))),
                        ),
                )
            })
            .child(body)
            .when(mood == Mood::Night, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(8.))
                        .right(px(18.))
                        .caption()
                        .semibold()
                        .text_color(colors.text_tertiary)
                        .child("z z"),
                )
            })
            .when_some(badge, |this, (label, bg, fg)| {
                this.child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .right(px(12.))
                        .min_w(px(22.))
                        .h(px(22.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::FULL))
                        .bg(bg)
                        .border_2()
                        .border_color(colors.background)
                        .caption()
                        .bold()
                        .text_color(fg)
                        .child(label),
                )
            })
            .when(mood == Mood::Listening, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .right(px(12.))
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::FULL))
                        .bg(colors.accent)
                        .border_2()
                        .border_color(colors.background)
                        .child(ui::icon_sized("mic", 13., colors.on_accent)),
                )
            })
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.press(window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, window, cx| {
                    // The menu runs until it closes: outside GPUI's event handling.
                    window.spawn(cx, async move |_| crate::menu_bar::pop_up()).detach();
                }),
            )
    }

    fn render_bubble(&self, bubble: Bubble, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let (name, unseen, status, screen, to, words, problem) = {
            let companion = self.companion.read(cx);
            let name = companion.agent_name().to_string();
            // With the tray up, it says what's working: the bubble doesn't say it again.
            let status = if companion.tray().is_empty() { companion.status() } else { None };
            let (words, problem) =
                (companion.listening_words().map(last_words), companion.listening_problem().map(str::to_string));
            (
                name,
                companion.unseen_total(),
                status,
                companion.listening_to_screen(),
                companion.listening_to().map(str::to_string),
                words,
                problem,
            )
        };
        let content = match bubble {
            Bubble::Listening => div()
                .flex()
                .items_center()
                .gap(px(space::MD))
                .child(
                    div()
                        .size(px(36.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::FULL))
                        .bg(colors.accent_soft)
                        .child(ui::icon_sized(if screen { "monitor" } else { "mic" }, 18., colors.on_accent_soft)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().subhead().semibold().child(if screen {
                            format!("{name} is looking and listening…")
                        } else {
                            format!("{name} is listening…")
                        }))
                        .child(div().footnote().text_color(colors.text_secondary).child(if screen {
                            "Let go to send with a screenshot"
                        } else {
                            "Let go to send"
                        }))
                        // Held from another app: who it goes to (a name it starts with may send it elsewhere).
                        .children(
                            to.map(|to| div().footnote().semibold().text_color(colors.accent).child(format!("→ {to}"))),
                        )
                        // What it has heard so far, or why it can't be sent (voice messages used up, offline).
                        .children(match problem {
                            Some(problem) => Some(
                                crate::screens::agent_work::max_lines(
                                    div().footnote().text_color(colors.danger),
                                    3.,
                                    18.,
                                )
                                .child(problem),
                            ),
                            None => words.map(|words| {
                                crate::screens::agent_work::max_lines(
                                    div().footnote().text_color(colors.text_secondary),
                                    2.,
                                    18.,
                                )
                                .child(words)
                            }),
                        }),
                )
                .into_any_element(),
            Bubble::Due(reminder) => {
                let (done, snooze, dismiss) = (reminder.id.clone(), reminder.id.clone(), reminder.id.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::SM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .caption()
                                    .semibold()
                                    .text_color(colors.accent)
                                    .child(format!("Reminder · {}", crate::format::clock(reminder.due_at))),
                            )
                            .child(IconButton::new("pet-due-close", "x").small().tooltip("Dismiss").on_click(
                                move |_, _, cx| companion::run(Command::DismissReminder(dismiss.clone()), cx),
                            )),
                    )
                    .child(
                        crate::screens::agent_work::max_lines(div().headline(), 2., 22.).child(reminder.title.clone()),
                    )
                    .when_some(reminder.note.clone().filter(|note| !note.trim().is_empty()), |this, note| {
                        this.child(div().footnote().text_color(colors.text_secondary).truncate().child(note))
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(space::SM))
                            .child(
                                Button::primary("pet-due-done", "Done")
                                    .small()
                                    .icon("check")
                                    .on_click(move |_, _, cx| companion::run(Command::ReminderDone(done.clone()), cx)),
                            )
                            .child(
                                Button::secondary(
                                    "pet-due-snooze",
                                    format!("Snooze {} min", companion::SNOOZE_MINUTES),
                                )
                                .small()
                                .on_click(move |_, _, cx| companion::run(Command::ReminderSnooze(snooze.clone()), cx)),
                            ),
                    )
                    .into_any_element()
            }
            Bubble::Reply(reply) => {
                let session_id = reply.session_id.clone();
                // The bubble's text width: less its border and padding.
                let text_w = BUBBLE_W as f32 - BUBBLE_MARGIN * 2. - 2. - space::MD * 2.;
                div()
                    .id("pet-reply")
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().flex_1().min_w_0().subhead().semibold().truncate().child(reply.title.clone()))
                            .child(IconButton::new("pet-reply-close", "x").small().tooltip("Dismiss").on_click(
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_reply(window, cx);
                                }),
                            )),
                    )
                    .when_some(reply.subtitle.clone(), |this, subtitle| {
                        this.child(div().caption().text_color(colors.text_tertiary).child(subtitle))
                    })
                    // Whole lines, the last cut with "…": a box capped in height would show part of the next one.
                    .child(
                        div().flex().flex_col().footnote().text_color(colors.text_secondary).children(
                            up_to_lines(
                                &reply.body,
                                REPLY_LINES,
                                text_w,
                                gpui::font(cx.theme().font_family()),
                                crate::theme::TypeVariant::Footnote.size(),
                                cx,
                            )
                            .into_iter()
                            .map(|line| one_line(div(), line, text_w)),
                        ),
                    )
                    .child(
                        div().pt(px(space::XS)).footnote().semibold().text_color(colors.accent).child(if reply.agent {
                            "Open agent"
                        } else {
                            "Open chat"
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.reply = None;
                        this._reply_timer = None;
                        cx.notify();
                        companion::run(Command::OpenChat(session_id.clone()), cx);
                    }))
                    .into_any_element()
            }
            Bubble::Status => {
                let settings = companion::settings(cx);
                let label = |keys: String| crate::hotkeys::label(Some(&keys));
                let hints: Vec<String> = [
                    settings.shortcut(companion::OPEN).map(|keys| format!("{} to open · hold to talk", label(keys))),
                    settings
                        .shortcut(companion::SCREEN)
                        .map(|keys| format!("{} hold to ask about your screen", label(keys))),
                ]
                .into_iter()
                .flatten()
                .collect();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .when(status.is_some() || (unseen == 0 && hints.is_empty()), |this| {
                        this.child(
                            div()
                                .subhead()
                                .semibold()
                                .child(status.clone().unwrap_or_else(|| format!("{name} is here if you need it"))),
                        )
                    })
                    .when(unseen > 0, |this| {
                        let label = if unseen == 1 {
                            "1 new reply · click to read".to_string()
                        } else {
                            format!("{} new replies · click to read", crate::unseen::format_count(unseen))
                        };
                        this.child(div().footnote().text_color(colors.accent).child(label))
                    })
                    .when(!hints.is_empty(), |this| {
                        this.child(
                            div()
                                .pt(px(space::XS))
                                .flex()
                                .flex_col()
                                .caption()
                                .text_color(colors.text_tertiary)
                                .children(hints.into_iter().map(|hint| div().child(hint))),
                        )
                    })
                    .into_any_element()
            }
        };
        let width = BUBBLE_W as f32 - BUBBLE_MARGIN * 2.;
        let bounds = self.bubble_bounds.clone();
        div()
            .relative()
            .w(px(width))
            .ml(px(self.layout.bubble_left() + BUBBLE_MARGIN))
            .my(px(2.))
            .p(px(space::MD))
            .rounded(px(radius::LG))
            .bg(colors.raised)
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            .text_color(colors.text)
            .child(content)
            // Where it was drawn, so clicks around it go through to the app below.
            .child(
                canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
    }

    /// What Filo and the agents are working on, a row each (`Companion::tray`): who and on what, what it's doing now,
    /// and how long; a coding tool waiting on the user is highlighted with Allow and Deny (a question opens the chat
    /// to answer); a task that just finished says how it went. A row opens its chat, or a finished task's result.
    fn render_tray(&self, tray: &[companion::TrayRow], cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let now = crate::format::now_ms();
        let more = tray.len().saturating_sub(TRAY_ROWS);
        let hovered = self.tray_hover;
        let heights = self.tray_row_heights(tray, cx);
        let row = |id: SharedString, index: usize| {
            div()
                .id(id)
                .h(px(heights[index]))
                .px(px(space::SM))
                .flex()
                .items_center()
                .gap(px(space::SM))
                .rounded(px(radius::SM))
                .cursor_pointer()
                .when(hovered == Some(index), |this| this.bg(colors.surface))
        };
        let rows: Vec<_> = tray
            .iter()
            .take(TRAY_ROWS)
            .enumerate()
            .map(|(index, task)| {
                let key = task.key.clone();
                let (name, face) = self.tray_who(task, TRAY_FACE, cx);
                let title =
                    if task.title.trim().is_empty() || task.title == name { None } else { Some(task.title.clone()) };
                let (heading, elapsed, heading_w) = self.tray_heading(task, now, cx);
                // Under the heading: who asks, or an agent's or Filo's task (the chat's title) and how it's going.
                let line = match (&task.ask, &task.finished) {
                    (Some(ask), _) => format!("{name} · Needs you · {}", ask.question),
                    (None, finished) => {
                        let state = match finished {
                            Some(finished) => finished_line(finished),
                            None if task.tasks > 1 => format!("Working · {} tasks", task.tasks),
                            None => "Working…".into(),
                        };
                        title.map_or_else(|| state.clone(), |title| format!("{title} · {state}"))
                    }
                };
                let line_color = if task.ask.is_some() { colors.on_warning_soft } else { colors.text_secondary };
                let ring = match (&task.ask, &task.finished) {
                    (Some(_), _) => Some(
                        div()
                            .absolute()
                            .size(px(TRAY_RING))
                            .rounded(px(radius::FULL))
                            .border_2()
                            .border_color(colors.warning)
                            .into_any_element(),
                    ),
                    (None, Some(finished)) => {
                        let (icon, bg, fg) = match finished.status.as_str() {
                            "done" => ("check", colors.success, colors.on_success),
                            "failed" => ("x", colors.danger, colors.on_danger),
                            _ => ("square", colors.surface_strong, colors.text_secondary),
                        };
                        Some(
                            div()
                                .absolute()
                                .right(px(0.))
                                .bottom(px(0.))
                                .size(px(14.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(radius::FULL))
                                .bg(bg)
                                .border_1()
                                .border_color(colors.raised)
                                .child(ui::icon_sized(icon, 9., fg))
                                .into_any_element(),
                        )
                    }
                    // Working: the picture alone; "Working…" under the name says so.
                    (None, None) => None,
                };
                let avatar = div()
                    .relative()
                    .size(px(TRAY_RING))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(face)
                    .children(ring);
                let heading = div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .footnote()
                    .semibold()
                    .text_color(colors.text)
                    .children(heading.into_iter().map(|line| one_line(div(), line, heading_w)));
                let first =
                    div().flex().items_start().gap(px(space::XS)).child(heading).when_some(elapsed, |this, elapsed| {
                        this.child(
                            div().flex_none().pt(px(1.)).caption().text_color(colors.text_tertiary).child(elapsed),
                        )
                    });
                let second = one_line(
                    div().caption().text_color(line_color),
                    line.split_whitespace().collect::<Vec<_>>().join(" "),
                    TRAY_TEXT_W,
                );
                let text = div().flex_1().min_w_0().flex().flex_col().child(first).child(second);
                let content = div()
                    .w_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(avatar)
                    .child(text);
                let open = task.command();
                let dismiss = task.dismiss.clone();
                let row = row(SharedString::from(format!("pet-tray-{key}")), index)
                    .when(task.ask.is_some(), |this| {
                        this.bg(colors.warning_soft).flex_col().items_start().justify_center().gap(px(space::XS))
                    })
                    .child(content)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !dismiss.is_empty() {
                            this.companion.update(cx, |companion, cx| companion.dismiss_finished(&dismiss, cx));
                        }
                        companion::run(open.clone(), cx);
                    }));
                match &task.ask {
                    Some(ask) => row.child(self.tray_ask_buttons(&key, ask, task.open_chat())).into_any_element(),
                    None => row.into_any_element(),
                }
            })
            .collect();
        let bounds = self.tray_bounds.clone();
        div()
            .relative()
            .w(px(TRAY_W))
            .ml(px(self.layout.tray_left))
            .my(px(2.))
            .p(px(space::XS))
            .flex()
            .flex_col()
            .rounded(px(radius::MD))
            .bg(colors.raised)
            .border_1()
            .border_color(colors.border)
            .shadow_md()
            .gap(px(TRAY_GAP))
            // What's at work, and the control that folds the tray into a pill.
            .child(
                div()
                    .h(px(TRAY_HEAD_H))
                    .pl(px(space::SM))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().caption().semibold().text_color(colors.text_tertiary).child(tray_summary(tray)))
                    .child(
                        div()
                            .id("pet-tray-fold")
                            .size(px(TRAY_HEAD_H))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(radius::SM))
                            .cursor_pointer()
                            .child(ui::icon_sized("chevron-down", 14., colors.text_secondary))
                            .on_click(|_, _, cx| {
                                companion::update_settings(cx, |settings| settings.tray_collapsed = true)
                            }),
                    ),
            )
            .children(rows)
            .when(more > 0, |this| {
                this.child(
                    row("pet-tray-more".into(), TRAY_ROWS)
                        .child(div().w(px(TRAY_RING)).flex_none())
                        .child(div().footnote().text_color(colors.text_secondary).child(format!("{more} more")))
                        .on_click(|_, _, cx| companion::run(Command::Open, cx)),
                )
            })
            .child(
                canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
    }

    /// The tray folded: a pill under the pet with the pictures of who's at work stacked and what's going on ("2
    /// working"); a click opens the tray again.
    fn render_pill(&self, tray: &[companion::TrayRow], cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let mut faces: Vec<gpui::AnyElement> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for row in tray {
            let (name, face) = self.tray_who(row, PILL_FACE, cx);
            if seen.contains(&name) || faces.len() == PILL_FACES {
                continue;
            }
            seen.push(name);
            // Stacked: each a little over the one before.
            let left = faces.len() as f32 * (PILL_FACE + 4. - PILL_FACE / 3.);
            faces.push(
                div()
                    .absolute()
                    .top_0()
                    .left(px(left))
                    .size(px(PILL_FACE + 4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::FULL))
                    .border_2()
                    .border_color(colors.raised)
                    .bg(colors.raised)
                    .child(face)
                    .into_any_element(),
            );
        }
        let bounds = self.tray_bounds.clone();
        div().w(px(TRAY_W)).ml(px(self.layout.tray_left)).my(px(2.)).flex().justify_center().child(
            div()
                .id("pet-tray-pill")
                .relative()
                .h(px(PILL_FACE + 12.))
                .pl(px(space::XS))
                .pr(px(space::SM))
                .flex()
                .items_center()
                .gap(px(space::SM))
                .rounded(px(radius::FULL))
                .bg(if self.tray_hover.is_some() { colors.surface } else { colors.raised })
                .border_1()
                .border_color(if self.companion.read(cx).needs_you() { colors.warning } else { colors.border })
                .shadow_md()
                .cursor_pointer()
                .child({
                    let stack =
                        PILL_FACE + 4. + faces.len().saturating_sub(1) as f32 * (PILL_FACE + 4. - PILL_FACE / 3.);
                    div().relative().flex_none().w(px(stack)).h(px(PILL_FACE + 4.)).children(faces)
                })
                .child(div().flex_none().footnote().semibold().text_color(colors.text).child(tray_summary(tray)))
                .child(ui::icon_sized("chevron-up", 14., colors.text_secondary))
                .on_click(|_, _, cx| companion::update_settings(cx, |settings| settings.tray_collapsed = false))
                // Its own bounds: clicks beside it go through.
                .child(
                    canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                ),
        )
    }

    /// The heights of the tray's rows (`tray_heights`), each waiting one's title in the lines `tray_heading` gives it.
    fn tray_row_heights(&self, tray: &[companion::TrayRow], cx: &App) -> Vec<f32> {
        let now = crate::format::now_ms();
        tray_heights(tray, |row| self.tray_heading(row, now, cx).0.len())
    }

    /// A tray row's heading: a waiting task's title (or who asks), in up to two lines; otherwise who works on it, in
    /// one. Each line is cut with "…" to fit beside the time (`two_lines`, `one_line`); with the time and that width.
    fn tray_heading(&self, task: &companion::TrayRow, now: f64, cx: &App) -> (Vec<String>, Option<String>, f32) {
        let name = self.companion.read(cx).who(task);
        let heading = if task.ask.is_some() && !task.title.trim().is_empty() { task.title.clone() } else { name };
        let elapsed = task.finished.is_none().then(|| crate::format::working_for(task.since, now));
        let family = gpui::font(cx.theme().font_family());
        let time_w = elapsed.as_deref().map_or(0., |elapsed| {
            let font = cx.text_system().resolve_font(&family);
            let size = px(crate::theme::TypeVariant::Caption.size());
            let width: f32 = elapsed
                .chars()
                .map(|ch| cx.text_system().advance(font, size, ch).map_or(0., |advance| f32::from(advance.width)))
                .sum();
            width.ceil() + space::XS
        });
        let width = TRAY_TEXT_W - time_w;
        let lines = match task.ask {
            Some(_) => two_lines(
                &heading,
                width,
                gpui::Font { weight: gpui::FontWeight::SEMIBOLD, ..family },
                crate::theme::TypeVariant::Footnote.size(),
                cx,
            ),
            None => vec![heading],
        };
        (lines, elapsed, width)
    }

    /// Who works on a tray row, by name (`Companion::who`), and their picture: the agent, the agents' group, or Filo.
    fn tray_who(&self, task: &companion::TrayRow, size: f32, cx: &App) -> (String, gpui::AnyElement) {
        use crate::screens::agents;
        let companion = self.companion.read(cx);
        let name = companion.who(task);
        if task.opens == companion::Opens::Agent(agents::GROUP.to_string()) && task.agent_id.is_none() {
            return (name, companion.group_tile(size, cx).into_any_element());
        }
        // A row of no agent's (or of one that's gone) is the main agent's, as `who` names it.
        let agent = task.agent_id.as_deref().and_then(|id| companion.agent(id)).or_else(|| companion.main_agent());
        let picture = match agent {
            Some(agent) => {
                agents::agent_picture(agents::picture_source(&agent.picture), agent.background.as_deref(), size, cx)
                    .into_any_element()
            }
            None => ui::letter_avatar(&name, size, cx.colors().accent, gpui::white()).into_any_element(),
        };
        (name, picture)
    }

    /// Under a row waiting on the user: Allow and Deny for a permission, or a way to the chat to answer a question.
    fn tray_ask_buttons(&self, key: &str, ask: &companion::Ask, open_chat: Command) -> impl IntoElement {
        let buttons = div().flex().gap(px(space::SM)).pl(px(TRAY_RING + space::SM));
        if !ask.is_permission() {
            return buttons.child(
                Button::primary(SharedString::from(format!("pet-tray-answer-{key}")), "Answer in chat")
                    .small()
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        companion::run(open_chat.clone(), cx);
                    }),
            );
        }
        let answer = |reply: &'static str| {
            let request = ask.request_id.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                cx.stop_propagation();
                companion::run(Command::AnswerCodingRequest(request.clone(), reply.into()), cx);
            }
        };
        buttons
            .child(
                Button::primary(SharedString::from(format!("pet-tray-allow-{key}")), "Allow")
                    .small()
                    .on_click(answer("allow")),
            )
            .child(
                Button::secondary(SharedString::from(format!("pet-tray-deny-{key}")), "Deny")
                    .small()
                    .on_click(answer("deny")),
            )
    }
}

/// What the tray holds, in a few words: "1 needs you · 2 working · 1 done".
fn tray_summary(rows: &[companion::TrayRow]) -> String {
    let asks = rows.iter().filter(|row| row.ask.is_some()).count();
    let working = rows.iter().filter(|row| row.ask.is_none() && row.finished.is_none()).count();
    let done = rows.iter().filter(|row| row.finished.is_some()).count();
    let parts: Vec<String> =
        [(asks, if asks == 1 { "needs you" } else { "need you" }), (working, "working"), (done, "done")]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, word)| format!("{count} {word}"))
            .collect();
    parts.join(" · ")
}

/// How the tasks a row stands for went, in a line: "Done · Centred the pet", "Done · 3 tasks".
fn finished_line(finished: &companion::Finished) -> String {
    if finished.count > 1 {
        return format!("{} · {} tasks", if finished.status == "done" { "Done" } else { "Finished" }, finished.count);
    }
    let word = match finished.status.as_str() {
        "done" => "Done",
        "failed" => "Couldn’t finish",
        _ => "Stopped",
    };
    if finished.summary.trim().is_empty() { word.to_string() } else { format!("{word} · {}", finished.summary.trim()) }
}

impl Render for Pet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bubble = self.bubble(cx);
        if bubble.is_none() {
            self.bubble_bounds.set(None);
        }
        let tray = self.companion.read(cx).tray().to_vec();
        if tray.is_empty() {
            self.tray_bounds.set(None);
        }
        let bubble = bubble.map(|bubble| self.render_bubble(bubble, cx).into_any_element());
        let collapsed = companion::settings(cx).tray_collapsed;
        let tray = (!tray.is_empty()).then(|| {
            if collapsed {
                self.render_pill(&tray, cx).into_any_element()
            } else {
                self.render_tray(&tray, cx).into_any_element()
            }
        });
        // Above the pet the bubble sits over the tray, below it under the tray: the tray stays next to the pet.
        let (mut above, mut beside_below) = (Vec::new(), Vec::new());
        let (bubble_below, tray_below) = (self.layout.below, self.layout.tray_below);
        if let Some(bubble) = bubble {
            if bubble_below { beside_below.push(bubble) } else { above.push(bubble) }
        }
        let tuck = if tray.is_some() && !tray_below { TRAY_TUCK } else { 0. };
        if let Some(tray) = tray {
            if tray_below { beside_below.insert(0, tray) } else { above.push(tray) }
        }
        let pet_top = self.layout.pet_offset().1;
        let window_h = self.layout.frame.3 as f32;
        div()
            .size_full()
            .relative()
            .font_family(cx.theme().font_family())
            .child(
                div()
                    .absolute()
                    .left_0()
                    .w_full()
                    .bottom(px(window_h - pet_top - tuck))
                    .flex()
                    .flex_col()
                    .children(above),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .w_full()
                    .top(px(pet_top + PET as f32))
                    .flex()
                    .flex_col()
                    .children(beside_below),
            )
            .child(self.render_pet(cx))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// AppKit

fn ns_window(window: &Window) -> Option<usize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    // SAFETY: a read-only message to GPUI's own view, on the main thread.
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut AnyObject;
        let window: *mut AnyObject = msg_send![view, window];
        (!window.is_null()).then_some(window as usize)
    }
}

/// Borderless (a titled window draws a light edge along its top), no window shadow (the pet draws its own), and it
/// stays put when Filo hides.
fn prepare_window(ns_window: usize) {
    if ns_window == 0 {
        return;
    }
    // NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
    const BORDERLESS_PANEL: usize = 1 << 7;
    // SAFETY: AppKit messages on the main thread to a window GPUI keeps alive.
    unsafe {
        let window = ns_window as *mut AnyObject;
        let _: () = msg_send![window, setStyleMask: BORDERLESS_PANEL];
        let _: () = msg_send![window, setHasShadow: false];
        let _: () = msg_send![window, setHidesOnDeactivate: false];
    }
}

/// Keeps the pet out of screenshots (NSWindowSharingNone) or lets it back in. It's in the user's own; Filo's,
/// sent with "ask about your screen", leaves it out for as long as screencapture runs (screenshot.rs).
pub fn set_in_screenshots(shown: bool, cx: &mut App) {
    let Some(handle) = cx.try_global::<PetWindow>().map(|pet| pet.0) else { return };
    let Ok(ns_window) = handle.update(cx, |pet, _, _| pet.ns_window) else { return };
    if ns_window == 0 {
        return;
    }
    // NSWindowSharingNone, NSWindowSharingReadOnly
    let sharing: usize = if shown { 1 } else { 0 };
    // SAFETY: as set_frame.
    unsafe {
        let _: () = msg_send![ns_window as *mut AnyObject, setSharingType: sharing];
    }
}

/// Clicks go through the window to the app below (everywhere but the pet and its bubble).
fn set_ignores_mouse(ns_window: usize, ignore: bool) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: an AppKit message on the main thread; AppKit doesn't call back into GPUI for it.
    unsafe {
        let _: () = msg_send![ns_window as *mut AnyObject, setIgnoresMouseEvents: ignore];
    }
}

fn set_frame(ns_window: usize, (x, y, width, height): (f64, f64, f64, f64)) {
    if ns_window == 0 {
        return;
    }
    let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    // SAFETY: an AppKit message on the main thread, outside GPUI's event handling.
    unsafe {
        let _: () = msg_send![ns_window as *mut AnyObject, setFrame: rect, display: true];
    }
}

fn order_front(ns_window: usize) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: as set_frame.
    unsafe {
        let _: () = msg_send![ns_window as *mut AnyObject, orderFrontRegardless];
    }
}

fn move_frame(ns_window: usize, (x, y): (f64, f64)) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: as set_frame.
    unsafe {
        let _: () = msg_send![ns_window as *mut AnyObject, setFrameOrigin: NSPoint::new(x, y)];
    }
}

fn mouse_location() -> NSPoint {
    // SAFETY: a class method that only reads.
    unsafe { msg_send![class!(NSEvent), mouseLocation] }
}

fn mouse_down() -> bool {
    // SAFETY: as mouse_location.
    let buttons: usize = unsafe { msg_send![class!(NSEvent), pressedMouseButtons] };
    // Held during a driven drag (`FILO_PET_DRIVE`, `dev` builds).
    #[cfg(feature = "dev")]
    if drive::BUTTON.load(std::sync::atomic::Ordering::SeqCst) {
        return true;
    }
    buttons & 1 != 0
}

/// The usable part (no menu bar or Dock) of the screen that holds `point`.
fn screen_at(point: (f64, f64)) -> Option<NSRect> {
    // SAFETY: read-only AppKit messages on the main thread.
    unsafe {
        let screens: Retained<AnyObject> = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![&screens, count];
        (0..count).find_map(|index| {
            let screen: *mut AnyObject = msg_send![&screens, objectAtIndex: index];
            let frame: NSRect = msg_send![screen, frame];
            let inside = point.0 >= frame.origin.x
                && point.0 < frame.origin.x + frame.size.width
                && point.1 >= frame.origin.y
                && point.1 < frame.origin.y + frame.size.height;
            inside.then(|| msg_send![screen, visibleFrame])
        })
    }
}

fn main_screen() -> NSRect {
    // SAFETY: as screen_at.
    unsafe {
        let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if screen.is_null() {
            return NSRect::new(NSPoint::new(0., 0.), NSSize::new(1440., 900.));
        }
        msg_send![screen, visibleFrame]
    }
}

/// Bottom right of the main screen, clear of the Dock.
fn start_origin() -> (f64, f64) {
    let screen = main_screen();
    (screen.origin.x + screen.size.width - PET - START_INSET, screen.origin.y + START_INSET)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> NSRect {
        NSRect::new(NSPoint::new(0., 80.), NSSize::new(1440., 795.))
    }

    #[test]
    fn the_bubble_opens_toward_the_middle() {
        // Right, with room under it for the tray: the bubble reaches left, above the pet.
        let layout = Layout::new((1300., 500.), screen());
        assert!(layout.toward_left && !layout.below && layout.tray_below);
        assert_eq!(layout.frame, (1300. + PET - BUBBLE_W, 500. - TRAY_H, WINDOW_W, PET + BUBBLE_H + TRAY_H));
        assert_eq!(layout.pet_offset(), ((BUBBLE_W - PET) as f32, BUBBLE_H as f32));
        assert_eq!(layout.bubble_left(), 0.);
        // Top left: it reaches right, under the pet and its tray.
        let layout = Layout::new((20., 760.), screen());
        assert!(!layout.toward_left && layout.below && layout.tray_below);
        assert_eq!(layout.frame.1, 760. - BUBBLE_H - TRAY_H);
        assert_eq!(layout.pet_offset(), (TRAY_SIDE as f32, 0.));
        // The bubble starts at the pet's left edge.
        assert_eq!(layout.bubble_left(), layout.pet_offset().0);
    }

    /// The tray's middle and the pet's, on the screen.
    fn tray_and_pet_middles(layout: &Layout) -> (f64, f64) {
        (layout.frame.0 + f64::from(layout.tray_left) + f64::from(TRAY_W) / 2., layout.origin.0 + PET / 2.)
    }

    #[test]
    fn the_tray_is_centred_on_the_pet() {
        // Anywhere it has room: both halves of the screen, high and low (the tray above or below it).
        for x in [60., 300., 640., 700., 900., 1200.] {
            for y in [100., 400., 700.] {
                let layout = Layout::new((x, y), screen());
                let (tray, pet) = tray_and_pet_middles(&layout);
                assert_eq!(tray, pet, "at {x}, {y}");
                // Inside the window, with room for its shadow.
                assert!(
                    layout.tray_left >= BUBBLE_MARGIN
                        && layout.tray_left + TRAY_W <= layout.frame.2 as f32 - BUBBLE_MARGIN,
                    "at {x}, {y}"
                );
            }
        }
    }

    #[test]
    fn at_the_screen_edge_the_tray_moves_in_only_as_far_as_it_has_to() {
        let screen = screen();
        let right = screen.origin.x + screen.size.width;
        // Against the left edge: the tray's left edge is the screen's.
        let layout = Layout::new((0., 400.), screen);
        assert_eq!(layout.frame.0 + f64::from(layout.tray_left), 0.);
        // Against the right edge: its right edge is the screen's.
        let layout = Layout::new((right - PET, 400.), screen);
        assert_eq!(layout.frame.0 + f64::from(layout.tray_left + TRAY_W), right);
        // Just far enough from the edge: centred again.
        let layout = Layout::new((f64::from(TRAY_W) / 2. - PET / 2., 400.), screen);
        let (tray, pet) = tray_and_pet_middles(&layout);
        assert_eq!(tray, pet);
        // On a second screen to the left, the same.
        let left_screen = NSRect::new(NSPoint::new(-1280., 0.), NSSize::new(1280., 800.));
        let layout = Layout::new((-PET, 400.), left_screen);
        assert_eq!(layout.frame.0 + f64::from(layout.tray_left + TRAY_W), 0.);
        let layout = Layout::new((-640., 400.), left_screen);
        let (tray, pet) = tray_and_pet_middles(&layout);
        assert_eq!(tray, pet);
    }

    #[test]
    fn the_shadow_sits_under_the_feet() {
        // A 4×10 picture whose feet end three rows up, with a faint shadow of its own under them.
        let mut picture = image::RgbaImage::new(4, 10);
        for row in 2..7 {
            picture.put_pixel(1, row, image::Rgba([255, 255, 255, 255]));
        }
        picture.put_pixel(1, 8, image::Rgba([0, 0, 0, 40]));
        let mut bytes = Vec::new();
        picture.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        assert_eq!(feet_rise(&bytes), Some(0.3));
        assert_eq!(feet_rise(b"not a picture"), None);
        // Every animal's pose has feet, in the bottom quarter of its picture.
        for animal in crate::avatars::ANIMALS.iter() {
            for (pose, _) in animal.poses {
                let id = animal.avatar_id(pose);
                let rise = feet_rise(&crate::assets::avatar_bytes(&id).unwrap()).unwrap();
                assert!((0. ..0.25).contains(&rise), "{id}: {rise}");
            }
        }
        // The shadow's middle is just above where the feet end, inside the pet's square.
        let feet = PICTURE_BOTTOM + 0.1 * PICTURE;
        let middle = shadow_bottom(0.1) + SHADOW_H / 2.;
        assert!(middle > feet && middle - feet <= 2.);
        assert!(shadow_bottom(0.) >= 0.);
    }

    #[test]
    fn the_tray_goes_under_the_pet_unless_it_sits_low() {
        // Where the pet starts (the bottom right corner): the tray goes above it, and the bubble above the tray.
        let layout = Layout::new((1300., 80. + START_INSET), screen());
        assert!(!layout.tray_below && !layout.below);
        assert_eq!(layout.frame.1, 80. + START_INSET);
        assert_eq!(layout.pet_offset().1, (BUBBLE_H + TRAY_H) as f32);
        // Centred under the pet, inside the window.
        let left = layout.tray_left;
        assert!(left >= BUBBLE_MARGIN && left + TRAY_W <= WINDOW_W as f32 - BUBBLE_MARGIN);
        let layout = Layout::new((20., 500.), screen());
        assert!(layout.tray_below);
        // The pet, its tray and the most rows it lists fit in the room kept (each waiting on the user, its title in two
        // lines).
        assert!(tray_title_line() <= TRAY_WRAP_ROOM);
        assert!(
            f64::from(TRAY_TOP)
                + f64::from(TRAY_ASK_H + tray_title_line() + TRAY_GAP) * TRAY_ROWS as f64
                + f64::from(TRAY_MORE_H)
                + f64::from(space::XS)
                + 1.
                + 4.
                <= TRAY_H
        );
    }

    #[test]
    fn the_pointer_picks_a_tray_row() {
        let layout = Layout::new((1300., 300.), screen());
        // The window's top-left corner on the screen is (0, 0) in the window.
        assert_eq!(layout.to_window(NSPoint::new(layout.frame.0, layout.frame.1 + layout.frame.3)), (0., 0.));
        let tray = Bounds::new(point(px(40.), px(320.)), size(px(TRAY_W), px(200.)));
        let top = 320. + TRAY_TOP;
        // A row waiting on the user (taller, with its buttons), then two others.
        let heights = [TRAY_ASK_H, TRAY_ROW_H, TRAY_ROW_H];
        assert_eq!(tray_row(tray, &heights, (50., top + 2.)), Some(0));
        assert_eq!(tray_row(tray, &heights, (50., top + TRAY_ASK_H - 1.)), Some(0));
        assert_eq!(tray_row(tray, &heights, (50., top + TRAY_ASK_H + TRAY_GAP + 1.)), Some(1));
        assert_eq!(tray_row(tray, &heights, (50., top + TRAY_ASK_H + TRAY_ROW_H + TRAY_GAP * 2. + 1.)), Some(2));
        // Between two rows, past the last one, in the padding, or beside the tray: none.
        assert_eq!(tray_row(tray, &heights, (50., top + TRAY_ASK_H + TRAY_GAP / 2.)), None);
        assert_eq!(tray_row(tray, &heights, (50., top + TRAY_ASK_H + TRAY_ROW_H * 2. + TRAY_GAP * 2. + 1.)), None);
        assert_eq!(tray_row(tray, &heights, (50., 321.)), None);
        // On the header: no row.
        assert_eq!(tray_row(tray, &heights, (50., top - TRAY_HEAD_H / 2.)), None);
        assert_eq!(tray_row(tray, &heights, (20., top + 2.)), None);
    }

    #[test]
    fn a_restored_pet_stays_on_the_screen_it_now_has() {
        let visible = NSRect::new(NSPoint::new(0., 80.), NSSize::new(1440., 795.));
        let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1440., 900.));
        let visible_at = |point: (f64, f64)| {
            inside(
                NSPoint::new(point.0, point.1),
                (frame.origin.x, frame.origin.y, frame.size.width, frame.size.height),
            )
            .then_some(visible)
        };
        // Where it was left, unchanged.
        assert_eq!(restored_origin(Some((600., 400.)), visible_at).map(|(origin, _)| origin), Some((600., 400.)));
        // Left on a bigger display (or before the Dock grew): its middle is still on this one, so it comes fully onto
        // its usable part.
        assert_eq!(
            restored_origin(Some((1380., 840.)), visible_at).map(|(origin, _)| origin),
            Some((1440. - PET, 875. - PET))
        );
        assert_eq!(restored_origin(Some((-60., 20.)), visible_at).map(|(origin, _)| origin), Some((0., 80.)));
        // On a display that's gone: the corner it starts in.
        assert_eq!(restored_origin(Some((3000., 400.)), visible_at), None);
        assert_eq!(restored_origin(None, visible_at), None);
        // A screen smaller than the pet keeps its corner on the screen.
        let tiny = NSRect::new(NSPoint::new(10., 10.), NSSize::new(100., 100.));
        assert_eq!(clamp_onto((500., 500.), tiny), (10., 10.));
    }

    #[test]
    fn clicks_go_to_what_the_pet_draws() {
        let layout = Layout::new((1300., 300.), screen());
        let tray = Bounds::new(point(px(78.), px(BUBBLE_H as f32 + PET as f32 + 2.)), size(px(TRAY_W), px(64.)));
        let tray_screen = layout.to_screen(tray);
        let on_tray = NSPoint::new(tray_screen.0 + 5., tray_screen.1 + 5.);
        let beside_tray = NSPoint::new(layout.frame.0 + 2., tray_screen.1 + 5.);
        assert!(takes_clicks(&layout, None, Some(tray), on_tray));
        assert!(!takes_clicks(&layout, None, Some(tray), beside_tray));
        // No tray (nothing running): its spot lets clicks through.
        assert!(!takes_clicks(&layout, None, None, on_tray));
        assert!(takes_clicks(&layout, None, None, NSPoint::new(1350., 350.)));
    }

    #[test]
    fn the_pet_and_its_bubble_take_clicks() {
        let layout = Layout::new((1300., 300.), screen());
        assert!(layout.over_pet(NSPoint::new(1350., 350.)));
        assert!(!layout.over_pet(NSPoint::new(1250., 350.)));
        // The bubble's top-left corner in the window is the top-left of the window on screen.
        let bubble = Bounds::new(point(px(10.), px(0.)), size(px(280.), px(100.)));
        assert_eq!(
            layout.to_screen(bubble),
            (layout.frame.0 + 10., layout.frame.1 + layout.frame.3 - 100., 280., 100.)
        );
    }
}

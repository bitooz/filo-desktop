// `FILO_PET_DRIVE="step;step;…"`: drives the pet as a person would, to check it with no hand on the mouse. The real
// pointer moves (CGWarpMouseCursorPosition) and presses reach the pet's window as AppKit mouse events, so GPUI's own
// hit-testing, hover and handlers run; only "the button is held" is pretended during a drag (posting real input needs
// Accessibility, which a coding tool's terminal usually lacks). Each step is logged as `pet drive: …`. Steps:
//   wait-row[:secs]    until the tray lists a row (a coding task or a chat; 2 minutes)
//   wait-new[:secs]    until a row the tray didn't list at this step joins it (2 minutes)
//   wait-gone[:secs]   until that new row leaves the tray, or with none, until the tray is empty (5 minutes)
//   hover[:ms]         the pointer on the pet, which brings up its status bubble (1.8 s)
//   away               the pointer off the pet
//   drag:dx,dy         press the pet, move it by (dx, dy) points (y up), let go
//   drag-to:x,y        the same, to where its bottom-left corner should go (kept on screen as a drop is)
//   through            the pointer beside the tray, then on it, each with a mouse move posted to Filo (so its own
//                      event monitor runs): whether the window lets clicks through, and whether the monitor switched it
//   click-row:n|new    the pointer on the tray's nth row (or the new chat's), then a click
//   lines              each tray row's heading as it's cut to fit, and the row's height
//   reply:long|short   a reply's bubble from an agent, with a long or a short message, as one coming in shows it
//   shot:name          a screenshot of the pet's window, /tmp/pet-drive-<name>.png
//   wait:ms

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, Task, Window};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect};

use super::{PET, Pet, mouse_location};

/// The button held during a driven drag.
pub(super) static BUTTON: AtomicBool = AtomicBool::new(false);

/// The pretended press, let go when dropped: a drag cut short (the pet closed, sign-out) lets go of it too.
struct Held;

impl Held {
    fn press() -> Held {
        BUTTON.store(true, Ordering::SeqCst);
        Held
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        BUTTON.store(false, Ordering::SeqCst);
    }
}

pub(super) fn steps() -> Option<Vec<String>> {
    let script = std::env::var("FILO_PET_DRIVE").ok().filter(|script| !script.trim().is_empty())?;
    Some(script.split(';').map(|step| step.trim().to_string()).filter(|step| !step.is_empty()).collect())
}

fn log(text: impl AsRef<str>) {
    eprintln!("pet drive: {}", text.as_ref());
}

pub(super) fn start(steps: Vec<String>, window: &mut Window, cx: &mut Context<Pet>) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        let sleep = async |cx: &mut gpui::AsyncWindowContext, ms: u64| cx.background_executor().timer(Duration::from_millis(ms)).await;
        sleep(cx, 1500).await;
        // The chat wait-new saw join the tray.
        let mut new_row: Option<String> = None;
        for step in steps {
            let (name, arg) = step.split_once(':').map_or((step.as_str(), None), |(name, arg)| (name, Some(arg)));
            let numbers: Vec<f64> = arg.map(|arg| arg.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
            log(format!("step {step}"));
            let Ok(state) = this.update(cx, |pet, cx| State::of(pet, cx)) else { return };
            match name {
                "wait-row" | "wait-new" | "wait-gone" => {
                    let limit = Duration::from_secs_f64(numbers.first().copied().unwrap_or(if name == "wait-gone" { 300. } else { 120. }));
                    let before: Vec<String> = state.rows.iter().map(|row| row.0.clone()).collect();
                    let started = Instant::now();
                    let mut last = state.rows.clone();
                    log(format!("tray rows: {last:?}"));
                    loop {
                        let Ok(state) = this.update(cx, |pet, cx| State::of(pet, cx)) else { return };
                        if state.rows != last {
                            log(format!("tray rows after {:.1}s: {:?}", started.elapsed().as_secs_f64(), state.rows));
                            last = state.rows.clone();
                        }
                        let listed = |id: &str| state.rows.iter().any(|row| row.0 == id);
                        let done = match name {
                            "wait-row" => !state.rows.is_empty(),
                            "wait-new" => match state.rows.iter().find(|row| !before.contains(&row.0)) {
                                Some(row) => {
                                    log(format!("new chat in the tray: {row:?}"));
                                    new_row = Some(row.0.clone());
                                    true
                                }
                                None => false,
                            },
                            _ => match &new_row {
                                Some(id) => !listed(id),
                                None => state.rows.is_empty(),
                            },
                        };
                        if done {
                            log(format!("{name} met after {:.1}s", started.elapsed().as_secs_f64()));
                            break;
                        }
                        if started.elapsed() > limit {
                            log("gave up waiting");
                            break;
                        }
                        sleep(cx, 250).await;
                    }
                }
                "hover" => {
                    warp(state.pet_center());
                    sleep(cx, numbers.first().copied().unwrap_or(1800.) as u64).await;
                    let Ok(state) = this.update(cx, |pet, cx| State::of(pet, cx)) else { return };
                    log(format!("bubble up: {}, tray rows: {}", state.bubble.is_some(), state.rows.len()));
                    state.log_places();
                }
                "away" => {
                    let (x, _) = state.pet_center();
                    let screen = super::main_screen();
                    let x = if x > screen.origin.x + screen.size.width / 2. { screen.origin.x + 200. } else { screen.origin.x + screen.size.width - 200. };
                    warp((x, screen.origin.y + screen.size.height / 2.));
                    sleep(cx, 600).await;
                }
                "drag" | "drag-to" => {
                    let (dx, dy) = match (name, numbers.as_slice()) {
                        ("drag", [dx, dy, ..]) => (*dx, *dy),
                        ("drag-to", [x, y, ..]) => (x - state.origin.0, y - state.origin.1),
                        _ => {
                            log("needs two numbers");
                            continue;
                        }
                    };
                    let start = state.pet_center();
                    warp(start);
                    sleep(cx, 300).await;
                    send_mouse(state.ns_window, true, start);
                    let held = Held::press();
                    sleep(cx, 120).await;
                    let moves = 45;
                    for index in 1..=moves {
                        let t = f64::from(index) / f64::from(moves);
                        warp((start.0 + dx * t, start.1 + dy * t));
                        sleep(cx, 22).await;
                    }
                    sleep(cx, 150).await;
                    drop(held);
                    let end = mouse_location();
                    send_mouse(state.ns_window, false, (end.x, end.y));
                    sleep(cx, 700).await;
                    let Ok(after) = this.update(cx, |pet, cx| State::of(pet, cx)) else { return };
                    log(format!("pet moved from {:?} to {:?}", state.origin, after.origin));
                    after.log_places();
                }
                "through" => {
                    let Some(tray) = state.tray else {
                        log("no tray");
                        continue;
                    };
                    let middle = tray.1 + tray.3 / 2.;
                    // Inside the pet's window, left or right of the tray (whichever side has room), clear of the pet.
                    let beside = if tray.0 - state.frame.0 > 4. { state.frame.0 + 3. } else { state.frame.0 + state.frame.2 - 3. };
                    for (label, point) in [("beside the tray", (beside, middle)), ("on the tray", (tray.0 + tray.2 / 2., middle))] {
                        let switches = super::MONITOR_SWITCHES.load(Ordering::Relaxed);
                        warp(point);
                        post_mouse_moved(point);
                        // Shorter than the pointer poll's 40 ms: a switch by then is the monitor's.
                        sleep(cx, 12).await;
                        log(format!("pointer {label}, 12 ms after a mouse move: clicks go through: {}, monitor switched it: {}", ignores_mouse(state.ns_window), super::MONITOR_SWITCHES.load(Ordering::Relaxed) > switches));
                        sleep(cx, 400).await;
                        let inside_window = point.0 >= state.frame.0 && point.0 < state.frame.0 + state.frame.2 && point.1 >= state.frame.1 && point.1 < state.frame.1 + state.frame.3;
                        log(format!("pointer {label} at {point:?} (inside the pet's window: {inside_window}): clicks go through: {}", ignores_mouse(state.ns_window)));
                    }
                }
                "click-row" => {
                    let index = match arg {
                        Some("new") => match new_row.as_ref().and_then(|id| state.rows.iter().position(|row| &row.0 == id)) {
                            Some(index) => index,
                            None => {
                                log("the new chat isn't in the tray");
                                continue;
                            }
                        },
                        _ => numbers.first().copied().unwrap_or(0.) as usize,
                    };
                    let Some(tray) = state.tray else {
                        log("no tray");
                        continue;
                    };
                    let Some(row) = state.rows.get(index).filter(|_| index < super::TRAY_ROWS) else {
                        log(format!("no row {index}"));
                        continue;
                    };
                    // Row centres: from the tray's top, past its border and padding (render_tray).
                    let above: f32 = state.heights[..index].iter().map(|height| height + super::TRAY_GAP).sum();
                    let from_top = f64::from(super::TRAY_TOP + above + state.heights[index] / 2.);
                    let point = (tray.0 + tray.2 / 2., tray.1 + tray.3 - from_top);
                    warp(point);
                    sleep(cx, 900).await;
                    let Ok(hovered) = this.update(cx, |pet, _| pet.tray_hover) else { return };
                    log(format!("pointer on row {index} ({row:?}), highlighted row: {hovered:?}"));
                    send_mouse(state.ns_window, true, point);
                    sleep(cx, 80).await;
                    send_mouse(state.ns_window, false, point);
                    log(format!("clicked row {index}: {}", row.0));
                }
                "shot" => {
                    // Only the pet's window (no shadow), not the rest of the screen.
                    let dir = std::env::var("FILO_DRIVE_SHOTS").ok().filter(|dir| !dir.is_empty()).unwrap_or("/tmp".into());
                    let path = format!("{dir}/pet-drive-{}.png", arg.unwrap_or("shot"));
                    let window: isize = if state.ns_window == 0 {
                        0
                    } else {
                        // SAFETY: an AppKit message on the main thread to a window GPUI keeps alive.
                        unsafe { msg_send![state.ns_window as *mut AnyObject, windowNumber] }
                    };
                    let saved = crate::window_shot::save(window, &path).await;
                    log(format!("screenshot {path}: saved {saved}"));
                }
                "lines" => {
                    let Ok(rows) = this.update(cx, |pet, cx| {
                        let now = crate::format::now_ms();
                        let tray = pet.companion.read(cx).tray().to_vec();
                        let heights = pet.tray_row_heights(&tray, cx);
                        tray.iter().zip(heights).map(|(row, height)| (row.key.clone(), pet.tray_heading(row, now, cx), height)).collect::<Vec<_>>()
                    }) else {
                        return;
                    };
                    for (key, (lines, time, width), height) in rows {
                        log(format!("row {key} ({height} high): {lines:?} in {width} beside {time:?}"));
                    }
                }
                "reply" => {
                    let body = match arg {
                        Some("short") => "Done: the tests pass.",
                        _ => "The zoomed view is sharper now (commit 488fc6c). I compared the before and after crops at 256%. The text that blurred before reads cleanly, and pictures past 16384 px are kept whole in tiles.",
                    };
                    let reply = crate::companion::Reply { session_id: "pet-drive".into(), title: "Desktop App Developer".into(), subtitle: None, body: body.into(), agent: true };
                    this.update_in(cx, |pet, window, cx| pet.show_reply(reply, window, cx)).ok();
                    sleep(cx, 400).await;
                }
                "wait" => sleep(cx, numbers.first().copied().unwrap_or(1000.) as u64).await,
                _ => log(format!("unknown step {step}")),
            }
        }
        log("done");
    })
}

/// What the driver reads from the pet, in AppKit screen points.
struct State {
    ns_window: usize,
    origin: (f64, f64),
    frame: (f64, f64, f64, f64),
    tray: Option<(f64, f64, f64, f64)>,
    bubble: Option<(f64, f64, f64, f64)>,
    /// (task or chat id, title)
    rows: Vec<(String, String)>,
    /// The tray's row heights (`Pet::tray_row_heights`).
    heights: Vec<f32>,
}

impl State {
    fn of(pet: &Pet, cx: &gpui::App) -> State {
        let tray = pet.companion.read(cx).tray();
        let rows = tray.iter().map(|row| (row.key.clone(), row.title.clone())).collect::<Vec<_>>();
        let heights = pet.tray_row_heights(tray, cx);
        State {
            ns_window: pet.ns_window,
            origin: pet.layout.origin,
            frame: pet.layout.frame,
            tray: pet.tray_bounds.get().filter(|_| !rows.is_empty()).map(|bounds| pet.layout.to_screen(bounds)),
            bubble: pet
                .bubble_bounds
                .get()
                .filter(|_| pet.bubble(cx).is_some())
                .map(|bounds| pet.layout.to_screen(bounds)),
            rows,
            heights,
        }
    }

    fn pet_center(&self) -> (f64, f64) {
        (self.origin.0 + PET / 2., self.origin.1 + PET / 2.)
    }

    /// Where the pet, tray and bubble are, and whether any of them is off the screen.
    fn log_places(&self) {
        let screen = super::screen_at(self.pet_center()).unwrap_or_else(super::main_screen);
        let on_screen = |(x, y, width, height): (f64, f64, f64, f64)| {
            x >= screen.origin.x - 0.5
                && y >= screen.origin.y - 0.5
                && x + width <= screen.origin.x + screen.size.width + 0.5
                && y + height <= screen.origin.y + screen.size.height + 0.5
        };
        let pet = (self.origin.0, self.origin.1, PET, PET);
        let side = |rect: (f64, f64, f64, f64)| {
            if rect.1 >= pet.1 + PET - 1. {
                "above the pet"
            } else if rect.1 + rect.3 <= pet.1 + 1. {
                "under the pet"
            } else {
                "beside the pet"
            }
        };
        log(format!("screen {:?}", rect_of(screen)));
        log(format!("pet {pet:?} on screen: {}", on_screen(pet)));
        if let Some(tray) = self.tray {
            log(format!("tray {tray:?} {} on screen: {}", side(tray), on_screen(tray)));
        }
        if let Some(bubble) = self.bubble {
            log(format!("bubble {bubble:?} {} on screen: {}", side(bubble), on_screen(bubble)));
        }
    }
}

fn rect_of(rect: NSRect) -> (f64, f64, f64, f64) {
    (rect.origin.x, rect.origin.y, rect.size.width, rect.size.height)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
}

/// Moves the real pointer to an AppKit screen point (y up from the bottom of the main display).
fn warp((x, y): (f64, f64)) {
    // SAFETY: read-only AppKit messages on the main thread, then a CoreGraphics call that takes a plain point.
    unsafe {
        let screens: objc2::rc::Retained<AnyObject> = msg_send![class!(NSScreen), screens];
        let main: *mut AnyObject = msg_send![&screens, objectAtIndex: 0usize];
        let frame: NSRect = msg_send![main, frame];
        CGWarpMouseCursorPosition(CGPoint { x, y: frame.size.height - y });
    }
}

/// A left press or release at a screen point, handed to the pet's window as AppKit would.
fn send_mouse(ns_window: usize, down: bool, (x, y): (f64, f64)) {
    if ns_window == 0 {
        return;
    }
    // SAFETY: AppKit messages on the main thread, outside any GPUI update, to a window GPUI keeps alive.
    unsafe {
        let window = ns_window as *mut AnyObject;
        let frame: NSRect = msg_send![window, frame];
        let location = NSPoint::new(x - frame.origin.x, y - frame.origin.y);
        let number: isize = msg_send![window, windowNumber];
        let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let uptime: f64 = msg_send![info, systemUptime];
        // NSEventTypeLeftMouseDown, NSEventTypeLeftMouseUp
        let kind: usize = if down { 1 } else { 2 };
        let event: *mut AnyObject = msg_send![
            class!(NSEvent),
            mouseEventWithType: kind,
            location: location,
            modifierFlags: 0usize,
            timestamp: uptime,
            windowNumber: number,
            context: std::ptr::null_mut::<AnyObject>(),
            eventNumber: 0isize,
            clickCount: 1isize,
            pressure: 1.0f32
        ];
        if !event.is_null() {
            let _: () = msg_send![window, sendEvent: event];
        }
    }
}

/// A mouse move at a screen point, queued to Filo itself as AppKit would deliver one (its local monitors see it).
fn post_mouse_moved((x, y): (f64, f64)) {
    // SAFETY: AppKit messages on the main thread; the event is queued, not handled here.
    unsafe {
        let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let uptime: f64 = msg_send![info, systemUptime];
        // NSEventTypeMouseMoved, in screen coordinates (no window).
        let event: *mut AnyObject = msg_send![
            class!(NSEvent),
            mouseEventWithType: 5usize,
            location: NSPoint::new(x, y),
            modifierFlags: 0usize,
            timestamp: uptime,
            windowNumber: 0isize,
            context: std::ptr::null_mut::<AnyObject>(),
            eventNumber: 0isize,
            clickCount: 0isize,
            pressure: 0.0f32
        ];
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if !event.is_null() {
            let _: () = msg_send![app, postEvent: event, atStart: false];
        }
    }
}

fn ignores_mouse(ns_window: usize) -> bool {
    // SAFETY: a read-only AppKit message on the main thread.
    ns_window != 0 && unsafe { msg_send![ns_window as *mut AnyObject, ignoresMouseEvents] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_drag_lets_go_of_the_button() {
        // A drag task dropped mid-move (the pet closed): its press goes with it.
        let mut drag = Box::pin(async {
            let _held = Held::press();
            std::future::pending::<()>().await;
        });
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(drag.as_mut().poll(&mut context).is_pending());
        assert!(BUTTON.load(Ordering::SeqCst));
        drop(drag);
        assert!(!BUTTON.load(Ordering::SeqCst));
    }
}

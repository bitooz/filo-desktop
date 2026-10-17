// ⌘Q twice to quit. The first ⌘Q doesn't quit: a small prompt, "Press ⌘Q again to quit", shows near the top of the
// screen (and VoiceOver reads it out); a second ⌘Q within two seconds quits. A key held down doesn't count twice (macOS
// marks its repeats, and they're ignored), the prompt goes after two seconds or at any other key, and the next ⌘Q
// starts over. Choosing Quit in a menu (Filo's menu, the menu bar item, the pet) is deliberate already and quits at
// once, as browsers do with their ⌘Q warning; so does closing the window when Filo quits on that. Quitting from outside
// (the Dock, logging out, shutting down) isn't held up: GPUI quits straight away. Either way, quitting applies "Keep
// coding after Filo quits" (local_backend/workers.rs): on, running turns and coding tasks carry on; off, they stop.
//
// `FILO_QUIT_DRIVE="wait:1500,press,wait:300,hold:900,…"` presses ⌘Q from a script, as real key events posted to the app
// (NSEvent, through AppKit's own event handling, so the menu and key bindings see them as typed), and logs what the
// guard decides: for checking it without a hand on the keyboard. Steps: `press`, `hold:<ms>` (with key repeats),
// `key:<letter>`, `menu` (Quit Filo in Filo's menu), `terminate` (NSApp terminate:, what the Dock's Quit, logging out
// and shutting down send), `close-window` (the main window's close button), `update-restart` (what the updater does
// before it relaunches), `wait:<ms>`. In `dev` builds only.

#![allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, Global, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div,
    point, px, size,
};

use crate::theme::{ActiveTheme as _, Typography as _, radius, space};

/// How long after the first ⌘Q the second one quits.
pub const WINDOW: Duration = Duration::from_secs(2);
/// A ⌘Q event older than this when its action arrives didn't come from the keyboard just now (a menu item run later).
const KEY_EVENT_AGE: f64 = 0.5;
const PROMPT: &str = "Press ⌘Q again to quit";
const HUD_W: f32 = 300.;
const HUD_H: f32 = 64.;

/// What one ⌘Q does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Press {
    /// The first: show the prompt.
    Prompt,
    /// The second within WINDOW: quit.
    Quit,
    /// A key repeat from holding ⌘Q down.
    Ignored,
}

/// The double press, on its own clock so it can be tested.
#[derive(Debug, Default)]
pub struct Guard {
    armed_at: Option<Instant>,
}

impl Guard {
    pub fn press(&mut self, now: Instant, repeat: bool) -> Press {
        if repeat {
            return Press::Ignored;
        }
        match self.armed_at {
            Some(at) if now.saturating_duration_since(at) <= WINDOW => {
                self.armed_at = None;
                Press::Quit
            }
            // Never armed, or armed too long ago: this one starts over.
            _ => {
                self.armed_at = Some(now);
                Press::Prompt
            }
        }
    }

    /// Takes the prompt back (it ran out, another key came); true when it was showing.
    pub fn cancel(&mut self) -> bool {
        self.armed_at.take().is_some()
    }

    pub fn armed(&self, now: Instant) -> bool {
        self.armed_at.is_some_and(|at| now.saturating_duration_since(at) <= WINDOW)
    }
}

struct QuitGuard {
    guard: Guard,
    quitting: bool,
    hud: Option<WindowHandle<Hud>>,
    /// Takes the prompt down when WINDOW runs out; replaced on each ⌘Q.
    expiry: Option<Task<()>>,
    _keys: Option<Subscription>,
}

impl Global for QuitGuard {}

pub fn init(cx: &mut App) {
    // Any other key takes the prompt back.
    let keys = cx.observe_keystrokes(|event, _, cx| {
        let keystroke = &event.keystroke;
        let modifier_only = matches!(
            keystroke.key.as_str(),
            "cmd" | "shift" | "alt" | "ctrl" | "control" | "fn" | "function" | "platform"
        );
        let quit_key = keystroke.key == "q" && keystroke.modifiers.platform;
        if modifier_only || quit_key || !cx.has_global::<QuitGuard>() {
            return;
        }
        let state = cx.global_mut::<QuitGuard>();
        if !state.quitting && state.guard.cancel() {
            eprintln!("quit: another key: the prompt goes");
            close_hud(cx);
        }
    });
    cx.set_global(QuitGuard { guard: Guard::default(), quitting: false, hud: None, expiry: None, _keys: Some(keys) });
    #[cfg(feature = "dev")]
    if let Ok(script) = std::env::var("FILO_QUIT_DRIVE") {
        drive(script, cx);
    }
}

/// The Quit action (⌘Q, or Quit Filo in Filo's menu): which one it was decides.
pub fn quit_requested(cx: &mut App) {
    match current_source() {
        Source::Key { repeat } => key_press(repeat, cx),
        Source::Other => {
            eprintln!("quit: Quit chosen in a menu");
            quit_deliberately(cx);
        }
    }
}

fn key_press(repeat: bool, cx: &mut App) {
    let state = cx.global_mut::<QuitGuard>();
    if state.quitting {
        eprintln!("quit: ⌘Q while quitting: nothing more to do");
        return;
    }
    match state.guard.press(Instant::now(), repeat) {
        Press::Ignored => eprintln!("quit: ⌘Q held down (a key repeat): ignored"),
        Press::Prompt => {
            eprintln!("quit: ⌘Q: \"{PROMPT}\" for {} s", WINDOW.as_secs());
            show_hud(PROMPT, cx);
            announce(PROMPT);
            let expiry = cx.spawn(async move |cx| {
                cx.background_executor().timer(WINDOW + Duration::from_millis(50)).await;
                cx.update(|cx| {
                    let state = cx.global_mut::<QuitGuard>();
                    if !state.quitting && !state.guard.armed(Instant::now()) && state.guard.cancel() {
                        eprintln!("quit: no second ⌘Q in {} s: the prompt goes", WINDOW.as_secs());
                        close_hud(cx);
                    }
                })
                .ok();
            });
            cx.global_mut::<QuitGuard>().expiry = Some(expiry);
        }
        Press::Quit => {
            eprintln!("quit: ⌘Q again within {} s", WINDOW.as_secs());
            quit_deliberately(cx);
        }
    }
}

/// Quits now, the way Filo means to.
pub fn quit_deliberately(cx: &mut App) {
    if cx.has_global::<QuitGuard>() {
        let state = cx.global_mut::<QuitGuard>();
        if state.quitting {
            return;
        }
        state.quitting = true;
        state.guard.cancel();
        state.expiry = None;
    }
    eprintln!("quit: quitting");
    close_hud(cx);
    cx.defer(|cx| cx.quit());
}

// ---------------------------------------------------------------------------------------------------------------
// Where the Quit action came from

enum Source {
    /// ⌘Q on the keyboard; `repeat` when it's macOS repeating a held key.
    Key { repeat: bool },
    /// A menu item, or anything else.
    Other,
}

/// The event AppKit is handling as the action arrives: a fresh ⌘Q key-down means the keyboard (through GPUI's key
/// bindings, or the menu's key equivalent when no window has the keyboard); anything else a click.
fn current_source() -> Source {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: read-only AppKit messages on the main thread; the event belongs to AppKit.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut AnyObject = msg_send![app, currentEvent];
        if event.is_null() {
            return Source::Other;
        }
        let kind: usize = msg_send![event, type];
        // NSEventTypeKeyDown
        if kind != 10 {
            return Source::Other;
        }
        let flags: usize = msg_send![event, modifierFlags];
        let timestamp: f64 = msg_send![event, timestamp];
        let process: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let uptime: f64 = msg_send![process, systemUptime];
        // NSEventModifierFlagCommand
        if flags & (1 << 20) == 0 || uptime - timestamp > KEY_EVENT_AGE {
            return Source::Other;
        }
        let repeat: bool = msg_send![event, isARepeat];
        Source::Key { repeat }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The prompt: a small card near the top of the screen, above every window, that never takes the keyboard

struct Hud {
    text: SharedString,
}

impl Render for Hud {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        div().size_full().flex().items_center().justify_center().font_family(cx.theme().font_family()).child(
            div()
                .px(px(space::LG))
                .py(px(space::SM))
                .rounded(px(radius::MD))
                .bg(colors.raised)
                .border_1()
                .border_color(colors.border)
                .shadow_md()
                .subhead()
                .semibold()
                .text_color(colors.text)
                .child(self.text.clone()),
        )
    }
}

fn show_hud(text: &'static str, cx: &mut App) {
    if let Some(handle) = cx.global::<QuitGuard>().hud {
        let shown = handle.update(cx, |hud, _, cx| {
            hud.text = text.into();
            cx.notify();
        });
        if shown.is_ok() {
            return;
        }
    }
    let display = cx.primary_display().map(|display| display.bounds());
    let origin = match display {
        Some(screen) => {
            point(screen.origin.x + (screen.size.width - px(HUD_W)) / 2., screen.origin.y + screen.size.height * 0.16)
        }
        None => point(px(0.), px(0.)),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size(px(HUD_W), px(HUD_H))))),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| {
        prepare_window(window);
        cx.new(|_| Hud { text: text.into() })
    }) {
        Ok(handle) => cx.global_mut::<QuitGuard>().hud = Some(handle),
        Err(error) => eprintln!("quit: couldn't show the prompt: {error:#}"),
    }
}

/// Like the pet's window (pet.rs): borderless with no window shadow (the card draws its own), staying up while Filo is
/// in the background, and clicks go through it.
fn prepare_window(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
    const BORDERLESS_PANEL: usize = 1 << 7;
    // SAFETY: AppKit messages on the main thread to the window GPUI just made and keeps alive.
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut AnyObject;
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setStyleMask: BORDERLESS_PANEL];
        let _: () = msg_send![ns_window, setHasShadow: false];
        let _: () = msg_send![ns_window, setHidesOnDeactivate: false];
        let _: () = msg_send![ns_window, setIgnoresMouseEvents: true];
        // For FILO_QUIT_DRIVE runs: the prompt's window number in the log, which shows it came up as its own window.
        #[cfg(feature = "dev")]
        if std::env::var_os("FILO_QUIT_DRIVE").is_some() {
            let number: isize = msg_send![ns_window, windowNumber];
            eprintln!("quit: prompt window {number}");
        }
    }
}

fn close_hud(cx: &mut App) {
    if let Some(handle) = cx.global_mut::<QuitGuard>().hud.take() {
        handle.update(cx, |_, window, _| window.remove_window()).ok();
    }
}

/// Has VoiceOver read the prompt out (GPUI's windows have nothing for it to read).
fn announce(text: &str) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSAccessibilityAnnouncementRequestedNotification: *mut Object;
        static NSAccessibilityAnnouncementKey: *mut Object;
        static NSAccessibilityPriorityKey: *mut Object;
        fn NSAccessibilityPostNotificationWithUserInfo(
            element: *mut Object,
            notification: *mut Object,
            user_info: *mut Object,
        );
    }
    // NSAccessibilityPriorityHigh
    const HIGH: isize = 90;
    // SAFETY: AppKit calls on the main thread with autoreleased objects AppKit retains as needed.
    unsafe {
        let text = std::ffi::CString::new(text).unwrap_or_default();
        let text: *mut Object = msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()];
        let priority: *mut Object = msg_send![class!(NSNumber), numberWithInteger: HIGH];
        let objects = [text, priority];
        let keys = [NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey];
        let info: *mut Object = msg_send![class!(NSDictionary), dictionaryWithObjects: objects.as_ptr() forKeys: keys.as_ptr() count: 2usize];
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        NSAccessibilityPostNotificationWithUserInfo(app, NSAccessibilityAnnouncementRequestedNotification, info);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// FILO_QUIT_DRIVE: ⌘Q from a script, as real key events (`dev` builds)

#[derive(Clone, Debug, PartialEq)]
#[cfg(feature = "dev")]
enum Step {
    Press,
    Hold(u64),
    Key(char),
    Menu,
    Terminate,
    CloseWindow,
    UpdateRestart,
    Wait(u64),
}

#[cfg(feature = "dev")]
fn parse_drive(script: &str) -> Vec<Step> {
    script
        .split(',')
        .filter_map(|step| {
            let step = step.trim();
            let (name, value) = step.split_once(':').unwrap_or((step, ""));
            match name {
                "press" => Some(Step::Press),
                "hold" => value.parse().ok().map(Step::Hold),
                "key" => value.chars().next().map(Step::Key),
                "menu" => Some(Step::Menu),
                "terminate" => Some(Step::Terminate),
                "close-window" => Some(Step::CloseWindow),
                "update-restart" => Some(Step::UpdateRestart),
                "wait" => value.parse().ok().map(Step::Wait),
                _ => {
                    eprintln!("quit-drive: skipping \"{step}\"");
                    None
                }
            }
        })
        .collect()
}

#[cfg(feature = "dev")]
fn drive(script: String, cx: &mut App) {
    let steps = parse_drive(&script);
    cx.spawn(async move |cx| {
        for step in steps {
            match step {
                Step::Wait(ms) => cx.background_executor().timer(Duration::from_millis(ms)).await,
                Step::Press => {
                    eprintln!("quit-drive: ⌘Q");
                    post_key('q', true, false, true);
                    post_key('q', true, false, false);
                }
                Step::Hold(ms) => {
                    eprintln!("quit-drive: ⌘Q held for {ms} ms");
                    post_key('q', true, false, true);
                    // macOS starts repeating a held key after a moment, then about every 50 ms.
                    cx.background_executor().timer(Duration::from_millis(300.min(ms))).await;
                    let mut held = 300;
                    while held < ms {
                        post_key('q', true, true, true);
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        held += 50;
                    }
                    post_key('q', true, false, false);
                }
                Step::Key(letter) => {
                    eprintln!("quit-drive: {letter}");
                    post_key(letter, false, false, true);
                    post_key(letter, false, false, false);
                }
                Step::Menu => {
                    eprintln!("quit-drive: Quit Filo in Filo's menu");
                    choose_quit_in_menu();
                }
                Step::Terminate => {
                    eprintln!("quit-drive: NSApp terminate: (as the Dock's Quit, logging out)");
                    terminate();
                }
                Step::CloseWindow => {
                    eprintln!("quit-drive: the main window's close button");
                    cx.update(|cx| {
                        if let Some(handle) = cx.try_global::<crate::companion::MainWindow>().map(|main| main.0) {
                            handle.update(cx, |_, window, _| window.remove_window()).ok();
                        }
                    })
                    .ok();
                }
                Step::UpdateRestart => {
                    eprintln!("quit-drive: the updater restarting the app");
                    cx.update(|cx| {
                        crate::local_backend::restarting();
                        cx.quit();
                    })
                    .ok();
                }
            }
        }
    })
    .detach();
}

/// Posts a key event to the app's own queue: AppKit hands it on as if it were typed (key window, menu key equivalents).
#[cfg(feature = "dev")]
fn post_key(letter: char, command: bool, repeat: bool, down: bool) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSPoint;
    let key_code: u16 = match letter {
        'q' => 12,
        'a' => 0,
        _ => 0,
    };
    // SAFETY: AppKit calls on the main thread; the event is autoreleased and retained by the queue.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let key_window: *mut AnyObject = msg_send![app, keyWindow];
        let window_number: isize = if key_window.is_null() { 0 } else { msg_send![key_window, windowNumber] };
        let process: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let uptime: f64 = msg_send![process, systemUptime];
        let text = std::ffi::CString::new(letter.to_string()).unwrap_or_default();
        let characters: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()];
        // NSEventTypeKeyDown / KeyUp; NSEventModifierFlagCommand
        let kind: usize = if down { 10 } else { 11 };
        let flags: usize = if command { 1 << 20 } else { 0 };
        let null: *mut AnyObject = std::ptr::null_mut();
        let event: *mut AnyObject = msg_send![
            class!(NSEvent),
            keyEventWithType: kind,
            location: NSPoint::new(0., 0.),
            modifierFlags: flags,
            timestamp: uptime,
            windowNumber: window_number,
            context: null,
            characters: characters,
            charactersIgnoringModifiers: characters,
            isARepeat: repeat,
            keyCode: key_code
        ];
        if !event.is_null() {
            let _: () = msg_send![app, postEvent: event, atStart: false];
        }
    }
}

/// Quits the way the system asks an app to (the Dock's Quit, logging out, shutting down): `terminate:`, which goes
/// straight to GPUI's quit, without the double press.
#[cfg(feature = "dev")]
fn terminate() {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: an AppKit message on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let nothing: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![app, terminate: nothing];
    }
}

/// Runs "Quit Filo" in Filo's application menu, as a click on it does.
#[cfg(feature = "dev")]
fn choose_quit_in_menu() {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: AppKit calls on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let main: *mut AnyObject = msg_send![app, mainMenu];
        if main.is_null() {
            return;
        }
        let first: *mut AnyObject = msg_send![main, itemAtIndex: 0isize];
        let menu: *mut AnyObject = if first.is_null() { std::ptr::null_mut() } else { msg_send![first, submenu] };
        if menu.is_null() {
            return;
        }
        let title = std::ffi::CString::new("Quit Filo").unwrap_or_default();
        let title: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: title.as_ptr()];
        let index: isize = msg_send![menu, indexOfItemWithTitle: title];
        if index >= 0 {
            let _: () = msg_send![menu, performActionForItemAtIndex: index];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quits_on_a_second_press_within_two_seconds() {
        let start = Instant::now();
        let mut guard = Guard::default();
        assert_eq!(guard.press(start, false), Press::Prompt);
        assert!(guard.armed(start + Duration::from_millis(1999)));
        assert_eq!(guard.press(start + Duration::from_millis(1500), false), Press::Quit);
        // Quitting disarms it.
        assert!(!guard.armed(start + Duration::from_millis(1500)));
    }

    #[test]
    fn a_held_key_never_counts_twice() {
        let start = Instant::now();
        let mut guard = Guard::default();
        assert_eq!(guard.press(start, false), Press::Prompt);
        for n in 1..40 {
            assert_eq!(guard.press(start + Duration::from_millis(300 + n * 40), true), Press::Ignored);
        }
        // Still armed by the first press, and its time hasn't moved: a real second press quits…
        assert_eq!(guard.press(start + Duration::from_millis(1900), false), Press::Quit);
        // …and a repeat alone never starts it.
        let mut fresh = Guard::default();
        assert_eq!(fresh.press(start, true), Press::Ignored);
        assert!(!fresh.armed(start));
    }

    #[test]
    fn starts_over_once_the_two_seconds_are_up() {
        let start = Instant::now();
        let mut guard = Guard::default();
        assert_eq!(guard.press(start, false), Press::Prompt);
        assert!(!guard.armed(start + Duration::from_millis(2001)));
        // Late: this one arms afresh instead of quitting, and its own two seconds count from now.
        let late = start + Duration::from_millis(2500);
        assert_eq!(guard.press(late, false), Press::Prompt);
        assert_eq!(guard.press(late + Duration::from_millis(1000), false), Press::Quit);
        // Exactly at the edge still counts.
        let mut edge = Guard::default();
        assert_eq!(edge.press(start, false), Press::Prompt);
        assert_eq!(edge.press(start + WINDOW, false), Press::Quit);
    }

    #[test]
    fn taking_the_prompt_back_needs_a_new_first_press() {
        let start = Instant::now();
        let mut guard = Guard::default();
        assert_eq!(guard.press(start, false), Press::Prompt);
        assert!(guard.cancel());
        assert!(!guard.cancel());
        assert_eq!(guard.press(start + Duration::from_millis(300), false), Press::Prompt);
        assert_eq!(guard.press(start + Duration::from_millis(600), false), Press::Quit);
    }

    #[test]
    fn rapid_presses_quit_once_then_start_over() {
        let start = Instant::now();
        let mut guard = Guard::default();
        let presses: Vec<Press> = (0..5).map(|n| guard.press(start + Duration::from_millis(n * 60), false)).collect();
        assert_eq!(presses, vec![Press::Prompt, Press::Quit, Press::Prompt, Press::Quit, Press::Prompt]);
    }

    #[test]
    fn reads_drive_scripts() {
        assert_eq!(
            parse_drive("wait:1500, press,hold:900,key:a,menu,bogus,wait:x,terminate,close-window,update-restart"),
            vec![
                Step::Wait(1500),
                Step::Press,
                Step::Hold(900),
                Step::Key('a'),
                Step::Menu,
                Step::Terminate,
                Step::CloseWindow,
                Step::UpdateRestart
            ],
        );
    }
}

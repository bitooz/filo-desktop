// Shortcuts that work while another app is in front (companion::SHORTCUTS), with no permission to ask for:
//
// - Keys with a key in them (⌃⌥N): Carbon's RegisterEventHotKey, which reports the press and the release.
// - Modifiers alone (⌃⌥, the open shortcut): the system-wide modifier state, read a few dozen times a second.
//   A key or a click while they're down means they were part of another shortcut (⌥⌘I, ⌥⌘-click): nothing happens.
//
// The open shortcut both opens Filo (a tap) and records a voice message (held; let go to send).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::App;

use crate::companion::{self, Command, SHORTCUTS, Settings};

/// Held this long, the open shortcut records instead (the phone's VOICE_RECORDING_START_DELAY_MS).
const HOLD_TO_TALK: Duration = Duration::from_millis(300);
/// How often modifier-only shortcuts are checked, and how often while none is set.
const CHORD_POLL: Duration = Duration::from_millis(30);
const CHORD_IDLE_POLL: Duration = Duration::from_millis(500);
/// A modifier-only shortcut held past this isn't a tap any more (only the open shortcut does something held).
const TAP_UNDER: Duration = Duration::from_millis(600);

// CGEventFlags, CGEventSourceStateID and CGEventType.
const FLAG_SHIFT: u64 = 0x20000;
const FLAG_CONTROL: u64 = 0x40000;
const FLAG_OPTION: u64 = 0x80000;
const FLAG_COMMAND: u64 = 0x100000;
const FLAG_FN: u64 = 0x800000;
const COMBINED_SESSION_STATE: i32 = 0;
const KEY_DOWN: u32 = 10;
const LEFT_MOUSE_DOWN: u32 = 1;
const RIGHT_MOUSE_DOWN: u32 = 3;
const OTHER_MOUSE_DOWN: u32 = 25;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
}

/// "Filo" as a four-character code, which marks Filo's hot keys.
const SIGNATURE: u32 = u32::from_be_bytes(*b"Filo");
const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_HOT_KEY_PRESSED: u32 = 5;
const EVENT_HOT_KEY_RELEASED: u32 = 6;
const PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const TYPE_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
// Carbon's modifier bits.
const CMD_KEY: u32 = 1 << 8;
const SHIFT_KEY: u32 = 1 << 9;
const OPTION_KEY: u32 = 1 << 11;
const CONTROL_KEY: u32 = 1 << 12;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type Handler = extern "C" fn(call: *mut c_void, event: *mut c_void, user_data: *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        count: u32,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: *mut c_void,
        options: u32,
        out: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hot_key: *mut c_void) -> i32;
    fn GetEventKind(event: *mut c_void) -> u32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
}

/// Hot key presses and releases as they happen, the hold timer, and modifier-only shortcuts.
enum Event {
    /// (shortcut index, pressed)
    Key(usize, bool),
    /// The open shortcut's press number `n` has been held long enough to talk.
    Held(u64),
    /// A modifier-only shortcut was pressed and let go, with nothing else in between.
    ChordTap(usize),
    /// Held long enough to talk, so far with nothing else pressed.
    ChordHeld(usize),
    /// Let go after being held.
    ChordReleased,
    /// A key or a click while it was held: it belonged to another shortcut.
    ChordInterrupted,
}

static EVENTS: OnceLock<UnboundedSender<Event>> = OnceLock::new();

thread_local! {
    static REGISTERED: RefCell<Vec<*mut c_void>> = const { RefCell::new(Vec::new()) };
    /// Modifier-only shortcuts, watched by `watch_chords`: (shortcut index, keys).
    static CHORDS: RefCell<Vec<(usize, Keys)>> = const { RefCell::new(Vec::new()) };
    /// Shortcuts macOS wouldn't give Filo (another app has them), by id.
    static TAKEN: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

extern "C" fn on_hot_key(_call: *mut c_void, event: *mut c_void, _user_data: *mut c_void) -> i32 {
    let mut id = EventHotKeyId::default();
    // SAFETY: reads the hot key's id out of the event Carbon handed us.
    let status = unsafe {
        GetEventParameter(
            event,
            PARAM_DIRECT_OBJECT,
            TYPE_HOT_KEY_ID,
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            std::ptr::null_mut(),
            &mut id as *mut EventHotKeyId as *mut c_void,
        )
    };
    if status != 0 || id.signature != SIGNATURE {
        return status;
    }
    let pressed = unsafe { GetEventKind(event) } == EVENT_HOT_KEY_PRESSED;
    if let Some(events) = EVENTS.get() {
        let _ = events.unbounded_send(Event::Key(id.id as usize, pressed));
    }
    0
}

/// Starts listening for Filo's hot keys (once, at launch).
pub fn install(cx: &mut App) {
    let (tx, mut rx) = unbounded();
    if EVENTS.set(tx).is_err() {
        return;
    }
    let types = [
        EventTypeSpec { event_class: EVENT_CLASS_KEYBOARD, event_kind: EVENT_HOT_KEY_PRESSED },
        EventTypeSpec { event_class: EVENT_CLASS_KEYBOARD, event_kind: EVENT_HOT_KEY_RELEASED },
    ];
    // SAFETY: installs a plain C callback on the app's own event target, for the life of the app.
    let status = unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            on_hot_key,
            types.len() as u32,
            types.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        eprintln!("shortcuts: couldn't listen for hot keys ({status})");
        return;
    }
    cx.spawn(async move |cx| {
        // The open shortcut is down (its press number): let go quickly it opens Filo, held it records.
        let mut open_down: Option<u64> = None;
        let mut talking = false;
        let mut presses = 0u64;
        // Which hold shortcut is down: open (talk) or screen (talk with a screenshot).
        let mut held_index = 0usize;
        while let Some(event) = rx.next().await {
            let command = match event {
                Event::Key(index, pressed) => {
                    let Some(shortcut) = SHORTCUTS.get(index) else { continue };
                    match (shortcut.id, pressed) {
                        (companion::OPEN | companion::SCREEN, true) => {
                            if open_down.is_none() {
                                held_index = index;
                                presses += 1;
                                open_down = Some(presses);
                                let press = presses;
                                let timer = cx.background_executor().timer(HOLD_TO_TALK);
                                cx.spawn(async move |_| {
                                    timer.await;
                                    if let Some(events) = EVENTS.get() {
                                        let _ = events.unbounded_send(Event::Held(press));
                                    }
                                })
                                .detach();
                            }
                            None
                        }
                        (companion::OPEN | companion::SCREEN, false) => match open_down.take() {
                            Some(_) if std::mem::take(&mut talking) => Some(Command::TalkEnd),
                            Some(_) if shortcut.id == companion::OPEN => Some(Command::Toggle),
                            _ => None,
                        },
                        (id, true) => Some(command_for(id)),
                        _ => None,
                    }
                }
                Event::Held(press) if open_down == Some(press) && !talking => {
                    talking = true;
                    Some(talk_command(held_index))
                }
                Event::Held(_) => None,
                Event::ChordTap(index) => match SHORTCUTS.get(index).map(|shortcut| shortcut.id) {
                    Some(companion::OPEN) => Some(Command::Toggle),
                    // Only held does anything.
                    Some(companion::SCREEN) | None => None,
                    Some(id) => Some(command_for(id)),
                },
                Event::ChordHeld(index)
                    if SHORTCUTS
                        .get(index)
                        .is_some_and(|shortcut| matches!(shortcut.id, companion::OPEN | companion::SCREEN)) =>
                {
                    talking = true;
                    Some(talk_command(index))
                }
                Event::ChordHeld(_) => None,
                Event::ChordReleased if std::mem::take(&mut talking) => Some(Command::TalkEnd),
                Event::ChordInterrupted if std::mem::take(&mut talking) => Some(Command::TalkCancel),
                Event::ChordReleased | Event::ChordInterrupted => None,
            };
            if let Some(command) = command
                && cx.update(|cx| companion::run(command, cx)).is_err()
            {
                break;
            }
        }
    })
    .detach();
    cx.spawn(async move |cx| watch_chords(cx).await).detach();
}

/// Where a modifier-only shortcut is: down since when, whether it's past the hold, and whether something else was
/// pressed meanwhile.
struct ChordPress {
    index: usize,
    since: std::time::Instant,
    held: bool,
    interrupted: bool,
}

/// Watches the modifier keys for the modifier-only shortcuts.
async fn watch_chords(cx: &mut gpui::AsyncApp) {
    let mut press: Option<ChordPress> = None;
    loop {
        let chords = CHORDS.with(|chords| chords.borrow().clone());
        cx.background_executor()
            .timer(if chords.is_empty() && press.is_none() { CHORD_IDLE_POLL } else { CHORD_POLL })
            .await;
        // SAFETY: read-only queries of the system's input state, which need no permission.
        let flags = unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) };
        let down = chords.iter().find(|(_, keys)| keys.matches_flags(flags)).map(|(index, _)| *index);
        let send = |event: Event| {
            if let Some(events) = EVENTS.get() {
                let _ = events.unbounded_send(event);
            }
        };
        match (&mut press, down) {
            (None, Some(index)) => {
                press = Some(ChordPress { index, since: std::time::Instant::now(), held: false, interrupted: false })
            }
            (Some(current), Some(index)) if current.index == index => {
                let elapsed = current.since.elapsed();
                if !current.interrupted && something_pressed_within(elapsed) {
                    current.interrupted = true;
                    if current.held {
                        send(Event::ChordInterrupted);
                    }
                }
                if !current.held && !current.interrupted && elapsed >= HOLD_TO_TALK {
                    current.held = true;
                    send(Event::ChordHeld(index));
                }
            }
            (Some(_), _) => {
                let Some(ended) = press.take() else { continue };
                let interrupted = ended.interrupted || something_pressed_within(ended.since.elapsed());
                if ended.held {
                    send(if interrupted { Event::ChordInterrupted } else { Event::ChordReleased });
                } else if !interrupted && ended.since.elapsed() < TAP_UNDER {
                    send(Event::ChordTap(ended.index));
                }
            }
            (None, None) => {}
        }
    }
}

/// Whether a key or a mouse button went down anywhere in the last `window`.
fn something_pressed_within(window: Duration) -> bool {
    let window = window.as_secs_f64();
    [KEY_DOWN, LEFT_MOUSE_DOWN, RIGHT_MOUSE_DOWN, OTHER_MOUSE_DOWN]
        .into_iter()
        // SAFETY: as in watch_chords.
        .any(
            |event_type| unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, event_type) } < window,
        )
}

/// What holding a hold shortcut starts.
fn talk_command(index: usize) -> Command {
    match SHORTCUTS.get(index).map(|shortcut| shortcut.id) {
        Some(companion::SCREEN) => Command::TalkScreenStart,
        _ => Command::TalkStart,
    }
}

fn command_for(id: &str) -> Command {
    match id {
        companion::PET => Command::TogglePet,
        companion::STOP_APPS => Command::StopApps,
        _ => Command::Open,
    }
}

/// Registers the shortcuts as set, in place of the ones before.
pub fn register(settings: &Settings) {
    unregister_all();
    let mut taken = Vec::new();
    let mut chords = Vec::new();
    REGISTERED.with(|registered| {
        let mut registered = registered.borrow_mut();
        for (index, shortcut) in SHORTCUTS.iter().enumerate() {
            let Some(keys) = settings.shortcut(shortcut.id).and_then(|keys| Keys::parse(&keys)) else { continue };
            if keys.is_chord() {
                chords.push((index, keys));
                continue;
            }
            let Some(code) = key_code(&keys.key) else { continue };
            let mut hot_key = std::ptr::null_mut();
            // SAFETY: registers with the app's event target; the reference is kept to unregister it.
            let status = unsafe {
                RegisterEventHotKey(
                    code,
                    keys.carbon_modifiers(),
                    EventHotKeyId { signature: SIGNATURE, id: index as u32 },
                    GetApplicationEventTarget(),
                    0,
                    &mut hot_key,
                )
            };
            if status == 0 && !hot_key.is_null() {
                registered.push(hot_key);
            } else {
                eprintln!("shortcuts: macOS wouldn't give Filo {} for \"{}\" ({status})", keys.label(), shortcut.label);
                taken.push(shortcut.id);
            }
        }
    });
    TAKEN.with(|slot| *slot.borrow_mut() = taken);
    CHORDS.with(|slot| *slot.borrow_mut() = chords);
}

/// Lets go of every shortcut (while the Shortcuts page records a new one, so the old one doesn't fire).
pub fn unregister_all() {
    CHORDS.with(|chords| chords.borrow_mut().clear());
    REGISTERED.with(|registered| {
        for hot_key in registered.borrow_mut().drain(..) {
            // SAFETY: a reference RegisterEventHotKey returned, unregistered once.
            unsafe { UnregisterEventHotKey(hot_key) };
        }
    });
}

/// Whether macOS refused a shortcut (another app has it).
pub fn is_taken(id: &str) -> bool {
    TAKEN.with(|taken| taken.borrow().contains(&id))
}

/// A keystroke in GPUI's notation ("ctrl-alt-n", "alt-space").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
    pub key: String,
}

impl Keys {
    pub fn parse(keystroke: &str) -> Option<Keys> {
        let keystroke = keystroke.trim();
        // "cmd--": the key is the minus.
        let (modifiers, key) = match keystroke.strip_suffix("--") {
            Some(rest) => (rest, "-"),
            None => match keystroke.rsplit_once('-') {
                Some((modifiers, key)) => (modifiers, key),
                None => ("", keystroke),
            },
        };
        if key.is_empty() {
            return None;
        }
        // Modifiers alone ("alt-cmd"): the last one isn't a key.
        let (modifiers, key) = if is_modifier(key) { (keystroke, "") } else { (modifiers, key) };
        let mut keys = Keys { key: key.to_lowercase(), ..Keys::default() };
        for modifier in modifiers.split('-').filter(|part| !part.is_empty()) {
            match modifier {
                "ctrl" | "control" => keys.ctrl = true,
                "alt" | "option" => keys.alt = true,
                "shift" => keys.shift = true,
                "cmd" | "super" | "platform" => keys.cmd = true,
                _ => return None,
            }
        }
        Some(keys)
    }

    /// From a key pressed on the Shortcuts page.
    pub fn from_keystroke(keystroke: &gpui::Keystroke) -> Keys {
        let modifiers = keystroke.modifiers;
        Keys {
            ctrl: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.platform,
            key: keystroke.key.to_lowercase(),
        }
    }

    /// Modifiers alone (⌥⌘).
    pub fn is_chord(&self) -> bool {
        self.key.is_empty()
    }

    fn modifier_count(&self) -> usize {
        [self.ctrl, self.alt, self.shift, self.cmd].into_iter().filter(|on| *on).count()
    }

    /// Exactly these modifiers are down (and not fn), in CGEventFlags.
    fn matches_flags(&self, flags: u64) -> bool {
        let on = |flag: u64| flags & flag != 0;
        on(FLAG_CONTROL) == self.ctrl
            && on(FLAG_OPTION) == self.alt
            && on(FLAG_SHIFT) == self.shift
            && on(FLAG_COMMAND) == self.cmd
            && !on(FLAG_FN)
    }

    /// A global shortcut needs ⌘, ⌃ or ⌥ (or a function key) and a key macOS can register, or two modifiers or more
    /// alone (one alone is pressed all day).
    pub fn usable(&self) -> bool {
        if self.is_chord() {
            return self.modifier_count() >= 2 && (self.ctrl || self.alt || self.cmd);
        }
        let function_key = self.key.len() > 1 && self.key.starts_with('f') && self.key[1..].parse::<u8>().is_ok();
        (self.ctrl || self.alt || self.cmd || function_key) && key_code(&self.key).is_some()
    }

    pub fn notation(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        if self.cmd {
            parts.push("cmd");
        }
        if !self.key.is_empty() {
            parts.push(&self.key);
        }
        parts.join("-")
    }

    /// The way macOS writes it: ⌃⌥⇧⌘ then the key ("⌥ Space", "⌃⌥N").
    pub fn label(&self) -> String {
        let mut label = String::new();
        for (on, symbol) in [(self.ctrl, "⌃"), (self.alt, "⌥"), (self.shift, "⇧"), (self.cmd, "⌘")] {
            if on {
                label.push_str(symbol);
            }
        }
        let key = match self.key.as_str() {
            "space" => " Space".to_string(),
            "enter" => "↩".to_string(),
            "tab" => "⇥".to_string(),
            "backspace" => "⌫".to_string(),
            "delete" => "⌦".to_string(),
            "escape" => "⎋".to_string(),
            "left" => "←".to_string(),
            "right" => "→".to_string(),
            "up" => "↑".to_string(),
            "down" => "↓".to_string(),
            "pageup" => " Page Up".to_string(),
            "pagedown" => " Page Down".to_string(),
            "" => String::new(),
            // F5 as on the key; other names in sentence case ("Home", never "HOME").
            key if key.len() > 1 && key.starts_with('f') && key[1..].chars().all(|c| c.is_ascii_digit()) => {
                format!(" {}", key.to_uppercase())
            }
            key if key.len() > 1 => {
                let mut chars = key.chars();
                format!(" {}{}", chars.next().map(|c| c.to_ascii_uppercase()).unwrap_or_default(), chars.as_str())
            }
            // A letter as on the key cap: ⌃⌥N.
            key => key.to_uppercase(),
        };
        label.push_str(&key);
        label.trim().to_string()
    }

    fn carbon_modifiers(&self) -> u32 {
        let mut modifiers = 0;
        if self.cmd {
            modifiers |= CMD_KEY;
        }
        if self.shift {
            modifiers |= SHIFT_KEY;
        }
        if self.alt {
            modifiers |= OPTION_KEY;
        }
        if self.ctrl {
            modifiers |= CONTROL_KEY;
        }
        modifiers
    }
}

fn is_modifier(part: &str) -> bool {
    matches!(part, "ctrl" | "control" | "alt" | "option" | "shift" | "cmd" | "super" | "platform")
}

/// The label of a shortcut as set ("Off" when turned off).
pub fn label(keystroke: Option<&str>) -> String {
    keystroke.and_then(Keys::parse).map(|keys| keys.label()).unwrap_or_else(|| "Off".into())
}

/// macOS's virtual key code for a key, by its place on a US keyboard.
fn key_code(key: &str) -> Option<u32> {
    let code = match key {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "=" => 24,
        "9" => 25,
        "7" => 26,
        "-" => 27,
        "8" => 28,
        "0" => 29,
        "]" => 30,
        "o" => 31,
        "u" => 32,
        "[" => 33,
        "i" => 34,
        "p" => 35,
        "enter" => 36,
        "l" => 37,
        "j" => 38,
        "'" => 39,
        "k" => 40,
        ";" => 41,
        "\\" => 42,
        "," => 43,
        "/" => 44,
        "n" => 45,
        "m" => 46,
        "." => 47,
        "tab" => 48,
        "space" => 49,
        "`" => 50,
        "backspace" => 51,
        "escape" => 53,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f3" => 99,
        "f8" => 100,
        "f9" => 101,
        "f11" => 103,
        "f13" => 105,
        "f14" => 107,
        "f10" => 109,
        "f12" => 111,
        "f15" => 113,
        "home" => 115,
        "pageup" => 116,
        "delete" => 117,
        "f4" => 118,
        "end" => 119,
        "f2" => 120,
        "pagedown" => 121,
        "f1" => 122,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystrokes_parse_and_print() {
        let keys = Keys::parse("ctrl-alt-n").unwrap();
        assert!(keys.ctrl && keys.alt && !keys.cmd && !keys.shift);
        assert_eq!(keys.key, "n");
        assert_eq!(keys.label(), "⌃⌥N");
        assert_eq!(keys.notation(), "ctrl-alt-n");
        assert_eq!(Keys::parse("alt-space").unwrap().label(), "⌥ Space");
        assert_eq!(Keys::parse("ctrl-alt-home").unwrap().label(), "⌃⌥ Home");
        assert_eq!(Keys::parse("cmd-pageup").unwrap().label(), "⌘ Page Up");
        assert_eq!(Keys::parse("cmd--").unwrap().key, "-");
        assert_eq!(Keys::parse("shift-cmd-f5").unwrap().label(), "⇧⌘ F5");
        assert_eq!(Keys::parse("hyper-x"), None);
        let chord = Keys::parse("alt-cmd").unwrap();
        assert!(chord.is_chord() && chord.alt && chord.cmd && !chord.ctrl);
        assert_eq!(chord.label(), "⌥⌘");
        assert_eq!(chord.notation(), "alt-cmd");
        assert_eq!(label(None), "Off");
    }

    #[test]
    fn global_shortcuts_need_a_modifier() {
        assert!(Keys::parse("alt-space").unwrap().usable());
        assert!(Keys::parse("f5").unwrap().usable());
        assert!(!Keys::parse("n").unwrap().usable());
        assert!(!Keys::parse("shift-n").unwrap().usable());
        assert!(!Keys::parse("alt-é").unwrap().usable());
        assert!(Keys::parse("alt-cmd").unwrap().usable());
        assert!(Keys::parse("shift-cmd").unwrap().usable());
        // One modifier alone is pressed all day.
        assert!(!Keys::parse("cmd").unwrap().usable());
    }

    #[test]
    fn chords_match_exactly_these_modifiers() {
        let chord = Keys::parse("alt-cmd").unwrap();
        assert!(chord.matches_flags(FLAG_OPTION | FLAG_COMMAND));
        assert!(!chord.matches_flags(FLAG_OPTION));
        assert!(!chord.matches_flags(FLAG_OPTION | FLAG_COMMAND | FLAG_SHIFT));
        assert!(!chord.matches_flags(FLAG_OPTION | FLAG_COMMAND | FLAG_FN));
    }

    #[test]
    fn every_default_registers() {
        for shortcut in SHORTCUTS {
            let keys = Keys::parse(shortcut.default).unwrap();
            assert!(keys.usable(), "{}", shortcut.id);
            assert!(keys.is_chord() || key_code(&keys.key).is_some(), "{}", shortcut.id);
        }
    }
}

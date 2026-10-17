// Filo in the menu bar: the Filo mark (a template image, so it takes the menu bar's colour) with the number of
// unseen replies beside it, and a menu of what Filo is doing (replying or working, tasks keeping the Mac awake, new
// replies, what's next in Scheduled) and quick ways in: open Filo, the pet, keyboard shortcuts. A right-click on the pet opens
// the same menu. What's picked goes to companion::run.

use std::cell::RefCell;
use std::sync::OnceLock;

use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::App;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AllocAnyThread as _, class, define_class, msg_send, sel};
use objc2_foundation::{NSData, NSPoint, NSSize, NSString};

use crate::companion::{self, Command, MenuState};

/// The menu bar's icon height, in points (the mark is drawn at twice this for Retina).
const ICON_POINTS: f64 = 16.;
/// `NSVariableStatusItemLength`
const VARIABLE_LENGTH: f64 = -1.;
/// `NSImageLeft`
const IMAGE_LEFT: usize = 2;

/// Commands picked in the menu, for the app to carry out.
static PICKS: OnceLock<UnboundedSender<Command>> = OnceLock::new();

thread_local! {
    static ITEM: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    static TARGET: RefCell<Option<Retained<Target>>> = const { RefCell::new(None) };
    /// What the open menu's items do, by tag.
    static COMMANDS: RefCell<Vec<Command>> = const { RefCell::new(Vec::new()) };
    static STATE: RefCell<Option<MenuState>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FiloMenuTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(pick:))]
        fn pick(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            let command = COMMANDS.with(|commands| commands.borrow().get(tag as usize).cloned());
            if let (Some(command), Some(picks)) = (command, PICKS.get()) {
                let _ = picks.unbounded_send(command);
            }
        }
    }
);

impl Target {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Starts taking menu picks (once, at launch).
pub fn install(cx: &mut App) {
    let (tx, mut rx) = unbounded();
    if PICKS.set(tx).is_err() {
        return;
    }
    TARGET.with(|target| *target.borrow_mut() = Some(Target::new()));
    cx.spawn(async move |cx| {
        while let Some(command) = rx.next().await {
            if cx.update(|cx| companion::run(command, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

/// Puts the item in the menu bar, or takes it out.
pub fn show(visible: bool) {
    let present = ITEM.with(|item| item.borrow().is_some());
    if visible == present {
        return;
    }
    // SAFETY: AppKit messages on the main thread.
    unsafe {
        let bar: Retained<AnyObject> = msg_send![class!(NSStatusBar), systemStatusBar];
        if !visible {
            if let Some(item) = ITEM.with(|item| item.borrow_mut().take()) {
                let _: () = msg_send![&bar, removeStatusItem: &*item];
            }
            return;
        }
        let item: Retained<AnyObject> = msg_send![&bar, statusItemWithLength: VARIABLE_LENGTH];
        let button: Option<Retained<AnyObject>> = msg_send![&item, button];
        if let Some(button) = &button {
            if let Some(image) = mark_image() {
                let _: () = msg_send![button, setImage: &*image];
            }
            let _: () = msg_send![button, setImagePosition: IMAGE_LEFT];
            let _: () = msg_send![button, setToolTip: &*NSString::from_str("Filo")];
        }
        ITEM.with(|slot| *slot.borrow_mut() = Some(item));
    }
    if let Some(state) = STATE.with(|state| state.borrow().clone()) {
        apply(&state);
    }
}

/// Shows what Filo is doing: the count beside the icon and the menu's contents.
pub fn set(state: MenuState) {
    STATE.with(|slot| *slot.borrow_mut() = Some(state.clone()));
    apply(&state);
}

fn apply(state: &MenuState) {
    ITEM.with(|item| {
        let item = item.borrow();
        let Some(item) = item.as_ref() else { return };
        // SAFETY: AppKit messages on the main thread.
        unsafe {
            let button: Option<Retained<AnyObject>> = msg_send![item, button];
            if let Some(button) = &button {
                let count = if state.unseen_total > 0 {
                    crate::unseen::format_count(state.unseen_total)
                } else {
                    String::new()
                };
                // While Filo uses the user's apps the menu bar says which, beside the icon, until it stops.
                let title = match &state.using_apps {
                    Some(apps) => {
                        let using = companion::apps_label(apps).unwrap_or_else(|| "your apps".into());
                        if count.is_empty() { format!("Using {using}") } else { format!("Using {using} · {count}") }
                    }
                    None => count,
                };
                let _: () = msg_send![button, setTitle: &*NSString::from_str(&title)];
            }
            let menu = build_menu(state);
            let _: () = msg_send![item, setMenu: &*menu];
        }
    });
}

/// Opens the menu where the pointer is (a right-click on the pet). Call it outside GPUI's event handling: it runs
/// until the menu closes.
pub fn pop_up() {
    let Some(state) = STATE.with(|state| state.borrow().clone()) else { return };
    // SAFETY: AppKit messages on the main thread.
    unsafe {
        let menu = build_menu(&state);
        let point: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let _: bool = msg_send![&menu, popUpMenuPositioningItem: None::<&AnyObject>, atLocation: point, inView: None::<&AnyObject>];
    }
}

unsafe fn build_menu(state: &MenuState) -> Retained<AnyObject> {
    let mut commands = Vec::new();
    let menu: Retained<AnyObject> = unsafe { msg_send![class!(NSMenu), new] };
    let _: () = unsafe { msg_send![&menu, setAutoenablesItems: false] };
    let target = TARGET.with(|target| target.borrow().clone());
    let mut add = |title: &str, command: Option<Command>, keys: Option<&str>| unsafe {
        let item: Retained<AnyObject> = msg_send![class!(NSMenuItem), new];
        let _: () = msg_send![&item, setTitle: &*NSString::from_str(title)];
        match (command, &target) {
            (Some(command), Some(target)) => {
                let _: () = msg_send![&item, setTarget: &**target];
                let _: () = msg_send![&item, setAction: sel!(pick:)];
                let _: () = msg_send![&item, setTag: commands.len() as isize];
                commands.push(command);
            }
            _ => {
                let _: () = msg_send![&item, setEnabled: false];
            }
        }
        if let Some((key, mask)) = keys.and_then(key_equivalent) {
            let _: () = msg_send![&item, setKeyEquivalent: &*NSString::from_str(&key)];
            let _: () = msg_send![&item, setKeyEquivalentModifierMask: mask];
        }
        let _: () = msg_send![&menu, addItem: &*item];
    };
    let separator = |menu: &AnyObject| unsafe {
        let item: Retained<AnyObject> = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: &*item];
    };
    let shortcut = |id: &str| state.shortcuts.get(id).map(String::as_str);

    if let Some(apps) = &state.using_apps {
        add(
            &format!("Filo is using {}", companion::apps_label(apps).unwrap_or_else(|| "your apps".into())),
            None,
            None,
        );
        add("Stop Filo Using Your Apps", Some(Command::StopApps), shortcut(companion::STOP_APPS));
        separator(&menu);
    }
    if let Some(status) = &state.status {
        add(status, None, None);
    }
    // Under what Filo is doing: the tasks keeping the Mac awake, and a way to let it sleep (the setting goes off).
    if let Some(running) = state.keeping_awake {
        add(&companion::keeping_awake_label(running), None, None);
        add("Let Mac Sleep", Some(Command::LetMacSleep), None);
    }
    if state.status.is_some() || state.keeping_awake.is_some() {
        separator(&menu);
    }
    if !state.replies.is_empty() {
        let header = match state.unseen_total {
            1 => "1 new reply".to_string(),
            count => format!("{} new replies", crate::unseen::format_count(count)),
        };
        add(&header, None, None);
        for chat in &state.replies {
            add(&chat.label, Some(Command::OpenChat(chat.session_id.clone())), None);
        }
        separator(&menu);
    }
    if !state.upcoming.is_empty() {
        add("Up next", None, None);
        for label in &state.upcoming {
            add(label, Some(Command::Scheduled), None);
        }
        separator(&menu);
    }
    add("Open Filo", Some(Command::Open), shortcut(companion::OPEN));
    if state.ready {
        add(if state.pet { "Hide Pet" } else { "Show Pet" }, Some(Command::TogglePet), shortcut(companion::PET));
        add("Keyboard Shortcuts…", Some(Command::Shortcuts), None);
    }
    separator(&menu);
    add("Quit Filo", Some(Command::Quit), Some("cmd-q"));
    COMMANDS.with(|slot| *slot.borrow_mut() = commands);
    menu
}

/// A keystroke ("ctrl-alt-n") as a menu item's key equivalent and modifier mask, when AppKit can show it.
fn key_equivalent(keystroke: &str) -> Option<(String, usize)> {
    let keys = crate::hotkeys::Keys::parse(keystroke)?;
    let key = match keys.key.as_str() {
        "space" => " ".to_string(),
        key if key.chars().count() == 1 => key.to_string(),
        _ => return None,
    };
    // NSEventModifierFlags
    let mut mask = 0;
    if keys.shift {
        mask |= 1 << 17;
    }
    if keys.ctrl {
        mask |= 1 << 18;
    }
    if keys.alt {
        mask |= 1 << 19;
    }
    if keys.cmd {
        mask |= 1 << 20;
    }
    Some((key, mask))
}

/// The Filo mark as a template image for the menu bar.
fn mark_image() -> Option<Retained<AnyObject>> {
    let png = mark_png()?;
    let data = NSData::with_bytes(&png);
    // SAFETY: AppKit messages on the main thread.
    unsafe {
        let image: Option<Retained<AnyObject>> = msg_send![msg_send![class!(NSImage), alloc], initWithData: &*data];
        let image = image?;
        let _: () = msg_send![&image, setSize: NSSize::new(ICON_POINTS, ICON_POINTS)];
        let _: () = msg_send![&image, setTemplate: true];
        Some(image)
    }
}

/// icons/filo-mark.svg drawn at 2x, black on clear.
fn mark_png() -> Option<Vec<u8>> {
    let svg = crate::assets::Embedded::get("icons/filo-mark.svg")?.data;
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default()).ok()?;
    let pixels = (ICON_POINTS * 2.) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixels, pixels)?;
    let scale = pixels as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // Black, so premultiplied and straight alpha are the same.
    let image = image::RgbaImage::from_raw(pixels, pixels, pixmap.data().to_vec())?;
    let mut png = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_draws() {
        let png = mark_png().expect("mark");
        let image = image::load_from_memory(&png).expect("png").to_rgba8();
        assert_eq!(image.dimensions(), (32, 32));
        assert!(image.pixels().any(|pixel| pixel[3] > 200), "the mark is drawn");
        assert!(image.pixels().any(|pixel| pixel[3] == 0), "around it stays clear");
    }

    #[test]
    fn menu_keys() {
        assert_eq!(key_equivalent("alt-space"), Some((" ".into(), 1 << 19)));
        assert_eq!(key_equivalent("ctrl-alt-n"), Some(("n".into(), (1 << 18) | (1 << 19))));
        assert_eq!(key_equivalent("cmd-q"), Some(("q".into(), 1 << 20)));
        assert_eq!(key_equivalent("ctrl-alt-f5"), None);
    }
}

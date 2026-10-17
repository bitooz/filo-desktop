// Window chrome for the hidden title bar: the menu bar (Quit, Edit, full screen,
// minimize, zoom) and drag regions. GPUI has no window dragging on macOS, so a
// mouse-down in a drag region hands the event to AppKit's window drag; a
// double-click zooms (or whatever the system setting says).

use gpui::{
    Action, App, Div, InteractiveElement as _, KeyBinding, Menu, MenuItem, MouseButton, OsAction, Stateful, Window,
    actions,
};

use crate::nav::Tab;

actions!(
    filo,
    [
        Quit,
        ToggleFullScreen,
        Minimize,
        Zoom,
        GoBack,
        GoForward,
        OpenSettings,
        OpenShortcuts,
        TogglePet,
        Search,
        TabAgents,
        TabFeed,
        TabNotes,
        TabArtifacts,
        TabCoding,
        TabConnectors,
        TabSettings,
    ]
);

pub fn init(cx: &mut App) {
    // ⌘Q twice, or Quit in the menu once (quit_guard.rs).
    cx.on_action(|_: &Quit, cx| crate::quit_guard::quit_requested(cx));
    // The main window's: the pet and the ⌘Q prompt can't be minimized, zoomed or made full screen.
    cx.on_action(|_: &ToggleFullScreen, cx| after_dispatch(cx, |window, _| window.toggle_fullscreen()));
    cx.on_action(|_: &Minimize, cx| after_dispatch(cx, |window, _| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| after_dispatch(cx, |window, _| window.zoom_window()));
    cx.on_action(|_: &GoBack, cx| after_dispatch(cx, crate::nav::back));
    cx.on_action(|_: &GoForward, cx| after_dispatch(cx, crate::nav::forward));
    cx.on_action(|_: &OpenSettings, cx| {
        after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Settings, window, cx))
    });
    cx.on_action(|_: &OpenShortcuts, cx| {
        after_dispatch(cx, |window, cx| crate::nav::push(crate::nav::Route::Shortcuts, window, cx))
    });
    cx.on_action(|_: &TogglePet, cx| crate::companion::run(crate::companion::Command::TogglePet, cx));
    cx.on_action(|_: &Search, cx| after_dispatch(cx, crate::nav::toggle_search));
    cx.on_action(|_: &TabAgents, cx| after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Agents, window, cx)));
    cx.on_action(|_: &TabFeed, cx| after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Feed, window, cx)));
    cx.on_action(|_: &TabNotes, cx| after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Notes, window, cx)));
    cx.on_action(|_: &TabArtifacts, cx| {
        after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Artifacts, window, cx))
    });
    cx.on_action(|_: &TabCoding, cx| after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Coding, window, cx)));
    cx.on_action(|_: &TabConnectors, cx| {
        after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Connectors, window, cx))
    });
    cx.on_action(|_: &TabSettings, cx| {
        after_dispatch(cx, |window, cx| crate::nav::open_tab(Tab::Settings, window, cx))
    });
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-m", Minimize, None),
        // Not in text fields, where these move to the line's start/end and outdent/indent.
        // (A binding without a context counts as matching at the focused element itself,
        // so it would beat the field's own; `!Input` matches only above the field.)
        KeyBinding::new("cmd-left", GoBack, Some("!Input")),
        KeyBinding::new("cmd-[", GoBack, Some("!Input")),
        KeyBinding::new("cmd-right", GoForward, Some("!Input")),
        KeyBinding::new("cmd-]", GoForward, Some("!Input")),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-/", OpenShortcuts, None),
        // Anywhere, a text field too (screens/search.rs).
        KeyBinding::new("cmd-p", Search, None),
        // The sidebar's order (app.rs), top to bottom.
        KeyBinding::new("cmd-1", TabAgents, None),
        KeyBinding::new("cmd-2", TabFeed, None),
        KeyBinding::new("cmd-3", TabNotes, None),
        KeyBinding::new("cmd-4", TabArtifacts, None),
        KeyBinding::new("cmd-5", TabCoding, None),
        KeyBinding::new("cmd-6", TabConnectors, None),
        KeyBinding::new("cmd-7", TabSettings, None),
    ]);
    set_menus(cx);
}

/// The menu bar's menus: View lists the sidebar's tabs, less Feed or Notes while it's off (pages.rs).
fn set_menus(cx: &mut App) {
    use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    let tabs = crate::nav::sidebar(cx);
    let tab_items =
        tabs.iter().map(|tab| MenuItem::Action { name: tab.label().into(), action: tab_action(*tab), os_action: None });
    cx.set_menus(vec![
        Menu {
            name: "Filo".into(),
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Keyboard Shortcuts…", OpenShortcuts),
                MenuItem::action("Show or Hide Pet", TogglePet),
                MenuItem::separator(),
                MenuItem::action("Quit Filo", Quit),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Search…", Search),
                MenuItem::separator(),
                MenuItem::action("Back", GoBack),
                MenuItem::action("Forward", GoForward),
                MenuItem::separator(),
            ]
            .into_iter()
            .chain(tab_items)
            .chain([MenuItem::separator(), MenuItem::action("Toggle Full Screen", ToggleFullScreen)])
            .collect(),
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Browser", crate::browser::window::OpenBrowser),
            ],
        },
    ]);
}

fn tab_action(tab: Tab) -> Box<dyn Action> {
    match tab {
        Tab::Agents => Box::new(TabAgents),
        Tab::Feed => Box::new(TabFeed),
        Tab::Notes => Box::new(TabNotes),
        Tab::Artifacts => Box::new(TabArtifacts),
        Tab::Coding => Box::new(TabCoding),
        Tab::Connectors => Box::new(TabConnectors),
        Tab::Settings => Box::new(TabSettings),
    }
}

/// Feed or Notes turned on or off (pages.rs): ⌘1 – ⌘7 go to the sidebar's tabs as it shows them now, the keys past
/// the last one to nothing, and the View menu lists the same tabs. The newest bindings win (gpui's keymap).
pub fn pages_changed(cx: &mut App) {
    let tabs = crate::nav::sidebar(cx);
    let bindings = (1..=7).map(|number| {
        let keys = format!("cmd-{number}");
        match tabs.get(number - 1) {
            Some(tab) => KeyBinding::load(&keys, tab_action(*tab), None, false, None, &gpui::DummyKeyboardMapper),
            None => KeyBinding::load(&keys, Box::new(gpui::NoAction), None, false, None, &gpui::DummyKeyboardMapper),
        }
    });
    cx.bind_keys(bindings.filter_map(Result::ok));
    set_menus(cx);
}

/// Runs `f` on Filo's main window once the current event is done: a key binding's
/// action arrives while the window is still dispatching it, and can't update it then.
fn after_dispatch(cx: &mut App, f: fn(&mut Window, &mut App)) {
    cx.defer(move |cx| {
        // The main window even when the pet or the ⌘Q prompt is key. The active one only before the main window exists.
        let main = cx.try_global::<crate::companion::MainWindow>().map(|main| main.0);
        if let Some(window) = main.or_else(|| cx.active_window()) {
            window.update(cx, |_, window, cx| f(window, cx)).ok();
        }
    });
}

/// Makes `element` drag the window, and zoom it on double-click, like a title bar.
/// Put it on empty areas only: a drag swallows the click.
pub fn drag_region(element: Stateful<Div>) -> Stateful<Div> {
    element.on_mouse_down(MouseButton::Left, |event, window, cx| {
        if event.click_count >= 2 {
            window.titlebar_double_click();
        } else {
            begin_drag(window, cx);
        }
    })
}

#[allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare
fn begin_drag(window: &mut Window, cx: &mut App) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: plain AppKit messages on the main thread; the event is retained until used.
    let (ns_window, event) = unsafe {
        let view = handle.ns_view.as_ptr() as *mut Object;
        let ns_window: *mut Object = msg_send![view, window];
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut Object = msg_send![app, currentEvent];
        if ns_window.is_null() || event.is_null() {
            return;
        }
        let _: *mut Object = msg_send![event, retain];
        (ns_window as usize, event as usize)
    };
    // AppKit's drag loop reports window moves back into GPUI, which must not happen
    // while GPUI is still dispatching this mouse-down, so start it on the next tick.
    window
        .spawn(cx, async move |_| unsafe {
            let (ns_window, event) = (ns_window as *mut Object, event as *mut Object);
            // A quick click is already over; starting a drag now would make the
            // window follow the pointer until the next click.
            let pressed: usize = msg_send![class!(NSEvent), pressedMouseButtons];
            if pressed & 1 != 0 {
                let _: () = msg_send![ns_window, performWindowDragWithEvent: event];
            }
            let _: () = msg_send![event, release];
        })
        .detach();
}

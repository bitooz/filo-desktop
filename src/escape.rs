// Esc closes what's open on top, one layer a press, wherever the keyboard is. Each open menu, picker, panel or inline
// editor of the app's own says so while it renders, so the frame's list runs bottom to top the way things draw: a
// view renders before the views inside it. What floats (`popup`) is over what sits in place (`layer`), and a dialog
// over both. Esc goes to the focused view first (a text field, gpui-component's menus and dialogs, an editor with its
// own Esc); only when nothing there used it does the top layer close. Pages pushed on the stack aren't layers: Esc
// doesn't go back.
//
// ⌘S works the same way: a page showing a save bar says so while it renders (`save`), and ⌘S saves it wherever the
// keyboard is, even with nothing focused (a menu that closed, a click in a list beside it).
//
// A popup is also a menu in gpui-component's one-open-menu rule (its one_menu.rs): the frame one starts showing, it
// closes any other menu or popover open in the app, and it closes when another opens.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{App, Context, Window, WindowId};
use gpui_component::WindowExt as _;
use gpui_component::one_menu::{self, MenuKey};

type Close = Box<dyn FnOnce(&mut Window, &mut App)>;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    /// In place: a side panel, a strip over the composer, an inline editor.
    Place,
    /// Floating over the page: a picker, a popover, a menu.
    Popup,
    /// A dialog's own content: closes before the dialog does.
    Dialog,
}

struct Layer {
    level: Level,
    close: Close,
}

thread_local! {
    /// Each window's layers from its last frame, bottom first. Only windows whose root starts a frame (`frame`) have
    /// a list: the app's window, and the chat fixture that mirrors it.
    static LAYERS: RefCell<HashMap<WindowId, Vec<Layer>>> = RefCell::new(HashMap::new());
    /// Each window's popups, by frame (the same windows).
    static POPUPS: RefCell<HashMap<WindowId, Shown>> = RefCell::new(HashMap::new());
    /// Each window's ⌘S from its last frame: the save of the page showing a save bar (the same windows).
    static SAVES: RefCell<HashMap<WindowId, Save>> = RefCell::new(HashMap::new());
}

type Save = Rc<dyn Fn(&mut Window, &mut App)>;

/// The popups a window's frames show: which just opened, and which just closed.
#[derive(Default)]
pub struct Shown {
    before: HashSet<MenuKey>,
    now: HashSet<MenuKey>,
}

impl Shown {
    /// A new frame: those the frame before the last showed and the last didn't, closed.
    pub fn frame(&mut self) -> Vec<MenuKey> {
        let closed = self.before.difference(&self.now).copied().collect();
        self.before = std::mem::take(&mut self.now);
        closed
    }

    /// `key` shows in this frame: whether it just opened (the last frame didn't show it).
    pub fn show(&mut self, key: MenuKey) -> bool {
        self.now.insert(key) && !self.before.contains(&key)
    }
}

/// Esc that nothing else used closes the top layer of the window it was pressed in.
pub fn init(cx: &mut App) {
    cx.observe_keystrokes(|event, window, cx| {
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key == "escape" && event.action.is_none() && !modifiers.modified() {
            dismiss(window, cx);
        }
    })
    .detach();
    // Before the focused view's own keys, so no field or menu takes it; not under a dialog.
    cx.intercept_keystrokes(|event, window, cx| {
        let keys = &event.keystroke;
        let modifiers = keys.modifiers;
        if keys.key != "s"
            || !modifiers.platform
            || modifiers.shift
            || modifiers.alt
            || modifiers.control
            || window.has_active_dialog(cx)
        {
            return;
        }
        let id = window.window_handle().window_id();
        if let Some(save) = SAVES.with(|saves| saves.borrow().get(&id).cloned()) {
            cx.stop_propagation();
            save(window, cx);
            window.refresh();
        }
    })
    .detach();
}

/// A new frame of the window's root view: the layers start over. Call first thing in the root's render.
pub fn frame(window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    LAYERS.with(|layers| layers.borrow_mut().insert(id, Vec::new()));
    SAVES.with(|saves| saves.borrow_mut().remove(&id));
    // Popups the last frame no longer showed closed: gone from the one-open-menu rule.
    for key in POPUPS.with(|popups| popups.borrow_mut().entry(id).or_default().frame()) {
        one_menu::closed(key, cx);
    }
}

/// Something open in place (a side panel, an inline editor): Esc runs `close` while nothing above it is open.
pub fn layer<V: 'static>(
    window: &Window,
    cx: &Context<V>,
    close: impl FnOnce(&mut V, &mut Window, &mut Context<V>) + 'static,
) {
    push(window, cx, Level::Place, close);
}

/// Something floating over the page (a picker, a popover): above every `layer`, wherever it rendered. Also one menu
/// of the one-open-menu rule, by its view and this call's place: the frame it first shows, any other menu open closes,
/// and `close` runs when another opens. Its box takes `gpui_component::one_menu::surface()` first, so the scroll wheel
/// over it doesn't close it and the pointer over it is its alone (nothing under it hovers or takes the click).
#[track_caller]
pub fn popup<V: 'static>(
    window: &mut Window,
    cx: &mut Context<V>,
    close: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) {
    let key = MenuKey::of((cx.entity_id(), std::panic::Location::caller()));
    let id = window.window_handle().window_id();
    let framed = LAYERS.with(|layers| layers.borrow().contains_key(&id));
    let close = Rc::new(close);
    if framed && POPUPS.with(|popups| popups.borrow_mut().entry(id).or_default().show(key)) {
        let (view, close) = (cx.weak_entity(), close.clone());
        one_menu::opened(key, window, cx, move |window, cx| {
            view.update(cx, |view, cx| close(view, window, cx)).ok();
        });
    }
    push(window, cx, Level::Popup, move |view, window, cx| close(view, window, cx));
}

/// Within a dialog's content, like the image editor: Esc runs `close` instead of just closing the dialog.
pub fn dialog_layer<V: 'static>(
    window: &Window,
    cx: &Context<V>,
    close: impl FnOnce(&mut V, &mut Window, &mut Context<V>) + 'static,
) {
    push(window, cx, Level::Dialog, close);
}

/// A page with something to save (its save bar showing): ⌘S runs `save` this frame, wherever the keyboard is.
pub fn save<V: 'static>(
    window: &Window,
    cx: &Context<V>,
    save: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) {
    let id = window.window_handle().window_id();
    if !LAYERS.with(|layers| layers.borrow().contains_key(&id)) {
        return;
    }
    let view = cx.weak_entity();
    let save: Save = Rc::new(move |window, cx| {
        view.update(cx, |view, cx| save(view, window, cx)).ok();
    });
    SAVES.with(|saves| saves.borrow_mut().insert(id, save));
}

fn push<V: 'static>(
    window: &Window,
    cx: &Context<V>,
    level: Level,
    close: impl FnOnce(&mut V, &mut Window, &mut Context<V>) + 'static,
) {
    let view = cx.weak_entity();
    let close: Close = Box::new(move |window, cx| {
        view.update(cx, |view, cx| close(view, window, cx)).ok();
    });
    let id = window.window_handle().window_id();
    LAYERS.with(|layers| {
        if let Some(list) = layers.borrow_mut().get_mut(&id) {
            list.push(Layer { level, close });
        }
    });
}

/// Closes the window's top layer: an open dialog (or a layer in it) before anything under it. Returns whether
/// something closed. Also for Esc pressed where GPUI doesn't hear it (a web page passes it on).
pub fn dismiss(window: &mut Window, cx: &mut App) -> bool {
    // A menu open over the page (gpui-component's, or a popup here) first; a dialog's own Esc goes to it.
    if !window.has_active_dialog(cx) && one_menu::close_in(window, cx) {
        return true;
    }
    let id = window.window_handle().window_id();
    let dialog = window.has_active_dialog(cx);
    let top = LAYERS.with(|layers| {
        let mut layers = layers.borrow_mut();
        let list = layers.get_mut(&id)?;
        // While a dialog is up, only what's in it; otherwise the highest level, the last of it rendered.
        let level = if dialog {
            Level::Dialog
        } else {
            list.iter().map(|layer| layer.level).filter(|level| *level != Level::Dialog).max()?
        };
        let at = list.iter().rposition(|layer| layer.level == level)?;
        Some(list.remove(at))
    });
    match top {
        Some(layer) => (layer.close)(window, cx),
        None if dialog => window.close_dialog(cx),
        None => return false,
    }
    window.refresh();
    true
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};
    use gpui_component::one_menu::{MenuKey, OneOpen, outside_menus};

    use super::Shown;

    #[test]
    fn opening_a_menu_closes_the_one_open() {
        let (picture, badge, view) = (MenuKey::next(), MenuKey::next(), MenuKey::of("view"));
        let mut open = OneOpen::default();
        assert_eq!(open.open(badge, "close the badge's menu"), None);
        // The picture's menu opening while the badge's is open: the badge's closes.
        assert_eq!(open.open(picture, "close the picture's menu"), Some("close the badge's menu"));
        assert_eq!(open.key(), Some(picture));
        // The badge's own close comes late: the picture's stays open.
        assert!(!open.closed(badge));
        assert_eq!(open.key(), Some(picture));
        // A dropdown after it, then the same dropdown again (drawn again, still open): nothing more closes.
        assert_eq!(open.open(view, "close the dropdown"), Some("close the picture's menu"));
        assert_eq!(open.open(view, "close the dropdown"), None);
        assert!(open.closed(view));
        assert_eq!(open.key(), None);
    }

    #[test]
    fn only_the_open_menus_window_closes_it() {
        let mut open = OneOpen::default();
        open.open(MenuKey::next(), 1);
        assert_eq!(open.take_if(|window| *window == 2), None);
        assert_eq!(open.take_if(|window| *window == 1), Some(1));
        assert_eq!(open.key(), None);
    }

    #[test]
    fn a_popup_opens_the_frame_it_first_shows_and_closes_the_frame_it_doesnt() {
        let picker = MenuKey::of(("picker", 1));
        let mut shown = Shown::default();
        assert!(shown.frame().is_empty());
        assert!(shown.show(picker), "first shown: just opened");
        assert!(!shown.show(picker), "twice in a frame: once");
        assert!(shown.frame().is_empty());
        assert!(!shown.show(picker), "still open");
        assert!(shown.frame().is_empty());
        // A frame without it, then the next frame starts: it closed.
        assert_eq!(shown.frame(), vec![picker]);
        assert!(shown.frame().is_empty());
        assert!(shown.show(picker), "open again");
    }

    #[test]
    fn a_popup_opening_closes_a_gpui_component_menu() {
        let (context_menu, popup) = (MenuKey::next(), MenuKey::of(("popup", 2)));
        let (mut open, mut shown) = (OneOpen::default(), Shown::default());
        open.open(context_menu, "close the right-click menu");
        shown.frame();
        let closes = shown.show(popup).then(|| open.open(popup, "close the popup")).flatten();
        assert_eq!(closes, Some("close the right-click menu"));
        // The next frames draw it again: it doesn't open (or close anything) again.
        shown.frame();
        assert!(!shown.show(popup));
    }

    #[test]
    fn the_scroll_wheel_closes_a_menu_only_off_every_menu() {
        let menus = [Bounds::new(point(px(100.), px(260.)), size(px(160.), px(40.)))];
        assert!(!outside_menus(&menus, point(px(110.), px(272.))));
        assert!(outside_menus(&menus, point(px(520.), px(420.))));
        assert!(outside_menus(&[], point(px(110.), px(272.))));
    }

    #[test]
    fn keys_of_their_own_never_meet_named_ones() {
        let own = MenuKey::next();
        assert_ne!(own, MenuKey::next());
        assert_eq!(MenuKey::of(("view", 1)), MenuKey::of(("view", 1)));
        assert_ne!(MenuKey::of(("view", 1)), MenuKey::of(("view", 2)));
    }
}

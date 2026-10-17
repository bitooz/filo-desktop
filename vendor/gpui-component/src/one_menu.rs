//! Filo: one menu or popover open at a time, across the app.
//!
//! Every menu that floats over the page says so when it opens (`opened`) and when it closes (`closed`): a right-click
//! menu (`menu::ContextMenu`), a popover or dropdown menu (`Popover`, `DropdownMenu`), a text field's right-click menu,
//! and the app's own popups (its escape.rs). Opening one closes the one open before, in any window, so menus never
//! stack. The open one also closes on the scroll wheel anywhere but over a menu (`surface`), when its window stops being
//! the active one (`Root`), and on Esc nothing else used (`close_in`); a click elsewhere closes each kind by itself.
//!
//! A web view (a native view over everything GPUI draws) makes room for a menu over it by itself, as for anything
//! GPUI paints over it (gpui's `Window::painted_over`); a dialog or sheet opening takes the keyboard back from one
//! (`on_modal`).
//!
//! The pointer over a menu is the menu's alone: its `surface` keeps it from what's drawn under the menu, which doesn't
//! hover, show a tooltip, change the cursor, scroll or take a click there.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    AnyWindowHandle, App, Bounds, DispatchPhase, Global, HitboxBehavior, IntoElement,
    MouseDownEvent, Pixels, Point, ScrollWheelEvent, Styled as _, Window, WindowId, canvas,
};

/// What names a menu: the same key while it stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MenuKey(u64);

impl MenuKey {
    /// A key of its own, for a menu that keeps it (a right-click menu's state).
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed) | 1 << 63)
    }

    /// The key of what `what` names (an entity's id, a view and a place in the code).
    pub fn of(what: impl Hash) -> Self {
        let mut hasher = DefaultHasher::new();
        what.hash(&mut hasher);
        Self(hasher.finish() & !(1 << 63))
    }
}

/// The rule apart from windows: the one open menu, by key, with what closes it (`T`).
pub struct OneOpen<T> {
    open: Option<(MenuKey, T)>,
}

impl<T> Default for OneOpen<T> {
    fn default() -> Self {
        Self { open: None }
    }
}

impl<T> OneOpen<T> {
    /// `key` opened: what was open under another key, to close now.
    pub fn open(&mut self, key: MenuKey, close: T) -> Option<T> {
        self.open
            .replace((key, close))
            .filter(|(before, _)| *before != key)
            .map(|(_, close)| close)
    }

    /// `key` closed by itself: forgotten if it's the open one (not if another opened since).
    pub fn closed(&mut self, key: MenuKey) -> bool {
        let open = self.key() == Some(key);
        if open {
            self.open = None;
        }
        open
    }

    /// What closes the open menu, if `which` says it's the one to close; it's no longer open.
    pub fn take_if(&mut self, which: impl FnOnce(&T) -> bool) -> Option<T> {
        match &self.open {
            Some((_, close)) if which(close) => self.open.take().map(|(_, close)| close),
            _ => None,
        }
    }

    /// The open menu's key.
    pub fn key(&self) -> Option<MenuKey> {
        self.open.as_ref().map(|(key, _)| *key)
    }

    pub fn get(&self) -> Option<&T> {
        self.open.as_ref().map(|(_, close)| close)
    }
}

/// Whether `at` is off every menu (`surfaces`, where menus drew): the scroll wheel there closes the open one.
/// A right-click there passes through a right-click menu's backdrop.
pub fn outside_menus(surfaces: &[Bounds<Pixels>], at: Point<Pixels>) -> bool {
    !surfaces.iter().any(|bounds| bounds.contains(&at))
}

struct Open {
    window: AnyWindowHandle,
    close: Box<dyn FnOnce(&mut Window, &mut App)>,
}

/// Where a window's menus drew: in the frame being drawn so far (all of it between frames), and the one before.
#[derive(Default)]
struct Surfaces {
    this: Vec<Bounds<Pixels>>,
    last: Vec<Bounds<Pixels>>,
}

#[derive(Default)]
struct Menus {
    open: OneOpen<Open>,
    surfaces: HashMap<WindowId, Surfaces>,
    /// Each window's frames, counted by its `Root`.
    frames: HashMap<WindowId, u64>,
    /// A right-click on an open right-click menu's backdrop (`pass_through`), where it was.
    through: Option<(WindowId, Point<Pixels>)>,
}

impl Global for Menus {}

/// The menu `key` opened in `window`: the one open before closes. `close` closes this one when another opens (or on
/// the scroll wheel, Esc, the window going to the back); it runs later, never inside what opened the new one.
pub fn opened(
    key: MenuKey,
    window: &mut Window,
    cx: &mut App,
    close: impl FnOnce(&mut Window, &mut App) + 'static,
) {
    let open = Open {
        window: window.window_handle(),
        close: Box::new(close),
    };
    if let Some(before) = cx.default_global::<Menus>().open.open(key, open) {
        run(before, window, cx);
    }
}

/// The menu `key` closed by itself (a click elsewhere, a choice, Esc).
pub fn closed(key: MenuKey, cx: &mut App) {
    cx.default_global::<Menus>().open.closed(key);
}

/// Whether a menu is open in `window`.
pub fn is_open_in(window: &Window, cx: &App) -> bool {
    let here = window.window_handle();
    cx.try_global::<Menus>()
        .and_then(|menus| menus.open.get())
        .is_some_and(|open| open.window == here)
}

/// Closes the menu open in `window`: whether there was one.
pub fn close_in(window: &mut Window, cx: &mut App) -> bool {
    let here = window.window_handle();
    let Some(open) = cx
        .default_global::<Menus>()
        .open
        .take_if(|open| open.window == here)
    else {
        return false;
    };
    run(open, window, cx);
    true
}

fn run(open: Open, window: &mut Window, cx: &mut App) {
    let Open {
        window: handle,
        close,
    } = open;
    if handle == window.window_handle() {
        window.defer(cx, move |window, cx| {
            close(window, cx);
            window.refresh();
        });
    } else {
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    close(window, cx);
                    window.refresh();
                })
                .ok();
        });
    }
}

/// Goes first inside a menu's box (absolute, its whole size): the scroll wheel over it scrolls the menu, not closing it,
/// and the pointer over it is the menu's alone. Nothing drawn under the menu hovers, shows a tooltip, changes the cursor,
/// scrolls or takes a click there, as under GPUI's `occlude`: a hitbox that blocks the mouse, over the box's own and
/// under what the menu draws after it (its items), so a menu needn't occlude itself (the app's own popups don't).
pub fn surface() -> impl IntoElement {
    canvas(
        |bounds, window, cx| {
            let id = window.window_handle().window_id();
            cx.default_global::<Menus>()
                .surfaces
                .entry(id)
                .or_default()
                .this
                .push(bounds);
            window.insert_hitbox(bounds, HitboxBehavior::BlockMouse);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Where `window`'s last frame drew menus (each says so with `surface`), between frames.
fn drawn_boxes<'a>(window: &Window, cx: &'a App) -> &'a [Bounds<Pixels>] {
    let id = window.window_handle().window_id();
    cx.try_global::<Menus>()
        .and_then(|menus| menus.surfaces.get(&id))
        .map_or(&[], |surfaces| surfaces.this.as_slice())
}

/// How many menus `window`'s last frame drew, for checks: one at most.
pub fn drawn(window: &Window, cx: &App) -> usize {
    drawn_boxes(window, cx).len()
}

/// Where menus are over `window`, while it draws a frame: this frame's so far and the last one's (menus are deferred,
/// so they draw after the rest). Checks find a menu's box with it.
pub fn boxes(window: &Window, cx: &App) -> Vec<Bounds<Pixels>> {
    let id = window.window_handle().window_id();
    let Some(surfaces) = cx.try_global::<Menus>().and_then(|menus| menus.surfaces.get(&id)) else {
        return Vec::new();
    };
    let mut boxes = surfaces.this.clone();
    boxes.extend(surfaces.last.iter().filter(|last| !surfaces.this.contains(last)));
    boxes
}

/// What the app does as a dialog or sheet opens (`on_modal`): a web view is a native view, and one that has the
/// keyboard keeps it from GPUI, so Esc wouldn't close the dialog.
struct ModalHook(Rc<dyn Fn(&Window)>);

impl Global for ModalHook {}

/// Sets what happens as a dialog or sheet opens: the app's web views give the keyboard back to GPUI's view.
pub fn on_modal(cx: &mut App, hook: impl Fn(&Window) + 'static) {
    cx.set_global(ModalHook(Rc::new(hook)));
}

pub(crate) fn modal_opened(window: &Window, cx: &App) {
    if let Some(hook) = cx.try_global::<ModalHook>().map(|hook| hook.0.clone()) {
        hook(window);
    }
}

/// The window's frame, as its `Root` starts drawing it (where its menus drew starts over); 0 for a window without one.
pub fn frame(window: &Window, cx: &App) -> u64 {
    let id = window.window_handle().window_id();
    cx.try_global::<Menus>()
        .and_then(|menus| menus.frames.get(&id).copied())
        .unwrap_or(0)
}

pub(crate) fn start_frame(window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let menus = cx.default_global::<Menus>();
    *menus.frames.entry(id).or_default() += 1;
    let surfaces = menus.surfaces.entry(id).or_default();
    surfaces.last = std::mem::take(&mut surfaces.this);
}

/// A right-click on an open right-click menu's backdrop at `at`: the menu closes, and a right-click menu under it may
/// open there (`passed_through`), as when nothing was open. Not on a menu itself.
pub(crate) fn pass_through(at: Point<Pixels>, window: &Window, cx: &mut App) {
    if outside_menus(drawn_boxes(window, cx), at) {
        cx.default_global::<Menus>().through = Some((window.window_handle().window_id(), at));
    }
}

pub(crate) fn passed_through(at: Point<Pixels>, window: &Window, cx: &App) -> bool {
    let id = window.window_handle().window_id();
    cx.try_global::<Menus>()
        .and_then(|menus| menus.through)
        .is_some_and(|through| through == (id, at))
}

/// Goes first in the window's `Root`: the scroll wheel anywhere but over a menu closes the open one.
pub(crate) fn watch() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|event: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !is_open_in(window, cx) {
                    return;
                }
                if outside_menus(drawn_boxes(window, cx), event.position) {
                    close_in(window, cx);
                }
            });
            window.on_mouse_event(|_: &MouseDownEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    cx.default_global::<Menus>().through = None;
                }
            });
        },
    )
    .absolute()
    .size_0()
}

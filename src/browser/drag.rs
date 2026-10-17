// An agent's drag in a page (browser_drag, browser.rs `drag`): the mouse part is real AppKit events handed to the web
// view (native.rs), pressed, dragged and let go. When the page starts an HTML5 drag and drop, WebKit asks AppKit for a
// drag session (`dragImage:at:offset:event:pasteboard:source:slideBack:`, or `beginDraggingSessionWithItems:event:source:`
// for an attachment), which would follow the user's real pointer across the screen. For a view an agent is dragging
// in, the web view's class answers those itself instead (`hook`), and the drag goes on the way AppKit would carry it:
// `draggingEntered:`, `draggingUpdated:` at each move, `performDragOperation:` (or `draggingExited:`) at the end and
// the source's ended call, with a dragging info of Filo's own (`FiloBrowserDragInfo`), as WebKit's own tests do. So
// the page gets WebKit's real drag events (dragstart, dragover, drop, dragend, with their data) and the user's pointer
// and screen are left alone. Any other drag (the user's, in the browser window) goes to AppKit as before.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_char;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Imp, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSSize};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    /// The pasteboard a drag carries its data on, which WebKit wrote the page's to.
    static NSPasteboardNameDrag: &'static objc2_foundation::NSString;
}

/// NSDragOperationEvery.
const EVERY: usize = usize::MAX;

/// How WebKit began the drag it asked AppKit for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Began {
    /// Not (yet): the drag is the mouse's.
    No,
    /// `dragImage:…`, ended with `draggedImage:endedAt:operation:`.
    Image,
    /// `beginDraggingSessionWithItems:…`, ended with `draggingSession:endedAtPoint:operation:`.
    Session,
}

/// What the dragging info answers for one drag.
struct Info {
    window: *mut AnyObject,
    view: *mut AnyObject,
    pasteboard: Retained<AnyObject>,
    /// In the window's coordinates.
    location: NSPoint,
    mask: usize,
    sequence: isize,
    valid_items: isize,
    formation: isize,
    animates: bool,
}

thread_local! {
    /// The web views an agent is dragging in now (by address), and how WebKit began its drag there.
    static DRAGGING: RefCell<HashMap<usize, Began>> = RefCell::new(HashMap::new());
    /// Each hooked class's own implementation of the two drag starts, by class address and selector name.
    static OWN: RefCell<HashMap<(usize, &'static str), Imp>> = RefCell::new(HashMap::new());
    /// Each live dragging info's answers, by its address.
    static INFOS: RefCell<HashMap<usize, Info>> = RefCell::new(HashMap::new());
}

static NEXT_SEQUENCE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(1);

/// The implementation `this`'s class (or the nearest hooked class above it: KVO puts a subclass in between) had of
/// `name` before the hook.
fn own(this: &AnyObject, name: &'static str) -> Option<Imp> {
    let mut class: Option<&AnyClass> = Some(this.class());
    while let Some(at) = class {
        if let Some(imp) = OWN.with(|own| own.borrow().get(&(at as *const AnyClass as usize, name)).copied()) {
            return Some(imp);
        }
        class = at.superclass();
    }
    None
}

/// Whether an agent is dragging in `this`: then it's marked as begun `how`.
fn take_over(this: &AnyObject, how: Began) -> bool {
    DRAGGING.with(|dragging| match dragging.borrow_mut().get_mut(&(this as *const AnyObject as usize)) {
        Some(began) => {
            *began = how;
            true
        }
        None => false,
    })
}

type DragImage = unsafe extern "C-unwind" fn(
    &AnyObject,
    Sel,
    *mut AnyObject,
    NSPoint,
    NSSize,
    *mut AnyObject,
    *mut AnyObject,
    *mut AnyObject,
    Bool,
);
type BeginSession =
    unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject) -> *mut AnyObject;

#[allow(clippy::too_many_arguments)]
extern "C-unwind" fn drag_image(
    this: &AnyObject,
    sel: Sel,
    image: *mut AnyObject,
    at: NSPoint,
    offset: NSSize,
    event: *mut AnyObject,
    pasteboard: *mut AnyObject,
    source: *mut AnyObject,
    slide_back: Bool,
) {
    if take_over(this, Began::Image) {
        return;
    }
    if let Some(imp) = own(this, "dragImage") {
        // SAFETY: the class's own implementation of this selector, with its signature.
        unsafe {
            std::mem::transmute::<Imp, DragImage>(imp)(
                this, sel, image, at, offset, event, pasteboard, source, slide_back,
            )
        }
    }
}

extern "C-unwind" fn begin_session(
    this: &AnyObject,
    sel: Sel,
    items: *mut AnyObject,
    event: *mut AnyObject,
    source: *mut AnyObject,
) -> *mut AnyObject {
    if take_over(this, Began::Session) {
        return std::ptr::null_mut();
    }
    match own(this, "beginDraggingSession") {
        // SAFETY: the class's own implementation of this selector, with its signature.
        Some(imp) => unsafe { std::mem::transmute::<Imp, BeginSession>(imp)(this, sel, items, event, source) },
        None => std::ptr::null_mut(),
    }
}

/// Puts the two drag starts of `view`'s class in Filo's hands (once per class); each still goes to AppKit unless an
/// agent is dragging in that view.
fn hook(view: &AnyObject) -> bool {
    let class = view.class();
    let key = class as *const AnyClass as usize;
    // SAFETY: each stand-in has the signature of the method it stands in for (NSView's), and keeps calling the class's
    // own implementation for every view an agent isn't dragging in; the runtime calls happen on the main thread.
    unsafe {
        let swaps: [(&'static str, Sel, Imp); 2] = [
            (
                "dragImage",
                sel!(dragImage:at:offset:event:pasteboard:source:slideBack:),
                std::mem::transmute::<DragImage, Imp>(drag_image as DragImage),
            ),
            (
                "beginDraggingSession",
                sel!(beginDraggingSessionWithItems:event:source:),
                std::mem::transmute::<BeginSession, Imp>(begin_session as BeginSession),
            ),
        ];
        for (name, selector, stand_in) in swaps {
            if OWN.with(|own| own.borrow().contains_key(&(key, name))) {
                continue;
            }
            let Some(method) = class.instance_method(selector) else { return false };
            let own = method.implementation();
            let types: *const c_char = objc2::ffi::method_getTypeEncoding(method as *const objc2::runtime::Method);
            objc2::ffi::class_replaceMethod(class as *const AnyClass as *mut AnyClass, selector, stand_in, types);
            OWN.with(|map| map.borrow_mut().insert((key, name), own));
        }
    }
    true
}

fn info_of<R>(this: &AnyObject, read: impl FnOnce(&mut Info) -> R) -> Option<R> {
    INFOS.with(|infos| infos.borrow_mut().get_mut(&(this as *const AnyObject as usize)).map(read))
}

/// The class of the dragging info handed to the web view: NSDraggingInfo's methods, answered from `INFOS`.
fn info_class() -> Option<&'static AnyClass> {
    static CLASS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let name = c"FiloBrowserDragInfo";
        if let Some(class) = AnyClass::get(name) {
            return Some(class as *const AnyClass as usize);
        }
        let mut builder = ClassBuilder::new(name, AnyClass::get(c"NSObject")?)?;
        if let Some(protocol) = objc2::runtime::AnyProtocol::get(c"NSDraggingInfo") {
            builder.add_protocol(protocol);
        }
        extern "C-unwind" fn window(this: &AnyObject, _: Sel) -> *mut AnyObject {
            info_of(this, |info| info.window).unwrap_or(std::ptr::null_mut())
        }
        extern "C-unwind" fn mask(this: &AnyObject, _: Sel) -> usize {
            info_of(this, |info| info.mask).unwrap_or(EVERY)
        }
        extern "C-unwind" fn location(this: &AnyObject, _: Sel) -> NSPoint {
            info_of(this, |info| info.location).unwrap_or(NSPoint::new(0., 0.))
        }
        extern "C-unwind" fn image(_: &AnyObject, _: Sel) -> *mut AnyObject {
            std::ptr::null_mut()
        }
        extern "C-unwind" fn pasteboard(this: &AnyObject, _: Sel) -> *mut AnyObject {
            info_of(this, |info| Retained::as_ptr(&info.pasteboard) as *mut AnyObject).unwrap_or(std::ptr::null_mut())
        }
        extern "C-unwind" fn source(this: &AnyObject, _: Sel) -> *mut AnyObject {
            info_of(this, |info| info.view).unwrap_or(std::ptr::null_mut())
        }
        extern "C-unwind" fn sequence(this: &AnyObject, _: Sel) -> isize {
            info_of(this, |info| info.sequence).unwrap_or(0)
        }
        extern "C-unwind" fn slide(_: &AnyObject, _: Sel, _: NSPoint) {}
        extern "C-unwind" fn promised(_: &AnyObject, _: Sel, _: *mut AnyObject) -> *mut AnyObject {
            std::ptr::null_mut()
        }
        extern "C-unwind" fn formation(this: &AnyObject, _: Sel) -> isize {
            info_of(this, |info| info.formation).unwrap_or(0)
        }
        extern "C-unwind" fn set_formation(this: &AnyObject, _: Sel, value: isize) {
            info_of(this, |info| info.formation = value);
        }
        extern "C-unwind" fn animates(this: &AnyObject, _: Sel) -> Bool {
            Bool::new(info_of(this, |info| info.animates).unwrap_or(false))
        }
        extern "C-unwind" fn set_animates(this: &AnyObject, _: Sel, value: Bool) {
            info_of(this, |info| info.animates = value.as_bool());
        }
        extern "C-unwind" fn valid_items(this: &AnyObject, _: Sel) -> isize {
            info_of(this, |info| info.valid_items).unwrap_or(1)
        }
        extern "C-unwind" fn set_valid_items(this: &AnyObject, _: Sel, value: isize) {
            info_of(this, |info| info.valid_items = value);
        }
        extern "C-unwind" fn enumerate(
            _: &AnyObject,
            _: Sel,
            _: usize,
            _: *mut AnyObject,
            _: *mut AnyObject,
            _: *mut AnyObject,
            _: *mut AnyObject,
        ) {
        }
        extern "C-unwind" fn highlight(_: &AnyObject, _: Sel) -> isize {
            0
        }
        extern "C-unwind" fn nothing(_: &AnyObject, _: Sel) {}
        // SAFETY: each method's signature matches NSDraggingInfo's.
        unsafe {
            builder.add_method(sel!(draggingDestinationWindow), window as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingSourceOperationMask), mask as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingLocation), location as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggedImageLocation), location as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggedImage), image as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingPasteboard), pasteboard as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingSource), source as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingSequenceNumber), sequence as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(slideDraggedImageTo:), slide as extern "C-unwind" fn(_, _, _));
            builder.add_method(
                sel!(namesOfPromisedFilesDroppedAtDestination:),
                promised as extern "C-unwind" fn(_, _, _) -> _,
            );
            builder.add_method(sel!(draggingFormation), formation as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(setDraggingFormation:), set_formation as extern "C-unwind" fn(_, _, _));
            builder.add_method(sel!(animatesToDestination), animates as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(setAnimatesToDestination:), set_animates as extern "C-unwind" fn(_, _, _));
            builder.add_method(sel!(numberOfValidItemsForDrop), valid_items as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(setNumberOfValidItemsForDrop:), set_valid_items as extern "C-unwind" fn(_, _, _));
            builder.add_method(
                sel!(enumerateDraggingItemsWithOptions:forView:classes:searchOptions:usingBlock:),
                enumerate as extern "C-unwind" fn(_, _, _, _, _, _, _),
            );
            builder.add_method(sel!(springLoadingHighlight), highlight as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(resetSpringLoading), nothing as extern "C-unwind" fn(_, _));
        }
        Some(builder.register() as *const AnyClass as usize)
    });
    // SAFETY: a registered class lives as long as the process.
    class.map(|class| unsafe { &*(class as *const AnyClass) })
}

/// Where (x, y), points from the page's top left, is in its window's coordinates.
fn in_window(view: &AnyObject, x: f64, y: f64) -> NSPoint {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let bounds: objc2_foundation::NSRect = msg_send![view, bounds];
        let frame: objc2_foundation::NSRect =
            msg_send![view, convertRect: bounds, toView: std::ptr::null_mut::<AnyObject>()];
        NSPoint::new(frame.origin.x + x, frame.origin.y + frame.size.height - y)
    }
}

fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: a read-only query.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// An agent's drag in a web view, from just before it's pressed until it's let go. Dropping it ends a drag and drop
/// WebKit began (with no drop) if `let_go` didn't.
pub struct Drag {
    view: Retained<AnyObject>,
    info: Option<Retained<AnyObject>>,
    ended: bool,
}

impl Drag {
    /// An error when an agent is already dragging in `view`, or its class can't be hooked.
    pub fn begin(view: &AnyObject) -> Result<Drag, String> {
        let key = view as *const AnyObject as usize;
        if DRAGGING.with(|dragging| dragging.borrow().contains_key(&key)) {
            return Err("Another drag is going on in this tab: wait for it to end.".into());
        }
        if !hook(view) {
            return Err("Couldn't drag in this tab: its page's view takes no drags.".into());
        }
        DRAGGING.with(|dragging| dragging.borrow_mut().insert(key, Began::No));
        // SAFETY: retaining a live view.
        let view = unsafe { Retained::retain(view as *const AnyObject as *mut AnyObject) }.ok_or("no view")?;
        Ok(Drag { view, info: None, ended: false })
    }

    fn began(&self) -> Began {
        DRAGGING.with(|dragging| {
            dragging.borrow().get(&(Retained::as_ptr(&self.view) as usize)).copied().unwrap_or(Began::No)
        })
    }

    /// Whether the page started a drag and drop (WebKit asked for a drag session).
    pub fn dropping(&self) -> bool {
        self.began() != Began::No
    }

    /// The drag and drop moved to (x, y), points from the page's top left: it enters the page the first time.
    /// Returns the operation the page last said it would take (none is 0).
    pub fn moved(&mut self, x: f64, y: f64) -> usize {
        let view = &*self.view;
        let location = in_window(view, x, y);
        // SAFETY: AppKit and WebKit on the main thread; the info is retained while it's in use, and its answers are
        // removed from `INFOS` only when it's done (`end`).
        unsafe {
            match &self.info {
                Some(info) => {
                    info_of(info, |answers| answers.location = location);
                    self.source_moved(location);
                    msg_send![view, draggingUpdated: &**info]
                }
                None => {
                    let Some(class) = info_class() else { return 0 };
                    let info: Option<Retained<AnyObject>> = msg_send![class, new];
                    let Some(info) = info else { return 0 };
                    let window: *mut AnyObject = msg_send![view, window];
                    let pasteboard: Option<Retained<AnyObject>> =
                        msg_send![class!(NSPasteboard), pasteboardWithName: NSPasteboardNameDrag];
                    let Some(pasteboard) = pasteboard else { return 0 };
                    // What the source allows within the app, as AppKit asks it (WebKit: generic, move and copy).
                    let mask: usize = if responds(view, sel!(draggingSession:sourceOperationMaskForDraggingContext:)) {
                        msg_send![view, draggingSession: std::ptr::null_mut::<AnyObject>(), sourceOperationMaskForDraggingContext: 1isize]
                    } else {
                        EVERY
                    };
                    let items: Option<Retained<AnyObject>> = msg_send![&*pasteboard, pasteboardItems];
                    let valid_items: usize = items.map_or(1, |items| msg_send![&*items, count]);
                    INFOS.with(|infos| {
                        infos.borrow_mut().insert(
                            Retained::as_ptr(&info) as usize,
                            Info {
                                window,
                                view: Retained::as_ptr(&self.view) as *mut AnyObject,
                                pasteboard,
                                location,
                                mask: if mask == 0 { EVERY } else { mask },
                                sequence: NEXT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                                valid_items: valid_items.max(1) as isize,
                                formation: 0,
                                animates: false,
                            },
                        )
                    });
                    let operation: usize = msg_send![view, draggingEntered: &*info];
                    self.info = Some(info);
                    operation
                }
            }
        }
    }

    /// Tells the page's drag source it moved (the page's `drag` events), as AppKit does.
    fn source_moved(&self, location: NSPoint) {
        let view = &*self.view;
        let screen = self.on_screen(location);
        // SAFETY: AppKit and WebKit on the main thread, each call only when the view answers it.
        unsafe {
            match self.began() {
                Began::Image if responds(view, sel!(draggedImage:movedTo:)) => {
                    let _: () = msg_send![view, draggedImage: std::ptr::null_mut::<AnyObject>(), movedTo: screen];
                }
                Began::Session if responds(view, sel!(draggingSession:movedToPoint:)) => {
                    let _: () =
                        msg_send![view, draggingSession: std::ptr::null_mut::<AnyObject>(), movedToPoint: screen];
                }
                _ => {}
            }
        }
    }

    fn on_screen(&self, location: NSPoint) -> NSPoint {
        // SAFETY: AppKit on the main thread.
        unsafe {
            let window: *mut AnyObject = msg_send![&*self.view, window];
            if window.is_null() {
                return location;
            }
            msg_send![window, convertPointToScreen: location]
        }
    }

    /// Lets go at the last spot: dropped there when the page last said it takes the drop (`operation`), else the
    /// drag leaves. Returns whether it was dropped.
    pub fn let_go(&mut self, operation: usize) -> bool {
        let Some(info) = self.info.clone() else { return false };
        let view = &*self.view;
        // SAFETY: AppKit and WebKit on the main thread; the info is alive (held above).
        let dropped = unsafe {
            if operation != 0 {
                let ready: bool = if responds(view, sel!(prepareForDragOperation:)) {
                    msg_send![view, prepareForDragOperation: &*info]
                } else {
                    true
                };
                let performed: bool = ready && msg_send![view, performDragOperation: &*info];
                if responds(view, sel!(concludeDragOperation:)) {
                    let _: () = msg_send![view, concludeDragOperation: &*info];
                }
                performed
            } else {
                let _: () = msg_send![view, draggingExited: &*info];
                false
            }
        };
        self.end(if dropped { operation } else { 0 });
        dropped
    }

    /// The source's end of the drag (the page's `dragend`), as AppKit tells it when a drag session is over.
    fn end(&mut self, operation: usize) {
        if self.ended || self.began() == Began::No {
            return;
        }
        self.ended = true;
        let view = &*self.view;
        let location = self
            .info
            .as_ref()
            .and_then(|info| info_of(info, |answers| answers.location))
            .unwrap_or(NSPoint::new(0., 0.));
        let screen = self.on_screen(location);
        // SAFETY: AppKit and WebKit on the main thread, each call only when the view answers it.
        unsafe {
            match self.began() {
                Began::Image if responds(view, sel!(draggedImage:endedAt:operation:)) => {
                    let _: () = msg_send![view, draggedImage: std::ptr::null_mut::<AnyObject>(), endedAt: screen, operation: operation];
                }
                Began::Session if responds(view, sel!(draggingSession:endedAtPoint:operation:)) => {
                    let _: () = msg_send![view, draggingSession: std::ptr::null_mut::<AnyObject>(), endedAtPoint: screen, operation: operation];
                }
                _ => {}
            }
        }
    }
}

impl Drop for Drag {
    fn drop(&mut self) {
        if self.info.is_some() && !self.ended {
            // SAFETY: as in `let_go`: the drag leaves the page.
            unsafe {
                let info = self.info.clone().expect("info");
                let _: () = msg_send![&*self.view, draggingExited: &*info];
            }
        }
        self.end(0);
        if let Some(info) = &self.info {
            INFOS.with(|infos| infos.borrow_mut().remove(&(Retained::as_ptr(info) as usize)));
        }
        DRAGGING.with(|dragging| dragging.borrow_mut().remove(&(Retained::as_ptr(&self.view) as usize)));
    }
}

// The browser's AppKit and WebKit side (browser.rs): each tab's WKWebView lives in a window of its own far off the
// screen while nobody looks at it, which WebKit takes for a visible, focused window (`FiloBrowserPark`'s
// `isKeyWindow`), with occlusion checks and background throttling off, so its page runs at full speed and takes
// keys. Input is AppKit events made here and handed to the web view itself: the page gets real (trusted) mouse and
// key events, without the user's pointer moving, the app coming forward or the Accessibility permission an outside
// event poster needs.

use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Where the parked windows are: far off every screen.
const PARK_AT: f64 = -30000.;

/// A `CGImageRef`, as AppKit returns it.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CgImage(*const c_void);

// SAFETY: a pointer to an opaque CGImage, as AppKit declares it.
unsafe impl objc2::Encode for CgImage {
    const ENCODING: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGImage", &[]));
}

/// The class of the windows tabs are parked in: an NSWindow that says it's the key window, so WebKit counts its page
/// as focused and active (it then takes keys and shows a caret), as WebKit's own tests do for their offscreen views, and
/// that's on the main screen.
fn park_class() -> Option<&'static AnyClass> {
    static CLASS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let name = c"FiloBrowserPark";
        if let Some(class) = AnyClass::get(name) {
            return Some(class as *const AnyClass as usize);
        }
        let mut builder = ClassBuilder::new(name, AnyClass::get(c"NSWindow")?)?;
        extern "C-unwind" fn yes(_: &AnyObject, _: Sel) -> Bool {
            Bool::YES
        }
        // Off every screen its screen is nil, which wry unwraps when a page opens a pop-up (it reads the screen's frame
        // for the new window's place): the main screen stands in.
        extern "C-unwind" fn screen(_: &AnyObject, _: Sel) -> *mut AnyObject {
            // SAFETY: a read-only AppKit query on the main thread.
            unsafe { msg_send![class!(NSScreen), mainScreen] }
        }
        // SAFETY: each method's signature matches AppKit's (a BOOL, an NSScreen, from no arguments).
        unsafe {
            builder.add_method(sel!(isKeyWindow), yes as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(screen), screen as extern "C-unwind" fn(_, _) -> _);
        }
        Some(builder.register() as *const AnyClass as usize)
    });
    // SAFETY: a registered class lives as long as the process.
    class.map(|class| unsafe { &*(class as *const AnyClass) })
}

/// A window a tab's web view is parked in, `width` × `height` points, off the screen.
pub struct Park {
    window: Retained<AnyObject>,
}

impl Park {
    pub fn new(width: f64, height: f64) -> Option<Park> {
        let class = park_class()?;
        // NSWindowStyleMaskBorderless, NSBackingStoreBuffered
        const BORDERLESS: usize = 0;
        const BUFFERED: usize = 2;
        // NSWindowCollectionBehaviorTransient | Stationary | IgnoresCycle: not in Mission Control or ⌘` cycling.
        const BEHAVIOR: usize = (1 << 3) | (1 << 4) | (1 << 6);
        // SAFETY: AppKit on the main thread; the window is kept by `Park` (released when closed: no).
        unsafe {
            let rect = NSRect::new(NSPoint::new(PARK_AT, PARK_AT), NSSize::new(width, height));
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            let window: Option<Retained<AnyObject>> =
                msg_send![allocated, initWithContentRect: rect, styleMask: BORDERLESS, backing: BUFFERED, defer: false];
            let window = window?;
            let _: () = msg_send![&*window, setReleasedWhenClosed: false];
            // Stays up while Filo is hidden (closing its window hides it), or the page would count as hidden.
            let _: () = msg_send![&*window, setCanHide: false];
            let _: () = msg_send![&*window, setExcludedFromWindowsMenu: true];
            let _: () = msg_send![&*window, setCollectionBehavior: BEHAVIOR];
            let _: () = msg_send![&*window, setIgnoresMouseEvents: true];
            let _: () = msg_send![&*window, setHasShadow: false];
            let _: () = msg_send![&*window, orderFrontRegardless];
            Some(Park { window })
        }
    }

    /// Its content view, which a web view is put in.
    fn content(&self) -> *mut AnyObject {
        // SAFETY: AppKit on the main thread; the window is alive.
        unsafe { msg_send![&*self.window, contentView] }
    }

    /// The window as wry takes a parent: its content view.
    pub fn handle(&self) -> ParentView {
        ParentView(self.content())
    }

    /// Puts `view` back in it, filling it, with the keyboard.
    pub fn take(&self, view: &AnyObject, size: Option<(f64, f64)>) {
        // SAFETY: AppKit on the main thread; `view` is kept alive by its tab while it moves (a removed view isn't freed
        // while something holds it).
        unsafe {
            if let Some((width, height)) = size {
                let frame = NSRect::new(NSPoint::new(PARK_AT, PARK_AT), NSSize::new(width.max(200.), height.max(200.)));
                let _: () = msg_send![&*self.window, setFrame: frame, display: false];
            }
            let content = self.content();
            let superview: *mut AnyObject = msg_send![view, superview];
            if superview != content {
                let _: () = msg_send![view, removeFromSuperview];
                let _: () = msg_send![content, addSubview: view];
            }
            let bounds: NSRect = msg_send![content, bounds];
            let _: () = msg_send![view, setFrame: bounds];
            // Width and height follow the window.
            let _: () = msg_send![view, setAutoresizingMask: 18usize];
            let _: bool = msg_send![&*self.window, makeFirstResponder: view];
        }
    }
}

impl Drop for Park {
    fn drop(&mut self) {
        // SAFETY: AppKit on the main thread.
        unsafe {
            let _: () = msg_send![&*self.window, orderOut: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![&*self.window, close];
        }
    }
}

/// A view wry puts its web view in (`build_as_child`).
pub struct ParentView(*mut AnyObject);

impl raw_window_handle::HasWindowHandle for ParentView {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let view = NonNull::new(self.0.cast::<c_void>()).ok_or(raw_window_handle::HandleError::Unavailable)?;
        let raw = raw_window_handle::RawWindowHandle::AppKit(raw_window_handle::AppKitWindowHandle::new(view));
        // SAFETY: the view outlives the call wry makes with it (its window is the tab's, kept by the tab).
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

/// Sets up a new tab's web view: WebKit's check of whether its window is covered goes (it's never on a screen, and a
/// covered page is throttled), and it gets the keyboard of its window.
pub fn prepare(view: &AnyObject) {
    // SAFETY: AppKit and WebKit on the main thread; the private setter is called only where WebKit has it.
    unsafe {
        let occlusion = sel!(_setWindowOcclusionDetectionEnabled:);
        let has: bool = msg_send![view, respondsToSelector: occlusion];
        if has {
            let _: () = msg_send![view, _setWindowOcclusionDetectionEnabled: false];
        }
        let _: () = msg_send![view, setAllowsBackForwardNavigationGestures: true];
        // WebKit reads again whether its page shows when the view is hidden and shown: so it reads it with the check
        // off (it was in its window already, counted as covered).
        let _: () = msg_send![view, setHidden: true];
        let _: () = msg_send![view, setHidden: false];
        let window: *mut AnyObject = msg_send![view, window];
        if !window.is_null() {
            let _: bool = msg_send![window, makeFirstResponder: view];
        }
    }
}

/// Shows `view` in another window's view (`parent`, GPUI's), at `frame` (points from the parent's top left), with the
/// key stop after it in the responder chain.
pub fn show_in(view: &AnyObject, parent: *mut AnyObject, (x, y, width, height): (f64, f64, f64, f64)) {
    // SAFETY: AppKit on the main thread; `parent` is GPUI's view, alive with its window, and the tab keeps `view`.
    unsafe {
        let superview: *mut AnyObject = msg_send![view, superview];
        if superview != parent {
            let _: () = msg_send![view, removeFromSuperview];
            let _: () = msg_send![parent, addSubview: view];
            let _: () = msg_send![view, setAutoresizingMask: 0usize];
        }
        // Keys the page leaves unused end at the key stop; the rest of the chain is GPUI's view's.
        if let Some(stop) = key_stop() {
            let _: () = msg_send![stop, setNextResponder: parent];
            let next: *mut AnyObject = msg_send![view, nextResponder];
            if !std::ptr::eq(next, stop) {
                let _: () = msg_send![view, setNextResponder: stop];
            }
        }
        let flipped: bool = msg_send![parent, isFlipped];
        let bounds: NSRect = msg_send![parent, bounds];
        let top = if flipped { y } else { bounds.size.height - y - height };
        let frame = NSRect::new(NSPoint::new(x, top), NSSize::new(width.max(1.), height.max(1.)));
        let now: NSRect = msg_send![view, frame];
        if now != frame {
            let _: () = msg_send![view, setFrame: frame];
        }
    }
}

/// Whether `view` is in `parent` now.
pub fn is_in(view: &AnyObject, parent: *mut AnyObject) -> bool {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let superview: *mut AnyObject = msg_send![view, superview];
        superview == parent
    }
}

/// Whether the page (or a view inside it) has its window's keyboard.
pub fn has_keyboard(view: &AnyObject) -> bool {
    // SAFETY: AppKit read-only queries on the main thread; the first responder may be the window itself, not a view.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            return false;
        }
        let responder: *mut AnyObject = msg_send![window, firstResponder];
        if responder.is_null() {
            return false;
        }
        let is_view: bool = msg_send![responder, isKindOfClass: class!(NSView)];
        is_view && {
            let inside: bool = msg_send![responder, isDescendantOf: view];
            inside
        }
    }
}

/// The responder between a page in the browser window and GPUI's view (`show_in`). A key the page leaves unused goes
/// up the responder chain (WebKit sends it on once the page is done with it), and in GPUI's view to whatever has
/// GPUI's focus: a Return the page took care of itself (Google's sign-in's Next) went on to the address bar, which
/// opened its address again and reloaded the page. Here it ends, as in a browser; everything else goes on to GPUI.
fn key_stop() -> Option<&'static AnyObject> {
    static STOP: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let stop = STOP.get_or_init(|| {
        let name = c"FiloBrowserKeyStop";
        let class = match AnyClass::get(name) {
            Some(class) => class,
            None => {
                let mut builder = ClassBuilder::new(name, AnyClass::get(c"NSResponder")?)?;
                extern "C-unwind" fn unused(_: &AnyObject, _: Sel, _: *mut AnyObject) {}
                // SAFETY: each method's signature matches AppKit's (no result from an NSEvent).
                unsafe {
                    builder.add_method(sel!(keyDown:), unused as extern "C-unwind" fn(_, _, _));
                    builder.add_method(sel!(keyUp:), unused as extern "C-unwind" fn(_, _, _));
                }
                builder.register()
            }
        };
        // SAFETY: AppKit on the main thread; the one instance is kept for the life of the process.
        let stop: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
        stop.map(|stop| Retained::into_raw(stop) as usize)
    });
    // SAFETY: never released (above).
    stop.map(|stop| unsafe { &*(stop as *const AnyObject) })
}

/// Watches for clicks in the window `view` is in: `clicked` runs on each, as AppKit is about to hand it on. It stops
/// when dropped.
pub struct ClickWatch(Retained<AnyObject>);

impl ClickWatch {
    pub fn new(view: *mut AnyObject, clicked: impl Fn() + 'static) -> Option<ClickWatch> {
        // NSEventMaskLeftMouseDown | RightMouseDown | OtherMouseDown
        const DOWN: u64 = (1 << 1) | (1 << 3) | (1 << 25);
        // SAFETY: AppKit on the main thread; `view` is alive with its window.
        let window: *mut AnyObject = unsafe { msg_send![view, window] };
        if window.is_null() {
            return None;
        }
        let window = window as usize;
        let watch = block2::RcBlock::new(move |event: NonNull<AnyObject>| -> *mut AnyObject {
            // SAFETY: AppKit calls a monitor on the main thread with a live event.
            let here = unsafe {
                let at: *mut AnyObject = msg_send![event.as_ref(), window];
                at as usize == window
            };
            if here {
                clicked();
            }
            event.as_ptr()
        });
        // SAFETY: AppKit on the main thread; it copies the block.
        let monitor: Option<Retained<AnyObject>> =
            unsafe { msg_send![class!(NSEvent), addLocalMonitorForEventsMatchingMask: DOWN, handler: &*watch] };
        monitor.map(ClickWatch)
    }
}

impl Drop for ClickWatch {
    fn drop(&mut self) {
        // SAFETY: removing a monitor AppKit gave, on the main thread.
        unsafe {
            let _: () = msg_send![class!(NSEvent), removeMonitor: &*self.0];
        }
    }
}

/// Whether `view` is in one of the windows tabs are parked in (off the screen), not the browser window.
pub fn parked(view: &AnyObject) -> bool {
    // SAFETY: AppKit read-only queries on the main thread.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        window.is_null()
            || park_class().is_some_and(|class| {
                let is: bool = msg_send![window, isKindOfClass: class];
                is
            })
    }
}

/// Gives the page the keyboard of the window it's in (the user took it over in the browser window).
pub fn focus_page(view: &AnyObject) {
    // SAFETY: AppKit on the main thread; the tab keeps `view`.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if !window.is_null() {
            let _: bool = msg_send![window, makeFirstResponder: view];
        }
    }
}

/// A `CGColorRef`, as AppKit returns it.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CgColor(*const c_void);

// SAFETY: a pointer to an opaque CGColor, as AppKit declares it.
unsafe impl objc2::Encode for CgColor {
    const ENCODING: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGColor", &[]));
}

/// A ring that grows and fades at (x, y) of a page on the user's screen, where an agent clicks: a view of its own over
/// the page in the browser window (the page and an agent's screenshots never see it). None for a page off the screen.
/// The caller takes it away once it has faded (`remove_view`, about a second).
pub fn flash(view: &AnyObject, x: f64, y: f64, radius: f64) -> Option<Retained<AnyObject>> {
    if parked(view) {
        return None;
    }
    let (location, _) = in_window(view, x, y)?;
    // SAFETY: AppKit and Core Animation on the main thread; the ring is kept by its superview and the returned handle.
    unsafe {
        let parent: *mut AnyObject = msg_send![view, superview];
        if parent.is_null() {
            return None;
        }
        let at: NSPoint = msg_send![parent, convertPoint: location, fromView: std::ptr::null_mut::<AnyObject>()];
        let frame = NSRect::new(NSPoint::new(at.x - radius, at.y - radius), NSSize::new(2. * radius, 2. * radius));
        let allocated: Allocated<AnyObject> = msg_send![class!(NSView), alloc];
        let ring: Option<Retained<AnyObject>> = msg_send![allocated, initWithFrame: frame];
        let ring = ring?;
        let _: () = msg_send![&*ring, setWantsLayer: true];
        let host: *mut AnyObject = msg_send![&*ring, layer];
        let layer: Option<Retained<AnyObject>> = msg_send![class!(CALayer), layer];
        let (Some(layer), false) = (layer, host.is_null()) else { return None };
        let bounds = NSRect::new(NSPoint::new(0., 0.), frame.size);
        let _: () = msg_send![&*layer, setFrame: bounds];
        let _: () = msg_send![&*layer, setCornerRadius: radius];
        let _: () = msg_send![&*layer, setBorderWidth: 3f64];
        let blue: *mut AnyObject = msg_send![class!(NSColor), systemBlueColor];
        let fill: *mut AnyObject = msg_send![blue, colorWithAlphaComponent: 0.22f64];
        let border: CgColor = msg_send![blue, CGColor];
        let background: CgColor = msg_send![fill, CGColor];
        let _: () = msg_send![&*layer, setBorderColor: border];
        let _: () = msg_send![&*layer, setBackgroundColor: background];
        let _: () = msg_send![host, addSublayer: &*layer];
        // Grows from a dot to the ring, then fades.
        let number = |value: f64| -> *mut AnyObject { msg_send![class!(NSNumber), numberWithDouble: value] };
        let animation = |path: &str, from: f64, to: f64, duration: f64| -> *mut AnyObject {
            let animation: *mut AnyObject =
                msg_send![class!(CABasicAnimation), animationWithKeyPath: &*NSString::from_str(path)];
            let _: () = msg_send![animation, setFromValue: number(from)];
            let _: () = msg_send![animation, setToValue: number(to)];
            let _: () = msg_send![animation, setDuration: duration];
            animation
        };
        let grow = animation("transform.scale", 0.3, 1., 0.35);
        let fade = animation("opacity", 1., 0., 0.9);
        let _: () = msg_send![&*layer, setOpacity: 0f32];
        let _: () = msg_send![&*layer, addAnimation: grow, forKey: &*NSString::from_str("grow")];
        let _: () = msg_send![&*layer, addAnimation: fade, forKey: &*NSString::from_str("fade")];
        // NSWindowAbove: over the page.
        let _: () = msg_send![parent, addSubview: &*ring, positioned: 1isize, relativeTo: view];
        #[cfg(feature = "dev")]
        FLASHED.with(|flashed| flashed.set(flashed.get() + 1));
        Some(ring)
    }
}

#[cfg(feature = "dev")]
thread_local! {
    static FLASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many rings `flash` put over pages on screen (the browser tools check: they aren't in its pictures).
#[cfg(feature = "dev")]
pub fn flashed() -> usize {
    FLASHED.with(std::cell::Cell::get)
}

/// Takes a view out of its window (a faded `flash`).
pub fn remove_view(view: &AnyObject) {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let _: () = msg_send![view, removeFromSuperview];
    }
}

/// Gives a window's keyboard to its view `view` (GPUI's, after the page had it).
pub fn focus(view: *mut AnyObject) {
    // SAFETY: AppKit on the main thread; `view` is alive with its window.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if !window.is_null() {
            let _: bool = msg_send![window, makeFirstResponder: view];
        }
    }
}

fn string(object: *mut AnyObject) -> String {
    if object.is_null() {
        return String::new();
    }
    // SAFETY: an NSString (or nil), read on the main thread.
    unsafe {
        let text: *const std::ffi::c_char = msg_send![object, UTF8String];
        if text.is_null() { String::new() } else { CStr::from_ptr(text).to_string_lossy().into_owned() }
    }
}

/// The page's title, address and whether it's loading.
pub fn state(view: &AnyObject) -> (String, String, bool) {
    if let Some(page) = super::chromium::of(view) {
        return page.state();
    }
    // SAFETY: WebKit on the main thread.
    unsafe {
        let title: *mut AnyObject = msg_send![view, title];
        let url: *mut AnyObject = msg_send![view, URL];
        let url = if url.is_null() { String::new() } else { string(msg_send![url, absoluteString]) };
        let loading: bool = msg_send![view, isLoading];
        (string(title), url, loading)
    }
}

/// Whether it can go back and forward.
pub fn history(view: &AnyObject) -> (bool, bool) {
    if let Some(page) = super::chromium::of(view) {
        return page.history();
    }
    // SAFETY: WebKit on the main thread.
    unsafe {
        let back: bool = msg_send![view, canGoBack];
        let forward: bool = msg_send![view, canGoForward];
        (back, forward)
    }
}

pub fn go(view: &AnyObject, how: &str) {
    if let Some(page) = super::chromium::of(view) {
        return page.go(how);
    }
    // SAFETY: WebKit on the main thread; the WKNavigation returned is ignored.
    unsafe {
        let _: *mut AnyObject = match how {
            "back" => msg_send![view, goBack],
            "forward" => msg_send![view, goForward],
            "stop" => {
                let _: () = msg_send![view, stopLoading];
                std::ptr::null_mut()
            }
            _ => msg_send![view, reload],
        };
    }
}

/// Runs `body` (a function body, which may `await`) in the page's main frame, in a JavaScript world of Filo's own: it
/// shares the page's DOM but not its scripts, so the page can't see or change what runs there. `arg` is its variable
/// `a`. `done` gets what it returns, as a string.
pub fn run_js(view: &AnyObject, body: &str, arg: &str, done: impl FnOnce(Result<String, String>) + 'static) {
    if let Some(page) = super::chromium::of(view) {
        return page.run_js(body, arg, done);
    }
    let done = RefCell::new(Some(done));
    let handler = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut AnyObject| {
        let Some(done) = done.borrow_mut().take() else { return };
        if !error.is_null() {
            // SAFETY: an NSError, on the main thread.
            let text = unsafe { string(msg_send![error, localizedDescription]) };
            done(Err(text));
            return;
        }
        // SAFETY: WebKit hands back a property-list object or nil; only a string is read.
        let is_string = !value.is_null()
            && unsafe {
                let class: &AnyClass = class!(NSString);
                let yes: bool = msg_send![value, isKindOfClass: class];
                yes
            };
        done(Ok(if is_string { string(value) } else { String::new() }));
    });
    // SAFETY: WebKit on the main thread; the arguments are autoreleased objects WebKit keeps as it needs.
    unsafe {
        let body = NSString::from_str(body);
        let key = NSString::from_str("a");
        let value = NSString::from_str(arg);
        let arguments: *mut AnyObject = msg_send![class!(NSDictionary), dictionaryWithObject: &*value, forKey: &*key];
        let world: *mut AnyObject = msg_send![class!(WKContentWorld), defaultClientWorld];
        let _: () = msg_send![view, callAsyncJavaScript: &*body, arguments: arguments, inFrame: std::ptr::null_mut::<AnyObject>(), inContentWorld: world, completionHandler: &*handler];
    }
}

/// The size of what the page shows, in points (the page's CSS pixels).
pub fn viewport(view: &AnyObject) -> (f64, f64) {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let bounds: NSRect = msg_send![view, bounds];
        (bounds.size.width, bounds.size.height)
    }
}

/// A picture of what the page shows, one pixel per CSS pixel (WebKit draws only what shows: a full-page picture is
/// pieced from several).
pub fn snapshot(view: &AnyObject, done: impl FnOnce(Result<image::RgbaImage, String>) + 'static) {
    if let Some(page) = super::chromium::of(view) {
        return page.snapshot(view, done);
    }
    let (width, height) = viewport(view);
    let (width, height) = (width.round().max(1.) as usize, height.round().max(1.) as usize);
    let done = RefCell::new(Some(done));
    let handler = block2::RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
        let Some(done) = done.borrow_mut().take() else { return };
        if image.is_null() {
            // SAFETY: an NSError or nil, on the main thread.
            let text = if error.is_null() {
                "no picture".to_string()
            } else {
                unsafe { string(msg_send![error, localizedDescription]) }
            };
            done(Err(format!("The screenshot failed: {text}")));
            return;
        }
        done(image_rgba(image, width, height));
    });
    // SAFETY: WebKit on the main thread; the configuration is retained for the call.
    unsafe {
        let config: Option<Retained<AnyObject>> = msg_send![class!(WKSnapshotConfiguration), new];
        let Some(config) = config else { return };
        let number: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: width as f64];
        let _: () = msg_send![&*config, setSnapshotWidth: number];
        let _: () = msg_send![view, takeSnapshotWithConfiguration: &*config, completionHandler: &*handler];
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *const c_void;
    fn CGColorSpaceRelease(space: *const c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *const c_void,
        bitmap_info: u32,
    ) -> *const c_void;
    fn CGContextDrawImage(context: *const c_void, rect: CGRect, image: *const c_void);
    fn CGContextRelease(context: *const c_void);
}

/// An NSImage drawn at `width` × `height` pixels.
fn image_rgba(image: *mut AnyObject, width: usize, height: usize) -> Result<image::RgbaImage, String> {
    /// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big: R, G, B, A in memory.
    const RGBA: u32 = 1 | (4 << 12);
    // SAFETY: AppKit and Core Graphics on the main thread; the context draws into `pixels`, which outlives it.
    let pixels = unsafe {
        let cg: CgImage = msg_send![image, CGImageForProposedRect: std::ptr::null_mut::<NSRect>(), context: std::ptr::null_mut::<AnyObject>(), hints: std::ptr::null_mut::<AnyObject>()];
        if cg.0.is_null() {
            return Err("The screenshot had no picture.".into());
        }
        let mut pixels = vec![0u8; width * height * 4];
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        let context = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), width, height, 8, width * 4, space, RGBA);
        CGColorSpaceRelease(space);
        if context.is_null() {
            return Err("Couldn't draw the screenshot.".into());
        }
        CGContextDrawImage(context, CGRect { x: 0., y: 0., width: width as f64, height: height as f64 }, cg.0);
        CGContextRelease(context);
        pixels
    };
    // A page is opaque where it drew; what it left clear is white, as a browser shows it.
    let mut rgba = pixels;
    for pixel in rgba.chunks_exact_mut(4) {
        let keep = 255 - u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) + keep).min(255) as u8;
        }
        pixel[3] = 255;
    }
    Ok(image::RgbaImage::from_raw(width as u32, height as u32, rgba).ok_or("bad picture size")?)
}

pub fn png(picture: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(png)
}

fn uptime() -> f64 {
    // SAFETY: a read-only Foundation call.
    unsafe {
        let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        msg_send![info, systemUptime]
    }
}

/// Where (x, y), points from the page's top left, is in its window's coordinates, and the window's number.
fn in_window(view: &AnyObject, x: f64, y: f64) -> Option<(NSPoint, isize)> {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            return None;
        }
        let bounds: NSRect = msg_send![view, bounds];
        let frame: NSRect = msg_send![view, convertRect: bounds, toView: std::ptr::null_mut::<AnyObject>()];
        let number: isize = msg_send![window, windowNumber];
        Some((NSPoint::new(frame.origin.x + x, frame.origin.y + frame.size.height - y), number))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mouse {
    Move,
    /// A move with the left button down.
    Dragged,
    Down,
    Up,
    RightDown,
    RightUp,
}

/// One mouse event at (x, y), points from the page's top left, handed to the web view.
pub fn mouse(view: &AnyObject, kind: Mouse, x: f64, y: f64, clicks: isize) -> bool {
    if let Some(page) = super::chromium::of(view) {
        return page.mouse(kind, x, y, clicks);
    }
    let Some((location, number)) = in_window(view, x, y) else { return false };
    let (event_type, selector): (usize, Sel) = match kind {
        // A move goes in as WebKit takes a drag's moves (`mouseDragged:`), with no button held: what it gets as
        // `mouseMoved:` it drops for a page off the screen (no mousemove, no hover), though it's the same move event.
        Mouse::Move => (5, sel!(mouseDragged:)),
        Mouse::Dragged => (6, sel!(mouseDragged:)),
        Mouse::Down => (1, sel!(mouseDown:)),
        Mouse::Up => (2, sel!(mouseUp:)),
        Mouse::RightDown => (3, sel!(rightMouseDown:)),
        Mouse::RightUp => (4, sel!(rightMouseUp:)),
    };
    let pressure: f32 = if matches!(kind, Mouse::Down | Mouse::RightDown | Mouse::Dragged) { 1. } else { 0. };
    // The buttons held during the event, as `pressedMouseButtons` says them: left 1, right 2.
    let buttons = match kind {
        Mouse::Down | Mouse::Dragged => 1,
        Mouse::RightDown => 2,
        _ => 0,
    };
    // SAFETY: AppKit on the main thread; the event is autoreleased and handed straight to the view.
    unsafe {
        let event: *mut AnyObject = msg_send![class!(NSEvent), mouseEventWithType: event_type, location: location, modifierFlags: 0usize, timestamp: uptime(), windowNumber: number, context: std::ptr::null_mut::<AnyObject>(), eventNumber: 0isize, clickCount: clicks, pressure: pressure];
        if event.is_null() {
            return false;
        }
        with_buttons(buttons, || {
            let _: () = objc2::runtime::MessageReceiver::send_message(view, selector, (event,));
        });
    }
    true
}

/// Runs `send` (handing the web view an event) with `[NSEvent pressedMouseButtons]` answering `buttons`. WebKit reads
/// it, while it turns the event into the page's, for the page's `buttons`: with the real mouse's (none held), a press
/// had no button down, and the page got no pointerdown, pointermove with the button held or pointerup (pointer events
/// lists and galleries ignored the drag). It's put back before this returns.
fn with_buttons<R>(buttons: usize, send: impl FnOnce() -> R) -> R {
    thread_local! {
        static BUTTONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    extern "C-unwind" fn pressed(_: &AnyClass, _: Sel) -> usize {
        BUTTONS.with(std::cell::Cell::get)
    }
    let Some(method) = AnyClass::get(c"NSEvent").and_then(|class| class.class_method(sel!(pressedMouseButtons))) else {
        return send();
    };
    BUTTONS.with(|held| held.set(buttons));
    // SAFETY: the stand-in has the signature of the method it stands in for (a class method returning NSUInteger);
    // everything runs on the main thread, and the method's own implementation is back before this returns.
    unsafe {
        let method = method as *const objc2::runtime::Method;
        let own = objc2::ffi::method_setImplementation(
            method,
            std::mem::transmute::<extern "C-unwind" fn(&AnyClass, Sel) -> usize, objc2::runtime::Imp>(pressed),
        );
        let sent = send();
        if let Some(own) = own {
            objc2::ffi::method_setImplementation(method, own);
        }
        sent
    }
}

/// A key: what it types, its key code (a US keyboard's) and the modifiers it needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub characters: String,
    /// Without the modifiers (shift's letter case stays).
    pub plain: String,
    pub code: u16,
    pub flags: usize,
}

pub const SHIFT: usize = 1 << 17;
pub const CONTROL: usize = 1 << 18;
pub const OPTION: usize = 1 << 19;
pub const COMMAND: usize = 1 << 20;
const NUMERIC_PAD: usize = 1 << 21;
const FUNCTION: usize = 1 << 23;

/// US keyboard key codes for the printable characters, unshifted and shifted.
const KEYS: [(char, char, u16); 47] = [
    ('a', 'A', 0),
    ('s', 'S', 1),
    ('d', 'D', 2),
    ('f', 'F', 3),
    ('h', 'H', 4),
    ('g', 'G', 5),
    ('z', 'Z', 6),
    ('x', 'X', 7),
    ('c', 'C', 8),
    ('v', 'V', 9),
    ('b', 'B', 11),
    ('q', 'Q', 12),
    ('w', 'W', 13),
    ('e', 'E', 14),
    ('r', 'R', 15),
    ('y', 'Y', 16),
    ('t', 'T', 17),
    ('1', '!', 18),
    ('2', '@', 19),
    ('3', '#', 20),
    ('4', '$', 21),
    ('6', '^', 22),
    ('5', '%', 23),
    ('=', '+', 24),
    ('9', '(', 25),
    ('7', '&', 26),
    ('-', '_', 27),
    ('8', '*', 28),
    ('0', ')', 29),
    (']', '}', 30),
    ('o', 'O', 31),
    ('u', 'U', 32),
    ('[', '{', 33),
    ('i', 'I', 34),
    ('p', 'P', 35),
    ('l', 'L', 37),
    ('j', 'J', 38),
    ('\'', '"', 39),
    ('k', 'K', 40),
    (';', ':', 41),
    ('\\', '|', 42),
    (',', '<', 43),
    ('/', '?', 44),
    ('n', 'N', 45),
    ('m', 'M', 46),
    ('.', '>', 47),
    ('`', '~', 50),
];

impl Key {
    /// The key that types `character`.
    pub fn typing(character: char) -> Key {
        match character {
            '\n' | '\r' => return Key::named("Enter").expect("Enter"),
            '\t' => return Key::named("Tab").expect("Tab"),
            ' ' => return Key { characters: " ".into(), plain: " ".into(), code: 49, flags: 0 },
            _ => {}
        }
        let text = character.to_string();
        match KEYS.iter().find(|(plain, shifted, _)| *plain == character || *shifted == character) {
            Some((plain, shifted, code)) if *shifted == character => {
                Key { characters: text.clone(), plain: text, code: *code, flags: SHIFT }
                    .with_plain(if plain.is_alphabetic() { character } else { *plain })
            }
            Some((_, _, code)) => Key { characters: text.clone(), plain: text, code: *code, flags: 0 },
            // Not on the keyboard (é, emoji): typed as it is, as an input method would.
            None => Key { characters: text.clone(), plain: text, code: 0, flags: 0 },
        }
    }

    fn with_plain(mut self, plain: char) -> Key {
        self.plain = plain.to_string();
        self
    }

    /// A named key or a combination: "Enter", "ArrowDown", "cmd+a", "shift+Tab".
    pub fn named(spec: &str) -> Option<Key> {
        let mut flags = 0;
        let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
        let (last, modifiers) = if spec.trim().ends_with("++") || spec.trim() == "+" {
            ("+", &parts[..parts.len().saturating_sub(2)])
        } else {
            let (last, modifiers) = parts.split_last()?;
            (*last, modifiers)
        };
        for modifier in modifiers {
            flags |= match modifier.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "meta" | "⌘" => COMMAND,
                "shift" | "⇧" => SHIFT,
                "alt" | "option" | "opt" | "⌥" => OPTION,
                "ctrl" | "control" | "⌃" => CONTROL,
                _ => return None,
            };
        }
        let special = |characters: &str, code: u16, extra: usize| Key {
            characters: characters.into(),
            plain: characters.into(),
            code,
            flags: extra,
        };
        let key = match last.to_ascii_lowercase().as_str() {
            "enter" | "return" => special("\r", 36, 0),
            "tab" => special("\t", 48, 0),
            "space" => special(" ", 49, 0),
            "backspace" => special("\u{7f}", 51, 0),
            "escape" | "esc" => special("\u{1b}", 53, 0),
            "delete" => special("\u{f728}", 117, FUNCTION),
            "arrowup" | "up" => special("\u{f700}", 126, FUNCTION | NUMERIC_PAD),
            "arrowdown" | "down" => special("\u{f701}", 125, FUNCTION | NUMERIC_PAD),
            "arrowleft" | "left" => special("\u{f702}", 123, FUNCTION | NUMERIC_PAD),
            "arrowright" | "right" => special("\u{f703}", 124, FUNCTION | NUMERIC_PAD),
            "home" => special("\u{f729}", 115, FUNCTION),
            "end" => special("\u{f72b}", 119, FUNCTION),
            "pageup" => special("\u{f72c}", 116, FUNCTION),
            "pagedown" => special("\u{f72d}", 121, FUNCTION),
            _ => {
                let mut chars = last.chars();
                let (Some(character), None) = (chars.next(), chars.next()) else { return None };
                // A letter with a modifier is its lower case, as macOS gives it (shift is a flag).
                let lower = if flags != 0 { character.to_ascii_lowercase() } else { character };
                Key::typing(lower)
            }
        };
        let flags = key.flags | flags;
        // ⌥ and ⌃ change what a key types; ⌘ leaves it (the menu takes it).
        let characters = if flags & CONTROL != 0 && key.characters.len() == 1 {
            let byte = key.characters.as_bytes()[0];
            if byte.is_ascii_alphabetic() {
                ((byte.to_ascii_lowercase() - b'a' + 1) as char).to_string()
            } else {
                key.characters.clone()
            }
        } else if flags & SHIFT != 0 && key.code != 0 && key.characters.chars().all(|c| c.is_ascii_lowercase()) {
            key.characters.to_ascii_uppercase()
        } else {
            key.characters.clone()
        };
        Some(Key { characters, flags, ..key })
    }
}

/// The edit command a ⌘ key runs in a browser's Edit menu (the page's own window has none).
fn edit_command(key: &Key) -> Option<Sel> {
    if key.flags & (COMMAND | CONTROL | OPTION) != COMMAND {
        return None;
    }
    let shift = key.flags & SHIFT != 0;
    Some(match (key.plain.to_ascii_lowercase().as_str(), shift) {
        ("a", false) => sel!(selectAll:),
        ("c", false) => sel!(copy:),
        ("x", false) => sel!(cut:),
        ("v", false) => sel!(paste:),
        ("z", false) => sel!(undo:),
        ("z", true) => sel!(redo:),
        _ => return None,
    })
}

/// One key pressed and let go in the web view, which has its window's keyboard (`prepare`).
pub fn key(view: &AnyObject, key: &Key) -> bool {
    if let Some(page) = super::chromium::of(view) {
        return page.key(key);
    }
    let Some((location, number)) = in_window(view, 1., 1.) else { return false };
    // SAFETY: AppKit on the main thread; each event is autoreleased and handed straight to the view.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        // The page (or a view inside it) has the keyboard of its window, or gets it.
        if !has_keyboard(view) {
            let _: bool = msg_send![window, makeFirstResponder: view];
        }
        let characters = NSString::from_str(&key.characters);
        let plain = NSString::from_str(&key.plain);
        for (kind, selector) in [(10usize, sel!(keyDown:)), (11usize, sel!(keyUp:))] {
            let event: *mut AnyObject = msg_send![class!(NSEvent), keyEventWithType: kind, location: location, modifierFlags: key.flags, timestamp: uptime(), windowNumber: number, context: std::ptr::null_mut::<AnyObject>(), characters: &*characters, charactersIgnoringModifiers: &*plain, isARepeat: false, keyCode: key.code];
            if event.is_null() {
                return false;
            }
            if kind == 10 && key.flags & COMMAND != 0 {
                // ⌘ keys go to the page first, as a key equivalent; what it leaves, the Edit command does.
                let taken: bool = msg_send![view, performKeyEquivalent: event];
                if !taken && let Some(command) = edit_command(key) {
                    let _: () = objc2::runtime::MessageReceiver::send_message(
                        view,
                        command,
                        (std::ptr::null_mut::<AnyObject>(),),
                    );
                }
                continue;
            }
            let _: () = objc2::runtime::MessageReceiver::send_message(view, selector, (event,));
        }
    }
    true
}

/// Whether Filo is the active app, and the name of the one in front.
pub fn frontmost() -> (bool, String) {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let active: bool = msg_send![app, isActive];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let front: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let name = if front.is_null() { "none".into() } else { string(msg_send![front, localizedName]) };
        (active, name)
    }
}

/// What WebKit sees of the page's window, for the browser check (`dev`).
#[cfg(feature = "dev")]
pub fn diagnose(view: &AnyObject) -> String {
    // SAFETY: AppKit and WebKit read-only queries on the main thread.
    unsafe {
        let setter: bool = msg_send![view, respondsToSelector: sel!(_setWindowOcclusionDetectionEnabled:)];
        let getter: bool = msg_send![view, respondsToSelector: sel!(_windowOcclusionDetectionEnabled)];
        let detection: bool = if getter { msg_send![view, _windowOcclusionDetectionEnabled] } else { true };
        let window: *mut AnyObject = msg_send![view, window];
        let (visible, occlusion, key) = if window.is_null() {
            (false, 0usize, false)
        } else {
            let visible: bool = msg_send![window, isVisible];
            let occlusion: usize = msg_send![window, occlusionState];
            let key: bool = msg_send![window, isKeyWindow];
            (visible, occlusion, key)
        };
        let hidden: bool = msg_send![view, isHiddenOrHasHiddenAncestor];
        format!(
            "occlusion setter {setter}, detection {detection}, window visible {visible}, occlusionState {occlusion}, key {key}, view hidden {hidden}"
        )
    }
}

/// Keeps macOS from napping Filo (App Nap slows a background app's timers) while tabs are open: dropping it lets
/// macOS nap again.
pub struct Awake(Retained<AnyObject>);

impl Awake {
    pub fn begin() -> Option<Awake> {
        // NSActivityUserInitiatedAllowingIdleSystemSleep: work the user asked for, with no App Nap, which doesn't keep
        // the Mac awake.
        const USER_INITIATED: u64 = 0x00FF_FFFF & !(1 << 20);
        // SAFETY: Foundation on the main thread; the token is kept until `end`.
        unsafe {
            let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
            let reason = NSString::from_str("Filo's browser has pages open for agents");
            let token: Option<Retained<AnyObject>> =
                msg_send![info, beginActivityWithOptions: USER_INITIATED, reason: &*reason];
            token.map(Awake)
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        // SAFETY: ends the activity begun with this token.
        unsafe {
            let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
            let _: () = msg_send![info, endActivity: &*self.0];
        }
    }
}

/// A click at (x, y) of the page and keys after it, queued for the app the way the user's mouse and keyboard send
/// them (`[NSApp postEvent:atStart:]`): AppKit offers a key to the window's views and the menus as a key equivalent
/// first, then to the window's first responder. Not handed to the web view as an agent's are (the browser keys check).
#[cfg(feature = "dev")]
pub fn as_user(view: &AnyObject, click: Option<(f64, f64)>, keys: &[Key]) -> bool {
    let Some((location, number)) = in_window(view, click.map_or(1., |spot| spot.0), click.map_or(1., |spot| spot.1))
    else {
        return false;
    };
    // SAFETY: AppKit on the main thread; each event is autoreleased and retained by the queue.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if click.is_some() {
            for (kind, pressure) in [(1usize, 1f32), (2usize, 0f32)] {
                let event: *mut AnyObject = msg_send![class!(NSEvent), mouseEventWithType: kind, location: location, modifierFlags: 0usize, timestamp: uptime(), windowNumber: number, context: std::ptr::null_mut::<AnyObject>(), eventNumber: 0isize, clickCount: 1isize, pressure: pressure];
                if event.is_null() {
                    return false;
                }
                let _: () = msg_send![app, postEvent: event, atStart: false];
            }
        }
        for key in keys {
            let characters = NSString::from_str(&key.characters);
            let plain = NSString::from_str(&key.plain);
            for kind in [10usize, 11] {
                let event: *mut AnyObject = msg_send![class!(NSEvent), keyEventWithType: kind, location: location, modifierFlags: key.flags, timestamp: uptime(), windowNumber: number, context: std::ptr::null_mut::<AnyObject>(), characters: &*characters, charactersIgnoringModifiers: &*plain, isARepeat: false, keyCode: key.code];
                if event.is_null() {
                    return false;
                }
                let _: () = msg_send![app, postEvent: event, atStart: false];
            }
        }
    }
    true
}

/// Whether the window `view` is in is the key window, and the page has its keyboard (the browser keys check).
#[cfg(feature = "dev")]
pub fn has_keys(view: &AnyObject) -> (bool, bool) {
    // SAFETY: an AppKit read-only query on the main thread.
    let key = unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        !window.is_null() && {
            let key: bool = msg_send![window, isKeyWindow];
            key
        }
    };
    (key, has_keyboard(view))
}

/// Where the user's pointer is on the screen (the browser drag check: an agent's drag never moves it).
#[cfg(feature = "dev")]
pub fn pointer() -> (f64, f64) {
    // SAFETY: a read-only AppKit query.
    let at: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
    (at.x, at.y)
}

/// Sends Filo to the back, the app before it in front again (the browser check's start).
#[cfg(feature = "dev")]
pub fn deactivate() {
    // SAFETY: AppKit on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, deactivate];
    }
}

/// Runs `build` (making a web view) without Filo coming to the front: wry activates the app each time it makes one
/// ("so the window is on top"), which would take the user's keyboard whenever an agent opens a page. AppKit's
/// `activate` and `activateIgnoringOtherApps:` do nothing meanwhile, and are put back after.
pub fn without_activation<R>(build: impl FnOnce() -> R) -> R {
    extern "C-unwind" fn stay(_: &AnyObject, _: Sel) {}
    extern "C-unwind" fn stay_ignoring(_: &AnyObject, _: Sel, _: Bool) {}
    let Some(class) = AnyClass::get(c"NSApplication") else { return build() };
    // SAFETY: both stand-ins have the signatures of the methods they stand in for; everything runs on the main thread,
    // and the methods' own implementations are back before this returns (a panic in `build` aborts the app anyway).
    unsafe {
        let swaps: Vec<(*const objc2::runtime::Method, objc2::runtime::Imp)> = [
            (sel!(activate), std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel), objc2::runtime::Imp>(stay)),
            (
                sel!(activateIgnoringOtherApps:),
                std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, Bool), objc2::runtime::Imp>(stay_ignoring),
            ),
        ]
        .into_iter()
        .filter_map(|(selector, stand_in)| {
            let method = class.instance_method(selector)? as *const objc2::runtime::Method;
            let own = objc2::ffi::method_setImplementation(method, stand_in)?;
            Some((method, own))
        })
        .collect();
        let built = build();
        for (method, own) in swaps {
            objc2::ffi::method_setImplementation(method, own);
        }
        built
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_type_what_they_say() {
        assert_eq!(Key::typing('a'), Key { characters: "a".into(), plain: "a".into(), code: 0, flags: 0 });
        let capital = Key::typing('A');
        assert_eq!((capital.code, capital.flags, capital.plain.as_str()), (0, SHIFT, "A"));
        let bang = Key::typing('!');
        assert_eq!((bang.code, bang.flags, bang.plain.as_str()), (18, SHIFT, "1"));
        assert_eq!(Key::typing('é').code, 0);
        assert_eq!(Key::named("Enter").unwrap().code, 36);
        let all = Key::named("cmd+a").unwrap();
        assert_eq!((all.code, all.flags, all.characters.as_str()), (0, COMMAND, "a"));
        assert_eq!(edit_command(&all), Some(sel!(selectAll:)));
        let back_tab = Key::named("shift+Tab").unwrap();
        assert_eq!((back_tab.code, back_tab.flags), (48, SHIFT));
        assert_eq!(Key::named("ArrowDown").unwrap().code, 125);
        assert!(Key::named("hyper+x").is_none());
        assert!(Key::named("Banana").is_none());
    }
}

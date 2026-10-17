// What's drawn over a web view (an artifact's page, the note editor, a file in the viewer): a web view is a native view
// drawn over everything GPUI draws, so a menu, tooltip, notification, toast, dialog or anything else there would be
// under it. After each frame that drew it, gpui-component's `WebView` hands over what GPUI painted after it, over it
// (gpui's `Window::painted_over`): its view gets a hole in each one's shape, and a click in a hole is GPUI's. A
// see-through fill over it (a dialog's dim) is laid over the page as a tint, and takes the clicks there too, so the page
// under a dialog is dimmed and out of reach like the rest of the window. A see-through gradient over it (the fade under
// a page's header, `ui::sub_page`) is laid over it as that gradient, and clicks go through it to the page. A glance's
// page does the same in chat/glance.rs.

use gpui::App;

// The test windows' checks and screenshots (`dev` builds).
#[cfg(feature = "dev")]
pub use native::{Fade, hole_rects, holes, in_hole, window_holes};
pub use native::{cut_view, lay_tints};

pub fn init(cx: &mut App) {
    gpui_component::webview::set_hole_cutter(cx, native::cut);
    // A dialog or sheet opening takes the keyboard from a web view that has it, so Esc and Return reach the dialog.
    gpui_component::one_menu::on_modal(cx, native::take_keys);
}

mod native {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use gpui::{Bounds, PaintedFade, PaintedOver, Pixels};
    use gpui_component::wry::{self, WebViewExtMacOS as _};
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPathCreateMutable() -> *mut c_void;
        fn CGPathAddRect(path: *mut c_void, transform: *const c_void, rect: NSRect);
        fn CGPathAddRoundedRect(
            path: *mut c_void,
            transform: *const c_void,
            rect: NSRect,
            corner_width: f64,
            corner_height: f64,
        );
        fn CGPathRelease(path: *mut c_void);
    }

    /// A `CGPathRef`, as Core Animation's `setPath:` takes it.
    #[repr(transparent)]
    struct Path(*mut c_void);

    // SAFETY: a pointer to an opaque CGPath, as Core Animation declares the argument.
    unsafe impl objc2::Encode for Path {
        const ENCODING: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGPath", &[]));
    }

    /// Room around a hole for the shadow of what shows through it.
    const MARGIN: f64 = 4.;

    /// A hole or a tint: a rect in the view's points from its top left, and a corner radius (a hole's) or an sRGB colour
    /// (a tint's, alpha last).
    type Rect = [f64; 4];

    /// A gradient laid over a view: its line from `start` to `end` in its rect's unit space from the top left, and its
    /// two stops' sRGB colours (alpha last) and places on that line.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Fade {
        pub start: [f64; 2],
        pub end: [f64; 2],
        pub colors: [[f64; 4]; 2],
        pub locations: [f64; 2],
    }

    impl Fade {
        fn new(fade: &PaintedFade) -> Self {
            let rgba = |color: gpui::Hsla| {
                let rgba = gpui::Rgba::from(color);
                [rgba.r, rgba.g, rgba.b, rgba.a].map(f64::from)
            };
            let at = |point: gpui::Point<f32>| [f64::from(point.x), f64::from(point.y)];
            Fade {
                start: at(fade.start),
                end: at(fade.end),
                colors: fade.stops.map(|stop| rgba(stop.color)),
                locations: fade.stops.map(|stop| f64::from(stop.percentage)),
            }
        }

        /// Its colour at (x, y) in its rect's unit space, a `width` × `height` rect (the checks' pictures).
        #[cfg(feature = "dev")]
        pub fn color_at(&self, x: f64, y: f64, width: f64, height: f64) -> [f64; 4] {
            let line = ((self.end[0] - self.start[0]) * width, (self.end[1] - self.start[1]) * height);
            let along = ((x - self.start[0]) * width * line.0 + (y - self.start[1]) * height * line.1)
                / (line.0 * line.0 + line.1 * line.1).max(f64::EPSILON);
            let [from, to] = self.locations;
            let t = if to == from {
                f64::from(u8::from(along >= from))
            } else {
                ((along - from) / (to - from)).clamp(0., 1.)
            };
            std::array::from_fn(|channel| self.colors[0][channel] * (1. - t) + self.colors[1][channel] * t)
        }
    }

    #[derive(Default, Clone, PartialEq)]
    struct Cut {
        /// The view's top left, window points (the checks' `window_holes`).
        origin: [f64; 2],
        holes: Vec<(Rect, f64)>,
        tints: Vec<(Rect, [f64; 4])>,
        fades: Vec<(Rect, Fade)>,
    }

    /// What's laid over a view: tints and fades.
    #[derive(Default, Clone, PartialEq)]
    struct Laid {
        tints: Vec<(Rect, [f64; 4])>,
        fades: Vec<(Rect, Fade)>,
    }

    impl Laid {
        /// What of `over` (window points) is laid over a view at `bounds`, in its points from its top left.
        fn from(over: &[PaintedOver], bounds: Bounds<Pixels>) -> Self {
            let f = |value: Pixels| f64::from(f32::from(value));
            let mut laid = Laid::default();
            for painted in over {
                let at = painted.bounds;
                let rect = [
                    f(at.origin.x - bounds.origin.x),
                    f(at.origin.y - bounds.origin.y),
                    f(at.size.width),
                    f(at.size.height),
                ];
                if let Some(tint) = painted.tint {
                    let rgba = gpui::Rgba::from(tint);
                    laid.tints.push((rect, [rgba.r, rgba.g, rgba.b, rgba.a].map(f64::from)));
                } else if let Some(fade) = &painted.fade {
                    laid.fades.push((rect, Fade::new(fade)));
                }
            }
            laid
        }
    }

    thread_local! {
        /// What's cut in and laid over each web view, by the view.
        static CUTS: RefCell<HashMap<usize, Cut>> = RefCell::new(HashMap::new());
        /// Each cut view, weakly, by its address (the checks' `window_holes` skips one that's gone).
        #[cfg(feature = "dev")]
        static VIEWS: RefCell<HashMap<usize, objc2::rc::Weak<AnyObject>>> = RefCell::new(HashMap::new());
        /// The tints and fades `lay_tints` laid over each view, by the view.
        static LAID: RefCell<HashMap<usize, Laid>> = RefCell::new(HashMap::new());
        /// The tint and fade layers laid over each web view, by the view.
        static TINT_LAYERS: RefCell<HashMap<usize, Vec<Retained<AnyObject>>>> = RefCell::new(HashMap::new());
    }

    /// How many holes are cut in `webview` (the test windows' checks).
    #[cfg(feature = "dev")]
    pub fn holes(webview: &wry::WebView) -> usize {
        let view: Retained<_> = webview.webview();
        let key = Retained::as_ptr(&view) as *const AnyObject as usize;
        CUTS.with(|cuts| cuts.borrow().get(&key).map_or(0, |cut| cut.holes.len()))
    }

    /// The holes cut in the native views that show, window points from the top left, and how many tints are over them
    /// (the test windows' checks).
    #[cfg(feature = "dev")]
    pub fn window_holes() -> (Vec<Bounds<Pixels>>, usize) {
        CUTS.with(|cuts| {
            let mut holes = Vec::new();
            let mut tints = 0;
            for (key, cut) in cuts.borrow().iter() {
                let Some(view) = VIEWS.with(|views| views.borrow().get(key).and_then(|view| view.load())) else {
                    continue;
                };
                // SAFETY: on the main thread; `view` is held. Only views that show in a window count.
                let shows = unsafe {
                    let hidden: bool = msg_send![&*view, isHiddenOrHasHiddenAncestor];
                    let window: *mut AnyObject = msg_send![&*view, window];
                    !hidden && !window.is_null()
                };
                if !shows {
                    continue;
                }
                let [x, y] = cut.origin;
                holes.extend(cut.holes.iter().map(|([left, top, width, height], _)| {
                    gpui::Bounds::new(
                        gpui::point(gpui::px((x + left) as f32), gpui::px((y + top) as f32)),
                        gpui::size(gpui::px(*width as f32), gpui::px(*height as f32)),
                    )
                }));
                tints += cut.tints.len();
            }
            (holes, tints)
        })
    }

    /// The holes cut in a web view (its WKWebView) and the tints and fades over it, in its points from its top left
    /// (window_shot.rs): each hole a rect and its corner radius, each tint a rect and its sRGB colour, each fade a rect
    /// and its gradient.
    #[cfg(feature = "dev")]
    #[allow(clippy::type_complexity)]
    pub fn hole_rects(
        view: *const AnyObject,
    ) -> (Vec<([f64; 4], f64)>, Vec<([f64; 4], [f64; 4])>, Vec<([f64; 4], Fade)>) {
        CUTS.with(|cuts| {
            cuts.borrow()
                .get(&(view as usize))
                .map(|cut| (cut.holes.clone(), cut.tints.clone(), cut.fades.clone()))
                .unwrap_or_default()
        })
    }

    /// Whether (x, y), points from the web view's top left, is in `hole`, round corners and all.
    #[cfg(feature = "dev")]
    pub fn in_hole(&([left, top, width, height], radius): &([f64; 4], f64), x: f64, y: f64) -> bool {
        if x < left || x >= left + width || y < top || y >= top + height {
            return false;
        }
        let radius = corner_radius(radius, width, height);
        let dx = (left + radius - x).max(x - (left + width - radius)).max(0.);
        let dy = (top + radius - y).max(y - (top + height - radius)).max(0.);
        dx * dx + dy * dy <= radius * radius
    }

    fn corner_radius(radius: f64, width: f64, height: f64) -> f64 {
        (radius + MARGIN).min(width.min(height) / 2.)
    }

    /// Cuts holes in `webview` (at `bounds`, window points) where GPUI painted `over` it, and lays its tints over it;
    /// none clears them. A click in either is GPUI's.
    pub fn cut(webview: &wry::WebView, bounds: Bounds<Pixels>, over: &[PaintedOver]) {
        let view: Retained<_> = webview.webview();
        let view = Retained::as_ptr(&view) as *const AnyObject;
        let empty = over.is_empty();
        if !empty && !hit_test_installed() {
            return;
        }
        // SAFETY: the WKWebView is alive (`webview` holds it).
        unsafe { cut_view(view, bounds, over) };
    }

    /// `cut` for any native view over GPUI's (a video's): one that takes no clicks of its own needs no hit-testing.
    ///
    /// # Safety
    /// `view` is a live NSView, on the main thread.
    pub unsafe fn cut_view(view: *const AnyObject, bounds: Bounds<Pixels>, over: &[PaintedOver]) {
        let f = |value: Pixels| f64::from(f32::from(value));
        let Laid { tints, fades } = Laid::from(over, bounds);
        let mut cut = Cut { origin: [f(bounds.origin.x), f(bounds.origin.y)], tints, fades, ..Cut::default() };
        let empty = Cut { origin: cut.origin, ..Cut::default() };
        for painted in over.iter().filter(|painted| !painted.laid()) {
            let (x, y) = (f(painted.bounds.origin.x - bounds.origin.x), f(painted.bounds.origin.y - bounds.origin.y));
            let (width, height) = (f(painted.bounds.size.width), f(painted.bounds.size.height));
            cut.holes.push(([x - MARGIN, y - MARGIN, width + 2. * MARGIN, height + 2. * MARGIN], f(painted.radius)));
        }
        let key = view as usize;
        let same = CUTS.with(|cuts| cuts.borrow().get(&key).map_or(cut == empty, |before| *before == cut));
        if same {
            return;
        }
        #[cfg(feature = "dev")]
        // SAFETY: the caller keeps the view alive.
        if let Some(held) = unsafe { Retained::retain(view as *mut AnyObject) } {
            VIEWS.with(|views| views.borrow_mut().insert(key, objc2::rc::Weak::from_retained(&held)));
        }
        CUTS.with(|cuts| {
            let mut cuts = cuts.borrow_mut();
            if cut == empty {
                cuts.remove(&key);
            } else {
                cuts.insert(key, cut.clone());
            }
        });
        // SAFETY: AppKit and Core Animation on the main thread, where GPUI draws; the caller keeps the view alive.
        unsafe {
            set_tints(&*view, key, &Laid { tints: cut.tints.clone(), fades: cut.fades.clone() });
            set_mask(&*view, &cut.holes, (f(bounds.size.width), f(bounds.size.height)));
        }
    }

    /// Lays the tints and fades GPUI painted over a native view at `bounds` (window points) over it, in place of the
    /// ones before; for a view that cuts its own holes (a glance's page).
    pub fn lay_tints(view: &AnyObject, bounds: Bounds<Pixels>, over: &[PaintedOver]) {
        let laid = Laid::from(over, bounds);
        let none = laid == Laid::default();
        let key = view as *const AnyObject as usize;
        if LAID.with(|before| before.borrow().get(&key).map_or(none, |before| *before == laid)) {
            return;
        }
        LAID.with(|before| {
            if none {
                before.borrow_mut().remove(&key);
            } else {
                before.borrow_mut().insert(key, laid.clone());
            }
        });
        // SAFETY: AppKit and Core Animation on the main thread; `view` is alive (borrowed).
        unsafe { set_tints(view, key, &laid) };
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGColorCreateSRGB(red: f64, green: f64, blue: f64, alpha: f64) -> *mut c_void;
        fn CGColorRelease(color: *mut c_void);
    }

    /// A `CGColorRef`, as Core Animation's `setBackgroundColor:` takes it.
    #[repr(transparent)]
    struct Color(*mut c_void);

    // SAFETY: a pointer to an opaque CGColor, as Core Animation declares the argument.
    unsafe impl objc2::Encode for Color {
        const ENCODING: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGColor", &[]));
    }

    /// Lays `laid`'s tints and fades over the view (in place of the ones before): a layer each, over the page's own.
    unsafe fn set_tints(view: &AnyObject, key: usize, laid: &Laid) {
        unsafe {
            let before = TINT_LAYERS.with(|layers| layers.borrow_mut().remove(&key)).unwrap_or_default();
            for layer in before {
                let _: () = msg_send![&*layer, removeFromSuperlayer];
            }
            let host: *mut AnyObject = msg_send![view, layer];
            if host.is_null() || *laid == Laid::default() {
                return;
            }
            let (Some(class), Some(gradient), Some(array), Some(number)) = (
                AnyClass::get(c"CALayer"),
                AnyClass::get(c"CAGradientLayer"),
                AnyClass::get(c"NSArray"),
                AnyClass::get(c"NSNumber"),
            ) else {
                return;
            };
            let flipped: bool = msg_send![host, contentsAreFlipped];
            let bounds: NSRect = msg_send![host, bounds];
            let mut made = Vec::new();
            for ([x, y, width, height], fade) in &laid.fades {
                let layer: Option<Retained<AnyObject>> = msg_send![gradient, layer];
                let Some(layer) = layer else { continue };
                // The layer's y, and its unit space's, run up unless it's drawn flipped.
                let y = if flipped { *y } else { bounds.size.height - y - height };
                let point = |[px, py]: [f64; 2]| NSPoint::new(px, if flipped { py } else { 1. - py });
                let colors = fade.colors.map(|[red, green, blue, alpha]| CGColorCreateSRGB(red, green, blue, alpha));
                let colors_array: *mut AnyObject =
                    msg_send![array, arrayWithObjects: colors.as_ptr().cast::<*mut AnyObject>(), count: colors.len()];
                let locations: [*mut AnyObject; 2] =
                    fade.locations.map(|location| msg_send![number, numberWithDouble: location]);
                let locations_array: *mut AnyObject =
                    msg_send![array, arrayWithObjects: locations.as_ptr(), count: locations.len()];
                let _: () =
                    msg_send![&*layer, setFrame: NSRect::new(NSPoint::new(*x, y), NSSize::new(*width, *height))];
                let _: () = msg_send![&*layer, setColors: colors_array];
                let _: () = msg_send![&*layer, setLocations: locations_array];
                let _: () = msg_send![&*layer, setStartPoint: point(fade.start)];
                let _: () = msg_send![&*layer, setEndPoint: point(fade.end)];
                let _: () = msg_send![&*layer, setZPosition: 1000_f64];
                let _: () = msg_send![host, addSublayer: &*layer];
                for color in colors {
                    CGColorRelease(color);
                }
                made.push(layer);
            }
            for ([x, y, width, height], [red, green, blue, alpha]) in &laid.tints {
                let layer: Option<Retained<AnyObject>> = msg_send![class, layer];
                let Some(layer) = layer else { continue };
                let y = if flipped { *y } else { bounds.size.height - y - height };
                let color = CGColorCreateSRGB(*red, *green, *blue, *alpha);
                let _: () =
                    msg_send![&*layer, setFrame: NSRect::new(NSPoint::new(*x, y), NSSize::new(*width, *height))];
                let _: () = msg_send![&*layer, setBackgroundColor: Color(color)];
                let _: () = msg_send![&*layer, setZPosition: 1000_f64];
                let _: () = msg_send![host, addSublayer: &*layer];
                CGColorRelease(color);
                made.push(layer);
            }
            TINT_LAYERS.with(|layers| layers.borrow_mut().insert(key, made));
        }
    }

    /// The view's layer shows all but `holes` (rects from its top left), or all of it with none.
    unsafe fn set_mask(view: &AnyObject, holes: &[([f64; 4], f64)], (width, height): (f64, f64)) {
        unsafe {
            let layer: *mut AnyObject = msg_send![view, layer];
            if layer.is_null() {
                return;
            }
            if holes.is_empty() {
                let _: () = msg_send![layer, setMask: std::ptr::null_mut::<AnyObject>()];
                return;
            }
            let Some(shape_class) = AnyClass::get(c"CAShapeLayer") else { return };
            let shape: *mut AnyObject = msg_send![shape_class, layer];
            let size = NSSize::new(width, height);
            let path = CGPathCreateMutable();
            CGPathAddRect(path, std::ptr::null(), NSRect::new(NSPoint::new(0., 0.), size));
            let flipped: bool = msg_send![layer, contentsAreFlipped];
            for ([x, y, w, h], radius) in holes {
                // The mask's y runs up unless the layer is drawn flipped.
                let y = if flipped { *y } else { height - y - h };
                let radius = corner_radius(*radius, *w, *h);
                CGPathAddRoundedRect(
                    path,
                    std::ptr::null(),
                    NSRect::new(NSPoint::new(*x, y), NSSize::new(*w, *h)),
                    radius,
                    radius,
                );
            }
            let rule = objc2_foundation::NSString::from_str("even-odd");
            let _: () = msg_send![shape, setFrame: NSRect::new(NSPoint::new(0., 0.), size)];
            let _: () = msg_send![shape, setPath: Path(path)];
            let _: () = msg_send![shape, setFillRule: &*rule];
            let _: () = msg_send![layer, setMask: shape];
            CGPathRelease(path);
        }
    }

    /// Gives the keyboard back to GPUI's view when a view inside it (a web view) has it.
    pub fn take_keys(window: &gpui::Window) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
        let view = handle.ns_view.as_ptr() as *mut AnyObject;
        // SAFETY: AppKit messages on the main thread; `view` is GPUI's view, alive with its window.
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            if ns_window.is_null() {
                return;
            }
            let responder: *mut AnyObject = msg_send![ns_window, firstResponder];
            let Some(ns_view) = AnyClass::get(c"NSView") else { return };
            if responder.is_null() || responder == view {
                return;
            }
            let is_view: objc2::runtime::Bool = msg_send![responder, isKindOfClass: ns_view];
            if !is_view.as_bool() {
                return;
            }
            let inside: objc2::runtime::Bool = msg_send![responder, isDescendantOf: view];
            if inside.as_bool() {
                let _: objc2::runtime::Bool = msg_send![ns_window, makeFirstResponder: view];
            }
        }
    }

    /// wry's web view class hit-tests through its holes (`hit_test`), once; whether it does.
    fn hit_test_installed() -> bool {
        static INSTALLED: OnceLock<bool> = OnceLock::new();
        *INSTALLED.get_or_init(|| {
            let Some(class) = AnyClass::get(c"WryWebView") else { return false };
            // SAFETY: `hit_test` has `hitTest:`'s signature (an object from an NSPoint), and wry's class doesn't
            // define the method, so this adds it over WKWebView's (which `hit_test` calls).
            unsafe {
                let imp: Imp = std::mem::transmute::<
                    extern "C-unwind" fn(&AnyObject, Sel, NSPoint) -> *mut AnyObject,
                    Imp,
                >(hit_test);
                objc2::ffi::class_addMethod(
                    class as *const AnyClass as *mut AnyClass,
                    sel!(hitTest:),
                    imp,
                    c"@@:{CGPoint=dd}".as_ptr(),
                )
                .as_bool()
            }
        })
    }

    /// A click in one of the view's holes or tints is the window's (GPUI's view under it): the view says it isn't there.
    extern "C-unwind" fn hit_test(this: &AnyObject, _: Sel, point: NSPoint) -> *mut AnyObject {
        let key = this as *const AnyObject as usize;
        // SAFETY: AppKit messages on the main thread, where AppKit hit-tests.
        unsafe {
            if let Some(cut) = CUTS.with(|cuts| cuts.borrow().get(&key).cloned()) {
                let superview: *mut AnyObject = msg_send![this, superview];
                let local: NSPoint = msg_send![this, convertPoint: point, fromView: superview];
                let flipped: bool = msg_send![this, isFlipped];
                let bounds: NSRect = msg_send![this, bounds];
                let y = if flipped { local.y } else { bounds.size.height - local.y };
                let inside = |[x0, y0, w, h]: &[f64; 4]| local.x >= *x0 && local.x < x0 + w && y >= *y0 && y < y0 + h;
                if cut.holes.iter().any(|(rect, _)| inside(rect)) || cut.tints.iter().any(|(rect, _)| inside(rect)) {
                    return std::ptr::null_mut();
                }
            }
            let Some(web_view) = AnyClass::get(c"WKWebView") else { return std::ptr::null_mut() };
            msg_send![super(this, web_view), hitTest: point]
        }
    }
}

// Whether the person is looking at Filo, and the Dock badge. A chat on screen
// counts as read while Filo is the active app, or while its window is in view
// and the Mac has had input in the last half minute (Filo side by side with
// another app); an untouched Mac or a covered window doesn't.

#![allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare

use gpui::Window;
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Input this recent means someone is at the Mac.
const IDLE_AFTER_SECS: f64 = 30.;
/// `NSWindowOcclusionStateVisible`.
const OCCLUSION_VISIBLE: usize = 1 << 1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
}

/// Seconds since the last keyboard, mouse or trackpad input anywhere on the Mac.
pub(crate) fn idle_seconds() -> f64 {
    // kCGEventSourceStateCombinedSessionState, kCGAnyInputEventType
    unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) }
}

/// Whether some of the window shows on screen (not minimized, covered, or on a locked screen).
pub(crate) fn window_in_view(window: &Window) -> bool {
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return false };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return false };
    // SAFETY: read-only AppKit messages on the main thread.
    unsafe {
        let view = handle.ns_view.as_ptr() as *mut Object;
        let ns_window: *mut Object = msg_send![view, window];
        if ns_window.is_null() {
            return false;
        }
        let state: usize = msg_send![ns_window, occlusionState];
        state & OCCLUSION_VISIBLE != 0
    }
}

pub fn user_present(window: &Window) -> bool {
    window.is_window_active() || (window_in_view(window) && idle_seconds() < IDLE_AFTER_SECS)
}

/// The number on Filo's Dock icon; None clears it.
pub fn set_dock_badge(label: Option<&str>) {
    // SAFETY: AppKit messages on the main thread; the label string is autoreleased by AppKit.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let tile: *mut Object = msg_send![app, dockTile];
        if tile.is_null() {
            return;
        }
        let label: *mut Object = match label {
            Some(text) => {
                let text = std::ffi::CString::new(text).unwrap_or_default();
                msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()]
            }
            None => std::ptr::null_mut(),
        };
        let _: () = msg_send![tile, setBadgeLabel: label];
    }
}

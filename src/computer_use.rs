// What computer use needs from macOS: the runner's Accessibility bridge (computer-use/) acts on the user's apps on
// Filo's behalf. It runs as a child of the app, so macOS asks for, and counts, the permissions as Filo's. Accessibility
// lets it read and work apps; Screen Recording lets it take screenshots of their windows.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Whether Filo may use Accessibility (read and work the user's apps).
pub fn accessibility_allowed() -> bool {
    // SAFETY: a read-only query of this process's trust.
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Asks macOS to add Filo to Accessibility (its own prompt, with a button to System Settings).
pub fn ask_accessibility() {
    if crate::test_run::refuse("Accessibility prompt") {
        return;
    }
    // SAFETY: builds `{ AXTrustedCheckOptionPrompt: true }` (NSDictionary is toll-free bridged to CFDictionary) and
    // passes it for the duration of the call.
    unsafe {
        let key = NSString::from_str("AXTrustedCheckOptionPrompt");
        let yes: Retained<AnyObject> = msg_send![class!(NSNumber), numberWithBool: true];
        let options: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionaryWithObject: &*yes, forKey: &*key];
        AXIsProcessTrustedWithOptions(Retained::as_ptr(&options) as *const c_void);
    }
}

/// Whether Filo may record the screen (pictures of windows).
pub fn screen_recording_allowed() -> bool {
    // SAFETY: a read-only query.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Asks macOS for Screen Recording (it shows its prompt once; after that only System Settings changes it).
pub fn ask_screen_recording() {
    if crate::test_run::refuse("Screen Recording prompt") {
        return;
    }
    // SAFETY: shows the system prompt when macOS still asks.
    unsafe {
        CGRequestScreenCaptureAccess();
    }
}

/// Opens System Settings at a privacy pane: "Privacy_Accessibility" or "Privacy_ScreenCapture".
pub fn open_settings(pane: &str) {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{pane}"))
        .spawn();
}

// A screenshot of what the user is looking at, sent with a voice message about it (hold ⌥⌘ in any app;
// companion.rs). The display under the pointer is taken with macOS's own screencapture, made ready for the model
// like any picked image (screens/chat/attachments.rs) and uploaded while the user is still talking, so the message
// goes out as soon as they let go. macOS asks once for Screen Recording; until it's allowed the message goes
// without a screenshot. The pet keeps out of this picture, though it's in the user's own (pet.rs).

use futures::FutureExt as _;
use futures::future::{LocalBoxFuture, Shared};
use gpui::App;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect};
use serde_json::{Value, json};

use crate::backend::backend;
use crate::screens::chat::attachments;

/// The uploaded screenshot as a message attachment (`{storageId, name, contentType, size}`), or None if it
/// couldn't be taken or uploaded.
pub type Screenshot = Shared<LocalBoxFuture<'static, Option<Value>>>;

const NAME: &str = "Screen.jpg";

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Whether macOS lets Filo see the screen (System Settings › Privacy & Security › Screen Recording).
pub fn allowed() -> bool {
    // SAFETY: a read-only query.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Takes the screenshot and starts uploading it. None when Filo may not see the screen yet: macOS is asked then
/// (once; after that it's System Settings), and this message goes without one.
pub fn take(cx: &mut App) -> Option<Screenshot> {
    if crate::test_run::refuse("screenshot of the screen") {
        return None;
    }
    if !allowed() {
        // SAFETY: shows macOS's prompt, or does nothing once the user has answered.
        unsafe { CGRequestScreenCaptureAccess() };
        return None;
    }
    let display = display_under_pointer();
    let backend = backend(cx);
    let path = std::env::temp_dir().join(format!("filo-screen-{}.jpg", uuid::Uuid::new_v4()));
    // The pet is in the user's screenshots but not in this one: out while screencapture runs, then back, even if
    // the message is dropped before the upload is done.
    crate::pet::set_in_screenshots(false, cx);
    let capture = backend.run({
        let path = path.clone();
        async move {
            let status = tokio::process::Command::new("/usr/sbin/screencapture")
                .args(["-x", "-t", "jpg", "-D", &display.to_string()])
                .arg(&path)
                .status()
                .await;
            status.is_ok_and(|status| status.success())
        }
    });
    let (captured_tx, captured) = futures::channel::oneshot::channel();
    cx.spawn(async move |cx| {
        let ok = capture.await;
        cx.update(|cx| crate::pet::set_in_screenshots(true, cx)).ok();
        let _ = captured_tx.send(ok);
    })
    .detach();
    let upload = backend.clone().run(async move {
        if !captured.await.unwrap_or(false) {
            eprintln!("screenshot: screencapture failed");
            return None;
        }
        let prepared = attachments::prepare(&path, NAME);
        let _ = std::fs::remove_file(&path);
        let (_, content_type, bytes) = prepared.inspect_err(|error| eprintln!("screenshot: {error:#}")).ok()?;
        let size = bytes.len();
        let storage_id = attachments::upload(backend, NAME.to_string(), content_type.clone(), bytes)
            .await
            .inspect_err(|error| eprintln!("screenshot: {error:#}"))
            .ok()?;
        Some(json!({ "storageId": storage_id, "name": NAME, "contentType": content_type, "size": size as f64 }))
    });
    Some(upload.boxed_local().shared())
}

/// screencapture's number (1 = the main display) for the display the pointer is on.
fn display_under_pointer() -> usize {
    // SAFETY: read-only AppKit messages on the main thread.
    unsafe {
        let pointer: NSPoint = msg_send![class!(NSEvent), mouseLocation];
        let screens: Retained<AnyObject> = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![&screens, count];
        (0..count)
            .find(|index| {
                let screen: *mut AnyObject = msg_send![&screens, objectAtIndex: *index];
                let frame: NSRect = msg_send![screen, frame];
                pointer.x >= frame.origin.x
                    && pointer.x < frame.origin.x + frame.size.width
                    && pointer.y >= frame.origin.y
                    && pointer.y < frame.origin.y + frame.size.height
            })
            .map_or(1, |index| index + 1)
    }
}

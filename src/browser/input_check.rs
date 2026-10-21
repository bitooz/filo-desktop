// `FILO_OPEN=browser-input-check` (`dev` builds, on a data folder of its own): what the user does in a page in the
// browser window besides clicks and keys, against the test site's `/input` (browser/check.rs), in the engine new pages
// open in (FILO_BROWSER_ENGINE). A tab of the user's, shown in the window, where ⌘P's print makes a PDF (saved to a file
// here, not the panel). In Chromium also: an input method composes text in the field (marked text, as Japanese input
// sends it, then committed: the page gets composition events and the text, and the candidate window's spot is by the
// field), keys typed after it still type; the page's window.print() prints the same way; the right-click menu has a
// link's items and a
// field's editing ones, and Open Link in New Tab opens it in a tab of the user's (picked by label, not shown); a file
// dropped from another app (a dragging info of the check's own, as Finder's) reaches the page's drop zone with its
// name and size. Each check prints a line; the process ends with 1 when any failed. `FILO_FIXTURE_SHOTS=<dir>`: the
// window after the input method's text.

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::Duration;

use gpui::{App, AsyncApp};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSString};

use super::engine::Kind;
use super::native::{self, Key};

pub const PAGE: &str = r#"<!doctype html><title>Check input</title>
<style>body{font:16px -apple-system;margin:0} input{font:16px -apple-system;padding:6px;position:absolute;left:40px;top:40px;width:300px}
#link{position:absolute;left:40px;top:110px} #drop{position:absolute;left:40px;top:160px;width:300px;height:120px;background:#eef;line-height:120px;text-align:center}
#print{position:absolute;left:400px;top:40px;font:16px -apple-system;padding:6px 14px} #said{position:absolute;left:40px;top:320px;right:40px}</style>
<input id=field placeholder="Type here">
<a id=link href="/form">A link to the form</a>
<div id=drop>Drop a file here</div>
<button id=print onclick="window.print()">Print</button>
<p id=said>said:</p>
<script>
const log = [];
const show = () => said.textContent = 'said: ' + log.join(' | ');
field.addEventListener('compositionstart', () => { log.push('compositionstart'); show(); });
field.addEventListener('compositionend', (e) => { log.push('compositionend ' + e.data); show(); });
field.addEventListener('input', (e) => { if (!e.isComposing) { log.push('value ' + field.value); show(); } });
drop.addEventListener('dragover', (e) => e.preventDefault());
drop.addEventListener('drop', (e) => {
  e.preventDefault();
  const files = [...e.dataTransfer.files].map((file) => file.name + ':' + file.size);
  log.push('dropped ' + (files.join(',') || e.dataTransfer.getData('text/plain'))); show();
});
</script>"#;

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        println!(
            "browser input check: {} {name}: {}",
            if ok { "ok" } else { "FAILED" },
            detail.as_ref().replace('\n', " | ")
        );
        if !ok {
            self.failed += 1;
        }
    }
}

fn sleep(cx: &AsyncApp, ms: u64) -> gpui::Task<()> {
    cx.background_executor().timer(Duration::from_millis(ms))
}

/// What the page's `said` line says now.
async fn said(cx: &AsyncApp, id: &str) -> String {
    let Some(tab) = super::tab(id) else { return String::new() };
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.update(|_| {
        native::run_js(tab.view(), "return document.getElementById('said').textContent;", "", move |result| {
            drop(tx.send(result))
        })
    })
    .ok();
    rx.await.ok().and_then(Result::ok).unwrap_or_default()
}

/// The middle of the element `selector` matches, in points from the page's top left.
async fn spot(cx: &AsyncApp, id: &str, selector: &str) -> (f64, f64) {
    let Some(tab) = super::tab(id) else { return (-1., -1.) };
    let (tx, rx) = futures::channel::oneshot::channel();
    let body = "const r = document.querySelector(a).getBoundingClientRect(); return (r.left + r.width / 2) + ',' + (r.top + r.height / 2);";
    cx.update(|_| native::run_js(tab.view(), body, selector, move |result| drop(tx.send(result)))).ok();
    let text = rx.await.ok().and_then(Result::ok).unwrap_or_default();
    let mut parts = text.split(',').map(|part| part.parse::<f64>().unwrap_or(-1.));
    (parts.next().unwrap_or(-1.), parts.next().unwrap_or(-1.))
}

thread_local! {
    /// The drag the check's dragging info answers for: its pasteboard and spot (window points).
    static DRAG: RefCell<Option<(Retained<AnyObject>, NSPoint)>> = const { RefCell::new(None) };
}

/// A dragging info as Finder's drag hands a view (NSDraggingInfo's three answers a page's view asks for).
fn dragging_info() -> Option<Retained<AnyObject>> {
    static CLASS: OnceLock<Option<usize>> = OnceLock::new();
    let class = CLASS.get_or_init(|| {
        let mut builder = ClassBuilder::new(c"FiloCheckDraggingInfo", AnyClass::get(c"NSObject")?)?;
        extern "C-unwind" fn pasteboard(_: &AnyObject, _: Sel) -> *mut AnyObject {
            DRAG.with(|drag| {
                drag.borrow()
                    .as_ref()
                    .map_or(std::ptr::null_mut(), |(pasteboard, _)| Retained::as_ptr(pasteboard) as *mut AnyObject)
            })
        }
        extern "C-unwind" fn location(_: &AnyObject, _: Sel) -> NSPoint {
            DRAG.with(|drag| drag.borrow().as_ref().map_or(NSPoint::new(0., 0.), |(_, at)| *at))
        }
        extern "C-unwind" fn mask(_: &AnyObject, _: Sel) -> usize {
            // NSDragOperationCopy | Generic
            1 | 4
        }
        // SAFETY: each method's signature matches NSDraggingInfo's.
        unsafe {
            builder.add_method(sel!(draggingPasteboard), pasteboard as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingLocation), location as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(draggingSourceOperationMask), mask as extern "C-unwind" fn(_, _) -> _);
        }
        Some(builder.register() as *const AnyClass as usize)
    });
    let class = (*class)?;
    // SAFETY: a registered class; returned retained.
    unsafe {
        let class = &*(class as *const AnyClass);
        msg_send![class, new]
    }
}

pub fn open(cx: &mut App) {
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    let site = format!("http://127.0.0.1:{}", super::check::serve());
    let folder = crate::config::config_dir().join("input-check");
    let _ = std::fs::create_dir_all(&folder);
    cx.spawn(async move |cx| {
        let mut checks = Checks { failed: 0 };
        sleep(cx, 1500).await;
        let engine = super::engine::current();
        println!("browser input check: engine {}", engine.label());
        let id = match cx.update(|_| super::open_for_user(&format!("{site}/input"))).ok() {
            Some(Ok(tab)) => tab.id.clone(),
            other => {
                println!("browser input check: FAILED no tab: {:?}", other.map(|result| result.err()));
                std::process::exit(1);
            }
        };
        cx.update(|cx| super::window::open(Some(id.clone()), cx)).ok();
        sleep(cx, 3000).await;
        let Some(tab) = super::tab(&id) else { std::process::exit(1) };
        let view = Retained::as_ptr(&tab.view) as *mut AnyObject;

        let field = spot(cx, &id, "#field").await;
        let pdf = |path: &std::path::Path| std::fs::read(path).map(|bytes| (bytes.starts_with(b"%PDF"), bytes.len())).unwrap_or((false, 0));
        let mut got = (false, 0);
        if engine == Kind::Chromium {
        // An input method composes Japanese in the field, as the user's keyboard would: marked text, then committed.
        cx.update(|_| native::as_user(tab.view(), Some(field), &[])).ok();
        sleep(cx, 600).await;
        let none = NSRangeLike { location: isize::MAX as usize, length: 0 };
        cx.update(|_| {
            // SAFETY: AppKit's text input calls on the page's view, on the main thread, as an input method makes them.
            unsafe {
                for marked in ["に", "にほ", "にほん"] {
                    let text = NSString::from_str(marked);
                    let selected = NSRangeLike { location: marked.encode_utf16().count(), length: 0 };
                    let _: () = msg_send![view, setMarkedText: &*text, selectedRange: selected, replacementRange: none];
                }
            }
        })
        .ok();
        sleep(cx, 500).await;
        let candidates: Option<NSRect> = cx
            .update(|_| {
                // SAFETY: as above.
                unsafe {
                    let marked: bool = msg_send![view, hasMarkedText];
                    marked.then(|| msg_send![view, firstRectForCharacterRange: NSRangeLike { location: 0, length: 3 }, actualRange: std::ptr::null_mut::<NSRangeLike>()])
                }
            })
            .ok()
            .flatten();
        let window: Option<NSRect> = cx
            .update(|_| {
                // SAFETY: AppKit on the main thread; the view's in the browser window.
                unsafe {
                    let window: *mut AnyObject = msg_send![view, window];
                    (!window.is_null()).then(|| msg_send![window, frame])
                }
            })
            .ok()
            .flatten();
        checks.check(
            "the input method's candidates show by the field",
            candidates.zip(window).is_some_and(|(rect, window)| {
                rect.size.height > 0.
                    && rect.origin.x >= window.origin.x
                    && rect.origin.x <= window.origin.x + window.size.width
                    && rect.origin.y >= window.origin.y
                    && rect.origin.y <= window.origin.y + window.size.height
            }),
            format!("{candidates:?} in window {window:?}"),
        );
        cx.update(|_| {
            // SAFETY: as above.
            unsafe {
                let text = NSString::from_str("日本");
                let _: () = msg_send![view, insertText: &*text, replacementRange: none];
            }
        })
        .ok();
        sleep(cx, 500).await;
        for key in "ok".chars().map(Key::typing) {
            cx.update(|_| native::as_user(tab.view(), None, std::slice::from_ref(&key))).ok();
            sleep(cx, 80).await;
        }
        sleep(cx, 600).await;
        let page = said(cx, &id).await;
        checks.check(
            "an input method composes in the field, and keys type after it",
            page.contains("compositionstart") && page.contains("compositionend 日本") && page.contains("value 日本ok"),
            &page,
        );
        if let Some(dir) = &shots {
            crate::window_shot::save_largest(format!("{dir}/input-{}.png", engine.label().to_lowercase())).await;
        }

        // The page's window.print(), as a PDF saved here.
        let printed = folder.join("window-print.pdf");
        cx.update(|_| native::print_to(Some(printed.clone()))).ok();
        let button = spot(cx, &id, "#print").await;
        cx.update(|_| native::as_user(tab.view(), Some(button), &[])).ok();
        for _ in 0..40 {
            sleep(cx, 250).await;
            got = pdf(&printed);
            if got.0 {
                break;
            }
        }
        checks.check("the page's window.print() prints", got.0 && got.1 > 1000, format!("{} {got:?}", printed.display()));
        }

        // ⌘P's print (browser.rs `print`), in either engine, as a PDF saved here.
        let command_p = folder.join("command-p.pdf");
        cx.update(|_| native::print_to(Some(command_p.clone()))).ok();
        let asked = cx.update(|_| super::print(&id)).ok();
        for _ in 0..40 {
            sleep(cx, 250).await;
            got = pdf(&command_p);
            if got.0 {
                break;
            }
        }
        checks.check("⌘P's print prints", got.0 && got.1 > 1000, format!("{asked:?} {} {got:?}", command_p.display()));
        cx.update(|_| native::print_to(None)).ok();

        if engine == Kind::Chromium {
            // The right-click menu: a link's items, a field's editing ones; Open Link in New Tab opens it for the user.
            let link = spot(cx, &id, "#link").await;
            let before = super::tabs().len();
            cx.update(|_| super::chromium::pick_from_menu(Some("Open Link in New Tab"))).ok();
            for kind in [native::Mouse::RightDown, native::Mouse::RightUp] {
                cx.update(|_| native::mouse(tab.view(), kind, link.0, link.1, 1)).ok();
                sleep(cx, 60).await;
            }
            sleep(cx, 1500).await;
            let menu = super::chromium::last_menu();
            let opened = super::tabs().into_iter().skip(before).map(|tab| (tab.id.clone(), tab.owner.is_none(), tab.state().1)).collect::<Vec<_>>();
            checks.check(
                "a link's menu, and Open Link in New Tab opens it in a tab of the user's",
                menu.iter().any(|item| item == "Copy Link Address")
                    && opened.iter().any(|(_, users, url)| *users && url.ends_with("/form")),
                format!("menu {menu:?}; opened {opened:?}"),
            );
            for (id, _, _) in &opened {
                super::close(id);
            }
            cx.update(|cx| super::window::open(Some(id.clone()), cx)).ok();
            sleep(cx, 800).await;
            cx.update(|_| super::chromium::pick_from_menu(Some("(none)"))).ok();
            for kind in [native::Mouse::RightDown, native::Mouse::RightUp] {
                cx.update(|_| native::mouse(tab.view(), kind, field.0, field.1, 1)).ok();
                sleep(cx, 60).await;
            }
            sleep(cx, 1000).await;
            let menu = super::chromium::last_menu();
            checks.check(
                "a field's menu has its editing items",
                ["Cut", "Copy", "Paste", "Select All"].iter().all(|wanted| menu.iter().any(|item| item.contains(wanted))),
                format!("{menu:?}"),
            );

            // A file dropped from another app (Finder) on the drop zone.
            let file = folder.join("dropped.txt");
            let _ = std::fs::write(&file, "twelve bytes");
            let zone = spot(cx, &id, "#drop").await;
            cx.update(|_| {
                // SAFETY: AppKit on the main thread: a pasteboard of the check's own with the file, and the view's
                // drag destination calls as AppKit makes them.
                unsafe {
                    let pasteboard: Retained<AnyObject> = msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
                    let _: isize = msg_send![&*pasteboard, clearContents];
                    let path = NSString::from_str(&file.display().to_string());
                    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: &*path];
                    let urls: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: url];
                    let _: bool = msg_send![&*pasteboard, writeObjects: urls];
                    let at: NSPoint = msg_send![view, convertPoint: NSPoint::new(zone.0, zone.1), toView: std::ptr::null_mut::<AnyObject>()];
                    DRAG.with(|drag| *drag.borrow_mut() = Some((pasteboard, at)));
                }
            })
            .ok();
            let info = cx.update(|_| dragging_info()).ok().flatten();
            if let Some(info) = info {
                for step in [sel!(draggingEntered:), sel!(draggingUpdated:), sel!(draggingUpdated:)] {
                    // SAFETY: as above; the info is the check's own.
                    cx.update(|_| unsafe {
                        let _: usize = msg_send![view, performSelector: step, withObject: &*info];
                    })
                    .ok();
                    sleep(cx, 150).await;
                }
                // SAFETY: as above.
                cx.update(|_| unsafe {
                    let _: bool = msg_send![view, performDragOperation: &*info];
                })
                .ok();
            }
            sleep(cx, 800).await;
            let page = said(cx, &id).await;
            checks.check("a file dropped from another app reaches the page", page.contains("dropped dropped.txt:12"), &page);
            DRAG.with(|drag| drag.borrow_mut().take());
        }

        println!("browser-input-check: {}", if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) });
        crate::window_shot::flush(cx.background_executor()).await;
        super::remove_test_store(cx).await;
        cx.update(|_| super::quit()).ok();
        sleep(cx, 1500).await;
        std::process::exit(if checks.failed == 0 { 0 } else { 1 });
    })
    .detach();
}

/// NSRange, for the input method's calls.
#[repr(C)]
#[derive(Clone, Copy)]
struct NSRangeLike {
    location: usize,
    length: usize,
}

// SAFETY: NSRange's layout and encoding.
unsafe impl objc2::Encode for NSRangeLike {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("_NSRange", &[objc2::Encoding::ULong, objc2::Encoding::ULong]);
}

// SAFETY: as NSRange's.
unsafe impl objc2::RefEncode for NSRangeLike {
    const ENCODING_REF: objc2::Encoding = objc2::Encoding::Pointer(&<NSRangeLike as objc2::Encode>::ENCODING);
}

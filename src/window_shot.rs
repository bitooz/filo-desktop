// Screenshots of Filo's own windows for smoke runs and fixtures, made in the app: the frame GPUI last drew, read back
// from the window's Metal layer, with each web view's own snapshot (WebKit's) laid over it where it shows. Nothing
// here asks macOS for Screen Recording, so test windows never bring up its prompt (test_run.rs); `screencapture`
// asked for every picture. Unlike macOS's own picture there's no shadow, the corners are square and the traffic
// lights (AppKit's, over the content) aren't in it. A web view's part comes a moment later (WebKit snapshots
// asynchronously): `Shot` resolves when the file is complete, quitting waits for pictures still being made, and
// fixtures that end with `std::process::exit` call `flush` first.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{Encode, Encoding, class, msg_send, sel};
use objc2_foundation::NSRect;

/// `-[CAMetalLayer nextDrawable]` as it was.
static NEXT_DRAWABLE: OnceLock<Imp> = OnceLock::new();
/// The last drawable each Metal layer handed GPUI (retained, by the layer), and when.
static LAST: LazyLock<Mutex<HashMap<usize, (usize, Instant)>>> = LazyLock::new(Default::default);
/// Pictures still waiting on a web view's snapshot.
static PENDING: AtomicUsize = AtomicUsize::new(0);
/// While quitting waits for them: when it stops waiting.
static QUIT_BY: Mutex<Option<Instant>> = Mutex::new(None);

/// GPUI's frame commands for a drawable can still be running just after it was handed out.
const FRESH: Duration = Duration::from_millis(40);
/// The longest quitting (or `flush`) waits for web views' snapshots.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// A saved picture: resolves to whether it was written. Dropping it doesn't stop it.
pub struct Shot(oneshot::Receiver<bool>);

impl Future for Shot {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool> {
        Pin::new(&mut self.0).poll(cx).map(|saved| saved.unwrap_or(false))
    }
}

/// In a test run, before any window opens: Metal layers keep their frames readable and remember the last one, and
/// quitting waits for pictures still being made. The real app draws as GPUI does, and so does the run timing a chat's
/// frames (`FILO_OPEN=chat-scroll-timing`) unless it's recorded (`FILO_DRIVE_VIDEO`, `Recording`): a kept frame is one
/// fewer for GPUI to draw into.
pub fn init() {
    let timing = std::env::var("FILO_OPEN").is_ok_and(|open| open == "chat-scroll-timing")
        && std::env::var_os("FILO_DRIVE_VIDEO").is_none();
    if !crate::test_run::active() || timing {
        return;
    }
    let Some(class) = AnyClass::get(c"CAMetalLayer") else { return };
    let Some(method) = class.instance_method(sel!(nextDrawable)) else { return };
    if NEXT_DRAWABLE.set(method.implementation()).is_ok() {
        // SAFETY: `next_drawable` has nextDrawable's signature and calls the implementation kept above.
        unsafe {
            method.set_implementation(
                std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel) -> *mut AnyObject, Imp>(next_drawable),
            )
        };
    }
    let Some(delegate) = AnyClass::get(c"GPUIApplicationDelegate") else { return };
    // SAFETY: `should_terminate` has applicationShouldTerminate:'s signature (an NSApplicationTerminateReply from the
    // app); GPUI's delegate class doesn't define it, so this adds it.
    unsafe {
        let imp = std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize, Imp>(
            should_terminate,
        );
        objc2::ffi::class_addMethod(
            delegate as *const AnyClass as *mut AnyClass,
            sel!(applicationShouldTerminate:),
            imp,
            c"Q@:@".as_ptr(),
        );
    }
}

/// GPUI asks its layer for a drawable to draw a frame in: one it can be read back from, kept until the next.
extern "C-unwind" fn next_drawable(layer: &AnyObject, sel: Sel) -> *mut AnyObject {
    // SAFETY: plain messages to the layer; the original takes and returns the same as this.
    unsafe {
        let only: bool = msg_send![layer, framebufferOnly];
        if only {
            let _: () = msg_send![layer, setFramebufferOnly: false];
        }
        let original = std::mem::transmute::<Imp, extern "C-unwind" fn(&AnyObject, Sel) -> *mut AnyObject>(
            *NEXT_DRAWABLE.get().expect("kept before swizzling"),
        );
        let drawable = original(layer, sel);
        if !drawable.is_null() {
            objc2::ffi::objc_retain(drawable);
            let before =
                LAST.lock().unwrap().insert(layer as *const AnyObject as usize, (drawable as usize, Instant::now()));
            if let Some((before, _)) = before {
                objc2::ffi::objc_release(before as *mut AnyObject);
            }
        }
        drawable
    }
}

/// Saves a PNG of the window numbered `number` (its `windowNumber`, as `smoke::native_input::window_number` finds
/// it) at `path`.
pub fn save(number: isize, path: impl AsRef<Path>) -> Shot {
    // SAFETY: an AppKit message on the main thread.
    let window: *mut AnyObject = unsafe { msg_send![&*app(), windowWithWindowNumber: number] };
    save_window(window, path.as_ref().to_path_buf())
}

/// The same for the app's largest window on screen (a fixture's only window).
pub fn save_largest(path: impl AsRef<Path>) -> Shot {
    // SAFETY: AppKit messages on the main thread.
    let window = unsafe {
        let windows: *mut AnyObject = msg_send![&*app(), windows];
        let count: usize = msg_send![windows, count];
        (0..count)
            .map(|index| -> *mut AnyObject { msg_send![windows, objectAtIndex: index] })
            .filter(|window| {
                let visible: bool = msg_send![*window, isVisible];
                visible && metal_view(*window).is_some()
            })
            .max_by(|a, b| {
                let area = |window: *mut AnyObject| {
                    let frame: NSRect = msg_send![window, frame];
                    frame.size.width * frame.size.height
                };
                area(*a).total_cmp(&area(*b))
            })
            .unwrap_or(std::ptr::null_mut())
    };
    save_window(window, path.as_ref().to_path_buf())
}

/// A drive's or fixture's step `<name>`: `<FILO_DRIVE_SHOTS>/<name>.png` (or `FILO_FIXTURE_SHOTS`, which the fixtures
/// and older drives were given) of the app's largest window.
pub fn save_step(name: &str) -> Option<Shot> {
    let set = |name: &str| std::env::var(name).ok().filter(|dir| !dir.is_empty());
    let dir = set("FILO_DRIVE_SHOTS").or_else(|| set("FILO_FIXTURE_SHOTS"))?;
    Some(save_largest(Path::new(&dir).join(format!("{name}.png"))))
}

/// Waits (up to 5 s) until every picture being made is saved: before a fixture ends with `std::process::exit`.
pub async fn flush(executor: &gpui::BackgroundExecutor) {
    let started = Instant::now();
    while PENDING.load(Ordering::SeqCst) > 0 && started.elapsed() < QUIT_WAIT {
        executor.timer(Duration::from_millis(20)).await;
    }
}

fn app() -> objc2::rc::Retained<AnyObject> {
    // SAFETY: the shared application, on the main thread.
    unsafe { msg_send![class!(NSApplication), sharedApplication] }
}

fn save_window(window: *mut AnyObject, path: PathBuf) -> Shot {
    let (done, shot) = oneshot::channel();
    let shot = Shot(shot);
    if window.is_null() {
        eprintln!("window shot: {}: no such window", path.display());
        let _ = done.send(false);
        return shot;
    }
    // SAFETY: AppKit, Metal and WebKit on the main thread, on a window AppKit has open.
    let started = objc2::rc::autoreleasepool(|_| unsafe { start(window, &path, done) });
    if let Err((error, done)) = started {
        eprintln!("window shot: {}: {error}", path.display());
        let _ = done.send(false);
    }
    shot
}

/// A picture: premultiplied RGBA, rows from the top.
struct Picture {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

/// A picture waiting on web views' snapshots.
struct Job {
    picture: Picture,
    path: PathBuf,
    waiting: usize,
    done: Option<oneshot::Sender<bool>>,
}

type Done = oneshot::Sender<bool>;

unsafe fn start(window: *mut AnyObject, path: &Path, done: Done) -> Result<(), (String, Done)> {
    unsafe {
        let Some((view, layer)) = metal_view(window) else {
            return Err(("the window has no Metal layer".into(), done));
        };
        let picture = match read_frame(layer) {
            Ok(picture) => picture,
            Err(error) => return Err((error, done)),
        };
        let bounds: NSRect = msg_send![view, bounds];
        let scale = picture.width as f64 / bounds.size.width.max(1.);
        let content: *mut AnyObject = msg_send![window, contentView];
        #[cfg(feature = "dev")]
        let picture = with_chromium(picture, content, view, bounds, scale);
        let mut webs = Vec::new();
        web_views(content, &mut webs);
        let webs: Vec<Web> = webs.into_iter().filter_map(|web| place(web, view, bounds, scale, &picture)).collect();
        if webs.is_empty() {
            return match write(&picture, path) {
                Ok(()) => {
                    let _ = done.send(true);
                    Ok(())
                }
                Err(error) => Err((error, done)),
            };
        }
        // What GPUI drew is there at once; the web views' parts replace it when they come.
        if let Err(error) = write(&picture, path) {
            return Err((error, done));
        }
        PENDING.fetch_add(1, Ordering::SeqCst);
        let job =
            Rc::new(RefCell::new(Job { picture, path: path.to_path_buf(), waiting: webs.len(), done: Some(done) }));
        for web in webs {
            let job = job.clone();
            let view = web.view;
            let handler = block2::RcBlock::new(move |image: *mut AnyObject, _error: *mut AnyObject| {
                let mut job = job.borrow_mut();
                match (!image.is_null()).then(|| image_pixels(image, web.width, web.height)).flatten() {
                    Some(pixels) => lay_over(&mut job.picture, &pixels, &web),
                    None => eprintln!("window shot: {}: a web view's snapshot failed", job.path.display()),
                }
                job.waiting -= 1;
                if job.waiting == 0 {
                    let saved = write(&job.picture, &job.path);
                    if let Err(error) = &saved {
                        eprintln!("window shot: {}: {error}", job.path.display());
                    }
                    if let Some(done) = job.done.take() {
                        let _ = done.send(saved.is_ok());
                    }
                    PENDING.fetch_sub(1, Ordering::SeqCst);
                }
            });
            let _: () = msg_send![view, takeSnapshotWithConfiguration: std::ptr::null_mut::<AnyObject>(), completionHandler: &*handler];
        }
        Ok(())
    }
}

/// The window's GPUI view (the one its Metal layer draws) and that layer.
unsafe fn metal_view(window: *mut AnyObject) -> Option<(*mut AnyObject, *mut AnyObject)> {
    unsafe {
        let metal = AnyClass::get(c"CAMetalLayer")?;
        let content: *mut AnyObject = msg_send![window, contentView];
        let mut queue = VecDeque::from([content]);
        while let Some(view) = queue.pop_front() {
            if view.is_null() {
                continue;
            }
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let is_metal: bool = msg_send![layer, isKindOfClass: metal];
                if is_metal {
                    return Some((view, layer));
                }
            }
            let subviews: *mut AnyObject = msg_send![view, subviews];
            let count: usize = msg_send![subviews, count];
            queue.extend((0..count).map(|index| -> *mut AnyObject { msg_send![subviews, objectAtIndex: index] }));
        }
        None
    }
}

/// The last frame drawn in `layer`, read back from the GPU.
unsafe fn read_frame(layer: *mut AnyObject) -> Result<Picture, String> {
    let kept = LAST.lock().unwrap().get(&(layer as usize)).copied();
    let Some((drawable, at)) = kept else {
        return Err(if NEXT_DRAWABLE.get().is_some() {
            "nothing drawn yet".into()
        } else {
            "not a test run (window_shot::init)".into()
        });
    };
    if let Some(wait) = FRESH.checked_sub(at.elapsed()) {
        std::thread::sleep(wait);
    }
    // SAFETY: `LAST` holds the drawable (retained) until the layer hands out the next one, on this thread.
    let texture = unsafe { &*(drawable as *const metal::MetalDrawableRef) }.texture();
    if texture.framebuffer_only() {
        return Err("the frame can't be read back".into());
    }
    if texture.pixel_format() != metal::MTLPixelFormat::BGRA8Unorm {
        return Err(format!("unexpected pixel format {:?}", texture.pixel_format()));
    }
    let (width, height) = (texture.width(), texture.height());
    let row = width * 4;
    thread_local! {
        static QUEUE: RefCell<Option<metal::CommandQueue>> = const { RefCell::new(None) };
    }
    let device = texture.device();
    let buffer = device.new_buffer(row * height, metal::MTLResourceOptions::StorageModeShared);
    QUEUE.with(|queue| {
        let mut queue = queue.borrow_mut();
        let queue = queue.get_or_insert_with(|| device.new_command_queue());
        let commands = queue.new_command_buffer();
        let blit = commands.new_blit_command_encoder();
        let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        let size = metal::MTLSize { width, height, depth: 1 };
        blit.copy_from_texture_to_buffer(
            texture,
            0,
            0,
            origin,
            size,
            &buffer,
            0,
            row,
            row * height,
            metal::MTLBlitOption::empty(),
        );
        blit.end_encoding();
        commands.commit();
        commands.wait_until_completed();
    });
    let (width, height) = (width as usize, height as usize);
    // SAFETY: the blit above filled the shared buffer, `row * height` bytes, and it's done.
    let bgra = unsafe { std::slice::from_raw_parts(buffer.contents() as *const u8, width * 4 * height) };
    let rgba = bgra.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]]).collect();
    Ok(Picture { width, height, rgba })
}

/// A video of a test window as it draws (`Recording::start`), made from its own frames like the screenshots, so it asks
/// for no permission: each frame read back as it's replaced, written at 30 a second for as long as it showed (a frame
/// that took long to come shows that long), through `ffmpeg`. Reading the frames back slows the window a little.
pub struct Recording {
    layer: usize,
    frames: Option<std::sync::mpsc::Sender<(Instant, Vec<u8>)>>,
    writer: Option<std::thread::JoinHandle<bool>>,
}

impl Recording {
    /// Starts recording the window numbered `number` into `path` (an .mp4). None when there's no such window, it has
    /// drawn nothing yet, or `ffmpeg` can't be started.
    pub fn start(number: isize, path: &Path) -> Option<Recording> {
        // SAFETY: AppKit messages on the main thread, on a window AppKit has open.
        let (layer, (width, height)) = unsafe {
            let window: *mut AnyObject = msg_send![&*app(), windowWithWindowNumber: number];
            if window.is_null() {
                return None;
            }
            let (_, layer) = metal_view(window)?;
            (layer as usize, frame_bgra(layer).ok()?.0)
        };
        let mut ffmpeg = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-s"])
            .arg(format!("{width}x{height}"))
            .args(["-r", "30", "-i", "-", "-vf", "scale=trunc(iw/4)*2:-2", "-c:v", "libx264", "-preset", "veryfast"])
            .args(["-pix_fmt", "yuv420p", "-crf", "24"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        let mut stdin = ffmpeg.stdin.take()?;
        let (frames, received) = std::sync::mpsc::channel::<(Instant, Vec<u8>)>();
        let writer = std::thread::spawn(move || {
            use std::io::Write as _;
            const STEP: Duration = Duration::from_nanos(1_000_000_000 / 30);
            let mut shown: Option<(Instant, Vec<u8>)> = None;
            let mut next: Option<Instant> = None;
            let mut ok = true;
            for (at, frame) in received {
                // The frame showing until this one came, once for each 30th of a second it showed.
                if let Some((_, previous)) = &shown {
                    let mut out = next.unwrap_or(at);
                    while out < at && ok {
                        ok = stdin.write_all(previous).is_ok();
                        out += STEP;
                    }
                    next = Some(out);
                } else {
                    next = Some(at);
                }
                shown = Some((at, frame));
            }
            if let Some((_, last)) = &shown {
                ok &= stdin.write_all(last).is_ok();
            }
            drop(stdin);
            ffmpeg.wait().is_ok_and(|status| status.success()) && ok
        });
        Some(Recording { layer, frames: Some(frames), writer: Some(writer) })
    }

    /// Takes the frame drawn last: call it as the next one starts (`Window::on_next_frame`), when it's done drawing.
    pub fn frame(&self) {
        // SAFETY: the layer belongs to a window that's still open while the drive records it, on the main thread.
        if let Ok((_, bgra)) = unsafe { frame_bgra(self.layer as *mut AnyObject) }
            && let Some(frames) = &self.frames
        {
            let _ = frames.send((Instant::now(), bgra));
        }
    }

    /// Ends the video and waits for it to be written; whether it was.
    pub fn finish(mut self) -> bool {
        drop(self.frames.take());
        self.writer.take().is_some_and(|writer| writer.join().unwrap_or(false))
    }
}

/// The last frame drawn in `layer`, as it is (BGRA, rows from the top), with its size: `read_frame` without waiting
/// for it to be fresh, for a frame already replaced on screen.
unsafe fn frame_bgra(layer: *mut AnyObject) -> Result<((usize, usize), Vec<u8>), String> {
    let kept = LAST.lock().unwrap().get(&(layer as usize)).copied();
    let Some((drawable, _)) = kept else { return Err("nothing drawn yet".into()) };
    // SAFETY: `LAST` holds the drawable (retained) until the layer hands out the next one, on this thread.
    let texture = unsafe { &*(drawable as *const metal::MetalDrawableRef) }.texture();
    if texture.framebuffer_only() || texture.pixel_format() != metal::MTLPixelFormat::BGRA8Unorm {
        return Err("the frame can't be read back".into());
    }
    let (width, height) = (texture.width(), texture.height());
    let row = width * 4;
    let device = texture.device();
    let buffer = device.new_buffer(row * height, metal::MTLResourceOptions::StorageModeShared);
    let queue = device.new_command_queue();
    let commands = queue.new_command_buffer();
    let blit = commands.new_blit_command_encoder();
    let origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
    let size = metal::MTLSize { width, height, depth: 1 };
    blit.copy_from_texture_to_buffer(
        texture,
        0,
        0,
        origin,
        size,
        &buffer,
        0,
        row,
        row * height,
        metal::MTLBlitOption::empty(),
    );
    blit.end_encoding();
    commands.commit();
    commands.wait_until_completed();
    let (width, height) = (width as usize, height as usize);
    // SAFETY: the blit above filled the shared buffer, `row * height` bytes, and it's done.
    let bgra = unsafe { std::slice::from_raw_parts(buffer.contents() as *const u8, width * 4 * height) }.to_vec();
    Ok(((width, height), bgra))
}

/// Every web view showing under `view`.
unsafe fn web_views(view: *mut AnyObject, out: &mut Vec<*mut AnyObject>) {
    unsafe {
        let Some(web) = AnyClass::get(c"WKWebView") else { return };
        if view.is_null() {
            return;
        }
        let is_web: bool = msg_send![view, isKindOfClass: web];
        if is_web {
            let hidden: bool = msg_send![view, isHiddenOrHasHiddenAncestor];
            let alpha: f64 = msg_send![view, alphaValue];
            if !hidden && alpha > 0. {
                out.push(view);
            }
            return;
        }
        let subviews: *mut AnyObject = msg_send![view, subviews];
        let count: usize = msg_send![subviews, count];
        for index in 0..count {
            let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
            web_views(subview, out);
        }
    }
}

/// The pages in Chromium the window shows (browser/chromium.rs, a test engine), laid over the picture from their last
/// frames.
#[cfg(feature = "dev")]
unsafe fn with_chromium(
    mut picture: Picture,
    content: *mut AnyObject,
    view: *mut AnyObject,
    bounds: NSRect,
    scale: f64,
) -> Picture {
    unsafe fn find(view: *mut AnyObject, class: &AnyClass, out: &mut Vec<*mut AnyObject>) {
        unsafe {
            if view.is_null() {
                return;
            }
            let is: bool = msg_send![view, isKindOfClass: class];
            if is {
                out.push(view);
                return;
            }
            let subviews: *mut AnyObject = msg_send![view, subviews];
            let count: usize = msg_send![subviews, count];
            for index in 0..count {
                let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
                find(subview, class, out);
            }
        }
    }
    let Some(class) = AnyClass::get(c"FiloChromiumView") else { return picture };
    let mut pages = Vec::new();
    unsafe {
        find(content, class, &mut pages);
        for page in pages {
            if let Some(web) = place(page, view, bounds, scale, &picture)
                && let Some(pixels) = crate::browser::chromium_picture(page, web.width, web.height)
            {
                lay_over(&mut picture, &pixels, &web);
            }
        }
    }
    picture
}

/// A web view in the picture: its pixels' rect there, and the holes cut in it for menus (web_holes.rs).
struct Web {
    view: *mut AnyObject,
    x: i64,
    y: i64,
    width: usize,
    height: usize,
    scale: f64,
    holes: Vec<([f64; 4], f64)>,
    /// The tints laid over it (a dialog's dim): rects in its points and their sRGB colours.
    tints: Vec<([f64; 4], [f64; 4])>,
    /// The fades laid over it (the one under a page's header): rects in its points and their gradients.
    fades: Vec<([f64; 4], crate::web_holes::Fade)>,
}

unsafe fn place(
    web: *mut AnyObject,
    view: *mut AnyObject,
    bounds: NSRect,
    scale: f64,
    picture: &Picture,
) -> Option<Web> {
    unsafe {
        let own: NSRect = msg_send![web, bounds];
        let rect: NSRect = msg_send![web, convertRect: own, toView: view];
        let flipped: bool = msg_send![view, isFlipped];
        let top = if flipped {
            rect.origin.y - bounds.origin.y
        } else {
            bounds.origin.y + bounds.size.height - rect.origin.y - rect.size.height
        };
        let (x, y) = (((rect.origin.x - bounds.origin.x) * scale).round() as i64, (top * scale).round() as i64);
        let (width, height) = ((rect.size.width * scale).round() as usize, (rect.size.height * scale).round() as usize);
        let inside =
            x < picture.width as i64 && y < picture.height as i64 && x + width as i64 > 0 && y + height as i64 > 0;
        let (holes, tints, fades) = crate::web_holes::hole_rects(web);
        (inside && width > 0 && height > 0).then_some(Web {
            view: web,
            x,
            y,
            width,
            height,
            scale,
            holes,
            tints,
            fades,
        })
    }
}

/// `CGImageRef`, as AppKit's `CGImageForProposedRect:context:hints:` returns it.
#[repr(transparent)]
struct CgImage(*mut c_void);

// SAFETY: a pointer to an opaque CGImage, as AppKit declares it.
unsafe impl Encode for CgImage {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}

/// Core Graphics' rect, as pictures/imageio.rs declares it too.
#[repr(C)]
struct CGRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

// The same signatures as pictures/imageio.rs's.
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

/// An NSImage's pixels at `width` × `height`, premultiplied RGBA from the top.
fn image_pixels(image: *mut AnyObject, width: usize, height: usize) -> Option<Vec<u8>> {
    /// kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big: R, G, B, A in memory.
    const RGBA: u32 = 1 | (4 << 12);
    // SAFETY: AppKit and Core Graphics on the main thread; the context draws into `pixels`, which outlives it.
    unsafe {
        let cg: CgImage = msg_send![image, CGImageForProposedRect: std::ptr::null_mut::<NSRect>(), context: std::ptr::null_mut::<AnyObject>(), hints: std::ptr::null_mut::<AnyObject>()];
        if cg.0.is_null() {
            return None;
        }
        let mut pixels = vec![0u8; width * height * 4];
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        let context = CGBitmapContextCreate(pixels.as_mut_ptr().cast(), width, height, 8, width * 4, space, RGBA);
        CGColorSpaceRelease(space);
        if context.is_null() {
            return None;
        }
        CGContextDrawImage(context, CGRect { x: 0., y: 0., width: width as f64, height: height as f64 }, cg.0);
        CGContextRelease(context);
        Some(pixels)
    }
}

/// Lays a web view's snapshot over the picture, except in its holes (GPUI's menus show there).
fn lay_over(picture: &mut Picture, pixels: &[u8], web: &Web) {
    for row in 0..web.height {
        let y = web.y + row as i64;
        if y < 0 || y >= picture.height as i64 {
            continue;
        }
        for column in 0..web.width {
            let x = web.x + column as i64;
            if x < 0 || x >= picture.width as i64 {
                continue;
            }
            let (px, py) = ((column as f64 + 0.5) / web.scale, (row as f64 + 0.5) / web.scale);
            if web.holes.iter().any(|hole| crate::web_holes::in_hole(hole, px, py)) {
                continue;
            }
            let from = &pixels[(row * web.width + column) * 4..][..4];
            let to = &mut picture.rgba[(y as usize * picture.width + x as usize) * 4..][..4];
            let keep = 255 - u32::from(from[3]);
            for channel in 0..4 {
                to[channel] = (u32::from(from[channel]) + (u32::from(to[channel]) * keep + 127) / 255).min(255) as u8;
            }
            let fades = web.fades.iter().filter_map(|([left, top, width, height], fade)| {
                let inside = px >= *left && px < left + width && py >= *top && py < top + height;
                inside.then(|| fade.color_at((px - left) / width, (py - top) / height, *width, *height))
            });
            for [red, green, blue, alpha] in
                fades.chain(web.tints.iter().filter_map(|([left, top, width, height], color)| {
                    (px >= *left && px < left + width && py >= *top && py < top + height).then_some(*color)
                }))
            {
                for (channel, value) in [red, green, blue].into_iter().enumerate() {
                    to[channel] = (f64::from(to[channel]) * (1. - alpha) + value * 255. * alpha).round() as u8;
                }
            }
        }
    }
}

/// Writes the picture as a PNG (straight alpha), whole or not at all.
fn write(picture: &Picture, path: &Path) -> Result<(), String> {
    let mut rgba = picture.rgba.clone();
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    let image =
        image::RgbaImage::from_raw(picture.width as u32, picture.height as u32, rgba).ok_or("bad picture size")?;
    let mut png = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|error| error.to_string())?;
    filo_local::files::write_atomic(path, &png).map_err(|error| error.to_string())
}

#[allow(non_upper_case_globals)]
unsafe extern "C" {
    /// `dispatch_get_main_queue()`.
    static _dispatch_main_q: u8;
    fn dispatch_time(when: u64, delta: i64) -> u64;
    fn dispatch_after_f(when: u64, queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

/// macOS asks before quitting (`terminate:`, which GPUI's quit sends): with pictures still being made, later, once
/// they're saved or after 5 s.
extern "C-unwind" fn should_terminate(_: &AnyObject, _: Sel, _: *mut AnyObject) -> usize {
    const NOW: usize = 1;
    const LATER: usize = 2;
    if PENDING.load(Ordering::SeqCst) == 0 {
        return NOW;
    }
    eprintln!("window shot: quitting once the pictures still being made are saved");
    *QUIT_BY.lock().unwrap() = Some(Instant::now() + QUIT_WAIT);
    check_quit(std::ptr::null_mut());
    LATER
}

/// Every 20 ms while quitting waits: lets it go on once the pictures are saved, or the wait is up.
extern "C" fn check_quit(_: *mut c_void) {
    let by = *QUIT_BY.lock().unwrap();
    let Some(by) = by else { return };
    if PENDING.load(Ordering::SeqCst) == 0 || Instant::now() >= by {
        *QUIT_BY.lock().unwrap() = None;
        // SAFETY: on the main queue, answering the applicationShouldTerminate: that said later.
        unsafe {
            let _: () = msg_send![&*app(), replyToApplicationShouldTerminate: true];
        }
        return;
    }
    // SAFETY: schedules this again on the main queue; it takes no context.
    unsafe {
        dispatch_after_f(
            dispatch_time(0, 20_000_000),
            &raw const _dispatch_main_q as *const c_void,
            std::ptr::null_mut(),
            check_quit,
        )
    };
}

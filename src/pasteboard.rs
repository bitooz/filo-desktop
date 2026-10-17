// What ⌘V brings besides text. GPUI's clipboard only gives text, or a picture when there's no text; files copied in
// Finder come as their names, so their paths are read here. Long text goes as a file too (pasted_long_text). And a
// picture copied from the media viewer or a chat (copy_pictures): GPUI's clipboard writes one item without saying if
// it worked.

#![allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare

use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{App, ClipboardEntry, ImageFormat};
use objc::runtime::{BOOL, NO, Object};
use objc::{class, msg_send, sel, sel_impl};

/// Files copied in Finder, in order.
fn copied_files() -> Vec<PathBuf> {
    // SAFETY: the general pasteboard always exists.
    unsafe { file_paths(msg_send![class!(NSPasteboard), generalPasteboard]) }
}

/// The file URLs on `pasteboard`.
///
/// # Safety
/// `pasteboard` is an `NSPasteboard`.
unsafe fn file_paths(pasteboard: *mut Object) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    // SAFETY: read-only AppKit messages.
    unsafe {
        let classes: *mut Object = msg_send![class!(NSArray), arrayWithObject: class!(NSURL)];
        let options: *mut Object = msg_send![class!(NSDictionary), dictionary];
        let urls: *mut Object = msg_send![pasteboard, readObjectsForClasses: classes options: options];
        if urls.is_null() {
            return paths;
        }
        let count: usize = msg_send![urls, count];
        for index in 0..count {
            let url: *mut Object = msg_send![urls, objectAtIndex: index];
            let is_file: BOOL = msg_send![url, isFileURL];
            if is_file == NO {
                continue;
            }
            let path: *mut Object = msg_send![url, path];
            if path.is_null() {
                continue;
            }
            let utf8: *const c_char = msg_send![path, UTF8String];
            if !utf8.is_null() {
                paths.push(PathBuf::from(CStr::from_ptr(utf8).to_string_lossy().into_owned()));
            }
        }
    }
    paths
}

/// Pasted pictures and long text are kept this long, so a coding task started from the message can still open them.
const KEEP_PASTED: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Where pasted pictures and long text are saved: a folder of the app's own, not the temp folder, so a pasted file has a
/// path the user's message, Filo and a coding tool on this Mac can point at, like a file from Finder.
fn pasted_dir() -> PathBuf {
    crate::config::config_dir().join("pasted")
}

/// Whether `path` is a picture or text pasted into Filo (saved in pasted_dir).
pub fn is_pasted(path: &Path) -> bool {
    path.starts_with(pasted_dir())
}

/// A new file in `dir` for a picture pasted at `now`: `pasted-image-2026-10-07-165012.png`, then `…-2.png` and on.
/// Text is `pasted-text-…txt` (`kind` "text").
fn pasted_file_path(dir: &Path, now: chrono::DateTime<chrono::Local>, kind: &str, extension: &str) -> PathBuf {
    let stem = format!("pasted-{kind}-{}", now.format("%Y-%m-%d-%H%M%S"));
    (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem}.{extension}") } else { format!("{stem}-{n}.{extension}") }))
        .find(|path| !path.exists())
        .expect("a free name")
}

/// Removes pasted pictures older than KEEP_PASTED.
fn clean_pasted(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > KEEP_PASTED));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A copied picture (a screenshot, an image copied in a browser or Preview) saved as a file (pasted_dir).
fn copied_picture(cx: &App) -> Option<PathBuf> {
    let item = cx.read_from_clipboard()?;
    let image = item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image),
        ClipboardEntry::String(_) => None,
    })?;
    let extension = match image.format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Svg => return None,
    };
    let folder = pasted_dir();
    std::fs::create_dir_all(&folder).ok()?;
    clean_pasted(&folder, SystemTime::now());
    let path = pasted_file_path(&folder, chrono::Local::now(), "image", extension);
    std::fs::write(&path, &image.bytes).ok()?;
    Some(path)
}

/// Text being pasted into a message of `draft_chars` characters that goes as a file (`paste.txt`,
/// attachments::pastes_as_file), saved exactly as copied (pasted_dir); None for text that goes into the message.
pub fn pasted_long_text(cx: &App, draft_chars: usize) -> Option<PathBuf> {
    let text = cx.read_from_clipboard()?.text()?;
    let attachments = crate::screens::chat::attachments::pastes_as_file(&text, draft_chars);
    if !attachments || text.len() > crate::screens::chat::attachments::MAX_COPY_BYTES {
        return None;
    }
    let folder = pasted_dir();
    std::fs::create_dir_all(&folder).ok()?;
    clean_pasted(&folder, SystemTime::now());
    let path = pasted_file_path(&folder, chrono::Local::now(), "text", "txt");
    std::fs::write(&path, text.as_bytes()).ok()?;
    Some(path)
}

/// The files ⌘V would attach, or nothing when it's text to paste.
pub fn pasted_files(cx: &App) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = copied_files().into_iter().filter(|path| path.is_file()).collect();
    if !files.is_empty() {
        return files;
    }
    copied_picture(cx).into_iter().collect()
}

/// A picture's bytes as the clipboard takes them, by type: PNG first, which every app pastes, then the file as it was
/// when that's another format (a GIF keeps its animation, a JPEG its bytes). HEIC and other formats the image crate
/// can't read go through AppKit as TIFF. Slow for a big picture: off the main thread.
pub fn picture_for_clipboard(bytes: &[u8]) -> anyhow::Result<Vec<(&'static str, Vec<u8>)>> {
    let format = image::guess_format(bytes).ok();
    let own_type = match format {
        Some(image::ImageFormat::Jpeg) => Some("public.jpeg"),
        Some(image::ImageFormat::Gif) => Some("com.compuserve.gif"),
        Some(image::ImageFormat::WebP) => Some("org.webmproject.webp"),
        Some(image::ImageFormat::Tiff) => Some("public.tiff"),
        Some(image::ImageFormat::Bmp) => Some("com.microsoft.bmp"),
        _ => None,
    };
    let png = if format == Some(image::ImageFormat::Png) {
        Some(bytes.to_vec())
    } else {
        image::load_from_memory(bytes).ok().and_then(|picture| {
            let mut png = Vec::new();
            picture.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
            Some(png)
        })
    };
    let mut types = Vec::new();
    match png {
        Some(png) => {
            types.push(("public.png", png));
            types.extend(own_type.map(|own| (own, bytes.to_vec())));
        }
        None => types.push((
            "public.tiff",
            tiff_from_appkit(bytes).ok_or_else(|| anyhow::anyhow!("This picture's format can't be copied."))?,
        )),
    }
    Ok(types)
}

/// A picture AppKit can read (HEIC and the like) as TIFF.
pub(crate) fn tiff_from_appkit(bytes: &[u8]) -> Option<Vec<u8>> {
    // SAFETY: NSImage and NSData from bytes that outlive the calls; the TIFF is copied out before the pool drains.
    unsafe {
        let data: *mut Object = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
        let picture: *mut Object = msg_send![class!(NSImage), alloc];
        let picture: *mut Object = msg_send![picture, initWithData: data];
        if picture.is_null() {
            return None;
        }
        let tiff: *mut Object = msg_send![picture, TIFFRepresentation];
        let copied = (!tiff.is_null()).then(|| {
            let length: usize = msg_send![tiff, length];
            let start: *const u8 = msg_send![tiff, bytes];
            std::slice::from_raw_parts(start, length).to_vec()
        });
        let _: () = msg_send![picture, release];
        copied
    }
}

/// Puts `pictures` (each picture_for_clipboard's types) on the clipboard in place of what was there, one pasteboard
/// item a picture, as Finder and Photos copy several; false if the pasteboard didn't take them.
pub fn copy_pictures(pictures: &[Vec<(&'static str, Vec<u8>)>]) -> bool {
    // SAFETY: the general pasteboard always exists.
    unsafe { write_pictures(msg_send![class!(NSPasteboard), generalPasteboard], pictures) }
}

/// # Safety
/// `pasteboard` is an `NSPasteboard`.
unsafe fn write_pictures(pasteboard: *mut Object, pictures: &[Vec<(&'static str, Vec<u8>)>]) -> bool {
    if pictures.is_empty() || pictures.iter().any(|types| types.is_empty()) {
        return false;
    }
    // SAFETY: AppKit messages with strings and data built here; the items are autoreleased.
    unsafe {
        let items: *mut Object = msg_send![class!(NSMutableArray), array];
        for types in pictures {
            let item: *mut Object = msg_send![class!(NSPasteboardItem), alloc];
            let item: *mut Object = msg_send![item, init];
            let item: *mut Object = msg_send![item, autorelease];
            for (kind, bytes) in types {
                let kind = std::ffi::CString::new(*kind).expect("a type without nulls");
                let kind: *mut Object = msg_send![class!(NSString), stringWithUTF8String: kind.as_ptr()];
                let data: *mut Object = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
                let set: BOOL = msg_send![item, setData: data forType: kind];
                if set == NO {
                    return false;
                }
            }
            let _: () = msg_send![items, addObject: item];
        }
        let _: isize = msg_send![pasteboard, clearContents];
        let written: BOOL = msg_send![pasteboard, writeObjects: items];
        written != NO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn ns_string(text: &str) -> *mut Object {
        let text = std::ffi::CString::new(text).unwrap();
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    }

    #[test]
    fn reads_files_copied_in_finder() {
        let file = std::env::temp_dir().join("filo-pasteboard-test.png");
        std::fs::write(&file, b"png").unwrap();
        unsafe {
            // A private pasteboard, so the test leaves the clipboard alone.
            let pasteboard: *mut Object = msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
            let _: isize = msg_send![pasteboard, clearContents];
            let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: ns_string(file.to_str().unwrap())];
            let web: *mut Object = msg_send![class!(NSURL), URLWithString: ns_string("https://example.com/a.png")];
            let objects: *mut Object = msg_send![class!(NSMutableArray), array];
            let _: () = msg_send![objects, addObject: url];
            let _: () = msg_send![objects, addObject: web];
            let written: BOOL = msg_send![pasteboard, writeObjects: objects];
            assert!(written != NO);
            let paths = file_paths(pasteboard);
            let _: () = msg_send![pasteboard, releaseGlobally];
            assert_eq!(paths.len(), 1);
            assert!(paths[0].ends_with("filo-pasteboard-test.png"));
        }
    }

    #[test]
    fn pasted_pictures_get_a_name_of_their_own_and_old_ones_go() {
        let dir = std::env::temp_dir().join(format!("filo-pasted-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 10, 7, 16, 50, 12).unwrap();
        let first = pasted_file_path(&dir, now, "image", "png");
        assert_eq!(first, dir.join("pasted-image-2026-10-07-165012.png"));
        std::fs::write(&first, b"png").unwrap();
        // Two in the same second don't overwrite each other.
        assert_eq!(pasted_file_path(&dir, now, "image", "png"), dir.join("pasted-image-2026-10-07-165012-2.png"));
        assert_eq!(pasted_file_path(&dir, now, "text", "txt"), dir.join("pasted-text-2026-10-07-165012.txt"));
        clean_pasted(&dir, SystemTime::now());
        assert!(first.exists());
        clean_pasted(&dir, SystemTime::now() + KEEP_PASTED + Duration::from_secs(60));
        assert!(!first.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_a_picture_as_png_and_as_it_was() {
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 2, image::Rgb([200, 40, 10])))
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let types = picture_for_clipboard(&jpeg).unwrap();
        assert_eq!(types.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(), ["public.png", "public.jpeg"]);
        assert_eq!(image::load_from_memory(&types[0].1).unwrap().width(), 3);
        assert_eq!(types[1].1, jpeg);
        // A PNG goes as it is, once.
        let png = types[0].1.clone();
        assert_eq!(picture_for_clipboard(&png).unwrap(), vec![("public.png", png.clone())]);
        assert!(picture_for_clipboard(b"not a picture").is_err());
        unsafe {
            // A private pasteboard, so the test leaves the clipboard alone.
            let pasteboard: *mut Object = msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
            // Two pictures, two items, each with its PNG.
            assert!(write_pictures(pasteboard, &[types.clone(), types.clone()]));
            let items: *mut Object = msg_send![pasteboard, pasteboardItems];
            let count: usize = msg_send![items, count];
            let item: *mut Object = msg_send![items, objectAtIndex: 1usize];
            let data: *mut Object = msg_send![item, dataForType: ns_string("public.png")];
            let length: usize = msg_send![data, length];
            assert!(!write_pictures(pasteboard, &[]));
            let _: () = msg_send![pasteboard, releaseGlobally];
            assert_eq!(count, 2);
            assert_eq!(length, png.len());
        }
    }
}

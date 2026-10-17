// Files a chat or a page shared: pictures and videos open inside the app (media_viewer.rs, `open`), and so do files it
// can show (`preview_of`), each by itself in the same viewer (media_viewer/file_view.rs), as filo.im's shared result
// page shows them: markdown, JSON, an HTML page, a PDF, and text, logs and code. Anything else opens in its own app on
// this Mac (`open_in_app`). Quick Look (`quick_look`, the panel
// Finder shows on Space) is still the viewer's and the image editor's button for a picture: the panel takes its file
// from a controller it finds in the responder chain, and Filo's app object answers for that
// (`acceptsPreviewPanelControl:`) and hands it the file last opened.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use gpui::{App, Window};

use crate::ui;

/// A chat's attachment or a shared item.
#[derive(Clone)]
pub struct Item {
    /// Its `file://` address, or empty when it's known by `file` alone.
    pub url: String,
    pub name: String,
    pub content_type: String,
    /// The file on this Mac: shown and opened from there.
    pub file: Option<PathBuf>,
}

impl Item {
    /// Where its picture loads from: its file on this Mac while there is one, else its URL.
    pub fn picture(&self) -> gpui::Resource {
        match self.file.as_ref().filter(|file| file.exists()) {
            Some(file) => gpui::Resource::Path(file.as_path().into()),
            None => gpui::Resource::Uri(self.url.clone().into()),
        }
    }
}

thread_local! {
    /// The file the panel shows (main thread only, like the panel).
    static SHOWN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Quick Look picks a viewer by extension: a name without one gets it from the content type.
fn file_name(item: &Item) -> String {
    // The name came from a chat: only its last part, so it can't write outside the folder.
    let name = Path::new(&item.name).file_name().and_then(|name| name.to_str()).unwrap_or("file").to_string();
    if Path::new(&name).extension().is_some() {
        return name;
    }
    let extension = match item.content_type.split(';').next().unwrap_or_default().trim() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/wav" | "audio/x-wav" => "wav",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => return name,
    };
    format!("{name}.{extension}")
}

/// The file on this Mac `item` is: its `file`, else its `file://` address or path. Every file a chat or a page shares is
/// here; the image editor opens pictures from there too.
pub fn download(item: &Item) -> impl Future<Output = anyhow::Result<PathBuf>> + 'static {
    let address = item.url.strip_prefix("file://").unwrap_or(&item.url);
    let file = item
        .file
        .clone()
        .or_else(|| address.starts_with('/').then(|| PathBuf::from(address)))
        .filter(|file| file.exists());
    let name = item.name.clone();
    async move { file.ok_or_else(|| anyhow::anyhow!("{name} isn't on this Mac any more.")) }
}

/// How a file shows in the viewer, as filo.im's shared result page shows it (its `previewOf`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    /// In the app's markdown style.
    Markdown,
    /// Laid out, in monospace.
    Json,
    /// Running in a sandbox, as an artifact does.
    Html,
    /// WebKit's own viewer.
    Pdf,
    /// In monospace: text, logs, code.
    Text,
}

/// Extensions of files that are text, shown in monospace.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "text",
    "log",
    "out",
    "err",
    "csv",
    "tsv",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "xml",
    "plist",
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "css",
    "scss",
    "less",
    "py",
    "rb",
    "go",
    "swift",
    "kt",
    "java",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "m",
    "mm",
    "cs",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "sql",
    "graphql",
    "proto",
    "diff",
    "patch",
    "lock",
    "gradle",
    "vue",
    "svelte",
    "lua",
    "r",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "scala",
    "zig",
    "nim",
    "tf",
    "dockerfile",
    "makefile",
    "gitignore",
    "editorconfig",
    "srt",
    "vtt",
    "rst",
    "adoc",
    "tex",
    "bib",
    "ndjson",
    "jsonl",
];

/// How `item` (not a picture or video) shows in the viewer, by its extension, else its content type; None for a file
/// the viewer can't show, or that only its bytes can tell (`open` looks).
pub fn preview_of(item: &Item) -> Option<Preview> {
    let name = Path::new(&item.name).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_lowercase();
    let extension = match name.rsplit_once('.') {
        Some((_, extension)) => extension.to_string(),
        // "Makefile", "Dockerfile": by their names.
        None => name.clone(),
    };
    let by_extension = match extension.as_str() {
        "md" | "markdown" | "mdown" | "mkd" => Some(Preview::Markdown),
        "json" | "jsonc" | "geojson" | "webmanifest" => Some(Preview::Json),
        "html" | "htm" | "xhtml" => Some(Preview::Html),
        "pdf" => Some(Preview::Pdf),
        extension if TEXT_EXTENSIONS.contains(&extension) => Some(Preview::Text),
        _ => None,
    };
    by_extension.or_else(|| match item.content_type.split(';').next().unwrap_or_default().trim() {
        "text/markdown" => Some(Preview::Markdown),
        "application/json" => Some(Preview::Json),
        "text/html" => Some(Preview::Html),
        "application/pdf" => Some(Preview::Pdf),
        kind if kind.starts_with("text/") => Some(Preview::Text),
        _ => None,
    })
}

/// Whether the start of a file is text: UTF-8 (a character cut off at the end aside) with no NUL in it.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        // Cut inside a character: up to the last whole one.
        Err(error) if error.error_len().is_none() && bytes.len() - error.valid_up_to() < 4 => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return false,
    };
    !text.contains('\0')
}

/// Shows `item`: a picture or video in the app's viewer over `window`, with the file a chat or a result names in the
/// same viewer by itself when it can show it (`preview_of`), anything else in its own app. A file with a name that
/// doesn't say is looked at first: text shows in the viewer.
pub fn open(item: Item, window: &mut Window, cx: &mut App) {
    if crate::media_viewer::is_picture(&item) || crate::media_viewer::is_video(&item) || preview_of(&item).is_some() {
        crate::media_viewer::open(vec![item], 0, None, window, cx);
        return;
    }
    let saved = download(&item);
    window
        .spawn(cx, async move |cx| {
            let result = match saved.await {
                Ok(path) => {
                    let start = cx
                        .background_executor()
                        .spawn({
                            let path = path.clone();
                            async move {
                                use std::io::Read as _;
                                let mut start = Vec::new();
                                std::fs::File::open(&path)?.take(64 * 1024).read_to_end(&mut start)?;
                                anyhow::Ok(start)
                            }
                        })
                        .await;
                    start.map(|start| (path, looks_like_text(&start)))
                }
                Err(error) => Err(error),
            };
            cx.update(|window, cx| match result {
                Ok((_, true)) => crate::media_viewer::open(vec![item], 0, None, window, cx),
                Ok((path, false)) => cx.open_with_system(&path),
                Err(error) => ui::toast_error(window, cx, "Couldn’t open the file", error.to_string()),
            })
            .ok();
        })
        .detach();
}

/// Downloads `item` (once) and opens it in its own app on this Mac.
pub fn open_in_app(item: Item, cx: &mut App) {
    let saved = download(&item);
    cx.spawn(async move |cx| {
        let saved = saved.await;
        cx.update(|cx| match saved {
            Ok(path) => cx.open_with_system(&path),
            Err(error) => {
                if let Some(window) = cx.active_window() {
                    window
                        .update(cx, |_, window, cx| {
                            ui::toast_error(window, cx, "Couldn’t open the file", error.to_string())
                        })
                        .ok();
                }
            }
        })
        .ok();
    })
    .detach();
}

/// Downloads `item` (once) and shows it in Quick Look.
pub fn quick_look(item: Item, cx: &mut App) {
    let saved = download(&item);
    cx.spawn(async move |cx| {
        let result = saved.await.and_then(|path| {
            SHOWN.with(|shown| *shown.borrow_mut() = Some(path));
            panel::show()
        });
        if let Err(error) = result {
            cx.update(|cx| {
                if let Some(window) = cx.active_window() {
                    window
                        .update(cx, |_, window, cx| {
                            ui::toast_error(window, cx, "Couldn’t open the file", error.to_string())
                        })
                        .ok();
                }
            })
            .ok();
        }
    })
    .detach();
}

/// Saves a copy of `item` in the Downloads folder, under a name free there ("photo 2.png" beside "photo.png").
pub fn save_to_downloads(item: Item, cx: &mut App) {
    let saved = download(&item);
    let name = file_name(&item);
    cx.spawn(async move |cx| {
        let result = saved.await.and_then(|path| {
            let folder = dirs::download_dir().ok_or_else(|| anyhow::anyhow!("There's no Downloads folder."))?;
            let target = free_name(&folder, &name);
            std::fs::copy(&path, &target)?;
            anyhow::Ok(target)
        });
        cx.update(|cx| {
            let Some(window) = cx.active_window() else { return };
            window
                .update(cx, |_, window, cx| match &result {
                    Ok(target) => {
                        let name = target.file_name().and_then(|name| name.to_str()).unwrap_or("it");
                        ui::toast(window, cx, format!("Saved {name} to Downloads"));
                    }
                    Err(error) => ui::toast_error(window, cx, "Couldn’t save the file", error.to_string()),
                })
                .ok();
        })
        .ok();
    })
    .detach();
}

/// `name` in `folder`, or with " 2", " 3"… before its extension until no file there has it.
fn free_name(folder: &Path, name: &str) -> PathBuf {
    let path = folder.join(name);
    if !path.exists() {
        return path;
    }
    let stem = Path::new(name).file_stem().and_then(|stem| stem.to_str()).unwrap_or(name);
    let extension = Path::new(name).extension().and_then(|extension| extension.to_str());
    (2..)
        .map(|number| {
            folder.join(match extension {
                Some(extension) => format!("{stem} {number}.{extension}"),
                None => format!("{stem} {number}"),
            })
        })
        .find(|path| !path.exists())
        .expect("a free name")
}

#[allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare
mod panel {
    use std::ffi::CString;
    use std::sync::OnceLock;

    use objc::declare::ClassDecl;
    use objc::runtime::{BOOL, Class, Imp, Object, Sel, YES, class_addMethod};
    use objc::{Encode as _, class, msg_send, sel, sel_impl};

    use super::SHOWN;

    #[link(name = "Quartz", kind = "framework")]
    unsafe extern "C" {}

    extern "C" fn accepts(_: &Object, _: Sel, _: *mut Object) -> BOOL {
        YES
    }

    extern "C" fn begin(_: &Object, _: Sel, panel: *mut Object) {
        // SAFETY: the panel calls this on the main thread with itself.
        unsafe {
            let _: () = msg_send![panel, setDataSource: source()];
        }
    }

    extern "C" fn end(_: &Object, _: Sel, panel: *mut Object) {
        // SAFETY: as in `begin`.
        unsafe {
            let _: () = msg_send![panel, setDataSource: std::ptr::null_mut::<Object>()];
        }
    }

    extern "C" fn count(_: &Object, _: Sel, _: *mut Object) -> isize {
        SHOWN.with(|shown| shown.borrow().is_some() as isize)
    }

    extern "C" fn item(_: &Object, _: Sel, _: *mut Object, _: isize) -> *mut Object {
        let Some(path) = SHOWN.with(|shown| shown.borrow().clone()) else { return std::ptr::null_mut() };
        let Ok(path) = CString::new(path.to_string_lossy().as_bytes()) else { return std::ptr::null_mut() };
        // SAFETY: Foundation calls on the main thread; both objects are autoreleased.
        unsafe {
            let path: *mut Object = msg_send![class!(NSString), stringWithUTF8String: path.as_ptr()];
            msg_send![class!(NSURL), fileURLWithPath: path]
        }
    }

    /// The panel's data source: one item, the file in `SHOWN`. Made once and kept.
    fn source() -> *mut Object {
        static SOURCE: OnceLock<usize> = OnceLock::new();
        *SOURCE.get_or_init(|| {
            let mut decl = ClassDecl::new("FiloQuickLookSource", class!(NSObject)).expect("a new class");
            // SAFETY: the signatures match the selectors' (QLPreviewPanelDataSource).
            unsafe {
                decl.add_method(
                    sel!(numberOfPreviewItemsInPreviewPanel:),
                    count as extern "C" fn(&Object, Sel, *mut Object) -> isize,
                );
                decl.add_method(
                    sel!(previewPanel:previewItemAtIndex:),
                    item as extern "C" fn(&Object, Sel, *mut Object, isize) -> *mut Object,
                );
                let source: *mut Object = msg_send![decl.register(), new];
                source as usize
            }
        }) as *mut Object
    }

    /// Teaches the app object (and its delegate) to control the panel; the panel looks for that up the responder chain.
    fn install() {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            let bool_type = BOOL::encode();
            let accepts_types = CString::new(format!("{}@:@", bool_type.as_str())).expect("types");
            let void_types = CString::new("v@:@").expect("types");
            // SAFETY: adding methods with matching signatures to live classes, on the main thread. A class that
            // already has one keeps its own (class_addMethod doesn't replace).
            unsafe {
                let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
                let delegate: *mut Object = msg_send![app, delegate];
                for object in [app, delegate] {
                    if object.is_null() {
                        continue;
                    }
                    let class: *mut Class = msg_send![object, class];
                    let accepts: Imp = std::mem::transmute(accepts as extern "C" fn(&Object, Sel, *mut Object) -> BOOL);
                    let begin: Imp = std::mem::transmute(begin as extern "C" fn(&Object, Sel, *mut Object));
                    let end: Imp = std::mem::transmute(end as extern "C" fn(&Object, Sel, *mut Object));
                    class_addMethod(class, sel!(acceptsPreviewPanelControl:), accepts, accepts_types.as_ptr());
                    class_addMethod(class, sel!(beginPreviewPanelControl:), begin, void_types.as_ptr());
                    class_addMethod(class, sel!(endPreviewPanelControl:), end, void_types.as_ptr());
                }
            }
        });
    }

    pub fn show() -> anyhow::Result<()> {
        install();
        let Some(panel_class) = Class::get("QLPreviewPanel") else { anyhow::bail!("Quick Look isn't available") };
        // SAFETY: AppKit calls on the main thread (the caller runs on GPUI's foreground executor).
        unsafe {
            let panel: *mut Object = msg_send![panel_class, sharedPreviewPanel];
            let visible: BOOL = msg_send![panel, isVisible];
            if visible == YES {
                let _: () = msg_send![panel, reloadData];
            } else {
                let _: () = msg_send![panel, makeKeyAndOrderFront: std::ptr::null_mut::<Object>()];
            }
        }
        Ok(())
    }
}

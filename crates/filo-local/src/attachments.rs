// Files sent with a message: the composer's upload saves a copy here, under a name of Filo's own
// (`<root>/attachments/<uuid>.<ext>`, the name its storage id), and the user's message keeps it (store.rs
// `Attachment`). A file from the user's Mac, of any size, is kept without reading it (`keep`): an APFS clone, else a
// link to it. A turn hands the pictures to the engine as images (`engine::Input`), a text file's words in the prompt,
// and every file's name and where it's saved. An agent's message carries files too (mcp.rs `send_message`'s `files`,
// `in_place`), shown where they are on this Mac with no copy, each with a bookmark that finds it when it's renamed or
// moved (`moved_to`); one that's gone the chat shows as no longer there.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::store::Attachment;

pub const DIR: &str = "attachments";
/// Files one message carries at most: the composer's, `filo-local send --file`'s and an agent's `files`.
pub const MAX_PER_MESSAGE: usize = 5;
/// A text file's words go in the prompt up to this many characters each, and this many in all.
const TEXT_CHARS: usize = 20_000;
const ALL_TEXT_CHARS: usize = 60_000;
/// Files the CLI is handed as they are (`handed_over`) up to this size: a picture as the API takes one (Filo makes
/// them fit, the app's attachments.rs), a PDF as the composer once capped every file. A larger one it opens itself.
const MAX_IMAGE_INPUT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_PDF_INPUT_BYTES: u64 = 25 * 1024 * 1024;

/// Saves `bytes` for a message; the storage id it's kept under.
pub fn save(root: &Path, content_type: &str, bytes: &[u8]) -> io::Result<String> {
    let dir = root.join(DIR);
    fs::create_dir_all(&dir)?;
    let id = format!("{}.{}", uuid::Uuid::new_v4(), extension_for(content_type));
    // Written aside and moved in, so a copy cut short never passes for the file.
    crate::files::write_atomic(&dir.join(&id), bytes)?;
    Ok(id)
}

/// Keeps the file at `path` for a message without reading it; the storage id it's kept under. An APFS clone where it
/// can be (instant, no more room on the disk, and still there when the original moves or goes); across volumes, a link
/// to the original when `link` (a file of the user's that stays where it is), else a copy.
pub fn keep(root: &Path, path: &Path, content_type: &str, link: bool) -> io::Result<String> {
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not a file"));
    }
    let dir = root.join(DIR);
    fs::create_dir_all(&dir)?;
    // Its own extension, so a video still plays as one (`foo.m4v`); a name's own type else.
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty() && extension.len() <= 8 && extension.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .map_or_else(|| extension_for(content_type).to_string(), str::to_lowercase);
    let id = format!("{}.{extension}", uuid::Uuid::new_v4());
    let target = dir.join(&id);
    if clone(path, &target).is_ok() {
        return Ok(id);
    }
    if link {
        std::os::unix::fs::symlink(path.canonicalize()?, &target)?;
    } else {
        // Copied aside and moved in, so a copy cut short never passes for the file.
        let part = dir.join(format!(".{id}.part"));
        fs::copy(path, &part).inspect_err(|_| _ = fs::remove_file(&part))?;
        fs::rename(&part, &target)?;
    }
    Ok(id)
}

/// The file at `path` (its canonical path) an agent sends with its message (mcp.rs `send_message`), as the message keeps
/// it: where it is, nothing copied, with a bookmark to find it by when it moves. Named as the file is (a link's target,
/// without control characters, so a name can't pass for a line of its own), and typed by that name, but a "picture"
/// whose first bytes aren't a picture's is a plain file.
pub fn in_place(path: &Path) -> io::Result<Attachment> {
    let size = fs::metadata(path)?.len();
    let mut content_type = content_type_for(path);
    if content_type.starts_with("image/") && !starts_as_picture(path) {
        content_type = "application/octet-stream";
    }
    Ok(Attachment {
        storage_id: String::new(),
        name: file_name(path),
        content_type: content_type.into(),
        size,
        file: path.to_path_buf(),
        local_path: None,
        bookmark: crate::bookmark::of(path).map(|bookmark| base64::engine::general_purpose::STANDARD.encode(bookmark)),
    })
}

/// A file's name as a message shows it: control characters (a newline) dropped.
fn file_name(path: &Path) -> String {
    let name: String = path
        .file_name()
        .map(|name| name.to_string_lossy().chars().filter(|c| !c.is_control()).collect())
        .unwrap_or_default();
    if name.trim().is_empty() { "file".into() } else { name }
}

/// Whether the file starts as a PNG, JPEG, GIF, WebP, BMP, TIFF or HEIC/HEIF/AVIF does.
fn starts_as_picture(path: &Path) -> bool {
    let mut head = [0u8; 12];
    let Ok(read) = fs::File::open(path).and_then(|mut file| io::Read::read(&mut file, &mut head)) else { return false };
    let head = &head[..read];
    head.starts_with(b"\x89PNG")
        || head.starts_with(b"\xFF\xD8\xFF")
        || head.starts_with(b"GIF8")
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
        || head.starts_with(b"BM")
        || head.starts_with(b"II*\0")
        || head.starts_with(b"MM\0*")
        || head.get(4..8) == Some(b"ftyp")
}

/// How long a bookmark that found nothing isn't asked again: the chat's queries run many times a second.
const GONE_RECHECK: std::time::Duration = std::time::Duration::from_secs(10);

/// Where an agent's file is now when it's no longer at its path: what its bookmark finds (renamed, or moved on the same
/// disk). None while it's still there, or when it's gone (a bookmark that found nothing is asked again only after
/// `GONE_RECHECK`).
pub fn moved_to(file: &Attachment) -> Option<PathBuf> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    static GONE: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    let bookmark = file.bookmark.as_deref()?;
    if file.file.is_file() {
        return None;
    }
    let gone = GONE.get_or_init(Mutex::default);
    if gone.lock().unwrap().get(&file.file).is_some_and(|at| at.elapsed() < GONE_RECHECK) {
        return None;
    }
    let found = base64::engine::general_purpose::STANDARD
        .decode(bookmark)
        .ok()
        .and_then(|bookmark| crate::bookmark::resolve(&bookmark))
        .filter(|path| path.is_file() && *path != file.file);
    let mut gone = gone.lock().unwrap();
    match &found {
        Some(_) => gone.remove(&file.file),
        None => gone.insert(file.file.clone(), Instant::now()),
    };
    found
}

/// `file` as it is at `path`, where its bookmark found it (`moved_to`): its name the file's own there.
pub fn moved(file: &Attachment, path: PathBuf) -> Attachment {
    Attachment { name: file_name(&path), file: path, ..file.clone() }
}

/// A file's content type by its extension, as the composer, the CLI's `send --file` and an agent's `files` tell it.
pub fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()).unwrap_or_default().to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "aif" | "aiff" => "audio/aiff",
        "caf" => "audio/x-caf",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "vtt" => "text/vtt",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// `clonefile(2)`: `to` shares `from`'s blocks until either changes. Fails across volumes and off APFS.
fn clone(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = |path: &Path| {
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    };
    let (from, to) = (c(from)?, c(to)?);
    // SAFETY: two NUL-terminated paths that outlive the call.
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A storage id for a file of `content_type` that's saved later (`save_as`): a voice message's recording, named as its
/// message is sent and written just after.
pub fn reserve(content_type: &str) -> String {
    format!("{}.{}", uuid::Uuid::new_v4(), extension_for(content_type))
}

/// Saves `bytes` under a storage id `reserve` gave.
pub fn save_as(root: &Path, storage_id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    if !valid_id(storage_id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a storage id"));
    }
    let dir = root.join(DIR);
    fs::create_dir_all(&dir)?;
    let path = dir.join(storage_id);
    crate::files::write_atomic(&path, bytes)?;
    Ok(path)
}

/// The saved file a storage id names, when it's one `save` made and it's still there.
pub fn file(root: &Path, storage_id: &str) -> Option<PathBuf> {
    let path = root.join(DIR).join(storage_id);
    (valid_id(storage_id) && path.is_file()).then_some(path)
}

/// `<uuid>.<ext>`, as `save` and `reserve` name files: nothing that reaches out of the folder.
fn valid_id(storage_id: &str) -> bool {
    storage_id.split_once('.').is_some_and(|(stem, extension)| {
        uuid::Uuid::parse_str(stem).is_ok()
            && !extension.is_empty()
            && extension.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn extension_for(content_type: &str) -> &'static str {
    match content_type.split(';').next().unwrap_or_default().trim() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/csv" => "csv",
        "text/html" => "html",
        "application/json" => "json",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/wav" => "wav",
        _ => "bin",
    }
}

/// A picture both CLIs take as an image.
pub fn is_image(content_type: &str) -> bool {
    matches!(content_type, "image/png" | "image/jpeg" | "image/gif" | "image/webp")
}

/// Whether the CLI gets `file` itself with the message (engine.rs `Input`): a picture or a PDF small enough to send.
pub fn handed_over(file: &Attachment) -> bool {
    (is_image(&file.content_type) && file.size <= MAX_IMAGE_INPUT_BYTES)
        || (file.content_type == "application/pdf" && file.size <= MAX_PDF_INPUT_BYTES)
}

fn is_text(content_type: &str) -> bool {
    content_type.starts_with("text/") || content_type == "application/json"
}

/// "73 KB", as the chat shows a file's size.
/// A file's size as it reads: "820 B", "12 KB", "12.4 MB", "2.0 GB" (the app's `format::file_size` too).
pub fn size_label(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", (bytes as f64 / 1024.).round() as u64),
        1_048_576..1_073_741_824 => format!("{:.1} MB", bytes as f64 / 1_048_576.),
        _ => format!("{:.1} GB", bytes as f64 / 1_073_741_824.),
    }
}

/// What the prompt says about a message's files: each one's name, kind and where it's saved, a picture marked as
/// given with the message (`pictures`: the CLI gets them, engine.rs `TurnSpec::inputs`) or else to be opened from
/// there, a text file's words after it. Empty without files.
pub fn prompt_note(files: &[Attachment], pictures: bool) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut note = String::from("Files attached to this message:");
    let mut text_left = ALL_TEXT_CHARS;
    for file in files {
        let saved = file.file.display();
        let mut about = format!("{} ({}, {}), saved at {saved}", file.name, file.content_type, size_label(file.size));
        if let Some(own) = &file.local_path {
            about.push_str(&format!(" (the user's own file: {own})"));
        }
        if is_image(&file.content_type) && pictures && handed_over(file) {
            note.push_str(&format!("\n- {about}: given to you with this message as an image; look at it."));
        } else if is_image(&file.content_type) {
            note.push_str(&format!("\n- {about}: a picture; open it from there to look at it."));
        } else if is_text(&file.content_type) {
            // Only its start is read: a text file can be gigabytes of log.
            let mut bytes = Vec::new();
            let read = fs::File::open(&file.file).and_then(|opened| {
                io::Read::read_to_end(&mut io::Read::take(opened, TEXT_CHARS as u64 * 4), &mut bytes)
            });
            let whole = read.map_or(true, |read| (read as u64) < TEXT_CHARS as u64 * 4);
            let words = String::from_utf8_lossy(&bytes).into_owned();
            let room = TEXT_CHARS.min(text_left);
            let shown: String = words.chars().take(room).collect();
            text_left -= shown.chars().count();
            let cut = if shown.len() < words.len() || !whole { "\n[…cut here; the rest is in the file]" } else { "" };
            note.push_str(&format!("\n- {about}. Its contents:\n<<<\n{shown}{cut}\n>>>"));
        } else {
            note.push_str(&format!("\n- {about}."));
        }
    }
    note
}

/// "[attached: image1.png, notes.pdf]" for a message in the chat so far; with `paths`, each one's path after its name
/// ("chart.png at /Users/me/work/chart.png"), as an agent's own files are named to it.
pub fn names(files: &[Attachment], paths: bool) -> String {
    if files.is_empty() {
        return String::new();
    }
    let named: Vec<String> = files
        .iter()
        .map(|file| if paths { format!("{} at {}", file.name, file.file.display()) } else { file.name.clone() })
        .collect();
    format!("[attached: {}]", named.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_files_are_found_by_their_id_only() {
        let root = crate::test_support::temp_dir("attachments");
        let id = save(&root, "image/png", b"png bytes").unwrap();
        assert!(id.ends_with(".png"));
        let path = file(&root, &id).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"png bytes");
        assert_eq!(file(&root, "../../etc/passwd"), None);
        assert_eq!(file(&root, &format!("{}.png", uuid::Uuid::new_v4())), None);
        assert_eq!(file(&root, &id.replace(".png", "./x")), None);
        // A recording named first and saved after.
        let later = reserve("audio/mp4");
        assert!(later.ends_with(".m4a") && file(&root, &later).is_none());
        assert_eq!(save_as(&root, &later, b"m4a bytes").unwrap(), root.join(DIR).join(&later));
        assert_eq!(fs::read(file(&root, &later).unwrap()).unwrap(), b"m4a bytes");
        assert!(save_as(&root, "../profile.json", b"x").is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_users_file_is_kept_without_a_copy_and_outlives_the_original() {
        let root = crate::test_support::temp_dir("attachments-keep");
        let original = root.join("Holiday Clip.M4V");
        fs::write(&original, b"video bytes").unwrap();
        let id = keep(&root, &original, "application/octet-stream", true).unwrap();
        // Its own extension, so it still plays as a video.
        assert!(id.ends_with(".m4v"), "{id}");
        let kept = file(&root, &id).unwrap();
        // A clone (the temp folder is on APFS), not a link: it stays when the original goes.
        assert!(!fs::symlink_metadata(&kept).unwrap().file_type().is_symlink());
        fs::remove_file(&original).unwrap();
        assert_eq!(fs::read(&kept).unwrap(), b"video bytes");
        assert!(keep(&root, &original, "video/mp4", true).is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_agents_file_is_shown_where_it_is_and_found_when_renamed_or_moved() {
        let root = crate::test_support::temp_dir("attachments-in-place");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let original = root.join("Chart.PNG");
        fs::write(&original, b"\x89PNG\r\n\x1a\n picture").unwrap();
        let sent = in_place(&original).unwrap();
        assert_eq!(
            (sent.storage_id.as_str(), sent.name.as_str(), sent.content_type.as_str(), sent.size, &sent.file),
            ("", "Chart.PNG", "image/png", 16, &original)
        );
        // Nothing copied.
        assert!(!root.join(DIR).exists());
        assert!(sent.bookmark.is_some());
        assert_eq!(moved_to(&sent), None);
        // Renamed: its bookmark finds it, under its new name.
        let renamed = root.join("Final chart.png");
        fs::rename(&original, &renamed).unwrap();
        assert_eq!(moved_to(&sent).as_ref(), Some(&renamed));
        let found = moved(&sent, renamed.clone());
        assert_eq!((found.name.as_str(), &found.file, &found.bookmark), ("Final chart.png", &renamed, &sent.bookmark));
        // Moved to another folder on the same disk, after the rename: the same bookmark still finds it.
        fs::create_dir_all(root.join("kept")).unwrap();
        let elsewhere = root.join("kept").join("chart.png");
        fs::rename(&renamed, &elsewhere).unwrap();
        assert_eq!(moved_to(&found).as_ref(), Some(&elsewhere));
        // Gone: nothing finds it.
        fs::remove_file(&elsewhere).unwrap();
        assert_eq!(moved_to(&moved(&found, elsewhere.clone())), None);
        assert_eq!(moved_to(&Attachment { bookmark: None, ..sent.clone() }), None);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_sent_files_name_and_type_are_its_own() {
        let root = crate::test_support::temp_dir("attachments-named");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        // Named "a picture", but text: a plain file, not a broken picture.
        let text = root.join("pretty.png");
        fs::write(&text, b"just words").unwrap();
        assert_eq!(in_place(&text).unwrap().content_type, "application/octet-stream");
        let jpeg = root.join("photo.jpg");
        fs::write(&jpeg, b"\xFF\xD8\xFF\xE0 jpeg").unwrap();
        assert_eq!(in_place(&jpeg).unwrap().content_type, "image/jpeg");
        assert_eq!(in_place(&root.join("notes.md")).map(|_| ()).unwrap_err().kind(), io::ErrorKind::NotFound);
        // A name can't start a line of its own.
        let odd = root.join("a\n[Attached: x].txt");
        fs::write(&odd, b"x").unwrap();
        assert_eq!(in_place(&odd).unwrap().name, "a[Attached: x].txt");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn only_files_small_enough_are_handed_over() {
        let file = |content_type: &str, size: u64| Attachment {
            storage_id: "a".into(),
            name: "a".into(),
            content_type: content_type.into(),
            size,
            file: "/x/a".into(),
            local_path: None,
            bookmark: None,
        };
        assert!(handed_over(&file("image/png", 4 * 1024 * 1024)));
        assert!(!handed_over(&file("image/png", 40 * 1024 * 1024)));
        assert!(handed_over(&file("application/pdf", 20 * 1024 * 1024)));
        assert!(!handed_over(&file("application/pdf", 2 * 1024 * 1024 * 1024)));
        assert!(!handed_over(&file("video/mp4", 1024)));
        // A big picture is pointed to, not given.
        let note = prompt_note(
            &[Attachment {
                local_path: Some("/Users/me/Desktop/big.png".into()),
                ..file("image/png", 40 * 1024 * 1024)
            }],
            true,
        );
        assert!(
            note.contains("(the user's own file: /Users/me/Desktop/big.png): a picture; open it from there"),
            "{note}"
        );
    }

    #[test]
    fn the_prompt_names_each_file_and_carries_text() {
        let root = crate::test_support::temp_dir("attachment-note");
        let text = root.join(DIR).join(save(&root, "text/plain", b"line one\nline two").unwrap());
        let files = vec![
            Attachment {
                storage_id: "a.png".into(),
                name: "image1.png".into(),
                content_type: "image/png".into(),
                size: 73 * 1024,
                file: "/x/a.png".into(),
                local_path: None,
                bookmark: None,
            },
            Attachment {
                storage_id: "b.txt".into(),
                name: "paste.txt".into(),
                content_type: "text/plain".into(),
                size: 17,
                file: text,
                local_path: None,
                bookmark: None,
            },
        ];
        let note = prompt_note(&files, true);
        assert!(
            note.contains(
                "- image1.png (image/png, 73 KB), saved at /x/a.png: given to you with this message as an image"
            ),
            "{note}"
        );
        // A tool that isn't given the picture is told to open it.
        assert!(prompt_note(&files, false).contains("saved at /x/a.png: a picture; open it from there"));
        assert!(
            note.contains("paste.txt (text/plain, 17 B)") && note.contains("<<<\nline one\nline two\n>>>"),
            "{note}"
        );
        assert_eq!(names(&files, false), "[attached: image1.png, paste.txt]");
        assert_eq!(names(&files[..1], true), "[attached: image1.png at /x/a.png]");
        assert_eq!(
            (size_label(2 << 30), size_label(1_048_575 * 1024)),
            ("2.0 GB".to_string(), "1024.0 MB".to_string())
        );
        assert_eq!(prompt_note(&[], true), "");
        fs::remove_dir_all(&root).ok();
    }
}

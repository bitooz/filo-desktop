// Attachment rules and the upload flow: at most 5 files, of any size; images the model
// can't take as they are get resized (long side ≤ 2000) and re-encoded as JPEG
// at 0.8, then 0.6, and must end up ≤ 4 MB. Upload = a copy saved on this Mac
// (filo-local attachments.rs), its storage id what the message carries: for a picture
// made fit, its bytes; for any other file, the file itself, kept without reading it
// (an APFS clone, else a link to it), so a 2 GB video attaches at once.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::backend::Backend;

pub const MAX_CHAT_ATTACHMENTS: usize = 5;
/// The most Filo reads into memory for a copy it makes: a picture made fit for the model, a pasted text. A larger
/// picture goes as it is (the model is told where it is).
pub const MAX_COPY_BYTES: usize = 25 * 1024 * 1024;
const MAX_MODEL_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MODEL_IMAGE_MAX_SIDE: u32 = 2000;
const MODEL_IMAGE_QUALITIES: [u8; 2] = [80, 60];

/// The longest message sendMessage takes.
pub const MAX_MESSAGE_CHARS: usize = 4000;
/// Pasted text this long, or this many lines, goes as a text file (`paste.txt`) like a pasted picture, and so does a
/// paste that would push the message past MAX_MESSAGE_CHARS.
const PASTE_AS_FILE_MIN_CHARS: usize = 2000;
const PASTE_AS_FILE_MIN_LINES: usize = 40;

pub const TOO_MANY: &str = "Up to 5 files per message.";
pub const IMAGE_TOO_LARGE: &str = "Image is too large for Filo.";
pub const UPLOAD_FAILED: &str = "The attachment could not be uploaded. Check your connection and try again.";

#[derive(Clone, Debug)]
pub struct PendingAttachment {
    pub local_id: u64,
    pub name: String,
    pub content_type: String,
    pub size: usize,
    /// Local file shown as the preview for images.
    pub preview: Option<PathBuf>,
    /// Set once the upload finished.
    pub storage_id: Option<String>,
    /// Where the user keeps the file (one they picked, dropped or pasted from Finder): sent as `localPath`, so Filo
    /// and a coding tool on this Mac can open their own file, not only the upload. None for copies Filo made.
    pub local_path: Option<PathBuf>,
}

impl PendingAttachment {
    /// The `SentAttachment` shape `voiceSessions:sendMessage` takes.
    pub fn sent(&self) -> Option<Value> {
        let storage_id = self.storage_id.as_ref()?;
        let mut sent = json!({
            "storageId": storage_id,
            "name": self.name,
            "contentType": self.content_type,
            "size": self.size as f64,
        });
        if let Some(path) = self.local_path.as_ref().and_then(|path| path.to_str()) {
            sent["localPath"] = json!(path);
        }
        Some(sent)
    }

    /// How it shows in the bubble of a message still being sent: like the sent message's own (its picture comes from
    /// the file here). An empty storage id while it uploads.
    pub fn shown(&self) -> super::types::Attachment {
        super::types::Attachment {
            storage_id: self.storage_id.clone().unwrap_or_default(),
            name: self.name.clone(),
            content_type: self.content_type.clone(),
            size: self.size as f64,
            local_path: self.local_path.as_ref().map(|path| path.to_string_lossy().into_owned()),
            local_file: self.preview.clone().filter(|file| decodes_here(file)),
        }
    }
}

/// A picture the chat can draw from its own file (`pictures.rs` decodes it), told by the file itself: a HEIC goes up
/// as a JPEG (its `content_type` then says JPEG) and shows from that.
fn decodes_here(file: &Path) -> bool {
    let content_type = content_type_for(file);
    content_type.starts_with("image/") && !matches!(content_type.as_str(), "image/heic" | "image/heif")
}

/// Whether `path` is a file that stays, not a temporary one: a marked-up copy (saved under the temp folder) or the
/// screenshot thumbnail macOS lets you drag, which disappear soon after. A pasted picture is saved where it stays
/// (pasteboard.rs), so it counts.
pub fn users_file(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    let canonical_temp = temp.canonicalize().unwrap_or_else(|_| temp.clone());
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.is_absolute() && !path.starts_with(&temp) && !canonical.starts_with(&canonical_temp)
}

/// A file's chip in the composer (the vendored input's atoms): its text, the room at its start for its picture, and
/// where its muted size begins.
#[derive(Debug, PartialEq)]
pub struct ChipText {
    pub text: String,
    pub image_len: usize,
    pub muted_from: usize,
}

/// Room for a picture: two figure spaces, about a line's height across. No-break spaces inside, so a chip wraps as
/// one word.
const PICTURE_ROOM: &str = "\u{2007}\u{2007}";
const EDGE: &str = "\u{202F}";
const MAX_CHIP_NAME_CHARS: usize = 28;
/// Characters a shortened name keeps from its end (the extension and what's just before it).
const CHIP_NAME_TAIL_CHARS: usize = 12;

/// `[picture] name 40 KB` for a file in the composer; a long name loses its middle.
pub fn chip_text(name: &str, size: usize, picture: bool) -> ChipText {
    let chars: Vec<char> = name.chars().collect();
    let name = if chars.len() > MAX_CHIP_NAME_CHARS {
        let head: String = chars[..MAX_CHIP_NAME_CHARS - CHIP_NAME_TAIL_CHARS - 1].iter().collect();
        let tail: String = chars[chars.len() - CHIP_NAME_TAIL_CHARS..].iter().collect();
        format!("{head}…{tail}")
    } else {
        name.to_string()
    };
    let lead = if picture { format!("{PICTURE_ROOM}{EDGE}") } else { EDGE.to_string() };
    let before_size = format!("{lead}{name}\u{00A0}");
    let text = format!("{before_size}{}{EDGE}", crate::format::file_size(size as f64));
    ChipText { text, image_len: if picture { PICTURE_ROOM.len() } else { 0 }, muted_from: before_size.len() }
}

/// Files whose chips this Mac put in a composer, by the chip's text. Chip text copied out of a message being written
/// and pasted into another (another chat, a fresh window), or brought back by undo, is only text: without this it went
/// as words ("image1.png 73 KB") and the picture stayed behind. With it the text becomes that file's chip again
/// (conversation.rs `restore_chips`).
#[derive(Default)]
struct ChipFiles(std::collections::HashMap<String, PathBuf>);

impl gpui::Global for ChipFiles {}

/// Remembers the file a chip with `text` stands for.
pub fn remember_chip(text: &str, file: &Path, cx: &mut gpui::App) {
    cx.default_global::<ChipFiles>().0.insert(text.to_string(), file.to_path_buf());
}

/// The file chip text `text` stood for, while it's still there.
pub fn chip_file(text: &str, cx: &gpui::App) -> Option<PathBuf> {
    cx.try_global::<ChipFiles>()?.0.get(text).filter(|file| file.is_file()).cloned()
}

/// Chip text in `text` that isn't a chip (outside `atoms`): where each is and the name it shows. Told by the marks only
/// a chip has: the narrow no-break spaces at its edges, a no-break space before its size.
pub fn loose_chips(text: &str, atoms: &[Range<usize>]) -> Vec<(Range<usize>, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(EDGE).map(|at| from + at) {
        from = at + EDGE.len();
        if atoms.iter().any(|atom| atom.start <= at && at < atom.end) {
            continue;
        }
        let Some((name, end)) = chip_after_edge(&text[from..]) else { continue };
        let start = if text[..at].ends_with(PICTURE_ROOM) { at - PICTURE_ROOM.len() } else { at };
        if atoms.iter().any(|atom| atom.start < from + end && start < atom.end) {
            continue;
        }
        found.push((start..from + end, name));
        from += end;
    }
    found
}

/// `name\u{a0}73 KB\u{202f}` at the start of `rest`: the name and the length up to the closing edge's end.
fn chip_after_edge(rest: &str) -> Option<(String, usize)> {
    let (name, after) = rest.split_once('\u{00A0}')?;
    if name.is_empty() || name.chars().count() > MAX_CHIP_NAME_CHARS || name.contains(EDGE) || name.contains('\n') {
        return None;
    }
    let (size, _) = after.split_once(EDGE)?;
    let (number, unit) = size.split_once(' ')?;
    let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !number_ok || !matches!(unit, "B" | "KB" | "MB" | "GB") {
        return None;
    }
    Some((name.to_string(), name.len() + '\u{00A0}'.len_utf8() + size.len() + EDGE.len()))
}

/// The composer's text as it is sent: each chip replaced by its file's path (or its name, when it has none).
pub fn with_paths(text: &str, chips: &[(Range<usize>, String)]) -> String {
    let mut chips: Vec<_> = chips
        .iter()
        .filter(|(range, _)| {
            range.end <= text.len() && text.is_char_boundary(range.start) && text.is_char_boundary(range.end)
        })
        .collect();
    chips.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_string();
    for (range, replacement) in chips {
        out.replace_range(range.clone(), replacement);
    }
    out
}

/// A small round-croppable picture of an image file for its chip, or None when it can't be read. Told by its bytes,
/// not its name: a JPEG saved as `.png` (as some sites name their downloads) still shows.
pub fn thumbnail(path: &Path) -> Option<Arc<gpui::RenderImage>> {
    let image = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let mut pixels = image.resize_to_fill(48, 48, image::imageops::FilterType::Triangle).to_rgba8();
    // gpui draws BGRA.
    pixels.chunks_exact_mut(4).for_each(|pixel| pixel.swap(0, 2));
    Some(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(pixels)])))
}

/// Whether text pasted into a message of `draft_chars` characters goes as a file.
pub fn pastes_as_file(pasted: &str, draft_chars: usize) -> bool {
    if pasted.trim().is_empty() {
        return false;
    }
    let chars = pasted.chars().count();
    chars >= PASTE_AS_FILE_MIN_CHARS
        || pasted.matches('\n').count() + 1 >= PASTE_AS_FILE_MIN_LINES
        || draft_chars + chars > MAX_MESSAGE_CHARS
}

/// What a file pasted into the composer is called, after those `taken`: a picture `image1.png`, then `image2.png` and
/// on; text (`pasted-text-…`) `paste.txt`, then `paste-2.txt`.
pub fn pasted_name(path: &Path, taken: &[&str]) -> String {
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("png").to_lowercase();
    let text = path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("pasted-text-"));
    (1..)
        .map(|n| match (text, n) {
            (true, 1) => "paste.txt".to_string(),
            (true, n) => format!("paste-{n}.txt"),
            (false, n) => format!("image{n}.{extension}"),
        })
        .find(|name| !taken.contains(&name.as_str()))
        .expect("a free name")
}

/// A sent message's text as it shows: without the paths of its files, which the composer wrote where their chips were
/// (`sent_text`) for Filo to hand on. The files show on their own (pictures above the bubble, other files in it), so a
/// chip in the text again showed each twice. A line that held only files goes with them.
pub fn without_files(text: &str, files: &[super::types::Attachment]) -> String {
    let paths: Vec<(usize, String)> = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| file.local_path.clone().map(|path| (index, path)))
        .collect();
    if paths.is_empty() {
        return text.to_string();
    }
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let pieces = split_files(line, &paths);
        if !pieces.iter().any(|piece| matches!(piece, Piece::File(_))) {
            lines.push(line.to_string());
            continue;
        }
        let mut shown = String::new();
        for piece in pieces {
            let Piece::Text(words) = piece else { continue };
            // One space where a file was between words, none where it began the line.
            if shown.is_empty() || shown.ends_with(' ') {
                shown.push_str(words.trim_start_matches(' '));
            } else {
                shown.push_str(&words);
            }
        }
        let shown = shown.trim_end();
        if !shown.is_empty() {
            lines.push(shown.to_string());
        }
    }
    lines.join("\n").trim().to_string()
}

/// A picture's or video's chip in a sent message's text (`with_chips`): its bytes there, the attachment it stands for,
/// and its `ChipText` marks (the room for its picture, where its muted size begins).
#[derive(Debug, PartialEq)]
pub struct SentChip {
    pub range: Range<usize>,
    pub file: usize,
    pub image_len: usize,
    pub muted_from: usize,
}

/// A sent message's text as its bubble shows it: each picture's or video's path, which the composer wrote where its
/// chip was, is that chip again (`chip_text`), with a space either side; other files' paths go, as in
/// `without_files` (they show as rows in the bubble). Its pictures still show above the bubble too. A message of
/// files alone has no text, so no chips.
pub fn with_chips(text: &str, files: &[super::types::Attachment]) -> (String, Vec<SentChip>) {
    let shown = without_files(text, files);
    let paths: Vec<(usize, String)> = files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| file.local_path.clone().map(|path| (index, path)))
        .collect();
    let is_media = |index: usize| {
        files
            .get(index)
            .is_some_and(|file| file.content_type.starts_with("image/") || file.content_type.starts_with("video/"))
    };
    if shown.trim().is_empty() || !paths.iter().any(|(index, _)| is_media(*index)) {
        return (shown, Vec::new());
    }
    // Only spaces and line breaks are trimmed: a chip's own room and edges are spaces too (`chip_text`).
    let blank = |c: char| c == ' ' || c == '\n' || c == '\t';
    let mut out = String::new();
    let mut chips = Vec::new();
    for line in text.split('\n') {
        let pieces = split_files(line, &paths);
        if !pieces.iter().any(|piece| matches!(piece, Piece::File(_))) {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let mut shown = String::new();
        let mut line_chips = Vec::new();
        for piece in pieces {
            match piece {
                // One space where a file was between words, none where it began the line.
                Piece::Text(words) if shown.is_empty() || shown.ends_with(' ') => {
                    shown.push_str(words.trim_start_matches(' '))
                }
                Piece::Text(words) => shown.push_str(&words),
                Piece::File(index) if is_media(index) => {
                    if !shown.is_empty() && !shown.ends_with(' ') {
                        shown.push(' ');
                    }
                    let file = &files[index];
                    let chip = chip_text(&file.name, file.size as usize, file.content_type.starts_with("image/"));
                    let at = out.len() + shown.len();
                    line_chips.push(SentChip {
                        range: at..at + chip.text.len(),
                        file: index,
                        image_len: chip.image_len,
                        muted_from: chip.muted_from,
                    });
                    shown.push_str(&chip.text);
                    shown.push(' ');
                }
                Piece::File(_) => {}
            }
        }
        // A line that held only other files goes with them.
        let shown = shown.trim_end_matches(' ');
        if !shown.is_empty() {
            out.push_str(shown);
            out.push('\n');
            chips.extend(line_chips);
        }
    }
    let lead = out.len() - out.trim_start_matches(blank).len();
    let out = out.trim_matches(blank).to_string();
    let chips = chips
        .into_iter()
        .map(|chip| SentChip { range: chip.range.start - lead..chip.range.end - lead, ..chip })
        .collect();
    (out, chips)
}

/// Pictures for the chips in sent messages' text (`thumbnail`), by file: None while one loads or when it can't be read.
#[derive(Default)]
struct SentChipPictures(std::collections::HashMap<PathBuf, Option<Arc<gpui::RenderImage>>>);

impl gpui::Global for SentChipPictures {}

/// The picture for a sent message's chip of `file`, made once in the background (the windows draw again when it's
/// ready).
pub fn sent_chip_picture(file: &Path, cx: &mut gpui::App) -> Option<Arc<gpui::RenderImage>> {
    if let Some(picture) = cx.default_global::<SentChipPictures>().0.get(file) {
        return picture.clone();
    }
    cx.default_global::<SentChipPictures>().0.insert(file.to_path_buf(), None);
    let path = file.to_path_buf();
    let made = cx.background_executor().spawn({
        let path = path.clone();
        async move { thumbnail(&path) }
    });
    cx.spawn(async move |cx| {
        let Some(picture) = made.await else { return };
        cx.update(|cx| {
            cx.default_global::<SentChipPictures>().0.insert(path, Some(picture));
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
    None
}

/// A piece of a sent message's text: words, or one of its files (an index into its attachments).
#[derive(Debug, PartialEq)]
enum Piece {
    Text(String),
    File(usize),
}

/// A sent message's text split around the paths of its files, written where their chips were (`mention`). `paths`
/// pairs each file's index with its path.
fn split_files(text: &str, paths: &[(usize, String)]) -> Vec<Piece> {
    // The quoted form first: it holds the bare one.
    let forms: Vec<(usize, String)> = paths
        .iter()
        .flat_map(|(index, path)| {
            let quoted = mention(Path::new(path));
            let bare = (quoted != *path).then(|| (*index, path.clone()));
            std::iter::once((*index, quoted)).chain(bare)
        })
        .filter(|(_, form)| !form.is_empty())
        .collect();
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some((at, index, len)) = forms
        .iter()
        .filter_map(|(index, form)| rest.find(form.as_str()).map(|at| (at, *index, form.len())))
        .min_by_key(|(at, _, len)| (*at, std::cmp::Reverse(*len)))
    {
        if at > 0 {
            pieces.push(Piece::Text(rest[..at].to_string()));
        }
        pieces.push(Piece::File(index));
        rest = &rest[at + len..];
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest.to_string()));
    }
    pieces
}

/// How a file reads in the composer's text: its path, in quotes when it has a space, so where one ends is clear.
pub fn mention(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.chars().any(char::is_whitespace) { format!("\"{text}\"") } else { text.into_owned() }
}

pub fn content_type_for(path: &Path) -> String {
    filo_local::attachments::content_type_for(path).to_string()
}

/// `imagePassesThrough`: a supported type, small enough, and no side over MODEL_IMAGE_MAX_SIDE (Claude's cap once a
/// request holds over 20 pictures).
fn image_passes_through(content_type: &str, bytes: &[u8]) -> bool {
    matches!(content_type, "image/jpeg" | "image/png" | "image/webp" | "image/gif")
        && bytes.len() <= MAX_MODEL_IMAGE_BYTES
        && image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()
            .and_then(|reader| reader.into_dimensions().ok())
            .is_some_and(|(width, height)| width.max(height) <= MODEL_IMAGE_MAX_SIDE)
}

/// `reencodedImageName`: `photo.heic` → `photo.jpg`
fn reencoded_name(name: &str) -> String {
    let base = Path::new(name).file_stem().and_then(|stem| stem.to_str()).unwrap_or("image");
    format!("{base}.jpg")
}

fn reencode(bytes: &[u8]) -> Result<Vec<u8>> {
    let image = image::load_from_memory(bytes).map_err(|_| anyhow!(IMAGE_TOO_LARGE))?;
    let (width, height) = (image.width(), image.height());
    let long_side = width.max(height);
    let image = if long_side > MODEL_IMAGE_MAX_SIDE {
        let scale = MODEL_IMAGE_MAX_SIDE as f64 / long_side as f64;
        let (w, h) = (((width as f64) * scale).round() as u32, ((height as f64) * scale).round() as u32);
        image.resize_exact(w.max(1), h.max(1), image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let rgb = image.to_rgb8();
    for quality in MODEL_IMAGE_QUALITIES {
        let mut out = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        rgb.write_with_encoder(encoder)?;
        if out.len() <= MAX_MODEL_IMAGE_BYTES {
            return Ok(out);
        }
    }
    bail!(IMAGE_TOO_LARGE)
}

/// A file read from disk and made ready for upload: (name, content type, bytes). `name` is what it's called in the
/// chat, the file's own name unless it was pasted (`image1.png`).
pub fn prepare(path: &Path, name: &str) -> Result<(String, String, Vec<u8>)> {
    let name = name.to_string();
    let content_type = content_type_for(path);
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        bail!(UPLOAD_FAILED);
    }
    if bytes.len() > MAX_COPY_BYTES {
        bail!(IMAGE_TOO_LARGE);
    }
    if content_type.starts_with("image/") && !image_passes_through(&content_type, &bytes) {
        let encoded = reencode(&bytes)?;
        return Ok((reencoded_name(&name), "image/jpeg".into(), encoded));
    }
    Ok((name, content_type, bytes))
}

/// What goes up for a file the composer attaches: a copy Filo made (a picture made fit for the model), or the file
/// itself, of any size, never read (`keep`).
pub enum Prepared {
    Copy(Vec<u8>),
    File(PathBuf),
}

/// A file the composer attaches made ready for upload: (name, content type, size, what goes up). A picture Filo can
/// make fit for the model is read and maybe re-encoded (`prepare`); any other file, or a picture too large to, goes as
/// it is.
pub fn prepare_attachment(path: &Path, name: &str) -> Result<(String, String, usize, Prepared)> {
    let content_type = content_type_for(path);
    let size = std::fs::metadata(path)?.len() as usize;
    if size == 0 {
        bail!(UPLOAD_FAILED);
    }
    if content_type.starts_with("image/") && size <= MAX_COPY_BYTES {
        let (name, content_type, bytes) = prepare(path, name)?;
        return Ok((name, content_type, bytes.len(), Prepared::Copy(bytes)));
    }
    Ok((name.to_string(), content_type, size, Prepared::File(path.to_path_buf())))
}

/// Saves the file for the message on this Mac; its storage id. `name` stays the composer's: the message says it.
pub async fn upload(backend: Backend, _name: String, content_type: String, bytes: Vec<u8>) -> Result<String> {
    backend.save_attachment(content_type, bytes).await
}

/// Uploads what `prepare_attachment` made ready; its storage id. A file of the user's (`users_file`) that can't be
/// cloned is linked to, a temporary one copied.
pub async fn upload_prepared(
    backend: Backend,
    name: String,
    content_type: String,
    prepared: Prepared,
) -> Result<String> {
    match prepared {
        Prepared::Copy(bytes) => upload(backend, name, content_type, bytes).await,
        Prepared::File(path) => {
            let link = users_file(&path);
            backend.keep_attachment(content_type, path, link).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(range: Range<usize>) -> gpui_component::input::InputAtom {
        gpui_component::input::InputAtom {
            id: 1,
            range,
            image: None,
            image_len: 0,
            muted_from: 0,
            background: gpui::Hsla::default(),
            border: gpui::Hsla::default(),
            muted: gpui::Hsla::default(),
        }
    }

    // The composer's input (vendor/gpui-component/src/input/atom.rs) edits a chip as one unit.
    #[test]
    fn an_edit_takes_a_chip_whole() {
        use gpui_component::input::expand_to_atoms;
        let atoms = vec![atom(5..10)];
        // Backspace at its end deletes its last byte: all of it goes.
        assert_eq!(expand_to_atoms(&atoms, 9..10), 5..10);
        // A selection reaching into it.
        assert_eq!(expand_to_atoms(&atoms, 2..7), 2..10);
        // Typing right before or after it leaves it be.
        assert_eq!(expand_to_atoms(&atoms, 5..5), 5..5);
        assert_eq!(expand_to_atoms(&atoms, 10..10), 10..10);
        assert_eq!(expand_to_atoms(&atoms, 0..5), 0..5);
    }

    #[test]
    fn chips_move_with_edits_before_them_and_go_with_their_text() {
        use gpui_component::input::atoms_after_edit;
        let mut atoms = vec![atom(5..10), atom(12..15)];
        let ranges =
            |atoms: &[gpui_component::input::InputAtom]| atoms.iter().map(|a| a.range.clone()).collect::<Vec<_>>();
        atoms_after_edit(&mut atoms, &(0..0), 3);
        assert_eq!(ranges(&atoms), vec![8..13, 15..18]);
        atoms_after_edit(&mut atoms, &(8..13), 0);
        assert_eq!(ranges(&atoms), vec![10..13]);
        // Typing right after it.
        atoms_after_edit(&mut atoms, &(13..13), 1);
        assert_eq!(ranges(&atoms), vec![10..13]);
    }

    #[test]
    fn the_cursor_never_stops_inside_a_chip() {
        use gpui_component::input::snap_out_of_atoms;
        let atoms = vec![atom(5..10)];
        assert_eq!(snap_out_of_atoms(&atoms, 7, 8), 10);
        assert_eq!(snap_out_of_atoms(&atoms, 7, 6), 5);
        assert_eq!(snap_out_of_atoms(&atoms, 10, 11), 10);
    }

    #[test]
    fn pictures_over_2000_px_are_resized_even_when_small() {
        let png = |width, height| {
            let mut out = std::io::Cursor::new(Vec::new());
            image::RgbImage::new(width, height).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        // A pasted 3104 px screenshot: well under 4 MB, but more pixels than Claude takes in a long chat.
        let big = png(3104, 1964);
        assert!(big.len() < MAX_MODEL_IMAGE_BYTES);
        assert!(!image_passes_through("image/png", &big));
        assert_eq!(image::load_from_memory(&reencode(&big).unwrap()).unwrap().width(), 2000);
        assert!(image_passes_through("image/png", &png(2000, 1200)));
        assert!(!image_passes_through("image/heic", &png(20, 20)));
    }

    #[test]
    fn long_or_many_line_pastes_go_as_a_file() {
        assert!(!pastes_as_file(&"x".repeat(1999), 0));
        assert!(pastes_as_file(&"x".repeat(2000), 0));
        assert!(pastes_as_file(&"line\n".repeat(39), 0));
        assert!(!pastes_as_file(&"line\n".repeat(38), 0));
        // One that would push the message past its limit, which would cut it off.
        assert!(!pastes_as_file(&"x".repeat(500), MAX_MESSAGE_CHARS - 500));
        assert!(pastes_as_file(&"x".repeat(501), MAX_MESSAGE_CHARS - 500));
        assert!(!pastes_as_file(&" \n".repeat(3000), 0));
    }

    #[test]
    fn pasted_pictures_are_numbered_in_the_message() {
        let path = Path::new("/Users/me/.config/filo/pasted/pasted-image-2026-10-07-170726.png");
        assert_eq!(pasted_name(path, &[]), "image1.png");
        assert_eq!(pasted_name(path, &["image1.png", "notes.pdf"]), "image2.png");
        assert_eq!(pasted_name(Path::new("/x/pasted-image-1.JPG"), &["image2.jpg"]), "image1.jpg");
        let text = Path::new("/Users/me/.config/filo/pasted/pasted-text-2026-10-08-120000.txt");
        assert_eq!(pasted_name(text, &["image1.png"]), "paste.txt");
        assert_eq!(pasted_name(text, &["paste.txt"]), "paste-2.txt");
    }

    #[test]
    fn a_sent_message_shows_its_files_where_their_paths_are() {
        let paths = vec![
            (0, "/Users/me/Desktop/Screen Shot.png".to_string()),
            (2, "/Users/me/.config/filo/pasted/p1.png".to_string()),
        ];
        let text = "look \"/Users/me/Desktop/Screen Shot.png\" and /Users/me/.config/filo/pasted/p1.png ok";
        assert_eq!(
            split_files(text, &paths),
            vec![
                Piece::Text("look ".into()),
                Piece::File(0),
                Piece::Text(" and ".into()),
                Piece::File(2),
                Piece::Text(" ok".into()),
            ]
        );
        // Without its paths, the text is one piece.
        assert_eq!(split_files("just words", &paths), vec![Piece::Text("just words".into())]);
    }

    #[test]
    fn a_sent_message_shows_its_text_without_its_files() {
        let file = |path: Option<&str>| crate::screens::chat::types::Attachment {
            storage_id: "s".into(),
            name: "image1.png".into(),
            content_type: "image/png".into(),
            size: 12.0 * 1024.0,
            local_path: path.map(str::to_string),
            local_file: None,
        };
        let pasted = "/Users/me/.config/filo/local/pasted/pasted-image-2026-10-10-191941.png";
        let files = vec![file(Some(pasted)), file(Some("/Users/me/Desktop/Screen Shot.png"))];
        // A pasted picture's chip before the words, as the composer leaves it.
        assert_eq!(without_files(&format!("{pasted} there is still an issue"), &files), "there is still an issue");
        assert_eq!(without_files(&format!("{pasted}\nthere is still an issue"), &files), "there is still an issue");
        // Between words, one space stays; a line of only files goes.
        assert_eq!(
            without_files(
                &format!("look {pasted} and \"/Users/me/Desktop/Screen Shot.png\" ok\n{pasted} \nbye"),
                &files
            ),
            "look and ok\nbye"
        );
        // Only files: nothing to show in a bubble.
        assert_eq!(without_files(&format!("{pasted} "), &files), "");
        // A message without files, or one whose files have no path, is left as it is.
        assert_eq!(without_files("  just  words\n", &[]), "  just  words\n");
        assert_eq!(without_files("see image1.png", &[file(None)]), "see image1.png");
    }

    #[test]
    fn a_sent_message_shows_its_pictures_as_chips_where_they_were() {
        let file = |name: &str, path: &str, content_type: &str| crate::screens::chat::types::Attachment {
            storage_id: "s".into(),
            name: name.into(),
            content_type: content_type.into(),
            size: 40.0 * 1024.0,
            local_path: Some(path.into()),
            local_file: None,
        };
        let (one, two) =
            ("/Users/me/.config/filo/pasted/pasted-image-1.png", "/Users/me/.config/filo/pasted/pasted-image-2.png");
        let files = vec![
            file("image1.png", one, "image/png"),
            file("image2.png", two, "image/png"),
            file("notes.pdf", "/Users/me/notes.pdf", "application/pdf"),
        ];
        let chip = |name: &str| chip_text(name, 40 * 1024, true).text;
        // The user's message: a chip after "developer centric" and one after "based on", a space before it.
        let (text, chips) = with_chips(&format!("more developer centric {one} based on{two}"), &files);
        assert_eq!(text, format!("more developer centric {} based on {}", chip("image1.png"), chip("image2.png")));
        assert_eq!(
            chips.iter().map(|chip| (&text[chip.range.clone()], chip.file)).collect::<Vec<_>>(),
            [(chip("image1.png").as_str(), 0), (chip("image2.png").as_str(), 1)]
        );
        assert_eq!(
            (chips[0].image_len, chips[0].muted_from),
            (chip_text("image1.png", 40 * 1024, true).image_len, chip_text("image1.png", 40 * 1024, true).muted_from)
        );
        // At the start, its chip leads; another file's path, and a line of only that, go; plain lines stay as they are.
        let (text, chips) =
            with_chips(&format!("{one}\n/Users/me/notes.pdf\nsee /Users/me/notes.pdf this\n  indented"), &files);
        assert_eq!(text, format!("{}\nsee this\n  indented", chip("image1.png")));
        assert_eq!(chips[0].range, 0..chip("image1.png").len());
        // Pictures alone: no text, so no chips (they show above, with no bubble); no pictures: the text as it was.
        assert_eq!(with_chips(&format!("{one} {two}"), &files), (String::new(), Vec::new()));
        assert_eq!(with_chips("just words /Users/me/notes.pdf", &files), ("just words".to_string(), Vec::new()));
    }

    #[test]
    fn a_path_with_a_space_is_quoted() {
        assert_eq!(mention(Path::new("/Users/me/Desktop/Screen Shot.png")), "\"/Users/me/Desktop/Screen Shot.png\"");
        assert_eq!(mention(Path::new("/Users/me/notes.md")), "/Users/me/notes.md");
    }

    #[test]
    fn a_chip_holds_room_for_the_picture_the_name_and_the_size() {
        let chip = chip_text("image.png", 40 * 1024, true);
        assert_eq!(chip.text, "\u{2007}\u{2007}\u{202F}image.png\u{00A0}40 KB\u{202F}");
        assert_eq!(&chip.text[..chip.image_len], "\u{2007}\u{2007}");
        assert_eq!(&chip.text[chip.muted_from..], "40 KB\u{202F}");
        // A long name loses its middle.
        let long = chip_text("a-very-long-file-name-that-goes-on-and-on-forever-v2.pdf", 1_000, false);
        assert!(long.text.contains("a-very-long-fil…rever-v2.pdf"), "{}", long.text);
        assert_eq!(long.image_len, 0);
    }

    #[test]
    fn chip_text_without_its_chip_is_found() {
        let picture = chip_text("image1.png", 73 * 1024, true).text;
        let file = chip_text("notes.pdf", 2 * 1024 * 1024 * 1024, false).text;
        let text = format!("look {picture} and {file} ok");
        let found = loose_chips(&text, &[]);
        let picture_at = 5..5 + picture.len();
        assert_eq!(
            found,
            vec![
                (picture_at.clone(), "image1.png".to_string()),
                (text.find(&file).unwrap()..text.find(&file).unwrap() + file.len(), "notes.pdf".to_string())
            ]
        );
        assert_eq!(&text[found[0].0.clone()], picture);
        // A real chip (an atom) is left alone.
        assert_eq!(loose_chips(&text, &[picture_at]).len(), 1);
        // Words that only look a little like one aren't.
        assert!(loose_chips("image1.png 73 KB", &[]).is_empty());
        assert!(loose_chips("prix\u{202F}: 10\u{00A0}€\u{202F}!", &[]).is_empty());
    }

    #[test]
    fn chips_are_sent_as_their_paths() {
        let chip = chip_text("shot.png", 2048, true).text;
        let text = format!("this image: {chip} and that one: {chip}");
        let first = 12..12 + chip.len();
        let second = text.len() - chip.len()..text.len();
        let sent =
            with_paths(&text, &[(second, "/Users/me/b.png".into()), (first, "\"/Users/me/Screen Shot.png\"".into())]);
        assert_eq!(sent, "this image: \"/Users/me/Screen Shot.png\" and that one: /Users/me/b.png");
    }

    #[test]
    fn temporary_copies_are_not_the_users_files() {
        assert!(users_file(Path::new("/Users/me/Desktop/shot.png")));
        assert!(!users_file(&std::env::temp_dir().join("filo-desktop-files").join("pasted").join("Pasted image.png")));
        assert!(!users_file(Path::new("relative/shot.png")));
    }

    #[test]
    fn sends_where_the_file_is() {
        let attachment = PendingAttachment {
            local_id: 1,
            name: "shot.png".into(),
            content_type: "image/png".into(),
            size: 5,
            preview: None,
            storage_id: Some("m/1".into()),
            local_path: Some(PathBuf::from("/Users/me/shot.png")),
        };
        let sent = attachment.sent().unwrap();
        assert_eq!(
            sent,
            json!({ "storageId": "m/1", "name": "shot.png", "contentType": "image/png", "size": 5.0, "localPath": "/Users/me/shot.png" })
        );
    }

    #[test]
    fn a_sent_heic_is_drawn_from_its_upload_not_its_file() {
        let attachment = PendingAttachment {
            local_id: 1,
            name: "IMG_1.jpg".into(),
            content_type: "image/jpeg".into(),
            size: 5,
            preview: Some(PathBuf::from("/Users/me/IMG_1.HEIC")),
            storage_id: Some("m/1".into()),
            local_path: None,
        };
        assert_eq!(attachment.shown().local_file, None);
        let jpeg = PendingAttachment { preview: Some(PathBuf::from("/Users/me/view.jpg")), ..attachment };
        assert_eq!(jpeg.shown().local_file, Some(PathBuf::from("/Users/me/view.jpg")));
    }
}

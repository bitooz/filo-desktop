// Files next to an artifact's page: a video, a sound or a picture the page names by a relative path (`<video
// src="demo.mp4">`), as it does when it's published with them. The preview's frame has no origin and no network, so
// such a path leads nowhere there; the page is shown with those paths as `filo-media://<artifact id>/<path>` links
// (`link_files`), and the artifact's web view answers them (`response`) from the folders the artifact's files are in
// (`register`: the folder of the HTML file it's published from, then its own folder beside the database), with byte
// ranges, so a video plays, seeks and has its sound. Only media files, only inside those folders, and only for an
// artifact the app has open: nothing else on the Mac can be read this way.

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use gpui_component::wry;
use regex::Regex;

pub const SCHEME: &str = "filo-media";
/// A range answer holds at most this much; WebKit asks for the rest.
const MOST: u64 = 4 << 20;

fn folders() -> &'static Mutex<HashMap<String, Vec<PathBuf>>> {
    static FOLDERS: OnceLock<Mutex<HashMap<String, Vec<PathBuf>>>> = OnceLock::new();
    FOLDERS.get_or_init(Mutex::default)
}

/// The folders artifact `id`'s files may come from, first first.
pub fn register(id: &str, from: Vec<PathBuf>) {
    let from: Vec<PathBuf> = from.into_iter().filter_map(|folder| folder.canonicalize().ok()).collect();
    folders().lock().unwrap().insert(id.to_string(), from);
}

/// What a file is, by its extension: only pictures, videos and sounds are answered.
fn content_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "vtt" => "text/vtt",
        _ => return None,
    })
}

/// A path the page names that's a file next to it: no scheme, not from the root, not an anchor, and not a script
/// building a tag (`'<img src="' + src + '">'`, `` `<img src="${src}">` ``), whose value is code.
fn relative(target: &str) -> bool {
    let target = target.trim();
    if target.contains(['\'', '"', '`', '{', '}', '<', '>']) {
        return false;
    }
    let scheme = target.split_once(':').is_some_and(|(scheme, _)| {
        scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    !target.is_empty() && !scheme && !target.starts_with('/') && !target.starts_with('#') && !target.starts_with('?')
}

/// `document` with the relative `src` and `poster` of its `<video>`, `<audio>`, `<source>`, `<track>` and `<img>`
/// tags as artifact `id`'s `filo-media:` links.
pub fn link_files(document: &str, id: &str) -> String {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static ATTRIBUTE: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| Regex::new(r"(?i)<(?:video|audio|source|track|img)\b[^>]*>").expect("regex"));
    let attribute = ATTRIBUTE.get_or_init(|| {
        Regex::new(r#"(?i)(\s(?:src|poster)\s*=\s*)(?:"([^"]*)"|'([^']*)'|([^\s"'>]+))"#).expect("regex")
    });
    tag.replace_all(document, |tag: &regex::Captures| {
        attribute
            .replace_all(&tag[0], |found: &regex::Captures| {
                let value = found.get(2).or(found.get(3)).or(found.get(4)).map_or("", |value| value.as_str());
                if !relative(value) {
                    return found[0].to_string();
                }
                let path = value.trim().trim_start_matches("./");
                format!("{}\"{SCHEME}://{id}/{}\"", &found[1], path.replace('"', "%22"))
            })
            .into_owned()
    })
    .into_owned()
}

/// The file a `filo-media://<artifact id>/<path>` link names: in one of that artifact's folders, a media file.
fn file_for(url: &str) -> Option<(PathBuf, &'static str)> {
    let parsed = url::Url::parse(url).ok()?;
    let id = parsed.host_str()?;
    let path: PathBuf = parsed.path_segments()?.map(percent_decode).collect::<Option<_>>()?;
    let folders = folders().lock().unwrap().get(id)?.clone();
    folders
        .iter()
        .find_map(|folder| {
            let file = folder.join(&path).canonicalize().ok()?;
            (file.starts_with(folder) && file.is_file()).then_some(file)
        })
        .and_then(|file| content_type(&file).map(|kind| (file, kind)))
}

fn percent_decode(segment: &str) -> Option<String> {
    let decoded =
        url::form_urlencoded::parse(format!("x={}", segment.replace('+', "%2B")).as_bytes()).next()?.1.into_owned();
    (!decoded.is_empty() && decoded != "." && decoded != ".." && !decoded.contains('/')).then_some(decoded)
}

/// `bytes=<start>-<end>` (end included, optional) against a file `size` long: where to start and how much to send.
fn range(header: &str, size: u64) -> Option<(u64, u64)> {
    let (start, end) = header.trim().strip_prefix("bytes=")?.split(',').next()?.split_once('-')?;
    let (start, end) = match (start.trim(), end.trim()) {
        // The last `end` bytes.
        ("", end) => {
            let last = end.parse::<u64>().ok()?.min(size);
            (size - last, size.checked_sub(1)?)
        }
        (start, "") => (start.parse().ok()?, size.checked_sub(1)?),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(size.checked_sub(1)?)),
    };
    (start <= end && start < size).then(|| (start, (end - start + 1).min(MOST)))
}

/// The web view's answer to a `filo-media:` request.
pub fn response(request: &wry::http::Request<Vec<u8>>) -> wry::http::Response<std::borrow::Cow<'static, [u8]>> {
    let answer = |status: u16| wry::http::Response::builder().status(status).header("Access-Control-Allow-Origin", "*");
    let empty = |status: u16| answer(status).body(std::borrow::Cow::Borrowed(&[][..])).expect("response");
    let Some((path, kind)) = file_for(&request.uri().to_string()) else { return empty(404) };
    let Ok(mut file) = std::fs::File::open(&path) else { return empty(404) };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else { return empty(404) };
    let asked = request.headers().get("range").and_then(|value| value.to_str().ok());
    let (status, start, length) = match asked {
        Some(asked) => match range(asked, size) {
            Some((start, length)) => (206, start, length),
            None => {
                return answer(416)
                    .header("Content-Range", format!("bytes */{size}"))
                    .body(std::borrow::Cow::Borrowed(&[][..]))
                    .expect("response");
            }
        },
        None => (200, 0, size),
    };
    let mut bytes = vec![0; length as usize];
    if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut bytes)).is_err() {
        return empty(500);
    }
    let mut response = answer(status)
        .header("Content-Type", kind)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", length.to_string());
    if status == 206 {
        response = response.header("Content-Range", format!("bytes {start}-{}/{size}", start + length - 1));
    }
    response.body(std::borrow::Cow::Owned(bytes)).expect("response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_only_relative_media_paths() {
        let page = r##"<video controls src="demo.mp4" poster='./frames/first.jpg'></video><source src=clip%20one.webm type="video/webm">
<img src="data:image/png;base64,AA"><img alt="x" src="https://example.com/a.png"><a href="demo.mp4">demo</a><img src="#top">"##;
        let linked = link_files(page, "abc");
        assert!(
            linked.contains(
                r#"<video controls src="filo-media://abc/demo.mp4" poster="filo-media://abc/frames/first.jpg">"#
            ),
            "{linked}"
        );
        assert!(linked.contains(r#"<source src="filo-media://abc/clip%20one.webm" type="video/webm">"#), "{linked}");
        assert!(
            linked.contains(r#"src="data:image/png;base64,AA""#)
                && linked.contains(r#"src="https://example.com/a.png""#)
        );
        assert!(linked.contains(r#"<a href="demo.mp4">"#) && linked.contains(r##"<img src="#top">"##));
        // A script building a tag keeps its code: the address it puts in comes at run time.
        let script =
            r#"<script>const a = '<img class="av" src="' + src + '" alt="">', b = `<img src="${it.img}">`;</script>"#;
        assert_eq!(link_files(script, "abc"), script);
    }

    #[test]
    fn answers_media_inside_the_artifact_folders_only() {
        let root = std::env::temp_dir().join(format!("filo-artifact-media-{}", std::process::id()));
        let folder = root.join("pages");
        std::fs::create_dir_all(folder.join("clips")).unwrap();
        std::fs::write(folder.join("clips/demo one.mp4"), b"0123456789").unwrap();
        std::fs::write(folder.join("notes.txt"), b"secret").unwrap();
        std::fs::write(root.join("outside.mp4"), b"outside").unwrap();
        register("art-1", vec![folder.clone()]);
        let get = |url: &str, range: Option<&str>| {
            let mut request = wry::http::Request::builder().uri(url);
            if let Some(range) = range {
                request = request.header("Range", range);
            }
            response(&request.body(Vec::new()).unwrap())
        };
        let whole = get("filo-media://art-1/clips/demo%20one.mp4", None);
        assert_eq!((whole.status().as_u16(), whole.body().as_ref()), (200, &b"0123456789"[..]));
        assert_eq!(whole.headers()["content-type"], "video/mp4");
        let part = get("filo-media://art-1/clips/demo%20one.mp4", Some("bytes=2-5"));
        assert_eq!((part.status().as_u16(), part.body().as_ref()), (206, &b"2345"[..]));
        assert_eq!(part.headers()["content-range"], "bytes 2-5/10");
        let tail = get("filo-media://art-1/clips/demo%20one.mp4", Some("bytes=7-"));
        assert_eq!(tail.body().as_ref(), b"789");
        assert_eq!(get("filo-media://art-1/clips/demo%20one.mp4", Some("bytes=20-")).status().as_u16(), 416);
        // Not media, outside the folder, or another artifact's: nothing.
        assert_eq!(get("filo-media://art-1/notes.txt", None).status().as_u16(), 404);
        assert_eq!(get("filo-media://art-1/..%2Foutside.mp4", None).status().as_u16(), 404);
        assert_eq!(get("filo-media://art-1/../outside.mp4", None).status().as_u16(), 404);
        assert_eq!(get("filo-media://art-2/clips/demo%20one.mp4", None).status().as_u16(), 404);
        let _ = std::fs::remove_dir_all(&root);
    }
}

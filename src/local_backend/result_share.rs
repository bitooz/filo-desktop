// Sharing a coding round's result (the result page's Share, screens/coding_result.rs): a copy of the round's report
// and its proof goes to Filo's server (which isn't part of this repository), which
// shows it at filo.im/results/<id> to anyone with the link; the page isn't listed or indexed. Unshare deletes it there,
// with the token the share answered. `coding:shareResult` and `coding:unshareResult` (handoff.rs) call these, and
// `coding:roundResult` carries the link while it's shared.
//
// Nothing of this Mac's folders goes with it (`scrub`): paths in the round's proof folder and the project become their
// relative names ("after.png", "src/app.rs"), any other path in a home folder or the temp folders is cut to its file's
// name (report.rs `redact_paths`), and keys and tokens are hidden (report.rs `hide_keys_in`: their common formats, the
// secrets this Mac holds by value, `known_secrets`, and long random strings, `hide_long_secrets`). That holds for the
// title, the report and the text files it sends. A JPEG or PNG goes without its metadata (`without_metadata`: where and
// when it was taken, the camera). Pictures, recordings and PDFs otherwise go as they are, each recording with its poster
// (`poster`, the still the shared page's player shows until it plays), a PDF to be shown in the page's own viewer;
// other files that aren't text stay here, and so does
// whatever doesn't fit the share's size (`TOTAL_MAX`), each named on the page with why. The page is drawn like this page
// in the app, in the sharer's Appearance settings (`look`: theme, light or dark, typeface and size). Before it goes,
// the result page asks, showing what will go (`coding:sharePreview`, `share_preview`).
//
// Artifacts are shared the same way, to the same server (artifact_link.rs): `form`, `send`, `replace` and `delete`.
//
//   local/coding/shares/<task>-<round>.json   the link, and the token that unshares it

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filo_local::coding::{self, Task};
use filo_local::coding_handoff as handoff;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Local, handoff::round_result, now_ms, required};
use crate::report;

/// Filo's server; results go to `<SERVER>/v1/results`, artifacts to `<SERVER>/v1/artifacts`.
const SERVER: &str = "https://dev.filo.im";
/// What a share sends at most, files and report together (the server takes 32 MB, the form's own lines included).
const TOTAL_MAX: usize = 30 * 1024 * 1024;
/// A text file longer than this goes as its last part.
const TEXT_MAX: usize = 2 * 1024 * 1024;
/// A recording's poster, at most (the server takes 1 MB), and how long making it may take: Quick Look takes half a
/// second for a minute of 1080p, and never ends for a file it can't read.
const POSTER_MAX: usize = 1024 * 1024;
const POSTER_TIMEOUT: Duration = Duration::from_secs(5);
const SEND_TIMEOUT: Duration = Duration::from_secs(180);
const DELETE_TIMEOUT: Duration = Duration::from_secs(20);

/// `local/coding/shares/<task>-<round>.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    pub id: String,
    pub url: String,
    pub token: String,
    pub shared_at: i64,
}

fn share_file(task_id: &str, round: i64) -> String {
    format!("coding/shares/{task_id}-{round}.json")
}

/// Where shares of `kind` ("results", "artifacts") go: Filo's server, or in `dev` builds `FILO_RESULTS_URL` or
/// `FILO_ARTIFACTS_URL` (a test server). A share is `<endpoint>/<id>` (PUT replaces it, DELETE deletes it).
pub(super) fn endpoint(kind: &str) -> String {
    crate::test_run::dev_override(&format!("FILO_{}_URL", kind.to_ascii_uppercase()))
        .unwrap_or_else(|| format!("{SERVER}/v1/{kind}"))
}

impl Local {
    /// The round's share, while it's shared.
    pub(super) fn result_share(&self, task_id: &str, round: i64) -> Option<Share> {
        std::fs::read(self.root.join(share_file(task_id, round)))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// `coding:shareResult`: shares the round to `endpoint` (once; shared already, its link) and answers `{ url }`.
    pub(super) fn share_result(&self, args: &Value, endpoint: &str) -> Result<Value, String> {
        let (task, round) = self.shared_round(args)?;
        if let Some(share) = self.result_share(&task.id, round.round) {
            return Ok(json!({ "url": share.url }));
        }
        let page = round_result(&task, &round, filo_local::coding_handoff::round_proof(self.db()?, &round)?);
        let copy = copy(
            &task,
            &round,
            &page,
            look(&crate::config::load_prefs()),
            dirs::home_dir().as_deref(),
            &self.known_secrets(),
            &poster,
        );
        let (form, boundary) = form("result", &copy.result, parts(&copy.files));
        let share = send(endpoint, form, &boundary)?;
        self.write_json(&share_file(&task.id, round.round), &share)?;
        self.changed();
        Ok(json!({ "url": share.url }))
    }

    /// `coding:sharePreview`: what Share would send, for the result page to ask first (security review M1): `files` that
    /// go, those `left` here and why, `asIs` (the pictures, recordings and PDFs among them, which go as they are: only
    /// a person can see what's in them) and `hidden` (how many keys and tokens were taken out of the text).
    pub(super) fn share_preview(&self, args: &Value) -> Result<Value, String> {
        let (task, round) = self.shared_round(args)?;
        let page = round_result(&task, &round, filo_local::coding_handoff::round_proof(self.db()?, &round)?);
        let copy =
            copy(&task, &round, &page, Value::Null, dirs::home_dir().as_deref(), &self.known_secrets(), &|_| None);
        let as_is: Vec<&str> = copy.files.iter().filter(|file| file.as_is).map(|file| file.name.as_str()).collect();
        Ok(json!({ "files": copy.result["files"], "left": copy.result["left"], "asIs": as_is, "hidden": copy.hidden }))
    }

    /// Secrets this Mac holds, hidden by their value from whatever is shared (report.rs `hide_keys_in`): the apps' keys,
    /// sign-in tokens and client secrets (the Keychain, apps.rs), and the tokens of this Mac's own shares.
    pub(super) fn known_secrets(&self) -> Vec<String> {
        let mut known = Vec::new();
        let mut add = |text: &str| match serde_json::from_str::<Value>(text) {
            // Sign-in tokens are kept as JSON: each value in it.
            Ok(Value::Object(fields)) => known.extend(fields.values().filter_map(Value::as_str).map(str::to_string)),
            _ => known.push(text.to_string()),
        };
        if let Ok(apps) = self.db().and_then(filo_local::apps::list) {
            let secrets = filo_local::secrets::Secrets::system();
            for app in apps.iter().filter(|app| app.has_secret || app.auth == filo_local::apps::AUTH_OAUTH) {
                for kind in
                    [filo_local::apps::SECRET_KEY, filo_local::apps::SECRET_TOKENS, filo_local::apps::SECRET_CLIENT]
                {
                    if let Ok(Some(secret)) = secrets.value(&filo_local::secrets::account(&app.id, kind)) {
                        add(&secret);
                    }
                }
            }
        }
        let shares = std::fs::read_dir(self.root.join("coding/shares"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path());
        let links = std::fs::read_dir(self.root.join("artifacts"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("filo-link.json"));
        for file in shares.chain(links) {
            if let Some(share) =
                std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice::<Share>(&bytes).ok())
            {
                known.push(share.token);
            }
        }
        known.retain(|secret| secret.len() >= 8);
        known
    }

    /// `coding:unshareResult`: deletes the round's page on the server, then forgets it here.
    pub(super) fn unshare_result(&self, args: &Value, endpoint: &str) -> Result<Value, String> {
        let (task, round) = self.shared_round(args)?;
        let Some(share) = self.result_share(&task.id, round.round) else { return Ok(Value::Null) };
        delete(endpoint, &share)?;
        std::fs::remove_file(self.root.join(share_file(&task.id, round.round)))
            .map_err(|error| format!("Couldn't forget the link: {error}"))?;
        self.changed();
        Ok(Value::Null)
    }

    fn shared_round(&self, args: &Value) -> Result<(Task, handoff::Round), String> {
        let db = self.db()?;
        let task = coding::task(db, required(args, "taskId")?)?.ok_or("This task isn't on this Mac any more.")?;
        let number = args.get("round").and_then(Value::as_f64).ok_or("Which round?")? as i64;
        let round = handoff::round(db, &task.id, number)?.ok_or("This round isn't on this Mac any more.")?;
        Ok((task, round))
    }
}

/// What a share sends: the result's fields, the files that go and those that stay, and how many keys and tokens were
/// taken out of its text.
#[derive(Debug, PartialEq)]
struct Copy {
    result: Value,
    files: Vec<Sent>,
    hidden: usize,
}

/// A file that goes, and a recording's poster.
#[derive(Debug, PartialEq)]
struct Sent {
    name: String,
    bytes: Vec<u8>,
    poster: Option<Vec<u8>>,
    /// A picture, recording or PDF: it goes as it is (a picture without its metadata), unread.
    as_is: bool,
}

/// The form's parts for `files`: `file0`, `file1`, … in order, each recording's poster after it as `poster<n>`.
fn parts(files: &[Sent]) -> impl Iterator<Item = (String, &str, &[u8])> {
    files.iter().enumerate().flat_map(|(index, file)| {
        let poster = file.poster.as_deref().map(|poster| (format!("poster{index}"), "image/jpeg", poster));
        std::iter::once((format!("file{index}"), "application/octet-stream", file.bytes.as_slice())).chain(poster)
    })
}

/// The sharer's Appearance settings (Settings › Appearance), so the shared page looks like their app.
fn look(prefs: &crate::config::Prefs) -> Value {
    json!({
        "theme": prefs.theme_family,
        "mode": prefs.theme_mode,
        "contrast": prefs.theme_contrast,
        "font": prefs.font_family,
        "size": prefs.font_size,
        "mono": prefs.mono_family,
        "monoSize": prefs.mono_size,
    })
}

/// The round's page (`round_result`) as it may leave this Mac, to be drawn in `look`, with `known` secrets hidden;
/// `poster` makes a recording's.
fn copy(
    task: &Task,
    round: &handoff::Round,
    page: &Value,
    look: Value,
    home: Option<&Path>,
    known: &[String],
    poster: &dyn Fn(&Path) -> Option<Vec<u8>>,
) -> Copy {
    let mut folders: Vec<PathBuf> = [round.proof_dir.as_str(), task.work_path.as_str(), task.project_path.as_str()]
        .into_iter()
        .filter(|folder| folder.len() > 1)
        .map(PathBuf::from)
        .collect();
    folders.dedup();
    let hidden = std::cell::Cell::new(0);
    let clean = |text: &str| {
        let (text, count) = scrub(text, &folders, home, known);
        hidden.set(hidden.get() + count);
        text
    };
    let markdown = clean(page["result"].as_str().unwrap_or_default());
    let mut budget = TOTAL_MAX.saturating_sub(markdown.len() + 64 * 1024);
    let (mut files, mut left) = (Vec::new(), Vec::new());
    for file in page["files"].as_array().into_iter().flatten() {
        let (Some(name), Some(path)) = (file["name"].as_str(), file["url"].as_str()) else { continue };
        let video = file["kind"].as_str() == Some("video");
        let media = video || file["kind"].as_str() == Some("image");
        let pdf = Path::new(name).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
        let bytes = match std::fs::read(path) {
            Ok(bytes) if media && !video => Some(without_metadata(bytes)),
            Ok(bytes) if media || pdf => Some(bytes),
            Ok(bytes) => text_of(&bytes).map(|text| clean(&last_part(&text, TEXT_MAX)).into_bytes()),
            Err(_) => {
                left.push(json!({ "name": name, "why": "couldn’t be read" }));
                continue;
            }
        };
        match bytes {
            None => left.push(json!({ "name": name, "why": "not a picture, recording, PDF or text" })),
            Some(bytes) if bytes.len() > budget => left.push(json!({ "name": name, "why": "too big to share" })),
            Some(bytes) => {
                budget -= bytes.len();
                // Its poster if there's room for it; without one, the page shows the recording's first frame.
                let poster = if video { poster(Path::new(path)).filter(|poster| poster.len() <= budget) } else { None };
                budget -= poster.as_ref().map_or(0, Vec::len);
                files.push(Sent { name: name.to_string(), bytes, poster, as_is: media || pdf });
            }
        }
    }
    let status = match round.status.as_str() {
        "done" => "Reported done",
        "failed" => "Failed",
        "stopped" => "Stopped",
        "monitoring" => "Monitoring",
        _ => "Working",
    };
    let result = json!({
        "title": clean(&task.title),
        "tool": handoff::tool_name(&task.tool),
        "status": status,
        "round": round.round,
        "rounds": task.rounds.max(round.round),
        "createdAt": round.finished_at.unwrap_or(round.started_at),
        "timeZone": iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into()),
        "look": look,
        "markdown": markdown,
        "files": files.iter().map(|file| json!({ "name": file.name })).collect::<Vec<_>>(),
        "left": left,
    });
    Copy { result, files, hidden: hidden.get() }
}

/// A JPEG's or PNG's pixels without the rest of what its file carries (EXIF: where and when it was taken and with
/// what; XMP; text), turned the way it shows (its EXIF orientation applied). Anything else, or a picture it can't
/// read, as it is.
fn without_metadata(bytes: Vec<u8>) -> Vec<u8> {
    use image::ImageDecoder as _;
    let format = match image::guess_format(&bytes) {
        Ok(format @ (image::ImageFormat::Jpeg | image::ImageFormat::Png)) => format,
        _ => return bytes,
    };
    let redrawn = (|| -> image::ImageResult<Vec<u8>> {
        let mut decoder = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format).into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut picture = image::DynamicImage::from_decoder(decoder)?;
        picture.apply_orientation(orientation);
        let mut out = std::io::Cursor::new(Vec::new());
        match format {
            image::ImageFormat::Jpeg => {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&picture.to_rgb8())?
            }
            _ => picture.write_to(&mut out, image::ImageFormat::Png)?,
        }
        Ok(out.into_inner())
    })();
    redrawn.unwrap_or(bytes)
}

/// A recording's poster: the still Quick Look shows for it (`qlmanage -t`, which takes it with AVFoundation, a frame
/// a little way in), as a JPEG at most 1280 across; none when it has none or it's too big.
pub(crate) fn poster(video: &Path) -> Option<Vec<u8>> {
    let folder = std::env::temp_dir().join(format!("filo-poster-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&folder).ok()?;
    let still = quick_look_still(video, &folder).and_then(|png| {
        let picture = image::open(png).ok()?;
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 82).encode_image(&picture.to_rgb8()).ok()?;
        (jpeg.len() <= POSTER_MAX).then_some(jpeg)
    });
    let _ = std::fs::remove_dir_all(&folder);
    still
}

/// Quick Look's still of `video`, as a PNG it writes in `folder`.
fn quick_look_still(video: &Path, folder: &Path) -> Option<PathBuf> {
    crate::command::run(
        Command::new("/usr/bin/qlmanage").args(["-t", "-s", "1280", "-o"]).arg(folder).arg(video),
        None,
        POSTER_TIMEOUT,
    )?;
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "png"))
}

/// `text` as it may leave this Mac: paths in `folders` (the round's proof folder, the project) relative to them, any
/// other path in a home folder or the temp folders cut to its file's name, and keys and tokens hidden (`known` by
/// value, long random strings too); and how many keys and tokens it hid.
fn scrub(text: &str, folders: &[PathBuf], home: Option<&Path>, known: &[String]) -> (String, usize) {
    let mut folders: Vec<&PathBuf> = folders.iter().collect();
    // The proof folder can be in the project; the longer goes first.
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.as_os_str().len()));
    let mut text = text.to_string();
    for folder in folders {
        let name = folder.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
        text = relative(&text, &folder.display().to_string(), &name);
    }
    let (text, keys) = report::hide_keys_in(&report::redact_paths(&text, home), known);
    let (text, long) = report::hide_long_secrets(&text);
    (text, keys + long)
}

/// `folder/x` (and `file://folder/x`) as `x`, the folder itself as its `name`; a longer folder's name that starts the
/// same ("app-old", "app.v2", not "app." ending a sentence) is left to `redact_paths`.
fn relative(text: &str, folder: &str, name: &str) -> String {
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(at) = rest.find(folder) {
        let (before, after) = (&rest[..at], &rest[at + folder.len()..]);
        let mut next = after.chars();
        let (first, second) = (next.next(), next.next());
        let longer = match first {
            Some('.') => second.is_some_and(char::is_alphanumeric),
            Some(first) => first.is_alphanumeric() || "-_".contains(first),
            None => false,
        };
        if longer {
            out.push_str(&rest[..at + folder.len()]);
            rest = after;
            continue;
        }
        out.push_str(before.strip_suffix("file://").unwrap_or(before));
        match first {
            Some('/') => rest = &after[1..],
            _ => {
                out.push_str(name);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A text file's text; none for anything else (no NULs, valid UTF-8).
fn text_of(bytes: &[u8]) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// The end of a long text (a log's), from a line's start, said to be the end.
fn last_part(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let start = text[start..].find('\n').map_or(start, |at| start + at + 1);
    format!("[The start of this file is left out; this is its last {} KB.]\n{}", max / 1024, &text[start..])
}

/// The form the server takes: `field` (JSON: a result's, an artifact's) and then its files, by their part's name
/// (a result's `file0`, `file1`, … in the order `result.files` names them; an artifact's `page`).
pub(super) fn form<'a>(
    field: &str,
    value: &Value,
    files: impl IntoIterator<Item = (String, &'a str, &'a [u8])>,
) -> (Vec<u8>, String) {
    let boundary = format!("filo-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    let mut part = |name: &str, filename: Option<&str>, content_type: &str, bytes: &[u8]| {
        let filename = filename.map(|filename| format!("; filename=\"{filename}\"")).unwrap_or_default();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{filename}\r\nContent-Type: {content_type}\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    };
    part(field, None, "application/json", value.to_string().as_bytes());
    for (name, content_type, bytes) in files {
        part(&name, Some(&name), content_type, bytes);
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (body, boundary)
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("filo-desktop")
        .timeout(timeout)
        .build()
        .map_err(|error| error.to_string())
}

/// The words for a request that didn't go through.
fn failed(error: reqwest::Error) -> String {
    if error.is_connect() || error.is_timeout() {
        "Couldn’t reach Filo’s server. Check your connection and try again.".into()
    } else {
        format!("Couldn’t reach Filo’s server: {}", error.without_url())
    }
}

/// What the server said no with, in words for the page.
fn refused(status: u16, answer: &Value) -> String {
    match (status, answer["error"].as_str()) {
        (429, _) => "Too many shares from here just now. Try again in a minute.".into(),
        (413, _) => "It’s too big to share.".into(),
        (status, _) if status >= 500 => format!("Filo’s server had a problem ({status}). Try again in a few minutes."),
        (status, Some(said)) => format!("Filo’s server didn’t take it ({status}: {said})."),
        (status, None) => format!("Filo’s server didn’t take it ({status})."),
    }
}

/// A new share: `form` to `endpoint`, which answers its id, link and token.
pub(super) fn send(endpoint: &str, form: Vec<u8>, boundary: &str) -> Result<Share, String> {
    let response = client(SEND_TIMEOUT)?
        .post(endpoint)
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(form)
        .send()
        .map_err(failed)?;
    let status = response.status().as_u16();
    let answer: Value = response.json().unwrap_or(Value::Null);
    if status != 201 {
        return Err(refused(status, &answer));
    }
    let field = |name: &str| {
        answer[name]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("Filo’s server answered without the share’s {name}."))
    };
    Ok(Share { id: field("id")?, url: field("url")?, token: field("token")?, shared_at: now_ms() })
}

/// What's shared at `share`'s link replaced by `form`; false when it isn't shared there any more.
pub(super) fn replace(endpoint: &str, share: &Share, form: Vec<u8>, boundary: &str) -> Result<bool, String> {
    let response = client(SEND_TIMEOUT)?
        .put(share_url(endpoint, share))
        .bearer_auth(&share.token)
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(form)
        .send()
        .map_err(failed)?;
    match response.status().as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        status => Err(refused(status, &response.json().unwrap_or(Value::Null))),
    }
}

fn share_url(endpoint: &str, share: &Share) -> String {
    format!("{}/{}", endpoint.trim_end_matches('/'), share.id)
}

pub(super) fn delete(endpoint: &str, share: &Share) -> Result<(), String> {
    let response =
        client(DELETE_TIMEOUT)?.delete(share_url(endpoint, share)).bearer_auth(&share.token).send().map_err(failed)?;
    match response.status().as_u16() {
        // Gone already (unshared from another copy of this folder): the same.
        204 | 404 => Ok(()),
        status => Err(refused(status, &response.json().unwrap_or(Value::Null))),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use filo_local::coding::{NewTask, Project};

    use super::*;

    #[test]
    fn paths_become_relative_or_names_and_keys_go() {
        let folders = [
            PathBuf::from("/Users/ann/.config/filo/local/coding/proof/t/round-1"),
            PathBuf::from("/Users/ann/code/app"),
        ];
        let home = Some(Path::new("/Users/ann"));
        let text = "Saved /Users/ann/.config/filo/local/coding/proof/t/round-1/after.png and file:///Users/ann/code/app/src/main.rs:12 \
                    in /Users/ann/code/app. Also /Users/ann/code/app-old/x.rs, ~/Desktop/notes/plan.md, /Users/bob/secret/thing.txt, \
                    /private/var/folders/ab/cd/T/run.log and key sk-ant-abcdefghijklmnop.";
        assert_eq!(
            scrub(text, &folders, home, &[]),
            ("Saved after.png and src/main.rs:12 in app. Also ~/…/x.rs, ~/…/plan.md, /Users/…/…/thing.txt, …/…/run.log and key [key].".to_string(), 1)
        );
        // Long names stay: they aren't keys.
        let name = "a_test_that_has_a_really_quite_long_name_for_what_it_checks passed";
        assert_eq!(scrub(name, &folders, home, &[]), (name.to_string(), 0));
        // A secret this Mac holds goes by its value, an env file's by its name, a long random one by its look.
        let log = "DB_PASSWORD=Tr0ub4dor-3 · used plainwords-long · aws wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY1";
        assert_eq!(
            scrub(log, &folders, home, &["plainwords-long".into()]),
            ("DB_PASSWORD=[hidden] · used [hidden] · aws [hidden]".to_string(), 3)
        );
    }

    #[test]
    fn a_long_log_goes_as_its_end() {
        let log = (0..1000).map(|n| format!("line {n}\n")).collect::<String>();
        let end = last_part(&log, 100);
        assert!(end.starts_with("[The start of this file is left out; this is its last 0 KB.]\nline 9"), "{end}");
        assert!(end.ends_with("line 999\n") && end.len() < 200);
        assert_eq!(last_part("short", 100), "short");
    }

    /// A server that answers each request with the next of `answers`, and keeps what it was sent; its `/v1/results`.
    fn server(answers: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        test_server("results", answers)
    }

    /// `server` at `/v1/<kind>` (artifact_link.rs's tests share it).
    pub(in crate::local_backend) fn test_server(
        kind: &str,
        answers: Vec<(u16, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/{kind}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 65536];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                    if read == 0 {
                        break;
                    }
                }
                kept.lock().unwrap().push(String::from_utf8_lossy(&request).to_string());
                write!(stream, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (url, seen)
    }

    /// A round shares as a copy free of this Mac's paths, its link shows on its page, and Unshare deletes it there.
    #[test]
    fn a_round_is_shared_and_unshared() {
        let local = Local::open(std::env::temp_dir().join(format!("filo-local-share-{}", uuid::Uuid::new_v4())));
        let db = local.db().unwrap();
        let project = local.root.join("My App");
        std::fs::create_dir_all(&project).unwrap();
        let new = NewTask {
            brief: "Goal: fix the login button".into(),
            tool: "claude-code".into(),
            project: Project {
                name: "My App".into(),
                path: project.display().to_string(),
                branch: None,
                remote: None,
                plain_folder: true,
                added: true,
            },
            worktree: false,
            owner: "Filo".into(),
        };
        let task = coding::create_task(db, &new, 1).unwrap();
        let proof = handoff::proof_dir(&local.root, &task.id, 1);
        std::fs::create_dir_all(&proof).unwrap();
        std::fs::write(proof.join("after.png"), [0x89, b'P', b'N', b'G']).unwrap();
        std::fs::write(proof.join("tests.log"), format!("ok in {}/src/login.rs\n", project.display())).unwrap();
        std::fs::write(proof.join("core.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(proof.join("plan.pdf"), b"%PDF-1.4\n\0").unwrap();
        handoff::start_round(db, &task.id, 1, &proof, 2).unwrap();
        let report = format!(
            "# Fixed\n\nChanged {}/src/login.rs.\n\n![After](after.png)\n\nLog: {}/tests.log",
            project.display(),
            proof.display()
        );
        handoff::finish_round(db, &task.id, 1, "done", Some(&report), None, 3).unwrap();
        let task = coding::task(db, &task.id).unwrap().unwrap();

        let (url, seen) = server(vec![
            (201, r#"{"id":"0b5c","url":"https://filo.im/results/0b5c","token":"secret-token"}"#),
            (204, ""),
        ]);
        let args = json!({ "taskId": task.id, "round": 1 });
        // Asked first: what goes, what stays and why, what goes unread.
        let preview = local.call("coding:sharePreview", &args).unwrap();
        assert_eq!(preview["files"], json!([{ "name": "after.png" }, { "name": "plan.pdf" }, { "name": "tests.log" }]));
        assert_eq!(preview["left"], json!([{ "name": "core.bin", "why": "not a picture, recording, PDF or text" }]));
        assert_eq!((preview["asIs"].clone(), preview["hidden"].as_u64()), (json!(["after.png", "plan.pdf"]), Some(0)));
        assert!(seen.lock().unwrap().is_empty());
        let shared = local.share_result(&args, &url).unwrap();
        assert_eq!(shared["url"], "https://filo.im/results/0b5c");
        let sent = seen.lock().unwrap()[0].clone();
        assert!(sent.starts_with("POST /v1/results ") && sent.contains("multipart/form-data; boundary="), "{sent}");
        assert!(!sent.contains(&local.root.display().to_string()), "{sent}");
        let result: Value = serde_json::from_str(sent.lines().find(|line| line.starts_with("{\"")).unwrap()).unwrap();
        assert_eq!(
            result["markdown"],
            "# Fixed\n\nChanged src/login.rs.\n\n![After](after.png)\n\nLog: tests.log\n\n**Proof it saved**\n\n![core.bin](<core.bin>)\n\n![plan.pdf](<plan.pdf>)\n\n![tests.log](<tests.log>)"
        );
        assert_eq!(
            (result["tool"].as_str(), result["status"].as_str(), result["round"].as_i64()),
            (Some("Claude Code"), Some("Reported done"), Some(1))
        );
        // Drawn in the sharer's Appearance settings.
        assert!(
            result["look"]["font"].is_string()
                && result["look"]["size"].is_number()
                && result["look"]["mode"].is_string(),
            "{}",
            result["look"]
        );
        // A PDF goes as it is, for the page's viewer; other files that aren't text stay here.
        assert_eq!(result["files"], json!([{ "name": "after.png" }, { "name": "plan.pdf" }, { "name": "tests.log" }]));
        assert_eq!(result["left"], json!([{ "name": "core.bin", "why": "not a picture, recording, PDF or text" }]));
        assert!(sent.contains("ok in src/login.rs\n"), "{sent}");

        // Its page says where it is; sharing again gives the same link.
        let page = local.call("coding:roundResult", &args).unwrap();
        assert_eq!(page["share"]["url"], "https://filo.im/results/0b5c");
        assert_eq!(local.share_result(&args, &url).unwrap()["url"], "https://filo.im/results/0b5c");
        assert_eq!(seen.lock().unwrap().len(), 1);

        local.unshare_result(&args, &url).unwrap();
        let deleted = seen.lock().unwrap()[1].clone();
        assert!(
            deleted.starts_with("DELETE /v1/results/0b5c ")
                && deleted.to_ascii_lowercase().contains("authorization: bearer secret-token"),
            "{deleted}"
        );
        assert!(local.call("coding:roundResult", &args).unwrap()["share"].is_null());
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A recording goes with its poster, after it in the form; a picture goes alone.
    #[test]
    fn a_recording_goes_with_its_poster() {
        let local = Local::open(std::env::temp_dir().join(format!("filo-local-poster-{}", uuid::Uuid::new_v4())));
        let db = local.db().unwrap();
        let new = NewTask {
            brief: "Goal: record the demo".into(),
            tool: "claude-code".into(),
            project: Project {
                name: "App".into(),
                path: local.root.display().to_string(),
                branch: None,
                remote: None,
                plain_folder: true,
                added: true,
            },
            worktree: false,
            owner: "Filo".into(),
        };
        let task = coding::create_task(db, &new, 1).unwrap();
        let proof = handoff::proof_dir(&local.root, &task.id, 1);
        std::fs::create_dir_all(&proof).unwrap();
        std::fs::write(proof.join("after.png"), [0x89, b'P', b'N', b'G']).unwrap();
        std::fs::write(proof.join("demo.mp4"), [0u8; 16]).unwrap();
        handoff::start_round(db, &task.id, 1, &proof, 2).unwrap();
        handoff::finish_round(
            db,
            &task.id,
            1,
            "done",
            Some("# Done\n\n![Demo](demo.mp4)\n\n![After](after.png)"),
            None,
            3,
        )
        .unwrap();
        let round = handoff::round(db, &task.id, 1).unwrap().unwrap();
        let page = round_result(&task, &round, handoff::round_proof(db, &round).unwrap());
        let still =
            |path: &Path| Some(format!("still of {}", path.file_name().unwrap().to_string_lossy()).into_bytes());
        let copy = copy(&task, &round, &page, json!({}), None, &[], &still);
        let names: Vec<_> = copy.files.iter().map(|file| (file.name.as_str(), file.poster.as_deref())).collect();
        assert_eq!(names, [("after.png", None), ("demo.mp4", Some(b"still of demo.mp4".as_slice()))]);
        let parts: Vec<_> = parts(&copy.files).map(|(name, kind, _)| (name, kind)).collect();
        assert_eq!(
            parts,
            [
                ("file0".into(), "application/octet-stream"),
                ("file1".into(), "application/octet-stream"),
                ("poster1".into(), "image/jpeg")
            ]
        );
        std::fs::remove_dir_all(&local.root).ok();
    }

    /// A shared picture goes without its EXIF (a phone photo's place and time), turned the way it showed.
    #[test]
    fn a_picture_goes_without_its_metadata() {
        let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 2, image::Rgb([200, 30, 30])));
        let mut jpeg = std::io::Cursor::new(Vec::new());
        picture.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let jpeg = jpeg.into_inner();
        // An APP1 Exif segment right after the start: orientation 6 (turned right), and a GPS-ish payload.
        let mut exif =
            b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0GPS 41.38N 2.17E".to_vec();
        let mut with = jpeg[..2].to_vec();
        with.extend_from_slice(&[0xff, 0xe1]);
        with.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        with.append(&mut exif);
        with.extend_from_slice(&jpeg[2..]);
        let without = without_metadata(with.clone());
        assert!(with.windows(4).any(|bytes| bytes == b"GPS ") && !without.windows(4).any(|bytes| bytes == b"GPS "));
        let shown = image::load_from_memory(&without).unwrap();
        assert_eq!((shown.width(), shown.height()), (2, 4));
        // Not a picture it reads: as it is.
        assert_eq!(without_metadata(b"%PDF-1.4".to_vec()), b"%PDF-1.4");
    }

    #[test]
    fn a_refusal_says_why() {
        assert_eq!(refused(429, &Value::Null), "Too many shares from here just now. Try again in a minute.");
        assert_eq!(
            refused(400, &json!({ "error": "Invalid result field: title" })),
            "Filo’s server didn’t take it (400: Invalid result field: title)."
        );
        assert_eq!(refused(503, &Value::Null), "Filo’s server had a problem (503). Try again in a few minutes.");
    }
}

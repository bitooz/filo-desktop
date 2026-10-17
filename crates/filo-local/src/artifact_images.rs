// Pictures from the web an artifact's page names: an avatar's `<img src="https://…">`, a CSS `url(https://…)`, an
// image address in a script's string. The artifact viewer has no network (screens/artifact.rs `FRAME_CSP`), so they
// wouldn't show there; instead, before a version is saved (artifacts.rs `create`, `update`, `publish`), Filo downloads
// them into the artifact's folder (`save`), and wherever the page is shown it points at those copies (`rewrite`): the
// viewer as `filo-media:` links (screens/artifact_media.rs serves the folder), a shared link and the claude.ai copy
// with the pictures put in the page (`inline`). Artifacts made before get the same once (`save_existing`).
//
//   <artifacts>/<id>/images/<hash>.<ext>   a picture, named by its contents, kept for every version that names it
//   <artifacts>/<id>/images.json           each address as the page writes it, and its picture's file
//
// The version's file itself stays as published, so publishing the same file again still saves no new version, and
// opening it in a browser loads the pictures from the web as before. Only real pictures (PNG, JPEG, GIF, WebP, AVIF,
// by their first bytes), each at most `MAX_IMAGE`, from a host that isn't this Mac or its network; a download that
// fails leaves the address as it is.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use crate::db::Db;

/// The pictures' folder, in the artifact's.
pub const DIR: &str = "images";
/// Which address is which picture, in the artifact's folder.
const MANIFEST: &str = "images.json";
/// One picture at most this big.
const MAX_IMAGE: usize = 5 * 1024 * 1024;
/// A page's new pictures at most this big together, and at most this many: the share puts them in the page.
const MAX_TOTAL: usize = 16 * 1024 * 1024;
const MAX_COUNT: usize = 300;
/// What one download may take, and all of a save's together (a page's publish waits for them).
const TIMEOUT: Duration = Duration::from_secs(15);
const DEADLINE: Duration = Duration::from_secs(45);
const WORKERS: usize = 6;
/// The database's note that the artifacts made before have had their pictures saved (`save_existing`).
const SAVED_EXISTING: &str = "artifact_web_images";

/// A web address in the page: up to a quote, a space, a bracket or a backslash, without a sentence's last stop.
fn addresses(html: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(found) = html[from..].find("http") {
            let at = from + found;
            let rest = &html[at..];
            let scheme = if rest.starts_with("https://") {
                8
            } else if rest.starts_with("http://") {
                7
            } else {
                0
            };
            if scheme == 0 {
                from = at + 4;
                continue;
            }
            let length =
                rest.find(|char: char| char.is_whitespace() || "\"'<>()\\`".contains(char)).unwrap_or(rest.len());
            from = at + length.max(scheme);
            let text = rest[..length].trim_end_matches(['.', ',', ';', ':', '!', '?']);
            // A link's address opens in the browser: it stays.
            let before = html[..at].trim_end_matches(['"', '\'']).trim_end();
            let before = before.get(before.len().saturating_sub(5)..).unwrap_or("").to_ascii_lowercase();
            if text.len() > scheme && before != "href=" {
                return Some((at, text));
            }
        }
        None
    })
}

/// Whether an address at `at` is a picture's: where a picture goes (`src=`, `srcset=`, `poster=`, `url(`), or with a
/// picture's extension or format anywhere else (a script's string).
fn picture_address(html: &str, at: usize, address: &str) -> bool {
    let Ok(url) = url::Url::parse(&address.replace("&amp;", "&")) else { return false };
    let path = url.path().to_ascii_lowercase();
    const PICTURES: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "avif"];
    let extension = path.rsplit_once('.').map(|(_, extension)| extension).unwrap_or("");
    if PICTURES.contains(&extension)
        || url.query_pairs().any(|(key, value)| {
            (key == "format" || key == "fm") && PICTURES.contains(&value.to_ascii_lowercase().as_str())
        })
    {
        return true;
    }
    if ["js", "mjs", "css", "html", "htm", "json", "svg", "woff", "woff2", "ttf", "mp4", "mp3", "pdf"]
        .contains(&extension)
    {
        return false;
    }
    let before = html[..at].trim_end_matches(['"', '\'']).trim_end();
    let before = before.get(before.len().saturating_sub(7)..).unwrap_or("").to_ascii_lowercase();
    ["src=", "srcset=", "poster=", "url("].iter().any(|context| before.ends_with(context))
}

/// Not this Mac or its network: a page mustn't make Filo fetch from there (and share what it gets).
fn public_host(address: &str) -> bool {
    let Ok(url) = url::Url::parse(address) else { return false };
    match url.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.to_ascii_lowercase();
            name != "localhost" && !name.ends_with(".localhost") && !name.ends_with(".local") && name.contains('.')
        }
        Some(url::Host::Ipv4(ip)) => {
            cfg!(test) && ip.is_loopback()
                || !(ip.is_loopback()
                    || ip.is_private()
                    || ip.is_link_local()
                    || ip.is_unspecified()
                    || ip.is_broadcast())
        }
        Some(url::Host::Ipv6(ip)) => {
            !(ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80)
        }
        None => false,
    }
}

/// A picture's extension from its first bytes; none for anything else.
fn picture_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.len() > 12 && &bytes[4..8] == b"ftyp" && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis") {
        Some("avif")
    } else {
        None
    }
}

fn manifest(folder: &Path) -> BTreeMap<String, String> {
    crate::files::read_json(&folder.join(MANIFEST)).ok().flatten().unwrap_or_default()
}

/// The picture at `address`, when it's one and small enough.
fn download(client: &reqwest::blocking::Client, address: &str) -> Option<(Vec<u8>, &'static str)> {
    let response = client.get(address).send().ok()?;
    if !response.status().is_success() || response.content_length().is_some_and(|length| length > MAX_IMAGE as u64) {
        return None;
    }
    let mut bytes = Vec::new();
    response.take(MAX_IMAGE as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_IMAGE {
        return None;
    }
    let kind = picture_kind(&bytes)?;
    Some((bytes, kind))
}

/// Downloads the web pictures `html` names into the artifact folder `folder` (made when there's one), those it has
/// already aside. How many it names that are saved there now.
pub fn save(folder: &Path, html: &str) -> usize {
    let known = manifest(folder);
    let mut named: Vec<&str> = addresses(html)
        .filter(|&(at, address)| picture_address(html, at, address))
        .map(|(_, address)| address)
        .collect();
    named.sort_unstable();
    named.dedup();
    let saved = |address: &str| known.get(address).is_some_and(|file| folder.join(DIR).join(file).is_file());
    let wanted: Vec<&str> = named
        .iter()
        .copied()
        .filter(|address| !saved(address))
        .filter(|address| public_host(&address.replace("&amp;", "&")))
        .take(MAX_COUNT)
        .collect();
    let had = named.len() - named.iter().filter(|address| !saved(address)).count();
    if wanted.is_empty() {
        return had;
    }
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Filo")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
    else {
        return had;
    };
    let started = Instant::now();
    let (next, total, got) = (AtomicUsize::new(0), AtomicUsize::new(0), Mutex::new(Vec::new()));
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(wanted.len()) {
            scope.spawn(|| {
                while started.elapsed() < DEADLINE {
                    let Some(&address) = wanted.get(next.fetch_add(1, Ordering::SeqCst)) else { break };
                    let Some((bytes, kind)) = download(&client, &address.replace("&amp;", "&")) else { continue };
                    if total.fetch_add(bytes.len(), Ordering::SeqCst) + bytes.len() > MAX_TOTAL {
                        continue;
                    }
                    let hash = Sha256::digest(&bytes);
                    let file = format!(
                        "{}.{kind}",
                        hash.iter().take(10).map(|byte| format!("{byte:02x}")).collect::<String>()
                    );
                    let path = folder.join(DIR).join(&file);
                    if path.is_file() || crate::files::write_atomic(&path, &bytes).is_ok() {
                        got.lock().unwrap().push((address.to_string(), file));
                    }
                }
            });
        }
    });
    let got = got.into_inner().unwrap();
    let count = got.len();
    if count > 0 {
        // Read again: another process may have saved some of this artifact's meanwhile.
        let mut all = manifest(folder);
        all.extend(got);
        if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
            let _ = crate::files::write_atomic(&folder.join(MANIFEST), &bytes);
        }
    }
    had + count
}

/// `html` with the web pictures saved in artifact folder `folder` as `to` gives each (by its file there); the rest
/// as they are.
pub fn rewrite(html: &str, folder: &Path, mut to: impl FnMut(&str, &Path) -> Option<String>) -> String {
    let known = manifest(folder);
    if known.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len());
    let mut from = 0;
    for (at, address) in addresses(html) {
        let Some(file) = known.get(address) else { continue };
        let path = folder.join(DIR).join(file);
        if !path.is_file() {
            continue;
        }
        if let Some(replacement) = to(file, &path) {
            out.push_str(&html[from..at]);
            out.push_str(&replacement);
            from = at + address.len();
        }
    }
    out.push_str(&html[from..]);
    out
}

/// `html` with the web pictures saved for it put in it as `data:` addresses, so it shows them anywhere (a shared
/// link, the claude.ai copy), up to `MAX_TOTAL` of them.
pub fn inline(html: &str, folder: &Path) -> String {
    let mut done: BTreeMap<String, String> = BTreeMap::new();
    let mut total = 0;
    rewrite(html, folder, |file, path| {
        if let Some(data) = done.get(file) {
            return Some(data.clone());
        }
        let bytes = std::fs::read(path).ok()?;
        let kind = picture_kind(&bytes)?;
        total += bytes.len();
        if total > MAX_TOTAL {
            return None;
        }
        let mime = if kind == "jpg" { "jpeg" } else { kind };
        let data = format!("data:image/{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes));
        done.insert(file.to_string(), data.clone());
        Some(data)
    })
}

/// Saves the web pictures of every version of the artifacts made before this, once (the app, at its start): the
/// database notes it's been done.
pub fn save_existing(db: &Db, dir: &Path) -> Result<usize, String> {
    let claimed = db.write(|tx| {
        if crate::app_data::state(tx, SAVED_EXISTING)?.is_some() {
            return Ok(false);
        }
        crate::app_data::set_state(tx, SAVED_EXISTING, "started")?;
        Ok(true)
    })?;
    if !claimed {
        return Ok(0);
    }
    let versions: Vec<(String, String)> = db.with(|conn| {
        let mut statement =
            conn.prepare("SELECT artifact_id, file FROM artifact_versions ORDER BY artifact_id, version")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    })?;
    let mut saved = 0;
    for (id, file) in versions {
        let Ok(folder) = crate::artifacts::artifact_dir(dir, &id) else { continue };
        if file.contains('/') || file.contains("..") {
            continue;
        }
        if let Ok(page) = std::fs::read_to_string(folder.join(&file)) {
            saved += save(&folder, &page);
        }
    }
    db.with(|conn| crate::app_data::set_state(conn, SAVED_EXISTING, "done"))?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\xdac\xf8\x0f\0\x01\x01\x01\0\x18\xdd\x8d\xb0\0\0\0\0IEND\xaeB`\x82";

    /// A server on this Mac that answers `/avatar.png` (and `/face`) with a picture, `/page.png` with a page and
    /// anything else with 404; how many requests it took.
    fn server() -> (String, std::sync::Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                let mut stream = stream;
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let line = String::from_utf8_lossy(&request[..read]).lines().next().unwrap_or("").to_string();
                let (status, body): (&str, &[u8]) = if line.contains("/avatar.png") || line.contains("/face ") {
                    ("200 OK", PNG)
                } else if line.contains("/page.png") {
                    ("200 OK", b"<html>not a picture</html>")
                } else {
                    ("404 Not Found", b"")
                };
                let _ =
                    write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(body);
            }
        });
        (address, asked)
    }

    #[test]
    fn picks_picture_addresses_not_links() {
        let page = r#"<img src="https://a.example/face"><a href="https://a.example/x.png">x</a><script src="https://cdn.example/lib.js"></script>
<style>.b{background:url(https://a.example/bg)}</style><script>const avatars = {"x": "https://pbs.example/p/1_normal.jpg", "y": "https://a.example/post"};
const photo = "https://pbs.example/media/2?format=webp&amp;name=small";</script><p>See https://a.example/logo.png.</p>"#;
        let found: Vec<&str> = addresses(page)
            .filter(|&(at, address)| picture_address(page, at, address))
            .map(|(_, address)| address)
            .collect();
        assert_eq!(
            found,
            [
                "https://a.example/face",
                "https://a.example/bg",
                "https://pbs.example/p/1_normal.jpg",
                "https://pbs.example/media/2?format=webp&amp;name=small",
                "https://a.example/logo.png"
            ]
        );
        assert!(
            !public_host("http://localhost:3000/a.png")
                && !public_host("http://192.168.1.4/a.png")
                && !public_host("http://[::1]/a.png")
        );
        assert!(public_host("https://pbs.twimg.com/a.png") && public_host("http://93.184.216.34/a.png"));
    }

    #[test]
    fn saves_real_pictures_and_points_the_page_at_them() {
        let (server, asked) = server();
        let folder = std::env::temp_dir().join(format!("filo-artifact-images-{}", uuid::Uuid::new_v4()));
        let page = format!(
            r#"<img src="{server}/avatar.png"><img src='{server}/page.png'><img src="{server}/missing.png"><script>const a = {{"x": "{server}/avatar.png"}}; const b = '<img src="' + a.x + '">';</script><a href="{server}/avatar.png">open</a>"#
        );
        assert_eq!(save(&folder, &page), 1);
        let files: Vec<_> = std::fs::read_dir(folder.join(DIR))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with(".png"));
        assert_eq!(std::fs::read(folder.join(DIR).join(&files[0])).unwrap(), PNG);
        // Saved once: saving the page again asks only for the ones that failed.
        let before = asked.load(Ordering::SeqCst);
        assert_eq!(save(&folder, &page), 1);
        assert_eq!(asked.load(Ordering::SeqCst) - before, 2);

        let shown = rewrite(&page, &folder, |file, _| Some(format!("filo-media://art/images/{file}")));
        let local = format!("filo-media://art/images/{}", files[0]);
        assert_eq!(shown.matches(&local).count(), 2, "{shown}");
        assert!(
            shown.contains(&format!("'{server}/page.png'")) && shown.contains(&format!("\"{server}/missing.png\""))
        );
        assert!(shown.contains(&format!("<a href=\"{server}/avatar.png\">")), "a link stays a link: {shown}");
        let alone = inline(&page, &folder);
        assert_eq!(alone.matches("data:image/png;base64,iVBORw0KGgo").count(), 2, "{alone}");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

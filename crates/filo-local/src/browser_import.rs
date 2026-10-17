// Bringing the user's profile from Helium or Chrome into Filo's browser (the app's src/browser/import.rs): bookmarks
// and history, read from the profile's own plain files, and logins, from a passwords CSV the user exported from that
// browser's password manager. Nothing of the other browser's is changed, and its cookie and password stores are never
// read: they're encrypted with a key of that browser's, and the user's own export is the way out it offers.
//
// - Profiles: `Local State` in the browser's folder under ~/Library/Application Support (`net.imput.helium`,
//   `Google/Chrome`) names them (`profile.info_cache`); each is a folder there (`Default`, `Profile 2`, …).
// - Bookmarks: the profile's `Bookmarks`, JSON. History: its `History`, SQLite, which the browser keeps locked while it
//   runs, so it (and its WAL) is copied to a scratch file first, read, and the copy removed.
// - Both go to `browser-places.json` in the local data folder, merged by address with what's there: the address bar's
//   suggestions (`suggest`).
// - Logins: the CSV's rows (Chrome's and Helium's `name,url,username,password,note`; Safari's and Firefox's headers
//   too) saved in one Keychain item of Filo's own (service `im.filo.desktop.browser`, account `logins`), as JSON,
//   merged by site and username. The file is deleted once they're saved. `logins_for` gives a page's.
//
// `FILO_BROWSER_IMPORT_ROOT` (`dev` builds) stands in for ~/Library/Application Support, for checks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::secrets::Secrets;

/// The address bar's places, in the local data folder.
pub const PLACES: &str = "browser-places.json";
/// The Keychain item the logins are kept in.
pub const LOGINS_SERVICE: &str = "im.filo.desktop.browser";
const LOGINS_ACCOUNT: &str = "logins";
/// The newest pages of a profile's history brought in at most.
const MAX_HISTORY: usize = 20_000;
/// The places kept at most: every bookmark, then history by when it was last visited.
const MAX_PLACES: usize = 30_000;
/// Seconds from 1601-01-01 (Chrome's times count microseconds from it) to 1970-01-01.
const WINDOWS_EPOCH: i64 = 11_644_473_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Helium,
    Chrome,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Helium, Source::Chrome];

    pub fn name(self) -> &'static str {
        match self {
            Source::Helium => "Helium",
            Source::Chrome => "Chrome",
        }
    }

    /// Its folder under Application Support.
    fn folder(self) -> &'static str {
        match self {
            Source::Helium => "net.imput.helium",
            Source::Chrome => "Google/Chrome",
        }
    }

    pub fn dir(self) -> PathBuf {
        root().join(self.folder())
    }

    pub fn installed(self) -> bool {
        self.dir().join("Local State").is_file()
    }
}

fn root() -> PathBuf {
    #[cfg(feature = "dev")]
    if let Some(dir) = std::env::var_os("FILO_BROWSER_IMPORT_ROOT").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Library/Application Support")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub source: Source,
    /// As the browser shows it ("Work"), with the account's address when one is signed in.
    pub name: String,
    pub email: String,
    pub path: PathBuf,
}

/// The browser's profiles, the one used last first.
pub fn profiles(source: Source) -> Vec<Profile> {
    let dir = source.dir();
    let Some(state) =
        std::fs::read(dir.join("Local State")).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Vec::new();
    };
    let last = state.pointer("/profile/last_used").and_then(Value::as_str).unwrap_or("Default").to_string();
    let mut found: Vec<(String, Profile)> = state
        .pointer("/profile/info_cache")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(folder, _)| !folder.contains('/') && !folder.contains("..") && dir.join(folder).is_dir())
        .map(|(folder, info)| {
            let text = |key: &str| info.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_string();
            let name = Some(text("name")).filter(|name| !name.is_empty()).unwrap_or_else(|| folder.clone());
            (folder.clone(), Profile { source, name, email: text("user_name"), path: dir.join(folder) })
        })
        .collect();
    found.sort_by_key(|(folder, profile)| (*folder != last, profile.name.to_lowercase()));
    found.into_iter().map(|(_, profile)| profile).collect()
}

/// A page the address bar can suggest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub bookmark: bool,
    #[serde(default)]
    pub visits: u32,
    /// Last visited (or bookmarked), seconds since 1970.
    #[serde(default)]
    pub last: i64,
}

fn web_address(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Chrome's time (microseconds since 1601) as seconds since 1970.
fn unix_time(chrome: i64) -> i64 {
    if chrome <= 0 { 0 } else { chrome / 1_000_000 - WINDOWS_EPOCH }
}

/// The profile's bookmarks (`Bookmarks`), every folder's.
pub fn read_bookmarks(profile: &Path) -> Result<Vec<Place>, String> {
    let path = profile.join("Bookmarks");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Couldn't read the bookmarks: {error}")),
    };
    let file: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("Couldn't read the bookmarks: {error}"))?;
    fn walk(node: &Value, out: &mut Vec<Place>) {
        match node.get("type").and_then(Value::as_str) {
            Some("url") => {
                let url = node.get("url").and_then(Value::as_str).unwrap_or_default();
                if web_address(url) {
                    let added =
                        node.get("date_added").and_then(Value::as_str).and_then(|time| time.parse().ok()).unwrap_or(0);
                    out.push(Place {
                        url: url.into(),
                        title: node.get("name").and_then(Value::as_str).unwrap_or_default().into(),
                        bookmark: true,
                        visits: 0,
                        last: unix_time(added),
                    });
                }
            }
            _ => {
                node.get("children").and_then(Value::as_array).into_iter().flatten().for_each(|child| walk(child, out))
            }
        }
    }
    let mut out = Vec::new();
    for root in file.get("roots").and_then(Value::as_object).into_iter().flat_map(|roots| roots.values()) {
        walk(root, &mut out);
    }
    Ok(out)
}

/// The profile's history (`History`), newest first, read from a copy in `scratch` that's removed after.
pub fn read_history(profile: &Path, scratch: &Path) -> Result<Vec<Place>, String> {
    let path = profile.join("History");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(scratch).map_err(|error| format!("Couldn't read the history: {error}"))?;
    let copy = scratch.join(format!("history-{}.sqlite", uuid::Uuid::new_v4()));
    let read = (|| {
        std::fs::copy(&path, &copy).map_err(|error| format!("Couldn't read the history: {error}"))?;
        // Pages not yet checkpointed are in its WAL: that's copied beside the copy, so the browser's own files are
        // never opened (opening them can make their shared-memory file).
        let wal = profile.join("History-wal");
        if wal.is_file() {
            std::fs::copy(wal, format!("{}-wal", copy.display()))
                .map_err(|error| format!("Couldn't copy the history journal: {error}"))?;
        }
        let db = rusqlite::Connection::open_with_flags(
            &copy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("Couldn't open the history: {error}"))?;
        let mut query = db
            .prepare("SELECT url, title, visit_count, last_visit_time FROM urls WHERE hidden = 0 ORDER BY last_visit_time DESC LIMIT ?1")
            .map_err(|error| format!("Couldn't read the history: {error}"))?;
        let rows = query
            .query_map([MAX_HISTORY as i64], |row| {
                Ok(Place {
                    url: row.get(0)?,
                    title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    bookmark: false,
                    visits: row.get::<_, i64>(2)?.clamp(0, u32::MAX as i64) as u32,
                    last: unix_time(row.get(3)?),
                })
            })
            .map_err(|error| format!("Couldn't read the history: {error}"))?;
        Ok(rows.filter_map(Result::ok).filter(|place| web_address(&place.url)).collect())
    })();
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", copy.display()));
    }
    read
}

pub fn load_places(local_dir: &Path) -> Vec<Place> {
    std::fs::read(local_dir.join(PLACES)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// What an import brought in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Brought {
    pub bookmarks: usize,
    pub pages: usize,
}

/// Reads the profile's bookmarks and history and merges them into the address bar's places.
pub fn import_places(profile: &Profile, local_dir: &Path) -> Result<Brought, String> {
    let bookmarks = read_bookmarks(&profile.path)?;
    let history = read_history(&profile.path, &local_dir.join("tmp"))?;
    let brought = Brought { bookmarks: bookmarks.len(), pages: history.len() };
    let places = merge(load_places(local_dir), bookmarks.into_iter().chain(history));
    let json = serde_json::to_vec(&places).map_err(|error| error.to_string())?;
    crate::files::write_atomic(&local_dir.join(PLACES), &json)
        .map_err(|error| format!("Couldn't save them: {error}"))?;
    Ok(brought)
}

/// `kept` and `new` as one list, an address once: a bookmark either way, the most visits, the latest visit, a title.
fn merge(kept: Vec<Place>, new: impl IntoIterator<Item = Place>) -> Vec<Place> {
    let mut places: Vec<Place> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for place in kept.into_iter().chain(new) {
        match at.get(&place.url) {
            Some(&index) => {
                let old = &mut places[index];
                old.bookmark |= place.bookmark;
                old.visits = old.visits.max(place.visits);
                old.last = old.last.max(place.last);
                if !place.title.is_empty() && (old.title.is_empty() || place.bookmark) {
                    old.title = place.title;
                }
            }
            None => {
                at.insert(place.url.clone(), places.len());
                places.push(place);
            }
        }
    }
    places.sort_by(|a, b| b.bookmark.cmp(&a.bookmark).then(b.last.cmp(&a.last)));
    places.truncate(MAX_PLACES);
    places
}

/// An address without its scheme and `www.`, for matching what's typed.
fn bare(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_prefix("www.").unwrap_or(rest)
}

/// The places that match what's typed, best first: every word in the address or title; bookmarks, addresses that
/// start with it, and often and lately visited ones first.
pub fn suggest<'a>(places: &'a [Place], typed: &str, now: i64, limit: usize) -> Vec<&'a Place> {
    let typed = typed.trim().to_lowercase();
    let words: Vec<&str> = typed.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let start = bare(&typed);
    let mut found: Vec<(f64, &Place)> = places
        .iter()
        .filter_map(|place| {
            let url = place.url.to_lowercase();
            let title = place.title.to_lowercase();
            if !words.iter().all(|word| url.contains(word) || title.contains(word)) {
                return None;
            }
            let days = ((now - place.last).max(0) / 86_400) as f64;
            let mut score = (1. + place.visits as f64).ln() * 20. + (60. - days).max(0.);
            if place.bookmark {
                score += 150.;
            }
            if bare(&url).starts_with(start) {
                score += 400.;
            } else if title.starts_with(&typed) {
                score += 120.;
            }
            Some((score, place))
        })
        .collect();
    found.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.url.len().cmp(&b.1.url.len())));
    found.into_iter().take(limit).map(|(_, place)| place).collect()
}

/// A saved login. No `Debug`, so a password can't end up in a log or an error.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Login {
    /// The address it was saved for.
    pub url: String,
    pub username: String,
    pub password: String,
}

impl Login {
    fn host(&self) -> String {
        host_of(&self.url)
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_lowercase)).unwrap_or_default()
}

/// A CSV's rows (quoted fields, doubled quotes, newlines inside quotes), blank ones left out.
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|row| row.iter().any(|field| !field.trim().is_empty()));
    rows
}

/// The logins in a passwords CSV: Chrome's and Helium's (`name,url,username,password,note`), Safari's (`Title,URL,
/// Username,Password,…`) or Firefox's (`"url","username","password",…`). Rows for apps (android://) and without a web
/// address or password are left out.
pub fn parse_passwords_csv(text: &str) -> Result<Vec<Login>, String> {
    let mut rows = csv_rows(text).into_iter();
    let header: Vec<String> = rows.next().unwrap_or_default().iter().map(|name| name.trim().to_lowercase()).collect();
    let column = |names: &[&str]| header.iter().position(|name| names.contains(&name.as_str()));
    let (Some(url), Some(username), Some(password)) = (
        column(&["url", "origin", "website", "login_uri"]),
        column(&["username", "login", "login_username"]),
        column(&["password", "login_password"]),
    ) else {
        return Err("That isn't a passwords file: its first line should name the url, username and password columns. Export it again from the browser's password manager.".into());
    };
    Ok(rows
        .filter_map(|row| {
            let field = |index: usize| row.get(index).map(|value| value.trim().to_string()).unwrap_or_default();
            let login = Login {
                url: field(url),
                username: field(username),
                password: row.get(password).cloned().unwrap_or_default(),
            };
            (web_address(&login.url) && !login.host().is_empty() && !login.password.is_empty()).then_some(login)
        })
        .collect())
}

/// Where the logins are kept: the Keychain, or `FILO_SECRETS_DIR` for checks.
pub fn secrets() -> Secrets {
    Secrets::system_for(LOGINS_SERVICE)
}

/// The account's name in `secrets`: its own in a `FILO_SECRETS_DIR` shared with the apps'.
fn account(secrets: &Secrets) -> &'static str {
    if matches!(secrets, Secrets::Keychain { .. }) { LOGINS_ACCOUNT } else { "browser-logins" }
}

pub fn load_logins(secrets: &Secrets) -> Result<Vec<Login>, String> {
    match secrets.get(account(secrets))? {
        // serde's error can quote what it read.
        Some(json) => serde_json::from_str(&json).map_err(|_| "Couldn't read the saved logins.".to_string()),
        None => Ok(Vec::new()),
    }
}

/// Saves `new` with the logins kept: one per site and username, the newer one's password.
pub fn save_logins(secrets: &Secrets, new: Vec<Login>) -> Result<usize, String> {
    let mut logins = load_logins(secrets)?;
    for login in new {
        let key = (login.host(), login.username.clone());
        match logins.iter_mut().find(|kept| (kept.host(), kept.username.clone()) == key) {
            Some(kept) => *kept = login,
            None => logins.push(login),
        }
    }
    let json = serde_json::to_string(&logins).map_err(|error| error.to_string())?;
    secrets.set(account(secrets), &json)?;
    Ok(logins.len())
}

pub fn forget_logins(secrets: &Secrets) -> Result<(), String> {
    secrets.delete(account(secrets))
}

/// Saves the logins in the exported `csv`, then deletes the file. Gives how many it had.
pub fn import_passwords(csv: &Path, secrets: &Secrets) -> Result<usize, String> {
    let bytes = std::fs::read(csv).map_err(|error| format!("Couldn't read {}: {error}", csv.display()))?;
    let logins = parse_passwords_csv(&String::from_utf8_lossy(&bytes))?;
    let count = logins.len();
    save_logins(secrets, logins)?;
    std::fs::remove_file(csv).map_err(|error| {
        format!("The logins are saved, but the file couldn't be deleted: {error}. Delete {} yourself.", csv.display())
    })?;
    Ok(count)
}

/// The saved logins for the page at `url`: the same host (`www.` or not), over https unless the login itself was for
/// http.
pub fn logins_for<'a>(logins: &'a [Login], url: &str) -> Vec<&'a Login> {
    let Ok(page) = url::Url::parse(url) else { return Vec::new() };
    let Some(host) = page.host_str().map(str::to_lowercase) else { return Vec::new() };
    let strip = |host: &str| host.strip_prefix("www.").unwrap_or(host).to_string();
    logins
        .iter()
        .filter(|login| {
            let Ok(saved) = url::Url::parse(&login.url) else { return false };
            let secure_enough = page.scheme() == "https" || saved.scheme() == "http";
            secure_enough && saved.host_str().is_some_and(|saved| strip(&saved.to_lowercase()) == strip(&host))
        })
        .collect()
}

/// A Chrome-like profile with a bookmark, a folder's bookmark and some history, for checks: `<root>/Google/Chrome`.
#[cfg(feature = "dev")]
pub fn make_test_profile(root: &Path, site: &str) -> Result<PathBuf, String> {
    let dir = root.join(Source::Chrome.folder());
    let profile = dir.join("Profile 1");
    std::fs::create_dir_all(&profile).map_err(|error| error.to_string())?;
    let state = serde_json::json!({ "profile": { "last_used": "Profile 1", "info_cache": { "Profile 1": { "name": "Check", "user_name": "check@example.com" } } } });
    std::fs::write(dir.join("Local State"), state.to_string()).map_err(|error| error.to_string())?;
    let bookmarks = serde_json::json!({ "roots": {
        "bookmark_bar": { "type": "folder", "name": "Bookmarks bar", "children": [
            { "type": "url", "name": "Example Domain", "url": "https://example.com/", "date_added": "13370000000000000" },
            { "type": "folder", "name": "Work", "children": [ { "type": "url", "name": "Check sign-in", "url": format!("{site}/signin") } ] }
        ] },
        "other": { "type": "folder", "children": [] }
    } });
    std::fs::write(profile.join("Bookmarks"), bookmarks.to_string()).map_err(|error| error.to_string())?;
    let db = rusqlite::Connection::open(profile.join("History")).map_err(|error| error.to_string())?;
    db.execute_batch(
        "CREATE TABLE urls (id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR, visit_count INTEGER DEFAULT 0 NOT NULL, typed_count INTEGER DEFAULT 0 NOT NULL, last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);
         INSERT INTO urls (url, title, visit_count, last_visit_time) VALUES ('https://example.org/reading-list', 'Example reading list', 12, 13400000000000000);
         INSERT INTO urls (url, title, visit_count, last_visit_time) VALUES ('https://news.example.net/', 'Example News', 40, 13400000000000000);
         INSERT INTO urls (url, title, visit_count, last_visit_time, hidden) VALUES ('https://hidden.example.com/', 'Hidden', 1, 13400000000000000, 1);",
    )
    .map_err(|error| error.to_string())?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_and_safari_csvs_give_their_logins() {
        let chrome = "name,url,username,password,note\r\nexample.com,https://example.com/login,ann,\"p,w\"\"1\",\nandroid,android://abc@com.app/,ann,x,\nnone,https://nopass.com/,ann,,\n\"multi\",https://a.b/,bo,\"line1\nline2\",\"a note\"\n";
        let logins = parse_passwords_csv(chrome).unwrap();
        assert_eq!(logins.len(), 2);
        assert!(
            logins[0]
                == Login { url: "https://example.com/login".into(), username: "ann".into(), password: "p,w\"1".into() }
        );
        assert!(logins[1].password == "line1\nline2");
        let safari = "\u{feff}Title,URL,Username,Password,Notes,OTPAuth\nEx,https://ex.com,me,secret,,\n";
        assert_eq!(parse_passwords_csv(safari).unwrap()[0].username, "me");
        assert!(parse_passwords_csv("a,b,c\n1,2,3\n").is_err());
    }

    #[test]
    fn a_login_is_offered_on_its_own_site_only() {
        let logins = vec![
            Login { url: "https://www.example.com/login".into(), username: "ann".into(), password: "1".into() },
            Login { url: "http://intranet.local/".into(), username: "bo".into(), password: "2".into() },
        ];
        assert_eq!(logins_for(&logins, "https://example.com/signin").len(), 1);
        assert_eq!(logins_for(&logins, "https://www.example.com/").len(), 1);
        assert!(logins_for(&logins, "http://example.com/").is_empty(), "not over plain http");
        assert!(logins_for(&logins, "https://example.com.evil.io/").is_empty());
        assert!(logins_for(&logins, "https://evil.io/?example.com").is_empty());
        assert!(logins_for(&logins, "https://sub.example.com/").is_empty());
        assert_eq!(logins_for(&logins, "http://intranet.local/x").len(), 1);
    }

    #[test]
    fn logins_merge_by_site_and_username() {
        let secrets = Secrets::memory();
        save_logins(&secrets, vec![Login { url: "https://a.com/".into(), username: "x".into(), password: "1".into() }])
            .unwrap();
        let count = save_logins(
            &secrets,
            vec![
                Login { url: "https://a.com/login".into(), username: "x".into(), password: "2".into() },
                Login { url: "https://a.com/".into(), username: "y".into(), password: "3".into() },
            ],
        )
        .unwrap();
        assert_eq!(count, 2);
        let logins = load_logins(&secrets).unwrap();
        assert!(logins.iter().find(|login| login.username == "x").unwrap().password == "2");
    }

    #[test]
    fn a_profile_s_bookmarks_and_history_become_places_and_the_csv_goes() {
        let dir = crate::test_support::temp_dir("browser-import");
        let profile_dir = make_test_profile(&dir, "http://127.0.0.1:9").unwrap();
        let before = std::fs::read(profile_dir.join("History")).unwrap();
        let profile =
            Profile { source: Source::Chrome, name: "Check".into(), email: String::new(), path: profile_dir.clone() };
        let local = dir.join("local");
        let brought = import_places(&profile, &local).unwrap();
        assert_eq!(brought, Brought { bookmarks: 2, pages: 2 });
        assert_eq!(std::fs::read(profile_dir.join("History")).unwrap(), before, "the browser's file is untouched");
        assert_eq!(std::fs::read_dir(local.join("tmp")).unwrap().count(), 0, "the copy is gone");
        // Again: nothing doubles.
        import_places(&profile, &local).unwrap();
        let places = load_places(&local);
        assert_eq!(places.len(), 4);
        let now = 13_400_000_000 - WINDOWS_EPOCH;
        let found = suggest(&places, "exa", now, 8);
        assert_eq!(found[0].url, "https://example.com/", "a bookmark whose address starts with it first");
        assert_eq!(suggest(&places, "reading list", now, 8)[0].url, "https://example.org/reading-list");
        assert!(suggest(&places, "hidden", now, 8).is_empty());

        let csv = dir.join("Chrome Passwords.csv");
        std::fs::write(
            &csv,
            "name,url,username,password,note\n127.0.0.1,http://127.0.0.1:9/signin,check-user,check-pass,\n",
        )
        .unwrap();
        let secrets = Secrets::memory();
        assert_eq!(import_passwords(&csv, &secrets).unwrap(), 1);
        assert!(!csv.exists());
        assert!(logins_for(&load_logins(&secrets).unwrap(), "http://127.0.0.1:9/signin")[0].password == "check-pass");
    }

    #[test]
    fn history_in_its_wal_is_read_and_the_profile_s_files_stay_as_they_were() {
        let dir = crate::test_support::temp_dir("browser-import-wal");
        let profile = make_test_profile(&dir, "http://127.0.0.1:9").unwrap();
        let db = rusqlite::Connection::open(profile.join("History")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO urls (url, title, visit_count, last_visit_time) VALUES ('https://wal.example/', 'WAL', 1, 13400000000000000);").unwrap();
        let before = std::fs::read(profile.join("History-wal")).unwrap();
        let places = read_history(&profile, &dir.join("scratch")).unwrap();
        assert!(places.iter().any(|place| place.url == "https://wal.example/"));
        assert_eq!(std::fs::read(profile.join("History-wal")).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.join("scratch")).unwrap().count(), 0);
    }

    #[test]
    fn profiles_come_from_local_state() {
        let dir = crate::test_support::temp_dir("browser-profiles");
        let chrome = dir.join("Google/Chrome");
        for folder in ["Default", "Profile 2"] {
            std::fs::create_dir_all(chrome.join(folder)).unwrap();
        }
        let state = serde_json::json!({ "profile": { "last_used": "Profile 2", "info_cache": { "Default": { "name": "You" }, "Profile 2": { "name": "Work", "user_name": "w@x.io" }, "Gone": { "name": "Gone" } } } });
        std::fs::write(chrome.join("Local State"), state.to_string()).unwrap();
        // SAFETY: only this test reads the variable, in a `dev` build.
        unsafe { std::env::set_var("FILO_BROWSER_IMPORT_ROOT", &*dir) };
        let found = profiles(Source::Chrome);
        unsafe { std::env::remove_var("FILO_BROWSER_IMPORT_ROOT") };
        assert_eq!(found.iter().map(|profile| profile.name.as_str()).collect::<Vec<_>>(), ["Work", "You"]);
        assert_eq!(found[0].email, "w@x.io");
    }
}

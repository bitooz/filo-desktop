// `FILO_OPEN=browser-import-check` (`dev` builds, on a data folder of its own): bringing a profile into Filo's browser,
// end to end in the browser window. `FILO_BROWSER_IMPORT_ROOT` names a folder that stands in for Application Support;
// the check makes a Chrome profile there (filo-local browser_import.rs `make_test_profile`), imports its bookmarks and
// history from the import panel, then a passwords CSV it writes, and checks that the file is gone, that nothing in
// the profile changed, that typing in the address bar suggests what came in, and that the test site's sign-in page
// (browser/check.rs `/signin`) gets the saved login offered and filled. The logins go to the data folder's own
// `local/test-secrets` (or `FILO_SECRETS_DIR`), never the Keychain, and its WebKit data store is removed at the end
// (`remove_test_store`). Each check prints a line; the process ends with 1
// when any failed. `FILO_FIXTURE_SHOTS=<dir>`: pictures of the panel, the suggestions and the login bar.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{App, AsyncApp};
use serde_json::json;

use super::window::BrowserWindow;

struct Checks {
    failed: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        println!(
            "browser import check: {} {name}: {}",
            if ok { "ok" } else { "FAILED" },
            detail.as_ref().replace('\n', " | ")
        );
        if !ok {
            self.failed += 1;
        }
    }
}

/// Every file in the profile with what's in it, to see that nothing there changed.
fn fingerprint(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(fingerprint(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            files.push((path, bytes));
        }
    }
    files.sort();
    files
}

pub fn open(cx: &mut App) {
    let shots = std::env::var("FILO_FIXTURE_SHOTS").ok().filter(|dir| !dir.is_empty());
    let port = super::check::serve();
    let site = format!("http://127.0.0.1:{port}");
    cx.spawn(async move |cx| {
        let mut checks = Checks { failed: 0 };
        run(cx, &mut checks, &site, shots.as_deref()).await;
        println!(
            "browser-import-check: {}",
            if checks.failed == 0 { "all checks passed".to_string() } else { format!("{} failed", checks.failed) }
        );
        crate::window_shot::flush(cx.background_executor()).await;
        super::remove_test_store(cx).await;
        cx.update(|_| super::quit()).ok();
        std::process::exit(if checks.failed == 0 { 0 } else { 1 });
    })
    .detach();
}

async fn sleep(cx: &AsyncApp, ms: u64) {
    cx.background_executor().timer(Duration::from_millis(ms)).await;
}

/// Waits up to `seconds` for `done` to say yes.
async fn until(cx: &mut AsyncApp, seconds: u64, mut done: impl FnMut(&mut App) -> bool) -> bool {
    for _ in 0..seconds * 10 {
        if cx.update(|cx| done(cx)).unwrap_or(false) {
            return true;
        }
        sleep(cx, 100).await;
    }
    false
}

async fn shot(cx: &AsyncApp, dir: Option<&str>, name: &str) {
    if let Some(dir) = dir {
        sleep(cx, 600).await;
        crate::window_shot::save_largest(format!("{dir}/{name}")).await;
    }
}

async fn run(cx: &mut AsyncApp, checks: &mut Checks, site: &str, shots: Option<&str>) {
    let Some(root) = std::env::var_os("FILO_BROWSER_IMPORT_ROOT").map(PathBuf::from) else {
        checks.check("FILO_BROWSER_IMPORT_ROOT names the stand-in for Application Support", false, "it isn't set");
        return;
    };
    let profile = match filo_local::browser_import::make_test_profile(&root, site) {
        Ok(profile) => profile,
        Err(error) => return checks.check("the test profile", false, error),
    };
    let before = fingerprint(&root);

    // The panel, from the browser window's toolbar.
    let opened = cx.update(|cx| {
        super::window::open(None, cx);
        BrowserWindow::open_view(cx)
    });
    let Ok(Some((handle, view))) = opened else { return checks.check("the browser window opens", false, "") };
    let toggled = handle.update(cx, |_, window, cx| view.update(cx, |view, cx| view.toggle_import(window, cx)));
    let panel = cx.update(|cx| view.read(cx).import_panel()).ok().flatten();
    let Some(panel) = panel.filter(|_| toggled.is_ok()) else {
        return checks.check("the import panel opens", false, "");
    };
    sleep(cx, 500).await;
    shot(cx, shots, "import-panel.png").await;

    // Bookmarks and history.
    panel.update(cx, |panel, cx| panel.import_places(cx)).ok();
    let done = until(cx, 20, |cx| !matches!(panel.read(cx).status().0.as_str(), "" | "running")).await;
    let places = cx.update(|cx| panel.read(cx).status().0).unwrap_or_default();
    checks.check(
        "bookmarks and history come in",
        done && places.contains("2 bookmarks and 2 pages of history"),
        &places,
    );
    let local = crate::config::config_dir().join("local");
    let kept = filo_local::browser_import::load_places(&local);
    checks.check(
        "they're kept in the local data folder",
        kept.len() == 4,
        format!("{} places in {}", kept.len(), filo_local::browser_import::PLACES),
    );
    let scratch = std::fs::read_dir(local.join("tmp")).map(|dir| dir.count()).unwrap_or(0);
    checks.check("the history's copy is gone", scratch == 0, format!("{scratch} files left in local/tmp"));

    // Passwords, from an exported CSV.
    let csv = crate::config::config_dir().join("Chrome Passwords.csv");
    let wrote = std::fs::write(
        &csv,
        format!(
            "name,url,username,password,note\n127.0.0.1,{site}/signin,check-user,check-pass-123,\nother,https://other.example/,someone,nope,\n"
        ),
    );
    checks.check("the exported CSV is written", wrote.is_ok(), csv.display().to_string());
    panel.update(cx, |panel, cx| panel.import_passwords(csv.clone(), cx)).ok();
    let done = until(cx, 20, |cx| !matches!(panel.read(cx).status().1.as_str(), "" | "running")).await;
    let passwords = cx.update(|cx| panel.read(cx).status().1).unwrap_or_default();
    checks.check("the logins are saved", done && passwords.contains("Saved 2 logins"), &passwords);
    checks.check("the CSV isn't kept", !csv.exists(), csv.display().to_string());
    let store = std::env::var_os("FILO_SECRETS_DIR").map(PathBuf::from).unwrap_or_else(|| local.join("test-secrets"));
    let in_store = std::fs::read_to_string(store.join("browser-logins")).unwrap_or_default();
    checks.check(
        "the logins are in the test run's own store (the Keychain in the app)",
        in_store.contains("check-user"),
        store.display().to_string(),
    );
    shot(cx, shots, "import-done.png").await;
    checks.check(
        "nothing in the browser's profile changed",
        fingerprint(&root) == before,
        profile.display().to_string(),
    );

    // The address bar suggests what came in.
    handle.update(cx, |_, window, cx| view.update(cx, |view, cx| view.toggle_import(window, cx))).ok();
    sleep(cx, 300).await;
    handle.update(cx, |_, window, cx| view.update(cx, |view, cx| view.type_address("exa", window, cx))).ok();
    let shown = until(cx, 5, |cx| !view.read(cx).suggested().0.is_empty()).await;
    let (suggested, _) = cx.update(|cx| view.read(cx).suggested()).unwrap_or_default();
    checks.check(
        "typing suggests the bookmark first, then history",
        shown
            && suggested.first().is_some_and(|url| url == "https://example.com/")
            && suggested.iter().any(|url| url.contains("reading-list")),
        suggested.join(", "),
    );
    view.update(cx, |view, cx| view.pick_next(cx)).ok();
    shot(cx, shots, "address-suggestions.png").await;
    let (_, picked) = cx.update(|cx| view.read(cx).suggested()).unwrap_or_default();
    checks.check("the arrow keys pick one", picked == Some(0), format!("{picked:?}"));

    // The sign-in page gets the saved login offered, and filled with a click.
    let tab = match cx.update(|_| super::open_for_user(&format!("{site}/signin"))) {
        Ok(Ok(tab)) => tab,
        other => {
            return checks.check(
                "the sign-in page opens",
                false,
                format!("{:?}", other.map(|tab| tab.map(|tab| tab.id.clone()))),
            );
        }
    };
    cx.update(|cx| super::window::open(Some(tab.id.clone()), cx)).ok();
    let offered = until(cx, 15, |cx| view.read(cx).offered().is_some()).await;
    let usernames = cx.update(|cx| view.read(cx).offered()).ok().flatten().unwrap_or_default();
    checks.check(
        "the login bar offers the saved login (and only that site's)",
        offered && usernames == ["check-user"],
        format!("{usernames:?}"),
    );
    let (left, _) = cx.update(|cx| view.read(cx).suggested()).unwrap_or_default();
    checks.check("the suggestions close when another page shows", left.is_empty(), left.join(", "));
    shot(cx, shots, "login-offer.png").await;
    handle.update(cx, |_, window, cx| view.update(cx, |view, cx| view.fill("check-user", window, cx))).ok();
    sleep(cx, 800).await;
    let read = super::page(cx, &tab, "read", json!({ "find": "filled:" }))
        .await
        .map(|value| value["text"].as_str().unwrap_or_default().to_string());
    let text = read.unwrap_or_else(|error| error);
    checks.check(
        "Fill puts the username and password in",
        text.contains("filled: user=check-user pass=right"),
        text.lines().find(|line| line.contains("filled:")).unwrap_or(&text),
    );
    checks.check(
        "the bar goes once it's filled",
        cx.update(|cx| view.read(cx).offered().is_none()).unwrap_or(false),
        "",
    );
    shot(cx, shots, "login-filled.png").await;
}

// Sharing an artifact as a link (the artifact page's Share, screens/artifact.rs): its current page goes to Filo's
// server (which isn't part of this repository), which shows it at
// filo.im/artifacts/<id> to anyone with the link, unlisted and not indexed, sandboxed as the app sandboxes it: its
// scripts run, in a frame with no origin of filo.im's. It goes the way a coding result does (result_share.rs `form`,
// `send`, `replace`, `delete`), with the token the share answered kept here: sharing again after the artifact changed
// puts its new version at the same link, and Unshare deletes it there. `canvasArtifacts:share` and
// `canvasArtifacts:unshare` (artifacts.rs) call these, and `canvasArtifacts:get` carries the link while it's shared.
//
// What goes is the page as it stands alone, the copy claude.ai gets (artifact_share.rs `share_page`): one with Filo's
// `<!--filo:head-->` marker gets Filo's Default theme there, light or dark with the viewer's, and the web pictures
// saved for it go in it (filo_local artifact_images.rs `inline`). It's the user's own page,
// shown as it is, so nothing in it is rewritten; but it's looked over first (`artifact_share_preview`), and the page
// asks before it goes, saying what in it looks private.
//
//   local/artifacts/<id>/filo-link.json   the link, the version it shows, and the token that updates or unshares it

use std::path::PathBuf;

use filo_local::artifact_images;
use filo_local::artifact_share;
use filo_local::artifacts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::result_share::{self, Share};
use super::{Local, now_ms};

/// In the artifact's folder, beside its claude.ai link (artifact_share.rs `claude-link.json`).
const LINK_FILE: &str = "filo-link.json";

/// `local/artifacts/<id>/filo-link.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    #[serde(flatten)]
    pub share: Share,
    /// The artifact's version the link shows.
    pub version: i64,
    /// When that version went (ms since the epoch).
    pub updated_at: i64,
}

impl Local {
    fn link_file(&self, id: &str) -> Result<PathBuf, String> {
        Ok(artifacts::artifact_dir(&self.artifacts_dir(), id)?.join(LINK_FILE))
    }

    /// The artifact's link, while it's shared.
    pub(super) fn artifact_link(&self, id: &str) -> Option<Link> {
        std::fs::read(self.link_file(id).ok()?).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// The artifact's current page with the web pictures saved for it in it.
    fn page_with_pictures(&self, id: &str) -> Result<String, String> {
        let dir = self.artifacts_dir();
        let (_, page) = artifacts::document(self.db()?, &dir, id, None)?
            .ok_or("This artifact's page isn't on this Mac any more.")?;
        Ok(artifact_images::inline(&page, &artifacts::artifact_dir(&dir, id)?))
    }

    /// `canvasArtifacts:share`: the artifact's current version to its link on `endpoint`, and `{ url, version }`. Shared
    /// already, the same link: with the new version when it changed since (a link the server no longer has gets a new
    /// one).
    pub(super) fn share_artifact_link(&self, id: &str, endpoint: &str) -> Result<Value, String> {
        let db = self.db()?;
        let found = artifacts::get(db, id)?.ok_or("This artifact isn't there any more.")?;
        let before = self.artifact_link(id);
        if let Some(link) = before.as_ref().filter(|link| link.version == found.current_version) {
            return Ok(json!({ "url": link.share.url, "version": link.version }));
        }
        let page = artifact_share::share_page(&self.page_with_pictures(id)?);
        let fields = json!({ "title": found.title, "version": found.current_version });
        let (form, boundary) = result_share::form(
            "artifact",
            &fields,
            [("page".to_string(), "text/html; charset=utf-8", page.as_bytes())],
        );
        let share = match before {
            Some(link) if result_share::replace(endpoint, &link.share, form.clone(), &boundary)? => link.share,
            _ => result_share::send(endpoint, form, &boundary)?,
        };
        let link = Link { share, version: found.current_version, updated_at: now_ms() };
        let bytes = serde_json::to_vec_pretty(&link).map_err(|error| error.to_string())?;
        crate::config::write_atomic(&self.link_file(id)?, &bytes)
            .map_err(|error| format!("Couldn't keep the link: {error}"))?;
        self.changed();
        Ok(json!({ "url": link.share.url, "version": link.version }))
    }

    /// `canvasArtifacts:sharePreview`: what Share (or Update) would put at the link, looked over first (security review
    /// M2), for the page to ask: how many things in it look like keys or tokens (`keys`: their formats, the secrets
    /// this Mac holds, long random strings outside pictures), paths in someone's home folder (`paths`), and pictures
    /// inside the page itself (`pictures`, which only a person can look over); `updating` when it's shared already.
    pub(super) fn artifact_share_preview(&self, id: &str) -> Result<Value, String> {
        let db = self.db()?;
        artifacts::get(db, id)?.ok_or("This artifact isn't there any more.")?;
        let page = self.page_with_pictures(id)?;
        Ok(
            json!({ "updating": self.artifact_link(id).is_some(), "keys": key_count(&page, &self.known_secrets()), "paths": home_paths(&page), "pictures": page.matches("data:image/").count() }),
        )
    }

    /// `canvasArtifacts:unshare`: deletes the artifact's page on the server, then forgets the link here.
    pub(super) fn unshare_artifact_link(&self, id: &str, endpoint: &str) -> Result<Value, String> {
        let Some(link) = self.artifact_link(id) else { return Ok(Value::Null) };
        result_share::delete(endpoint, &link.share)?;
        std::fs::remove_file(self.link_file(id)?).map_err(|error| format!("Couldn't forget the link: {error}"))?;
        self.changed();
        Ok(Value::Null)
    }
}

/// How many things in `page` look like keys or tokens (report.rs `hide_keys_in`, `hide_long_secrets`), `known` among
/// them. Pictures inside it (`data:` addresses) are left out: their base64 would read as random strings.
fn key_count(page: &str, known: &[String]) -> usize {
    static DATA: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let data = DATA.get_or_init(|| regex::Regex::new(r#"data:[^"'\s)]+"#).expect("a valid pattern"));
    let page = data.replace_all(page, "data:");
    crate::report::hide_keys_in(&page, known).1 + crate::report::hide_long_secrets(&page).1
}

/// How many paths in someone's home folder (`/Users/<name>/…`, which says the Mac's account name) `page` holds.
fn home_paths(page: &str) -> usize {
    static PATHS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let paths = PATHS.get_or_init(|| regex::Regex::new(r"/Users/([A-Za-z0-9._\-]+)/").expect("a valid pattern"));
    paths.captures_iter(page).filter(|caps| &caps[1] != "Shared").count()
}

#[cfg(test)]
mod tests {
    use filo_local::artifacts::{Change, New, Origin};

    use super::*;
    use crate::local_backend::result_share::tests::test_server;

    /// What the page is asked about before it goes: keys (by format, by value, long random strings), home paths and
    /// pictures inside it; a picture's base64 isn't a key.
    #[test]
    fn a_page_is_looked_over_before_it_goes() {
        let picture = format!(
            "data:image/png;base64,{}",
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg".repeat(3)
        );
        let page = format!(
            "<p>Deploy with DB_PASSWORD=Tr0ub4dor-3 from /Users/ann/code/app/.env and plainwords-long</p><img src=\"{picture}\"><img src=\"{picture}\"><p>/Users/Shared/x</p>"
        );
        assert_eq!(
            (key_count(&page, &["plainwords-long".into()]), home_paths(&page), page.matches("data:image/").count()),
            (2, 1, 2)
        );
        assert_eq!(key_count(&format!("<img src='{picture}'>"), &[]), 0);
    }

    /// An artifact shares as its page, its link shows on its page, a change goes to the same link, and Unshare deletes it.
    #[test]
    fn an_artifact_is_shared_updated_and_unshared() {
        let local = Local::open(std::env::temp_dir().join(format!("filo-local-link-{}", uuid::Uuid::new_v4())));
        let db = local.db().unwrap();
        let dir = local.artifacts_dir();
        let page = "<!doctype html><html><head><!--filo:head--><title>Plan</title></head><body><script>document.body.append('ran')</script></body></html>";
        let new = New { title: "Plan".into(), html: page.into(), summary: String::new(), kind: Some("tool".into()) };
        let artifact = artifacts::create(db, &dir, &new, &Origin::default(), 1).unwrap();
        let id = artifact.id.clone();
        let args = json!({ "id": id });

        // Asked first: nothing in it looks private.
        assert_eq!(
            local.call("canvasArtifacts:sharePreview", &args).unwrap(),
            json!({ "updating": false, "keys": 0, "paths": 0, "pictures": 0 })
        );
        let created = r#"{"id":"7f1e","url":"https://filo.im/artifacts/7f1e","token":"secret-token"}"#;
        let (url, seen) = test_server(
            "artifacts",
            vec![(201, created), (200, r#"{"id":"7f1e","url":"https://filo.im/artifacts/7f1e"}"#), (204, "")],
        );
        assert_eq!(
            local.share_artifact_link(&id, &url).unwrap(),
            json!({ "url": "https://filo.im/artifacts/7f1e", "version": 1 })
        );
        let sent = seen.lock().unwrap()[0].clone();
        assert!(sent.starts_with("POST /v1/artifacts ") && sent.contains("multipart/form-data; boundary="), "{sent}");
        assert!(sent.contains(r#"{"title":"Plan","version":1}"#), "{sent}");
        // The page as it stands alone: Filo's Default theme at its marker, its script as it is.
        assert!(sent.contains("name=\"page\"; filename=\"page\"\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<!doctype html><html><head><style>:root{--background:"), "{sent}");
        assert!(sent.contains("<script>document.body.append('ran')</script>"), "{sent}");
        let shown = local.call("canvasArtifacts:get", &args).unwrap();
        assert_eq!(
            (shown["link"]["url"].as_str(), shown["link"]["version"].as_i64()),
            (Some("https://filo.im/artifacts/7f1e"), Some(1))
        );
        assert!(shown["link"]["token"].is_null(), "the page needn't see the token");

        // Unchanged, sharing again only gives the link.
        local.share_artifact_link(&id, &url).unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        // Changed, the new version goes to the same link.
        let change = Change { html: Some(page.replace("ran", "ran again")), ..Change::default() };
        artifacts::update(db, &dir, &id, &change, &Origin::default(), 2).unwrap();
        assert_eq!(local.call("canvasArtifacts:sharePreview", &args).unwrap()["updating"], true);
        assert_eq!(
            local.share_artifact_link(&id, &url).unwrap(),
            json!({ "url": "https://filo.im/artifacts/7f1e", "version": 2 })
        );
        let updated = seen.lock().unwrap()[1].clone();
        assert!(
            updated.starts_with("PUT /v1/artifacts/7f1e ")
                && updated.to_ascii_lowercase().contains("authorization: bearer secret-token"),
            "{updated}"
        );
        assert!(updated.contains(r#""version":2"#) && updated.contains("ran again"), "{updated}");

        local.unshare_artifact_link(&id, &url).unwrap();
        let deleted = seen.lock().unwrap()[2].clone();
        assert!(
            deleted.starts_with("DELETE /v1/artifacts/7f1e ")
                && deleted.to_ascii_lowercase().contains("authorization: bearer secret-token"),
            "{deleted}"
        );
        assert!(local.call("canvasArtifacts:get", &args).unwrap()["link"].is_null());
        std::fs::remove_dir_all(&local.root).ok();
    }
}

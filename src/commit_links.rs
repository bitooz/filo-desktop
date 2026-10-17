// Commit hashes in chat messages open on GitHub. gpui-component's `text::commits` draws a hash as a `commit:<hash>`
// link, since the text doesn't say which repo it's from; this handler finds out. A click asks once for the user's
// projects (`coding:projects`: each repo the runners found, its path and remote), looks for the commit in the ones on
// this Mac that live on GitHub, and opens it there once it's pushed (a remote branch has it). A hash that's only on
// this Mac, or that none of them has, goes on the clipboard instead, with the app's "Copied" toast.

use std::path::Path;
use std::process::{Command, Stdio};

use gpui::{App, AppContext as _, ClipboardItem, Window};
use serde::Deserialize;
use serde_json::json;

use crate::backend::backend;

#[derive(Deserialize)]
struct Project {
    path: String,
    remote: Option<String>,
}

/// Sends the `commit:` links every text view opens here, and the group chat's @names (`mention:` links) to the
/// conversation they're in.
pub fn init(cx: &mut App) {
    gpui_component::text::safety::set_link_handler(cx, |url, window, cx| {
        let Some(hash) = gpui_component::text::commits::commit_of(url) else {
            return crate::screens::chat::conversation::open_mention(url, cx);
        };
        open(hash.to_string(), window, cx);
        true
    });
}

fn open(hash: String, window: &mut Window, cx: &mut App) {
    let projects = backend(cx).call_raw("coding:projects", json!({}));
    let background = cx.background_executor().clone();
    let window = window.window_handle();
    cx.spawn(async move |cx| {
        // No projects (or the database couldn't say): the hash is copied.
        let projects: Vec<Project> =
            projects.await.ok().and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
        let page = background.spawn({
            let hash = hash.clone();
            async move { commit_page(&hash, &projects) }
        });
        match page.await {
            Some(page) => cx.update(|cx| cx.open_url(&page)).ok(),
            None => cx
                .update_window(window, |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(hash));
                    crate::ui::toast(window, cx, "Copied");
                })
                .ok(),
        };
    })
    .detach();
}

/// The GitHub page for commit `hash`: in the first of `projects` here that has it and has pushed it. None when it's
/// only on this Mac (no GitHub remote, or not pushed yet) or no repo here has it.
fn commit_page(hash: &str, projects: &[Project]) -> Option<String> {
    projects.iter().find_map(|project| {
        let repo = project.remote.as_deref().and_then(github_repo)?;
        if !Path::new(&project.path).is_dir() {
            return None;
        }
        let full = local_commit(&project.path, hash)?;
        pushed(&project.path, &full).then(|| format!("{repo}/commit/{full}"))
    })
}

/// The full hash of commit `hash` in the git repo at `path`, if it has one (a short hash two commits share: none).
fn local_commit(path: &str, hash: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", path, "rev-parse", "--verify", "--quiet", &format!("{hash}^{{commit}}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let full = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && gpui_component::text::commits::is_hash(&full)).then_some(full)
}

/// Whether a remote branch in the repo at `path` has commit `full`, as of its last fetch or push.
fn pushed(path: &str, full: &str) -> bool {
    Command::new("git")
        .args(["-C", path, "for-each-ref", "--count=1", "--format=%(refname)", "--contains", full, "refs/remotes"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.trim_ascii().is_empty())
}

/// The repo's page on GitHub (`https://github.com/<owner>/<repo>`) from its git remote: `git@github.com:o/r.git`,
/// `https://github.com/o/r`, `ssh://git@github.com/o/r.git`, with or without a user or token. None for another host.
pub fn github_repo(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/');
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let path = match remote.strip_prefix("git@github.com:") {
        Some(path) => path,
        None => {
            let (authority, path) = remote.split_once("://")?.1.split_once('/')?;
            let host = authority.rsplit('@').next()?.split(':').next()?;
            if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
                return None;
            }
            path
        }
    };
    let (owner, repo) = path.split_once('/')?;
    let name = |part: &str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (name(owner) && name(repo)).then(|| format!("https://github.com/{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::{Project, commit_page, github_repo};

    #[test]
    fn hashes_are_whole_words_with_digits_and_letters() {
        use gpui_component::text::commits::{commit_of, hash_ranges, is_hash};
        let words = |text: &str| hash_ranges(text).into_iter().map(|range| text[range].to_string()).collect::<Vec<_>>();
        assert_eq!(words("Pushed fd806ce6 and b2bc0a22, then 8d40428f."), ["fd806ce6", "b2bc0a22", "8d40428f"]);
        // Numbers, hex words, ids, paths, versions and file names aren't commits.
        assert!(words("1234567 defaced a/8d40428f v1-8d40428f #8d40428f 8d40428f.txt ABCDEF12 fd806c").is_empty());
        assert!(is_hash("c2ceda6c") && is_hash("c2ceda6") && !is_hash("c2ceda"));
        assert_eq!(commit_of("commit:fd806ce6"), Some("fd806ce6"));
        assert_eq!(commit_of("commit:nothex!"), None);
        assert_eq!(commit_of("https://example.com"), None);
    }

    #[test]
    fn hashes_in_a_message_are_drawn_as_links() {
        use gpui_component::text::safety::markdown_targets;
        let style = || crate::ui::markdown_style().commit_links(true);
        assert_eq!(
            markdown_targets("Pushed `fd806ce6` and b2bc0a22.", style()),
            ["link:commit:fd806ce6", "link:commit:b2bc0a22"]
        );
        // In bold, beside a link, and not inside one or in a code block.
        assert_eq!(
            markdown_targets(
                "**Fixed in fd806ce6** see [b2bc0a22](https://example.com/x)\n\n```\nc2ceda6c\n```",
                style()
            ),
            ["link:commit:fd806ce6", "link:https://example.com/x"]
        );
        // Off by default: other text views draw hashes as text.
        assert!(markdown_targets("Pushed fd806ce6.", crate::ui::markdown_style()).is_empty());
    }

    #[test]
    fn github_remotes_read_as_their_page() {
        for remote in [
            "git@github.com:acme/notes-app.git",
            "https://github.com/acme/notes-app",
            "https://github.com/acme/notes-app.git/",
            "https://bob:s3cret@github.com/acme/notes-app.git",
            "ssh://git@github.com:22/acme/notes-app.git",
        ] {
            assert_eq!(github_repo(remote).as_deref(), Some("https://github.com/acme/notes-app"), "{remote}");
        }
        for remote in [
            "git@gitlab.com:a/b.git",
            "https://example.com/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "/Users/me/repo",
            "",
        ] {
            assert_eq!(github_repo(remote), None, "{remote}");
        }
    }

    #[test]
    fn a_commit_opens_in_the_repo_that_has_it() {
        // A repo of its own with one commit (the checks run on a copy with no .git).
        let root = std::env::temp_dir().join(format!("filo-commit-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let head = git(&["rev-parse", "HEAD"]);
        let path = root.to_string_lossy().to_string();
        let projects = vec![
            Project { path: "/nowhere/at/all".into(), remote: Some("git@github.com:a/elsewhere.git".into()) },
            Project { path: path.clone(), remote: Some("git@github.com:acme/notes-app.git".into()) },
        ];
        // Not pushed yet: nothing to open.
        assert_eq!(commit_page(&head[..8], &projects), None);
        // Pushed (a remote branch has it): its page.
        git(&["update-ref", "refs/remotes/origin/main", &head]);
        assert_eq!(
            commit_page(&head[..8], &projects),
            Some(format!("https://github.com/acme/notes-app/commit/{head}"))
        );
        // A commit after the push is only here.
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        assert_eq!(commit_page(&git(&["rev-parse", "--short=8", "HEAD"]), &projects), None);
        // One no repo here has, and one in a repo with no GitHub remote: nothing.
        assert_eq!(commit_page("0000000a", &projects), None);
        let local_only = vec![Project { path, remote: None }];
        assert_eq!(commit_page(&head[..8], &local_only), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}

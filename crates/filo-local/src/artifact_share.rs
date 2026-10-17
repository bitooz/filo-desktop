// Sharing an artifact to claude.ai, an optional setting (the Artifacts tab's settings): Filo's artifacts stay on this
// Mac as before, and when "Share to claude.ai" is on, Claude Code can publish one to the user's claude.ai account
// with its own Artifact tool, when the user asks. Only while Claude Code is signed in to claude.ai (an account, not an
// API key): the page goes to that account, private until the user shares it there.
//
// A Claude Code chat turn gets the tool while it's on (`enable`: the undocumented `CLAUDE_CODE_ARTIFACT=1`, which
// headless Claude Code needs for it, and `Artifact` in `--tools` and `--allowedTools`), with a line in its
// instructions to use it only when asked (`CAPABILITY`). Codex never gets it, and with the setting off nothing changes.
// The turn's events show each publish (`Watcher`); one of an artifact's page saves its link beside it
// (`<artifacts>/<id>/claude-link.json`), which the artifact's page shows. Its Share button does the same through a
// short Claude Code run of its own (`publish`). Publishing the same file again makes a new version at the same link.
//
// What goes to claude.ai is a copy of the page that stands alone (`share_page`): most of Filo's pages take their
// colours and type from the app, which puts its theme in at their `<!--filo:head-->` marker, so the copy gets Filo's
// Default theme there instead, light or dark with the viewer's (glance_document.rs `theme_css`), and the web pictures
// saved for it in it (artifact_images.rs `inline`). While sharing is on,
// it's kept up to date as the page changes (artifacts.rs, `page_changed`) and at each turn (`refresh_copies`).
//
//   <root>/artifact-share.json   the switch
//   <artifacts>/<id>/claude/index.html   the copy that's published
//   <artifacts>/<id>/claude-link.json   the artifact's claude.ai link, and which version went there

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::artifacts;
use crate::db::Db;
use crate::engine::{Engine, Invocation};

/// The switch, in the local folder (beside the database).
pub const SETTINGS_FILE: &str = "artifact-share.json";
/// An artifact's claude.ai link, in its folder.
const LINK_FILE: &str = "claude-link.json";
/// The folder of the copy that's published, in the artifact's.
pub const COPY_DIR: &str = "claude";
/// Claude Code's tool, and what turns it on in a headless run (without it, `--tools Artifact` is quietly dropped).
const TOOL: &str = "Artifact";
const ENV: (&str, &str) = ("CLAUDE_CODE_ARTIFACT", "1");
/// How long the Share button's run may take.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// `local/artifact-share.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// "Share to claude.ai" is on. Off by default.
    #[serde(default)]
    pub share_to_claude: bool,
}

impl Settings {
    /// The settings, the defaults when the file is missing or can't be read.
    pub fn read(root: &Path) -> Settings {
        crate::files::read_json(&root.join(SETTINGS_FILE)).ok().flatten().unwrap_or_default()
    }
}

/// The claude.ai account Claude Code is signed in to (its email), from the account it keeps in `~/.claude.json`. None
/// when it isn't, or only has an API key, which has no claude.ai to publish to.
pub fn claude_ai_account() -> Option<String> {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()).map(PathBuf::from);
    let file = config_dir.map_or_else(
        || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".claude.json"),
        |dir| dir.join(".claude.json"),
    );
    let config: Value =
        std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
    claude_ai_account_in(&config)
}

fn claude_ai_account_in(config: &Value) -> Option<String> {
    config["oauthAccount"]["emailAddress"].as_str().map(str::trim).filter(|email| !email.is_empty()).map(str::to_string)
}

/// Whether a Claude Code turn starting now may publish: the switch is on and Claude Code is signed in to claude.ai.
pub fn is_on(root: &Path) -> bool {
    Settings::read(root).share_to_claude && claude_ai_account().is_some()
}

/// What a turn's instructions say of it, while it's on: where an artifact's page is, and to publish only when asked.
pub fn capability(artifacts_dir: &Path) -> String {
    format!(
        "Sharing to claude.ai is on: your Artifact tool publishes a page to the user's claude.ai account, private to them until they share it there. \
Use it only when the user asks to share one of Filo's artifacts (to claude.ai, or for a link to it), never on your own, and for nothing else: \
make and change artifacts with Filo's artifact tools as always. To share one, call Artifact with `file_path` set to the copy Filo keeps for claude.ai, \
`{}/<artifact id>/{COPY_DIR}/index.html` (list_artifacts gives the ids), as it is: don't change, copy or rewrite the file and don't load a skill for it. \
Publishing the same file again updates the same link; {OVERWRITE} Filo keeps the link on the artifact; give it to the user in your reply.",
        artifacts_dir.display()
    )
}

/// Updating a link from another session (each turn's, each Share's), the tool refuses a live page it hasn't seen:
/// Filo's page is the one to keep, so it goes over it.
const OVERWRITE: &str = "if Artifact refuses because the live page there changed or has content you haven't read, publish again over it \
(`overwrite_unread` with the paths it names, else `force: true`), since the user asked to share Filo's page and it is the one to keep, unless they say otherwise.";

/// The page as it goes to claude.ai: one with Filo's `<!--filo:head-->` marker gets Filo's Default theme there,
/// light or dark with the viewer's, and a font; a page without it carries its own look and goes as it is.
pub fn share_page(html: &str) -> String {
    const MARKER: &str = "<!--filo:head-->";
    match html.find(MARKER) {
        Some(at) => format!(
            "{}<style>{}html,body{{font-family:system-ui,-apple-system,sans-serif}}</style>{}",
            &html[..at],
            crate::glance_document::theme_css(),
            &html[at..]
        ),
        None => html.to_string(),
    }
}

/// Writes artifact `id`'s copy for claude.ai from its current page (`index.html`), when it isn't that already; its
/// path.
fn refresh_copy(artifacts_dir: &Path, id: &str) -> Result<PathBuf, String> {
    let folder = artifacts::artifact_dir(artifacts_dir, id)?;
    let page = std::fs::read_to_string(folder.join("index.html"))
        .map_err(|_| "This artifact's page isn't on this Mac any more.".to_string())?;
    let copy = folder.join(COPY_DIR).join("index.html");
    let shared = share_page(&crate::artifact_images::inline(&page, &folder));
    if std::fs::read_to_string(&copy).ok().as_deref() != Some(shared.as_str()) {
        crate::files::write_atomic(&copy, shared.as_bytes())
            .map_err(|error| format!("Couldn't write the copy for claude.ai: {error}"))?;
    }
    Ok(copy)
}

/// Every artifact's copy for claude.ai, brought up to date (at a turn's start, while sharing is on).
pub fn refresh_copies(artifacts_dir: &Path) {
    for entry in std::fs::read_dir(artifacts_dir).into_iter().flatten().flatten() {
        if let Some(id) = entry.file_name().to_str().filter(|_| entry.path().join("index.html").is_file()) {
            let _ = refresh_copy(artifacts_dir, id);
        }
    }
}

/// An artifact's page changed (artifacts.rs): its copy for claude.ai follows, while sharing is on.
pub fn page_changed(artifacts_dir: &Path, id: &str) {
    if artifacts_dir.parent().is_some_and(|root| Settings::read(root).share_to_claude) {
        let _ = refresh_copy(artifacts_dir, id);
    }
}

/// Gives a Claude Code turn's command line (engine.rs `invocation`) the Artifact tool: in `--tools` and
/// `--allowedTools`, with the variable that turns it on.
pub fn enable(call: &mut Invocation) {
    let mut args = call.args.iter_mut();
    while let Some(arg) = args.next() {
        if arg == "--tools" {
            if let Some(tools) = args.next() {
                push_tool(tools);
            }
        } else if arg.starts_with("--allowedTools=") {
            push_tool(arg);
        }
    }
    if !call.env.iter().any(|(key, _)| key == ENV.0) {
        call.env.push((ENV.0.into(), ENV.1.into()));
    }
}

fn push_tool(list: &mut String) {
    let tools = list.split_once('=').map_or(list.as_str(), |(_, tools)| tools);
    if !tools.split(',').any(|tool| tool == TOOL) {
        list.push(',');
        list.push_str(TOOL);
    }
}

/// An artifact's claude.ai link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub url: String,
    /// The artifact's version that was published.
    pub version: i64,
    /// When (ms since the epoch).
    pub at: i64,
}

pub fn link(artifacts_dir: &Path, id: &str) -> Option<Link> {
    let dir = artifacts::artifact_dir(artifacts_dir, id).ok()?;
    std::fs::read(dir.join(LINK_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub fn save_link(artifacts_dir: &Path, id: &str, link: &Link) -> Result<(), String> {
    let dir = artifacts::artifact_dir(artifacts_dir, id)?;
    if !dir.is_dir() {
        return Err("This artifact isn't there any more.".into());
    }
    let bytes = serde_json::to_vec_pretty(link).map_err(|error| error.to_string())?;
    crate::files::write_atomic(&dir.join(LINK_FILE), &bytes).map_err(|error| format!("Couldn't save the link: {error}"))
}

/// The claude.ai link in an Artifact tool's result ("Published … at https://claude.ai/artifact/… (Version 1 …)").
fn url_in(text: &str) -> Option<String> {
    let start = text.find("https://claude.ai/")?;
    let url: String = text[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, ')' | '"' | '\'' | '<' | '>' | ',' | '`'))
        .collect();
    let url = url.trim_end_matches('.').to_string();
    url.contains("/artifact/").then_some(url)
}

/// A tool result's text: a string, or its text blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Reads a Claude Code run's events for the pages its Artifact tool published: the file and its link, as each
/// publish's result arrives. Assets, reads and the other actions aren't publishes.
#[derive(Debug, Default)]
pub struct Watcher {
    /// Publishes asked for and not answered yet: the tool call's id and the file.
    pending: HashMap<String, PathBuf>,
}

impl Watcher {
    pub fn line(&mut self, line: &str) -> Vec<(PathBuf, String)> {
        let Ok(event) = serde_json::from_str::<Value>(line) else { return Vec::new() };
        let kind = event["type"].as_str().unwrap_or_default();
        let mut published = Vec::new();
        for block in event.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
            match (kind, block["type"].as_str().unwrap_or_default()) {
                ("assistant", "tool_use") if block["name"] == TOOL => {
                    let input = &block["input"];
                    let action = input["action"].as_str().unwrap_or("publish");
                    let (Some(id), Some(file)) = (block["id"].as_str(), input["file_path"].as_str()) else { continue };
                    if action == "publish" && input["asset"] != true {
                        self.pending.insert(id.to_string(), PathBuf::from(file));
                    }
                }
                ("user", "tool_result") => {
                    let Some(file) = block["tool_use_id"].as_str().and_then(|id| self.pending.remove(id)) else {
                        continue;
                    };
                    if block["is_error"] == true {
                        continue;
                    }
                    if let Some(url) = url_in(&result_text(&block["content"])) {
                        published.push((file, url));
                    }
                }
                _ => {}
            }
        }
        published
    }
}

/// Which artifact a published file is the page of, and which version: its copy for claude.ai or `index.html` (the
/// current one), or a `v<n>.html`, in `artifacts_dir`.
pub fn artifact_of(artifacts_dir: &Path, file: &Path) -> Option<(String, Option<i64>)> {
    // As given, else as both resolve (`/tmp` is `/private/tmp`).
    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let real_file = match (file.parent(), file.file_name()) {
        (Some(parent), Some(name)) => real(parent).join(name),
        _ => file.to_path_buf(),
    };
    let relative = file.strip_prefix(artifacts_dir).or_else(|_| real_file.strip_prefix(real(artifacts_dir))).ok()?;
    let parts: Vec<&str> = relative.iter().map(|part| part.to_str()).collect::<Option<_>>()?;
    let (id, version) = match parts[..] {
        [id, COPY_DIR, "index.html"] | [id, "index.html"] => (id, None),
        [id, name] => (id, Some(name.strip_prefix('v')?.strip_suffix(".html")?.parse().ok()?)),
        _ => return None,
    };
    crate::store::valid_id(id).then(|| (id.to_string(), version))
}

/// Keeps the link of a page a turn published, when it's an artifact's (else nothing). True when one was saved.
pub fn record(db_path: &Path, file: &Path, url: &str) -> bool {
    let dir = artifacts::folder_for(db_path);
    let Some((id, version)) = artifact_of(&dir, file) else { return false };
    let version = match version {
        Some(version) => version,
        None => match Db::open(db_path).ok().and_then(|db| artifacts::get(&db, &id).ok().flatten()) {
            Some(found) => found.current_version,
            None => return false,
        },
    };
    save_link(&dir, &id, &Link { url: url.to_string(), version, at: crate::now_ms() as i64 }).is_ok()
}

/// The command line for the Share button's run: Claude Code with the Artifact tool and nothing else, told to publish
/// `file` (to `url`, when it was shared before), and Read: the tool reads the file it publishes under Read's
/// permission, which a run without Read refuses.
fn publish_invocation(program: &Path, file: &Path, title: &str, url: Option<&str>) -> Invocation {
    let target = match url {
        // The user clicked Share: Filo's page replaces whatever this run hasn't seen there.
        Some(url) => format!(
            " Pass `url`: \"{url}\", its link from before, so the same link gets the new version, and `force: true`: this file replaces what's there."
        ),
        None => String::new(),
    };
    let prompt = format!(
        "Publish the page at {file} to claude.ai: call the Artifact tool once, with `file_path` set to that file and `title` \"{title}\".{target} \
Publish the file exactly as it is: don't read, change or copy it, and don't load a skill. If the tool still refuses because the live page has content you \
haven't read, publish again with `overwrite_unread` listing the paths it names: this file is the version to keep. Then reply with just the link.",
        file = file.display(),
    );
    Invocation {
        program: program.to_path_buf(),
        args: vec![
            "-p".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--strict-mcp-config".into(),
            "--tools".into(),
            format!("{TOOL},Read"),
            format!("--allowedTools={TOOL},Read"),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--no-session-persistence".into(),
            "--model".into(),
            "haiku".into(),
        ],
        stdin: prompt,
        env: vec![(ENV.0.into(), ENV.1.into()), ("CLAUDE_CODE_PROMPT_CACHE_TTL".into(), "5m".into())],
    }
}

/// The Share button: publishes artifact `id`'s current page to claude.ai with a short Claude Code run in its folder,
/// keeps the link beside it and returns it. Blocks until the run ends.
pub fn publish(db_path: &Path, id: &str, path_env: Option<&str>) -> Result<Link, String> {
    if claude_ai_account().is_none() {
        return Err(
            "Claude Code isn't signed in to claude.ai. Sign in with `claude auth login`, then share again.".into()
        );
    }
    let db = Db::open(db_path)?;
    let found = artifacts::get(&db, id)?.ok_or("This artifact isn't there any more.")?;
    let artifacts_dir = artifacts::folder_for(db_path);
    let file = refresh_copy(&artifacts_dir, id)?;
    let folder = artifacts::artifact_dir(&artifacts_dir, id)?;
    let program =
        crate::engine::find_program(Engine::Claude, path_env).ok_or("Claude Code isn't installed on this Mac.")?;
    let before = link(&artifacts_dir, id);
    let call = publish_invocation(&program, &file, &found.title, before.as_ref().map(|link| link.url.as_str()));
    let running = crate::engine::start_invocation(&call, Engine::Claude, &folder, path_env, PUBLISH_TIMEOUT)?;
    let mut watcher = Watcher::default();
    let mut url = None;
    let outcome = running.wait_lines(|line| {
        if let Some((_, published)) = watcher.line(line).into_iter().last() {
            url = Some(published);
        }
    });
    let Some(url) = url else {
        let why = outcome.error.or(outcome.final_text).unwrap_or_else(|| "it ended without publishing".into());
        return Err(format!("Couldn't share it to claude.ai: {why}"));
    };
    let link = Link { url, version: found.current_version, at: crate::now_ms() as i64 };
    save_link(&artifacts_dir, id, &link)?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_a_claude_ai_account_counts() {
        assert_eq!(
            claude_ai_account_in(&json!({ "oauthAccount": { "emailAddress": "me@x.com" } })),
            Some("me@x.com".into())
        );
        assert_eq!(claude_ai_account_in(&json!({ "oauthAccount": { "emailAddress": " " } })), None);
        assert_eq!(claude_ai_account_in(&json!({ "primaryApiKey": "sk-…" })), None);
        assert_eq!(claude_ai_account_in(&Value::Null), None);
    }

    #[test]
    fn the_switch_is_off_by_default() {
        let root = crate::test_support::temp_dir("share");
        assert!(!Settings::read(&root).share_to_claude);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(SETTINGS_FILE), r#"{"shareToClaude":true}"#).unwrap();
        assert!(Settings::read(&root).share_to_claude);
    }

    #[test]
    fn enable_adds_the_tool_and_its_variable_once() {
        let mut call = Invocation {
            program: PathBuf::from("claude"),
            args: vec![
                "-p".into(),
                "--tools".into(),
                "Read,Bash".into(),
                "--allowedTools=mcp__filo,Read,Bash".into(),
                "--model".into(),
                "opus".into(),
            ],
            stdin: String::new(),
            env: Vec::new(),
        };
        enable(&mut call);
        enable(&mut call);
        assert_eq!(
            call.args,
            ["-p", "--tools", "Read,Bash,Artifact", "--allowedTools=mcp__filo,Read,Bash,Artifact", "--model", "opus"]
        );
        assert_eq!(call.env, [("CLAUDE_CODE_ARTIFACT".to_string(), "1".to_string())]);
    }

    #[test]
    fn the_watcher_finds_each_published_page_and_its_link() {
        let mut watcher = Watcher::default();
        let assistant = |id: &str, input: Value| {
            json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": id, "name": "Artifact", "input": input }] } }).to_string()
        };
        let result = |id: &str, content: Value, is_error: bool| {
            json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error }] } }).to_string()
        };
        assert!(watcher.line(&assistant("t1", json!({ "file_path": "/a/index.html", "icon": "chart" }))).is_empty());
        assert!(watcher.line(&assistant("t2", json!({ "action": "list" }))).is_empty());
        assert!(
            watcher
                .line(&assistant(
                    "t3",
                    json!({ "file_path": "/a/pic.png", "asset": true, "url": "https://claude.ai/artifact/x" })
                ))
                .is_empty()
        );
        assert!(watcher.line(&assistant("t4", json!({ "file_path": "/b/index.html" }))).is_empty());
        let text = "Published /a/index.html at https://claude.ai/artifact/3d2FqdRd2c9RdiL17b5i89 (Version 1, version id 1791761855-ffbe) Icon: \"chart\".";
        assert_eq!(
            watcher.line(&result("t1", json!(text), false)),
            [(PathBuf::from("/a/index.html"), "https://claude.ai/artifact/3d2FqdRd2c9RdiL17b5i89".to_string())]
        );
        assert!(watcher.line(&result("t3", json!("Uploaded https://claude.ai/artifact/x"), false)).is_empty());
        assert!(
            watcher
                .line(&result(
                    "t4",
                    json!([{ "type": "text", "text": "Couldn't publish https://claude.ai/artifact/nope" }]),
                    true
                ))
                .is_empty()
        );
        // Not an artifact link.
        assert_eq!(url_in("see https://claude.ai/settings"), None);
        assert_eq!(
            url_in("at https://claude.ai/code/artifact/0b8e.").as_deref(),
            Some("https://claude.ai/code/artifact/0b8e")
        );
    }

    #[test]
    fn a_published_page_is_its_artifacts() {
        let dir = std::env::temp_dir().join(format!("filo-share-{}", uuid::Uuid::new_v4())).join("artifacts");
        std::fs::create_dir_all(dir.join("a1")).unwrap();
        assert_eq!(artifact_of(&dir, &dir.join("a1/index.html")), Some(("a1".into(), None)));
        assert_eq!(artifact_of(&dir, &dir.join("a1/claude/index.html")), Some(("a1".into(), None)));
        assert_eq!(artifact_of(&dir, &dir.join("a1/v3.html")), Some(("a1".into(), Some(3))));
        assert_eq!(artifact_of(&dir, &dir.join("a1/notes.html")), None);
        assert_eq!(artifact_of(&dir, &dir.join("a1/x/index.html")), None);
        assert_eq!(artifact_of(&dir, Path::new("/tmp/elsewhere/index.html")), None);
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn the_copy_for_claude_ai_stands_alone_and_follows_the_page() {
        let themed = share_page(
            "<html><head><!--filo:head--><title>T</title></head><body style=\"color:var(--text)\">x</body></html>",
        );
        assert!(
            themed.starts_with("<html><head><style>:root{--background:")
                && themed.contains("prefers-color-scheme:dark"),
            "{themed}"
        );
        assert!(themed.contains("</style><!--filo:head--><title>T</title>"), "{themed}");
        assert_eq!(share_page("<h1>own look</h1>"), "<h1>own look</h1>");

        let root = crate::test_support::temp_dir("share");
        let dir = root.join("artifacts");
        std::fs::create_dir_all(dir.join("a1")).unwrap();
        std::fs::write(dir.join("a1/index.html"), "<p>one</p>").unwrap();
        let copy = dir.join("a1/claude/index.html");
        // Off: no copy.
        page_changed(&dir, "a1");
        assert!(!copy.exists());
        std::fs::write(root.join(SETTINGS_FILE), r#"{"shareToClaude":true}"#).unwrap();
        page_changed(&dir, "a1");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "<p>one</p>");
        std::fs::write(dir.join("a1/index.html"), "<p>two</p>").unwrap();
        refresh_copies(&dir);
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "<p>two</p>");
    }

    #[test]
    fn links_are_kept_beside_the_page() {
        let dir = crate::test_support::temp_dir("share");
        std::fs::create_dir_all(dir.join("a1")).unwrap();
        assert_eq!(link(&dir, "a1"), None);
        let kept = Link { url: "https://claude.ai/artifact/x".into(), version: 2, at: 5 };
        save_link(&dir, "a1", &kept).unwrap();
        assert_eq!(link(&dir, "a1"), Some(kept));
        assert!(save_link(&dir, "gone", &Link { url: String::new(), version: 1, at: 1 }).is_err());
    }

    #[test]
    fn the_share_button_runs_claude_code_with_only_the_artifact_tool() {
        let call = publish_invocation(
            Path::new("/bin/claude"),
            Path::new("/a/index.html"),
            "Plan",
            Some("https://claude.ai/artifact/x"),
        );
        let has = |pair: [&str; 2]| call.args.windows(2).any(|window| window[0] == pair[0] && window[1] == pair[1]);
        assert!(
            has(["--tools", "Artifact,Read"])
                && call.args.contains(&"--allowedTools=Artifact,Read".to_string())
                && has(["--permission-mode", "dontAsk"])
        );
        assert!(call.env.contains(&("CLAUDE_CODE_ARTIFACT".into(), "1".into())));
        assert!(
            call.stdin.contains("/a/index.html")
                && call.stdin.contains("https://claude.ai/artifact/x")
                && call.stdin.contains("`force: true`")
        );
        // A first share has nothing to replace.
        assert!(
            !publish_invocation(Path::new("/bin/claude"), Path::new("/a/index.html"), "Plan", None)
                .stdin
                .contains("force")
        );
    }
}

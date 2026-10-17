// The artifacts' MCP tools (artifacts.rs): what the coding tool calls in a Filo conversation to put a report, a
// page, a diagram or a small interactive tool in the user's Artifacts library, open one again and find it. Each is one
// HTML page on this Mac, with every version kept; what a turn publishes or opens shows as a card in its conversation.
//
// It works file first, like Claude's own Artifact tool: the agent builds the page as an HTML file (the artifacts
// skill, skills.rs, has its template), edits it with its file tools, and `publish_artifact` reads the file. Publishing
// the same file again saves the artifact's next version. A file can be anywhere on this Mac, so a page a coding task
// built is published the same way once its round has finished; a path that isn't full is in the turn's working folder
// (the store's workspace, where the CLI runs). `open_artifact` gives the file to change, making one in the working
// folder's `pages` for an artifact that has none yet.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::artifacts::{self, Origin, Published};
use crate::db::Db;

const NAMES: &[&str] = &["publish_artifact", "open_artifact", "list_artifacts"];
/// `list_artifacts` shows at most this many.
const LIST_DEFAULT: usize = 20;
const LIST_MAX: usize = 50;

/// How a page is written, for publish_artifact's description (the artifacts skill has the rest).
const PAGE_RULES: &str = "The page is one self-contained HTML document (CSS and JavaScript inline). It's shown in a sandbox with no network: \
external scripts, stylesheets, fonts, images, fetch and frames are blocked, so inline everything (SVG for diagrams and charts, data: URLs \
for pictures). Put <!--filo:head--> inside <head> to get the app's theme as CSS variables (--background, --surface, --surfaceStrong, \
--border, --text, --textSecondary, --accent, --onAccent, --success, --danger, --warning, --c1 to --c4 for series; sizes --body, \
--headline, --footnote…) and its font; use them so the page matches light and dark mode. A button can ask Filo something with \
filo.chat(\"text\") (the user confirms first).";

/// Whether `name` is one of these tools.
pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn tools() -> Vec<Value> {
    let kinds = artifacts::KINDS;
    vec![
        json!({
            "name": "publish_artifact",
            "title": "Publish artifact",
            "description": format!("Publish an HTML page from its file on this Mac to the user's Artifacts library in the Filo app, where they keep, look at and use what you make for them: a report, a web page, a diagram or an interactive tool (a calculator, a planner, a quiz…). Use it when the user asks for something like that rather than a chat answer. Build the page as a file first, as the artifacts skill says (start from its template), then publish the file. The first publish of a file makes a new artifact, and a card in this chat opens it. Publishing the same file again after you edit it saves the artifact's next version: earlier ones stay and the user can restore them, and if they have it open it updates in front of them. So keep changing the same file for as long as it's the same artifact: a file at another path makes another artifact. A page a coding task built is published the same way, by its path, once its round has finished. {PAGE_RULES} Returns the artifact's id and version; tell the user about it with send_message."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": format!("The page's HTML file (.html or .htm, {} MB at most): its full path, one from ~/, or one in your working folder like pages/meal-plan.html.", artifacts::MAX_HTML_BYTES / 1024 / 1024) },
                    "title": { "type": "string", "description": "A short name for it, on one line. Leave it out to keep its name (a new one is named by the page's <title>)." },
                    "summary": { "type": "string", "description": "One or two sentences on what it is, shown on its card. Leave it out to keep it as it is." },
                    "kind": { "type": "string", "enum": kinds, "description": "What it is (page by default)." },
                    "label": { "type": "string", "description": "A few words on this version: what changed (\"Added March\"), or a tag like \"first draft\". Shown on its card and in its versions." }
                },
                "required": ["path"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "open_artifact",
            "title": "Open artifact",
            "description": "Open an artifact by its id (list_artifacts gives them): shows its card in this chat so the user can open it, and gives you its details and the HTML file it's published from. Read that file to answer questions about it. To change it, edit that file and publish it again with publish_artifact, which saves the next version. `version` also gives an older version's page to read.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "version": { "type": "number", "description": "An older version to read (the current one by default)." }
                },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "list_artifacts",
            "title": "List artifacts",
            "description": "The artifacts in the user's library, most recently changed first, with their ids: those whose title, summary or kind has every word of `query` (empty for all).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "number", "description": "At most this many (50 at most, 20 by default)." }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
    ]
}

use crate::mcp::text_arg as text;

fn id(args: &Value) -> Result<&str, String> {
    crate::mcp::id_arg(args, "Give the artifact's `id`.")
}

/// The file `path` names: a full path, one from the home folder (`~/`), or one in the turn's working folder.
fn full_path(path: &str, workspace: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None if Path::new(path).is_absolute() => PathBuf::from(path),
        None => workspace.join(path),
    }
}

fn line(found: &artifacts::Artifact) -> String {
    let summary = if found.summary.is_empty() { String::new() } else { format!(": {}", found.summary) };
    format!("- [{}] {} ({}, version {}){summary}", found.id, found.title, found.kind, found.current_version)
}

/// Runs a tool on the database at `db_path` (the artifacts' files beside it) for `origin`, in a turn working in
/// `workspace`.
pub fn call(db_path: &Path, workspace: &Path, origin: &Origin, name: &str, args: &Value) -> Result<String, String> {
    let db = Db::open(db_path).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    artifacts::ensure(&db)?;
    call_with(&db, &artifacts::folder_for(db_path), workspace, origin, name, args, crate::now_ms() as i64)
}

fn call_with(
    db: &Db,
    dir: &Path,
    workspace: &Path,
    origin: &Origin,
    name: &str,
    args: &Value,
    now: i64,
) -> Result<String, String> {
    match name {
        "publish_artifact" => {
            let path = text(args, "path")
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .ok_or("Give `path`, the page's HTML file.")?;
            let file = full_path(path, workspace);
            let source = artifacts::source_key(&file);
            // Filo's own copies: a version's page never changes, so it's never the file to publish.
            if Path::new(&source).starts_with(artifacts::source_key(dir)) {
                return Err("That's Filo's own copy of an artifact's page, which stays as it is. open_artifact gives the file to edit and publish.".into());
            }
            let html = artifacts::read_page_file(&file)?;
            let publish = artifacts::Publish {
                source: source.clone(),
                title: text(args, "title").map(str::to_string),
                summary: text(args, "summary").map(str::to_string),
                kind: text(args, "kind").map(str::to_string),
                label: text(args, "label").map(str::to_string),
                html,
            };
            let (found, outcome) = artifacts::publish(db, dir, &publish, origin, now)?;
            let (title, id, version) = (&found.title, &found.id, found.current_version);
            let mut reply = match outcome {
                Published::Created => format!(
                    "Published \u{201c}{title}\u{201d} as a new artifact (id {id}, version 1) from {source}. It's in the user's Artifacts library, and a card in this chat opens it. To change it, edit that file and publish it again: that saves version 2. Tell the user about it with send_message."
                ),
                Published::Saved => format!(
                    "Saved version {version} of \u{201c}{title}\u{201d} (id {id}) from {source}; the earlier versions stay. If the user has it open, it updated in front of them."
                ),
                Published::Unchanged => format!(
                    "\u{201c}{title}\u{201d} (id {id}) is still version {version}: the page in {source} is the same as that version, so no new version was saved.{}",
                    if ["title", "summary", "kind"].iter().any(|key| text(args, key).is_some()) {
                        " Its title, summary and kind are as you gave them."
                    } else {
                        ""
                    }
                ),
            };
            // Its web pictures are saved beside it (artifact_images.rs), so they aren't what it loads from the internet.
            let folder = artifacts::artifact_dir(dir, id)?;
            let shown = crate::artifact_images::rewrite(&publish.html, &folder, |file, _| {
                Some(format!("{}/{file}", crate::artifact_images::DIR))
            });
            for warning in artifacts::page_warnings(&shown) {
                reply.push_str(&format!("\nCheck: {warning}"));
            }
            Ok(reply)
        }
        "open_artifact" => {
            let id = id(args)?;
            let version = args.get("version").and_then(Value::as_f64).map(|version| version.round() as i64);
            let Some(found) = artifacts::get(db, id)? else {
                return Err("There's no artifact with that id. list_artifacts shows them.".into());
            };
            let Some((shown, _)) = artifacts::document(db, dir, id, version)? else {
                return Err(format!(
                    "\u{201c}{}\u{201d} has no version {}: it's at version {}.",
                    found.title,
                    version.unwrap_or(0),
                    found.current_version
                ));
            };
            let file = artifacts::page_file(db, dir, &workspace.join(artifacts::PAGES_DIR), id)?
                .ok_or("There's no artifact with that id. list_artifacts shows them.")?;
            artifacts::link_opened(db, id, origin, now)?;
            let path = file.path.display();
            let current = found.current_version;
            let mut reply = format!(
                "\u{201c}{}\u{201d} (id {}, {}), version {current}{}. Its card is in this chat now.\nSummary: {}\n",
                found.title,
                found.id,
                found.kind,
                artifacts::versions(db, id, 1)?
                    .first()
                    .filter(|latest| !latest.note.is_empty())
                    .map(|latest| format!(" ({})", latest.note))
                    .unwrap_or_default(),
                if found.summary.is_empty() { "(none)" } else { found.summary.as_str() },
            );
            let made = if found.source.is_none() { " (it had none, so Filo wrote its current page there)" } else { "" };
            reply.push_str(&format!(
                "Its page's file: {path}{made}. Read it to answer questions about it; to change it, edit that file and publish it again with publish_artifact, which saves version {}.",
                current + 1
            ));
            if !file.current {
                reply.push_str(&format!(
                    "\nThat file has changes that aren't published: it isn't version {current}'s page, which is at {}. Publish it as it is to keep them, or copy version {current}'s page over it first to start from what the user sees.",
                    artifacts::artifact_dir(dir, id)?.join(format!("v{current}.html")).display()
                ));
            }
            if version.is_some_and(|version| version != current) {
                let old = artifacts::artifact_dir(dir, id)?.join(&shown.file);
                let note = if shown.note.is_empty() { String::new() } else { format!(" ({})", shown.note) };
                reply.push_str(&format!(
                    "\nVersion {}{note}'s page, to read: {}. Filo's copy stays as it is; to bring it back, copy it over the page's file and publish that.",
                    shown.version,
                    old.display()
                ));
            }
            Ok(reply)
        }
        "list_artifacts" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_f64)
                .map_or(LIST_DEFAULT, |limit| (limit.max(1.) as usize).min(LIST_MAX));
            let found = artifacts::search(db, text(args, "query").unwrap_or_default(), limit)?;
            if found.is_empty() {
                return Ok("No artifacts match.".into());
            }
            Ok(found.iter().map(line).collect::<Vec<_>>().join("\n"))
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tests::temp_path;

    struct Setup {
        path: PathBuf,
        dir: PathBuf,
        workspace: PathBuf,
        db: Db,
    }

    fn setup(name: &str) -> Setup {
        let path = temp_path(name);
        let db = Db::open(&path).unwrap();
        let workspace = path.parent().unwrap().join("workspace");
        std::fs::create_dir_all(workspace.join("pages")).unwrap();
        Setup { dir: artifacts::folder_for(&path), workspace, db, path }
    }

    impl Setup {
        fn call(&self, name: &str, args: Value) -> Result<String, String> {
            let origin =
                Origin { author: "Chef".into(), chat_id: Some("chat-1".into()), turn_id: Some("turn-1".into()) };
            call_with(&self.db, &self.dir, &self.workspace, &origin, name, &args, 1_000)
        }

        fn only_id(&self) -> String {
            artifacts::list(&self.db, 10).unwrap()[0].id.clone()
        }
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(self.path.parent().unwrap()).ok();
        }
    }

    fn page(heading: &str) -> String {
        format!(
            "<!doctype html><html><head><title>Week plan</title><!--filo:head--></head><body><h1>{heading}</h1></body></html>"
        )
    }

    #[test]
    fn the_tools_publish_open_and_list() {
        let setup = setup("artifacts-mcp");
        let file = setup.workspace.join("pages/meal-plan.html");
        std::fs::write(&file, page("Week")).unwrap();

        // A path in the working folder; the first publish makes the artifact.
        let made = setup.call("publish_artifact", json!({ "path": "pages/meal-plan.html", "title": "Meal plan", "summary": "Dinners", "kind": "report", "label": "First draft" })).unwrap();
        let id = setup.only_id();
        assert!(
            made.starts_with("Published \u{201c}Meal plan\u{201d} as a new artifact")
                && made.contains(&id)
                && !made.contains("Check:"),
            "{made}"
        );
        assert!(made.contains(&artifacts::source_key(&file)), "{made}");

        // Edited and published again by its full path: version 2 of the same one.
        std::fs::write(&file, page("Week 2")).unwrap();
        let saved = setup
            .call("publish_artifact", json!({ "path": format!(" {} ", file.display()), "label": "Next week" }))
            .unwrap();
        assert!(saved.starts_with("Saved version 2 of \u{201c}Meal plan\u{201d}"), "{saved}");
        let same = setup
            .call("publish_artifact", json!({ "path": file.display().to_string(), "summary": "Dinners for two" }))
            .unwrap();
        assert!(same.contains("is still version 2"), "{same}");
        assert_eq!(artifacts::get(&setup.db, &id).unwrap().unwrap().summary, "Dinners for two");

        let opened = setup.call("open_artifact", json!({ "id": id })).unwrap();
        assert!(
            opened.contains("version 2 (Next week)")
                && opened.contains(&format!("Its page's file: {}.", artifacts::source_key(&file))),
            "{opened}"
        );
        assert!(
            opened.contains("saves version 3") && !opened.contains("aren't published") && !opened.contains("<h1>"),
            "{opened}"
        );
        let old = setup.call("open_artifact", json!({ "id": id, "version": 1 })).unwrap();
        assert!(
            old.contains("Version 1 (First draft)'s page, to read") && old.contains(&format!("{id}/v1.html")),
            "{old}"
        );
        assert!(setup.call("open_artifact", json!({ "id": id, "version": 7 })).is_err());
        // Changes not published yet are pointed out.
        std::fs::write(&file, page("Draft")).unwrap();
        assert!(setup.call("open_artifact", json!({ "id": id })).unwrap().contains("changes that aren't published"));

        assert!(
            setup
                .call("list_artifacts", json!({ "query": "meal" }))
                .unwrap()
                .contains(&format!("[{id}] Meal plan (report, version 2): Dinners for two"))
        );
        assert_eq!(setup.call("list_artifacts", json!({ "query": "zebra" })).unwrap(), "No artifacts match.");
        assert_eq!(artifacts::get(&setup.db, &id).unwrap().unwrap().author, "Chef");
        // One card for the turn, whatever it did with it.
        assert_eq!(artifacts::chat_cards(&setup.db, "chat-1").unwrap().len(), 1);
    }

    #[test]
    fn a_big_page_a_coding_task_built_is_published_by_its_path() {
        let setup = setup("artifacts-mcp-path");
        let project = setup.path.parent().unwrap().join("project");
        std::fs::create_dir_all(&project).unwrap();
        // A coding task's page with its font inlined: about 1.5 MB, far past what a call should carry.
        let inlined = "QUJDRA".repeat(256 * 1024);
        let mock = |heading: &str| {
            format!(
                "<!doctype html><html><head><style>@font-face{{src:url(data:font/woff2;base64,{inlined})}}</style><!--filo:head--></head><body><h1>{heading}</h1></body></html>"
            )
        };
        let file = project.join("onboarding.html");
        std::fs::write(&file, mock("Welcome")).unwrap();
        let made = setup.call("publish_artifact", json!({ "path": file.display().to_string() })).unwrap();
        let id = setup.only_id();
        assert!(made.contains(&id) && made.contains("\u{201c}Onboarding\u{201d}"), "{made}");
        assert_eq!(artifacts::document(&setup.db, &setup.dir, &id, None).unwrap().unwrap().1, mock("Welcome"));
        std::fs::write(&file, mock("Welcome back")).unwrap();
        assert!(
            setup
                .call("publish_artifact", json!({ "path": file.display().to_string(), "label": "Round 2" }))
                .unwrap()
                .starts_with("Saved version 2")
        );
        assert_eq!(
            full_path("~/designs/a.html", &setup.workspace),
            PathBuf::from(std::env::var("HOME").unwrap()).join("designs/a.html")
        );
    }

    #[test]
    fn what_cant_be_published_is_refused_and_saves_nothing() {
        let setup = setup("artifacts-mcp-refused");
        let refused = |args: Value| setup.call("publish_artifact", args).unwrap_err();
        let at = |name: &str| setup.workspace.join("pages").join(name).display().to_string();
        assert!(refused(json!({ "title": "T" })).contains("Give `path`"));
        assert!(refused(json!({ "path": at("gone.html") })).contains("no file"));
        assert!(
            refused(json!({ "path": setup.workspace.join("pages").display().to_string() }))
                .contains("isn't an HTML file")
        );
        std::fs::write(at("notes.txt"), "secret").unwrap();
        assert!(refused(json!({ "path": at("notes.txt") })).contains("isn't an HTML file"));
        std::fs::write(at("huge.html"), "x".repeat(artifacts::MAX_HTML_BYTES + 1)).unwrap();
        assert!(refused(json!({ "path": at("huge.html") })).contains("too big (2048 KB, at most 2048 KB)"));
        std::fs::write(at("binary.html"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(refused(json!({ "path": at("binary.html") })).contains("isn't text"));
        std::fs::write(at("blank.htm"), " \n").unwrap();
        assert!(refused(json!({ "path": at("blank.htm") })).contains("is empty"));
        std::fs::write(at("page.html"), page("x")).unwrap();
        assert!(refused(json!({ "path": at("page.html"), "kind": "movie" })).contains("`kind` is one of"));
        assert!(artifacts::list(&setup.db, 10).unwrap().is_empty());

        // Filo's own copy of a version is never the file to publish (nor to edit).
        setup.call("publish_artifact", json!({ "path": at("page.html") })).unwrap();
        let id = setup.only_id();
        assert!(
            refused(json!({ "path": setup.dir.join(&id).join("v1.html").display().to_string() }))
                .contains("Filo's own copy")
        );
        assert_eq!(artifacts::list(&setup.db, 10).unwrap().len(), 1);
        // A page that loads from the internet is published, with what to fix.
        std::fs::write(
            at("cdn.html"),
            "<html><head><script src=\"https://cdn.example.com/x.js\"></script></head></html>",
        )
        .unwrap();
        let warned = setup.call("publish_artifact", json!({ "path": at("cdn.html") })).unwrap();
        assert!(
            warned.contains("Check: It loads a file from the internet (https://cdn.example.com/x.js)")
                && warned.contains("Check: It has no <!--filo:head-->"),
            "{warned}"
        );
    }

    #[test]
    fn an_artifact_made_before_gets_a_file_to_edit() {
        let setup = setup("artifacts-mcp-old");
        let origin = Origin { author: "Filo".into(), ..Origin::default() };
        let new = artifacts::New { title: "Trip to Lisbon".into(), html: page("Day 1"), ..artifacts::New::default() };
        let made = artifacts::create(&setup.db, &setup.dir, &new, &origin, 1).unwrap();
        let opened = setup.call("open_artifact", json!({ "id": made.id })).unwrap();
        let file = setup.workspace.join("pages/trip-to-lisbon.html");
        assert!(
            opened.contains(&format!(
                "Its page's file: {} (it had none, so Filo wrote its current page there)",
                artifacts::source_key(&file)
            )),
            "{opened}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), page("Day 1"));
        std::fs::write(&file, page("Day 2")).unwrap();
        let saved =
            setup.call("publish_artifact", json!({ "path": "pages/trip-to-lisbon.html", "label": "Day 2" })).unwrap();
        assert!(saved.starts_with("Saved version 2 of \u{201c}Trip to Lisbon\u{201d}"), "{saved}");
        assert!(!setup.call("open_artifact", json!({ "id": made.id })).unwrap().contains("it had none"));
        assert!(setup.call("open_artifact", json!({ "id": "missing" })).unwrap_err().contains("no artifact"));
    }

    #[test]
    fn while_sharing_to_claude_ai_is_on_its_copy_follows_each_publish() {
        let setup = setup("artifacts-mcp-share");
        std::fs::write(
            setup.path.parent().unwrap().join(crate::artifact_share::SETTINGS_FILE),
            r#"{"shareToClaude":true}"#,
        )
        .unwrap();
        let file = setup.workspace.join("pages/plan.html");
        std::fs::write(&file, page("One")).unwrap();
        setup.call("publish_artifact", json!({ "path": "pages/plan.html" })).unwrap();
        let id = setup.only_id();
        let copy = setup.dir.join(&id).join(crate::artifact_share::COPY_DIR).join("index.html");
        // The copy stands alone: Filo's theme at the marker.
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), crate::artifact_share::share_page(&page("One")));
        std::fs::write(&file, page("Two")).unwrap();
        setup.call("publish_artifact", json!({ "path": "pages/plan.html" })).unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), crate::artifact_share::share_page(&page("Two")));
        assert_eq!(crate::artifact_share::artifact_of(&setup.dir, &copy), Some((id, None)));
    }

    #[test]
    fn every_tool_is_named_and_handled() {
        let names: Vec<String> = tools().iter().map(|tool| tool["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, NAMES);
        assert!(names.iter().all(|name| handles(name)));
        assert!(!handles("send_message") && !handles("create_artifact") && !handles("update_artifact"));
    }
}

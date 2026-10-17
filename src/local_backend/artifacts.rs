// Artifacts on this Mac: the Artifacts tab's and the artifact page's `canvasArtifacts:*` functions, answered from Filo's
// database and the pages' files beside it (crates/filo-local `artifacts`), and their cards in a conversation.
//
// Agents make and change them with the MCP server's artifact tools (crates/filo-local `artifacts_mcp`), in any
// conversation's turn of a coding tool. The server is another process, so its saves reach the pages through
// the turn watcher, which notices the database changing (local_backend.rs `watch_turns`). Nothing is published, unless
// the user turns on "Share to claude.ai" (`artifactShare:*`, crates/filo-local `artifact_share`): then Claude Code
// publishes one to their claude.ai account when they ask, in a conversation or with the page's Share button
// (`canvasArtifacts:publish`), and the link is kept on the artifact (`claudeLink`). Apart from that, the page's Share
// puts one at a link of its own on filo.im (`canvasArtifacts:share`, `canvasArtifacts:unshare`, artifact_link.rs),
// which the artifact carries while it's shared (`link`).
//
// A conversation shows what each of its turns did with an artifact as the finished-work card the chat already has
// ("About …", the summary, Open): `voiceSessions:detail` lists them as completed tasks with the artifact (`chat_tasks`).

use std::path::PathBuf;
use std::sync::Arc;

use filo_local::artifact_share;
use filo_local::artifacts::{self, Artifact, Origin};
use filo_local::turn;
use serde_json::{Value, json};

use super::{Local, in_namespace, not_here, now_ms, required};

/// The functions answered here, for the test that each is (the module answers its namespaces whole: `in_namespace`).
#[cfg(test)]
pub const ARTIFACTS: &[&str] = &[
    "canvasArtifacts:list",
    "canvasArtifacts:get",
    "canvasArtifacts:listVersions",
    "canvasArtifacts:getVersion",
    "canvasArtifacts:markOpened",
    "canvasArtifacts:publish",
    "canvasArtifacts:sharePreview",
    "canvasArtifacts:share",
    "canvasArtifacts:unshare",
    "canvasArtifacts:restoreVersion",
    "canvasArtifacts:sendArtifactMessage",
    "artifactShare:settings",
    "artifactShare:setOn",
];

/// What the page's `filo.chat(text)` may send, at most.
const MESSAGE_CHARS: usize = 2_000;

/// An artifact as the pages read it (`ArtifactCard`): its card shows the summary.
fn card(found: &Artifact) -> Value {
    json!({
        "_id": found.id,
        "title": found.title,
        "summary": found.summary,
        "currentVersion": found.current_version,
        "createdAt": found.created_at,
        "openedAt": found.opened_at,
    })
}

fn version_arg(args: &Value) -> Result<i64, String> {
    args.get("version")
        .and_then(Value::as_f64)
        .map(|version| version.round() as i64)
        .ok_or_else(|| "`version` is required".into())
}

impl Local {
    /// The pages' files, beside the database.
    pub(super) fn artifacts_dir(&self) -> PathBuf {
        artifacts::folder_for(&self.root.join(filo_local::db::FILE))
    }

    /// Answers the artifacts' functions (`canvasArtifacts:`, `artifactShare:`); `None` for any other path.
    pub(super) fn artifacts_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        in_namespace(path, &["canvasArtifacts", "artifactShare"]).then(|| self.artifacts_answer(path, args))
    }

    fn artifacts_answer(self: &Arc<Self>, path: &str, args: &Value) -> Result<Value, String> {
        let db = self.db()?;
        let dir = self.artifacts_dir();
        let id = || required(args, "id");
        match path {
            "canvasArtifacts:list" => {
                let limit = super::limit(args, 50, 100);
                Ok(Value::Array(artifacts::list(db, limit)?.iter().map(card).collect()))
            }
            "canvasArtifacts:get" => {
                let Some(found) = artifacts::get(db, id()?)? else { return Ok(Value::Null) };
                let document =
                    artifacts::document(db, &dir, &found.id, None)?.map(|(_, html)| html).unwrap_or_default();
                let mut value = card(&found);
                value["document"] = json!(document);
                // Its folder, for Show in Finder.
                value["folder"] = json!(artifacts::artifact_dir(&dir, &found.id)?.display().to_string());
                // The folder of the file it's published from, where the files the page names next to it are.
                value["sourceFolder"] = json!(
                    found
                        .source
                        .as_deref()
                        .and_then(|source| std::path::Path::new(source).parent())
                        .map(|folder| folder.display().to_string())
                );
                // Its claude.ai link, once shared there.
                value["claudeLink"] = json!(artifact_share::link(&dir, &found.id));
                // Its link on filo.im, while it's shared (not its token).
                value["link"] = json!(
                    self.artifact_link(&found.id).map(|link| json!({ "url": link.share.url, "version": link.version }))
                );
                Ok(value)
            }
            "canvasArtifacts:listVersions" => {
                let id = id()?;
                let current = artifacts::get(db, id)?.map(|found| found.current_version);
                let limit = super::limit(args, 50, usize::MAX);
                let rows = artifacts::versions(db, id, limit)?;
                Ok(Value::Array(
                    rows.iter()
                        .map(|row| json!({ "version": row.version, "createdAt": row.created_at, "isCurrent": Some(row.version) == current }))
                        .collect(),
                ))
            }
            "canvasArtifacts:getVersion" => {
                let id = id()?;
                let current = artifacts::get(db, id)?.map(|found| found.current_version);
                Ok(match artifacts::document(db, &dir, id, Some(version_arg(args)?))? {
                    Some((row, html)) => {
                        json!({ "version": row.version, "document": html, "createdAt": row.created_at, "isCurrent": Some(row.version) == current })
                    }
                    None => Value::Null,
                })
            }
            "canvasArtifacts:markOpened" => self.write_db(|db| artifacts::mark_opened(db, id()?, now_ms())),
            "canvasArtifacts:publish" => self.share_artifact(id()?),
            "canvasArtifacts:sharePreview" => self.artifact_share_preview(id()?),
            "canvasArtifacts:share" => self.share_artifact_link(id()?, &super::result_share::endpoint("artifacts")),
            "canvasArtifacts:unshare" => self.unshare_artifact_link(id()?, &super::result_share::endpoint("artifacts")),
            "canvasArtifacts:restoreVersion" => {
                let restored = artifacts::restore(db, &dir, id()?, version_arg(args)?, &Origin::default(), now_ms())?;
                self.changed();
                match restored {
                    Some(found) => Ok(json!({ "version": found.current_version })),
                    None => Err("That version isn't there any more.".into()),
                }
            }
            "canvasArtifacts:sendArtifactMessage" => self.send_artifact_message(args),
            // The switch, and the claude.ai account Claude Code is signed in to (it shows only with one).
            "artifactShare:settings" => Ok(json!({
                "account": artifact_share::claude_ai_account(),
                "on": artifact_share::Settings::read(&self.root).share_to_claude,
            })),
            "artifactShare:setOn" => {
                let on = args.get("on").and_then(Value::as_bool).ok_or("`on` is required")?;
                self.update_json::<artifact_share::Settings, _>(artifact_share::SETTINGS_FILE, |settings| {
                    settings.share_to_claude = on;
                    Ok(json!(on))
                })
            }
            _ => Err(not_here(path)),
        }
    }

    /// The page's Share button: its current version to claude.ai with a short Claude Code run (taking a while), and the
    /// link, kept on the artifact. Only while "Share to claude.ai" is on.
    fn share_artifact(&self, id: &str) -> Result<Value, String> {
        if !artifact_share::is_on(&self.root) {
            return Err("Artifacts stay on this Mac. To share them to claude.ai, turn on Share to claude.ai in the Artifacts tab's settings.".into());
        }
        let link = artifact_share::publish(&self.root.join(filo_local::db::FILE), id, Some(super::login_path()))?;
        self.changed();
        Ok(json!({ "url": link.url, "version": link.version }))
    }

    /// The page's `filo.chat(text)`, once the user agreed: a message in the conversation the artifact came from (else
    /// Filo's own), with the artifact named for the engine only, and its turn started. One made in the group gets it
    /// in the group, as the user's message there.
    fn send_artifact_message(self: &Arc<Self>, args: &Value) -> Result<Value, String> {
        let id = required(args, "artifactId")?;
        let text: String =
            args.get("text").and_then(Value::as_str).unwrap_or_default().trim().chars().take(MESSAGE_CHARS).collect();
        if text.is_empty() {
            return Err("The message is empty.".into());
        }
        let found = artifacts::get(self.db()?, id)?.ok_or("This artifact isn't there any more.")?;
        let context = format!(
            "The user sent this from the page of the artifact \u{201c}{}\u{201d} (id {}, version {}). open_artifact gives the file it's published from; to change it, edit that file and publish it again with publish_artifact.",
            found.title, found.id, found.current_version
        );
        if let Some(group) = self.group_session().filter(|group| found.chat_id.as_deref() == Some(group.as_str())) {
            let sent = self.group_user_message(
                &group,
                &uuid::Uuid::new_v4().to_string(),
                &text,
                Vec::new(),
                Some(&context),
                None,
            )?;
            return Ok(json!({ "sessionId": sent["sessionId"] }));
        }
        // The conversation it was made in; if that's gone, the main agent's.
        let session_id = match found.chat_id.clone().filter(|chat| self.store_of(chat).is_some()) {
            Some(chat) => chat,
            None => self.agent_or_main_chat(None)?,
        };
        let store = self.store_of(&session_id).ok_or("This conversation no longer exists.")?.clone();
        let turn_id = uuid::Uuid::new_v4().to_string();
        store
            .add_user_message(&session_id, &text, &turn_id)
            .map_err(|error| format!("Couldn't save the message: {error}"))?;
        turn::save_context(&store, &session_id, &turn_id, &context)
            .map_err(|error| format!("Couldn't save the message's context: {error}"))?;
        self.start_turn(store, session_id.clone(), turn_id);
        self.changed();
        Ok(json!({ "sessionId": session_id }))
    }

    /// The artifact cards of a conversation, as the finished work `voiceSessions:detail` lists (`tasks`).
    pub(super) fn chat_tasks(&self, chat_id: &str) -> Value {
        let Ok(db) = self.db() else { return json!([]) };
        let cards = artifacts::chat_cards(db, chat_id).unwrap_or_default();
        Value::Array(
            cards
                .iter()
                .map(|card| {
                    json!({
                        "_id": format!("artifact-{}", card.id),
                        "title": card.title,
                        "summary": card.text,
                        "createdAt": card.first_at,
                        "updatedAt": card.at,
                        "artifact": { "_id": card.artifact_id, "title": card.title },
                    })
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filo_local::artifacts::New;

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("artifacts")
    }

    #[test]
    fn the_pages_read_restore_and_open_artifacts_and_chats_show_cards() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        assert_eq!(call("canvasArtifacts:list", json!({ "limit": 100 })).unwrap(), json!([]));

        let chat = local.agent_chats.create_chat(filo_local::Engine::Claude).unwrap();
        let origin = Origin { author: "Filo".into(), chat_id: Some(chat.id.clone()), turn_id: Some("turn-1".into()) };
        let new = New {
            title: "Trip".into(),
            html: "<html><body>one</body></html>".into(),
            summary: "Lisbon".into(),
            kind: None,
        };
        let made = artifacts::create(local.db().unwrap(), &local.artifacts_dir(), &new, &origin, 1_000).unwrap();
        let change = artifacts::Change { html: Some("<html><body>two</body></html>".into()), ..Default::default() };
        artifacts::update(local.db().unwrap(), &local.artifacts_dir(), &made.id, &change, &origin, 2_000).unwrap();

        let list = call("canvasArtifacts:list", json!({ "limit": 100 })).unwrap();
        assert_eq!(
            (list[0]["_id"].as_str(), list[0]["currentVersion"].as_i64(), list[0]["openedAt"].is_null()),
            (Some(made.id.as_str()), Some(2), true)
        );
        let got = call("canvasArtifacts:get", json!({ "id": made.id })).unwrap();
        assert_eq!(got["document"], "<html><body>two</body></html>");
        assert!(PathBuf::from(got["folder"].as_str().unwrap()).join("v2.html").is_file());
        assert_eq!(call("canvasArtifacts:get", json!({ "id": "missing" })).unwrap(), Value::Null);
        let versions = call("canvasArtifacts:listVersions", json!({ "id": made.id, "limit": 50 })).unwrap();
        assert_eq!(
            (versions[0]["version"].as_i64(), versions[0]["isCurrent"].as_bool(), versions[1]["isCurrent"].as_bool()),
            (Some(2), Some(true), Some(false))
        );
        let old = call("canvasArtifacts:getVersion", json!({ "id": made.id, "version": 1 })).unwrap();
        assert_eq!(
            (old["document"].as_str(), old["isCurrent"].as_bool()),
            (Some("<html><body>one</body></html>"), Some(false))
        );
        assert_eq!(
            call("canvasArtifacts:restoreVersion", json!({ "id": made.id, "version": 1 })).unwrap()["version"],
            3
        );
        assert_eq!(
            call("canvasArtifacts:get", json!({ "id": made.id })).unwrap()["document"],
            "<html><body>one</body></html>"
        );
        assert_eq!(call("canvasArtifacts:markOpened", json!({ "id": made.id })).unwrap(), true);
        assert!(call("canvasArtifacts:list", json!({})).unwrap()[0]["openedAt"].is_number());
        // Not shared anywhere: "Share to claude.ai" is off by default.
        assert_eq!(call("artifactShare:settings", json!({})).unwrap()["on"], false);
        assert!(call("canvasArtifacts:publish", json!({ "id": made.id })).unwrap_err().contains("stay on this Mac"));
        assert_eq!(call("canvasArtifacts:get", json!({ "id": made.id })).unwrap()["claudeLink"], Value::Null);
        assert_eq!(call("artifactShare:setOn", json!({ "on": true })).unwrap(), true);
        assert_eq!(call("artifactShare:settings", json!({})).unwrap()["on"], true);
        // The link a turn saved shows on the artifact.
        let link = artifact_share::Link { url: "https://claude.ai/artifact/x".into(), version: 2, at: 3_000 };
        artifact_share::save_link(&local.artifacts_dir(), &made.id, &link).unwrap();
        assert_eq!(
            call("canvasArtifacts:get", json!({ "id": made.id })).unwrap()["claudeLink"]["url"],
            "https://claude.ai/artifact/x"
        );
        call("artifactShare:setOn", json!({ "on": false })).unwrap();
        assert!(call("canvasArtifacts:sendArtifactMessage", json!({ "artifactId": made.id, "text": "  " })).is_err());

        // The conversation it was made in shows one card for the turn, opening the artifact.
        let detail = call("voiceSessions:detail", json!({ "id": chat.id, "limit": 50 })).unwrap();
        let tasks = detail["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1, "{tasks:#?}");
        assert_eq!(
            (tasks[0]["artifact"]["_id"].as_str(), tasks[0]["summary"].as_str()),
            (Some(made.id.as_str()), Some("Lisbon"))
        );
        std::fs::remove_dir_all(&local.root).ok();
    }
}

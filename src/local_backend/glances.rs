// Glances on this Mac: the `glances:*` functions the chat's glance cards call (src/screens/chat/glance.rs), answered
// from Filo's database (crates/filo-local `glances`), and the card's head on the messages that show one.
//
// A turn's coding tool makes a glance with the MCP server's `show_glance` tool (crates/filo-local
// `glances_mcp`): the glance in the database and a message in the conversation that says it in words. The
// conversation's `voiceSessions:detail` gives that message its `glance` (`with_glances`); the card fetches the page
// once with `glances:get`, and Save to Artifacts keeps it as an artifact page of its own.

use std::sync::Arc;

use filo_local::artifacts::{self, Origin};
use filo_local::glances;
use serde_json::{Value, json};

use super::{Local, now_ms, required};

/// The functions answered here (checked by a test, like `MOVED`).
#[cfg(test)]
pub const GLANCES: &[&str] = &["glances:get", "glances:saveToArtifacts"];

impl Local {
    /// Answers the glances' functions; `None` for any other path.
    pub(super) fn glances_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        match path {
            "glances:get" => Some(self.glance(args)),
            "glances:saveToArtifacts" => Some(self.save_glance(args)),
            _ => None,
        }
    }

    /// One glance for its card and sheet, with its whole page. A glance never changes once made, so the app reads it once.
    fn glance(&self, args: &Value) -> Result<Value, String> {
        let Some(glance) = glances::get(self.db()?, required(args, "glanceId")?)? else { return Ok(Value::Null) };
        Ok(json!({
            "_id": glance.id,
            "title": glance.title,
            "asOf": glance.as_of,
            "icon": glance.icon,
            "source": glance.source,
            "visualLabel": glance.visual_label,
            "text": glance.text,
            "notes": glance.notes,
            "table": glance.table,
            "wide": glance.wide,
            "artifactId": glance.artifact_id,
            "createdAt": glance.created_at as f64,
            "document": glance.document(),
        }))
    }

    /// Saves a glance as a page in Artifacts, from the conversation it was in; saving it again gives back the same page.
    fn save_glance(&self, args: &Value) -> Result<Value, String> {
        let db = self.db()?;
        let glance = glances::get(db, required(args, "glanceId")?)?.ok_or("This glance isn't there any more.")?;
        if let Some(saved) = glance.artifact_id.as_deref().map(|id| artifacts::get(db, id)).transpose()?.flatten() {
            return Ok(json!({ "artifactId": saved.id, "created": false }));
        }
        // Made by whoever showed it (the conversation's agent, or Filo).
        let author =
            filo_local::feed_mcp::origin_in_chat(&self.root.join(filo_local::db::FILE), &glance.chat_id).author;
        let new = artifacts::New {
            title: glance.title.clone(),
            html: glance.standalone_document(),
            summary: glance.text.clone(),
            kind: Some("page".into()),
        };
        // Kept in the library only, with no card back in the conversation: the glance is there already.
        let saved = artifacts::create(
            db,
            &self.artifacts_dir(),
            &new,
            &Origin { author, chat_id: None, turn_id: None },
            now_ms(),
        )?;
        glances::set_artifact(db, &glance.id, &saved.id)?;
        self.changed();
        Ok(json!({ "artifactId": saved.id, "created": true }))
    }

    /// The cards' heads for a conversation's messages: each message that shows a glance gets its `glance`.
    pub(super) fn with_glances(&self, chat_id: &str, messages: &mut [Value]) {
        let Ok(heads) = self.db().and_then(|db| glances::heads(db, chat_id)) else { return };
        if heads.is_empty() {
            return;
        }
        for message in messages {
            if let Some(head) = message["_id"].as_str().and_then(|id| heads.get(id)) {
                message["glance"] = json!(head);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filo_local::store::{Message, Role};

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("glances")
    }

    fn new() -> glances::New {
        glances::checked(&glances::New {
            title: "Your week".into(),
            html: "<div class=\"chart\"></div>".into(),
            text: "Mon: 3 h".into(),
            as_of: Some("Oct 4".into()),
            icon: Some("chart-line".into()),
            table: Some(glances::Table {
                columns: vec!["Day".into(), "Hours".into()],
                rows: vec![vec!["Mon".into(), "3 h".into()]],
            }),
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn a_glance_shows_on_its_message_opens_and_saves_to_artifacts_once() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        let chat = local.agent_chats.create_chat(filo_local::Engine::Claude).unwrap();
        let new = new();
        let message = Message::new(
            Role::Assistant,
            glances::plain_text(&new.title, new.as_of.as_deref(), &new.text),
            Some("turn-1".into()),
        );
        let glance = local
            .db()
            .unwrap()
            .write(|tx| glances::insert(tx, &new, &chat.id, &message.id, Some("turn-1"), 1_000))
            .unwrap();
        local.agent_chats.append(&chat.id, &message).unwrap();
        local
            .agent_chats
            .append(&chat.id, &Message::new(Role::Assistant, "Busy Monday.", Some("turn-1".into())))
            .unwrap();

        let detail = call("voiceSessions:detail", json!({ "id": chat.id, "limit": 50 })).unwrap();
        let messages = detail["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["glance"],
            json!({ "glanceId": glance.id, "title": "Your week", "asOf": "Oct 4", "icon": "chart-line" })
        );
        assert_eq!(messages[0]["content"], "Your week (Oct 4)\nMon: 3 h");
        assert!(messages[1].get("glance").is_none());

        let got = call("glances:get", json!({ "glanceId": glance.id })).unwrap();
        assert_eq!(
            (got["title"].as_str(), got["text"].as_str(), got["table"]["rows"][0][1].as_str()),
            (Some("Your week"), Some("Mon: 3 h"), Some("3 h"))
        );
        let document = got["document"].as_str().unwrap();
        assert!(
            document.contains("<!--filo:head-->")
                && document.contains("<div id=\"filo-glance\"><div class=\"chart\"></div></div>")
        );
        assert_eq!(call("glances:get", json!({ "glanceId": "missing" })).unwrap(), Value::Null);

        let saved = call("glances:saveToArtifacts", json!({ "glanceId": glance.id })).unwrap();
        assert_eq!(saved["created"], true);
        let again = call("glances:saveToArtifacts", json!({ "glanceId": glance.id })).unwrap();
        assert_eq!((again["created"].as_bool(), &again["artifactId"]), (Some(false), &saved["artifactId"]));
        let page = call("canvasArtifacts:get", json!({ "id": saved["artifactId"] })).unwrap();
        assert!(page["document"].as_str().unwrap().contains("<h1 class=\"filo-title\">Your week</h1>"));
        let id = saved["artifactId"].as_str().unwrap();
        assert_eq!(filo_local::artifacts::get(local.db().unwrap(), id).unwrap().unwrap().author, "Filo");
        // Saving adds no card to the conversation: the glance is there already.
        assert!(
            call("voiceSessions:detail", json!({ "id": chat.id })).unwrap()["tasks"].as_array().unwrap().is_empty()
        );
        assert!(call("glances:saveToArtifacts", json!({ "glanceId": "missing" })).is_err());
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_glance_in_the_group_shows_on_the_members_post() {
        let local = temp();
        let db = local.db().unwrap();
        let session = filo_local::group::ensure(db, 1).unwrap();
        let new = new();
        let posted = filo_local::group::post_with(
            db,
            "chef",
            "Your week (Oct 4)\nMon: 3 h",
            filo_local::group::Asks::default(),
            None,
            2,
            |tx, message| glances::insert(tx, &new, &session, &message.id, None, 2).map(|_| ()),
        )
        .unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let shown =
            detail["messages"].as_array().unwrap().iter().find(|message| message["_id"] == posted.id.as_str()).unwrap();
        assert_eq!((shown["speaker"].as_str(), shown["glance"]["title"].as_str()), (Some("chef"), Some("Your week")));
        std::fs::remove_dir_all(&local.root).ok();
    }
}

// The pictures the user uploaded for their agents (the picture sheet's "Yours", screens/agent_picture.rs): each a round
// 512px PNG, `local/pictures/<id>.png`, listed newest first in Filo's database (crates/filo-local `app_data`, once
// pictures.json). Any agent can show one, saved as its picture `upload:<id>`. Removing one takes it off the list; its
// file goes too unless an agent still shows it.

use base64::Engine as _;
use filo_local::app_data;
use serde_json::{Value, json};

use super::{Local, required};

const PREFIX: &str = "upload:";
const FOLDER: &str = "pictures";
/// A kept picture is far smaller; anything bigger isn't one of the sheet's.
const MAX_BYTES: usize = 4 << 20;

/// An agent's picture for uploaded picture `id`.
pub fn upload_ref(id: &str) -> String {
    format!("{PREFIX}{id}")
}

/// The uploaded picture an agent's picture names, if it names one.
pub fn upload_id_of(picture: &str) -> Option<&str> {
    picture.strip_prefix(PREFIX).filter(|id| uuid::Uuid::parse_str(id).is_ok())
}

/// Uploaded picture `id`'s file, in this Mac's data.
pub fn picture_path(id: &str) -> std::path::PathBuf {
    crate::config::config_dir().join("local").join(FOLDER).join(format!("{id}.png"))
}

impl Local {
    /// `agentPictures:list`: newest first, each with its file.
    pub(super) fn picture_rows(&self) -> Value {
        let list = self.db().and_then(app_data::pictures).unwrap_or_default();
        let folder = self.root.join(FOLDER);
        Value::Array(list.iter().map(|(id, _)| json!({ "id": id, "url": folder.join(format!("{id}.png")) })).collect())
    }

    /// `agentPictures:add`: a cropped picture (`png`, base64) kept and put first on the list; its id.
    pub(super) fn add_picture(&self, args: &Value) -> Result<Value, String> {
        let png = base64::engine::general_purpose::STANDARD
            .decode(required(args, "png")?)
            .map_err(|_| "The picture didn't come through.".to_string())?;
        if png.len() > MAX_BYTES || image::guess_format(&png).ok() != Some(image::ImageFormat::Png) {
            return Err("The picture didn't come through.".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let folder = self.root.join(FOLDER);
        std::fs::create_dir_all(&folder).map_err(|error| format!("Couldn't keep the picture: {error}"))?;
        crate::config::write_atomic(&folder.join(format!("{id}.png")), &png)
            .map_err(|error| format!("Couldn't keep the picture: {error}"))?;
        app_data::add_picture(self.db()?, &id, filo_local::now_ms() as f64)?;
        self.changed();
        Ok(json!(id))
    }

    /// `agentPictures:remove`: off the list; the file too, unless an agent or the group shows it.
    pub(super) fn remove_picture(&self, args: &Value) -> Result<Value, String> {
        let id = required(args, "id")?.to_string();
        if uuid::Uuid::parse_str(&id).is_err() {
            return Err("No such picture.".into());
        }
        app_data::remove_picture(self.db()?, &id)?;
        self.changed();
        let shown = upload_ref(&id);
        let group = self.db().ok().and_then(|db| filo_local::group::look(db).ok()).and_then(|look| look.picture);
        if !self.agents().iter().any(|agent| agent.picture == shown) && group.as_deref() != Some(shown.as_str()) {
            let _ = std::fs::remove_file(self.root.join(FOLDER).join(format!("{id}.png")));
        }
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> String {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(4, 4)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn uploaded_pictures_are_listed_newest_first_and_kept_while_an_agent_shows_one() {
        let local = Local::open(std::env::temp_dir().join(format!("filo-pictures-{}", uuid::Uuid::new_v4())));
        let first = local.call("agentPictures:add", &json!({ "png": png() })).unwrap();
        let second = local.call("agentPictures:add", &json!({ "png": png() })).unwrap();
        let (first, second) = (first.as_str().unwrap().to_string(), second.as_str().unwrap().to_string());
        let ids = |local: &std::sync::Arc<Local>| {
            local
                .call("agentPictures:list", &json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&local), [second.clone(), first.clone()]);
        assert!(local.call("agentPictures:add", &json!({ "png": "bm90IGEgcGljdHVyZQ==" })).is_err());

        let file = |id: &str| local.root.join(FOLDER).join(format!("{id}.png"));
        let agent = local.agents()[0].id.clone();
        local.call("agents:update", &json!({ "id": agent, "picture": upload_ref(&first) })).unwrap();
        local.call("agentPictures:remove", &json!({ "id": first })).unwrap();
        local.call("agentPictures:remove", &json!({ "id": second })).unwrap();
        assert!(ids(&local).is_empty());
        assert!(file(&first).exists(), "an agent still shows it");
        assert!(!file(&second).exists());
        assert_eq!(upload_id_of(&upload_ref(&first)), Some(first.as_str()));
        assert_eq!(upload_id_of("upload:../agents.json"), None);
    }
}

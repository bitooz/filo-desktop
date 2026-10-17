// The user's characters, the pets the agents drew (crates/filo-local characters.rs, the desktop-pet skill): the custom
// avatars the agent picture, pet and App icon pickers offer (`customAvatars:list`), removing one from the agent picture
// sheet (`customAvatars:remove`), and `customAvatars:picked` once the app made a new one the desktop pet (pet_pick.rs).

use filo_local::characters;
use serde_json::{Value, json};

use super::{Local, required};

impl Local {
    /// `customAvatars:list`: newest first, each pose's picture a `file://localhost/…` url (avatars.rs `file_url`).
    pub(super) fn character_rows(&self) -> Value {
        Value::Array(
            characters::list(&self.root)
                .iter()
                .map(|character| characters::row(&self.root, character, crate::avatars::file_url))
                .collect(),
        )
    }

    /// `customAvatars:remove`: off the list, its pictures too. What showed it (an agent's picture, the pet, the App icon)
    /// goes back to its default.
    pub(super) fn remove_character(&self, args: &Value) -> Result<Value, String> {
        characters::remove(&self.root, required(args, "id")?)?;
        self.changed();
        Ok(Value::Null)
    }

    /// `customAvatars:picked`: a new pet the app has made the desktop pet.
    pub(super) fn character_picked(&self, args: &Value) -> Result<Value, String> {
        characters::picked(&self.root, required(args, "id")?)?;
        self.changed();
        Ok(json!(null))
    }
}

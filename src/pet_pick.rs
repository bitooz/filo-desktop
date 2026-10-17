// A pet an agent just drew becomes the desktop pet (the desktop-pet skill's `save_pet`). The MCP server that saved it
// is another process and can't change the app's settings, so it marks the new character (`pickAsPet` in
// `customAvatars:list`, crates/filo-local characters.rs); hearing of it, the app picks it as Settings › Desktop pet
// would, shows the pet, and clears the mark (`customAvatars:picked`).

use gpui::{App, AppContext as _, Entity, Global, Task};
use serde::Deserialize;
use serde_json::json;

use crate::backend;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: String,
    #[serde(default)]
    pick_as_pet: bool,
}

struct PetPick {
    _list: Task<()>,
}

struct PetPickGlobal(#[allow(dead_code)] Entity<PetPick>);

impl Global for PetPickGlobal {}

/// Watches for new pets.
pub fn start(cx: &mut App) {
    if cx.has_global::<PetPickGlobal>() {
        return;
    }
    let watcher = cx.new(|cx| PetPick {
        _list: backend::subscribe(
            cx,
            "customAvatars:list",
            json!({}),
            |_: &mut PetPick, result: Result<Vec<Row>, String>, cx| {
                let Ok(rows) = result else { return };
                let waiting: Vec<String> = rows.into_iter().filter(|row| row.pick_as_pet).map(|row| row.id).collect();
                // Newest first: the last one saved is the pet.
                let Some(newest) = waiting.first() else { return };
                let character = crate::avatars::custom_ref(newest);
                crate::companion::update_settings(cx, |settings| {
                    settings.pet_character = Some(character);
                    settings.pet = true;
                });
                for id in waiting {
                    backend::fire(cx, "customAvatars:picked", json!({ "id": id }));
                }
            },
        ),
    });
    cx.set_global(PetPickGlobal(watcher));
}

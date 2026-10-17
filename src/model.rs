// Shapes shared by several screens. The backend's numbers are JSON numbers, so every number is `f64`; optional fields
// are missing (never null) when unset.

use serde::Deserialize;

/// `users:current`: the user. Each agent's name and picture, the main agent's too, are its own (`agents:list`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub user_name: Option<String>,
}

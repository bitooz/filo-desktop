// Where the desktop app keeps its files. There's no account or hosted backend: everything is on this Mac under
// ~/.config/filo (or `FILO_CONFIG_DIR`), the local backend's data in `local/` (backend.rs).

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The backend's name in the folders below. It's this Mac (backend.rs), which an older app called "local" beside its
/// hosted ones, and the folders keep the name.
const BACKEND: &str = "local";

/// What the app keeps that must not be lost, like a skill not saved yet: ~/.config/filo/offline/local/<user>.
pub fn offline_dir(user_id: &str) -> PathBuf {
    config_dir().join("offline").join(BACKEND).join(file_safe(user_id))
}

fn file_safe(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FILO_CONFIG_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    dirs::home_dir().unwrap_or_default().join(".config/filo")
}

/// Writes a file only this user can read, whole or not at all: a crash mid-write leaves the old one (crates/filo-local
/// files.rs).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    filo_local::files::write_private(path, bytes)
}

/// The app's preferences on this Mac.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub theme_family: String,
    /// "system" | "light" | "dark"
    pub theme_mode: String,
    /// "system" | "standard" | "high"
    pub theme_contrast: String,
    /// Interface typeface and its body size in px; every text style scales from it.
    pub font_family: String,
    pub font_size: f32,
    /// Code blocks and artifact source.
    pub mono_family: String,
    pub mono_size: f32,
}

pub const DEFAULT_FONT: &str = "Atkinson Hyperlegible Next";
pub const DEFAULT_FONT_SIZE: f32 = 15.;
pub const DEFAULT_MONO: &str = "Menlo";
pub const DEFAULT_MONO_SIZE: f32 = 13.;

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme_family: "default".into(),
            theme_mode: "system".into(),
            theme_contrast: "system".into(),
            font_family: DEFAULT_FONT.into(),
            font_size: DEFAULT_FONT_SIZE,
            mono_family: DEFAULT_MONO.into(),
            mono_size: DEFAULT_MONO_SIZE,
        }
    }
}

const PREFS: &str = "desktop.json";

pub fn load_prefs() -> Prefs {
    load_settings(PREFS)
}

pub fn save_prefs(prefs: &Prefs) {
    if let Err(error) = save_settings(PREFS, prefs) {
        eprintln!("settings: {error}");
    }
}

// The app's own settings files (`desktop.json`, `desktop-companion.json`, `desktop-agents.json`…) beside `local/`.
// They're read and saved so that a newer Filo's file is never lost, the lesson of the agents.json wipe: one this build
// can't read (a value it doesn't know, or damage) reads as the defaults and is never saved over, and the keys it
// doesn't know are kept when it saves.

/// A settings file, or the defaults when there's none or it can't be read.
pub fn load_settings<T: DeserializeOwned + Default>(name: &str) -> T {
    load_settings_in(&config_dir(), name)
}

/// Saves a settings file read with `load_settings`, unless the one there can't be read as `T`.
pub fn save_settings<T: Serialize + DeserializeOwned>(name: &str, settings: &T) -> Result<(), String> {
    save_settings_in(&config_dir(), name, settings)
}

/// Sets some keys of a settings file read key by key, the others kept.
pub fn set_settings(name: &str, values: Map<String, Value>) -> Result<(), String> {
    let dir = config_dir();
    write_settings(&dir, name, read_settings(&dir, name)?, values)
}

fn load_settings_in<T: DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    let read = read_settings(dir, name).and_then(|object| match object {
        Some(object) => {
            serde_json::from_value(Value::Object(object)).map_err(|error| format!("Couldn't read {name}: {error}"))
        }
        None => Ok(T::default()),
    });
    read.unwrap_or_else(|error| {
        eprintln!("settings: {error}; the defaults are used, and the file is left as it is");
        T::default()
    })
}

fn save_settings_in<T: Serialize + DeserializeOwned>(dir: &Path, name: &str, settings: &T) -> Result<(), String> {
    let Value::Object(values) = serde_json::to_value(settings).map_err(|error| error.to_string())? else {
        return Err(format!("{name}: not a settings object"));
    };
    let saved = read_settings(dir, name)?;
    if let Some(object) = &saved {
        serde_json::from_value::<T>(Value::Object(object.clone()))
            .map_err(|error| format!("Couldn't read {name}, so it isn't saved over: {error}"))?;
    }
    write_settings(dir, name, saved, values)
}

/// A settings file's object, None when there's none yet, and an error when it's there but isn't a JSON object.
fn read_settings(dir: &Path, name: &str) -> Result<Option<Map<String, Value>>, String> {
    match filo_local::files::read_json(&dir.join(name))? {
        None => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(_) => Err(format!("Couldn't read {name}: not a JSON object")),
    }
}

fn write_settings(
    dir: &Path,
    name: &str,
    saved: Option<Map<String, Value>>,
    values: Map<String, Value>,
) -> Result<(), String> {
    let mut object = saved.unwrap_or_default();
    object.extend(values);
    let text = serde_json::to_string_pretty(&Value::Object(object)).map_err(|error| error.to_string())?;
    write_atomic(&dir.join(name), format!("{text}\n").as_bytes())
        .map_err(|error| format!("Couldn't save {name}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Look {
        mode: Mode,
        size: u32,
    }

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    enum Mode {
        #[default]
        Light,
        Dark,
    }

    #[test]
    fn settings_files_are_never_lost() {
        let dir = std::env::temp_dir().join(format!("filo-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("look.json");
        let set =
            |values: Map<String, Value>| write_settings(&dir, "look.json", read_settings(&dir, "look.json")?, values);

        // A newer Filo's value: read as the defaults, never saved over.
        fs::write(&file, r#"{"mode":"Sepia","size":3}"#).unwrap();
        assert_eq!(load_settings_in::<Look>(&dir, "look.json"), Look::default());
        assert!(save_settings_in(&dir, "look.json", &Look { mode: Mode::Dark, size: 4 }).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), r#"{"mode":"Sepia","size":3}"#);
        assert!(set(Map::new()).is_ok(), "a key-by-key file takes it");

        // A key this build doesn't know is kept.
        fs::write(&file, r#"{"mode":"Light","newer":true}"#).unwrap();
        save_settings_in(&dir, "look.json", &Look { mode: Mode::Dark, size: 4 }).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({ "mode": "Dark", "size": 4, "newer": true }));
        assert_eq!(load_settings_in::<Look>(&dir, "look.json"), Look { mode: Mode::Dark, size: 4 });

        // Damage, or something other than an object, is never saved over either.
        fs::write(&file, "[1, 2").unwrap();
        assert!(set(Map::from_iter([("size".into(), 5.into())])).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "[1, 2");

        // None yet: the defaults, then a new file.
        fs::remove_file(&file).unwrap();
        assert_eq!(load_settings_in::<Look>(&dir, "look.json"), Look::default());
        set(Map::from_iter([("size".into(), 6.into())])).unwrap();
        assert_eq!(load_settings_in::<Look>(&dir, "look.json"), Look { mode: Mode::Light, size: 6 });
        let _ = fs::remove_dir_all(&dir);
    }
}

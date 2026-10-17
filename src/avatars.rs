// The avatars an agent's picture, the pet and the App icon pick from: the built-in animal poses, kept by their id
// ("fox-prop"), and the user's own avatars, `custom:<id>`. The animals' pictures are 400px WebP cut-outs
// (assets/images/filo/avatars), embedded by assets.rs under `avatars/`. On this Mac the
// custom ones are the pets the agents drew (crates/filo-local characters.rs, the desktop-pet skill), their pictures
// files in the local folder, given as `file://localhost/…` urls that the app's HTTP client reads (media.rs).

use std::path::{Path, PathBuf};

use gpui::{ImageSource, Img, ObjectFit, Resource, Styled as _, StyledImage as _, img, px};
use serde::Deserialize;

pub struct Animal {
    pub id: &'static str,
    pub label: &'static str,
    /// (pose, label); the first is the animal's own picture in the picker.
    pub poses: [(&'static str, &'static str); 8],
}

/// The poses in assets/avatars/<animal>; the last one holds each animal's own prop.
const fn standard(prop: &'static str) -> [(&'static str, &'static str); 8] {
    [
        ("idle", "Hello"),
        ("left", "Look left"),
        ("right", "Look right"),
        ("waving", "Waving"),
        ("heart", "Heart"),
        ("laptop", "Laptop"),
        ("sleeping", "Sleepy"),
        ("prop", prop),
    ]
}

pub const ANIMALS: [Animal; 8] = [
    Animal {
        id: "panda",
        label: "Panda",
        poses: [
            ("waving", "Waving"),
            ("heart", "Heart"),
            ("thinking", "Thinking"),
            ("laptop", "Laptop"),
            ("running", "Running"),
            ("sleeping", "Sleepy"),
            ("bamboo", "Bamboo"),
            ("idea", "Idea"),
        ],
    },
    Animal { id: "bear", label: "Bear", poses: standard("Honey") },
    Animal { id: "cat", label: "Cat", poses: standard("Yarn") },
    Animal { id: "dog", label: "Dog", poses: standard("Bone") },
    Animal { id: "fox", label: "Fox", poses: standard("Leaf") },
    Animal { id: "owl", label: "Owl", poses: standard("Feather") },
    Animal { id: "penguin", label: "Penguin", poses: standard("Fish") },
    Animal { id: "rabbit", label: "Rabbit", poses: standard("Carrot") },
];

pub const DEFAULT_AVATAR: &str = "panda-waving";
const CUSTOM_PREFIX: &str = "custom:";

impl Animal {
    pub fn avatar_id(&self, pose: &str) -> String {
        format!("{}-{pose}", self.id)
    }

    /// The picture the picker shows for the animal.
    pub fn first_avatar(&self) -> String {
        self.avatar_id(self.poses[0].0)
    }
}

/// The animal and pose label of a built-in avatar id (`bear-waving`).
fn split(id: &str) -> Option<(&'static Animal, &'static str)> {
    let (animal, pose) = id.split_once('-')?;
    let animal = ANIMALS.iter().find(|item| item.id == animal)?;
    let label = animal.poses.iter().find(|(item, _)| *item == pose)?.1;
    Some((animal, label))
}

pub fn is_avatar_id(id: &str) -> bool {
    split(id).is_some()
}

pub fn animal_of(id: &str) -> Option<&'static Animal> {
    split(id).map(|(animal, _)| animal)
}

/// Switching animal keeps the pose when the new animal has it (waving stays waving), else takes its first.
pub fn avatar_for_animal(animal: &Animal, current: &str) -> String {
    let pose = current.split_once('-').map_or("", |(_, pose)| pose);
    match animal.poses.iter().find(|(item, _)| *item == pose) {
        Some((pose, _)) => animal.avatar_id(pose),
        None => animal.first_avatar(),
    }
}

pub fn custom_ref(id: &str) -> String {
    format!("{CUSTOM_PREFIX}{id}")
}

pub fn custom_id_of(saved: &str) -> Option<&str> {
    saved.strip_prefix(CUSTOM_PREFIX).filter(|id| !id.is_empty())
}

/// A picture on this Mac as a custom avatar's url: `file://localhost/<path>`, which GPUI's loader takes for a url
/// (a bare path would be one too, with no host) and hands to the app's HTTP client (media.rs).
pub fn file_url(path: &Path) -> String {
    match url::Url::from_file_path(path) {
        Ok(url) => url.as_str().replacen("file:///", "file://localhost/", 1),
        Err(()) => String::new(),
    }
}

/// The file a `file_url` names.
pub fn file_of_url(url: &str) -> Option<PathBuf> {
    let url = url::Url::parse(url).ok().filter(|url| url.scheme() == "file")?;
    url.to_file_path().ok()
}

/// The folder the characters are kept in (filo-local characters.rs), this Mac's local data.
fn characters_dir() -> PathBuf {
    crate::config::config_dir().join("local")
}

/// The user's characters, newest first, as their list is now (read again when it changes).
pub fn characters() -> Vec<filo_local::characters::Character> {
    static READ: std::sync::Mutex<Option<(Option<std::time::SystemTime>, Vec<filo_local::characters::Character>)>> =
        std::sync::Mutex::new(None);
    let dir = characters_dir();
    let changed = std::fs::metadata(dir.join("characters.json")).and_then(|meta| meta.modified()).ok();
    let mut read = READ.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if read.as_ref().is_none_or(|(when, _)| *when != changed) {
        *read = Some((changed, filo_local::characters::list(&dir)));
    }
    read.as_ref().map(|(_, list)| list.clone()).unwrap_or_default()
}

/// A character's poses as the App icon names them, in `filo_local::characters::POSES` order.
pub const CHARACTER_POSES: [(&str, &str); 4] =
    [("idle", "Resting"), ("working", "Working"), ("waving", "Waving"), ("sleeping", "Sleeping")];

/// A character's pose as an avatar id: `custom:<id>` for its resting pose, `custom:<id>:<pose>` for the others.
pub fn character_pose_id(id: &str, pose: &str) -> String {
    if pose == CHARACTER_POSES[0].0 { custom_ref(id) } else { format!("{}:{pose}", custom_ref(id)) }
}

/// The character and pose a `character_pose_id` names, while the character exists.
pub fn character_pose(avatar: &str) -> Option<(filo_local::characters::Character, &'static str, &'static str)> {
    let rest = avatar.strip_prefix(CUSTOM_PREFIX)?;
    let (id, pose) = rest.split_once(':').unwrap_or((rest, CHARACTER_POSES[0].0));
    let (pose, label) = CHARACTER_POSES.iter().copied().find(|(item, _)| *item == pose)?;
    let character = characters().into_iter().find(|character| character.id == id)?;
    Some((character, pose, label))
}

/// The picture of an avatar id: an animal's pose or a character's.
pub fn avatar_source(id: &str) -> ImageSource {
    match character_pose(id) {
        Some((character, pose, _)) => ImageSource::Resource(Resource::Path(
            filo_local::characters::pose_path(&characters_dir(), &character.id, pose).into(),
        )),
        None => animal_image(id),
    }
}

/// Its picture's bytes (WebP for an animal, PNG for a character).
pub fn avatar_picture_bytes(id: &str) -> Option<Vec<u8>> {
    match character_pose(id) {
        Some((character, pose, _)) => {
            std::fs::read(filo_local::characters::pose_path(&characters_dir(), &character.id, pose)).ok()
        }
        None => crate::assets::avatar_bytes(id).map(|bytes| bytes.into_owned()),
    }
}

/// One of a custom avatar's pictures (`customAvatars:list`).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CustomPose {
    pub url: String,
}

impl CustomPose {
    /// The picture: a file on this Mac (a path, or a `file://` address; the local folder's characters).
    pub fn image(&self) -> ImageSource {
        file_of_url(&self.url).unwrap_or_else(|| PathBuf::from(&self.url)).into()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CustomPoses {
    pub idle: CustomPose,
    pub working: CustomPose,
    pub waving: CustomPose,
    pub sleeping: CustomPose,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CustomAvatar {
    pub id: String,
    pub name: String,
    pub poses: CustomPoses,
}

/// A character that can change pose (the pet, an App icon): an animal pose in a colour (avatar_colors.rs), or a custom
/// avatar.
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    Animal(String, String),
    Custom(CustomAvatar),
}

impl Look {
    /// An agent's picture as a character: an animal as drawn (blue), or one of the user's own avatars (`custom:<id>`,
    /// in any pose, found in `custom`). A helper's picture or a photo has no poses: none.
    pub fn of_picture(picture: &str, custom: &[CustomAvatar]) -> Option<Look> {
        if is_avatar_id(picture) {
            return Some(Look::Animal(picture.to_string(), crate::avatar_colors::DEFAULT_COLOR.to_string()));
        }
        let id = custom_id_of(picture)?.split(':').next()?;
        custom.iter().find(|avatar| avatar.id == id).cloned().map(Look::Custom)
    }

    /// The waving panda in blue, for no agent or a picture with no poses.
    pub fn default_animal() -> Look {
        Look::Animal(DEFAULT_AVATAR.to_string(), crate::avatar_colors::DEFAULT_COLOR.to_string())
    }

    /// Its picture's reference: an `AvatarId` or `custom:<id>`.
    pub fn reference(&self) -> String {
        match self {
            Look::Animal(id, _) => id.clone(),
            Look::Custom(avatar) => custom_ref(&avatar.id),
        }
    }

    /// "Bear · Waving", or the custom avatar's name.
    pub fn label(&self) -> String {
        match self {
            Look::Animal(id, _) => match split(id) {
                Some((animal, pose)) => format!("{} · {pose}", animal.label),
                None => id.clone(),
            },
            Look::Custom(avatar) => avatar.name.clone(),
        }
    }
}

pub fn animal_image(id: &str) -> ImageSource {
    ImageSource::Resource(Resource::Embedded(format!("avatars/{id}.webp").into()))
}

/// A picture of Filo fitted in a `size` square.
pub fn picture(source: ImageSource, size: f32) -> Img {
    img(source).size(px(size)).flex_none().object_fit(ObjectFit::Contain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_labels() {
        assert!(is_avatar_id("panda-waving"));
        assert!(is_avatar_id("bear-prop"));
        assert!(!is_avatar_id("panda-prop"));
        assert!(!is_avatar_id("custom:abc"));
        assert_eq!(Look::Animal("owl-prop".into(), "blue".into()).label(), "Owl · Feather");
        assert_eq!(custom_id_of("custom:abc"), Some("abc"));
        assert_eq!(custom_id_of("custom:"), None);
        assert_eq!(character_pose_id("abc", "idle"), "custom:abc");
        assert_eq!(character_pose_id("abc", "sleeping"), "custom:abc:sleeping");
    }

    #[test]
    fn a_picture_on_this_mac_is_a_url_gpui_loads() {
        let path = Path::new("/Users/me/.config/filo/local/characters/1b4e28ba-2fa1-11d2-883f-0016d3cca427/idle.png");
        let url = file_url(path);
        assert_eq!(
            url,
            "file://localhost/Users/me/.config/filo/local/characters/1b4e28ba-2fa1-11d2-883f-0016d3cca427/idle.png"
        );
        // GPUI takes a string for a url when it parses as one; a bare path would too, with no host to fetch from.
        assert!(url.parse::<gpui_http_client::http::Uri>().is_ok());
        assert_eq!(file_of_url(&url).as_deref(), Some(path));
        let spaced = Path::new("/Users/me/My Pets/idle.png");
        assert_eq!(file_of_url(&file_url(spaced)).as_deref(), Some(spaced));
        assert_eq!(file_of_url("https://example.com/idle.png"), None);
    }

    #[test]
    fn a_custom_pose_on_this_mac_loads_from_its_file() {
        let source = |url: &str| match (CustomPose { url: url.into() }).image() {
            ImageSource::Resource(Resource::Path(path)) => format!("path {}", path.display()),
            ImageSource::Resource(Resource::Uri(uri)) => format!("uri {uri}"),
            _ => "other".into(),
        };
        assert_eq!(
            source("/Users/me/.config/filo/local/characters/a/idle.png"),
            "path /Users/me/.config/filo/local/characters/a/idle.png"
        );
        assert_eq!(source("file:///tmp/idle.png"), "path /tmp/idle.png");
        assert_eq!(source("file://localhost/tmp/My%20Pets/idle.png"), "path /tmp/My Pets/idle.png");
    }

    #[test]
    fn switching_animal_keeps_the_pose() {
        assert_eq!(avatar_for_animal(&ANIMALS[1], "panda-waving"), "bear-waving");
        assert_eq!(avatar_for_animal(&ANIMALS[0], "bear-left"), "panda-waving");
        assert_eq!(avatar_for_animal(&ANIMALS[2], "panda-sleeping"), "cat-sleeping");
    }

    #[test]
    fn every_animal_has_its_pictures() {
        for animal in &ANIMALS {
            for (pose, _) in &animal.poses {
                let id = animal.avatar_id(pose);
                assert!(crate::assets::avatar_bytes(&id).is_some(), "missing picture for {id}");
            }
        }
    }
}

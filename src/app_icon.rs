// Filo's app icon, picked in Settings › App icon: Filo's mark on a macOS-style
// rounded square in one of its colours, one of the avatars (any animal in any pose, or a pet the agents drew in any
// of its poses) on a light square or one in an avatar colour, or one of the pictures the user uploaded, cropped to the square. It's set on the Dock while Filo runs (NSApplication's
// applicationIconImage, which keeps the unseen badge). The pick is kept in desktop-app-icon.json (the uploaded
// pictures beside it), and the drawn icon in the config folder, so the next launch shows it at once, and scripts/app.sh makes
// it Filo.app's own icon (Finder, banners).

#![allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare

use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui::App;
use image::imageops::{self, FilterType};
use image::{ImageFormat, Rgba, RgbaImage};
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use serde::{Deserialize, Serialize};

use crate::avatars::Look;

const SIZE: u32 = 1024;
/// Apple's icon grid: an 824 px rounded square centred on the 1024 px canvas.
const BODY: f32 = 824.;
const CORNER: f32 = 185.4;
/// The box the figure is fitted in (trimmed to what it draws, so roomy cut-outs come out as big as tight ones),
/// a little below the middle so it sits rather than floats.
const AVATAR: u32 = 620;
const AVATAR_DROP: i64 = 16;
/// A soft light ground that keeps black ears and dark figures visible: white at the top, cool grey below.
pub const TOP: [f32; 3] = [252., 252., 254.];
pub const BOTTOM: [f32; 3] = [221., 228., 240.];
/// How much lighter a coloured ground is at the top than at the bottom (a share of the way to white).
const GROUND_LIFT: f32 = 0.22;
/// The figure's box as a share of the rounded square, and how far below the middle it sits (for drawing it in the app).
pub const AVATAR_SHARE: f32 = AVATAR as f32 / BODY;
pub const AVATAR_DROP_SHARE: f32 = AVATAR_DROP as f32 / BODY;
const SHADOW_OFFSET: f32 = 10.;
const SHADOW_BLUR: f32 = 14.;
const SHADOW_ALPHA: f32 = 0.3;

/// Where the Dock icon is kept for the bundle's icon (scripts/app.sh reads it). Its name is from when each backend kept
/// its own.
pub fn cache_path() -> PathBuf {
    crate::config::config_dir().join("app-icon-local.png")
}

/// The look's resting picture: the embedded animal, or the custom avatar's PNG from this Mac.
pub fn picture(look: Look) -> Result<Vec<u8>> {
    match look {
        Look::Animal(id, color) => crate::assets::avatar_color_bytes(&color, &id)
            .or_else(|| crate::assets::avatar_bytes(&id))
            .map(|bytes| bytes.into_owned())
            .context("no picture for this avatar"),
        Look::Custom(avatar) => {
            let path = crate::avatars::file_of_url(&avatar.poses.idle.url)
                .context("the avatar's picture isn't on this Mac")?;
            Ok(std::fs::read(path)?)
        }
    }
}

/// How much of the pixel at (x, y) the rounded square `offset_y` px lower covers, 0 to 1.
fn body_coverage(x: u32, y: u32, offset_y: f32) -> f32 {
    let half = BODY / 2.;
    let centre = SIZE as f32 / 2.;
    let qx = (x as f32 + 0.5 - centre).abs() - (half - CORNER);
    let qy = (y as f32 + 0.5 - centre - offset_y).abs() - (half - CORNER);
    let outside = (qx.max(0.).powi(2) + qy.max(0.).powi(2)).sqrt();
    let distance = outside + qx.max(qy).min(0.) - CORNER;
    (0.5 - distance).clamp(0., 1.)
}

/// The picture cropped to its visible pixels.
fn trimmed(picture: RgbaImage) -> RgbaImage {
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in picture.enumerate_pixels() {
        if pixel[3] > 12 {
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
        }
    }
    if left > right {
        return picture;
    }
    imageops::crop_imm(&picture, left, top, right - left + 1, bottom - top + 1).to_image()
}

/// Where an avatar's figure is in its picture, for drawing the icon in the app as `render` does: the figure's larger
/// side as a share of the picture's, and its middle's offset from the picture's (shares of its width and height).
/// Measured once per avatar.
pub fn avatar_figure(id: &str) -> (f32, f32, f32) {
    static MEASURED: std::sync::Mutex<Option<std::collections::HashMap<String, (f32, f32, f32)>>> =
        std::sync::Mutex::new(None);
    let mut measured = MEASURED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *measured.get_or_insert_with(Default::default).entry(id.to_string()).or_insert_with(|| {
        let Some(picture) =
            crate::avatars::avatar_picture_bytes(id).and_then(|bytes| image::load_from_memory(&bytes).ok())
        else {
            return (1., 0., 0.);
        };
        let picture = picture.to_rgba8();
        let (width, height) = (picture.width() as f32, picture.height() as f32);
        let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
        for (x, y, pixel) in picture.enumerate_pixels() {
            if pixel[3] > 12 {
                (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x), bottom.max(y));
            }
        }
        if left > right {
            return (1., 0., 0.);
        }
        let share = ((right - left + 1) as f32).max((bottom - top + 1) as f32) / width.max(height);
        (share, (left + right + 1) as f32 / 2. / width - 0.5, (top + bottom + 1) as f32 / 2. / height - 0.5)
    })
}

/// The 1024 px icon (PNG) for an avatar picture (PNG or WebP), on the light ground.
pub fn render(picture: &[u8]) -> Result<Vec<u8>> {
    render_on(picture, None)
}

/// A character's ground, top and bottom (0–255): the light one, or an avatar colour (`purple`) a little lighter at
/// the top.
pub fn ground(color: Option<&str>) -> ([f32; 3], [f32; 3]) {
    let swatch = color
        .and_then(|color| crate::avatar_colors::COLORS.iter().find(|(id, _, _)| *id == color))
        .map(|(_, hex, _)| *hex);
    let Some(rgb) = swatch.and_then(|hex| u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()) else {
        return (TOP, BOTTOM);
    };
    let bottom = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8].map(f32::from);
    (bottom.map(|channel| channel + (255. - channel) * GROUND_LIFT), bottom)
}

/// The 1024 px icon (PNG) for an avatar picture on its ground (`ground`).
pub fn render_on(picture: &[u8], color: Option<&str>) -> Result<Vec<u8>> {
    let avatar = trimmed(image::load_from_memory(picture).context("unreadable avatar picture")?.to_rgba8());
    let avatar = image::DynamicImage::ImageRgba8(avatar).resize(AVATAR, AVATAR, FilterType::Lanczos3).to_rgba8();
    let mut layer = RgbaImage::new(SIZE, SIZE);
    let left = i64::from((SIZE - avatar.width()) / 2);
    let top = i64::from((SIZE - avatar.height()) / 2) + AVATAR_DROP;
    imageops::overlay(&mut layer, &avatar, left, top);
    compose(&layer, ground(color))
}

/// The 1024 px icon (PNG) for a picture of the user's own: it fills the rounded square (a square one fits exactly;
/// another is cropped to its middle).
pub fn render_picture(picture: &[u8]) -> Result<Vec<u8>> {
    let side = BODY as u32;
    let picture = image::load_from_memory(picture).context("unreadable picture")?;
    let picture = picture.resize_to_fill(side, side, FilterType::Lanczos3).to_rgba8();
    let mut layer = RgbaImage::new(SIZE, SIZE);
    let inset = i64::from((SIZE - side) / 2);
    imageops::overlay(&mut layer, &picture, inset, inset);
    compose(&layer, (TOP, BOTTOM))
}

/// `layer` (a 1024 px canvas) cut to the rounded square, over the ground (top to bottom), with the square's soft
/// shadow.
fn compose(layer: &RgbaImage, (top, bottom): ([f32; 3], [f32; 3])) -> Result<Vec<u8>> {
    let shadow = RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        Rgba([0, 0, 0, (body_coverage(x, y, SHADOW_OFFSET) * SHADOW_ALPHA * 255.).round() as u8])
    });
    let shadow = imageops::fast_blur(&shadow, SHADOW_BLUR);

    let icon = RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let coverage = body_coverage(x, y, 0.);
        let shade = f32::from(shadow.get_pixel(x, y)[3]) / 255.;
        let alpha = coverage + shade * (1. - coverage);
        if alpha <= 0. {
            return Rgba([0, 0, 0, 0]);
        }
        let t = y as f32 / (SIZE - 1) as f32;
        let picture = layer.get_pixel(x, y);
        let over = f32::from(picture[3]) / 255.;
        let channel = |index: usize| {
            let ground = top[index] + (bottom[index] - top[index]) * t;
            let colour = f32::from(picture[index]) * over + ground * (1. - over);
            // Over the shadow, which is black.
            (colour * coverage / alpha).round().clamp(0., 255.) as u8
        };
        Rgba([channel(0), channel(1), channel(2), (alpha * 255.).round() as u8])
    });
    let mut png = Vec::new();
    icon.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

/// Sets the Dock icon; None goes back to the app's own.
fn set_dock_icon(png: Option<&[u8]>) {
    // SAFETY: AppKit messages on the main thread; the image is released once the app holds it.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let Some(png) = png else {
            let _: () = msg_send![app, setApplicationIconImage: std::ptr::null_mut::<Object>()];
            return;
        };
        let data: *mut Object = msg_send![class!(NSData), dataWithBytes: png.as_ptr() length: png.len()];
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: data];
        if image.is_null() {
            return;
        }
        let _: () = msg_send![app, setApplicationIconImage: image];
        let _: () = msg_send![image, release];
    }
}

/// What the Dock shows now, as PNG (smoke test).
#[cfg(feature = "dev")]
pub fn dock_icon_png() -> Option<Vec<u8>> {
    // SAFETY: read-only AppKit messages on the main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let image: *mut Object = msg_send![app, applicationIconImage];
        if image.is_null() {
            return None;
        }
        let tiff: *mut Object = msg_send![image, TIFFRepresentation];
        let rep: *mut Object = msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff];
        if rep.is_null() {
            return None;
        }
        let properties: *mut Object = msg_send![class!(NSDictionary), dictionary];
        // NSBitmapImageFileTypePNG
        let png: *mut Object = msg_send![rep, representationUsingType: 4usize properties: properties];
        if png.is_null() {
            return None;
        }
        let length: usize = msg_send![png, length];
        let bytes: *const u8 = msg_send![png, bytes];
        Some(std::slice::from_raw_parts(bytes, length).to_vec())
    }
}

/// At launch, before the profile loads: the icon from last time.
pub fn restore() {
    if let Ok(png) = std::fs::read(cache_path()) {
        set_dock_icon(Some(&png));
    }
}

// --- The icons to pick from ---

/// One of the icons: Filo's mark (`mark`) on a rounded square (`ground`), colours as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
    pub ground: u32,
    pub mark: u32,
}

/// Filo's own first (the default), then the same face in seven other colours.
pub const CHOICES: [Choice; 8] = [
    Choice { id: "filo", label: "Filo", ground: 0x141416, mark: 0xececec },
    Choice { id: "light", label: "Light", ground: 0xf4f4f2, mark: 0x17171a },
    Choice { id: "midnight", label: "Midnight", ground: 0x1d2a4a, mark: 0xe9eefc },
    Choice { id: "clay", label: "Clay", ground: 0xd9845a, mark: 0xfff5ee },
    Choice { id: "sage", label: "Sage", ground: 0x5f8f72, mark: 0xf1f8f2 },
    Choice { id: "lilac", label: "Lilac", ground: 0x8a74d9, mark: 0xf6f2ff },
    Choice { id: "graphite", label: "Graphite", ground: 0x3b3d44, mark: 0xc9cbd2 },
    Choice { id: "sun", label: "Sun", ground: 0xe9b949, mark: 0x231c0e },
];

/// How much of the rounded square the mark spans, and the square's corner as a share of its side (Apple's grid).
pub const MARK_SHARE: f32 = 0.6;
pub const CORNER_SHARE: f32 = CORNER / BODY;

impl Choice {
    pub fn by_id(id: &str) -> Option<Choice> {
        CHOICES.iter().copied().find(|choice| choice.id == id)
    }

    pub fn is_default(self) -> bool {
        self.id == CHOICES[0].id
    }
}

/// What the icon shows: Filo's mark in one of its colours, an avatar (`bear-waving`, in its own colours, or a pet's pose,
/// `custom:<id>:working`, avatars.rs `character_pose_id`) on the light ground (None) or an avatar colour's (`purple`), or
/// a picture the user uploaded (its file in the config folder, `desktop-app-icon-<ms>.png`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    Mark(Choice),
    Avatar(String, Option<&'static str>),
    Picture(String),
}

/// Between an avatar's id and its ground's in the icon's id (`owl-prop+purple`).
const GROUND_SEPARATOR: char = '+';

/// An avatar colour's id as kept (`purple`), if it is one.
pub fn ground_color(id: &str) -> Option<&'static str> {
    crate::avatar_colors::COLORS.iter().map(|(color, _, _)| *color).find(|color| *color == id)
}

/// A ground's name: Light, or the colour's (Purple).
pub fn ground_label(color: Option<&str>) -> String {
    let color = color.unwrap_or("light");
    let mut letters = color.chars();
    letters.next().map(|first| first.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

/// What an uploaded picture's file is called: the prefix, a number, the suffix.
const PICTURE_PREFIX: &str = "desktop-app-icon-";
const PICTURE_SUFFIX: &str = ".png";
/// The one uploaded picture as an older Filo kept the pick (`picture` beside it).
const OLD_PICTURE_ID: &str = "picture";

fn is_picture_name(name: &str) -> bool {
    name.strip_prefix(PICTURE_PREFIX)
        .and_then(|rest| rest.strip_suffix(PICTURE_SUFFIX))
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

impl Default for Icon {
    fn default() -> Self {
        Icon::Mark(CHOICES[0])
    }
}

impl Icon {
    /// A colour's id (`sage`), an avatar's (`bear-waving`, or `bear-waving+purple` on a colour) or an uploaded
    /// picture's file name.
    pub fn by_id(id: &str) -> Option<Icon> {
        if let Some(choice) = Choice::by_id(id) {
            return Some(Icon::Mark(choice));
        }
        let (avatar, color) = match id.split_once(GROUND_SEPARATOR) {
            Some((avatar, color)) => (avatar, Some(ground_color(color)?)),
            None => (id, None),
        };
        if crate::avatars::is_avatar_id(avatar) || crate::avatars::character_pose(avatar).is_some() {
            return Some(Icon::Avatar(avatar.to_string(), color));
        }
        is_picture_name(id).then(|| Icon::Picture(id.to_string()))
    }

    pub fn id(&self) -> String {
        match self {
            Icon::Mark(choice) => choice.id.to_string(),
            Icon::Avatar(id, None) => id.clone(),
            Icon::Avatar(id, Some(color)) => format!("{id}{GROUND_SEPARATOR}{color}"),
            Icon::Picture(name) => name.clone(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Icon::Mark(choice) => choice.label.to_string(),
            Icon::Avatar(id, color) => {
                let avatar = match crate::avatars::character_pose(id) {
                    Some((character, _, pose)) => format!("{} · {pose}", character.name),
                    None => Look::Animal(id.clone(), crate::avatar_colors::DEFAULT_COLOR.into()).label(),
                };
                match color {
                    Some(_) => format!("{avatar} · {}", ground_label(*color)),
                    None => avatar,
                }
            }
            Icon::Picture(name) => picture_label(name),
        }
    }
}

/// An uploaded picture's name: "Your picture" while it's the only one, else "Picture 2" by its place among them.
pub fn picture_label(name: &str) -> String {
    let pictures = pictures();
    match pictures.iter().position(|item| item == name) {
        Some(index) if pictures.len() > 1 => format!("Picture {}", index + 1),
        _ => "Your picture".to_string(),
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    icon: String,
    /// The uploaded pictures' files in the config folder, oldest first, kept while another icon is picked so they can
    /// be picked again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pictures: Vec<String>,
    /// An older Filo's one uploaded picture (read only).
    #[serde(default, skip_serializing)]
    picture: Option<String>,
}

fn saved_path() -> PathBuf {
    crate::config::config_dir().join("desktop-app-icon.json")
}

#[derive(Clone, Default)]
struct Picked {
    icon: Icon,
    pictures: Vec<String>,
}

static CHOSEN: std::sync::Mutex<Option<Picked>> = std::sync::Mutex::new(None);

fn picked() -> Picked {
    let mut chosen = CHOSEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    chosen
        .get_or_insert_with(|| {
            let saved = std::fs::read(saved_path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
                .unwrap_or_default();
            loaded(saved, |name| picture_path(name).is_file())
        })
        .clone()
}

/// The pick as kept, an older Filo's one picture among the pictures; pictures whose file is gone are dropped (and
/// the icon, if it was one of them).
fn loaded(saved: Saved, exists: impl Fn(&str) -> bool) -> Picked {
    let mut pictures: Vec<String> = Vec::new();
    for name in saved.pictures.into_iter().chain(saved.picture.clone()) {
        if is_picture_name(&name) && exists(&name) && !pictures.contains(&name) {
            pictures.push(name);
        }
    }
    let icon = match (saved.icon.as_str(), saved.picture) {
        (OLD_PICTURE_ID, Some(old)) => Some(Icon::Picture(old)),
        (id, _) => Icon::by_id(id),
    };
    let icon = icon.filter(|icon| !matches!(icon, Icon::Picture(name) if !pictures.contains(name))).unwrap_or_default();
    Picked { icon, pictures }
}

fn keep(picked: Picked) {
    let saved = Saved { icon: picked.icon.id(), pictures: picked.pictures.clone(), picture: None };
    *CHOSEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(picked);
    if let Ok(text) = serde_json::to_vec_pretty(&saved) {
        let _ = crate::config::write_atomic(&saved_path(), &text);
    }
}

/// The icon picked, or Filo's own.
pub fn chosen() -> Icon {
    picked().icon
}

/// The pictures the user uploaded (square PNGs, their file names), oldest first, whether or not one is the icon now.
pub fn pictures() -> Vec<String> {
    picked().pictures
}

/// An uploaded picture's file.
pub fn picture_path(name: &str) -> PathBuf {
    crate::config::config_dir().join(name)
}

/// Filo's mark (icons/filo-mark.svg) as the SVG elements inside its root, for drawing it into the icon.
fn mark_elements() -> Result<String> {
    let svg = crate::assets::Embedded::get("icons/filo-mark.svg").context("no filo-mark.svg")?.data;
    let svg = std::str::from_utf8(&svg)?;
    let start = svg.find("<g").context("filo-mark.svg has no group")?;
    let end = svg.rfind("</svg>").context("filo-mark.svg isn't closed")?;
    Ok(svg[start..end].to_string())
}

/// The 1024 px icon (PNG) for a choice: the square on Apple's grid with its soft shadow, the mark centred on it.
pub fn render_choice(choice: Choice) -> Result<Vec<u8>> {
    let side = BODY * MARK_SHARE;
    let offset = (SIZE as f32 - side) / 2.;
    let inset = (SIZE as f32 - BODY) / 2.;
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}" viewBox="0 0 {SIZE} {SIZE}">
<defs><filter id="shadow" x="-10%" y="-10%" width="120%" height="130%"><feGaussianBlur stdDeviation="{SHADOW_BLUR}"/></filter></defs>
<rect x="{inset}" y="{shadow_y}" width="{BODY}" height="{BODY}" rx="{CORNER}" fill="#000" fill-opacity="{SHADOW_ALPHA}" filter="url(#shadow)"/>
<rect x="{inset}" y="{inset}" width="{BODY}" height="{BODY}" rx="{CORNER}" fill="#{ground:06x}"/>
<rect x="{edge}" y="{edge}" width="{edge_side}" height="{edge_side}" rx="{CORNER}" fill="none" stroke="#000" stroke-opacity="0.08" stroke-width="2"/>
<g fill="#{mark:06x}" transform="translate({offset} {offset}) scale({scale})">{elements}</g>
</svg>"##,
        shadow_y = inset + SHADOW_OFFSET,
        ground = choice.ground,
        edge = inset + 1.,
        edge_side = BODY - 2.,
        mark = choice.mark,
        scale = side / 512.,
        elements = mark_elements()?,
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE).context("no pixmap")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    let pixels = pixmap.pixels().iter().flat_map(|pixel| {
        let pixel = pixel.demultiply();
        [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
    });
    let icon = RgbaImage::from_raw(SIZE, SIZE, pixels.collect()).context("icon size")?;
    let mut png = Vec::new();
    icon.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    Ok(png)
}

/// The 1024 px icon (PNG) for any icon.
pub fn render_icon(icon: &Icon) -> Result<Vec<u8>> {
    match icon {
        Icon::Mark(choice) => render_choice(*choice),
        Icon::Avatar(id, color) => {
            render_on(&crate::avatars::avatar_picture_bytes(id).context("no picture for this avatar")?, *color)
        }
        Icon::Picture(name) => render_picture(&std::fs::read(picture_path(name)).context("no such picture")?),
    }
}

/// Picks an icon: kept for next time, drawn off the main thread, set on the Dock and kept for the bundle. A picture
/// needs to be one uploaded (`keep_picture`).
pub fn choose(icon: Icon, cx: &mut App) {
    let mut picked = picked();
    if matches!(&icon, Icon::Picture(name) if !picked.pictures.contains(name)) {
        return;
    }
    picked.icon = icon;
    keep(picked);
    show(cx);
    // What draws it in the app (the sidebar's logo, Settings) shows the new one.
    cx.refresh_windows();
}

/// An uploaded picture, cropped square (PNG): kept in the config folder after the others, and picked.
pub fn keep_picture(png: &[u8], cx: &mut App) -> Result<()> {
    // A new name each time, so the app's image cache doesn't show an earlier one.
    let name = format!(
        "{PICTURE_PREFIX}{}{PICTURE_SUFFIX}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
    );
    crate::config::write_atomic(&picture_path(&name), png)?;
    let mut picked = picked();
    picked.pictures.retain(|item| *item != name);
    picked.pictures.push(name.clone());
    picked.icon = Icon::Picture(name);
    keep(picked);
    show(cx);
    cx.refresh_windows();
    Ok(())
}

/// Removes an uploaded picture, its file too; if it was the icon, Filo's own takes its place.
pub fn remove_picture(name: &str, cx: &mut App) {
    let mut picked = picked();
    if !picked.pictures.iter().any(|item| item == name) {
        return;
    }
    picked.pictures.retain(|item| item != name);
    let was_icon = picked.icon == Icon::Picture(name.to_string());
    if was_icon {
        picked.icon = Icon::default();
    }
    keep(picked);
    let _ = std::fs::remove_file(picture_path(name));
    if was_icon {
        show(cx);
    }
    cx.refresh_windows();
}

/// Bumped on every pick, so a slower drawing of an earlier pick doesn't land after a later one.
static SHOWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Draws the picked icon, sets it on the Dock and keeps it. At launch, after `restore`, it sets it if the one kept
/// from last time differs (an older Filo kept the avatar's).
pub fn show(cx: &mut App) {
    use std::sync::atomic::Ordering;
    let turn = SHOWN.fetch_add(1, Ordering::SeqCst) + 1;
    let icon = chosen();
    cx.spawn(async move |cx| {
        let drawn = cx.background_executor().spawn(async move { render_icon(&icon) }).await;
        if SHOWN.load(Ordering::SeqCst) != turn {
            return;
        }
        match drawn {
            Ok(png) => {
                let _ = cx.update(|_| set_dock_icon(Some(&png)));
                let path = cache_path();
                if std::fs::read(&path).ok().as_ref() != Some(&png)
                    && let Err(error) = crate::config::write_atomic(&path, &png)
                {
                    eprintln!("app icon: couldn't keep it: {error}");
                }
            }
            Err(error) => eprintln!("app icon: {error:#}"),
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_choice_draws_its_colours() {
        for choice in CHOICES {
            let png = render_choice(choice).expect("icon");
            let image = image::load_from_memory(&png).expect("png").to_rgba8();
            assert_eq!(image.dimensions(), (SIZE, SIZE));
            assert_eq!(image.get_pixel(0, 0)[3], 0, "{}: the corner stays clear", choice.id);
            let rgb = |value: u32| [(value >> 16) as u8, (value >> 8) as u8, value as u8];
            let near =
                |pixel: &Rgba<u8>, want: [u8; 3]| pixel[3] == 255 && (0..3).all(|i| pixel[i].abs_diff(want[i]) <= 2);
            // Just inside the square's left edge, halfway down: the ground.
            assert!(near(image.get_pixel(130, SIZE / 2), rgb(choice.ground)), "{}: ground", choice.id);
            assert!(
                image.pixels().filter(|pixel| near(pixel, rgb(choice.mark))).count() > 50_000,
                "{}: the mark",
                choice.id
            );
        }
    }

    #[test]
    fn filo_is_the_default() {
        assert!(CHOICES[0].is_default());
        assert_eq!(Icon::default(), Icon::Mark(CHOICES[0]));
        assert_eq!(Choice::by_id("sage").map(|choice| choice.label), Some("Sage"));
        assert_eq!(Choice::by_id("panda"), None);
    }

    #[test]
    fn icons_by_id() {
        assert_eq!(Icon::by_id("sage"), Some(Icon::Mark(Choice::by_id("sage").unwrap())));
        assert_eq!(Icon::by_id("owl-prop"), Some(Icon::Avatar("owl-prop".into(), None)));
        assert_eq!(Icon::by_id("owl-prop+purple"), Some(Icon::Avatar("owl-prop".into(), Some("purple"))));
        assert_eq!(Icon::by_id("owl-prop+plaid"), None);
        assert_eq!(Icon::by_id("owl-plaid+purple"), None);
        assert_eq!(
            Icon::by_id("desktop-app-icon-1760263282000.png"),
            Some(Icon::Picture("desktop-app-icon-1760263282000.png".into()))
        );
        assert_eq!(Icon::by_id("picture"), None);
        assert_eq!(Icon::by_id("desktop-app-icon-.png"), None);
        assert_eq!(Icon::by_id("desktop-app-icon-1/../x.png"), None);
        assert_eq!(Icon::by_id("panda"), None);
        for icon in [
            Icon::by_id("lilac").unwrap(),
            Icon::Avatar("bear-waving".into(), None),
            Icon::Avatar("bear-waving".into(), Some("teal")),
            Icon::Picture("desktop-app-icon-7.png".into()),
        ] {
            assert_eq!(Icon::by_id(&icon.id()), Some(icon.clone()));
        }
        assert_eq!(Icon::Avatar("owl-prop".into(), None).label(), "Owl · Feather");
        assert_eq!(Icon::Avatar("owl-prop".into(), Some("green")).label(), "Owl · Feather · Green");
    }

    #[test]
    fn the_kept_pick_loads() {
        let saved = |json: &str| serde_json::from_str::<Saved>(json).unwrap();
        let old = "desktop-app-icon-1.png";
        // An older Filo's one picture, picked.
        let picked = loaded(saved(r#"{"icon":"picture","picture":"desktop-app-icon-1.png"}"#), |_| true);
        assert_eq!((picked.icon, picked.pictures), (Icon::Picture(old.into()), vec![old.to_string()]));
        // Kept while another icon is picked.
        let picked = loaded(saved(r#"{"icon":"sage","picture":"desktop-app-icon-1.png"}"#), |_| true);
        assert_eq!((picked.icon, picked.pictures), (Icon::by_id("sage").unwrap(), vec![old.to_string()]));
        // Several; one whose file is gone drops out, and Filo's own takes its place if it was the icon.
        let json = r#"{"icon":"desktop-app-icon-2.png","pictures":["desktop-app-icon-1.png","desktop-app-icon-2.png","../x.png"]}"#;
        let picked = loaded(saved(json), |name| name != "desktop-app-icon-2.png");
        assert_eq!((picked.icon, picked.pictures), (Icon::default(), vec![old.to_string()]));
        // Written back without the old field.
        let text =
            serde_json::to_string(&Saved { icon: old.into(), pictures: vec![old.into()], picture: Some(old.into()) })
                .unwrap();
        assert_eq!(text, r#"{"icon":"desktop-app-icon-1.png","pictures":["desktop-app-icon-1.png"]}"#);
    }

    #[test]
    fn an_avatar_sits_on_the_light_square() {
        let png = render_icon(&Icon::Avatar("bear-waving".into(), None)).expect("icon");
        let image = image::load_from_memory(&png).expect("png").to_rgba8();
        assert_eq!(image.dimensions(), (SIZE, SIZE));
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        // Just inside the square's left edge, clear of the figure: the light ground.
        let ground = image.get_pixel(130, SIZE / 2);
        assert!(ground[3] == 255 && ground[0] > 220 && ground[2] > 230, "{ground:?}");
        // On green (#3FBF7F): green there, and nearly the colour itself low down.
        let png = render_icon(&Icon::Avatar("bear-waving".into(), Some("green"))).expect("icon");
        let image = image::load_from_memory(&png).expect("png").to_rgba8();
        let ground = image.get_pixel(130, SIZE / 2);
        assert!(ground[3] == 255 && ground[1] > ground[0] + 60 && ground[1] > ground[2] + 30, "{ground:?}");
        let low = image.get_pixel(130, 800);
        assert!(low[0].abs_diff(0x3F) <= 12 && low[1].abs_diff(0xBF) <= 12, "{low:?}");
    }

    #[test]
    fn avatars_are_measured_as_the_icon_trims_them() {
        for id in ["penguin-waving", "panda-idea", "owl-prop"] {
            let (share, dx, dy) = avatar_figure(id);
            let picture = image::load_from_memory(&crate::assets::avatar_bytes(id).unwrap()).unwrap().to_rgba8();
            let figure = trimmed(picture.clone());
            assert_eq!(
                share,
                figure.width().max(figure.height()) as f32 / picture.width().max(picture.height()) as f32,
                "{id}"
            );
            assert!(share > 0.5 && share <= 1. && dx.abs() < 0.2 && dy.abs() < 0.2, "{id}: {share} {dx} {dy}");
        }
        assert_eq!(avatar_figure("nobody-here"), (1., 0., 0.));
    }

    #[test]
    fn a_picture_fills_the_rounded_square() {
        let red = RgbaImage::from_pixel(600, 400, Rgba([200, 30, 30, 255]));
        let mut source = Vec::new();
        red.write_to(&mut Cursor::new(&mut source), ImageFormat::Png).unwrap();
        let image = image::load_from_memory(&render_picture(&source).expect("icon")).expect("png").to_rgba8();
        let inset = (SIZE - BODY as u32) / 2;
        // The square's corners are cut round; its edges and middle are the picture.
        assert!(image.get_pixel(inset + 2, inset + 2)[3] < 255);
        for (x, y) in [(SIZE / 2, SIZE / 2), (inset + 2, SIZE / 2), (SIZE / 2, inset + 2)] {
            let pixel = image.get_pixel(x, y);
            assert!(pixel[3] == 255 && pixel[0].abs_diff(200) <= 2 && pixel[1].abs_diff(30) <= 2, "{x},{y}: {pixel:?}");
        }
        assert_eq!(image.get_pixel(10, SIZE / 2)[3], 0);
    }
}

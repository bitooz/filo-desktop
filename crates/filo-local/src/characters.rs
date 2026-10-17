// The user's own characters: pets an agent drew for them (characters_mcp.rs `save_pet`, the desktop-pet skill), each
// one character in the four poses Filo's pet takes (resting, working, waving, asleep). The app lists them as its
// custom avatars (`customAvatars:list`): for an agent's picture, the pet and the App icon's Characters.
//
// Kept in the local folder (beside filo.db): `characters/<id>/` with each pose cut out on a 400px square
// (`<pose>.png`), the same wrapped in an SVG (`<pose>.svg`, the PNG's bytes unchanged), `motion.json` (where each pose
// stands on its cushion and where its eyes are, so it can blink them) and `preview.png`; the list, newest
// first, in `characters.json`. A character never changes once saved, so the app's picture cache never shows an old
// one: redrawing one saves another.
//
// `prepare` is the phone app's avatar_motion.mjs in Rust: it removes a plain background, fills see-through eye holes,
// centres each pose in its square feet down, sizes every pose to the resting one by eye spacing and finds the eyes.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The poses, in the order the pet's moods use them.
pub const POSES: [&str; 4] = ["idle", "working", "waving", "sleeping"];
/// Poses whose eyes are shut in the picture: nothing to blink.
const EYES_SHUT: &str = "sleeping";
const LIST: &str = "characters.json";
const FOLDER: &str = "characters";
const MAX_NAME: usize = 40;

const SQUARE: usize = 400;
/// The figure fills at most this much of its square, standing on a line this far down.
const FIT: f64 = 360.;
const FLOOR: usize = 384;
/// The header scene a figure stands in is 88 x 82 with the cushion top at y 72. A resting figure is this tall in it, and no pose
/// may grow past the height and width that still fit under the header's buttons.
const SCENE: f64 = 88.;
const FEET_Y: f64 = 72.;
const CENTRE_X: f64 = 44.;
const REST_HEIGHT: f64 = 60.;
const MAX_HEIGHT: f64 = 70.;
const MAX_WIDTH: f64 = 84.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    pub id: String,
    pub name: String,
    pub created_at: f64,
    /// Saved to be the desktop pet: the app makes it the pet once it sees it, then clears this (`picked`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pick_as_pet: bool,
}

fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

/// The characters in `dir` (the local folder), newest first; one whose folder is gone is left out.
pub fn list(dir: &Path) -> Vec<Character> {
    let list: Vec<Character> =
        std::fs::read(dir.join(LIST)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    list.into_iter()
        .filter(|character| valid_id(&character.id) && pose_path(dir, &character.id, POSES[0]).is_file())
        .collect()
}

pub fn get(dir: &Path, id: &str) -> Option<Character> {
    list(dir).into_iter().find(|character| character.id == id)
}

/// A pose's picture (`idle`…), 400px square.
pub fn pose_path(dir: &Path, id: &str, pose: &str) -> PathBuf {
    dir.join(FOLDER).join(id).join(format!("{pose}.png"))
}

/// One as `customAvatars:list` gives it: `{ id, name, poses: { idle: { url }, … } }`, each url what `url` makes of
/// its file's path; `pickAsPet` while it's waiting to be made the desktop pet.
pub fn row(dir: &Path, character: &Character, url: impl Fn(&Path) -> String) -> Value {
    let poses: serde_json::Map<String, Value> = POSES
        .iter()
        .map(|pose| (pose.to_string(), json!({ "url": url(&pose_path(dir, &character.id, pose)) })))
        .collect();
    let mut row = json!({ "id": character.id, "name": character.name, "poses": poses });
    if character.pick_as_pet {
        row["pickAsPet"] = json!(true);
    }
    row
}

/// Takes one off the list and deletes its files. Whether it was there.
pub fn remove(dir: &Path, id: &str) -> Result<bool, String> {
    if !valid_id(id) {
        return Ok(false);
    }
    let mut list: Vec<Character> =
        std::fs::read(dir.join(LIST)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    let before = list.len();
    list.retain(|character| character.id != id);
    write_list(dir, &list)?;
    let _ = std::fs::remove_dir_all(dir.join(FOLDER).join(id));
    Ok(list.len() != before)
}

fn write_list(dir: &Path, list: &[Character]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(list).map_err(|error| error.to_string())?;
    write_file(&dir.join(LIST), &bytes)
}

/// Through a temporary file and a rename, so a reader never sees half of one.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::files::write_atomic(path, bytes).map_err(|error| format!("Couldn't save {}: {error}", path.display()))
}

/// A name as kept: one line, at most `MAX_NAME` characters.
pub fn clean_name(name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    name.chars().take(MAX_NAME).collect::<String>().trim().to_string()
}

/// Saves `prepared` as a new character called `name`, first on the list (to be made the desktop pet, with
/// `pick_as_pet`); its id.
pub fn save(dir: &Path, name: &str, prepared: &Prepared, pick_as_pet: bool) -> Result<String, String> {
    let name = clean_name(name);
    if name.is_empty() {
        return Err("The pet needs a name.".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let folder = dir.join(FOLDER).join(&id);
    std::fs::create_dir_all(&folder).map_err(|error| format!("Couldn't make {}: {error}", folder.display()))?;
    let written = (|| {
        for (pose, picture) in POSES.iter().zip(&prepared.poses) {
            let png = encode_png(picture)?;
            write_file(&folder.join(format!("{pose}.png")), &png)?;
            write_file(
                &folder.join(format!("{pose}.svg")),
                svg_wrapper(&png, &format!("{name}, {pose}"), &format!("Desktop pet, {pose} pose")).as_bytes(),
            )?;
        }
        write_file(
            &folder.join("motion.json"),
            format!("{}\n", serde_json::to_string_pretty(&prepared.motion).unwrap_or_default()).as_bytes(),
        )?;
        write_file(&folder.join("preview.png"), &encode_png(&prepared.preview)?)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(error);
    }
    let mut list: Vec<Character> =
        std::fs::read(dir.join(LIST)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    list.insert(0, Character { id: id.clone(), name, created_at: crate::now_ms() as f64, pick_as_pet });
    write_list(dir, &list)?;
    Ok(id)
}

/// The app made `id` the desktop pet: it's no longer waiting to be.
pub fn picked(dir: &Path, id: &str) -> Result<(), String> {
    let mut list: Vec<Character> =
        std::fs::read(dir.join(LIST)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    let Some(character) = list.iter_mut().find(|character| character.id == id && character.pick_as_pet) else {
        return Ok(());
    };
    character.pick_as_pet = false;
    write_list(dir, &list)
}

fn encode_png(picture: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("Couldn't encode a picture: {error}"))?;
    Ok(png)
}

/// The phone app's png_to_svg: an SVG whose `<image>` is the PNG's own bytes as a data URI, at the PNG's size.
fn svg_wrapper(png: &[u8], title: &str, description: &str) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    };
    let (width, height) = if png.len() >= 24 {
        (
            u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
            u32::from_be_bytes([png[20], png[21], png[22], png[23]]),
        )
    } else {
        (0, 0)
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-labelledby=\"title desc\">\n  <title id=\"title\">{}</title>\n  <desc id=\"desc\">{}</desc>\n  <image width=\"{width}\" height=\"{height}\" href=\"data:image/png;base64,{encoded}\"/>\n</svg>\n",
        escape(title),
        escape(description)
    )
}

// ---------- Cutting out and the motion ----------

/// The four poses cut out, with what the header scene moves them with and a contact sheet to look at.
pub struct Prepared {
    /// In `POSES` order, each 400 x 400.
    pub poses: Vec<RgbaImage>,
    /// `{ <pose>: { cushion: [size, left, top], eyes: { color, eyes: [[x, y, w, h], …] } | null } }`.
    pub motion: Value,
    /// The poses at half size side by side, eye boxes in red.
    pub preview: RgbaImage,
    /// One line per pose.
    pub lines: Vec<String>,
    /// A pose whose eyes weren't found (it won't blink).
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

struct Region {
    pixels: Vec<usize>,
    min_x: usize,
    max_x: usize,
    min_y: usize,
    max_y: usize,
}

impl Region {
    fn rect(&self) -> Rect {
        Rect { x: self.min_x, y: self.min_y, w: self.max_x - self.min_x + 1, h: self.max_y - self.min_y + 1 }
    }
}

struct Eyes {
    color: String,
    eyes: [Rect; 2],
}

fn luma(rgba: &[u8], p: usize) -> f64 {
    0.299 * f64::from(rgba[p * 4]) + 0.587 * f64::from(rgba[p * 4 + 1]) + 0.114 * f64::from(rgba[p * 4 + 2])
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() >> 1]
}

/// A picture with a plain background (most uploads): flood the border colour away so only the figure is left.
fn remove_plain_background(picture: &mut RgbaImage) -> bool {
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let rgba: &mut [u8] = picture;
    let transparent = rgba.chunks_exact(4).filter(|pixel| pixel[3] < 250).count();
    if transparent as f64 > (width * height) as f64 * 0.02 {
        return false;
    }
    let mut border = Vec::new();
    for x in 0..width {
        border.extend([x, (height - 1) * width + x]);
    }
    for y in 0..height {
        border.extend([y * width, y * width + width - 1]);
    }
    let channel = |rgba: &[u8], c: usize| {
        let mut values: Vec<u8> = border.iter().map(|p| rgba[p * 4 + c]).collect();
        values.sort_unstable();
        f64::from(values[values.len() >> 1])
    };
    let background = [channel(rgba, 0), channel(rgba, 1), channel(rgba, 2)];
    let distance = |rgba: &[u8], p: usize| {
        let d = |c: usize| f64::from(rgba[p * 4 + c]) - background[c];
        (d(0) * d(0) + d(1) * d(1) + d(2) * d(2)).sqrt()
    };
    let neighbours = |p: usize| {
        let x = p % width;
        [
            (x > 0).then(|| p - 1),
            (x < width - 1).then(|| p + 1),
            p.checked_sub(width),
            Some(p + width).filter(|q| *q < width * height),
        ]
    };
    let mut seen = vec![false; width * height];
    let mut stack: Vec<usize> = border.iter().copied().filter(|p| distance(rgba, *p) < 48.).collect();
    for p in &stack {
        seen[*p] = true;
    }
    while let Some(p) = stack.pop() {
        rgba[p * 4 + 3] = 0;
        for q in neighbours(p).into_iter().flatten() {
            if !seen[q] {
                seen[q] = true;
                if distance(rgba, q) < 48. {
                    stack.push(q);
                }
            }
        }
    }
    // Soften the cut: an edge pixel close to the background colour goes half see-through.
    for p in 0..width * height {
        if rgba[p * 4 + 3] == 0 {
            continue;
        }
        let next_to_cut = neighbours(p).into_iter().flatten().any(|q| rgba[q * 4 + 3] == 0);
        let d = distance(rgba, p);
        if next_to_cut && d < 96. {
            rgba[p * 4 + 3] = (255. * (d / 96.).min(1.)).round() as u8;
        }
    }
    true
}

fn bounds(picture: &RgbaImage) -> Option<Rect> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (usize::MAX, usize::MAX, 0, 0);
    let mut any = false;
    for (x, y, pixel) in picture.enumerate_pixels() {
        if pixel[3] < 24 {
            continue;
        }
        let (x, y) = (x as usize, y as usize);
        any = true;
        (min_x, min_y, max_x, max_y) = (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y));
    }
    any.then(|| Rect { x: min_x, y: min_y, w: max_x - min_x + 1, h: max_y - min_y + 1 })
}

/// The figure scaled into the middle of a 400px square, feet on the floor line; area-averaged with premultiplied alpha.
/// The square, the scale and where the figure is in it.
fn into_square(picture: &RgbaImage, area: Rect) -> (RgbaImage, f64, Rect) {
    let scale = (FIT / area.w as f64).min(FIT / area.h as f64);
    let out_w = ((area.w as f64 * scale).round() as usize).max(1);
    let out_h = ((area.h as f64 * scale).round() as usize).max(1);
    let left = ((SQUARE - out_w) as f64 / 2.).round() as usize;
    let top = FLOOR - out_h;
    let (width, height) = (picture.width() as usize, picture.height() as usize);
    let source: &[u8] = picture;
    let mut out = vec![0u8; SQUARE * SQUARE * 4];
    for oy in 0..out_h {
        let sy0 = area.y as f64 + oy as f64 / scale;
        let sy1 = area.y as f64 + (oy + 1) as f64 / scale;
        for ox in 0..out_w {
            let sx0 = area.x as f64 + ox as f64 / scale;
            let sx1 = area.x as f64 + (ox + 1) as f64 / scale;
            let (mut r, mut g, mut b, mut a, mut weight) = (0., 0., 0., 0., 0.);
            let mut sy = sy0.floor();
            while sy < sy1.ceil() {
                let wy = (sy + 1.).min(sy1) - sy.max(sy0);
                let mut sx = sx0.floor();
                while sx < sx1.ceil() {
                    let wx = (sx + 1.).min(sx1) - sx.max(sx0);
                    let w = wx * wy;
                    if w > 0. && sx >= 0. && sy >= 0. && (sx as usize) < width && (sy as usize) < height {
                        let i = (sy as usize * width + sx as usize) * 4;
                        let alpha = f64::from(source[i + 3]) / 255.;
                        r += f64::from(source[i]) * alpha * w;
                        g += f64::from(source[i + 1]) * alpha * w;
                        b += f64::from(source[i + 2]) * alpha * w;
                        a += alpha * w;
                        weight += w;
                    }
                    sx += 1.;
                }
                sy += 1.;
            }
            let o = ((top + oy) * SQUARE + left + ox) * 4;
            if a > 0. {
                let byte = |value: f64| value.round().clamp(0., 255.) as u8;
                out[o] = byte(r / a);
                out[o + 1] = byte(g / a);
                out[o + 2] = byte(b / a);
                out[o + 3] = byte(255. * a / weight);
            }
        }
    }
    let square = RgbaImage::from_raw(SQUARE as u32, SQUARE as u32, out).expect("the square's size");
    (square, scale, Rect { x: left, y: top, w: out_w, h: out_h })
}

/// Connected regions of pixels where `inside(p)` holds, inside `area`.
fn regions(width: usize, height: usize, area: Rect, inside: impl Fn(usize) -> bool) -> Vec<Region> {
    let mut label = vec![false; width * height];
    let mut found = Vec::new();
    for y in area.y..area.y + area.h {
        for x in area.x..area.x + area.w {
            let start = y * width + x;
            if label[start] || !inside(start) {
                continue;
            }
            let mut region = Region { pixels: Vec::new(), min_x: x, max_x: x, min_y: y, max_y: y };
            label[start] = true;
            let mut stack = vec![start];
            while let Some(p) = stack.pop() {
                region.pixels.push(p);
                let (px, py) = (p % width, p / width);
                region.min_x = region.min_x.min(px);
                region.max_x = region.max_x.max(px);
                region.min_y = region.min_y.min(py);
                region.max_y = region.max_y.max(py);
                let around = [
                    (px.checked_sub(1), Some(py)),
                    (Some(px + 1), Some(py)),
                    (Some(px), py.checked_sub(1)),
                    (Some(px), Some(py + 1)),
                ];
                for (qx, qy) in around {
                    let (Some(qx), Some(qy)) = (qx, qy) else { continue };
                    if qx < area.x || qy < area.y || qx >= area.x + area.w || qy >= area.y + area.h {
                        continue;
                    }
                    let q = qy * width + qx;
                    if !label[q] && inside(q) {
                        label[q] = true;
                        stack.push(q);
                    }
                }
            }
            found.push(region);
        }
    }
    found
}

/// Pixels in a band around `area` where `inside(p)` holds, for the colour an eyelid should be.
fn ring(width: usize, height: usize, area: Rect, inside: impl Fn(usize) -> bool) -> Vec<usize> {
    let pad = ((area.w.min(area.h) as f64 * 0.35).round() as i64).max(2);
    let mut pixels = Vec::new();
    for y in area.y as i64 - pad..(area.y + area.h) as i64 + pad {
        for x in area.x as i64 - pad..(area.x + area.w) as i64 + pad {
            if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
                continue;
            }
            let (ux, uy) = (x as usize, y as usize);
            if ux >= area.x && ux < area.x + area.w && uy >= area.y && uy < area.y + area.h {
                continue;
            }
            let p = uy * width + ux;
            if inside(p) {
                pixels.push(p);
            }
        }
    }
    pixels
}

fn touches_edge(rect: Rect, of: Rect) -> bool {
    rect.x == of.x || rect.y == of.y || rect.x + rect.w == of.x + of.w || rect.y + rect.h == of.y + of.h
}

/// Image models asked for a transparent background often cut light eyes out of a dark face as holes, which would show
/// what's behind the pet through them. Fills small see-through holes ringed by dark pixels with white, blending their
/// soft edges; gaps between an arm and the body are ringed by the light body and stay open.
fn fill_eye_holes(square: &mut RgbaImage, figure: Rect) -> usize {
    let (width, height) = (square.width() as usize, square.height() as usize);
    let area = (figure.w * figure.h) as f64;
    let holes = {
        let rgba: &[u8] = square;
        regions(width, height, figure, |p| rgba[p * 4 + 3] < 128)
    };
    let mut filled = 0;
    for hole in holes {
        let rect = hole.rect();
        if touches_edge(rect, figure) || hole.pixels.len() as f64 > area * 0.03 {
            continue;
        }
        let rgba: &mut [u8] = square;
        let around = ring(width, height, rect, |p| rgba[p * 4 + 3] > 200);
        if around.is_empty() || median(around.iter().map(|p| luma(rgba, *p)).collect()) > 80. {
            continue;
        }
        // The hole plus a pixel around it, so the anti-aliased rim is laid over white too.
        for y in rect.y.saturating_sub(1)..=(rect.y + rect.h).min(height - 1) {
            for x in rect.x.saturating_sub(1)..=(rect.x + rect.w).min(width - 1) {
                let p = y * width + x;
                let alpha = f64::from(rgba[p * 4 + 3]) / 255.;
                if alpha >= 1. {
                    continue;
                }
                for c in 0..3 {
                    rgba[p * 4 + c] =
                        (f64::from(rgba[p * 4 + c]) * alpha + 255. * (1. - alpha)).round().clamp(0., 255.) as u8;
                }
                rgba[p * 4 + 3] = 255;
            }
        }
        filled += 1;
    }
    filled
}

/// The two regions most like a pair of eyes: alike in size and height, side by side, a few eye-widths apart.
fn best_pair(candidates: &[Region]) -> Option<[Rect; 2]> {
    let mut best: Option<(f64, [Rect; 2])> = None;
    for i in 0..candidates.len() {
        for j in i + 1..candidates.len() {
            let (mut a, mut b) = (candidates[i].rect(), candidates[j].rect());
            if b.x < a.x {
                std::mem::swap(&mut a, &mut b);
            }
            let sizes = candidates[i].pixels.len() as f64 / candidates[j].pixels.len() as f64;
            let ratio = sizes.max(1. / sizes);
            let widths = (a.w as f64 / b.w as f64).max(b.w as f64 / a.w as f64);
            let gap = b.x as f64 - (a.x + a.w) as f64;
            let width = (a.w + b.w) as f64 / 2.;
            let rise = ((a.y as f64 + a.h as f64 / 2.) - (b.y as f64 + b.h as f64 / 2.)).abs();
            if ratio > 2.2 || widths > 1.6 || gap < width * 0.6 || gap > width * 7. || rise > a.h.max(b.h) as f64 * 0.8
            {
                continue;
            }
            let score =
                (candidates[i].pixels.len() + candidates[j].pixels.len()) as f64 - rise * 4. - (ratio - 1.) * 200.;
            if best.is_none_or(|(top, _)| score > top) {
                best = Some((score, [a, b]));
            }
        }
    }
    best.map(|(_, eyes)| eyes)
}

fn median_colour(rgba: &[u8], pixels: &[usize]) -> String {
    let pick = |c: usize| {
        let mut values: Vec<u8> = pixels.iter().map(|p| rgba[p * 4 + c]).collect();
        values.sort_unstable();
        values[values.len() >> 1]
    };
    format!("#{:02x}{:02x}{:02x}", pick(0), pick(1), pick(2))
}

/// Finds the eyes in a 400px pose. First a dark face (or visor) with lighter eyes in it, the mascot look; then dark
/// eyes on a light face. None when nothing looks like a pair, and that pose just doesn't blink.
fn find_eyes(square: &RgbaImage, figure: Rect) -> Option<Eyes> {
    let (width, height) = (square.width() as usize, square.height() as usize);
    let rgba: &[u8] = square;
    let solid = |p: usize| rgba[p * 4 + 3] > 200;
    let light = |p: usize| luma(rgba, p);
    let area = (figure.w * figure.h) as f64;
    let head = Rect { x: figure.x, y: figure.y, w: figure.w, h: (figure.h as f64 * 0.7).round() as usize };

    let mut dark: Vec<Region> = regions(width, height, head, |p| solid(p) && light(p) < 60.)
        .into_iter()
        .filter(|region| region.pixels.len() as f64 > area * 0.02)
        .collect();
    dark.sort_by_key(|region| std::cmp::Reverse(region.pixels.len()));
    for face in dark.iter().take(2) {
        let face_rect = face.rect();
        let size = face.pixels.len() as f64;
        // The median, not the mean: a glossy visor's highlight would lift the mean past dim eyes.
        let face_light = median(face.pixels.iter().map(|p| light(*p)).collect());
        // Brightest first, so crisp white eyes come out without the visor's softer highlights joined on; dimmer eyes are
        // found as the bar drops.
        for lift in [150., 100., 60., 18.] {
            let candidates: Vec<Region> =
                regions(width, height, face_rect, |p| solid(p) && light(p) > face_light + lift)
                    .into_iter()
                    .filter(|region| {
                        let rect = region.rect();
                        let n = region.pixels.len() as f64;
                        !touches_edge(rect, face_rect)
                            && n > size * 0.004
                            && n < size * 0.15
                            && rect.h as f64 >= rect.w as f64 * 0.7
                            && n > (rect.w * rect.h) as f64 * 0.5
                    })
                    .collect();
            if let Some(eyes) = best_pair(&candidates) {
                let around: Vec<usize> = eyes
                    .iter()
                    .flat_map(|rect| ring(width, height, *rect, |p| solid(p) && light(p) < face_light + 20.))
                    .collect();
                let color = median_colour(rgba, if around.is_empty() { &face.pixels } else { &around });
                return Some(Eyes { color, eyes });
            }
        }
    }

    let candidates: Vec<Region> = regions(width, height, head, |p| solid(p) && light(p) < 70.)
        .into_iter()
        .filter(|region| {
            let rect = region.rect();
            let n = region.pixels.len() as f64;
            let aspect = rect.h as f64 / rect.w as f64;
            n > area * 0.0015 && n < area * 0.03 && aspect > 0.5 && aspect < 3.5 && n > (rect.w * rect.h) as f64 * 0.45
        })
        .collect();
    let eyes = best_pair(&candidates)?;
    let around: Vec<usize> =
        eyes.iter().flat_map(|rect| ring(width, height, *rect, |p| solid(p) && light(p) > 110.)).collect();
    (!around.is_empty()).then(|| Eyes { color: median_colour(rgba, &around), eyes })
}

fn round3(value: f64) -> f64 {
    (value * 1000.).round() / 1000.
}

/// Where a pose stands on the header cushion as [size, left, top] in scene widths. `unit` is scene units per source
/// pixel, the same for every pose of the character so they keep one size, capped so tall or wide poses still fit.
fn cushion_place(figure: Rect, square_scale: f64, unit: f64) -> [f64; 3] {
    let per_pixel = (unit / square_scale).min(MAX_HEIGHT / figure.h as f64).min(MAX_WIDTH / figure.w as f64);
    let size = SQUARE as f64 * per_pixel;
    let left = CENTRE_X - (figure.x as f64 + figure.w as f64 / 2.) * per_pixel;
    let top = FEET_Y - (figure.y + figure.h) as f64 * per_pixel;
    [round3(size / SCENE), round3(left / SCENE), round3(top / SCENE)]
}

struct Cut {
    square: RgbaImage,
    scale: f64,
    figure: Rect,
    source: Rect,
    removed: bool,
    holes: usize,
    eyes: Option<Eyes>,
}

/// The four poses (`POSES` order, any size of PNG, JPEG or WebP) cut out and placed.
pub fn prepare(sources: &[PathBuf; 4]) -> Result<Prepared, String> {
    let mut cuts = Vec::new();
    for (pose, path) in POSES.iter().zip(sources) {
        let mut picture =
            image::open(path).map_err(|error| format!("{pose}: couldn't read {}: {error}", path.display()))?.to_rgba8();
        let removed = remove_plain_background(&mut picture);
        let source =
            bounds(&picture).ok_or_else(|| format!("{pose}: the picture is empty after cutting out the background"))?;
        if source.w < 64 || source.h < 64 {
            return Err(format!("{pose}: the figure is only {}x{}px; generate it larger", source.w, source.h));
        }
        let (mut square, scale, figure) = into_square(&picture, source);
        let holes = fill_eye_holes(&mut square, figure);
        let eyes = if *pose == EYES_SHUT { None } else { find_eyes(&square, figure) };
        cuts.push(Cut { square, scale, figure, source, removed, holes, eyes });
    }

    // One size for the whole character: the resting pose sets it, and a pose whose eyes were found is matched by eye
    // spacing (a sitting or curled pose shouldn't shrink or grow), otherwise by its picture's scale.
    let eye_span = |cut: &Cut| {
        cut.eyes.as_ref().map(|eyes| {
            (eyes.eyes[1].x as f64 + eyes.eyes[1].w as f64 / 2. - (eyes.eyes[0].x as f64 + eyes.eyes[0].w as f64 / 2.))
                / cut.scale
        })
    };
    let rest = &cuts[0];
    let rest_unit = (REST_HEIGHT / rest.source.h as f64).min(MAX_WIDTH / rest.source.w as f64);
    let rest_span = eye_span(rest).filter(|span| *span != 0.);

    let mut motion = serde_json::Map::new();
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    for (pose, cut) in POSES.iter().zip(&cuts) {
        let unit = match (rest_span, eye_span(cut).filter(|span| *span != 0.)) {
            (Some(rest_span), Some(span)) => rest_unit * rest_span / span,
            _ => rest_unit,
        };
        let cushion = cushion_place(cut.figure, cut.scale, unit);
        let eyes = cut
            .eyes
            .as_ref()
            .map(|eyes| json!({ "color": eyes.color, "eyes": eyes.eyes.map(|rect| [rect.x, rect.y, rect.w, rect.h]) }));
        if cut.eyes.is_none() && *pose != EYES_SHUT {
            warnings.push(format!("{pose}: no eyes found, so this pose won't blink"));
        }
        lines.push(format!(
            "{pose}: {}{}figure {}x{} in the square, cushion {}, eyes {}",
            if cut.removed { "background removed, " } else { "" },
            if cut.holes > 0 { format!("{} see-through eye holes filled, ", cut.holes) } else { String::new() },
            cut.figure.w,
            cut.figure.h,
            json!(cushion),
            eyes.as_ref().map_or("none".to_string(), |eyes| eyes["eyes"].to_string()),
        ));
        motion.insert(pose.to_string(), json!({ "cushion": cushion, "eyes": eyes }));
    }
    let motion = Value::Object(motion);
    let preview = contact_sheet(&cuts, &motion);
    Ok(Prepared { poses: cuts.into_iter().map(|cut| cut.square).collect(), motion, preview, lines, warnings })
}

/// The poses at half size on a light grey, eye boxes in red.
fn contact_sheet(cuts: &[Cut], motion: &Value) -> RgbaImage {
    let half = SQUARE / 2;
    let mut sheet = RgbaImage::new((half * cuts.len()) as u32, half as u32);
    for (index, (pose, cut)) in POSES.iter().zip(cuts).enumerate() {
        for y in 0..half {
            for x in 0..half {
                let pixel = cut.square.get_pixel((x * 2) as u32, (y * 2) as u32);
                let alpha = f64::from(pixel[3]) / 255.;
                let blend = |c: usize| (f64::from(pixel[c]) * alpha + 225. * (1. - alpha)).round() as u8;
                sheet.put_pixel((index * half + x) as u32, y as u32, image::Rgba([blend(0), blend(1), blend(2), 255]));
            }
        }
        for rect in motion[pose]["eyes"]["eyes"].as_array().into_iter().flatten() {
            let value = |i: usize| rect[i].as_f64().unwrap_or(0.);
            let (x, y, w, h) = (value(0), value(1), value(2), value(3));
            let steps = w.max(h).max(1.) as usize;
            for step in 0..=steps {
                let t = step as f64 / steps as f64;
                for (px, py) in [(x + w * t, y), (x + w * t, y + h), (x, y + h * t), (x + w, y + h * t)] {
                    let sx = ((px / 2.).round() as usize).min(half - 1);
                    let sy = ((py / 2.).round() as usize).min(half - 1);
                    sheet.put_pixel((index * half + sx) as u32, sy as u32, image::Rgba([230, 30, 30, 255]));
                }
            }
        }
    }
    sheet
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mascot on a plain white ground: a light body, a dark visor with two white eyes cut in it.
    fn mascot(path: &Path, sitting: bool) {
        let (width, height) = (300u32, 400u32);
        let body_top = if sitting { 160 } else { 80 };
        let picture = RgbaImage::from_fn(width, height, |x, y| {
            let (x, y) = (x as i64, y as i64);
            let in_body = (60..240).contains(&x) && (body_top..360).contains(&y);
            let in_visor = (90..210).contains(&x) && (body_top + 30..body_top + 110).contains(&y);
            let in_eye =
                (y >= body_top + 50 && y < body_top + 90) && ((120..135).contains(&x) || (165..180).contains(&x));
            match (in_body, in_visor, in_eye) {
                (_, true, true) => image::Rgba([250, 250, 250, 255]),
                (_, true, false) => image::Rgba([12, 12, 16, 255]),
                (true, _, _) => image::Rgba([200, 210, 240, 255]),
                _ => image::Rgba([255, 255, 255, 255]),
            }
        });
        picture.save(path).unwrap();
    }

    #[test]
    fn four_poses_are_cut_out_placed_and_saved() {
        let dir = crate::test_support::temp_dir("characters");
        let sources = POSES.map(|pose| dir.join(format!("{pose}-source.png")));
        for (pose, path) in POSES.iter().zip(&sources) {
            mascot(path, *pose != "idle");
        }
        let prepared = prepare(&sources).unwrap();
        assert_eq!(prepared.poses.len(), 4);
        assert!(prepared.poses.iter().all(|pose| pose.dimensions() == (400, 400)));
        // The white ground is gone; the figure stands on the floor line, centred.
        let idle = &prepared.poses[0];
        assert_eq!(idle.get_pixel(5, 5)[3], 0);
        let figure = bounds(idle).unwrap();
        assert_eq!(figure.y + figure.h, FLOOR);
        assert!((figure.x as i64 + figure.w as i64 / 2 - 200).abs() <= 1);
        // Eyes found in the waking poses, at the same spacing, none for sleeping.
        for pose in ["idle", "working", "waving"] {
            let eyes = prepared.motion[pose]["eyes"]["eyes"].as_array().unwrap_or_else(|| panic!("{pose} eyes"));
            assert_eq!(eyes.len(), 2);
        }
        assert!(prepared.motion["sleeping"]["eyes"].is_null());
        assert!(prepared.warnings.is_empty(), "{:?}", prepared.warnings);
        assert_eq!(prepared.motion["idle"]["eyes"]["color"], "#0c0c10");
        assert_eq!(prepared.preview.dimensions(), (800, 200));

        let id = save(&dir, "  Astronaut\ncat  ", &prepared, true).unwrap();
        let saved = list(&dir);
        assert_eq!(
            saved.iter().map(|character| (character.id.as_str(), character.name.as_str())).collect::<Vec<_>>(),
            [(id.as_str(), "Astronaut cat")]
        );
        for pose in POSES {
            assert!(pose_path(&dir, &id, pose).is_file());
            let svg = std::fs::read_to_string(dir.join(FOLDER).join(&id).join(format!("{pose}.svg"))).unwrap();
            assert!(svg.contains("width=\"400\" height=\"400\"") && svg.contains("data:image/png;base64,"));
        }
        let row = row(&dir, &saved[0], |path| path.display().to_string());
        assert_eq!(row["poses"]["sleeping"]["url"], json!(pose_path(&dir, &id, "sleeping")));
        assert_eq!(row["pickAsPet"], json!(true));
        picked(&dir, &id).unwrap();
        assert!(!list(&dir)[0].pick_as_pet);
        assert!(remove(&dir, &id).unwrap());
        assert!(list(&dir).is_empty() && !dir.join(FOLDER).join(&id).exists());
        assert!(!remove(&dir, "../agents").unwrap());
    }

    #[test]
    fn an_empty_picture_is_refused() {
        let dir = crate::test_support::temp_dir("characters");
        let blank = dir.join("blank.png");
        RgbaImage::from_pixel(200, 200, image::Rgba([255, 255, 255, 255])).save(&blank).unwrap();
        let error = prepare(&[blank.clone(), blank.clone(), blank.clone(), blank]).err().unwrap();
        assert!(error.starts_with("idle: the picture is empty"), "{error}");
    }
}

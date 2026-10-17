// Filo's animals in the user's colour (AVATAR_COLORS in src/features/filo/avatars.ts). Blue is the pictures as
// drawn; in the others the white body takes a light tint of the colour (multiplied, so its shading survives),
// the blue accents (ears, paws, cheeks, tail) take the colour itself, and the dark face stays. Both apps bundle
// the results (assets/images/filo/avatar-colors), made from this code by scripts/avatar-colors.sh.

use gpui::{ImageSource, Resource};

/// (id, swatch, the colour the pictures take; None for blue, the pictures as drawn).
pub const COLORS: [(&str, &str, Option<[u8; 3]>); 8] = [
    ("blue", "#3F7DF0", None),
    ("purple", "#8B6CF2", Some([0x8B, 0x6C, 0xF2])),
    ("pink", "#F0679F", Some([0xF0, 0x67, 0x9F])),
    ("red", "#EE5A52", Some([0xEE, 0x5A, 0x52])),
    ("orange", "#F5913A", Some([0xF5, 0x91, 0x3A])),
    ("yellow", "#EDB928", Some([0xED, 0xB9, 0x28])),
    ("green", "#3FBF7F", Some([0x3F, 0xBF, 0x7F])),
    ("teal", "#26B3C4", Some([0x26, 0xB3, 0xC4])),
];

pub const DEFAULT_COLOR: &str = "blue";

/// The pictures' own accent blue (hue 220°).
#[cfg(feature = "dev")]
const ACCENT_HUE: f32 = 220. / 360.;

fn target(color: &str) -> Option<[u8; 3]> {
    COLORS.iter().find(|(id, _, _)| *id == color).and_then(|(_, _, rgb)| *rgb)
}

#[cfg(feature = "dev")]
fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.;
    let d = max - min;
    if d <= 1e-6 {
        return (0., 0., l);
    }
    let s = d / (1. - (2. * l - 1.).abs() + 1e-6);
    let h = if r == max {
        (g - b) / d
    } else if g == max {
        2. + (b - r) / d
    } else {
        4. + (r - g) / d
    };
    ((h / 6.).rem_euclid(1.), s, l)
}

#[cfg(feature = "dev")]
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let hp = h * 6.;
    let x = c * (1. - (hp % 2. - 1.).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let m = l - c / 2.;
    (r + m, g + m, b + m)
}

/// Recolours an avatar picture in place.
#[cfg(feature = "dev")]
pub fn recolor(image: &mut image::RgbaImage, color: [u8; 3]) {
    let [tr, tg, tb] = color.map(|channel| channel as f32 / 255.);
    let (target_hue, _, target_saturation) = {
        let (h, s, l) = rgb_to_hsl(tr, tg, tb);
        (h, l, s)
    };
    let tint = [1. - (1. - tr) * 0.42, 1. - (1. - tg) * 0.42, 1. - (1. - tb) * 0.42];
    for pixel in image.pixels_mut() {
        if pixel[3] == 0 {
            continue;
        }
        let [r, g, b] = [pixel[0], pixel[1], pixel[2]].map(|channel| channel as f32 / 255.);
        let (h, s, l) = rgb_to_hsl(r, g, b);
        // Accent: saturated pixels near the pictures' blue take the colour's hue and saturation.
        let hue_distance = ((h - ACCENT_HUE + 0.5).rem_euclid(1.) - 0.5).abs();
        let hue_near = 1. - (hue_distance / 0.08).clamp(0., 1.);
        let accent = ((s - 0.18) / 0.25).clamp(0., 1.) * hue_near;
        let accent_rgb = hsl_to_rgb(target_hue, (s * (target_saturation / 0.75)).clamp(0., 1.), l);
        // Body: light pixels are multiplied by a light tint; the dark face and outlines stay.
        let body = ((l - 0.18) / 0.25).clamp(0., 1.);
        let body_rgb = [r, g, b]
            .iter()
            .zip(tint)
            .map(|(channel, tint)| channel * (1. - body) + channel * tint * body)
            .collect::<Vec<_>>();
        let out = [
            body_rgb[0] * (1. - accent) + accent_rgb.0 * accent,
            body_rgb[1] * (1. - accent) + accent_rgb.1 * accent,
            body_rgb[2] * (1. - accent) + accent_rgb.2 * accent,
        ];
        for (index, channel) in out.into_iter().enumerate() {
            pixel[index] = (channel.clamp(0., 1.) * 255.).round() as u8;
        }
    }
}

/// An animal's picture (`fox-idle`) recoloured to `color`, as PNG bytes (`filo --avatar-colors`); None for blue
/// or an unknown picture.
#[cfg(feature = "dev")]
pub fn colored_png(id: &str, color: &str) -> Option<Vec<u8>> {
    let target = target(color)?;
    let bytes = crate::assets::avatar_bytes(id)?;
    let mut image = image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP).ok()?.to_rgba8();
    recolor(&mut image, target);
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// An animal's picture in `color`: blue is the embedded picture as drawn, the others the embedded recoloured ones.
pub fn image(id: &str, color: &str) -> ImageSource {
    if target(color).is_none() || crate::assets::avatar_color_bytes(color, id).is_none() {
        return crate::avatars::animal_image(id);
    }
    ImageSource::Resource(Resource::Embedded(format!("avatar-colors/{color}/{id}.webp").into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_colour_but_blue_is_bundled_for_every_pose() {
        for (color, _, rgb) in COLORS {
            for animal in crate::avatars::ANIMALS.iter() {
                for (pose, _) in animal.poses {
                    let id = animal.avatar_id(pose);
                    assert_eq!(crate::assets::avatar_color_bytes(color, &id).is_some(), rgb.is_some(), "{color} {id}");
                }
            }
        }
    }

    #[test]
    fn recolouring_keeps_the_dark_face_and_tints_the_white_body() {
        let mut picture = image::RgbaImage::from_pixel(2, 1, image::Rgba([250, 250, 250, 255]));
        picture.put_pixel(1, 0, image::Rgba([10, 10, 12, 255]));
        recolor(&mut picture, [0x3F, 0xBF, 0x7F]);
        let body = picture.get_pixel(0, 0);
        assert!(body[1] > body[0] && body[1] > body[2], "the body leans green: {body:?}");
        assert_eq!(picture.get_pixel(1, 0), &image::Rgba([10, 10, 12, 255]));
    }
}

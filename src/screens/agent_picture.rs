// A picture from the user's Mac for an agent (the picture sheet's "Upload from your Mac", screens/agents.rs): the
// macOS file picker for PNG, JPEG and HEIC, the picture read and turned upright, then cropped in the sheet's preview
// circle (drag to move, a slider to zoom) and kept as a round 512px PNG (local_backend/pictures.rs). App icon's
// "Upload a picture" crops the same way to a rounded square (screens/app_icon.rs).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use gpui::{Context, IntoElement, Render, RenderImage, Window};
use image::{DynamicImage, ImageDecoder as _, RgbaImage};
use serde::Deserialize;

/// The preview circle's size in the sheet, and so the crop's.
pub const PREVIEW: f32 = 96.;
/// How far the slider zooms in, from the picture just covering the circle.
pub const MAX_ZOOM: f32 = 4.;
/// The kept picture's side.
const SIDE: u32 = 512;
/// A picture is read at most this many pixels across: enough for the kept one at full zoom.
const SOURCE_MAX: u32 = 1600;

/// One of the user's uploaded pictures (`agentPictures:list`); its file is `local_backend::uploaded_picture_path`.
#[derive(Clone, Debug, Deserialize)]
pub struct Uploaded {
    pub id: String,
}

/// The crop's drag (gpui's drags carry a value and draw a view; this one draws nothing).
#[derive(Clone)]
pub struct CropDrag;

impl Render for CropDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// An uploaded picture being cropped: where it sits under the circle and how far it's zoomed.
#[derive(Clone)]
pub struct Crop {
    picture: Arc<RgbaImage>,
    /// The picture as the sheet draws it.
    pub shown: Arc<RenderImage>,
    /// The side of the square it's cropped to on screen, in points: the sheet's circle (`PREVIEW`), or App icon's.
    frame: f32,
    /// The picture's top-left corner from the circle's, in points (0 or less: it always covers the circle).
    offset: (f32, f32),
    zoom: f32,
    /// Where a drag started, and the offset then.
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Crop {
    /// Reads `path` (slow: off the main thread), centred and just covering the circle.
    pub fn load(path: &Path) -> Result<Crop> {
        Crop::load_in(path, PREVIEW)
    }

    /// The same for a `frame` points square.
    pub fn load_in(path: &Path, frame: f32) -> Result<Crop> {
        let bytes = std::fs::read(path)?;
        let picture = read_upright(&bytes)?;
        let picture = if picture.width().max(picture.height()) > SOURCE_MAX {
            picture.resize(SOURCE_MAX, SOURCE_MAX, image::imageops::FilterType::Triangle)
        } else {
            picture
        };
        Ok(Crop::new(picture.to_rgba8(), frame))
    }

    fn new(picture: RgbaImage, frame: f32) -> Crop {
        let mut bgra = picture.clone();
        bgra.pixels_mut().for_each(|pixel| pixel.0.swap(0, 2));
        let shown = Arc::new(RenderImage::new(vec![image::Frame::new(bgra)]));
        let mut crop = Crop { picture: Arc::new(picture), shown, frame, offset: (0., 0.), zoom: 1., drag: None };
        let (width, height) = crop.shown_size();
        crop.offset = ((frame - width) / 2., (frame - height) / 2.);
        crop
    }

    /// Points per picture pixel.
    fn scale(&self) -> f32 {
        self.frame / self.picture.width().min(self.picture.height()).max(1) as f32 * self.zoom
    }

    /// The picture's drawn size, in points.
    pub fn shown_size(&self) -> (f32, f32) {
        let scale = self.scale();
        (self.picture.width() as f32 * scale, self.picture.height() as f32 * scale)
    }

    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }

    /// Keeps the circle covered.
    fn clamp(&mut self) {
        let (width, height) = self.shown_size();
        self.offset = (self.offset.0.clamp(self.frame - width, 0.), self.offset.1.clamp(self.frame - height, 0.));
    }

    /// Zooms around the circle's centre.
    pub fn set_zoom(&mut self, zoom: f32) {
        let before = self.scale();
        let half = self.frame / 2.;
        let centre = ((half - self.offset.0) / before, (half - self.offset.1) / before);
        self.zoom = zoom.clamp(1., MAX_ZOOM);
        let after = self.scale();
        self.offset = (half - centre.0 * after, half - centre.1 * after);
        self.clamp();
    }

    pub fn start_drag(&mut self, at: (f32, f32)) {
        self.drag = Some((at, self.offset));
    }

    /// The picture follows the pointer from where the drag started.
    pub fn drag_to(&mut self, at: (f32, f32)) {
        let Some((start, offset)) = self.drag else { return };
        self.offset = (offset.0 + at.0 - start.0, offset.1 + at.1 - start.1);
        self.clamp();
    }

    /// What's in the frame, `side` pixels square.
    pub fn square(&self, side: u32) -> RgbaImage {
        let scale = self.scale();
        let shown = (self.frame / scale).round().max(1.) as u32;
        let x = ((-self.offset.0 / scale).round().max(0.) as u32).min(self.picture.width().saturating_sub(shown));
        let y = ((-self.offset.1 / scale).round().max(0.) as u32).min(self.picture.height().saturating_sub(shown));
        let square = image::imageops::crop_imm(
            self.picture.as_ref(),
            x,
            y,
            shown.min(self.picture.width()),
            shown.min(self.picture.height()),
        )
        .to_image();
        image::imageops::resize(&square, side, side, image::imageops::FilterType::Lanczos3)
    }

    /// What's in the circle, as kept: a `SIDE` square PNG, round (transparent outside the circle).
    pub fn png(&self) -> Result<Vec<u8>> {
        let mut kept = self.square(SIDE);
        round(&mut kept);
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(kept).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        Ok(png)
    }
}

/// Clears what's outside the inscribed circle, with a one-pixel soft edge.
fn round(picture: &mut RgbaImage) {
    let radius = picture.width() as f32 / 2.;
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        let distance = ((x as f32 + 0.5 - radius).powi(2) + (y as f32 + 0.5 - radius).powi(2)).sqrt();
        let cover = (radius - distance + 0.5).clamp(0., 1.);
        pixel.0[3] = (pixel.0[3] as f32 * cover).round() as u8;
    }
}

/// A picture the way it's meant to be seen: rotated as its EXIF says. HEIC (and anything else the image crate can't
/// read) goes through AppKit, which turns it upright itself.
fn read_upright(bytes: &[u8]) -> Result<DynamicImage> {
    let read = || -> image::ImageResult<DynamicImage> {
        let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?.into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut picture = DynamicImage::from_decoder(decoder)?;
        picture.apply_orientation(orientation);
        Ok(picture)
    };
    if let Ok(picture) = read() {
        return Ok(picture);
    }
    let tiff = crate::pasteboard::tiff_from_appkit(bytes)
        .ok_or_else(|| anyhow!("This file isn't a picture Filo can read."))?;
    Ok(image::load_from_memory(&tiff)?)
}

/// The macOS file picker for one PNG, JPEG or HEIC picture; None when it's cancelled.
pub fn choose_file() -> futures::channel::oneshot::Receiver<Option<PathBuf>> {
    choose_file_saying("Choose a picture for your agent: PNG, JPEG or HEIC")
}

/// The same, with `message` at the top of the picker.
pub fn choose_file_saying(message: &str) -> futures::channel::oneshot::Receiver<Option<PathBuf>> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSArray, NSString};

    let (done, chosen) = futures::channel::oneshot::channel();
    // SAFETY: AppKit's open panel, on the main thread (a click handler); it's kept by the completion block until the
    // panel answers, and the chosen URL's path is copied out there.
    unsafe {
        let panel: Retained<AnyObject> = msg_send![class!(NSOpenPanel), openPanel];
        let _: () = msg_send![&*panel, setCanChooseFiles: true];
        let _: () = msg_send![&*panel, setCanChooseDirectories: false];
        let _: () = msg_send![&*panel, setAllowsMultipleSelection: false];
        let types = NSArray::from_retained_slice(&["png", "jpg", "jpeg", "heic", "heif"].map(NSString::from_str));
        let _: () = msg_send![&*panel, setAllowedFileTypes: &*types];
        let _: () = msg_send![&*panel, setPrompt: &*NSString::from_str("Choose")];
        let _: () = msg_send![&*panel, setMessage: &*NSString::from_str(message)];
        let done = Cell::new(Some(done));
        let answered = panel.clone();
        let block = block2::RcBlock::new(move |response: isize| {
            // NSModalResponseOK
            let path = (response == 1).then(|| {
                let url: Option<Retained<AnyObject>> = msg_send![&*answered, URL];
                let path: Option<Retained<NSString>> = url.and_then(|url| msg_send![&*url, path]);
                path.map(|path| PathBuf::from(path.to_string()))
            });
            if let Some(done) = done.take() {
                let _ = done.send(path.flatten());
            }
        });
        let _: () = msg_send![&*panel, beginWithCompletionHandler: &*block];
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop(width: u32, height: u32) -> Crop {
        Crop::new(
            RgbaImage::from_fn(width, height, |x, _| {
                if x < width / 2 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) }
            }),
            PREVIEW,
        )
    }

    #[test]
    fn a_picture_starts_centred_covering_the_circle_and_never_uncovers_it() {
        let mut crop = crop(400, 200);
        assert_eq!(crop.shown_size(), (192., 96.));
        assert_eq!(crop.offset(), (-48., 0.));
        crop.start_drag((10., 10.));
        crop.drag_to((500., -300.));
        assert_eq!(crop.offset(), (0., 0.));
        crop.drag_to((-900., 10.));
        assert_eq!(crop.offset(), (-96., 0.));
    }

    #[test]
    fn zooming_keeps_the_middle_in_the_middle() {
        let mut crop = crop(200, 200);
        crop.set_zoom(2.);
        assert_eq!(crop.offset(), (-48., -48.));
        crop.set_zoom(10.);
        assert_eq!(crop.shown_size(), (PREVIEW * MAX_ZOOM, PREVIEW * MAX_ZOOM));
        crop.set_zoom(0.5);
        assert_eq!(crop.offset(), (0., 0.));
    }

    #[test]
    fn the_kept_picture_is_round_and_shows_what_the_circle_did() {
        let mut crop = crop(400, 200);
        crop.start_drag((0., 0.));
        crop.drag_to((100., 0.));
        let kept = image::load_from_memory(&crop.png().unwrap()).unwrap().to_rgba8();
        assert_eq!(kept.dimensions(), (SIDE, SIDE));
        assert_eq!(kept.get_pixel(0, 0).0[3], 0);
        // Dragged right as far as it goes: the circle shows the picture's left half, red.
        assert_eq!(kept.get_pixel(SIDE / 2, SIDE / 2).0, [255, 0, 0, 255]);
    }
}

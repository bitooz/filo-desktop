// The part of a zoomed-in picture on screen, drawn at the screen's own pixels by crates/filo-resample from the
// picture cache's copy, which may come in tiles (pictures.rs, `Tiles`).

use gpui::RenderImage;

pub use filo_resample::Frame;

use crate::pictures::Tiles;

/// `frame` of the picture in `tiles` (its `size` theirs), BGRA.
pub fn render(tiles: &Tiles, frame: Frame) -> Option<RenderImage> {
    let read = filo_resample::reads(frame)?;
    let mut pixels = Vec::with_capacity(read.width as usize * read.height as usize * 4);
    for y in read.y..read.y + read.height {
        for x in read.x..read.x + read.width {
            pixels.extend(tiles.pixel(x, y)?);
        }
    }
    let drawn = filo_resample::render(&pixels, read, frame)?;
    let image = image::RgbaImage::from_raw(frame.out.0, frame.out.1, drawn)?;
    Some(RenderImage::new(vec![image::Frame::new(image)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn reads_across_tiles() {
        // The same picture whole and in 3 × 3 tiles draws the same.
        let image = image::RgbaImage::from_fn(40, 40, |x, y| {
            let value = ((x * 13 + y * 7) % 251) as u8;
            image::Rgba([value, value / 2, 255 - value, 255])
        });
        let whole = Tiles::single(Arc::new(RenderImage::new(vec![image::Frame::new(image)])));
        let tiled = Tiles::split(whole.images[0].clone(), 16);
        assert_eq!((tiled.images.len(), tiled.columns), (9, 3));
        let frame = Frame { size: (40, 40), scale: 2.5, origin: (-31.2, -27.9), out: (60, 50) };
        let (a, b) = (render(&whole, frame).unwrap(), render(&tiled, frame).unwrap());
        assert_eq!(a.as_bytes(0).unwrap(), b.as_bytes(0).unwrap());
    }
}

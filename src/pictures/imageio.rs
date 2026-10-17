// JPEG and PNG stills decoded by macOS ImageIO straight at the size they're kept at (`MAX_EDGE`), so the full-size
// picture is never allocated: decoding a 12 MP photo whole took 48 MB, and the allocator kept those blocks after they
// were freed (pictures.rs). ImageIO's thumbnail API decodes a JPEG at 1/2, 1/4 or 1/8 scale and reads a PNG row by
// row.
//
// Pixels come out as GPUI's own decoder makes them: BGRA with straight alpha, the image's own RGB values (no colour
// conversion) and the stored orientation (EXIF rotation isn't applied, as before). Anything else returns None and
// the caller decodes it the old way: other formats, animations, grey, CMYK or indexed colour, damaged files.

use std::ffi::c_void;
use std::sync::Arc;

use gpui::RenderImage;

type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> bool;
    fn CFRelease(object: CFTypeRef);
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageSourceShouldCache: CFTypeRef;
    static kCGImageSourceShouldCacheImmediately: CFTypeRef;
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImagePropertyPixelWidth: CFTypeRef;
    static kCGImagePropertyPixelHeight: CFTypeRef;
    fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceGetCount(source: CFTypeRef) -> usize;
    fn CGImageSourceCopyPropertiesAtIndex(source: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(source: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetWidth(image: CFTypeRef) -> usize;
    fn CGImageGetHeight(image: CFTypeRef) -> usize;
    fn CGImageGetColorSpace(image: CFTypeRef) -> CFTypeRef;
    fn CGColorSpaceGetModel(space: CFTypeRef) -> i32;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CFTypeRef,
        bitmap_info: u32,
    ) -> CFTypeRef;
    fn CGContextSetBlendMode(context: CFTypeRef, mode: i32);
    fn CGContextDrawImage(context: CFTypeRef, rect: CGRect, image: CFTypeRef);
}

const K_CF_NUMBER_SINT64: isize = 4;
const K_CG_COLOR_SPACE_MODEL_RGB: i32 = 1;
// kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little: B, G, R, A in memory.
const BGRA_PREMULTIPLIED: u32 = 2 | (2 << 12);
const K_CG_BLEND_MODE_COPY: i32 = 17;

/// A CoreFoundation object released when dropped.
struct Owned(CFTypeRef);

impl Owned {
    fn new(object: CFTypeRef) -> Option<Owned> {
        // Not `then_some(Owned(object))`: that builds an Owned for a null object too, and dropping it calls
        // CFRelease(NULL), which crashes.
        if object.is_null() { None } else { Some(Owned(object)) }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

fn dictionary(pairs: &[(CFTypeRef, CFTypeRef)]) -> Option<Owned> {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|pair| pair.0).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|pair| pair.1).collect();
    // SAFETY: CF objects that outlive the call; the dictionary retains them.
    Owned::new(unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            pairs.len() as isize,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    })
}

fn number(value: i64) -> Option<Owned> {
    Owned::new(unsafe { CFNumberCreate(std::ptr::null(), K_CF_NUMBER_SINT64, (&raw const value).cast()) })
}

fn dimension(properties: &Owned, key: CFTypeRef) -> Option<u32> {
    let value = unsafe { CFDictionaryGetValue(properties.0, key) };
    if value.is_null() {
        return None;
    }
    let mut out = 0i64;
    unsafe { CFNumberGetValue(value, K_CF_NUMBER_SINT64, (&raw mut out).cast()) }
        .then_some(out)
        .and_then(|v| u32::try_from(v).ok())
}

/// A JPEG or PNG still, no more than `max_edge` pixels across, and its size before; None for anything else.
pub(super) fn decode(bytes: &[u8], max_edge: u32) -> Option<(Arc<RenderImage>, (u32, u32))> {
    match image::guess_format(bytes).ok()? {
        image::ImageFormat::Jpeg | image::ImageFormat::Png => {}
        _ => return None,
    }
    // SAFETY: every CF object is checked for null and released by `Owned`; the bitmap context draws into `pixels`,
    // which outlives it.
    unsafe {
        let data = Owned::new(CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize))?;
        let source_options = dictionary(&[(kCGImageSourceShouldCache, kCFBooleanFalse)])?;
        let source = Owned::new(CGImageSourceCreateWithData(data.0, source_options.0))?;
        // An APNG holds more than one.
        if CGImageSourceGetCount(source.0) != 1 {
            return None;
        }
        let properties = Owned::new(CGImageSourceCopyPropertiesAtIndex(source.0, 0, source_options.0))?;
        let natural =
            (dimension(&properties, kCGImagePropertyPixelWidth)?, dimension(&properties, kCGImagePropertyPixelHeight)?);
        let max = number(i64::from(max_edge.min(natural.0.max(natural.1))))?;
        let thumbnail_options = dictionary(&[
            (kCGImageSourceCreateThumbnailFromImageAlways, kCFBooleanTrue),
            (kCGImageSourceThumbnailMaxPixelSize, max.0),
            (kCGImageSourceShouldCacheImmediately, kCFBooleanTrue),
        ])?;
        let image = Owned::new(CGImageSourceCreateThumbnailAtIndex(source.0, 0, thumbnail_options.0))?;
        let space = CGImageGetColorSpace(image.0);
        if space.is_null() || CGColorSpaceGetModel(space) != K_CG_COLOR_SPACE_MODEL_RGB {
            return None;
        }
        let (width, height) = (CGImageGetWidth(image.0), CGImageGetHeight(image.0));
        if width == 0 || height == 0 {
            return None;
        }
        let mut pixels = vec![0u8; width * height * 4];
        // In the image's own colour space, so its RGB values come through unchanged.
        let context = Owned::new(CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            space,
            BGRA_PREMULTIPLIED,
        ))?;
        CGContextSetBlendMode(context.0, K_CG_BLEND_MODE_COPY);
        CGContextDrawImage(context.0, CGRect { x: 0., y: 0., width: width as f64, height: height as f64 }, image.0);
        drop(context);
        unpremultiply(&mut pixels);
        let frame = image::RgbaImage::from_raw(width as u32, height as u32, pixels)?;
        Some((Arc::new(RenderImage::new(vec![image::Frame::new(frame)])), natural))
    }
}

/// GPUI draws straight alpha; CoreGraphics only draws 8-bit colour premultiplied.
fn unpremultiply(bgra: &mut [u8]) {
    for pixel in bgra.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn encode(image: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
        bytes
    }

    fn size(image: &RenderImage) -> (i32, i32) {
        let size = image.size(0);
        (size.width.0, size.height.0)
    }

    /// B, G, R, A at (x, y).
    fn pixel(image: &RenderImage, x: usize, y: usize) -> [u8; 4] {
        let width = size(image).0 as usize;
        let bytes = image.as_bytes(0).unwrap();
        let at = (y * width + x) * 4;
        [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
    }

    fn near(actual: [u8; 4], expected: [u8; 4], tolerance: u8) -> bool {
        actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= tolerance)
    }

    #[test]
    fn a_null_object_is_none_and_never_released() {
        // Before: Owned(NULL) was built and dropped, calling CFRelease(NULL), which aborts the process.
        assert!(Owned::new(std::ptr::null()).is_none());
    }

    #[test]
    fn a_big_jpeg_comes_out_at_the_kept_size_in_bgra() {
        // A 12 MP photo's size, reddish (R 200, G 80, B 30).
        let photo = image::RgbImage::from_pixel(4032, 3024, image::Rgb([200, 80, 30]));
        let bytes = encode(photo.into(), image::ImageFormat::Jpeg);
        let (picture, natural) = decode(&bytes, 1600).expect("decoded by ImageIO");
        assert_eq!(natural, (4032, 3024));
        assert_eq!(size(&picture), (1600, 1200));
        assert!(near(pixel(&picture, 800, 600), [30, 80, 200, 255], 4), "{:?}", pixel(&picture, 800, 600));
    }

    #[test]
    fn matches_the_old_decoder_for_a_jpeg() {
        // Left half blue, right half red: the same colours, and the same way round, as GPUI's own decoder.
        let photo = image::RgbImage::from_fn(2400, 1200, |x, _| {
            if x < 1200 { image::Rgb([20, 40, 220]) } else { image::Rgb([230, 30, 20]) }
        });
        let bytes = encode(photo.into(), image::ImageFormat::Jpeg);
        let (picture, _) = decode(&bytes, 1600).unwrap();
        let old = image::load_from_memory(&bytes).unwrap().into_rgba8();
        let old_left = old.get_pixel(300, 600).0;
        let old_right = old.get_pixel(2100, 600).0;
        assert_eq!(size(&picture), (1600, 800));
        assert!(near(pixel(&picture, 200, 400), [old_left[2], old_left[1], old_left[0], 255], 6));
        assert!(near(pixel(&picture, 1400, 400), [old_right[2], old_right[1], old_right[0], 255], 6));
    }

    #[test]
    fn keeps_the_stored_orientation_like_the_old_decoder() {
        // A landscape JPEG tagged EXIF orientation 6 (rotate 90°): GPUI's decoder showed it as stored, so does this.
        let photo = image::RgbImage::from_pixel(3000, 1000, image::Rgb([10, 120, 60]));
        let mut bytes = encode(photo.into(), image::ImageFormat::Jpeg);
        // APP1 Exif segment: big-endian TIFF header, one IFD entry, Orientation (0x0112) SHORT = 6.
        let exif: [u8; 36] = [
            0xFF, 0xE1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'M', b'M', 0x00, 0x2A, 0, 0, 0, 8, 0x00, 0x01, 0x01,
            0x12, 0x00, 0x03, 0, 0, 0, 1, 0x00, 0x06, 0, 0, 0, 0, 0, 0,
        ];
        bytes.splice(2..2, exif);
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 3000, "old decoder ignores EXIF orientation");
        let (picture, natural) = decode(&bytes, 1600).unwrap();
        assert_eq!(natural, (3000, 1000));
        assert_eq!(size(&picture), (1600, 533));
    }

    #[test]
    fn a_png_keeps_straight_alpha() {
        let picture = image::RgbaImage::from_fn(3200, 800, |x, _| {
            if x < 1600 { image::Rgba([200, 50, 10, 128]) } else { image::Rgba([0, 0, 0, 0]) }
        });
        let bytes = encode(picture.into(), image::ImageFormat::Png);
        let (decoded, natural) = decode(&bytes, 1600).unwrap();
        assert_eq!(natural, (3200, 800));
        assert_eq!(size(&decoded), (1600, 400));
        assert!(near(pixel(&decoded, 400, 200), [10, 50, 200, 128], 3), "{:?}", pixel(&decoded, 400, 200));
        assert_eq!(pixel(&decoded, 1200, 200)[3], 0);
    }

    #[test]
    fn a_small_picture_keeps_its_size() {
        let bytes =
            encode(image::RgbImage::from_pixel(800, 600, image::Rgb([1, 2, 3])).into(), image::ImageFormat::Png);
        let (picture, natural) = decode(&bytes, 1600).unwrap();
        assert_eq!((natural, size(&picture)), ((800, 600), (800, 600)));
    }

    #[test]
    fn leaves_other_pictures_to_the_old_decoder() {
        // Damaged: a JPEG header and then noise.
        let mut damaged = vec![0xFF, 0xD8, 0xFF, 0xE0];
        damaged.extend((0..4000u32).map(|i| (i * 7 % 251) as u8));
        assert!(decode(&damaged, 1600).is_none());
        assert!(decode(b"not a picture", 1600).is_none());
        // Other formats keep GPUI's decoding (animations, SVG sizing).
        let gif =
            encode(image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255])).into(), image::ImageFormat::Gif);
        assert!(decode(&gif, 1600).is_none());
        // Grey keeps the old colour handling too.
        let grey = encode(image::GrayImage::from_pixel(2000, 2000, image::Luma([90])).into(), image::ImageFormat::Jpeg);
        assert!(decode(&grey, 1600).is_none());
    }
}

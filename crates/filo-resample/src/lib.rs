// The part of a zoomed-in picture that's on screen, drawn on the CPU at the screen's own pixels (the app's picture
// viewer, src/media_viewer.rs). GPUI's sampler is linear, so a picture shown bigger than its pixels went soft (text
// at 256% read worse than in Preview). Here each screen pixel is weighed from the 6 × 6 picture pixels around it
// (Lanczos3), then held between the darkest and the lightest of the 2 × 2 nearest, so an edge has none of the light
// and dark rings Lanczos leaves beside it. Any filter that goes through the picture's pixels still ramps across a
// whole picture pixel at an edge (a letter's stroke 2.5 screen pixels of grey at 256%), so between two pixels far
// apart in shade a screen pixel reads as if it sat nearer the closer one (`steeper`, across then down): text keeps a
// crisp edge at any zoom, a screen pixel on a picture pixel stays its shade, and a photo's gentle shading stays as it
// was. Done in premultiplied alpha, so a transparent edge doesn't bleed colour. Pixels are 4 bytes, alpha last (the
// app's are BGRA).
//
// A crate of its own so debug builds, which the app runs in day to day, still build it optimised (Cargo.toml,
// `profile.dev.package.filo-resample`): unoptimised, a window's worth took 10 s. Nothing here is generic, so none of
// it is compiled in the app's crate instead.

/// How far either side a picture pixel counts, in picture pixels.
const LOBES: usize = 3;
/// Picture pixels read before and after the nearest one.
const BEFORE: usize = LOBES - 1;
const TAPS: usize = 2 * LOBES;
/// Steps between two picture pixels the weights are worked out for.
const STEPS: usize = 1024;
/// Between neighbours this far apart in shade (of 255) an edge starts getting steeper, and from this far it's
/// steepest.
const EDGE_FROM: f32 = 32.;
const EDGE_FULL: f32 = 128.;

/// What to draw: a picture `size` pixels, `scale` screen pixels to one of its pixels, its top left at `origin` from
/// the drawn part's top left (in screen pixels), the drawn part `out` screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub size: (u32, u32),
    pub scale: f64,
    pub origin: (f64, f64),
    pub out: (u32, u32),
}

/// The picture pixels a frame reads: from `x`, `y`, `width` × `height`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Read {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The picture pixels `frame` reads; None when there's nothing to draw.
pub fn reads(frame: Frame) -> Option<Read> {
    let (width, height) = frame.size;
    if width == 0 || height == 0 || frame.out.0 == 0 || frame.out.1 == 0 || frame.scale <= 0. {
        return None;
    }
    let along = |count: u32, origin: f64, length: u32| {
        let first = place(0, origin, frame.scale, length).0.saturating_sub(BEFORE);
        let last = (place(count as usize - 1, origin, frame.scale, length).0 + LOBES).min(length as usize - 1);
        (first as u32, (last - first + 1) as u32)
    };
    let (x, width) = along(frame.out.0, frame.origin.0, width);
    let (y, height) = along(frame.out.1, frame.origin.1, height);
    Some(Read { x, y, width, height })
}

/// The drawn part, `frame.out` pixels, from `pixels`: the picture's `read` part (`reads`), row by row.
pub fn render(pixels: &[u8], read: Read, frame: Frame) -> Option<Vec<u8>> {
    if reads(frame)? != read || pixels.len() != read.width as usize * read.height as usize * 4 {
        return None;
    }
    let (out_width, out_height) = (frame.out.0 as usize, frame.out.1 as usize);
    let span = read.width as usize;
    let table = Weights::new();
    // Premultiplied.
    let source: Vec<[f32; 4]> = pixels
        .chunks_exact(4)
        .map(|pixel| {
            let alpha = pixel[3] as f32 / 255.;
            [pixel[0] as f32 * alpha, pixel[1] as f32 * alpha, pixel[2] as f32 * alpha, pixel[3] as f32]
        })
        .collect();
    let columns: Vec<(usize, f32)> =
        (0..out_width).map(|index| place(index, frame.origin.0, frame.scale, frame.size.0)).collect();
    let rows: Vec<(usize, f32)> =
        (0..out_height).map(|index| place(index, frame.origin.1, frame.scale, frame.size.1)).collect();
    // In `read`, held to the picture: past its edges a pixel counts as the edge's.
    let local_x =
        |x: usize, offset: isize| (x as isize + offset).clamp(0, frame.size.0 as isize - 1) as usize - read.x as usize;
    let local_y =
        |y: usize, offset: isize| (y as isize + offset).clamp(0, frame.size.1 as isize - 1) as usize - read.y as usize;
    // Across first, for every row read, then down.
    let mut across = vec![[0f32; 4]; out_width * read.height as usize];
    for row in 0..read.height as usize {
        let line = &source[row * span..][..span];
        let target = &mut across[row * out_width..][..out_width];
        for (&(nearest, fraction), out) in columns.iter().zip(target.iter_mut()) {
            let (here, next) = (line[local_x(nearest, 0)], line[local_x(nearest, 1)]);
            let weights = table.at(steeper(fraction, here, next));
            let mut sum = [0f32; 4];
            for (tap, weight) in weights.iter().enumerate() {
                let pixel = line[local_x(nearest, tap as isize - BEFORE as isize)];
                for channel in 0..4 {
                    sum[channel] += pixel[channel] * weight;
                }
            }
            *out = sum;
        }
    }
    let mut pixels = vec![0u8; out_width * out_height * 4];
    for (row, &(nearest_y, fraction)) in rows.iter().enumerate() {
        let lines: [usize; TAPS] =
            std::array::from_fn(|tap| local_y(nearest_y, tap as isize - BEFORE as isize) * out_width);
        let (here_line, next_line) = (local_y(nearest_y, 0) * out_width, local_y(nearest_y, 1) * out_width);
        let (y0, y1) = (local_y(nearest_y, 0) * span, local_y(nearest_y, 1) * span);
        for (column, &(nearest_x, _)) in columns.iter().enumerate() {
            let weights = table.at(steeper(fraction, across[here_line + column], across[next_line + column]));
            let mut sum = [0f32; 4];
            for (line, weight) in lines.iter().zip(weights.iter()) {
                let pixel = across[line + column];
                for channel in 0..4 {
                    sum[channel] += pixel[channel] * weight;
                }
            }
            // Held within the nearest four, which takes the rings off an edge.
            let (x0, x1) = (local_x(nearest_x, 0), local_x(nearest_x, 1));
            let near = [source[y0 + x0], source[y0 + x1], source[y1 + x0], source[y1 + x1]];
            for channel in 0..4 {
                let (mut low, mut high) = (near[0][channel], near[0][channel]);
                for pixel in &near[1..] {
                    low = low.min(pixel[channel]);
                    high = high.max(pixel[channel]);
                }
                sum[channel] = sum[channel].clamp(low, high);
            }
            let alpha = sum[3].clamp(0., 255.);
            let out = &mut pixels[(row * out_width + column) * 4..][..4];
            for channel in 0..3 {
                let value = if alpha > 0. { sum[channel] * 255. / alpha } else { 0. };
                out[channel] = value.round().clamp(0., 255.) as u8;
            }
            out[3] = alpha.round() as u8;
        }
    }
    Some(pixels)
}

/// Where screen pixel `index` falls in the picture (`length` pixels, starting at `origin`, `scale` screen pixels to
/// one of its pixels): the picture pixel at or before its middle, and how far on towards the next (0 to 1). Before
/// the first pixel's middle it's on the first; past the last's, on the last.
fn place(index: usize, origin: f64, scale: f64, length: u32) -> (usize, f32) {
    let centre = ((index as f64 + 0.5 - origin) / scale - 0.5).clamp(0., length as f64 - 1.);
    let nearest = centre.floor();
    (nearest as usize, (centre - nearest) as f32)
}

/// `fraction` of the way from `here` to `next`, read nearer the closer of them along a smoothstep the further apart
/// they are in shade (not at all under `EDGE_FROM`).
fn steeper(fraction: f32, here: [f32; 4], next: [f32; 4]) -> f32 {
    let mut apart = 0f32;
    for channel in 0..4 {
        apart = apart.max((here[channel] - next[channel]).abs());
    }
    let edge = ((apart - EDGE_FROM) / (EDGE_FULL - EDGE_FROM)).clamp(0., 1.);
    let smooth = fraction * fraction * (3. - 2. * fraction);
    fraction + edge * (smooth - fraction)
}

/// The Lanczos3 weights of the picture pixels from `BEFORE` before the nearest to `LOBES` after it, for points
/// `STEPS` to a pixel past the nearest.
struct Weights(Vec<[f32; TAPS]>);

impl Weights {
    fn new() -> Weights {
        Weights(
            (0..=STEPS)
                .map(|step| {
                    let fraction = step as f64 / STEPS as f64;
                    let mut weights = [0f64; TAPS];
                    for (tap, weight) in weights.iter_mut().enumerate() {
                        *weight = lanczos(tap as f64 - BEFORE as f64 - fraction);
                    }
                    let total: f64 = weights.iter().sum();
                    weights.map(|weight| (weight / total) as f32)
                })
                .collect(),
        )
    }

    fn at(&self, fraction: f32) -> &[f32; TAPS] {
        &self.0[(fraction * STEPS as f32).round() as usize]
    }
}

fn lanczos(x: f64) -> f64 {
    let x = x.abs();
    if x < 1e-9 {
        return 1.;
    }
    if x >= LOBES as f64 {
        return 0.;
    }
    let pi_x = std::f64::consts::PI * x;
    LOBES as f64 * pi_x.sin() * (pi_x / LOBES as f64).sin() / (pi_x * pi_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` × `height` picture, `shade(x, y)` grey, opaque; drawn as `frame` says.
    fn draw(
        width: u32,
        height: u32,
        shade: impl Fn(u32, u32) -> u8,
        scale: f64,
        origin: (f64, f64),
        out: (u32, u32),
    ) -> Vec<Vec<u8>> {
        let frame = Frame { size: (width, height), scale, origin, out };
        let read = reads(frame).unwrap();
        let mut pixels = Vec::new();
        for y in read.y..read.y + read.height {
            for x in read.x..read.x + read.width {
                let value = shade(x, y);
                pixels.extend([value, value, value, 255]);
            }
        }
        let drawn = render(&pixels, read, frame).unwrap();
        drawn.chunks(out.0 as usize * 4).map(|row| row.chunks(4).map(|pixel| pixel[0]).collect()).collect()
    }

    #[test]
    fn an_edge_stays_sharp_and_has_no_rings() {
        // Black to white between the 4th and 5th pixel, shown 4 screen pixels to one.
        let shades = draw(8, 2, |x, _| if x < 4 { 0 } else { 255 }, 4., (0., 0.), (32, 8)).swap_remove(4);
        assert!(shades[..14].iter().all(|&shade| shade == 0), "no light ring before the edge: {shades:?}");
        assert!(shades[18..].iter().all(|&shade| shade == 255), "no dark ring after it: {shades:?}");
        // Linear filtering ramps over 4 screen pixels (one picture pixel: 32, 96, 159, 223); this is steeper.
        let grey = shades.iter().filter(|&&shade| shade > 10 && shade < 245).count();
        assert!(grey <= 2 && shades[15] < 96 && shades[16] > 159, "{shades:?}");
    }

    #[test]
    fn gentle_shading_is_left_smooth() {
        // A gradient 16 shades a pixel apart, shown 4 screen pixels to one: each screen pixel a little further on.
        let shades = draw(8, 2, |x, _| 60 + x as u8 * 16, 4., (0., 0.), (32, 8)).swap_remove(4);
        assert!(shades[2..30].windows(2).all(|pair| pair[1] >= pair[0] && pair[1] - pair[0] <= 5), "{shades:?}");
    }

    #[test]
    fn at_its_own_size_a_picture_comes_out_unchanged() {
        let shade = |x: u32, y: u32| ((x * 37 + y * 91) % 256) as u8;
        let drawn = draw(16, 16, shade, 1., (-3., -5.), (8, 8));
        for (y, row) in drawn.iter().enumerate() {
            let expected: Vec<u8> = (0..8).map(|x| shade(x + 3, y as u32 + 5)).collect();
            assert_eq!(row, &expected);
        }
    }

    #[test]
    fn reads_only_what_it_draws() {
        // 3 screen pixels to one, the picture's top left 300 screen pixels up and left: the first screen pixel is on
        // picture pixel 99.67, so from 2 before pixel 99 to 3 past pixel 109, the last one's.
        let read = reads(Frame { size: (720, 5000), scale: 3., origin: (-300., -300.), out: (30, 30) }).unwrap();
        assert_eq!(read, Read { x: 97, y: 97, width: 16, height: 16 });
        assert!(
            render(&[0; 4], read, Frame { size: (720, 5000), scale: 3., origin: (-300., -300.), out: (30, 30) })
                .is_none()
        );
    }
}

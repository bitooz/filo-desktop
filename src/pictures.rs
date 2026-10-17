// Pictures from the network or disk (chat attachments, link and file thumbnails, feed covers, artifact previews,
// coding proofs) through one bounded cache.
//
// GPUI's own `img(url)` keeps every picture it ever loaded for as long as the app runs, decoded at full size
// both in memory and in each window's GPU atlas: a 12 MP photo is 48 MB in each, and an app left open for hours
// reached 1.4 GB, most of it pictures (vmmap: MALLOC_LARGE regions of 20–48 MB, the same again as
// IOAccelerator). Here a picture is kept at most `MAX_EDGE` pixels across (the largest box one is drawn in is
// about 700 points). It's fetched (within `FETCH_TIMEOUT`), then decoded one at a time: JPEG and PNG stills by
// ImageIO straight at that size (imageio.rs, macOS), other raster formats from the same bytes the way GPUI's loader
// decodes them and then scaled down, SVG by GPUI's loader. A finished load is recorded when it finishes, whether or
// not its page is still drawn. Past `BUDGET` bytes the least recently drawn pictures are dropped from memory and
// the GPU: after the frame that loaded a new one, and again when pictures kept for being drawn lately can go.
// Never dropped: a picture drawn in the last `KEEP`, or one its window drew in its latest frame however long ago.
// GPUI keeps presenting a window's latest frame without drawing it again (an idle window, a second after input),
// and that frame must still find its textures: Metal's atlas panics on a texture that's gone. The latest frame is
// told by the latest picture its window looked up; a closed window pins nothing. This relies on every frame looking
// up the pictures it paints, so don't draw these pictures inside `AnyView::cached` (GPUI then replays an earlier
// paint without looking anything up) or in more than one window. A picture that failed is tried again when drawn `RETRY_AFTER`
// later, up to `ATTEMPTS` times. At most `FETCHES` downloads run at once, each held until its picture is decoded,
// so a burst can't pile up encoded bytes behind the decode turn.
//
// `img(pictures::source(url))` instead of `img(url)`; embedded pictures (avatars, icons) stay on GPUI's cache,
// they are few and small. The picture viewer zoomed in needs more than `MAX_EDGE` pixels (a 720 × 5000 screenshot
// was kept at 230 × 1600, and its text blurred at any zoom): `sharp` loads a sharper copy of the picture into the
// same cache, at the size it's shown (`sharp_edge`), and gives the sharpest copy loaded until it's there. A copy
// bigger than GPUI's largest texture is kept in tiles (`Tiles`).

mod imageio;

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{
    App, AppContext as _, Asset as _, Context, Entity, Global, ImageCacheError, ImageSource, RenderImage, Resource,
    Task, Window,
};

/// Longest side, in pixels, a picture is decoded at.
const MAX_EDGE: u32 = 1600;
/// Most pixels a sharper copy holds: 256 MB decoded (a 1440 × 30000 page whole).
const SHARP_MAX_PIXELS: f64 = (64 << 20) as f64;
/// GPUI's largest atlas texture (gpui metal_atlas.rs, MAX_ATLAS_SIZE): a picture longer than this is kept in tiles.
const TEXTURE_EDGE: u32 = 16384;
/// A tile's side.
const TILE: u32 = 4096;
/// Decoded bytes kept for pictures not drawn lately.
const BUDGET: usize = 128 << 20;
/// How long after it was last drawn a picture can't be dropped.
const KEEP: Duration = Duration::from_secs(30);
/// Longest a picture's download may take (the HTTP client itself has no timeout).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// A picture that failed is tried again when drawn this long after, up to `ATTEMPTS` loads in all.
const RETRY_AFTER: Duration = Duration::from_secs(30);
const ATTEMPTS: u8 = 3;
/// Downloads at once.
const FETCHES: usize = 4;
/// Lookups this close to a window's latest one belong to its latest frame (a frame draws in milliseconds).
const FRAME_SLACK: Duration = Duration::from_secs(1);
/// A failure nobody has looked at for this long is forgotten (tried afresh if it shows again).
const FORGET_FAILURE: Duration = Duration::from_secs(600);

static FETCHING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(FETCHES);

/// Taken while a picture is decoded (not while it downloads): macOS keeps freed full-size decodes for reuse, so one
/// at a time leaves one or two behind instead of one per picture loading at once (a coding task with eight
/// screenshots: 72 MB of them instead of 164 MB).
static DECODING: std::sync::LazyLock<futures::lock::Mutex<()>> =
    std::sync::LazyLock::new(|| futures::lock::Mutex::new(()));

/// `source` loaded through the bounded cache when it's a URL or a file, as is otherwise.
pub fn source(source: impl Into<ImageSource>) -> ImageSource {
    match source.into() {
        ImageSource::Resource(resource @ (Resource::Uri(_) | Resource::Path(_))) => {
            let key = (resource, MAX_EDGE);
            ImageSource::Custom(Arc::new(move |window, cx| {
                load(&key, window, cx).map(|result| result.map(|picture| picture.image()))
            }))
        }
        other => other,
    }
}

/// The sharpest copy of `resource` there is to draw: `edge` pixels across its longest side (`sharp_edge`) once that has
/// loaded; until then (or if it can't) the sharpest one loaded already, else the usual one. Looks it up as drawn, so
/// a frame that draws it calls this.
pub fn sharp(resource: &Resource, edge: u32, window: &mut Window, cx: &mut App) -> Option<Tiles> {
    if edge > MAX_EDGE {
        let key = (resource.clone(), edge);
        if let Some(Ok(picture)) = load(&key, window, cx) {
            return Some(picture.tiles);
        }
        let window_key = window.window_handle().window_id().as_u64();
        if let Some(picture) = cache(cx).update(cx, |cache, _| cache.sharpest_loaded(&key, window_key, Instant::now()))
        {
            return Some(picture.tiles);
        }
    }
    match load(&(resource.clone(), MAX_EDGE), window, cx) {
        Some(Ok(picture)) => Some(picture.tiles),
        _ => None,
    }
}

/// The longest side to decode a picture `natural` pixels across at, to show it `shown` pixels across (both its
/// longest side): `MAX_EDGE` doubled until it's enough, at most its own size and `SHARP_MAX_PIXELS`.
/// Doubling keeps the copies few, and none is drawn at less than half its size (GPUI samples without mipmaps, so a
/// copy much bigger than its box would flicker).
pub fn sharp_edge(natural: (f32, f32), shown: f32) -> u32 {
    let longest = natural.0.max(natural.1).max(1.);
    let by_pixels = longest * (SHARP_MAX_PIXELS / (natural.0 as f64 * natural.1 as f64).max(1.)).sqrt().min(1.) as f32;
    let sharpest = longest.min(by_pixels) as u32;
    let mut edge = MAX_EDGE;
    while (edge as f32) < shown && edge < sharpest {
        edge *= 2;
    }
    edge.min(sharpest).max(MAX_EDGE)
}

/// What the cache holds, for the memory soak (smoke.rs): pictures loaded and still loading, the loaded ones' bytes,
/// and how many loads were started and pictures dropped since launch.
#[cfg(feature = "dev")]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub loaded: usize,
    pub loading: usize,
    pub bytes: usize,
    pub started: usize,
    pub dropped: usize,
}

#[cfg(feature = "dev")]
pub fn stats(cx: &App) -> Stats {
    cx.try_global::<PicturesGlobal>().map(|global| global.0.read(cx).stats()).unwrap_or_default()
}

/// What the cache has for `url`, without counting as drawn: "loaded", "loading", "failed" or "none" (for logs).
pub fn state(url: &str, cx: &App) -> &'static str {
    state_of(&Resource::Uri(gpui::SharedUri::from(url.to_string())), cx)
}

/// `state` for a URL or a file.
pub fn state_of(resource: &Resource, cx: &App) -> &'static str {
    let Some(global) = cx.try_global::<PicturesGlobal>() else { return "none" };
    match global.0.read(cx).entries.get(&(resource.clone(), MAX_EDGE)).map(|entry| &entry.state) {
        Some(State::Loaded(Ok(_))) => "loaded",
        Some(State::Loaded(Err(_))) => "failed",
        Some(State::Loading(_)) => "loading",
        None => "none",
    }
}

/// Which boxes of a long page of pictures were on screen when last drawn, so the rest leave their pictures out and the
/// cache can drop them: every picture drawn in a frame is kept (see the top), and a grid in a scrolling page drew them
/// all. The box's `probe` records it in prepaint, with a screen's margin; when that changes, its window is drawn again
/// at the next step (`ui::ticker`). Pages that can be a `list` should be one instead.
#[derive(Clone, Default)]
pub struct OnScreen(std::rc::Rc<std::cell::RefCell<HashMap<gpui::SharedString, bool>>>);

impl OnScreen {
    /// Whether box `key` shows its picture: on screen (or near) when last drawn; `first` before it ever was.
    pub fn shows(&self, key: &gpui::SharedString, first: bool) -> bool {
        self.0.borrow().get(key).copied().unwrap_or(first)
    }

    /// Goes in the box (`relative`): records where it was drawn.
    pub fn probe(&self, key: gpui::SharedString) -> impl gpui::IntoElement {
        use gpui::Styled as _;
        let seen = self.0.clone();
        gpui::canvas(
            move |bounds, window, cx| {
                let height = window.viewport_size().height;
                let near = bounds.bottom() > -height && bounds.top() < height * 2.;
                if seen.borrow_mut().insert(key.clone(), near) != Some(near) {
                    crate::ui::ticker::keep(window, cx);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

/// The picture's size in pixels as it was before it was scaled down, for a URL or a file, once it has loaded. A file's
/// size is read from its header until it has loaded, so a picture sent straight after it was attached is laid out at
/// its own shape from its first frame, not a placeholder's it then jumps from.
pub fn natural_size_of(resource: &Resource, window: &mut Window, cx: &mut App) -> Option<(f32, f32)> {
    if let Some(Ok(picture)) = load(&(resource.clone(), MAX_EDGE), window, cx) {
        return Some((picture.natural.0 as f32, picture.natural.1 as f32));
    }
    let Resource::Path(path) = resource else { return None };
    header_size(path)
}

/// A file's size in pixels from its header, without decoding it (the same size ImageIO reports, before any EXIF
/// turn), remembered per file.
fn header_size(path: &Arc<std::path::Path>) -> Option<(f32, f32)> {
    static SIZES: std::sync::Mutex<Option<HashMap<Arc<std::path::Path>, Option<(u32, u32)>>>> =
        std::sync::Mutex::new(None);
    let mut sizes = SIZES.lock().unwrap();
    let sizes = sizes.get_or_insert_default();
    if sizes.len() > 256 {
        sizes.clear();
    }
    let size = *sizes.entry(path.clone()).or_insert_with(|| image::image_dimensions(path).ok());
    size.map(|(width, height)| (width as f32, height as f32))
}

fn cache(cx: &mut App) -> Entity<Pictures> {
    match cx.try_global::<PicturesGlobal>() {
        Some(global) => global.0.clone(),
        None => {
            let cache = cx.new(|_| Pictures::default());
            cx.set_global(PicturesGlobal(cache.clone()));
            cache
        }
    }
}

fn load(key: &Key, window: &mut Window, cx: &mut App) -> Option<Result<Picture, ImageCacheError>> {
    let cache = cache(cx);
    let (result, loaded) = cache.update(cx, |cache, cx| cache.load(key, window, cx));
    if loaded {
        cx.defer(move |cx| cache.update(cx, |cache, cx| cache.sweep(cx)));
    }
    result
}

struct PicturesGlobal(Entity<Pictures>);

impl Global for PicturesGlobal {}

#[derive(Clone)]
struct Picture {
    tiles: Tiles,
    natural: (u32, u32),
}

impl Picture {
    fn whole(image: Arc<RenderImage>, natural: (u32, u32)) -> Picture {
        Picture { tiles: Tiles::fit(image), natural }
    }

    /// The picture, or its first tile when it's in more than one.
    fn image(&self) -> Arc<RenderImage> {
        self.tiles.images[0].clone()
    }

    fn bytes(&self) -> usize {
        self.tiles
            .images
            .iter()
            .map(|image| {
                (0..image.frame_count()).map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len)).sum::<usize>()
            })
            .sum()
    }
}

/// A picture as one texture, or as `TILE`-pixel squares row by row when it's longer than GPUI's largest
/// (`TEXTURE_EDGE`); the last in a row or a column may be smaller.
#[derive(Clone)]
pub struct Tiles {
    pub images: Vec<Arc<RenderImage>>,
    /// Tiles in a row.
    pub columns: usize,
    /// A tile's side (the whole picture's longest when it's one).
    pub tile: u32,
    /// The whole picture's, in pixels.
    pub size: (u32, u32),
}

impl Tiles {
    pub fn single(image: Arc<RenderImage>) -> Tiles {
        let size = image.size(0);
        let size = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
        Tiles { images: vec![image], columns: 1, tile: size.0.max(size.1).max(1), size }
    }

    fn fit(image: Arc<RenderImage>) -> Tiles {
        let size = image.size(0);
        if size.width.0 as u32 <= TEXTURE_EDGE && size.height.0 as u32 <= TEXTURE_EDGE {
            return Tiles::single(image);
        }
        Tiles::split(image, TILE)
    }

    /// `image` cut into `tile`-pixel squares.
    pub fn split(image: Arc<RenderImage>, tile: u32) -> Tiles {
        let whole = Tiles::single(image);
        let (width, height) = whole.size;
        let Some(bytes) = whole.images[0].as_bytes(0) else { return whole };
        let columns = width.div_ceil(tile);
        let mut images = Vec::new();
        for top in (0..height).step_by(tile as usize) {
            for column in 0..columns {
                let left = column * tile;
                let (tile_width, tile_height) = (tile.min(width - left), tile.min(height - top));
                let mut pixels = Vec::with_capacity((tile_width * tile_height * 4) as usize);
                for y in top..top + tile_height {
                    let start = ((y * width + left) * 4) as usize;
                    pixels.extend_from_slice(&bytes[start..start + (tile_width * 4) as usize]);
                }
                let Some(frame) = image::RgbaImage::from_raw(tile_width, tile_height, pixels) else { return whole };
                images.push(Arc::new(RenderImage::new(vec![image::Frame::new(frame)])));
            }
        }
        Tiles { images, columns: columns as usize, tile, size: (width, height) }
    }

    /// Tile `index`'s top left and size, in the picture's pixels.
    pub fn bounds(&self, index: usize) -> ((u32, u32), (u32, u32)) {
        let size = self.images[index].size(0);
        let origin = ((index % self.columns) as u32 * self.tile, (index / self.columns) as u32 * self.tile);
        (origin, (size.width.0.max(0) as u32, size.height.0.max(0) as u32))
    }

    /// The pixel at `x`, `y`, BGRA.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let index = (y / self.tile) as usize * self.columns + (x / self.tile) as usize;
        let image = self.images.get(index)?;
        let width = image.size(0).width.0.max(0) as u32;
        let at = (((y % self.tile) * width + x % self.tile) * 4) as usize;
        image.as_bytes(0)?.get(at..at + 4)?.try_into().ok()
    }
}

type Loading = Shared<Task<Result<Picture, ImageCacheError>>>;

/// A picture and the longest side it's kept at: `MAX_EDGE`, or a sharper copy's (`sharp_source`).
type Key = (Resource, u32);

enum State {
    Loading(Loading),
    Loaded(Result<Picture, ImageCacheError>),
}

struct Entry {
    state: State,
    /// Last looked up by a frame (a load starts from one).
    drawn: Instant,
    /// When the last load finished, and how many loads there have been.
    finished: Instant,
    attempts: u8,
    /// Tells this entry's load from an earlier one for the same picture.
    generation: u64,
    /// The window that last drew it.
    window: u64,
}

/// What the cache has for a picture.
enum Lookup {
    Ready {
        result: Result<Picture, ImageCacheError>,
        just_loaded: bool,
    },
    Pending,
    /// Load it (again): `attempts` counts this load.
    Start {
        attempts: u8,
    },
}

#[derive(Default)]
struct Pictures {
    entries: HashMap<Key, Entry>,
    /// Loads started and pictures dropped since launch (`stats`, the memory soak).
    #[cfg(feature = "dev")]
    started: usize,
    #[cfg(feature = "dev")]
    dropped: usize,
    generation: u64,
    /// Each window's latest picture lookup: when it last drew a frame with pictures.
    latest_frame: HashMap<u64, Instant>,
    /// The windows still open, as of the last sweep (None: not known yet, so every window counts as open).
    open_windows: Option<std::collections::HashSet<u64>>,
    /// A window drew a new frame since the last sweep: what it no longer shows may go.
    new_frame: bool,
    /// The next sweep, while pictures over the budget are kept only for being drawn lately; replacing it cancels it.
    sweep: Option<Task<()>>,
}

impl Pictures {
    /// The picture if it has loaded, and whether it just did.
    fn load(
        &mut self,
        key: &Key,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<Result<Picture, ImageCacheError>>, bool) {
        let now = Instant::now();
        let window_key = window.window_handle().window_id().as_u64();
        let lookup = self.lookup(key, window_key, now);
        let sweep = std::mem::take(&mut self.new_frame);
        let attempts = match lookup {
            Lookup::Ready { result, just_loaded } => return (Some(result), just_loaded || sweep),
            Lookup::Pending => return (None, sweep),
            Lookup::Start { attempts } => attempts,
        };
        let task = start_load(key, cx).shared();
        let generation = self.begin(key.clone(), task.clone(), attempts, window_key, now);
        // Record it when it finishes, drawn or not, then draw its view again.
        let view = window.current_view();
        let key = key.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |cache, cx| {
                if cache.finish(&key, generation, result, Instant::now()) {
                    let app: &mut App = cx;
                    app.notify(view);
                }
                cache.sweep(cx);
            })
            .ok();
        })
        .detach();
        (None, sweep)
    }

    fn lookup(&mut self, key: &Key, window: u64, now: Instant) -> Lookup {
        if self.latest_frame.insert(window, now).is_none_or(|latest| now.duration_since(latest) > FRAME_SLACK) {
            self.new_frame = true;
        }
        let Some(entry) = self.entries.get_mut(key) else { return Lookup::Start { attempts: 1 } };
        entry.window = window;
        let result = match &entry.state {
            State::Loaded(Err(_)) if entry.attempts < ATTEMPTS && now.duration_since(entry.finished) >= RETRY_AFTER => {
                let attempts = entry.attempts + 1;
                self.entries.remove(key);
                return Lookup::Start { attempts };
            }
            State::Loaded(result) => {
                entry.drawn = now;
                return Lookup::Ready { result: result.clone(), just_loaded: false };
            }
            State::Loading(task) => match task.clone().now_or_never() {
                Some(result) => result,
                None => {
                    entry.drawn = now;
                    return Lookup::Pending;
                }
            },
        };
        entry.drawn = now;
        entry.finished = now;
        entry.state = State::Loaded(result.clone());
        Lookup::Ready { result, just_loaded: true }
    }

    /// The sharpest copy of `key`'s picture loaded at less than `key`'s size and more than `MAX_EDGE`, counted as
    /// drawn.
    fn sharpest_loaded(&mut self, key: &Key, window: u64, now: Instant) -> Option<Picture> {
        let edge = self
            .entries
            .iter()
            .filter(|((resource, edge), entry)| {
                resource == &key.0 && *edge > MAX_EDGE && *edge < key.1 && matches!(entry.state, State::Loaded(Ok(_)))
            })
            .map(|((_, edge), _)| *edge)
            .max()?;
        match self.lookup(&(key.0.clone(), edge), window, now) {
            Lookup::Ready { result: Ok(picture), .. } => Some(picture),
            _ => None,
        }
    }

    fn begin(&mut self, key: Key, task: Loading, attempts: u8, window: u64, now: Instant) -> u64 {
        self.generation += 1;
        #[cfg(feature = "dev")]
        {
            self.started += 1;
        }
        self.entries.insert(
            key,
            Entry {
                state: State::Loading(task),
                drawn: now,
                finished: now,
                attempts,
                generation: self.generation,
                window,
            },
        );
        self.generation
    }

    /// Records a finished load, unless the picture was looked up since (already recorded) or loaded again.
    fn finish(&mut self, key: &Key, generation: u64, result: Result<Picture, ImageCacheError>, now: Instant) -> bool {
        match self.entries.get_mut(key) {
            Some(entry) if entry.generation == generation && matches!(entry.state, State::Loading(_)) => {
                entry.state = State::Loaded(result);
                entry.finished = now;
                true
            }
            _ => false,
        }
    }

    /// Drops what's past the budget from memory and every window's atlas, and schedules the next sweep for when the
    /// pictures kept only for being drawn lately can go (none while within the budget).
    fn sweep(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let open: std::collections::HashSet<u64> =
            cx.windows().iter().map(|window| window.window_id().as_u64()).collect();
        self.latest_frame.retain(|window, _| open.contains(window));
        self.open_windows = Some(open);
        let images = self.take_evicted(now);
        if !images.is_empty() {
            for image in images {
                cx.drop_image(image, None);
            }
            release_freed_memory();
        }
        self.sweep = self.next_sweep(now).map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |cache, cx| cache.sweep(cx)).ok();
            })
        });
    }

    /// Loaded pictures: bytes, when last drawn, and whether their window's latest frame drew them.
    fn loaded(&self) -> Vec<(Key, usize, Instant, bool)> {
        self.entries
            .iter()
            .filter_map(|(key, entry)| match &entry.state {
                State::Loaded(Ok(picture)) => {
                    let window_open = self.open_windows.as_ref().is_none_or(|open| open.contains(&entry.window));
                    let in_latest_frame = window_open
                        && self
                            .latest_frame
                            .get(&entry.window)
                            .is_none_or(|latest| entry.drawn + FRAME_SLACK >= *latest);
                    // Kept for `KEEP` after it was drawn or finished loading, whichever is later.
                    Some((key.clone(), picture.bytes(), entry.drawn.max(entry.finished), in_latest_frame))
                }
                _ => None,
            })
            .collect()
    }

    /// Takes the least recently drawn pictures past the budget out of the cache.
    fn take_evicted(&mut self, now: Instant) -> Vec<Arc<RenderImage>> {
        self.entries.retain(|_, entry| {
            !matches!(entry.state, State::Loaded(Err(_))) || now.duration_since(entry.drawn) < FORGET_FAILURE
        });
        let mut images = Vec::new();
        for key in to_evict(self.loaded(), BUDGET, now, KEEP) {
            if let Some(Entry { state: State::Loaded(Ok(picture)), .. }) = self.entries.remove(&key) {
                images.extend(picture.tiles.images);
                #[cfg(feature = "dev")]
                {
                    self.dropped += 1;
                }
            }
        }
        images
    }

    /// While over the budget: how long until the least recently drawn picture may go.
    fn next_sweep(&self, now: Instant) -> Option<Duration> {
        let loaded = self.loaded();
        if loaded.iter().map(|(_, bytes, _, _)| bytes).sum::<usize>() <= BUDGET {
            return None;
        }
        // Pictures in a window's latest frame only become droppable once it draws again, which looks them up.
        let oldest =
            loaded.iter().filter(|(_, _, _, in_latest_frame)| !in_latest_frame).map(|(_, _, drawn, _)| *drawn).min()?;
        Some((oldest + KEEP).saturating_duration_since(now) + Duration::from_secs(1))
    }

    #[cfg(feature = "dev")]
    fn stats(&self) -> Stats {
        let mut stats = Stats { started: self.started, dropped: self.dropped, ..Stats::default() };
        for entry in self.entries.values() {
            match &entry.state {
                State::Loading(_) => stats.loading += 1,
                State::Loaded(Ok(picture)) => {
                    stats.loaded += 1;
                    stats.bytes += picture.bytes();
                }
                State::Loaded(Err(_)) => {}
            }
        }
        stats
    }
}

/// Fetches and decodes `key`'s picture off the main thread, at its size.
fn start_load((resource, edge): &Key, cx: &mut App) -> Task<Result<Picture, ImageCacheError>> {
    let fetch = fetch(resource, cx);
    let fallback = gpui::AssetLogger::<gpui::ImageAssetLoader>::load(resource.clone(), cx);
    let executor = cx.background_executor().clone();
    let edge = *edge;
    cx.background_executor().spawn(async move {
        load_picture(&FETCHING, fetch, || executor.timer(FETCH_TIMEOUT), edge, decode, move || fallback).await
    })
}

/// Downloads (in one of `slots`, until a deadline, without the decode turn), then decodes in turn. Bytes that
/// aren't a raster picture (SVG) go to `fallback`, GPUI's loader: it renders SVG sharper than a decode from bytes
/// can, and fetches it again, in the decode turn and within a deadline too.
async fn load_picture<Deadline, Fallback>(
    slots: &tokio::sync::Semaphore,
    fetch: impl Future<Output = Result<Vec<u8>, String>>,
    mut deadline: impl FnMut() -> Deadline,
    edge: u32,
    decode: impl FnOnce(&[u8], u32) -> Option<Result<Picture, ImageCacheError>>,
    fallback: impl FnOnce() -> Fallback,
) -> Result<Picture, ImageCacheError>
where
    Deadline: Future<Output = ()>,
    Fallback: Future<Output = Result<Arc<RenderImage>, ImageCacheError>>,
{
    // Held until decoded, so at most `FETCHES` encoded pictures wait at once.
    let _slot = slots.acquire().await.map_err(|_| failed("closed"))?;
    match fetch_then_decode(fetch, deadline(), |bytes| decode(&bytes, edge)).await {
        Fetched::Done(Some(result)) => result,
        Fetched::Done(None) => {
            let _turn = DECODING.lock().await;
            match bounded(fallback(), deadline()).await {
                Some(result) => result.map(|image| shrink(image, edge)),
                None => Err(failed("timed out")),
            }
        }
        Fetched::Failed(why) => Err(failed(&why)),
        Fetched::TimedOut => Err(failed("timed out")),
    }
}

fn failed(why: &str) -> ImageCacheError {
    ImageCacheError::Other(Arc::new(anyhow::anyhow!("loading picture: {why}")))
}

enum Fetched<T> {
    Done(T),
    Failed(String),
    TimedOut,
}

/// `future`, unless `deadline` comes first.
async fn bounded<T>(future: impl Future<Output = T>, deadline: impl Future<Output = ()>) -> Option<T> {
    match futures::future::select(std::pin::pin!(future), std::pin::pin!(deadline)).await {
        futures::future::Either::Left((value, _)) => Some(value),
        futures::future::Either::Right(_) => None,
    }
}

/// Fetches (until `deadline`) without holding the decode turn, so a stalled download doesn't hold up other pictures;
/// then decodes in turn.
async fn fetch_then_decode<T>(
    fetch: impl Future<Output = Result<Vec<u8>, String>>,
    deadline: impl Future<Output = ()>,
    decode: impl FnOnce(Vec<u8>) -> T,
) -> Fetched<T> {
    let bytes = match bounded(fetch, deadline).await {
        Some(Ok(bytes)) => bytes,
        Some(Err(why)) => return Fetched::Failed(why),
        None => return Fetched::TimedOut,
    };
    let _turn = DECODING.lock().await;
    Fetched::Done(decode(bytes))
}

/// The picture's bytes, as GPUI's loader gets them.
fn fetch(resource: &Resource, cx: &App) -> impl Future<Output = Result<Vec<u8>, String>> + Send + 'static {
    use futures::AsyncReadExt as _;
    let client = cx.http_client();
    let resource = resource.clone();
    async move {
        match resource {
            Resource::Uri(uri) => {
                let mut response =
                    client.get(uri.as_ref(), ().into(), true).await.map_err(|error| error.to_string())?;
                if !response.status().is_success() {
                    return Err(format!("{uri}: HTTP {}", response.status()));
                }
                let mut body = Vec::new();
                response.body_mut().read_to_end(&mut body).await.map_err(|error| error.to_string())?;
                Ok(body)
            }
            Resource::Path(path) => std::fs::read(path.as_ref()).map_err(|error| error.to_string()),
            Resource::Embedded(path) => Err(format!("{path} is embedded")),
        }
    }
}

/// A raster picture from its bytes, kept no larger than `edge` across; None when it isn't one (SVG).
fn decode(bytes: &[u8], edge: u32) -> Option<Result<Picture, ImageCacheError>> {
    if let Some((image, natural)) = imageio::decode(bytes, edge) {
        return Some(Ok(Picture::whole(image, natural)));
    }
    decode_raster(bytes).map(|result| result.map(|image| shrink(image, edge)))
}

/// Decodes like GPUI's own loader (gpui-0.2.2 `ImageAssetLoader`), one frame, BGRA. A GIF or animated WebP gives only
/// its first frame: an `img` without an id draws frame 0 anyway, and every frame of a long GIF kept at full size took
/// hundreds of MB that `shrink` couldn't scale down. None when `image` doesn't recognise the bytes, which GPUI's loader
/// then renders as SVG.
fn decode_raster(bytes: &[u8]) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
    use image::AnimationDecoder as _;
    use std::io::Cursor;
    let format = image::guess_format(bytes).ok()?;
    let to_bgra = |pixels: &mut [u8]| pixels.chunks_exact_mut(4).for_each(|pixel| pixel.swap(0, 2));
    let first = |frame: Option<image::ImageResult<image::Frame>>| -> image::ImageResult<image::Frame> {
        frame.unwrap_or_else(|| {
            Err(image::ImageError::Decoding(image::error::DecodingError::from_format_hint(format.into())))
        })
    };
    let decoded = (|| -> Result<Vec<image::Frame>, image::ImageError> {
        Ok(match format {
            image::ImageFormat::Gif => {
                let mut frame = first(image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?.into_frames().next())?;
                to_bgra(frame.buffer_mut());
                vec![frame]
            }
            image::ImageFormat::WebP => {
                let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?;
                if decoder.has_animation() {
                    let _ = decoder.set_background_color(image::Rgba([0, 0, 0, 0]));
                    let mut frame = first(decoder.into_frames().next())?;
                    to_bgra(frame.buffer_mut());
                    vec![frame]
                } else {
                    let mut data = image::DynamicImage::from_decoder(decoder)?.into_rgba8();
                    to_bgra(&mut data);
                    vec![image::Frame::new(data)]
                }
            }
            _ => {
                let mut data = image::load_from_memory_with_format(bytes, format)?.into_rgba8();
                to_bgra(&mut data);
                vec![image::Frame::new(data)]
            }
        })
    })();
    Some(decoded.map(|frames| Arc::new(RenderImage::new(frames))).map_err(ImageCacheError::from))
}

/// Which pictures to drop so the rest fit in `budget`: least recently drawn first, never one drawn within `keep` or
/// in its window's latest frame (the last flag).
fn to_evict<K>(mut pictures: Vec<(K, usize, Instant, bool)>, budget: usize, now: Instant, keep: Duration) -> Vec<K> {
    let mut total: usize = pictures.iter().map(|(_, bytes, _, _)| bytes).sum();
    pictures.sort_by_key(|(_, _, drawn, _)| *drawn);
    let mut dropped = Vec::new();
    for (key, bytes, drawn, in_latest_frame) in pictures {
        if total <= budget {
            break;
        }
        if in_latest_frame || now.duration_since(drawn) < keep {
            continue;
        }
        total -= bytes;
        dropped.push(key);
    }
    dropped
}

/// `image` no larger than `max_edge` pixels across, keeping its shape.
fn shrink(image: Arc<RenderImage>, max_edge: u32) -> Picture {
    let size = image.size(0);
    let natural = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
    let (width, height) = natural;
    let longest = width.max(height);
    if image.frame_count() != 1 || longest <= max_edge {
        return Picture::whole(image, natural);
    }
    let scale = max_edge as f64 / longest as f64;
    let (new_width, new_height) =
        (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1));
    // BGRA, but resizing treats the four channels alike. Read in place: a copy of a full-size picture is tens of MB.
    let resized = {
        let Some(pixels) = image
            .as_bytes(0)
            .and_then(|bytes| image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(width, height, bytes))
        else {
            return Picture::whole(image, natural);
        };
        image::imageops::resize(&pixels, new_width, new_height, image::imageops::FilterType::Triangle)
    };
    drop(image);
    release_freed_memory();
    Picture::whole(Arc::new(RenderImage::new(vec![image::Frame::new(resized)])), natural)
}

/// Asks the allocator to hand freed memory back to the system. macOS keeps freed large blocks (a full-size picture
/// decoded only to be scaled down, a dropped one) for reuse, and they count in the app's footprint until then.
fn release_freed_memory() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // A null zone means every zone; a goal of 0 releases all it can.
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn an_animation_keeps_its_first_frame_only() {
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            let frame =
                |shade: u8| image::Frame::new(image::RgbaImage::from_pixel(4, 3, image::Rgba([shade, 0, 0, 255])));
            encoder.encode_frames(vec![frame(10), frame(200)]).unwrap();
        }
        let image = decode_raster(&gif).unwrap().unwrap();
        assert_eq!(image.frame_count(), 1);
        assert_eq!((image.size(0).width.0, image.size(0).height.0), (4, 3));
    }

    const WINDOW: u64 = 1;

    fn picture(width: u32, height: u32) -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(width, height))]))
    }

    fn resource(name: &str) -> Key {
        (Resource::Uri(gpui::SharedUri::from(format!("https://example.com/{name}.jpg"))), MAX_EDGE)
    }

    /// A load task (never polled in these tests).
    fn pending() -> Loading {
        Task::ready(Err(failed("never polled"))).shared()
    }

    /// A 1600 × 1200 picture: 7.3 MB decoded, so 18 of them are past the 128 MB budget.
    fn big() -> Result<Picture, ImageCacheError> {
        Ok(Picture::whole(picture(1600, 1200), (4032, 3024)))
    }

    /// `count` pictures that finished loading at `at` in WINDOW.
    fn loaded_cache(count: usize, at: Instant) -> Pictures {
        let mut cache = Pictures::default();
        for index in 0..count {
            let generation = cache.begin(resource(&index.to_string()), pending(), 1, WINDOW, at);
            assert!(cache.finish(&resource(&index.to_string()), generation, big(), at));
        }
        cache
    }

    fn gif() -> Vec<u8> {
        let mut gif = Vec::new();
        image::DynamicImage::from(image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 10, 20, 255])))
            .write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif)
            .unwrap();
        gif
    }

    #[test]
    fn shrinks_big_pictures_and_keeps_their_shape() {
        let shrunk = shrink(picture(4032, 3024), 1600);
        assert_eq!(shrunk.natural, (4032, 3024));
        let size = shrunk.image().size(0);
        assert_eq!((size.width.0, size.height.0), (1600, 1200));
        assert_eq!(shrunk.bytes(), 1600 * 1200 * 4);

        let tall = shrink(picture(1000, 3000), 1600).image().size(0);
        assert_eq!((tall.width.0, tall.height.0), (533, 1600));
    }

    #[test]
    fn leaves_small_pictures_as_they_are() {
        let small = picture(800, 600);
        let kept = shrink(small.clone(), 1600);
        assert!(Arc::ptr_eq(&kept.image(), &small));
        assert_eq!(kept.natural, (800, 600));
    }

    #[test]
    fn a_zoomed_picture_gets_a_copy_as_sharp_as_it_is_shown() {
        // Before: the viewer drew a 720 × 5000 screenshot from its 230 × 1600 copy at any zoom.
        let tall = (720., 5000.);
        assert_eq!(sharp_edge(tall, 1400.), MAX_EDGE, "fitted to the window: the usual copy");
        assert_eq!(sharp_edge(tall, 1700.), 3200);
        assert_eq!(sharp_edge(tall, 12750.), 5000, "never more than its own size");
        assert_eq!(sharp_edge((800., 600.), 5000.), MAX_EDGE, "a small picture is whole already");
        assert_eq!(sharp_edge((1440., 30000.), 60000.), 30000, "whole, in tiles");
        let huge = sharp_edge((12000., 12000.), 30000.);
        assert!((huge as f64).powi(2) <= SHARP_MAX_PIXELS && huge > 5000, "within the pixel budget: {huge}");
    }

    #[test]
    fn a_picture_longer_than_the_largest_texture_is_kept_in_tiles() {
        let tall = Picture::whole(picture(4, 20000), (4, 20000));
        assert_eq!((tall.tiles.images.len(), tall.tiles.columns, tall.tiles.size), (5, 1, (4, 20000)));
        assert_eq!(tall.tiles.bounds(4), ((0, 16384), (4, 3616)));
        assert_eq!(tall.bytes(), 4 * 20000 * 4);
        assert_eq!(Picture::whole(picture(4, 16384), (4, 16384)).tiles.images.len(), 1);
    }

    #[test]
    fn until_a_sharper_copy_loads_the_sharpest_loaded_one_shows() {
        let now = Instant::now();
        let mut cache = Pictures::default();
        let (page, _) = resource("page");
        for (edge, done) in [(MAX_EDGE, true), (3200, true), (6400, false)] {
            let generation = cache.begin((page.clone(), edge), pending(), 1, WINDOW, now);
            if done {
                cache.finish(
                    &(page.clone(), edge),
                    generation,
                    Ok(Picture::whole(picture(edge / 7, edge), (720, 5000))),
                    now,
                );
            }
        }
        let shown = cache.sharpest_loaded(&(page.clone(), 12800), WINDOW, now).expect("the 3200 copy");
        assert_eq!(shown.image().size(0).height.0, 3200);
        assert!(cache.sharpest_loaded(&(page.clone(), 3200), WINDOW, now).is_none(), "the usual copy is the caller's");
        assert!(cache.sharpest_loaded(&(resource("other").0, 12800), WINDOW, now).is_none());
    }

    #[test]
    fn evicts_least_recently_drawn_until_under_budget() {
        let now = Instant::now();
        let ago = |secs| now - Duration::from_secs(secs);
        let pictures = vec![
            ("a", 40, ago(300), false),
            ("b", 40, ago(100), false),
            ("c", 40, ago(200), false),
            ("d", 40, ago(60), false),
        ];
        // 160 bytes, budget 90: drop the oldest two (a, then c).
        assert_eq!(to_evict(pictures, 90, now, KEEP), vec!["a", "c"]);
    }

    #[test]
    fn never_evicts_pictures_drawn_lately_or_in_the_latest_frame() {
        let now = Instant::now();
        let pictures = vec![
            ("old", 50, now - Duration::from_secs(120), false),
            ("on screen", 500, now - Duration::from_secs(1), false),
        ];
        // Over budget even after dropping "old", but what's on screen stays.
        assert_eq!(to_evict(pictures, 100, now, KEEP), vec!["old"]);
        assert!(to_evict(vec![("a", 10, now, false)], 100, now, KEEP).is_empty());
        // Drawn an hour ago but still in its window's latest frame: kept.
        let pictures = vec![
            ("idle screen", 500, now - Duration::from_secs(3600), true),
            ("gone", 50, now - Duration::from_secs(120), false),
        ];
        assert_eq!(to_evict(pictures, 100, now, KEEP), vec!["gone"]);
    }

    #[test]
    fn an_idle_windows_pictures_stay_until_it_draws_without_them() {
        // Before: a timed sweep could drop pictures an idle window still showed; GPUI presents that frame again and
        // Metal's atlas panics on the missing texture.
        let start = Instant::now();
        let mut cache = loaded_cache(20, start);
        for index in 0..20 {
            cache.lookup(&resource(&index.to_string()), WINDOW, start);
        }
        let hour = start + Duration::from_secs(3600);
        assert!(cache.take_evicted(hour).is_empty(), "the latest frame still shows them");
        assert!(cache.next_sweep(hour).is_none(), "no sweep to schedule until the window draws again");
        // The window draws another page: what it no longer shows can go.
        cache.lookup(&resource("other page"), WINDOW, hour);
        assert_eq!(cache.take_evicted(hour).len(), 3);
        assert!(cache.stats().bytes <= BUDGET);
    }

    #[test]
    fn a_load_that_finishes_off_screen_is_counted_and_can_be_evicted() {
        // Before: a finished load stayed "loading" until its page drew it again, holding the picture outside the
        // budget and the stats.
        let start = Instant::now();
        let mut cache = loaded_cache(1, start);
        let stats = cache.stats();
        assert_eq!((stats.loading, stats.loaded, stats.bytes), (0, 1, 1600 * 1200 * 4));
        for index in 1..21 {
            let generation = cache.begin(resource(&index.to_string()), pending(), 1, WINDOW, start);
            cache.finish(&resource(&index.to_string()), generation, big(), start);
        }
        // A minute later the window draws something else: the oldest go until within the budget.
        let later = start + Duration::from_secs(60);
        cache.lookup(&resource("elsewhere"), WINDOW, later);
        assert_eq!(cache.take_evicted(later).len(), 4);
        assert!(cache.stats().bytes <= BUDGET);
        assert!(cache.next_sweep(later).is_none());
    }

    #[test]
    fn a_new_frame_asks_for_a_sweep_once_a_second_at_most() {
        // Before: only a finished load swept, so a cache over budget stayed so while frames redrew loaded pictures.
        let start = Instant::now();
        let mut cache = loaded_cache(20, start);
        cache.new_frame = false;
        cache.lookup(&resource("0"), WINDOW, start + Duration::from_secs(40));
        assert!(std::mem::take(&mut cache.new_frame), "a new frame");
        cache.lookup(&resource("1"), WINDOW, start + Duration::from_millis(40_100));
        assert!(!cache.new_frame, "same frame");
        // That frame only drew "0" and "1": the rest, finished 40 s ago and not in it, can go.
        assert_eq!(cache.take_evicted(start + Duration::from_millis(40_200)).len(), 3);
    }

    #[test]
    fn a_finished_load_is_not_counted_as_drawn() {
        // Before: finishing set "drawn", pinning pictures their window never showed loaded.
        let start = Instant::now();
        let mut cache = Pictures::default();
        cache.lookup(&resource("first"), WINDOW, start);
        let generation = cache.begin(resource("first"), pending(), 1, WINDOW, start);
        cache.lookup(&resource("elsewhere"), WINDOW, start + Duration::from_secs(5));
        let finished = start + Duration::from_secs(10);
        cache.finish(&resource("first"), generation, big(), finished);
        let (_, _, kept_from, in_latest_frame) = cache.loaded().pop().unwrap();
        assert!(!in_latest_frame);
        assert_eq!(kept_from, finished, "KEEP still counts from when it finished");
    }

    #[test]
    fn a_closed_window_pins_nothing() {
        let start = Instant::now();
        let mut cache = loaded_cache(20, start);
        for index in 0..20 {
            cache.lookup(&resource(&index.to_string()), WINDOW, start);
        }
        let later = start + Duration::from_secs(60);
        assert!(cache.take_evicted(later).is_empty(), "the window still shows them");
        cache.open_windows = Some(std::collections::HashSet::from([WINDOW + 1]));
        assert_eq!(cache.take_evicted(later).len(), 3);
    }

    #[test]
    fn an_old_failure_is_forgotten() {
        let start = Instant::now();
        let mut cache = Pictures::default();
        let generation = cache.begin(resource("gone"), pending(), ATTEMPTS, WINDOW, start);
        cache.finish(&resource("gone"), generation, Err(failed("HTTP 404")), start);
        cache.take_evicted(start + Duration::from_secs(60));
        assert_eq!(cache.entries.len(), 1, "kept while recent");
        cache.take_evicted(start + FORGET_FAILURE);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn a_stale_load_is_ignored() {
        let mut cache = Pictures::default();
        let now = Instant::now();
        let first = cache.begin(resource("a"), pending(), 1, WINDOW, now);
        let second = cache.begin(resource("a"), pending(), 2, WINDOW, now);
        assert!(!cache.finish(&resource("a"), first, big(), now));
        assert!(cache.finish(&resource("a"), second, big(), now));
        assert!(!cache.finish(&resource("a"), second, big(), now), "recorded once");
    }

    #[test]
    fn a_load_promoted_by_a_lookup_is_not_recorded_twice() {
        let mut cache = Pictures::default();
        let now = Instant::now();
        let generation = cache.begin(resource("a"), Task::ready(big()).shared(), 1, WINDOW, now);
        assert!(matches!(
            cache.lookup(&resource("a"), WINDOW, now),
            Lookup::Ready { result: Ok(_), just_loaded: true }
        ));
        assert!(!cache.finish(&resource("a"), generation, Err(failed("late")), now));
        assert!(matches!(
            cache.lookup(&resource("a"), WINDOW, now),
            Lookup::Ready { result: Ok(_), just_loaded: false }
        ));
    }

    #[test]
    fn sweeps_again_when_pictures_kept_for_being_drawn_lately_can_go() {
        // Before: eviction ran only when another picture finished, so a burst stayed over budget after KEEP.
        let start = Instant::now();
        let mut cache = loaded_cache(20, start);
        let soon = start + Duration::from_secs(5);
        cache.lookup(&resource("elsewhere"), WINDOW, soon);
        assert!(cache.take_evicted(soon).is_empty(), "all drawn lately");
        let wait = cache.next_sweep(soon).expect("over budget");
        assert_eq!(wait, Duration::from_secs(26));
        assert!(!cache.take_evicted(soon + wait).is_empty());
        assert!(cache.next_sweep(soon + wait).is_none());
    }

    #[test]
    fn a_failed_picture_is_tried_again_a_bounded_number_of_times() {
        // Before: a failure was kept for good.
        let mut cache = Pictures::default();
        let mut now = Instant::now();
        let generation = cache.begin(resource("flaky"), pending(), 1, WINDOW, now);
        cache.finish(&resource("flaky"), generation, Err(failed("HTTP 503")), now);
        assert!(matches!(
            cache.lookup(&resource("flaky"), WINDOW, now + Duration::from_secs(5)),
            Lookup::Ready { result: Err(_), .. }
        ));
        for attempt in 2..=ATTEMPTS {
            now += RETRY_AFTER;
            let Lookup::Start { attempts } = cache.lookup(&resource("flaky"), WINDOW, now) else {
                panic!("should retry")
            };
            assert_eq!(attempts, attempt);
            let generation = cache.begin(resource("flaky"), pending(), attempts, WINDOW, now);
            cache.finish(&resource("flaky"), generation, Err(failed("HTTP 503")), now);
        }
        // Out of attempts: the error stays.
        assert!(matches!(
            cache.lookup(&resource("flaky"), WINDOW, now + RETRY_AFTER * 10),
            Lookup::Ready { result: Err(_), .. }
        ));
    }

    #[test]
    fn a_stalled_download_times_out_without_holding_up_other_pictures() {
        // Before: the decode turn was taken before the download, which had no deadline.
        let mut stalled = Box::pin(fetch_then_decode(futures::future::pending(), futures::future::pending(), |_| ()));
        assert!((&mut stalled).now_or_never().is_none(), "still downloading");
        let other = fetch_then_decode(async { Ok(vec![1, 2, 3]) }, futures::future::pending(), |bytes| bytes.len());
        assert!(matches!(other.now_or_never(), Some(Fetched::Done(3))), "decoded while the other still downloads");
        let timed_out = fetch_then_decode(futures::future::pending(), async {}, |_| ());
        assert!(matches!(timed_out.now_or_never(), Some(Fetched::TimedOut)));
        let failed = fetch_then_decode(async { Err("HTTP 404".to_string()) }, futures::future::pending(), |_| ());
        assert!(matches!(failed.now_or_never(), Some(Fetched::Failed(why)) if why == "HTTP 404"));
    }

    #[test]
    fn the_load_pipeline_fetches_once_and_falls_back_only_for_svg() {
        // Before: anything ImageIO left alone (WebP, GIF, damaged files) was downloaded again by GPUI's loader.
        let slots = tokio::sync::Semaphore::new(FETCHES);
        let (fetch_count, fallback_count) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let (fetches, fallbacks) = (&fetch_count, &fallback_count);
        let run = |bytes: Vec<u8>| {
            load_picture(
                &slots,
                async move {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    Ok(bytes)
                },
                futures::future::pending,
                MAX_EDGE,
                decode,
                || {
                    fallbacks.fetch_add(1, Ordering::SeqCst);
                    async { Ok(picture(10, 10)) }
                },
            )
            .now_or_never()
            .expect("finished")
        };
        let gif = run(gif()).expect("decoded");
        assert_eq!(&gif.image().as_bytes(0).unwrap()[..3], &[20, 10, 200], "BGRA like GPUI's loader");
        assert!(run(vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]).is_err(), "a damaged JPEG fails from the same bytes");
        assert_eq!((fetches.load(Ordering::SeqCst), fallbacks.load(Ordering::SeqCst)), (2, 0));
        let svg = run(b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec()).expect("GPUI's loader");
        assert_eq!(svg.natural, (10, 10));
        assert_eq!((fetches.load(Ordering::SeqCst), fallbacks.load(Ordering::SeqCst)), (3, 1));
        // A fallback past its deadline fails rather than waiting for good.
        let stuck = load_picture(
            &slots,
            async { Ok(b"<svg/>".to_vec()) },
            || async {},
            MAX_EDGE,
            decode,
            futures::future::pending,
        );
        assert!(stuck.now_or_never().expect("finished").is_err());
    }

    #[test]
    fn downloads_wait_for_a_free_slot() {
        // Before: every load fetched at once, piling up encoded bytes behind the decode turn.
        let slots = tokio::sync::Semaphore::new(1);
        let mut stalled = Box::pin(load_picture(
            &slots,
            futures::future::pending(),
            futures::future::pending,
            MAX_EDGE,
            decode,
            futures::future::pending,
        ));
        assert!((&mut stalled).now_or_never().is_none(), "holds the only slot");
        let mut next =
            Box::pin(load_picture(&slots, async { Ok(gif()) }, futures::future::pending, MAX_EDGE, decode, || {
                futures::future::pending()
            }));
        assert!((&mut next).now_or_never().is_none(), "waits for the slot");
        drop(stalled);
        assert!(next.now_or_never().expect("got the slot").is_ok());
    }
}

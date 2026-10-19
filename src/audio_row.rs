// An audio file where it was shared (a chat's or the Group's message, a coding result's page), played in place: a round
// play button, the sound's waveform (the part played in the accent colour; a press or a drag along it goes there), the
// time played and its length, and a speed button (1×, 1.5×, 2×, for every sound), with the file's name under them. It
// plays from its file on this Mac through media.rs (one sound at a time, AVFoundation's player), so a file attached
// without a copy plays from where it is. A click anywhere on the row (or Tab) gives it the keyboard, and Space plays or
// pauses it then (a ring shows it has the keyboard only after Tab, not after a click); a letter typed there goes back
// to the window's message box while one is drawn, with the letter, so typing never goes nowhere (`Keyboard`).
//
// The waveform is made once per file (`wave`), on two threads of their own (`maker`): macOS's `afconvert` turns the
// sound into 4,000 8-bit samples a second, mono, in a temporary file of its own, and the loudest of each stretch is a
// bar (read in pieces, so a long sound takes little memory). A conversion that runs past its time (`convert_timeout`,
// by the file's size: a file on a stalled network volume) is stopped, and one whose row is no longer drawn by its turn
// is skipped. The waveform is kept in a temporary folder by the file's path, size and time (media.rs `file_key`, as
// video_player.rs keeps a video's poster), and in memory by the same key, which is looked up off the main thread and
// again every `RECHECK` (a stalled drive never holds up drawing), so a file that changes is made again. Until then, or
// for a file it can't read (not tried again until it changes), the bars are all one height.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, DragMoveEvent, FocusHandle, Global, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakFocusHandle, Window, canvas, div, px,
};

use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// Stretches of a sound kept per file; the bars a row draws are taken from these.
const PEAKS: usize = 240;
/// Samples a second the waveform is made from.
const RATE: usize = 4000;
/// How often a sound's file is looked at again (its size and time, for its `file_key`).
const RECHECK: Duration = Duration::from_secs(10);
/// A waveform asked for longer ago than this is no longer drawn: its conversion is skipped.
const UNSEEN: Duration = Duration::from_secs(10);
/// A conversion that ran out of time (a slow network drive) is tried once more after this.
const RETRY_TIMED_OUT: Duration = Duration::from_secs(5 * 60);

/// A sound's waveform: how loud each of its `PEAKS` stretches is, from 0 to 1, and how long it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    pub peaks: Vec<f32>,
    pub seconds: f64,
}

enum Made {
    Making,
    Done(Wave),
    /// Tried again at this time (one that ran out of time, once), else not until the file changes (its key with it).
    Failed(Option<Instant>),
}

/// Why a waveform couldn't be made.
#[derive(Debug, PartialEq)]
enum Failure {
    Unreadable,
    TimedOut,
}

/// What came of a waveform's turn.
enum Outcome {
    Made(Wave),
    Failed(Failure),
    /// Its row wasn't drawn any more: asked again when it is.
    Skipped,
}

/// A sound file's key and size (`media::file_facts`), looked up off the main thread.
#[derive(Default)]
struct Known {
    facts: Option<(String, u64)>,
    checked: Option<Instant>,
    checking: bool,
}

/// Sound files' keys, and waveforms by key.
#[derive(Default)]
struct Waves {
    keys: HashMap<PathBuf, Known>,
    made: HashMap<String, Made>,
    /// Keys whose conversion ran out of time once already.
    timed_out: std::collections::HashSet<String>,
}

impl Global for Waves {}

/// When each waveform was last asked for (drawn), read by the threads making them.
fn asked() -> &'static std::sync::Mutex<HashMap<String, Instant>> {
    static ASKED: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();
    ASKED.get_or_init(Default::default)
}

/// `file`'s key and size as last looked up; the first ask, and one every `RECHECK`, looks again on a thread of its own
/// (a file on a stalled drive holds up only that), and the windows draw again when it changed.
fn facts_of(file: &Path, cx: &mut App) -> Option<(String, u64)> {
    let known = cx.default_global::<Waves>().keys.entry(file.to_path_buf()).or_default();
    let facts = known.facts.clone();
    if !known.checking && known.checked.is_none_or(|checked| checked.elapsed() >= RECHECK) {
        known.checking = true;
        let file = file.to_path_buf();
        let (reply, looked) = futures::channel::oneshot::channel();
        let looking = {
            let file = file.clone();
            std::thread::Builder::new()
                .name("sound file".into())
                .spawn(move || _ = reply.send(crate::media::file_facts(&file)))
        };
        if looking.is_err() {
            known.checking = false;
            return facts;
        }
        cx.spawn(async move |cx| {
            let facts = looked.await.ok().flatten();
            cx.update(|cx| {
                let known = cx.default_global::<Waves>().keys.entry(file).or_default();
                let changed = known.facts != facts;
                (known.facts, known.checked, known.checking) = (facts, Some(Instant::now()), false);
                if changed {
                    cx.refresh_windows();
                }
            })
            .ok();
        })
        .detach();
    }
    facts
}

/// `file`'s waveform once it's made; the first ask starts making it, and the windows draw again when it's there.
pub fn wave(file: &Path, cx: &mut App) -> Option<Wave> {
    let (key, size) = facts_of(file, cx)?;
    asked().lock().unwrap().insert(key.clone(), Instant::now());
    let waves = cx.default_global::<Waves>();
    match waves.made.get(&key) {
        Some(Made::Done(wave)) => return Some(wave.clone()),
        Some(Made::Failed(Some(retry))) if *retry <= Instant::now() => {}
        Some(Made::Making | Made::Failed(_)) => return None,
        None => {}
    }
    waves.made.insert(key.clone(), Made::Making);
    let (reply, outcome) = futures::channel::oneshot::channel();
    if maker().send((file.to_path_buf(), key.clone(), size, reply)).is_err() {
        waves.made.insert(key, Made::Failed(None));
        return None;
    }
    cx.spawn(async move |cx| {
        let outcome = outcome.await.unwrap_or(Outcome::Failed(Failure::Unreadable));
        cx.update(|cx| {
            let waves = cx.default_global::<Waves>();
            match outcome {
                Outcome::Made(wave) => {
                    waves.made.insert(key, Made::Done(wave));
                }
                Outcome::Failed(Failure::TimedOut) if waves.timed_out.insert(key.clone()) => {
                    waves.made.insert(key, Made::Failed(Some(Instant::now() + RETRY_TIMED_OUT)));
                    return;
                }
                Outcome::Failed(_) => {
                    waves.made.insert(key, Made::Failed(None));
                    return;
                }
                // Drawn again, a row that's still there asks again (one drawn since it was asked).
                Outcome::Skipped => {
                    waves.made.remove(&key);
                }
            }
            cx.refresh_windows();
        })
        .ok();
    })
    .detach();
    None
}

type Job = (PathBuf, String, u64, futures::channel::oneshot::Sender<Outcome>);

/// The two threads that make waveforms, in the order they're asked for, skipping those no longer drawn.
fn maker() -> &'static mpsc::Sender<Job> {
    static MAKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    MAKER.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        let queue = std::sync::Arc::new(std::sync::Mutex::new(queue));
        for _ in 0..2 {
            let queue = queue.clone();
            std::thread::Builder::new()
                .name("waveforms".into())
                .spawn(move || {
                    loop {
                        let Ok((file, key, size, reply)) = queue.lock().unwrap().recv() else { return };
                        let seen = asked().lock().unwrap().get(&key).is_some_and(|at| at.elapsed() < UNSEEN);
                        let outcome = match seen.then(|| make_wave(&file, &key, size)) {
                            None => Outcome::Skipped,
                            Some(Ok(wave)) => Outcome::Made(wave),
                            Some(Err(failure)) => Outcome::Failed(failure),
                        };
                        let _ = reply.send(outcome);
                    }
                })
                .expect("waveform thread");
        }
        jobs
    })
}

/// The waveform of the sound at `file` (`size` bytes long), kept under `key` (its `file_key`): read back, or made and
/// kept.
fn make_wave(file: &Path, key: &str, size: u64) -> Result<Wave, Failure> {
    let folder = crate::media::kept_folder("waves");
    let kept = folder.join(format!("{key}.json"));
    if let Some(wave) = std::fs::read(&kept).ok().and_then(|bytes| {
        let kept: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let peaks: Option<Vec<f32>> =
            kept["peaks"].as_array()?.iter().map(|peak| peak.as_f64().map(|peak| peak as f32)).collect();
        Some(Wave { peaks: peaks?, seconds: kept["seconds"].as_f64()? })
    }) {
        return Ok(wave);
    }
    std::fs::create_dir_all(&folder).map_err(|_| Failure::Unreadable)?;
    // Its own name: another Filo on this Mac may be making the same file's waveform.
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let samples = folder.join(format!("{key}-{}-{made}.wav", std::process::id()));
    let converted = convert(file, &samples, convert_timeout(size));
    let wave =
        converted.and_then(|_| std::fs::File::open(&samples).ok().and_then(wave_from).ok_or(Failure::Unreadable));
    let _ = std::fs::remove_file(&samples);
    let wave = wave?;
    // Written aside and moved in, so a half-written file is never read back.
    let part = folder.join(format!("{key}-{}-{made}.json.part", std::process::id()));
    let written = serde_json::json!({ "peaks": wave.peaks, "seconds": wave.seconds });
    if std::fs::write(&part, written.to_string()).and_then(|_| std::fs::rename(&part, &kept)).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    Ok(wave)
}

/// The longest a waveform's conversion of a file `size` bytes long may take: half a minute, and a second more for each
/// 2 MB, at most three minutes (an hour of mp3, 57 MB, takes about five seconds).
fn convert_timeout(size: u64) -> Duration {
    Duration::from_secs((30 + size / (2 << 20)).min(180))
}

/// `afconvert` from `file` to 8-bit mono samples at `RATE` in the WAVE file `samples`: why not, when it failed or ran
/// past `timeout` (it's stopped then).
fn convert(file: &Path, samples: &Path, timeout: Duration) -> Result<(), Failure> {
    let Ok(mut child) = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", &format!("UI8@{RATE}"), "-c", "1"])
        .arg(file)
        .arg(samples)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Err(Failure::Unreadable);
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(Failure::Unreadable),
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            waited => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if waited.is_ok() { Failure::TimedOut } else { Failure::Unreadable });
            }
        }
    }
}

/// The waveform of a WAVE file of unsigned 8-bit samples at `RATE`: `PEAKS` stretches of its `data` chunk, each its
/// loudest sample, the loudest stretch 1. Read in pieces.
fn wave_from(mut wave: impl Read + Seek) -> Option<Wave> {
    let mut header = [0; 12];
    wave.read_exact(&mut header).ok()?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return None;
    }
    let size = loop {
        let mut chunk = [0; 8];
        wave.read_exact(&mut chunk).ok()?;
        let size = u32::from_le_bytes(chunk[4..8].try_into().ok()?) as usize;
        if &chunk[0..4] == b"data" {
            break size;
        }
        wave.seek(SeekFrom::Current((size + size % 2) as i64)).ok()?;
    };
    let mut peaks = vec![0f32; PEAKS];
    let mut read = 0;
    let mut piece = vec![0; 64 << 10];
    let mut data = wave.take(size as u64);
    loop {
        let count = data.read(&mut piece).ok()?;
        if count == 0 {
            break;
        }
        for sample in &piece[..count] {
            let peak = &mut peaks[(read * PEAKS / size.max(1)).min(PEAKS - 1)];
            *peak = peak.max((f32::from(*sample) - 128.).abs() / 128.);
            read += 1;
        }
    }
    if read == 0 {
        return None;
    }
    let loudest = peaks.iter().copied().fold(0., f32::max);
    if loudest > 0. {
        peaks.iter_mut().for_each(|peak| *peak /= loudest);
    }
    Some(Wave { peaks, seconds: read as f64 / RATE as f64 })
}

/// `count` bars from `peaks`: each the loudest of its share.
fn bars(peaks: &[f32], count: usize) -> Vec<f32> {
    (0..count)
        .map(|index| {
            let (start, end) = (peaks.len() * index / count, (peaks.len() * (index + 1) / count).max(1));
            peaks[start.min(peaks.len().saturating_sub(1))..end.min(peaks.len())].iter().copied().fold(0., f32::max)
        })
        .collect()
}

/// A drag along a row's waveform, by its sound's id (gpui's drags carry a value and draw a view; this one draws
/// nothing).
#[derive(Clone)]
struct Scrub(SharedString);

impl gpui::Render for Scrub {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// In each window, its message box (`message_box`) and the keys on their way back there (`on_row_key`).
#[derive(Default)]
struct Keyboard {
    windows: HashMap<gpui::WindowId, HandBack>,
    /// `intercept` is listening.
    listening: bool,
}

impl Global for Keyboard {}

#[derive(Default)]
struct HandBack {
    /// The message box last drawn in the window; letters go back to it only while it's drawn.
    message_box: Option<WeakFocusHandle>,
    /// Keys typed since the first one on a row, and when that was.
    typed: Vec<gpui::Keystroke>,
    since: Option<Instant>,
}

/// Keys gathered this long ago and not typed back (the window closed, or drew no frame) are dropped.
const HAND_BACK_WITHIN: Duration = Duration::from_secs(1);

impl HandBack {
    /// Keys are on their way back; stale ones are dropped first.
    fn going(&mut self) -> bool {
        if self.since.is_some_and(|since| since.elapsed() > HAND_BACK_WITHIN) {
            self.typed.clear();
            self.since = None;
        }
        !self.typed.is_empty()
    }

    /// The message box, while it's drawn in `window` (a box on a page no longer shown keeps its handle).
    fn target(&self, window: &Window) -> Option<FocusHandle> {
        self.message_box.as_ref()?.upgrade().filter(|message_box| message_box.is_drawn(window))
    }
}

/// A shortcut, which goes on as usual: ⌘, ⌃ or fn held.
fn is_shortcut(modifiers: gpui::Modifiers) -> bool {
    modifiers.platform || modifiers.control || modifiers.function
}

/// A key a row hands back: one that types something, not a shortcut.
fn typed(keystroke: &gpui::Keystroke) -> bool {
    !is_shortcut(keystroke.modifiers)
        && keystroke.key_char.as_ref().is_some_and(|typed| !typed.is_empty() && !typed.chars().any(char::is_control))
}

/// A message box is drawn (conversation.rs, each time it draws): letters typed on a sound's row in this window go back
/// to it.
pub fn message_box(focus: &FocusHandle, window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    cx.default_global::<Keyboard>().windows.entry(id).or_default().message_box = Some(focus.downgrade());
}

/// A row's focus, made the first time it's drawn (the first one also starts `intercept`).
fn row_focus(sound: &SharedString, window: &mut Window, cx: &mut App) -> FocusHandle {
    let focus = window.use_keyed_state(SharedString::from(format!("{sound}-focus")), cx, |_, cx| {
        if !std::mem::replace(&mut cx.default_global::<Keyboard>().listening, true) {
            cx.intercept_keystrokes(intercept).detach();
        }
        cx.focus_handle().tab_stop(true)
    });
    focus.read(cx).clone()
}

/// Every key, before anything else has it: while keys are on their way back in this window, the next ones join them
/// (whatever has the keyboard meanwhile), so they're typed back in the order they came. A shortcut goes on as usual.
fn intercept(event: &gpui::KeystrokeEvent, window: &mut Window, cx: &mut App) {
    if is_shortcut(event.keystroke.modifiers) {
        return;
    }
    let id = window.window_handle().window_id();
    let Some(hand) = cx.default_global::<Keyboard>().windows.get_mut(&id) else { return };
    if !hand.going() {
        return;
    }
    hand.typed.push(event.keystroke.clone());
    cx.stop_propagation();
}

/// A key on a focused row: Space plays or pauses it; a letter goes back to the window's message box when one is drawn
/// (with none, the row keeps the keyboard). The row keeps it until the next frame (`intercept` gathers what's typed
/// meanwhile, a space too); then the keyboard goes back, and once the window has drawn with it there (its text input is
/// set up as it draws), the keys are typed there in order.
fn on_row_key(event: &KeyDownEvent, toggle: &dyn Fn(&mut App), window: &mut Window, cx: &mut App) {
    let keystroke = &event.keystroke;
    if keystroke.key == "space" && !keystroke.modifiers.modified() {
        cx.stop_propagation();
        toggle(cx);
        return;
    }
    let id = window.window_handle().window_id();
    let hand = cx.default_global::<Keyboard>().windows.entry(id).or_default();
    if !typed(keystroke) || hand.target(window).is_none() {
        return;
    }
    hand.typed.push(keystroke.clone());
    hand.since = Some(Instant::now());
    cx.stop_propagation();
    window.on_next_frame(move |window, cx| {
        let hand = cx.default_global::<Keyboard>().windows.entry(id).or_default();
        let Some(back) = hand.target(window).filter(|_| hand.going()) else {
            hand.typed.clear();
            hand.since = None;
            return;
        };
        window.focus(&back);
        window.on_next_frame(move |window, cx| {
            let hand = cx.default_global::<Keyboard>().windows.entry(id).or_default();
            let keys = std::mem::take(&mut hand.typed);
            hand.since = None;
            for keystroke in keys {
                window.dispatch_keystroke(keystroke, cx);
            }
        })
    });
}

/// Where a row's waveform was drawn, and where a press or a drag along it last went (until it's let go).
#[derive(Default)]
struct Track {
    bounds: Cell<Option<Bounds<Pixels>>>,
    scrubbing: Cell<Option<f64>>,
    /// The row has the keyboard from a click: it takes it for Space, but shows no ring, which is for Tab.
    clicked: Cell<bool>,
}

const BUTTON: f32 = 36.;
const BAR: f32 = 3.;
const BAR_GAP: f32 = 2.;
const WAVE_HEIGHT: f32 = 26.;
/// The time and the speed button beside the waveform.
const TIME_WIDTH: f32 = 76.;
const SPEED_WIDTH: f32 = 34.;

/// The row for the sound at `file` (None while it isn't on this Mac: it shows, and doesn't play) named `name`, `width`
/// wide, its words and unplayed bars in `fg`. `id` tells it apart from every other row of the same sound.
pub fn row(
    id: &str,
    file: Option<&Path>,
    name: &str,
    width: f32,
    fg: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> gpui::AnyElement {
    let colors = *cx.colors();
    let sound: SharedString = format!("audio-{id}").into();
    let url = file.map(|file| file.display().to_string());
    let wave = file.and_then(|file| wave(file, cx));
    crate::media::shown_in(&sound, window, cx);
    let state = crate::media::state(&sound, cx);
    let playing = state.is_some_and(|state| state.playing);
    // The samples' length over the player's: AVFoundation's can run past the sound's end (a FLAC's).
    let length = wave.as_ref().map(|wave| wave.seconds);
    let duration = length.or(state.and_then(|state| state.duration));
    let track =
        window.use_keyed_state(SharedString::from(format!("{sound}-track")), cx, |_, _| Rc::new(Track::default()));
    let track = track.read(cx).clone();
    let time = track.scrubbing.get().or(state.map(|state| state.time)).unwrap_or(0.);
    let fraction = duration.map_or(0., |duration| (time / duration.max(0.001)).clamp(0., 1.)) as f32;
    let focus = row_focus(&sound, window, cx);
    let focused = focus.is_focused(window);
    if !focused {
        track.clicked.set(false);
    }
    let ring = focused && !track.clicked.get();

    let wave_width = (width - BUTTON - TIME_WIDTH - SPEED_WIDTH - space::SM * 3.).max(60.);
    let count = ((wave_width + BAR_GAP) / (BAR + BAR_GAP)).floor().max(8.) as usize;
    let heights = wave.as_ref().map_or_else(|| vec![0.3; count], |wave| bars(&wave.peaks, count));
    let played = (fraction * count as f32).round() as usize;
    let toggle = {
        let (sound, url) = (sound.clone(), url.clone());
        move |cx: &mut App| {
            if let Some(url) = &url {
                crate::media::toggle(sound.clone(), url.clone(), length, cx);
            }
        }
    };
    // Seconds into the sound at `x` along the waveform, once its length is known.
    let at = {
        let track = track.clone();
        move |x: Pixels| -> Option<f64> {
            let bounds = track.bounds.get()?;
            let fraction = f32::from(x - bounds.origin.x) / f32::from(bounds.size.width).max(1.);
            Some(f64::from(fraction.clamp(0., 1.)) * duration?)
        }
    };
    let seek_to = {
        let (sound, url, track) = (sound.clone(), url.clone(), track.clone());
        move |seconds: f64, exact: bool, cx: &mut App| {
            let Some(url) = &url else { return };
            track.scrubbing.set((!exact).then_some(seconds));
            crate::media::seek(sound.clone(), url, length, seconds, exact, cx);
        }
    };
    let let_go = {
        let (seek_to, track) = (seek_to.clone(), track.clone());
        move |cx: &mut App| {
            if let Some(seconds) = track.scrubbing.take() {
                seek_to(seconds, true, cx);
            }
        }
    };

    let button = div()
        .id(SharedString::from(format!("{sound}-play")))
        .size(px(BUTTON))
        .flex_none()
        .rounded(px(radius::FULL))
        .bg(colors.accent)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .child(ui::icon(if playing { "pause" } else { "play" }).size(px(16.)).text_color(colors.on_accent))
        .on_click({
            let toggle = toggle.clone();
            move |_, _, cx| {
                cx.stop_propagation();
                toggle(cx);
            }
        });
    let waveform = div()
        .id(SharedString::from(format!("{sound}-wave")))
        .flex_1()
        .min_w_0()
        .h(px(WAVE_HEIGHT))
        .relative()
        .flex()
        .items_center()
        .gap(px(BAR_GAP))
        .overflow_hidden()
        .cursor_pointer()
        .child({
            let track = track.clone();
            canvas(move |bounds, _, _| track.bounds.set(Some(bounds)), |_, _, _, _| {}).absolute().inset_0()
        })
        .children(heights.iter().enumerate().map(|(index, height)| {
            div()
                .flex_none()
                .w(px(BAR))
                .h(px((WAVE_HEIGHT * height).max(3.)))
                .rounded(px(radius::FULL))
                .bg(if index < played { colors.accent } else { fg.opacity(0.28) })
        }))
        .on_mouse_down(MouseButton::Left, {
            let (at, seek_to, toggle, focus) = (at.clone(), seek_to.clone(), toggle.clone(), focus.clone());
            move |event: &MouseDownEvent, window, cx| {
                // The press stops here, so the row's own focusing on a press doesn't run: the row takes the keyboard
                // itself, for Space.
                cx.stop_propagation();
                window.focus(&focus);
                match at(event.position.x) {
                    Some(seconds) => seek_to(seconds, false, cx),
                    // Its length isn't known yet: it plays from the start.
                    None => toggle(cx),
                }
            }
        })
        .on_drag(Scrub(sound.clone()), |drag, _, _, cx| cx.new(|_| drag.clone()))
        .on_drag_move({
            let (sound, at, seek_to) = (sound.clone(), at.clone(), seek_to.clone());
            move |event: &DragMoveEvent<Scrub>, window, cx| {
                if event.drag(cx).0 != sound {
                    return;
                }
                if let Some(seconds) = at(event.event.position.x) {
                    seek_to(seconds, false, cx);
                    window.refresh();
                }
            }
        })
        .on_mouse_up(MouseButton::Left, {
            let let_go = let_go.clone();
            move |_, _, cx| let_go(cx)
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| let_go(cx));
    let clock = |seconds: f64| crate::format::duration_clock(seconds * 1000.);
    let times = match duration {
        Some(duration) => format!("{} / {}", clock(time.min(duration)), clock(duration)),
        None => clock(time),
    };
    let speed = crate::media::speed(cx);
    let speed_label = if speed.fract() == 0. { format!("{speed:.0}×") } else { format!("{speed}×") };
    let speed_button = div()
        .id(SharedString::from(format!("{sound}-speed")))
        .flex_none()
        .w(px(SPEED_WIDTH))
        .h(px(20.))
        .rounded(px(radius::FULL))
        .bg(fg.opacity(if speed == 1. { 0.08 } else { 0.16 }))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(fg.opacity(0.2)))
        .caption()
        .semibold()
        .child(speed_label)
        .on_click(|_, _, cx| {
            cx.stop_propagation();
            crate::media::next_speed(cx);
        });

    let row = div()
        .id(SharedString::from(format!("{sound}-row")))
        .track_focus(&focus)
        .on_key_down(move |event: &KeyDownEvent, window, cx| on_row_key(event, &toggle, window, cx))
        // Before any part of the row has the press (the waveform keeps it to itself).
        .capture_any_mouse_down({
            let track = track.clone();
            move |_, _, _| track.clicked.set(true)
        })
        .w(px(width))
        .max_w_full()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .p(px(space::XXS))
        .m(px(-space::XXS))
        .rounded(px(radius::MD))
        .border_1()
        .border_color(if ring { colors.accent } else { gpui::transparent_black() })
        .text_color(fg)
        .child(crate::smoke::tracked(format!("audio-play-{name}"), button))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(waveform)
                        .child(
                            div()
                                .flex_none()
                                .w(px(TIME_WIDTH))
                                .flex()
                                .justify_end()
                                .caption()
                                .opacity(0.8)
                                .child(times),
                        )
                        .child(crate::smoke::tracked(format!("audio-speed-{name}"), speed_button)),
                )
                .child(div().caption().opacity(0.7).truncate().child(name.to_string())),
        );
    crate::smoke::tracked(format!("audio-row-{name}"), row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sounds_are_known_by_their_type_or_name() {
        for name in ["a.mp3", "b.M4A", "c.wav", "d.aac", "e.ogg", "f.flac", "g.opus", "h.aiff"] {
            assert!(crate::media::is_audio("application/octet-stream", name), "{name}");
        }
        assert!(crate::media::is_audio("audio/mp4", "recording"));
        assert!(!crate::media::is_audio("video/mp4", "clip.mp4") && !crate::media::is_audio("text/plain", "mp3"));
    }

    #[test]
    fn a_conversion_gets_longer_for_a_bigger_file_up_to_three_minutes() {
        assert_eq!(convert_timeout(0), Duration::from_secs(30));
        assert_eq!(convert_timeout(57 << 20), Duration::from_secs(58));
        assert_eq!(convert_timeout(10 << 30), Duration::from_secs(180));
    }

    #[test]
    fn a_conversion_that_hangs_is_stopped_as_timed_out() {
        // A named pipe nobody writes to: afconvert waits on it for ever.
        let folder = std::env::temp_dir().join(format!("filo-audio-hang-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let pipe = folder.join("stalled.wav");
        let path = std::ffi::CString::new(pipe.to_str().unwrap()).unwrap();
        // SAFETY: a plain C call with a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert_eq!(convert(&pipe, &folder.join("out.wav"), Duration::from_millis(400)), Err(Failure::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            convert(&folder.join("missing.mp3"), &folder.join("out.wav"), CONVERT_CHECK),
            Err(Failure::Unreadable)
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// Long enough for afconvert to give up on a file that isn't there.
    const CONVERT_CHECK: Duration = Duration::from_secs(20);

    #[test]
    fn keys_not_typed_back_in_time_are_dropped() {
        let key = |key: &str, typed: Option<&str>, platform: bool| gpui::Keystroke {
            modifiers: gpui::Modifiers { platform, ..Default::default() },
            key: key.into(),
            key_char: typed.map(Into::into),
        };
        let mut hand =
            HandBack { message_box: None, typed: vec![key("a", Some("a"), false)], since: Some(Instant::now()) };
        assert!(hand.going());
        hand.since = Some(Instant::now() - HAND_BACK_WITHIN - Duration::from_millis(1));
        assert!(!hand.going() && hand.typed.is_empty() && hand.since.is_none());
        // A letter goes back; a shortcut or a key that types nothing doesn't.
        assert!(typed(&key("a", Some("a"), false)) && typed(&key("space", Some(" "), false)));
        assert!(!typed(&key("a", Some("a"), true)) && !typed(&key("left", None, false)));
    }

    #[test]
    fn a_wave_file_gives_its_loudest_stretches() {
        // 8,000 samples (two seconds): quiet, then loud, after a chunk that isn't the samples.
        let mut samples = vec![128 + 16; RATE];
        samples.extend(vec![255; RATE]);
        let mut file = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0".to_vec();
        file.extend([0; 16]);
        file.extend(b"data");
        file.extend((samples.len() as u32).to_le_bytes());
        file.extend(&samples);
        let wave = wave_from(std::io::Cursor::new(file)).unwrap();
        assert_eq!(wave.seconds, 2.);
        assert_eq!(wave.peaks.len(), PEAKS);
        assert!((wave.peaks[0] - 16. / 127.).abs() < 0.01 && wave.peaks[PEAKS - 1] == 1.);
        assert_eq!(bars(&wave.peaks, 4), vec![wave.peaks[0], wave.peaks[0], 1., 1.]);
        assert!(wave_from(std::io::Cursor::new(b"not a wave file".to_vec())).is_none());
    }

    #[test]
    fn a_waveform_is_made_from_a_sound_file_and_kept() {
        let folder = std::env::temp_dir().join(format!("filo-audio-row-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        // One second of a loud square wave, as a 16-bit WAVE file afconvert reads.
        let pcm: Vec<u8> =
            (0..8000).flat_map(|at: i32| (if at % 40 < 20 { 20000_i16 } else { -20000 }).to_le_bytes()).collect();
        let mut file = b"RIFF".to_vec();
        file.extend((36 + pcm.len() as u32).to_le_bytes());
        file.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
        file.extend(8000_u32.to_le_bytes());
        file.extend(16000_u32.to_le_bytes());
        file.extend(b"\x02\0\x10\0data");
        file.extend((pcm.len() as u32).to_le_bytes());
        file.extend(pcm);
        let sound = folder.join("beep.wav");
        std::fs::write(&sound, file).unwrap();
        let key = format!("test-{}", std::process::id());
        let size = std::fs::metadata(&sound).unwrap().len();
        let wave = make_wave(&sound, &key, size).expect("waveform");
        assert!((wave.seconds - 1.).abs() < 0.01, "{}", wave.seconds);
        assert!(wave.peaks.iter().all(|peak| *peak > 0.5));
        let kept = crate::media::kept_folder("waves").join(format!("{key}.json"));
        assert!(kept.is_file());
        // No samples or half-written files left behind.
        let left = std::fs::read_dir(crate::media::kept_folder("waves"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&format!("{key}-")))
            .count();
        assert_eq!(left, 0);
        // Read back the second time.
        std::fs::remove_file(&sound).unwrap();
        assert_eq!(make_wave(&sound, &key, size), Ok(wave));
        let _ = std::fs::remove_file(kept);
        let _ = std::fs::remove_dir_all(&folder);
    }
}

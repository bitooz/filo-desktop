// An audio file where it was shared (a chat's or the Group's message, a coding result's page), played in place: a
// round play button, the sound's waveform (the part played in the accent colour; a press or a drag along it goes
// there), the time played and its length, and a speed button (1×, 1.5×, 2×, for every sound), with the file's name under
// them. It plays from its file on this Mac through media.rs (one sound at a time, AVFoundation's player), so a file
// attached without a copy plays from where it is. A click anywhere on the row (or Tab) gives it the keyboard, and Space
// plays or pauses it then; a letter typed there goes back to where the keyboard was before (the message box), with
// the letter, so typing never goes nowhere (`Keyboard`).
//
// The waveform is made once per file (`wave`), one file at a time on a thread of its own: macOS's `afconvert` turns the
// sound into 4,000 8-bit samples a second, mono, in a temporary file of its own, and the loudest of each stretch is a
// bar (read in pieces, so a long sound takes little memory). A conversion that runs past `CONVERT_TIMEOUT` (a file on a
// stalled network volume) is stopped. The waveform is kept in a temporary folder by the file's path, size and time
// (media.rs `file_key`, as video_player.rs keeps a video's poster), and in memory by the same key, so a file that
// changes is made again. Until then, or for a file it can't read (tried again after `RETRY`), the bars are all one
// height.

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
/// The longest a waveform's conversion may take (an hour of mp3 takes about five seconds).
const CONVERT_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a file whose waveform couldn't be made waits before it's tried again.
const RETRY: Duration = Duration::from_secs(60);

/// A sound's waveform: how loud each of its `PEAKS` stretches is, from 0 to 1, and how long it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    pub peaks: Vec<f32>,
    pub seconds: f64,
}

enum Made {
    Making,
    Done(Wave),
    Failed(Instant),
}

/// Waveforms by file (`media::file_key`).
#[derive(Default)]
struct Waves(HashMap<String, Made>);

impl Global for Waves {}

/// `file`'s waveform once it's made; the first ask starts making it, and the windows draw again when it's there.
pub fn wave(file: &Path, cx: &mut App) -> Option<Wave> {
    let key = crate::media::file_key(file)?;
    let waves = cx.default_global::<Waves>();
    match waves.0.get(&key) {
        Some(Made::Done(wave)) => return Some(wave.clone()),
        Some(Made::Making) => return None,
        Some(Made::Failed(at)) if at.elapsed() < RETRY => return None,
        _ => {}
    }
    waves.0.insert(key.clone(), Made::Making);
    let (reply, made) = futures::channel::oneshot::channel();
    if maker().send((file.to_path_buf(), key.clone(), reply)).is_err() {
        waves.0.insert(key, Made::Failed(Instant::now()));
        return None;
    }
    cx.spawn(async move |cx| {
        let made = made.await.ok().flatten();
        cx.update(|cx| {
            let done = made.is_some();
            let made = made.map_or_else(|| Made::Failed(Instant::now()), Made::Done);
            cx.default_global::<Waves>().0.insert(key, made);
            if done {
                cx.refresh_windows();
            }
        })
        .ok();
    })
    .detach();
    None
}

type Job = (PathBuf, String, futures::channel::oneshot::Sender<Option<Wave>>);

/// The thread that makes waveforms, one at a time, in the order they're asked for.
fn maker() -> &'static mpsc::Sender<Job> {
    static MAKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    MAKER.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("waveforms".into())
            .spawn(move || {
                for (file, key, reply) in queue {
                    let _ = reply.send(make_wave(&file, &key));
                }
            })
            .expect("waveform thread");
        jobs
    })
}

/// The waveform of the sound at `file`, kept under `key` (its `file_key`): read back, or made and kept.
fn make_wave(file: &Path, key: &str) -> Option<Wave> {
    let folder = crate::media::kept_folder("waves");
    let kept = folder.join(format!("{key}.json"));
    if let Some(wave) = std::fs::read(&kept).ok().and_then(|bytes| {
        let kept: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let peaks: Option<Vec<f32>> =
            kept["peaks"].as_array()?.iter().map(|peak| peak.as_f64().map(|peak| peak as f32)).collect();
        Some(Wave { peaks: peaks?, seconds: kept["seconds"].as_f64()? })
    }) {
        return Some(wave);
    }
    std::fs::create_dir_all(&folder).ok()?;
    // Its own name: another Filo on this Mac may be making the same file's waveform.
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let samples = folder.join(format!("{key}-{}-{made}.wav", std::process::id()));
    let converted = convert(file, &samples);
    let wave = converted.then(|| std::fs::File::open(&samples).ok().and_then(wave_from)).flatten();
    let _ = std::fs::remove_file(&samples);
    let wave = wave?;
    // Written aside and moved in, so a half-written file is never read back.
    let part = folder.join(format!("{key}-{}-{made}.json.part", std::process::id()));
    let written = serde_json::json!({ "peaks": wave.peaks, "seconds": wave.seconds });
    if std::fs::write(&part, written.to_string()).and_then(|_| std::fs::rename(&part, &kept)).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    Some(wave)
}

/// `afconvert` from `file` to 8-bit mono samples at `RATE` in the WAVE file `samples`; false when it failed or ran past
/// `CONVERT_TIMEOUT` (it's stopped then).
fn convert(file: &Path, samples: &Path) -> bool {
    let Ok(mut child) = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", &format!("UI8@{RATE}"), "-c", "1"])
        .arg(file)
        .arg(samples)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < CONVERT_TIMEOUT => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
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

/// The rows' keyboard focus, and where the keyboard last was outside every row (the message box, mostly): a letter
/// typed on a row goes back there.
#[derive(Default)]
struct Keyboard {
    rows: Vec<WeakFocusHandle>,
    elsewhere: Option<WeakFocusHandle>,
    /// Keys typed on a row, on their way back there (`on_row_key`).
    typed: Vec<gpui::Keystroke>,
}

impl Global for Keyboard {}

/// A row's focus, made the first time it's drawn; and where the keyboard is now, when that's outside every row.
fn row_focus(sound: &SharedString, window: &mut Window, cx: &mut App) -> FocusHandle {
    let focus = window.use_keyed_state(SharedString::from(format!("{sound}-focus")), cx, |_, cx| {
        let focus = cx.focus_handle().tab_stop(true);
        let keyboard = cx.default_global::<Keyboard>();
        keyboard.rows.retain(|row| row.upgrade().is_some());
        keyboard.rows.push(focus.downgrade());
        focus
    });
    let focus = focus.read(cx).clone();
    if let Some(now) = window.focused(cx) {
        let keyboard = cx.default_global::<Keyboard>();
        if !keyboard.rows.iter().any(|row| *row == now) {
            keyboard.elsewhere = Some(now.downgrade());
        }
    }
    focus
}

/// A key on a focused row: Space plays or pauses it; a letter (no ⌘ or ⌃) goes back to where the keyboard was before
/// the row had it. The row keeps the keyboard until the next frame, gathering what's typed meanwhile (a space too); then
/// the keyboard goes back, and once the window has drawn with it there (its text input is set up as it draws), the keys
/// are typed there in order.
fn on_row_key(event: &KeyDownEvent, toggle: &dyn Fn(&mut App), window: &mut Window, cx: &mut App) {
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.function {
        return;
    }
    let handing_back = cx.try_global::<Keyboard>().is_some_and(|keyboard| !keyboard.typed.is_empty());
    if keystroke.key == "space" && !handing_back {
        if !modifiers.modified() {
            cx.stop_propagation();
            toggle(cx);
        }
        return;
    }
    let typed =
        keystroke.key_char.as_ref().is_some_and(|typed| !typed.is_empty() && !typed.chars().any(char::is_control));
    let elsewhere = cx.try_global::<Keyboard>().and_then(|keyboard| keyboard.elsewhere.as_ref()?.upgrade());
    if !typed || elsewhere.is_none() {
        return;
    }
    cx.stop_propagation();
    cx.default_global::<Keyboard>().typed.push(keystroke.clone());
    if handing_back {
        return;
    }
    window.on_next_frame(|window, cx| {
        let back = cx.try_global::<Keyboard>().and_then(|keyboard| keyboard.elsewhere.as_ref()?.upgrade());
        match back {
            Some(back) => window.focus(&back),
            None => return cx.default_global::<Keyboard>().typed.clear(),
        }
        window.on_next_frame(|window, cx| {
            for keystroke in std::mem::take(&mut cx.default_global::<Keyboard>().typed) {
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
        .w(px(width))
        .max_w_full()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .p(px(space::XXS))
        .m(px(-space::XXS))
        .rounded(px(radius::MD))
        .border_1()
        .border_color(if focused { colors.accent } else { gpui::transparent_black() })
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
        let wave = make_wave(&sound, &key).expect("waveform");
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
        assert_eq!(make_wave(&sound, &key), Some(wave));
        let _ = std::fs::remove_file(kept);
        let _ = std::fs::remove_dir_all(&folder);
    }
}

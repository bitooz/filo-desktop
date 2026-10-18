// An audio file where it was shared (a chat's or the Group's message, a coding result's page), played in place: a
// round play button, the sound's waveform (the part played in the accent colour; a press or a drag along it goes
// there), the time played and its length, and a speed button (1×, 1.5×, 2×, for every sound), with the file's name under
// them. It plays from its file on this Mac through media.rs (one sound at a time, AVFoundation's player), so a file
// attached without a copy plays from where it is. The row takes the keyboard when clicked or tabbed to, and Space
// plays or pauses it then.
//
// The waveform is made once per file (`wave`), off the main thread: macOS's `afconvert` turns the sound into 4,000
// 8-bit samples a second, mono, in a temporary folder, and the loudest of each stretch is a bar; it's kept there by the
// file's path, size and time, as video_player.rs keeps a video's poster. Until then, or for a file it can't read, the
// bars are all one height.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Bounds, DragMoveEvent, FocusHandle, Global, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};

use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// Stretches of a sound kept per file; the bars a row draws are taken from these.
const PEAKS: usize = 240;
/// Samples a second the waveform is made from.
const RATE: usize = 4000;

/// Whether `name` is a sound the row plays, by its extension (as AVFoundation reads them on macOS 13 and later).
pub fn is_audio_name(name: &str) -> bool {
    let extension = Path::new(name).extension().and_then(|extension| extension.to_str()).unwrap_or_default();
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "mp3" | "m4a" | "wav" | "aac" | "ogg" | "oga" | "opus" | "flac" | "aif" | "aiff" | "caf"
    )
}

/// A sound's waveform: how loud each of its `PEAKS` stretches is, from 0 to 1, and how long it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    pub peaks: Vec<f32>,
    pub seconds: f64,
}

/// Waveforms by file: `None` while one is being made, or when none could be.
#[derive(Default)]
struct Waves(HashMap<PathBuf, Option<Wave>>);

impl Global for Waves {}

/// `file`'s waveform once it's made; the first ask starts making it, and the windows draw again when it's there.
pub fn wave(file: &Path, cx: &mut App) -> Option<Wave> {
    let waves = cx.default_global::<Waves>();
    if let Some(known) = waves.0.get(file) {
        return known.clone();
    }
    waves.0.insert(file.to_path_buf(), None);
    let file = file.to_path_buf();
    cx.spawn(async move |cx| {
        let made = {
            let file = file.clone();
            cx.background_executor().spawn(async move { make_wave(&file) }).await
        };
        cx.update(|cx| {
            if made.is_some() {
                cx.default_global::<Waves>().0.insert(file, made);
                cx.refresh_windows();
            }
        })
        .ok();
    })
    .detach();
    None
}

/// The waveform of the sound at `file`, made once per file (by its path, size and time), then read back.
fn make_wave(file: &Path) -> Option<Wave> {
    use sha2::Digest as _;
    let meta = std::fs::metadata(file).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_millis());
    let id = sha2::Sha256::digest(format!("{}\n{}\n{modified}", file.display(), meta.len()).as_bytes());
    let name: String = id[..10].iter().map(|byte| format!("{byte:02x}")).collect();
    let folder = std::env::temp_dir().join("filo-desktop-waves");
    let kept = folder.join(format!("{name}.json"));
    if let Some(wave) = std::fs::read(&kept).ok().and_then(|bytes| {
        let kept: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let peaks: Option<Vec<f32>> =
            kept["peaks"].as_array()?.iter().map(|peak| peak.as_f64().map(|peak| peak as f32)).collect();
        Some(Wave { peaks: peaks?, seconds: kept["seconds"].as_f64()? })
    }) {
        return Some(wave);
    }
    std::fs::create_dir_all(&folder).ok()?;
    let samples = folder.join(format!("{name}.wav"));
    let converted = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", &format!("UI8@{RATE}"), "-c", "1"])
        .arg(file)
        .arg(&samples)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let bytes = converted.then(|| std::fs::read(&samples).ok()).flatten();
    let _ = std::fs::remove_file(&samples);
    let wave = peaks(wav_data(&bytes?)?)?;
    let written = serde_json::json!({ "peaks": wave.peaks, "seconds": wave.seconds });
    std::fs::write(&kept, written.to_string()).ok()?;
    Some(wave)
}

/// The samples of a WAVE file: its `data` chunk.
fn wav_data(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        if &bytes[at..at + 4] == b"data" {
            return bytes.get(body..(body + size).min(bytes.len()));
        }
        at = body + size + size % 2;
    }
    None
}

/// Unsigned 8-bit samples at `RATE` as `PEAKS` stretches, each its loudest sample, the loudest stretch 1.
fn peaks(samples: &[u8]) -> Option<Wave> {
    if samples.is_empty() {
        return None;
    }
    let mut peaks: Vec<f32> = (0..PEAKS)
        .map(|index| {
            let (start, end) = (samples.len() * index / PEAKS, (samples.len() * (index + 1) / PEAKS).max(1));
            let stretch = &samples[start.min(samples.len() - 1)..end.min(samples.len())];
            stretch.iter().map(|sample| (f32::from(*sample) - 128.).abs() / 128.).fold(0., f32::max)
        })
        .collect();
    let loudest = peaks.iter().copied().fold(0., f32::max);
    if loudest > 0. {
        peaks.iter_mut().for_each(|peak| *peak /= loudest);
    }
    Some(Wave { peaks, seconds: samples.len() as f64 / RATE as f64 })
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
    let state = crate::media::state(&sound, cx);
    let playing = state.is_some_and(|state| state.playing);
    // The samples' length over the player's: AVFoundation's can run past the sound's end (a FLAC's).
    let duration = wave.as_ref().map(|wave| wave.seconds).or(state.and_then(|state| state.duration));
    let track =
        window.use_keyed_state(SharedString::from(format!("{sound}-track")), cx, |_, _| Rc::new(Track::default()));
    let track = track.read(cx).clone();
    let time = track.scrubbing.get().or(state.map(|state| state.time)).unwrap_or(0.);
    let fraction = duration.map_or(0., |duration| (time / duration.max(0.001)).clamp(0., 1.)) as f32;
    let focus = window
        .use_keyed_state(SharedString::from(format!("{sound}-focus")), cx, |_, cx| cx.focus_handle().tab_stop(true));
    let focus: FocusHandle = focus.read(cx).clone();
    let focused = focus.is_focused(window);

    let wave_width = (width - BUTTON - TIME_WIDTH - SPEED_WIDTH - space::SM * 3.).max(60.);
    let count = ((wave_width + BAR_GAP) / (BAR + BAR_GAP)).floor().max(8.) as usize;
    let heights = wave.as_ref().map_or_else(|| vec![0.3; count], |wave| bars(&wave.peaks, count));
    let played = (fraction * count as f32).round() as usize;
    let toggle = {
        let (sound, url) = (sound.clone(), url.clone());
        move |cx: &mut App| {
            if let Some(url) = &url {
                crate::media::toggle(sound.clone(), url.clone(), cx);
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
            crate::media::seek(sound.clone(), url, seconds, exact, cx);
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
            let (at, seek_to, toggle) = (at.clone(), seek_to.clone(), toggle.clone());
            move |event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
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
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if event.keystroke.key == "space" && !event.keystroke.modifiers.modified() {
                cx.stop_propagation();
                toggle(cx);
            }
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
    fn sounds_are_known_by_their_extension() {
        for name in ["a.mp3", "b.M4A", "c.wav", "d.aac", "e.ogg", "f.flac"] {
            assert!(is_audio_name(name), "{name}");
        }
        assert!(!is_audio_name("clip.mp4") && !is_audio_name("notes.txt") && !is_audio_name("mp3"));
    }

    #[test]
    fn a_wave_file_gives_its_loudest_stretches() {
        // 8,000 samples (two seconds): quiet, then loud.
        let mut samples = vec![128 + 16; RATE];
        samples.extend(vec![255; RATE]);
        let mut file = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0".to_vec();
        file.extend([0; 16]);
        file.extend(b"data");
        file.extend((samples.len() as u32).to_le_bytes());
        file.extend(&samples);
        let wave = peaks(wav_data(&file).unwrap()).unwrap();
        assert_eq!(wave.seconds, 2.);
        assert_eq!(wave.peaks.len(), PEAKS);
        assert!((wave.peaks[0] - 16. / 127.).abs() < 0.01 && wave.peaks[PEAKS - 1] == 1.);
        assert_eq!(bars(&wave.peaks, 4), vec![wave.peaks[0], wave.peaks[0], 1., 1.]);
        assert!(wav_data(b"not a wave file").is_none());
    }
}

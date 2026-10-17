// Voice message recording with macOS's AVAudioRecorder: mono AAC in an .m4a
// (`audio/mp4`, chat-protocol.ts VOICE_CONTENT_TYPE), 64 kbps like the phone's
// expo-audio preset, to a temp file the chat uploads. Alongside it, `MicStream`
// taps the same microphone through AVAudioEngine for live transcription.

use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use objc2::AllocAnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_avf_audio::{
    AVAudioEngine, AVAudioPCMBuffer, AVAudioQuality, AVAudioRecorder, AVAudioTime, AVEncoderAudioQualityKey,
    AVEncoderBitRateKey, AVFormatIDKey, AVNumberOfChannelsKey, AVSampleRateKey,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

/// `kAudioFormatMPEG4AAC` ('aac ').
const AAC: u32 = u32::from_be_bytes(*b"aac ");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
    /// Never asked: the system asks on `request_permission`.
    Undetermined,
}

/// Whether this is `Filo.app` (scripts/app.sh) rather than the bare binary.
pub fn in_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

pub fn permission() -> Permission {
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return Permission::Denied };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) } {
        AVAuthorizationStatus::Authorized => Permission::Granted,
        AVAuthorizationStatus::NotDetermined => Permission::Undetermined,
        _ => Permission::Denied,
    }
}

/// Shows the system's microphone prompt; resolves to whether access was granted.
pub fn request_permission() -> futures::channel::oneshot::Receiver<bool> {
    let (tx, rx) = futures::channel::oneshot::channel();
    if crate::test_run::refuse("microphone prompt") {
        let _ = tx.send(false);
        return rx;
    }
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        let _ = tx.send(false);
        return rx;
    };
    // The handler runs once, on a queue of the system's choosing.
    let tx = Mutex::new(Some(tx));
    let handler = block2::RcBlock::new(move |granted: Bool| {
        if let Some(tx) = tx.lock().ok().and_then(|mut tx| tx.take()) {
            let _ = tx.send(granted.as_bool());
        }
    });
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler) };
    rx
}

pub struct Recorder {
    inner: Retained<AVAudioRecorder>,
    path: PathBuf,
}

impl Recorder {
    /// Starts recording to a new temp file.
    pub fn start() -> Result<Recorder> {
        if crate::test_run::refuse("microphone") {
            return Err(anyhow!("No microphone in test runs"));
        }
        let dir = std::env::temp_dir().join("filo-desktop-voice");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.m4a", uuid::Uuid::new_v4()));
        let keys = unsafe {
            [AVFormatIDKey, AVSampleRateKey, AVNumberOfChannelsKey, AVEncoderBitRateKey, AVEncoderAudioQualityKey]
        };
        let keys: Vec<&NSString> =
            keys.into_iter().collect::<Option<_>>().ok_or_else(|| anyhow!("Audio settings unavailable"))?;
        let values: [Retained<NSNumber>; 5] = [
            NSNumber::new_u32(AAC),
            NSNumber::new_f64(44_100.),
            NSNumber::new_u32(1),
            NSNumber::new_u32(64_000),
            NSNumber::new_isize(AVAudioQuality::High.0),
        ];
        let values: Vec<&AnyObject> = values.iter().map(|value| value.as_ref()).collect();
        let settings = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let inner = unsafe { AVAudioRecorder::initWithURL_settings_error(AVAudioRecorder::alloc(), &url, &settings) }
            .map_err(|error| anyhow!("{}", error.localizedDescription()))?;
        unsafe {
            inner.setMeteringEnabled(true);
            if !inner.prepareToRecord() || !inner.record() {
                return Err(anyhow!("The microphone could not start"));
            }
        }
        Ok(Recorder { inner, path })
    }

    pub fn elapsed_ms(&self) -> f64 {
        unsafe { self.inner.currentTime() * 1000. }
    }

    pub fn is_recording(&self) -> bool {
        unsafe { self.inner.isRecording() }
    }

    /// Input loudness now, 0 (silence) to 1.
    pub fn level(&self) -> f32 {
        let decibels = unsafe {
            self.inner.updateMeters();
            self.inner.averagePowerForChannel(0)
        };
        ((decibels + 50.) / 50.).clamp(0., 1.)
    }

    /// Stops and returns the finished file with its length.
    pub fn finish(self) -> (PathBuf, f64) {
        let elapsed = self.elapsed_ms();
        unsafe { self.inner.stop() };
        (self.path.clone(), elapsed)
    }

    /// Stops and deletes the file.
    pub fn cancel(self) {
        unsafe { self.inner.stop() };
        remove(&self.path);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // `finish` and `cancel` already stopped it; this covers a chat closed mid-recording.
        unsafe {
            if self.inner.isRecording() {
                self.inner.stop();
                self.inner.deleteRecording();
            }
        }
    }
}

/// Raw microphone audio for live transcription, captured next to the
/// recorder (macOS lets both read the input at once): an AVAudioEngine tap on
/// the input, downmixed to mono and resampled to PCM16 little-endian.
pub struct MicStream {
    engine: Retained<AVAudioEngine>,
}

impl MicStream {
    /// `on_pcm` runs on an audio thread with each chunk of `sample_rate` PCM16.
    pub fn start(sample_rate: u32, on_pcm: impl Fn(Vec<u8>) + Send + Sync + 'static) -> Result<MicStream> {
        if crate::test_run::refuse("microphone") {
            return Err(anyhow!("No microphone in test runs"));
        }
        unsafe {
            let engine = AVAudioEngine::new();
            let input = engine.inputNode();
            let format = input.outputFormatForBus(0);
            // No input device: tapping would throw an Objective-C exception.
            if format.sampleRate() <= 0. || format.channelCount() == 0 {
                return Err(anyhow!("No microphone input"));
            }
            let resampler = Mutex::new(Resampler::new(format.sampleRate(), sample_rate as f64));
            let tap = block2::RcBlock::new(move |buffer: NonNull<AVAudioPCMBuffer>, _: NonNull<AVAudioTime>| {
                let buffer = buffer.as_ref();
                let channels = buffer.floatChannelData();
                if channels.is_null() {
                    return;
                }
                let count = buffer.format().channelCount() as usize;
                let (frames, stride) = (buffer.frameLength() as usize, buffer.stride());
                let mono: Vec<f32> = (0..frames)
                    .map(|frame| {
                        (0..count).map(|channel| *(*channels.add(channel)).as_ptr().add(frame * stride)).sum::<f32>()
                            / count as f32
                    })
                    .collect();
                let pcm = resampler.lock().map(|mut resampler| resampler.push(&mono)).unwrap_or_default();
                if !pcm.is_empty() {
                    on_pcm(pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect());
                }
            });
            // About 100 ms per chunk at 48 kHz; the engine copies the block.
            input.installTapOnBus_bufferSize_format_block(0, 4800, Some(&format), block2::RcBlock::as_ptr(&tap));
            engine.prepare();
            if let Err(error) = engine.startAndReturnError() {
                input.removeTapOnBus(0);
                return Err(anyhow!("{}", error.localizedDescription()));
            }
            Ok(MicStream { engine })
        }
    }

    pub fn stop(self) {}
}

impl Drop for MicStream {
    fn drop(&mut self) {
        unsafe {
            self.engine.inputNode().removeTapOnBus(0);
            self.engine.stop();
        }
    }
}

/// Float samples at the microphone's rate to PCM16 at the target rate. Each
/// output sample averages the input samples it covers, which keeps speech
/// clear without a separate low-pass filter.
struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Input samples still to take before the next output sample.
    left: f64,
    sum: f32,
    count: u32,
    last: f32,
}

impl Resampler {
    fn new(from: f64, to: f64) -> Resampler {
        let step = from / to;
        Resampler { step, left: step, sum: 0., count: 0, last: 0. }
    }

    fn push(&mut self, samples: &[f32]) -> Vec<i16> {
        let mut out = Vec::with_capacity((samples.len() as f64 / self.step) as usize + 1);
        for &sample in samples {
            self.sum += sample;
            self.count += 1;
            self.left -= 1.;
            // Upsampling repeats a sample; downsampling averages several.
            while self.left <= 0. {
                if self.count > 0 {
                    self.last = self.sum / self.count as f32;
                    self.sum = 0.;
                    self.count = 0;
                }
                out.push((self.last.clamp(-1., 1.) * i16::MAX as f32).round() as i16);
                self.left += self.step;
            }
        }
        out
    }
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_the_target_rate_and_level() {
        for from in [48_000., 44_100., 8_000.] {
            let mut resampler = Resampler::new(from, 16_000.);
            let second = vec![0.5f32; from as usize];
            // In chunks, like the tap delivers them.
            let out: Vec<i16> = second.chunks(4800).flat_map(|chunk| resampler.push(chunk)).collect();
            assert!((out.len() as i64 - 16_000).abs() <= 1, "{from} Hz gave {} samples", out.len());
            assert!(out.iter().all(|&sample| (sample - 16_384).abs() <= 1));
        }
    }
}

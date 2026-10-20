// Voice message recording with macOS's AVAudioRecorder: mono AAC in an .m4a
// (`audio/mp4`, chat-protocol.ts VOICE_CONTENT_TYPE), 64 kbps like the phone's
// expo-audio preset, to a temp file the chat uploads. Alongside it, `MicStream`
// reads the same microphone through an audio queue for live transcription.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use objc2::AllocAnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_avf_audio::{
    AVAudioQuality, AVAudioRecorder, AVEncoderAudioQualityKey, AVEncoderBitRateKey, AVFormatIDKey,
    AVNumberOfChannelsKey, AVSampleRateKey,
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

/// Recorders alive now (one from `start` until it's dropped).
static RECORDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Whether the microphone records now: no sound may play then (media.rs), or it would be in the recording.
pub fn recording() -> bool {
    RECORDING.load(std::sync::atomic::Ordering::Relaxed) > 0
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
        RECORDING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        RECORDING.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        // `finish` and `cancel` already stopped it; this covers a chat closed mid-recording.
        unsafe {
            if self.inner.isRecording() {
                self.inner.stop();
                self.inner.deleteRecording();
            }
        }
    }
}

/// Raw microphone audio for live transcription, captured next to the recorder (macOS lets both read the input at
/// once): an audio queue on the input, which macOS hands mono PCM16 little-endian at the rate asked for. Not
/// AVAudioEngine: its input brings up a device that pairs the microphone with the speakers, which took ~0.5 s each
/// time the microphone opened (with the window frozen), and it stops when the output changes mid-recording.
pub struct MicStream {
    queue: AudioQueueRef,
    /// Where the queue's callback finds `on_pcm`; freed once the queue is gone.
    tap: *mut Tap,
}

type AudioQueueRef = *mut c_void;

struct Tap {
    on_pcm: Box<dyn Fn(Vec<u8>) + Send + Sync>,
}

/// `AudioStreamBasicDescription`
#[repr(C)]
struct StreamFormat {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

/// `AudioQueueBuffer`
#[repr(C)]
struct QueueBuffer {
    capacity: u32,
    data: *mut c_void,
    byte_size: u32,
    user_data: *mut c_void,
    packet_description_capacity: u32,
    packet_descriptions: *mut c_void,
    packet_description_count: u32,
}

type InputCallback = extern "C" fn(*mut c_void, AudioQueueRef, *mut QueueBuffer, *const c_void, u32, *const c_void);

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioQueueNewInput(
        format: *const StreamFormat,
        callback: InputCallback,
        user_data: *mut c_void,
        run_loop: *const c_void,
        run_loop_mode: *const c_void,
        flags: u32,
        queue: *mut AudioQueueRef,
    ) -> i32;
    fn AudioQueueAllocateBuffer(queue: AudioQueueRef, bytes: u32, buffer: *mut *mut QueueBuffer) -> i32;
    fn AudioQueueEnqueueBuffer(
        queue: AudioQueueRef,
        buffer: *mut QueueBuffer,
        count: u32,
        packets: *const c_void,
    ) -> i32;
    fn AudioQueueStart(queue: AudioQueueRef, start_time: *const c_void) -> i32;
    fn AudioQueueStop(queue: AudioQueueRef, immediate: u8) -> i32;
    fn AudioQueueDispose(queue: AudioQueueRef, immediate: u8) -> i32;
}

/// `kAudioFormatLinearPCM`, signed integer and packed.
const LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
const PCM_SIGNED_PACKED: u32 = (1 << 2) | (1 << 3);
/// Buffers the queue fills in turn, and how much each holds.
const BUFFERS: usize = 3;
const BUFFER_MS: u32 = 100;

/// Runs on the queue's own thread with each filled buffer, and hands the buffer back to be filled again.
extern "C" fn on_input(
    tap: *mut c_void,
    queue: AudioQueueRef,
    buffer: *mut QueueBuffer,
    _start_time: *const c_void,
    _packets: u32,
    _descriptions: *const c_void,
) {
    // SAFETY: `tap` is the `Tap` MicStream made for this queue, which outlives it; `buffer` is one of its buffers.
    unsafe {
        let (data, size) = ((*buffer).data as *const u8, (*buffer).byte_size as usize);
        if size > 0 {
            ((*(tap as *const Tap)).on_pcm)(std::slice::from_raw_parts(data, size).to_vec());
        }
        // Fails once the queue is stopping, which is fine.
        AudioQueueEnqueueBuffer(queue, buffer, 0, std::ptr::null());
    }
}

impl MicStream {
    /// `on_pcm` runs on the queue's thread with each chunk of `sample_rate` PCM16.
    pub fn start(sample_rate: u32, on_pcm: impl Fn(Vec<u8>) + Send + Sync + 'static) -> Result<MicStream> {
        if crate::test_run::refuse("microphone") {
            return Err(anyhow!("No microphone in test runs"));
        }
        let format = StreamFormat {
            sample_rate: sample_rate as f64,
            format_id: LINEAR_PCM,
            format_flags: PCM_SIGNED_PACKED,
            bytes_per_packet: 2,
            frames_per_packet: 1,
            bytes_per_frame: 2,
            channels_per_frame: 1,
            bits_per_channel: 16,
            reserved: 0,
        };
        let tap = Box::into_raw(Box::new(Tap { on_pcm: Box::new(on_pcm) }));
        let mut queue: AudioQueueRef = std::ptr::null_mut();
        // SAFETY: the format and the callback are as AudioToolbox declares them; with no run loop the callback runs
        // on the queue's own thread. `tap` stays alive until the queue is disposed (`Drop`).
        unsafe {
            let status = AudioQueueNewInput(
                &format,
                on_input,
                tap as *mut c_void,
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut queue,
            );
            if status != 0 || queue.is_null() {
                drop(Box::from_raw(tap));
                return Err(anyhow!("No microphone input ({status})"));
            }
            let stream = MicStream { queue, tap };
            let bytes = sample_rate * 2 * BUFFER_MS / 1000;
            for _ in 0..BUFFERS {
                let mut buffer = std::ptr::null_mut();
                let status = AudioQueueAllocateBuffer(queue, bytes, &mut buffer);
                if status != 0 || AudioQueueEnqueueBuffer(queue, buffer, 0, std::ptr::null()) != 0 {
                    return Err(anyhow!("The microphone stream could not start ({status})"));
                }
            }
            let status = AudioQueueStart(queue, std::ptr::null());
            if status != 0 {
                return Err(anyhow!("The microphone stream could not start ({status})"));
            }
            Ok(stream)
        }
    }

    pub fn stop(self) {}
}

impl Drop for MicStream {
    fn drop(&mut self) {
        // SAFETY: the queue and the tap made in `start`, freed once; disposing waits for a callback in progress.
        unsafe {
            AudioQueueStop(self.queue, 1);
            AudioQueueDispose(self.queue, 1);
            drop(Box::from_raw(self.tap));
        }
    }
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

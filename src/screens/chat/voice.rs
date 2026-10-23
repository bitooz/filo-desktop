// Voice messages from the composer. The mic shows while the composer is empty; a click records, then Send sends it,
// Stop keeps it to listen to first, and Delete throws it away. Holding fn+⌃ records too, for the message box: letting go
// writes what was said in at the caret, as if typed, and sends nothing (`dictate`); Esc cancels (the shell watches the
// keys, app.rs). The voice shortcut held in Filo's window records a voice message here, sent when it's let go. While
// recording,
// the microphone also streams to Filo's relay (live_transcription.rs), and the words it hears show in the recording's
// row; with its transcript voiceRecordings:sendLive sends the message at once and the recording is kept behind it
// (local_backend/voice.rs). Without one: createUpload, the recording kept (`Backend::upload_voice`, the old upload
// route), then voiceRecordings:send, which transcribes the file through the relay. A failed send keeps the recording
// and its upload, so Send again doesn't upload it again; an allowance used up or no connection says so, with when
// voice messages are back.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use super::*;
use crate::backend::Backend;
use crate::live_transcription::{self, Heard, LiveTranscription, Marks, Problem, Relay, Transcript};
use crate::recorder::{self, MicStream, Permission, Recorder};

/// A recording's shortest and longest length, its largest file and its type (local_backend/voice.rs takes the same).
const MIN_VOICE_MS: f64 = 300.;
const MAX_VOICE_MS: f64 = 10. * 60_000.;
const MAX_VOICE_BYTES: u64 = 10 * 1024 * 1024;
/// local_backend/voice.rs NOTHING_HEARD.
const NOTHING_HEARD: &str = "No words were heard in this recording.";
/// Level bars kept for the live meter, newest last.
const LEVELS: usize = 40;
const MIC_OFF: &str = "Microphone access is off. Turn it on in System Settings › Privacy & Security › Microphone.";
/// Started from a terminal, macOS treats Filo as part of the terminal app and won't ask for it.
const NOT_AN_APP: &str = "macOS asks for the microphone only when Filo runs as an app. Start it with scripts/app.sh.";

pub(super) enum VoiceState {
    /// The recording row is up and the microphone opens once it's drawn (`open_microphone`).
    Opening,
    Recording {
        recorder: Recorder,
        live: Option<Live>,
        started: Instant,
        levels: VecDeque<f32>,
    },
    /// Stopped: ready to listen to, send or delete.
    Preview {
        path: PathBuf,
        duration_ms: f64,
        transcript: Option<Transcript>,
    },
    /// Sent: shown as the pending bubble; the file stays for a retry.
    Sending {
        path: PathBuf,
        duration_ms: f64,
    },
}

/// The microphone streaming to live transcription while recording.
pub(super) struct Live {
    mic: MicStream,
    transcription: LiveTranscription,
}

/// Recordings of sent voice messages still being kept, by message id: their
/// bubbles play the recorder's file until the kept copy is there.
static UPLOADING: LazyLock<Mutex<HashMap<String, PathBuf>>> = LazyLock::new(Default::default);

pub(super) fn uploading_voice_url(message_id: &str) -> Option<String> {
    UPLOADING.lock().unwrap().get(message_id).map(|path| format!("file://{}", path.display()))
}

/// How far a voice send got, kept across a failed attempt.
pub(super) struct VoiceReceipt {
    path: PathBuf,
    /// The pending row's id, reused as sendLive's request id so a retry finds the sent message.
    request_id: String,
    session_id: String,
    reply: Option<ReplyTarget>,
    upload_id: Option<String>,
    token: Option<String>,
    uploaded: bool,
}

/// "0:07" while recording: whole seconds so far.
fn elapsed_clock(ms: f64) -> String {
    crate::format::minutes_seconds((ms / 1000.).floor().max(0.) as i64)
}

/// Keeps the recording of a voice message sendLive already sent (a copy on this Mac),
/// without holding up the chat. If it fails the message keeps its words, and the recorder's file plays until the app
/// quits.
fn upload_in_background(backend: &Backend, message_id: String, path: PathBuf, token: String) {
    UPLOADING.lock().unwrap().insert(message_id.clone(), path.clone());
    let upload = backend.upload_voice(token, path.clone());
    // `run` starts it now; nothing waits for it.
    drop(backend.run(async move {
        match upload.await {
            Ok(()) => {
                UPLOADING.lock().unwrap().remove(&message_id);
                recorder::remove(&path);
            }
            Err(error) => eprintln!("voice message recording wasn't kept: {error}"),
        }
    }));
}

/// What the composer says when a recording wasn't sent: why, and that it's still here.
fn not_sent(error: &str) -> String {
    if error == NOTHING_HEARD {
        return format!("{NOTHING_HEARD} Delete it and record again.");
    }
    format!("{} Your recording is kept here: send it again, or delete it.", error.trim())
}

/// The newest of what's been heard, as much as fits beside the recording's clock.
fn last_words(words: &str) -> String {
    const KEEP: usize = 90;
    let words = words.trim();
    let count = words.chars().count();
    if count <= KEEP {
        return words.to_string();
    }
    let tail: String = words.chars().skip(count - KEEP).collect();
    let tail = tail.split_once(' ').map_or(tail.as_str(), |(_, rest)| rest);
    format!("…{tail}")
}

/// `voiceDraftProblem`
fn draft_problem(duration_ms: f64, size: u64) -> Option<&'static str> {
    if duration_ms < MIN_VOICE_MS {
        Some("Recording is too short.")
    } else if duration_ms > MAX_VOICE_MS + 1000. {
        Some("Recording is longer than 10 minutes.")
    } else if size == 0 || size > MAX_VOICE_BYTES {
        Some("Recording must be under 10 MB.")
    } else {
        None
    }
}

impl Conversation {
    pub(super) fn voice_disabled(&self, cx: &App) -> bool {
        self.show_send_action(cx)
            || self.sending
            || self.pending.is_some()
            || (self.session_id.is_some() && self.detail().is_none())
    }

    pub(super) fn start_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.request_voice(false, false, window, cx);
    }

    /// The screenshot the next voice message goes with (None for none).
    pub fn set_voice_screenshot(&mut self, screenshot: Option<crate::screenshot::Screenshot>) {
        self.voice_screenshot = screenshot;
    }

    /// A voice message is recording (the pet's "Listening").
    pub fn recording(&self) -> bool {
        matches!(self.voice, Some(VoiceState::Opening | VoiceState::Recording { .. }))
    }

    /// A key went down to record until it's let go: fn+⌃ (`dictate`: at once with microphone access, else after a
    /// moment, app.rs) for the message box, or the voice shortcut for a voice message.
    pub fn hold_voice(&mut self, dictate: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice.is_some() {
            return;
        }
        if !dictate && self.show_send_action(cx) {
            self.hint = Some("Clear the message box to record a voice message.".into());
            cx.notify();
            return;
        }
        self.request_voice(true, dictate, window, cx);
    }

    /// The held key let go: fn+⌃'s words go into the message box, a voice message is sent, and a press too short to
    /// be either is dropped.
    pub fn release_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.voice_held) {
            return;
        }
        match &self.voice {
            Some(VoiceState::Opening) => self.cancel_voice(cx),
            Some(VoiceState::Recording { recorder, .. }) if recorder.elapsed_ms() < MIN_VOICE_MS => {
                self.cancel_voice(cx)
            }
            Some(VoiceState::Recording { .. }) if self.dictating => self.dictate(window, cx),
            Some(VoiceState::Recording { .. }) => self.send_voice(window, cx),
            _ => {}
        }
        self.focus_composer(window, cx);
    }

    /// fn+⌃ let go: what was said goes into the message box at the caret once it's transcribed (`type_dictated`), and
    /// the recording goes. Nothing is sent. Without words, the composer says why.
    fn dictate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.voice_held = false;
        self.dictating = false;
        let Some(VoiceState::Recording { recorder, live, .. }) = self.voice.take() else { return };
        self.voice_screenshot = None;
        cx.notify();
        // Transcribed as one held from another app is: the live transcript, else the file through the relay.
        let voice = match (AwayRecording { recorder, live }).finish() {
            Ok(voice) => voice,
            Err(problem) => {
                self.hint = Some(problem);
                return;
            }
        };
        let backend = backend(cx);
        cx.spawn_in(window, async move |this, cx| {
            let words = voice.words(&backend).await;
            this.update_in(cx, |this, window, cx| {
                match words {
                    Ok(words) => {
                        this.type_dictated(&words, window, cx);
                        voice.log_timing();
                    }
                    Err(problem) => this.hint = Some(problem),
                }
                cx.notify();
            })
            .ok();
            voice.discard();
        })
        .detach();
    }

    /// `words` in the message box at the caret (over what's selected), as if typed, with a space between them and the
    /// words beside them; the caret after them.
    pub fn type_dictated(&mut self, words: &str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::EntityInputHandler as _;
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            let units: Vec<u16> = input.value().encode_utf16().collect();
            let end = units.len();
            let range = input.selected_text_range(false, window, cx).map_or(end..end, |selection| selection.range);
            let before = String::from_utf16_lossy(&units[..range.start.min(end)]).chars().next_back();
            let after = String::from_utf16_lossy(&units[range.end.min(end)..]).chars().next();
            // As typed (so the composer hears of the change and keeps the draft).
            input.replace_text_in_range(None, &crate::dictation::spaced(words, before, after), window, cx);
        });
        cx.notify();
    }

    /// Esc while holding the key.
    pub fn cancel_held_voice(&mut self, cx: &mut Context<Self>) -> bool {
        if self.voice_held && self.recording() {
            self.cancel_voice(cx);
            return true;
        }
        false
    }

    /// The window lost focus mid-hold, so the release won't arrive: fn+⌃'s words go into the message box, and a voice
    /// message's recording is kept to send or delete.
    pub fn pause_held_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.voice_held {
            self.stop_voice(window, cx);
        }
    }

    /// Whether a recording can start: one for the message box needs only the microphone free, a voice message an
    /// empty composer with nothing on its way.
    fn may_record(&self, dictate: bool, cx: &App) -> bool {
        self.voice.is_none() && (dictate || !self.voice_disabled(cx))
    }

    fn request_voice(&mut self, hold: bool, dictate: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may_record(dictate, cx) {
            return;
        }
        if !recorder::in_app_bundle() && recorder::permission() != Permission::Granted {
            self.hint = Some(NOT_AN_APP.into());
            cx.notify();
            return;
        }
        match recorder::permission() {
            Permission::Granted => {
                self.voice_held = hold;
                self.dictating = hold && dictate;
                self.begin_recording(dictate, window, cx);
            }
            Permission::Undetermined => {
                let answer = recorder::request_permission();
                cx.spawn_in(window, async move |this, cx| {
                    let granted = answer.await.unwrap_or(false);
                    this.update_in(cx, |this, window, cx| {
                        // The keys were let go for the prompt: don't record behind the user's back.
                        if granted && hold {
                            this.hint = Some("Microphone ready. Hold fn ⌃ again to record.".into());
                            cx.notify();
                        } else if granted {
                            this.begin_recording(false, window, cx);
                        } else {
                            this.hint = Some(MIC_OFF.into());
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Permission::Denied => {
                self.hint = Some(MIC_OFF.into());
                let _ = open::that("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone");
                cx.notify();
            }
        }
    }

    /// Shows the recording row at once; the microphone opens behind it, as macOS takes a moment (0.1–0.3 s) to open
    /// it and the window doesn't draw meanwhile.
    fn begin_recording(&mut self, dictate: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may_record(dictate, cx) {
            return;
        }
        self.voice = Some(VoiceState::Opening);
        self.hint = None;
        // The text field leaves the screen for the recording row; focus left on it
        // would point nowhere, and key events (the fn+⌃ release, Esc) would be lost.
        self.focus.focus(window);
        cx.notify();
        // Frame callbacks run before the frame draws: the next one is the first after the row is drawn.
        let asked = Instant::now();
        cx.on_next_frame(window, move |_, window, cx| {
            cx.on_next_frame(window, move |this, window, cx| this.open_microphone(asked, window, cx))
        });
    }

    fn open_microphone(&mut self, asked: Instant, window: &mut Window, cx: &mut Context<Self>) {
        // Let go, cancelled or sent before it opened.
        if !matches!(self.voice, Some(VoiceState::Opening)) {
            return;
        }
        media::stop(cx);
        // The stream first: the words it hears are the message.
        let opening = Instant::now();
        let live = start_live(&backend(cx), asked);
        match Recorder::start() {
            Ok(recorder) => {
                eprintln!(
                    "voice: recording {} ms after it was asked for (the microphone took {} ms to open)",
                    asked.elapsed().as_millis(),
                    opening.elapsed().as_millis()
                );
                self.voice = Some(VoiceState::Recording {
                    recorder,
                    live,
                    started: Instant::now(),
                    levels: VecDeque::with_capacity(LEVELS),
                });
            }
            Err(_) => {
                self.voice = None;
                self.voice_held = false;
                self.dictating = false;
                self.hint = Some("Microphone could not start. Check permission and try again.".into());
            }
        }
        // Meter and clock; stops itself with the recording.
        self._voice_tick = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let recording = this
                    .update_in(cx, |this, window, cx| {
                        let Some(VoiceState::Recording { recorder, live, started, levels }) = &mut this.voice else {
                            return false;
                        };
                        if levels.len() == LEVELS {
                            levels.pop_front();
                        }
                        levels.push_back(recorder.level());
                        let elapsed = recorder.elapsed_ms();
                        let heard = live.as_ref().map(|live| live.transcription.heard()).unwrap_or_default();
                        // At the limit, or the input went away (a headset unplugged): keep what was recorded.
                        if elapsed >= MAX_VOICE_MS
                            || (!recorder.is_recording() && started.elapsed() > Duration::from_secs(1))
                        {
                            this.stop_voice(window, cx);
                        } else if let Some(problem) = heard.stopped {
                            // The relay's allowance ran out midway: what it heard until then is ready to send (or
                            // in the message box).
                            let dictating = this.dictating;
                            this.stop_voice(window, cx);
                            this.hint = Some(if dictating {
                                format!("{} What you said until now is in the message box.", problem.message())
                            } else {
                                format!("{} What you said until now is ready to send.", problem.message())
                            });
                        } else if let Some(problem) = heard.problem.as_ref().filter(|problem| problem.lasting()) {
                            // Refused (voice messages used up, or switched off): nothing could send it before then.
                            this.cancel_voice(cx);
                            this.hint = Some(problem.message());
                        } else if let Some(problem) = heard.problem.filter(|_| this.hint.is_none()) {
                            // Offline, say: the recording goes on, to be turned into text when it's sent.
                            this.hint = Some(format!(
                                "{} Keep going: the recording is kept, and Send tries again.",
                                problem.message()
                            ));
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !recording {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Recording → Preview, if what was recorded can be sent; fn+⌃'s goes into the message box instead.
    pub(super) fn stop_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dictating && matches!(self.voice, Some(VoiceState::Recording { .. })) {
            self.dictate(window, cx);
            return;
        }
        self.voice_held = false;
        self.dictating = false;
        let Some(VoiceState::Recording { recorder, live, .. }) = self.voice.take() else { return };
        // The relay hears it's over first (`end_live`); the transcript settles while the user listens or reaches for
        // Send. One too short to send is dropped.
        let live = live.filter(|_| recorder.elapsed_ms() >= MIN_VOICE_MS);
        let transcript = end_live(&recorder, live).map(|(transcript, _)| transcript);
        let (path, duration_ms) = recorder.finish();
        let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        match draft_problem(duration_ms, size) {
            Some(problem) => {
                recorder::remove(&path);
                self.hint = Some(if duration_ms < MIN_VOICE_MS {
                    problem.into()
                } else {
                    format!("{problem} Please record between 1 second and 10 minutes.")
                });
            }
            None => self.voice = Some(VoiceState::Preview { path, duration_ms, transcript }),
        }
        cx.notify();
    }

    pub(crate) fn cancel_voice(&mut self, cx: &mut Context<Self>) {
        self.voice_held = false;
        self.dictating = false;
        match self.voice.take() {
            Some(VoiceState::Recording { recorder, .. }) => {
                recorder.cancel();
                self.voice_screenshot = None;
            }
            Some(VoiceState::Preview { path, .. }) => {
                media::stop(cx);
                recorder::remove(&path);
                self.voice_receipt = None;
                self.voice_screenshot = None;
            }
            // Already on its way.
            sending @ Some(VoiceState::Sending { .. }) => self.voice = sending,
            Some(VoiceState::Opening) | None => {}
        }
        self.hint = None;
        cx.notify();
    }

    /// Send from recording or preview.
    pub(super) fn send_voice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.voice, Some(VoiceState::Recording { .. })) {
            self.stop_voice(window, cx);
        }
        let Some(VoiceState::Preview { path, duration_ms, transcript }) = self.voice.take() else { return };
        if self.sending || self.pending.is_some() {
            self.voice = Some(VoiceState::Preview { path, duration_ms, transcript });
            return;
        }
        media::stop(cx);
        // A retry keeps the reply it was first sent with.
        let retry = self.voice_receipt.as_ref().filter(|receipt| receipt.path == path);
        let reply = match retry {
            Some(receipt) => receipt.reply.clone(),
            None => self.reply_to.take(),
        };
        let request_id = retry
            .and_then(|receipt| receipt.upload_id.clone().or_else(|| Some(receipt.request_id.clone())))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let visible_ids =
            self.detail().map(|detail| detail.messages.iter().map(|m| m.id.clone()).collect()).unwrap_or_default();
        let mut pending = PendingSend::new(request_id.clone(), String::new(), reply.clone(), visible_ids);
        pending.voice_ms = Some(duration_ms);
        self.pending = Some(pending);
        self.voice = Some(VoiceState::Sending { path: path.clone(), duration_ms });
        self.sending = true;
        self.hint = None;
        self.rebuild(cx);
        self.show_sent();
        // The composer is back and empty: ready to type the next message.
        self.focus_composer(window, cx);
        cx.notify();

        let backend = backend(cx);
        let existing = self.session_id.clone();
        let screenshot = self.voice_screenshot.clone();
        let context = self.send_context.clone();
        let context_note = context.as_ref().map(|(_, note)| note.clone());
        // Shared notes and tasks stay on the composer until the message has gone, so a failed send keeps them.
        let notes = self.shared_notes.clone();
        let work = self.shared_work.clone();
        cx.spawn_in(window, async move |this, cx| {
            // The screenshot it goes with, uploaded while the user talked (a failed one is left out).
            let attachments = match screenshot {
                Some(screenshot) => screenshot.await.map(|shot| json!([shot])),
                None => None,
            };
            // The turn id, and whether the recording is still uploading behind the message.
            let result: Result<(String, bool), String> = async {
                // The conversation, created on a draft's first send.
                let known = this
                    .update(cx, |this, _| this.voice_receipt.as_ref().filter(|r| r.path == path).map(|r| r.session_id.clone()))
                    .ok()
                    .flatten();
                // Every chat has its conversation before it shows (an agent's opens it first, `agents:openChat`).
                let session_id = known.or(existing).ok_or("This conversation isn't ready yet.")?;
                this.update(cx, |this, _| {
                    if this.voice_receipt.as_ref().is_none_or(|r| r.path != path) {
                        this.voice_receipt = Some(VoiceReceipt {
                            path: path.clone(),
                            request_id: request_id.clone(),
                            session_id: session_id.clone(),
                            reply: reply.clone(),
                            upload_id: None,
                            token: None,
                            uploaded: false,
                        });
                    }
                })
                .ok();
                let receipt = |cx: &mut gpui::AsyncWindowContext| {
                    this.update(cx, |this, _| {
                        this.voice_receipt.as_ref().map(|r| (r.upload_id.clone(), r.token.clone(), r.uploaded))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or((None, None, false))
                };

                // The live transcript settles a moment after recording stops. With it the
                // message goes out now and the recording is kept behind it; without it
                // the recording is kept first and the backend transcribes it.
                let live_text = match (&transcript, receipt(cx).0) {
                    (Some(transcript), None) => transcript.clone().await.ok(),
                    _ => None,
                };
                if let Some(text) = live_text {
                    let request_id = this
                        .update(cx, |this, _| this.voice_receipt.as_ref().map(|r| r.request_id.clone()))
                        .ok()
                        .flatten()
                        .ok_or("Voice message was not sent")?;
                    let mut args = json!({ "sessionId": session_id, "requestId": request_id, "durationMs": duration_ms.round(), "transcript": text });
                    if let Some(reply) = &reply {
                        args["replyToMessageId"] = json!(reply.message_id);
                    }
                    if let Some(attachments) = &attachments {
                        args["attachments"] = attachments.clone();
                    }
                    if let Some(note) = &context_note {
                        args["contextNote"] = json!(note);
                    }
                    if !notes.is_empty() {
                        args["sharedNotes"] = note_share::args(&notes);
                    }
                    if !work.is_empty() {
                        args["sharedWork"] = work_share::args(&work);
                    }
                    let sent = backend.call_raw("voiceRecordings:sendLive", args).await?;
                    let field = |key: &str| sent.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
                    // Registered before the pending row hands over, so the sent bubble plays the local file.
                    upload_in_background(&backend, field("messageId"), path.clone(), field("token"));
                    this.update(cx, |this, cx| {
                        if let Some(pending) = &mut this.pending {
                            pending.request_id = field("uploadId");
                        }
                        this.rebuild(cx);
                    })
                    .ok();
                    return Ok((field("turnId"), true));
                }

                // An upload ticket; its id is the turn's request id.
                let (upload_id, token) = match receipt(cx) {
                    (Some(id), Some(token), _) => (id, token),
                    _ => {
                        let ticket = backend
                            .call_raw("voiceRecordings:createUpload", json!({ "sessionId": session_id, "durationMs": duration_ms.round() }))
                            .await?;
                        let id = ticket.get("uploadId").and_then(Value::as_str).ok_or("Voice upload failed")?.to_string();
                        let token = ticket.get("token").and_then(Value::as_str).ok_or("Voice upload failed")?.to_string();
                        this.update(cx, |this, cx| {
                            if let Some(r) = &mut this.voice_receipt {
                                r.upload_id = Some(id.clone());
                                r.token = Some(token.clone());
                            }
                            if let Some(pending) = &mut this.pending {
                                pending.request_id = id.clone();
                            }
                            this.rebuild(cx);
                        })
                        .ok();
                        (id, token)
                    }
                };

                if !receipt(cx).2 {
                    backend.upload_voice(token, path.clone()).await?;
                    this.update(cx, |this, _| {
                        if let Some(r) = &mut this.voice_receipt {
                            r.uploaded = true;
                        }
                    })
                    .ok();
                }

                let mut args = json!({ "uploadId": upload_id });
                if let Some(reply) = &reply {
                    args["replyToMessageId"] = json!(reply.message_id);
                }
                if let Some(attachments) = &attachments {
                    args["attachments"] = attachments.clone();
                }
                if let Some(note) = &context_note {
                    args["contextNote"] = json!(note);
                }
                if !notes.is_empty() {
                    args["sharedNotes"] = note_share::args(&notes);
                }
                if !work.is_empty() {
                    args["sharedWork"] = work_share::args(&work);
                }
                let sent = backend.call_raw("voiceRecordings:send", args).await?;
                Ok((sent.get("turnId").and_then(Value::as_str).unwrap_or_default().to_string(), false))
            }
            .await;

            this.update(cx, |this, cx| {
                this.sending = false;
                match result {
                    Ok((turn_id, uploading)) => {
                        if !uploading {
                            recorder::remove(&path);
                        }
                        if let Some((key, _)) = context {
                            cx.emit(ConversationEvent::ContextSent(key));
                        }
                        this.shared_notes.retain(|note| !notes.contains(note));
                        this.shared_work.retain(|task| !work.iter().any(|sent| sent.key() == task.key()));
                        this.voice = None;
                        this.voice_receipt = None;
                        this.voice_screenshot = None;
                        if let Some(pending) = &mut this.pending {
                            pending.turn_id = (!turn_id.is_empty()).then_some(turn_id);
                        }
                    }
                    Err(error) => {
                        // Keep the recording to send again.
                        this.pending = None;
                        this.reply_to = reply;
                        this.voice = Some(VoiceState::Preview { path, duration_ms, transcript });
                        this.hint = Some(not_sent(&error));
                    }
                }
                this.data_changed(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Records the text row's height (see `text_row_height`).
    pub(super) fn measure_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        gpui::canvas(
            move |bounds, _, cx| {
                let height = f32::from(bounds.size.height);
                view.update(cx, |this, cx| {
                    if (height - this.text_row_height).abs() > 0.5 && this.input.read(cx).value().is_empty() {
                        this.text_row_height = height;
                    }
                })
                .ok();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    pub(super) fn mic_button(&self, cx: &mut Context<Self>) -> AnyElement {
        IconButton::new("composer-mic", "mic")
            .small()
            .tooltip("Record a voice message")
            .disabled(self.voice_disabled(cx))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.start_voice(window, cx)))
            .into_any_element()
    }

    /// The composer's row while recording or holding a recording, in place of the text field.
    pub(super) fn render_voice_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = *cx.colors();
        // Once sent, the recording shows as the pending bubble in the chat and the
        // composer is empty again (it comes back here if the send fails).
        let voice = self.voice.as_ref().filter(|voice| !matches!(voice, VoiceState::Sending { .. }))?;
        let delete = IconButton::new("voice-delete", "trash-2")
            .small()
            .color(colors.danger)
            .tooltip("Delete recording")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_voice(cx)));
        let send = IconButton::new("voice-send", "arrow-up")
            .primary()
            .small()
            .tooltip("Send")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send_voice(window, cx)));
        let row = div().flex().items_center().gap(px(space::XS)).px(px(space::XS)).h(px(self.text_row_height));
        let row = match voice {
            VoiceState::Opening | VoiceState::Recording { .. } => {
                let (elapsed, heard, levels) = match voice {
                    VoiceState::Recording { recorder, live, levels, .. } => (
                        recorder.elapsed_ms(),
                        live.as_ref().map(|live| live.transcription.heard()).unwrap_or_default(),
                        Some(levels),
                    ),
                    _ => (0., Heard::default(), None),
                };
                // The dot blinks each half second, like a recording light.
                let lit = (elapsed / 500.) as i64 % 2 == 0;
                row.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .px(px(space::SM))
                        .child(div().size(px(8.)).rounded(px(radius::FULL)).bg(colors.danger).opacity(if lit {
                            1.
                        } else {
                            0.35
                        }))
                        .child(div().callout().medium().child(elapsed_clock(elapsed))),
                )
                .when(self.voice_held && heard.words.trim().is_empty(), |this| {
                    this.child(div().flex_none().footnote().text_color(colors.text_secondary).child(
                        if self.dictating {
                            "Release to write it in the message box · Esc to cancel"
                        } else {
                            "Release to send · Esc to cancel"
                        },
                    ))
                })
                .child(match heard.words.trim().is_empty() {
                    // The words so far: the newest stay in view, the oldest go off the left.
                    false => div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .justify_end()
                        .child(
                            div()
                                .flex_none()
                                .whitespace_nowrap()
                                .callout()
                                .text_color(colors.text_secondary)
                                .child(last_words(&heard.words)),
                        )
                        .into_any_element(),
                    true => div()
                        .flex_1()
                        .min_w_0()
                        .h(px(28.))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(px(2.))
                        .children(levels.into_iter().flatten().map(|level| {
                            div()
                                .flex_none()
                                .w(px(3.))
                                .h(px(3. + level * 21.))
                                .rounded(px(2.))
                                .bg(colors.text_secondary)
                        }))
                        .into_any_element(),
                })
                .child(
                    IconButton::new("voice-stop", "square")
                        .small()
                        .tooltip("Stop and listen")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.stop_voice(window, cx))),
                )
            }
            VoiceState::Preview { path, duration_ms, .. } | VoiceState::Sending { path, duration_ms } => {
                row.child(div().flex_1().min_w_0().px(px(space::XS)).child(render_voice_sized(
                    "draft",
                    Some(format!("file://{}", path.display())),
                    *duration_ms,
                    "Voice message",
                    false,
                    28.,
                    cx,
                )))
            }
        };
        // Delete sits next to Send.
        Some(row.child(delete).child(send).into_any_element())
    }
}

/// A voice message held from another app (the push-to-talk shortcut, app.rs `talk`), recorded outside any chat: which
/// chat it goes to is picked once it's transcribed (voice_route.rs).
pub struct AwayRecording {
    recorder: Recorder,
    live: Option<Live>,
}

/// A held voice message let go: the recording, its length, and its transcript as it settles (none without live
/// transcription: `AwayVoice::words` sends the recording through the relay).
pub struct AwayVoice {
    pub path: PathBuf,
    pub duration_ms: f64,
    pub transcript: Option<Transcript>,
    marks: Option<Arc<Mutex<Marks>>>,
}

impl AwayRecording {
    /// Starts recording for keys that went down at `asked`, or says why it can't. The first time, macOS is asked for
    /// the microphone and nothing records: the keys were let go for its prompt.
    pub fn start(backend: &Backend, asked: Instant) -> Result<AwayRecording, String> {
        if !recorder::in_app_bundle() && recorder::permission() != Permission::Granted {
            return Err(NOT_AN_APP.into());
        }
        match recorder::permission() {
            Permission::Granted => {}
            Permission::Undetermined => {
                drop(recorder::request_permission());
                return Err("Microphone access asked for".into());
            }
            Permission::Denied => return Err(MIC_OFF.into()),
        }
        let recorder = Recorder::start().map_err(|_| "Microphone could not start.".to_string())?;
        Ok(AwayRecording { recorder, live: start_live(backend, asked) })
    }

    pub fn cancel(self) {
        self.recorder.cancel();
    }

    /// How long it's been recording.
    pub fn elapsed_ms(&self) -> f64 {
        self.recorder.elapsed_ms()
    }

    /// Whether the microphone still records (it stops when the input goes away, a headset unplugged).
    pub fn is_recording(&self) -> bool {
        self.recorder.is_recording()
    }

    /// What the relay has heard so far, as it changes; none without live transcription.
    pub fn heard(&self) -> Option<tokio::sync::watch::Receiver<Heard>> {
        self.live.as_ref().map(|live| live.transcription.watch())
    }

    /// Stops recording: what was recorded, or why it can't be sent (too short: nothing worth saying).
    pub fn finish(self) -> Result<AwayVoice, String> {
        let AwayRecording { recorder, live } = self;
        if recorder.elapsed_ms() < MIN_VOICE_MS {
            recorder.cancel();
            return Err(draft_problem(0., 0).unwrap_or_default().into());
        }
        let (transcript, marks) = end_live(&recorder, live).unzip();
        let (path, duration_ms) = recorder.finish();
        let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if let Some(problem) = draft_problem(duration_ms, size) {
            recorder::remove(&path);
            return Err(problem.into());
        }
        Ok(AwayVoice { path, duration_ms, transcript, marks })
    }
}

impl AwayVoice {
    /// What was said: the live transcript, else the recording sent through the relay afterwards (not when voice
    /// messages are used up or switched off: that would be refused too). Without words, what to tell the user.
    pub async fn words(&self, backend: &Backend) -> Result<String, String> {
        let live = match &self.transcript {
            Some(transcript) => transcript.clone().await,
            None => Err(Problem::Failed("no live transcription".into())),
        };
        let words = match live {
            Err(problem) if !problem.lasting() => {
                let (relay, path) = (Relay::new(&live_transcription::origin()), self.path.clone());
                backend.run(async move { live_transcription::transcribe_file(&relay, &path).await }).await
            }
            words => words,
        };
        match words {
            Ok(words) if words.trim().is_empty() => Err(NOTHING_HEARD.into()),
            Ok(words) => Ok(words),
            Err(problem) => Err(problem.message()),
        }
    }

    /// Logs how long each step took ("dictation: …", `Marks::line`), now that its words are in.
    pub fn log_timing(&self) {
        if let Some(marks) = &self.marks {
            eprintln!("dictation: {}", marks.lock().unwrap().line(Instant::now()));
        }
    }

    /// Not sent after all: the recording goes.
    pub fn discard(self) {
        recorder::remove(&self.path);
    }
}

/// Sends a voice message recorded from another app to `session_id` with what was said (`AwayVoice::words`), as the
/// composer's Send does: `voiceRecordings:sendLive`, and the recording kept behind it. There's no retry: the recording
/// goes either way.
pub async fn send_away(
    backend: Backend,
    session_id: String,
    voice: AwayVoice,
    text: String,
    attachments: Option<Value>,
) -> Result<(), String> {
    let AwayVoice { path, duration_ms, .. } = voice;
    let mut args = json!({
        "sessionId": session_id,
        "requestId": uuid::Uuid::new_v4().to_string(),
        "durationMs": duration_ms.round(),
        "transcript": text,
    });
    if let Some(attachments) = attachments {
        args["attachments"] = attachments;
    }
    match backend.call_raw("voiceRecordings:sendLive", args).await {
        Ok(sent) => {
            let field = |key: &str| sent.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
            upload_in_background(&backend, field("messageId"), path, field("token"));
            Ok(())
        }
        Err(error) => {
            recorder::remove(&path);
            Err(error)
        }
    }
}

/// Ends `live` as the keys are let go: the microphone's stream stops and the relay is told at once, before `recorder`
/// closes its file (which takes a moment), so the final words are on their way meanwhile.
fn end_live(recorder: &Recorder, live: Option<Live>) -> Option<(Transcript, Arc<Mutex<Marks>>)> {
    let Live { mic, transcription } = live?;
    let marks = transcription.marks();
    marks.lock().unwrap().let_go = Some(Instant::now());
    mic.stop();
    Some((transcription.finish(recorder.elapsed_ms()), marks))
}

/// Live transcription for a new recording whose keys went down at `asked`, or `None` (the recording is then
/// transcribed once it's kept) when the microphone can't be tapped.
fn start_live(backend: &Backend, asked: Instant) -> Option<Live> {
    let transcription = LiveTranscription::start(backend, asked);
    let sink = transcription.sink();
    match MicStream::start(live_transcription::SAMPLE_RATE, move |pcm| sink.push(pcm)) {
        Ok(mic) => Some(Live { mic, transcription }),
        Err(error) => {
            eprintln!("live transcription off: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_rules_match_chat_protocol() {
        assert_eq!(elapsed_clock(7_900.), "0:07");
        assert_eq!(elapsed_clock(61_000.), "1:01");
        assert!(draft_problem(200., 1000).is_some());
        assert!(draft_problem(5_000., 0).is_some());
        assert!(draft_problem(5_000., MAX_VOICE_BYTES + 1).is_some());
        assert!(draft_problem(5_000., 40_000).is_none());
    }

    #[test]
    fn the_newest_words_show_while_recording() {
        assert_eq!(last_words("  Milo, check the build  "), "Milo, check the build");
        let long = "one two three four five six seven eight nine ten ".repeat(4);
        let shown = last_words(&long);
        assert!(shown.starts_with('…') && shown.ends_with("nine ten") && shown.chars().count() <= 91, "{shown}");
        // From a word's start.
        assert!(
            shown.trim_start_matches('…').split(' ').all(|word| long.split(' ').any(|whole| whole == word)),
            "{shown}"
        );
        assert_eq!(
            not_sent("You're offline."),
            "You're offline. Your recording is kept here: send it again, or delete it."
        );
    }
}

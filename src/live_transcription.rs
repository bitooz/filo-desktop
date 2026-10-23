// Voice messages turned into text by Filo's relay at dev.filo.im (Filo's Worker, outside this repository), which
// passes the audio on to Microsoft's MAI-Transcribe-2-Streaming. Filo for Mac has no account, so it opens as a desktop
// (`{"type":"start","client":"desktop",…}`) rather than with a sign-in token, and the relay holds it to a daily
// allowance per network and for every Mac together.
//
// While the user records, microphone PCM (16 kHz mono PCM16, recorder::MicStream) streams to it, and the words it
// hears come back as they're said (`Heard`, shown with the recording). When recording stops, `finish` commits and
// resolves with the final transcript a fraction of a second later, which goes as the message (voiceRecordings:sendLive).
// If the stream failed, the saved recording goes through the relay afterwards instead (`transcribe_file`,
// voiceRecordings:send). An allowance used up, the relay switched off, or no connection is a `Problem`, which says what
// happened and when voice messages are back.
//
// Connecting and the relay's start (it books the recording's share of the day, then opens the speech service) take
// 1–3 s, which a short recording would otherwise wait out after it's let go. So a session is kept open ahead of the
// next recording (`keep_warm`, `SPARE`) while Filo's window is in front and after each recording, and a recording
// takes it. Each one's steps are timed (`Marks`) and logged once its words are in ("dictation: …").

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::{FutureExt as _, LocalBoxFuture, Shared};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::backend::Backend;

pub const SAMPLE_RATE: u32 = 16_000;
const BYTES_PER_MS: f64 = SAMPLE_RATE as f64 * 2. / 1000.;
/// The relay's own origin (`origin`).
const DEFAULT_ORIGIN: &str = "https://dev.filo.im";
/// How long `finish` waits for the final transcript before giving up on it.
const FINAL_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A saved recording goes up in pieces this long, this many times faster than it was said, so the speech service keeps
/// up and the final comes soon after the last piece (the relay waits 15 s for it).
const FILE_CHUNK_MS: f64 = 1_000.;
const FILE_SPEED: f64 = 4.;
/// A microphone that only ever produced near-zero samples was not really streaming.
const SILENT_PEAK: i16 = 16;
/// The streamed audio may fall this far short of the recording before it is distrusted.
const MAX_MISSING_MS: f64 = 1_500.;
/// A chunk this loud (and at least a sixth of the loudest so far) is speech, for `Marks::last_speech`.
const SPEECH_PEAK: i16 = 800;
/// How long a session opened ahead waits for a recording. The relay ends a session 12 minutes after its start and a
/// recording runs up to 10; Cloudflare drops a WebSocket that's quiet for 100 s.
const SPARE_FOR: Duration = Duration::from_secs(90);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Why a recording couldn't be turned into text.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// An allowance is used up: this network's for the day ("ip"), every Mac's ("global"), or this recording's own
    /// ("session"); back at `reset_at` (ms since the epoch) when the relay says.
    Limit { scope: String, reset_at: Option<f64> },
    /// The relay has voice messages switched off.
    Disabled,
    /// The relay couldn't be reached.
    Offline,
    /// Anything else, in a few words for the message and the log.
    Failed(String),
}

impl Problem {
    /// What the user is told.
    pub fn message(&self) -> String {
        self.message_at(crate::format::now_ms())
    }

    fn message_at(&self, now: f64) -> String {
        match self {
            Problem::Limit { scope, reset_at } => {
                let back = reset_at.map(|at| format!(" They're back {}.", back_at(at, now))).unwrap_or_default();
                match scope.as_str() {
                    "session" => "This recording is as long as a voice message can be.".into(),
                    "global" => format!("Voice messages have run out for everyone today.{back}"),
                    _ => format!("You've used up today's voice messages.{back}"),
                }
            }
            Problem::Disabled => "Voice messages are switched off for now. Type your message instead.".into(),
            Problem::Offline => "You're offline, so the recording couldn't be turned into text.".into(),
            Problem::Failed(why) => format!("The recording couldn't be turned into text ({why})."),
        }
    }

    /// Trying again now won't help: not before the allowance comes back, or the relay is switched on again.
    pub fn lasting(&self) -> bool {
        matches!(self, Problem::Limit { .. } | Problem::Disabled)
    }
}

/// "at 5:00 AM" later today, "tomorrow at 5:00 AM", else "on Thu, Oct 15 at 5:00 AM".
fn back_at(at: f64, now: f64) -> String {
    let (then, today) = (crate::format::local(at), crate::format::local(now));
    let clock = crate::format::clock(at);
    if crate::format::same_day(then, today) {
        format!("at {clock}")
    } else if crate::format::same_day(then, today + chrono::Duration::days(1)) {
        format!("tomorrow at {clock}")
    } else {
        format!("on {} at {clock}", then.format("%a, %b %-d"))
    }
}

/// What the relay has heard of a live recording so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heard {
    /// The words so far.
    pub words: String,
    /// The relay stopped taking audio (an allowance ran out midway) and is writing out what it heard: stop recording.
    pub stopped: Option<Problem>,
    /// Live transcription failed, and why.
    pub problem: Option<Problem>,
}

/// The final transcript (empty when nothing was said), or why there's none: then the recording itself is transcribed
/// (`transcribe_file`). Shared so a retried send reuses it.
pub type Transcript = Shared<LocalBoxFuture<'static, Result<String, Problem>>>;

/// When a recording's steps happened, from fn+⌃ (or the mic) going down to its words being in.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    /// The keys went down (the session may have been opened before: `opened`).
    pub asked: Option<Instant>,
    opened: Option<Instant>,
    connected: Option<Instant>,
    ready: Option<Instant>,
    first_words: Option<Instant>,
    /// The last chunk loud enough to be speech reached this Mac's stream.
    last_speech: Option<Instant>,
    /// The keys were let go (`LiveTranscription::finish`).
    pub let_go: Option<Instant>,
    committed: Option<Instant>,
    final_words: Option<Instant>,
}

impl Marks {
    /// One line for the log once the words are in at `typed`: each step in ms from the keys going down, or going up.
    pub fn line(&self, typed: Instant) -> String {
        let since = |from: Option<Instant>, to: Option<Instant>| match (from, to) {
            (Some(from), Some(to)) => format!("{} ms", to.saturating_duration_since(from).as_millis()),
            _ => "–".into(),
        };
        let warm = match (self.opened, self.asked) {
            (Some(opened), Some(asked)) if opened < asked => {
                format!(" (opened {:.0} s before)", asked.duration_since(opened).as_secs_f64())
            }
            _ => String::new(),
        };
        format!(
            "key down → connected {}, ready {}{warm}, first words {} · last word → final {} · key up → commit {}, final {}, typed {}",
            since(self.asked, self.connected.zip(self.asked).map(|(at, asked)| at.max(asked))),
            since(self.asked, self.ready.zip(self.asked).map(|(at, asked)| at.max(asked))),
            since(self.asked, self.first_words),
            since(self.last_speech, self.final_words),
            since(self.let_go, self.committed),
            since(self.let_go, self.final_words),
            since(self.let_go, Some(typed)),
        )
    }
}

/// Where Filo's relay is: dev.filo.im, or FILO_WORKER_ORIGIN in `dev` builds.
pub fn origin() -> String {
    crate::test_run::dev_override("FILO_WORKER_ORIGIN").unwrap_or_else(|| DEFAULT_ORIGIN.into())
}

/// `voiceTranscribeUrl`
fn socket_url(worker_origin: &str) -> String {
    let origin = worker_origin.trim_end_matches('/');
    let origin = match origin.strip_prefix("http://") {
        Some(rest) => format!("ws://{rest}"),
        None => format!("wss://{}", origin.strip_prefix("https://").unwrap_or(origin)),
    };
    format!("{origin}/v1/voice-transcribe")
}

/// Filo's relay.
#[derive(Clone, Debug)]
pub struct Relay {
    url: String,
}

impl Relay {
    pub fn new(worker_origin: &str) -> Relay {
        Relay { url: socket_url(worker_origin) }
    }

    /// What this Mac tells it as a recording starts: that it's Filo for Mac, and which build. Nothing that identifies
    /// the Mac or its user (the relay counts its daily allowance by network).
    fn start_message(&self) -> Value {
        json!({ "type": "start", "client": "desktop", "version": crate::updater::VERSION, "build": crate::updater::build_number() })
    }

    /// The socket, started.
    async fn connect(&self) -> Result<Socket, Problem> {
        let (mut socket, _) =
            tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(self.url.as_str()))
                .await
                .map_err(|_| Problem::Offline)?
                .map_err(|error| match error {
                    tungstenite::Error::Io(_) => Problem::Offline,
                    tungstenite::Error::Http(response) => {
                        Problem::Failed(format!("the relay answered {}", response.status()))
                    }
                    other => Problem::Failed(other.to_string()),
                })?;
        socket.send(Message::text(self.start_message().to_string())).await.map_err(stream_problem)?;
        Ok(socket)
    }
}

/// A connection lost midway.
fn stream_problem(error: tungstenite::Error) -> Problem {
    match error {
        tungstenite::Error::Io(_) => Problem::Offline,
        other => Problem::Failed(other.to_string()),
    }
}

/// What the relay says.
#[derive(Debug, PartialEq)]
enum Reply {
    Ready,
    Partial(String),
    Final(String),
    /// An allowance ran out midway: it stops taking audio and sends the final for what it heard.
    Limit(Problem),
    Error(Problem),
    Other,
}

fn reply(text: &str) -> Reply {
    let message: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let field = |key: &str| message.get(key).and_then(Value::as_str);
    let limit = || Problem::Limit {
        scope: field("scope").unwrap_or("ip").to_string(),
        reset_at: message.get("resetAt").and_then(Value::as_f64),
    };
    match field("type") {
        Some("ready") => Reply::Ready,
        Some("partial") => Reply::Partial(field("text").unwrap_or_default().to_string()),
        Some("final") => Reply::Final(field("text").unwrap_or_default().trim().to_string()),
        Some("limit") => Reply::Limit(limit()),
        Some("error") => Reply::Error(match field("code") {
            Some("usage_limit") => limit(),
            Some("disabled") => Problem::Disabled,
            code => Problem::Failed(field("message").or(code).unwrap_or("the relay failed").to_string()),
        }),
        _ => Reply::Other,
    }
}

enum Command {
    Audio(Vec<u8>),
    Finish { recorded_ms: f64 },
    Cancel,
}

pub struct LiveTranscription {
    commands: mpsc::UnboundedSender<Command>,
    heard: watch::Receiver<Heard>,
    task: Option<tokio::task::JoinHandle<Result<String, Problem>>>,
    rt: tokio::runtime::Handle,
    marks: Arc<Mutex<Marks>>,
}

/// The session opened ahead of the next recording, and when (`warm`).
static SPARE: Mutex<Option<(u64, LiveTranscription)>> = Mutex::new(None);
static SPARES: AtomicU64 = AtomicU64::new(0);
/// The app keeps spares (`keep_warm`): never in a check or a test.
static WARMING: AtomicBool = AtomicBool::new(false);
/// Filo's window is in front: a spare that runs out is replaced.
static IN_FRONT: AtomicBool = AtomicBool::new(false);

/// Filo's window came to the front (`front`) or left it: in front, a session is kept open for the next recording.
pub fn keep_warm(front: bool, backend: &Backend) {
    if crate::test_run::active() {
        return;
    }
    WARMING.store(true, Ordering::Relaxed);
    IN_FRONT.store(front, Ordering::Relaxed);
    if front {
        warm(Relay::new(&origin()), backend.runtime());
    }
}

/// Opens a session for the next recording, unless one is open: kept `SPARE_FOR`, then closed, and replaced while
/// Filo's window is in front.
pub fn warm(relay: Relay, rt: tokio::runtime::Handle) {
    let mut spare = SPARE.lock().unwrap();
    if spare.as_ref().is_some_and(|(_, live)| live.usable()) {
        return;
    }
    let id = SPARES.fetch_add(1, Ordering::Relaxed);
    *spare = Some((id, LiveTranscription::start_with(relay.clone(), rt.clone())));
    rt.clone().spawn(async move {
        tokio::time::sleep(SPARE_FOR).await;
        let mut spare = SPARE.lock().unwrap();
        if spare.as_ref().is_some_and(|(spare, _)| *spare == id) {
            *spare = None;
            if IN_FRONT.load(Ordering::Relaxed) {
                drop(spare);
                warm(relay, rt);
            }
        }
    });
}

/// A recording's session is done: another recording may well follow, so its session opens now (once the app keeps
/// spares).
fn warm_next(rt: tokio::runtime::Handle) {
    if WARMING.load(Ordering::Relaxed) {
        warm(Relay::new(&origin()), rt);
    }
}

/// The open spare, if it's still good.
fn take_spare() -> Option<LiveTranscription> {
    SPARE.lock().unwrap().take().map(|(_, live)| live).filter(LiveTranscription::usable)
}

/// Feeds microphone audio in from the audio thread.
#[derive(Clone)]
pub struct AudioSink(mpsc::UnboundedSender<Command>);

impl AudioSink {
    pub fn push(&self, pcm: Vec<u8>) {
        let _ = self.0.send(Command::Audio(pcm));
    }
}

impl LiveTranscription {
    /// The stream for a recording whose keys went down at `asked`: the spare opened for it (`warm`), else a new one.
    pub fn start(backend: &Backend, asked: Instant) -> LiveTranscription {
        LiveTranscription::take_or_start(backend.runtime(), asked)
    }

    fn take_or_start(rt: tokio::runtime::Handle, asked: Instant) -> LiveTranscription {
        let live = take_spare().unwrap_or_else(|| LiveTranscription::start_with(Relay::new(&origin()), rt));
        live.marks.lock().unwrap().asked = Some(asked);
        live
    }

    fn start_with(relay: Relay, rt: tokio::runtime::Handle) -> LiveTranscription {
        let (commands, mut rx) = mpsc::unbounded_channel();
        let (heard_tx, heard) = watch::channel(Heard::default());
        let marks = Arc::new(Mutex::new(Marks { opened: Some(Instant::now()), ..Marks::default() }));
        let task = rt.spawn({
            let marks = marks.clone();
            async move {
                let result = live(relay, &mut rx, &heard_tx, &marks).await;
                if let Err(problem) = &result {
                    eprintln!("live transcription: {problem:?}");
                    heard_tx.send_modify(|heard| heard.problem = Some(problem.clone()));
                }
                result
            }
        });
        LiveTranscription { commands, heard, task: Some(task), rt, marks }
    }

    /// Still connected (or connecting) and taking audio: a spare that can take a recording.
    fn usable(&self) -> bool {
        let heard = self.heard.borrow();
        self.task.as_ref().is_some_and(|task| !task.is_finished())
            && heard.problem.is_none()
            && heard.stopped.is_none()
            && self.marks.lock().unwrap().opened.is_some_and(|opened| opened.elapsed() < SPARE_FOR)
    }

    /// When its steps happened, filled in as they do.
    pub fn marks(&self) -> Arc<Mutex<Marks>> {
        self.marks.clone()
    }

    pub fn sink(&self) -> AudioSink {
        AudioSink(self.commands.clone())
    }

    /// What it has heard so far.
    pub fn heard(&self) -> Heard {
        self.heard.borrow().clone()
    }

    /// `heard`, as it changes.
    pub fn watch(&self) -> watch::Receiver<Heard> {
        self.heard.clone()
    }

    /// Ends the stream. `recorded_ms` is the saved recording's length, so a stream that dropped out midway is not
    /// trusted.
    pub fn finish(mut self, recorded_ms: f64) -> Transcript {
        self.marks.lock().unwrap().let_go.get_or_insert_with(Instant::now);
        let _ = self.commands.send(Command::Finish { recorded_ms });
        let task = self.task.take();
        let rt = self.rt.clone();
        let waited = self.rt.spawn(async move {
            let mut task = task.ok_or_else(|| Problem::Failed("live transcription ended".into()))?;
            let result = match tokio::time::timeout(FINAL_TIMEOUT, &mut task).await {
                Ok(joined) => joined.unwrap_or_else(|_| Err(Problem::Failed("live transcription stopped".into()))),
                Err(_) => {
                    task.abort();
                    Err(Problem::Failed("the words took too long to come back".into()))
                }
            };
            warm_next(rt);
            result
        });
        async move { waited.await.unwrap_or_else(|_| Err(Problem::Failed("live transcription stopped".into()))) }
            .boxed_local()
            .shared()
    }
}

/// Dropped unfinished (the recording was discarded, or a spare ran out): abandon the stream.
impl Drop for LiveTranscription {
    fn drop(&mut self) {
        if self.task.is_some() {
            let _ = self.commands.send(Command::Cancel);
            if self.marks.lock().unwrap().asked.is_some() {
                let rt = self.rt.clone();
                self.rt.spawn(async move { warm_next(rt) });
            }
        }
    }
}

/// Streams the microphone until `Finish`; the final transcript.
async fn live(
    relay: Relay,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    heard: &watch::Sender<Heard>,
    marks: &Mutex<Marks>,
) -> Result<String, Problem> {
    let mark = |step: fn(&mut Marks) -> &mut Option<Instant>| {
        step(&mut marks.lock().unwrap()).get_or_insert_with(Instant::now);
    };
    // Audio queues in `commands` while this connects.
    let mut socket = relay.connect().await?;
    mark(|marks| &mut marks.connected);
    let (mut audio_bytes, mut peak) = (0usize, 0i16);
    // The relay ended the recording itself (`Reply::Limit`): it takes no more audio, and its final ends it.
    let mut limited = false;
    let mut committed = false;
    // That final, when it came before the recording stopped.
    let mut early: Option<String> = None;
    let result = loop {
        tokio::select! {
            command = commands.recv(), if !committed => match command {
                Some(Command::Audio(pcm)) => {
                    if limited {
                        continue;
                    }
                    audio_bytes += pcm.len();
                    let loudest = pcm.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]]).saturating_abs()).fold(0, i16::max);
                    peak = peak.max(loudest);
                    if loudest >= SPEECH_PEAK.max(peak / 6) {
                        marks.lock().unwrap().last_speech = Some(Instant::now());
                    }
                    if let Err(error) = socket.send(Message::binary(pcm)).await {
                        break Err(stream_problem(error));
                    }
                }
                Some(Command::Finish { recorded_ms }) => {
                    if let Some(text) = early.take() {
                        break Ok(text);
                    }
                    if !limited {
                        if peak < SILENT_PEAK {
                            break Err(Problem::Failed("the microphone stream was silent".into()));
                        }
                        if (audio_bytes as f64 / BYTES_PER_MS) < recorded_ms - MAX_MISSING_MS {
                            break Err(Problem::Failed("the microphone stream stopped before the recording did".into()));
                        }
                        if let Err(error) = socket.send(Message::text(json!({ "type": "commit" }).to_string())).await {
                            break Err(stream_problem(error));
                        }
                        mark(|marks| &mut marks.committed);
                    }
                    committed = true;
                }
                // Discarded: nobody waits for the words.
                Some(Command::Cancel) | None => break Ok(String::new()),
            },
            message = socket.next(), if early.is_none() => match message {
                Some(Ok(Message::Text(text))) => match reply(text.as_str()) {
                    Reply::Partial(words) => {
                        if !words.trim().is_empty() {
                            mark(|marks| &mut marks.first_words);
                        }
                        heard.send_modify(|heard| heard.words = words)
                    }
                    Reply::Final(text) if committed => {
                        mark(|marks| &mut marks.final_words);
                        break Ok(text);
                    }
                    Reply::Final(text) => {
                        mark(|marks| &mut marks.final_words);
                        early = Some(text);
                    }
                    Reply::Limit(problem) => {
                        limited = true;
                        heard.send_modify(|heard| heard.stopped = Some(problem));
                    }
                    Reply::Error(problem) => break Err(problem),
                    Reply::Ready => mark(|marks| &mut marks.ready),
                    Reply::Other => {}
                },
                Some(Ok(Message::Close(_))) | None => break Err(Problem::Failed("the relay closed early".into())),
                Some(Ok(_)) => {}
                Some(Err(error)) => break Err(stream_problem(error)),
            },
            else => break Err(Problem::Failed("live transcription ended".into())),
        }
    };
    let _ = socket.close(None).await;
    result
}

/// Turns a saved recording (the recorder's .m4a) into text through the relay: decoded to 16 kHz PCM by macOS's
/// afconvert, sent once the relay is ready, a little faster than it was said, then committed. An allowance that runs
/// out midway refuses it, rather than send part of what was said.
pub async fn transcribe_file(relay: &Relay, path: &Path) -> Result<String, Problem> {
    let pcm = decode(path).await?;
    let mut socket = relay.connect().await?;
    // Audio sent before `ready` waits at the relay, which keeps only 30 s of it.
    loop {
        match next_reply(&mut socket).await? {
            Reply::Ready => break,
            Reply::Error(problem) | Reply::Limit(problem) => return Err(problem),
            _ => {}
        }
    }
    let chunk = (FILE_CHUNK_MS * BYTES_PER_MS) as usize & !1;
    let pace = Duration::from_secs_f64(FILE_CHUNK_MS / FILE_SPEED / 1000.);
    let (mut sink, mut stream) = socket.split();
    let sending = async {
        for piece in pcm.chunks(chunk) {
            sink.send(Message::binary(piece.to_vec())).await?;
            tokio::time::sleep(pace).await;
        }
        sink.send(Message::text(json!({ "type": "commit" }).to_string())).await
    };
    let answer = async {
        loop {
            let message = stream
                .next()
                .await
                .ok_or_else(|| Problem::Failed("the relay closed early".into()))?
                .map_err(stream_problem)?;
            if let Message::Text(text) = message {
                match reply(text.as_str()) {
                    Reply::Final(text) => return Ok(text),
                    Reply::Error(problem) | Reply::Limit(problem) => return Err(problem),
                    _ => {}
                }
            }
        }
    };
    let audio_ms = pcm.len() as f64 / BYTES_PER_MS;
    let deadline = Duration::from_secs_f64(audio_ms / FILE_SPEED / 1000.) + Duration::from_secs(20);
    // The answer ends it: a refusal midway stops the sending too.
    let result = tokio::time::timeout(deadline, async {
        tokio::pin!(sending, answer);
        let mut sent = false;
        loop {
            tokio::select! {
                done = &mut sending, if !sent => match done {
                    Ok(()) => sent = true,
                    // The relay's reason for closing, if it gave one, says more than the broken pipe.
                    Err(error) => return tokio::time::timeout(Duration::from_secs(2), &mut answer).await.unwrap_or(Err(stream_problem(error))),
                },
                result = &mut answer => return result,
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(Problem::Failed("the words took too long to come back".into())));
    if let Ok(mut socket) = sink.reunite(stream) {
        let _ = socket.close(None).await;
    }
    result
}

async fn next_reply(socket: &mut Socket) -> Result<Reply, Problem> {
    loop {
        let message = tokio::time::timeout(CONNECT_TIMEOUT, socket.next())
            .await
            .map_err(|_| Problem::Failed("the relay didn't start in time".into()))?
            .ok_or_else(|| Problem::Failed("the relay closed early".into()))?
            .map_err(stream_problem)?;
        match message {
            Message::Text(text) => return Ok(reply(text.as_str())),
            Message::Close(_) => return Err(Problem::Failed("the relay closed early".into())),
            _ => {}
        }
    }
}

/// The recording as 16 kHz mono PCM16.
async fn decode(path: &Path) -> Result<Vec<u8>, Problem> {
    let unreadable = || Problem::Failed("the recording couldn't be read".into());
    let dir = std::env::temp_dir().join("filo-desktop-voice");
    tokio::fs::create_dir_all(&dir).await.map_err(|_| unreadable())?;
    let wav = dir.join(format!("{}.wav", uuid::Uuid::new_v4()));
    let converted = tokio::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", &format!("LEI16@{SAMPLE_RATE}"), "-c", "1"])
        .arg(path)
        .arg(&wav)
        .output()
        .await;
    let bytes = tokio::fs::read(&wav).await;
    let _ = tokio::fs::remove_file(&wav).await;
    if !converted.is_ok_and(|output| output.status.success()) {
        return Err(unreadable());
    }
    let bytes = bytes.map_err(|_| unreadable())?;
    wav_data(&bytes).map(<[u8]>::to_vec).filter(|pcm| !pcm.is_empty()).ok_or_else(unreadable)
}

/// A WAV file's samples: its `data` chunk.
pub fn wav_data(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        if &bytes[at..at + 4] == b"data" {
            return bytes.get(body..body.saturating_add(size).min(bytes.len()));
        }
        at = body.saturating_add(size + (size & 1));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn socket_url_follows_the_worker_origin() {
        assert_eq!(socket_url("https://dev.filo.im"), "wss://dev.filo.im/v1/voice-transcribe");
        assert_eq!(socket_url("http://127.0.0.1:8000/"), "ws://127.0.0.1:8000/v1/voice-transcribe");
    }

    #[test]
    fn the_start_message_says_it_is_a_desktop_without_a_token() {
        let start = Relay::new("https://dev.filo.im").start_message();
        assert_eq!(start["type"], "start");
        assert_eq!(start["client"], "desktop");
        assert!(start.get("token").is_none() && start.get("installId").is_none());
        assert!(start["version"].is_string() && start["build"].is_u64());
    }

    #[test]
    fn the_relays_replies_are_read() {
        assert_eq!(reply(r#"{"type":"ready","allowanceMs":610000,"remainingMs":3600000,"resetAt":1}"#), Reply::Ready);
        assert_eq!(reply(r#"{"type":"partial","text":"hello the"}"#), Reply::Partial("hello the".into()));
        assert_eq!(
            reply(r#"{"type":"final","text":" Hello there. ","audioMs":900}"#),
            Reply::Final("Hello there.".into())
        );
        let limit = Problem::Limit { scope: "global".into(), reset_at: Some(1_800_000_000_000.) };
        assert_eq!(
            reply(r#"{"type":"error","code":"usage_limit","scope":"global","message":"…","resetAt":1800000000000}"#),
            Reply::Error(limit)
        );
        let session = Problem::Limit { scope: "session".into(), reset_at: None };
        assert_eq!(reply(r#"{"type":"limit","scope":"session","message":"…"}"#), Reply::Limit(session));
        assert_eq!(reply(r#"{"type":"error","code":"disabled","message":"Off"}"#), Reply::Error(Problem::Disabled));
        assert_eq!(
            reply(r#"{"type":"error","code":"unavailable","message":"Live transcription failed"}"#),
            Reply::Error(Problem::Failed("Live transcription failed".into()))
        );
        assert_eq!(reply("not json"), Reply::Other);
    }

    #[test]
    fn problems_say_when_voice_messages_are_back() {
        let noon = chrono::Local::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .earliest()
            .unwrap();
        let noon = noon.timestamp_millis() as f64;
        let hour = 3_600_000.;
        let ip = |reset_at| Problem::Limit { scope: "ip".into(), reset_at: Some(reset_at) };
        let at = |ms: f64| crate::format::clock(ms);
        assert_eq!(
            ip(noon + 3. * hour).message_at(noon),
            format!("You've used up today's voice messages. They're back at {}.", at(noon + 3. * hour))
        );
        assert_eq!(
            ip(noon + 17. * hour).message_at(noon),
            format!("You've used up today's voice messages. They're back tomorrow at {}.", at(noon + 17. * hour))
        );
        let global = Problem::Limit { scope: "global".into(), reset_at: Some(noon + 17. * hour) };
        assert!(
            global
                .message_at(noon)
                .starts_with("Voice messages have run out for everyone today. They're back tomorrow at ")
        );
        assert_eq!(
            Problem::Limit { scope: "ip".into(), reset_at: None }.message_at(noon),
            "You've used up today's voice messages."
        );
        assert!(Problem::Offline.message().contains("offline"));
        assert!(ip(noon).lasting() && Problem::Disabled.lasting() && !Problem::Offline.lasting());
    }

    #[test]
    fn a_wav_files_samples_are_found_past_other_chunks() {
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend(b"fmt \x10\0\0\0");
        wav.extend([0u8; 16]);
        wav.extend(b"FLLR\x03\0\0\0abc\0");
        wav.extend(b"data\x04\0\0\0\x01\x02\x03\x04");
        assert_eq!(wav_data(&wav), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(wav_data(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(wav_data(b"not a wav file"), None);
    }

    /// A relay on this Mac answering as `script` says, given what it was sent.
    async fn fake_relay(
        script: impl Fn(Value, usize) -> Vec<String> + Send + Sync + 'static,
    ) -> (Relay, tokio::task::JoinHandle<(Value, usize)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let start: Value = match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
                other => panic!("{other:?}"),
            };
            for line in script(start.clone(), 0) {
                socket.send(Message::text(line)).await.unwrap();
            }
            let mut audio = 0;
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Binary(pcm) => audio += pcm.len(),
                    Message::Text(text) if text.as_str().contains("commit") => {
                        for line in script(start.clone(), audio) {
                            socket.send(Message::text(line)).await.unwrap();
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            (start, audio)
        });
        (Relay::new(&origin), served)
    }

    fn speech(ms: usize) -> Vec<u8> {
        (0..ms * 16).flat_map(|n| (((n as f32 / 10.).sin() * 8_000.) as i16).to_le_bytes()).collect()
    }

    #[tokio::test]
    async fn live_words_come_back_and_the_final_is_the_message() {
        let (relay, served) = fake_relay(|_, audio| match audio {
            0 => vec![r#"{"type":"ready"}"#.into(), r#"{"type":"partial","text":"Milo, check"}"#.into()],
            _ => vec![r#"{"type":"final","text":"Milo, check the build."}"#.into()],
        })
        .await;
        let live = LiveTranscription::start_with(relay, tokio::runtime::Handle::current());
        let sink = live.sink();
        for _ in 0..10 {
            sink.push(speech(100));
        }
        let mut heard = live.watch();
        heard.wait_for(|heard| heard.words == "Milo, check").await.unwrap();
        assert_eq!(live.finish(1_000.).await, Ok("Milo, check the build.".into()));
        let (start, audio) = served.await.unwrap();
        assert_eq!((start["client"].as_str(), audio), (Some("desktop"), 32_000));
    }

    #[tokio::test]
    async fn a_recording_takes_the_session_opened_ahead() {
        let (relay, served) = fake_relay(|_, audio| match audio {
            0 => vec![r#"{"type":"ready"}"#.into()],
            _ => vec![r#"{"type":"final","text":"Sounds good."}"#.into()],
        })
        .await;
        warm(relay, tokio::runtime::Handle::current());
        let ready =
            || SPARE.lock().unwrap().as_ref().is_some_and(|(_, spare)| spare.marks.lock().unwrap().ready.is_some());
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The keys go down after it's ready: it's this recording's, already connected.
        let asked = Instant::now();
        let live = LiveTranscription::take_or_start(tokio::runtime::Handle::current(), asked);
        assert!(SPARE.lock().unwrap().is_none());
        live.sink().push(speech(1_000));
        let marks = live.marks();
        assert_eq!(live.finish(1_000.).await, Ok("Sounds good.".into()));
        let marks = marks.lock().unwrap().clone();
        assert!(marks.connected.is_some_and(|connected| connected < asked));
        assert!(marks.final_words.is_some_and(|words| words > asked) && marks.last_speech.is_some());
        let line = marks.line(Instant::now());
        assert!(line.starts_with("key down → connected 0 ms, ready 0 ms (opened 0 s before)"), "{line}");
        assert_eq!(served.await.unwrap().1, 32_000);
    }

    #[tokio::test]
    async fn a_refusal_says_why_while_recording_and_after() {
        let (relay, _served) = fake_relay(|_, _| {
            vec![
                r#"{"type":"error","code":"usage_limit","scope":"ip","message":"Used up","resetAt":1800000000000}"#
                    .into(),
            ]
        })
        .await;
        let live = LiveTranscription::start_with(relay, tokio::runtime::Handle::current());
        let limit = Problem::Limit { scope: "ip".into(), reset_at: Some(1_800_000_000_000.) };
        let mut heard = live.watch();
        heard.wait_for(|heard| heard.problem.is_some()).await.unwrap();
        assert_eq!(live.heard().problem, Some(limit.clone()));
        assert_eq!(live.finish(1_000.).await, Err(limit));
    }

    #[tokio::test]
    async fn a_limit_midway_stops_the_recording_and_keeps_what_was_heard() {
        let (relay, _served) = fake_relay(|_, _| {
            vec![
                r#"{"type":"ready"}"#.into(),
                r#"{"type":"limit","scope":"ip","message":"That's today's","resetAt":1800000000000}"#.into(),
                r#"{"type":"final","text":"What was said so far."}"#.into(),
            ]
        })
        .await;
        let live = LiveTranscription::start_with(relay, tokio::runtime::Handle::current());
        live.sink().push(speech(100));
        let mut heard = live.watch();
        heard.wait_for(|heard| heard.stopped.is_some()).await.unwrap();
        // The recording runs on a little past the limit; the relay's final is what it heard.
        assert_eq!(live.finish(5_000.).await, Ok("What was said so far.".into()));
    }

    #[tokio::test]
    async fn no_relay_is_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let live = LiveTranscription::start_with(Relay::new(&origin), tokio::runtime::Handle::current());
        assert_eq!(live.finish(1_000.).await, Err(Problem::Offline));
    }

    #[tokio::test]
    async fn a_saved_recording_is_transcribed_through_the_relay() {
        let dir = std::env::temp_dir().join(format!("filo-voice-file-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Two seconds of tone as a WAV, then as the recorder's AAC .m4a.
        let pcm = speech(2_000);
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + pcm.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
        wav.extend(SAMPLE_RATE.to_le_bytes());
        wav.extend((SAMPLE_RATE * 2).to_le_bytes());
        wav.extend(b"\x02\0\x10\0data");
        wav.extend((pcm.len() as u32).to_le_bytes());
        wav.extend(&pcm);
        std::fs::write(dir.join("tone.wav"), &wav).unwrap();
        let m4a = dir.join("tone.m4a");
        let made = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(dir.join("tone.wav"))
            .arg(&m4a)
            .status()
            .unwrap();
        assert!(made.success());

        let (relay, served) = fake_relay(|_, audio| match audio {
            0 => vec![r#"{"type":"ready","allowanceMs":610000}"#.into()],
            _ => vec![r#"{"type":"final","text":"A tone."}"#.into()],
        })
        .await;
        assert_eq!(transcribe_file(&relay, &m4a).await, Ok("A tone.".into()));
        let (_, audio) = served.await.unwrap();
        // About two seconds of 16 kHz PCM16 (AAC adds a little at the edges).
        assert!((60_000..=70_000).contains(&audio), "{audio} bytes");

        let (refusing, _) =
            fake_relay(|_, _| vec![r#"{"type":"error","code":"disabled","message":"Off"}"#.into()]).await;
        assert_eq!(transcribe_file(&refusing, &m4a).await, Err(Problem::Disabled));
        assert!(transcribe_file(&refusing, &dir.join("missing.m4a")).await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}

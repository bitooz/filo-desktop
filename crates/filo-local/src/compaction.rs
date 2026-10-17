// A session that went cold isn't resumed as it is. Both CLIs keep a session's prompt cache for about an hour on this Mac
// (`CACHE_LIFETIME`): Claude Code writes its cache for an hour on a Claude subscription (its `promptCacheTtl`, unset),
// and Codex's lasts up to an hour on OpenAI's side. Past that, a turn that resumes the session sends its whole history
// again at the full, uncached price, and every call of the turn after it reads all of it from the cache. A chat agent's
// or coding task's session grows to several hundred thousand tokens that way, since neither CLI compacts before it's
// near its context limit. So for a session idle past the cache's lifetime and over `MIN_TOKENS` (`due`), a chat's turn
// (turn.rs) starts a fresh session seeded with the chat's own messages word for word (reseed.rs), and a coding task's
// round (the app's local_backend/coding.rs and handoff.rs) has its CLI compact it first, told what the summary keeps
// (`note`): what the user asked for, the decisions and the open work. A session in use grows freely, but for a chat's:
// one near its context window (`full_from`) starts afresh the same way, idle or not (`reseed_due`), rather than let its
// CLI summarize it once it's full. Where each CLI compacts:
// - Claude Code at its window less min(max output, 20k) less 13k, before a model call: 967k on 1M, 167k on 200k
//   (2.1.300's own code; its docs say "at about 967K tokens" for a 1M window and "at the 200K boundary" without one,
//   https://code.claude.com/docs/en/model-config). A smaller auto-compact window (`CLAUDE_CODE_AUTO_COMPACT_WINDOW`,
//   the `autoCompactWindow` setting, `/autocompact`, `--autocompact`) or `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` makes it
//   compact earlier (https://code.claude.com/docs/en/env-vars); Filo doesn't read them, so with one set Claude Code
//   compacts first. `CLAUDE_CODE_DISABLE_1M_CONTEXT` holds 1M models to 200k, which the CLI then says as their window.
//   Its idle compaction (`CLAUDE_CODE_IDLE_COMPACT_MIN_TOKENS`) never comes into it: Filo runs it with `-p`, which
//   exits after each turn.
// - Codex at 90% of its model's window, after a call (codex-rs protocol/src/openai_models.rs, the model's
//   `auto_compact_token_limit`; the config's `model_auto_compact_token_limit` overrides it); its record counts the
//   window as 95% of it (`usable_context_window`, the `model_context_window` of its `token_count` events): 450k for
//   gpt-6.1-sol's 500k, recorded as 475k (0.162.1).
// So a chat starts afresh from 850k on a 1M window, from 325k on Codex's 475k and from 100k on 200k, ahead of either; a
// turn may still fill the rest, the check being at its start. A session is full only once it has grown a quarter of the
// window past its own first call (`GROWTH_DIVISOR`), so a fresh one whose first call is past half of a small window
// isn't started afresh again at once; and always `TURN_ROOM` short of the window, so on 200k one that started big is
// started afresh at 150k, before Claude Code's 167k. The seed shrinks with a small window for the same reason
// (reseed.rs `sizes`).
//
// The window is the model's as its CLI said it on an earlier turn of any chat (`Windows`, kept for the whole app by
// engine and model), else that of a current Claude model, all 1M (`known_window`). A model neither knows is taken for
// `SMALL_WINDOW` only when the chat has just switched to it: on the model its session already ran on, it isn't judged
// full until its CLI has said its window, after this turn.
//
// How others do it: T3 Code (pingdotgg/t3code) leaves compaction to the CLIs; it keeps a table of models' windows
// (apps/server/src/provider/model-manifest.json: `fixedContextWindowTokens`, or 200k and 1M by a `[1m]` option)
// beside what a provider reports, and for a Claude session idle past its cache with 100k or more, it offers a
// `/compact` (apps/web ContextWindowMeter.logic.ts). OpenCode compacts at the window less max(10%, 16k), the window
// from models.dev, and never on its own when it doesn't know it (packages/core/src/session/compaction.ts
// `calculateCeiling`). Filo differs on purpose: a chat starts afresh from its own words rather than a summary.
//
// How long a session has been idle and how big it is come from the CLI's own record of it (`last_call`): its last
// model call's time and the context it sent.
// - Claude Code: `~/.claude/projects/<folder>/<session>.jsonl` (`CLAUDE_CONFIG_DIR` in place of `~/.claude`): the last
//   main-thread `assistant` entry's usage, or a `compact_boundary`'s size after. It doesn't say the window: that's in
//   the `result` of each turn's stream, for the model its `init` says (`WindowWatch`).
// - Codex: `$CODEX_HOME/sessions/<y>/<m>/<d>/rollout-<time>-<thread>.jsonl`, in the folder of the day the thread
//   started (its id is a UUID v7, which starts with that time): the last `token_count` event's `last_token_usage`, and
//   its `model_context_window`.
//
// How each compacts headless:
// - Claude Code: `claude -p --resume <session> --input-format stream-json --output-format stream-json --verbose
//   --strict-mcp-config`, the message `/compact <note>`: it says `compacting`, then `compact_boundary` with the sizes
//   before and after, then its `result`.
// - Codex: `codex exec` has no `/compact` (it's taken as a message), so `codex app-server -c mcp_servers={}`:
//   `initialize`, `thread/resume`, `thread/inject_items` with the note as a developer message, then
//   `thread/compact/start`, which runs a turn whose `contextCompaction` item is the compaction;
//   `thread/tokenUsage/updated` says the size after. Signed in with ChatGPT, Codex compacts on OpenAI's side, which
//   takes no prompt of its own: the note in the history is what tells it what to keep.
// A compaction that fails leaves the session as it was, and the turn or round resumes it so.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::time::Duration;

use serde_json::{Value, json};

use crate::engine::{Engine, Stopper};

/// How long both CLIs keep a session's prompt cache on this Mac (measured from their own records on Oct 13: a Claude
/// Code session resumed within the hour was read from the cache, one resumed after it wasn't; Codex's current models
/// mostly within the hour, mostly not after it).
const CACHE_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// A session smaller than this is resumed as it is: resending it costs little, and compacting takes a while.
const MIN_TOKENS: u64 = 150_000;
/// A chat's session this close to its context window is full (`full_from`): its next turn starts afresh. On a window
/// under twice this, from half of it, so one turn with long tool output fits in the other half before the CLI would
/// compact (at about the window less 33k).
const HEADROOM: u64 = 150_000;
/// How far past where it started (its first call: the instructions, Filo's tools, the agent's notes and, for a fresh
/// one, the seed) a session grows before it can be full, as a share of the window: a quarter, 50k on 200k. A fresh
/// session's first call was 54k to 114k on this Mac (Oct 16, the chats that had started afresh, with the seed made for
/// 1M; 22k to 83k for the others), past half of a 200k window for some, which would make every turn start afresh again.
const GROWTH_DIVISOR: u64 = 4;
/// However it started, a session is full this far short of its window: below where the CLI compacts (Claude Code's
/// 167k on 200k, Codex's 90%), with room for a turn.
const TURN_ROOM: u64 = 50_000;
/// The window of a model the chat has just switched to, when it isn't known (`Windows::window_for`): Claude models'
/// without 1M (Opus and Sonnet 4.6 without `[1m]`, Haiku 4.5), and below Codex's (gpt-6.1-sol's is 272k in codex-rs's
/// bundled models.json, 500k as it runs here). Taking a window for smaller than it is starts a session afresh once too
/// early; taking it for larger lets the CLI compact it.
const SMALL_WINDOW: u64 = 200_000;
/// Claude Code's models that run with a 1M window without a `[1m]`: the current ones by id and the aliases that name
/// them on Anthropic's API ("Fable 5.1, Fable 5, Sonnet 5 and later, Haiku 5.5, and Opus 4.7 and later run with the 1M
/// window by default", model-config above; T3 Code's model-manifest.json says the same).
const CLAUDE_1M: &[&str] = &[
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "fable",
    "best",
    "opus",
    "sonnet",
    "haiku",
];
/// What the summary keeps: Claude Code's `/compact` instructions, and Codex's note before it compacts.
const KEEP: &str = "When you summarize this conversation, keep what the user asked for (in their words where it matters) and what they said they want or don't want; the decisions made, and why; and the open work: what's started, promised or waiting on the user, with the files, names, ids and links it needs. Leave out tool output and steps that no longer matter.";

/// A compaction that takes longer is stopped (the session is then resumed as it is).
const TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The CLIs' files are read from their end this much at a time.
const CHUNK: usize = 256 * 1024;

/// A session's last model call, from the CLI's own record of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LastCall {
    pub at_ms: i64,
    /// The context it sent and got back: what the next call starts from.
    pub tokens: u64,
    /// The context window of the model it ran on, when the record says (Codex's).
    pub window: Option<u64>,
}

impl LastCall {
    /// Its cache has expired and it's big enough to compact first.
    fn is_cold(&self, now_ms: i64) -> bool {
        now_ms - self.at_ms >= CACHE_LIFETIME.as_millis() as i64 && self.tokens >= MIN_TOKENS
    }
}

/// The size a chat's session on `window` that started at `start` (its first call's size, when known) is full from:
/// within `HEADROOM` of the window or past half of a small one, once it has grown a quarter of the window past its
/// start, and `TURN_ROOM` short of the window whatever it started at.
fn full_from(window: u64, start: Option<u64>) -> u64 {
    let grown = start.unwrap_or(0) + window / GROWTH_DIVISOR;
    window.saturating_sub(HEADROOM).max(window / 2).max(grown).min(window.saturating_sub(TURN_ROOM))
}

/// The session's last call when it's cold (`LastCall::is_cold`): compact it before resuming it. None for a session
/// in use, a small one, or one whose record isn't on this Mac (another Mac's, Muse Code's or OpenCode's).
pub fn due(engine: Engine, session: &str, now_ms: i64) -> Option<LastCall> {
    last_call(engine, session).filter(|last| last.is_cold(now_ms))
}

/// For a chat's turn: the session's last call when it's cold or full (`full_from`, on the turn's model's `window`, when
/// it's known: `Windows::window_for`), so the turn starts a fresh session (turn.rs) instead of resuming it. None as for
/// `due`.
pub fn reseed_due(engine: Engine, session: &str, now_ms: i64, window: Option<u64>) -> Option<LastCall> {
    let last = last_call(engine, session)?;
    if last.is_cold(now_ms) {
        return Some(last);
    }
    let window = window?;
    // Where the session started is read only for one big enough to be full without it.
    let full =
        last.tokens >= full_from(window, None) && last.tokens >= full_from(window, first_tokens(engine, session));
    full.then_some(last)
}

/// The context window the session's record says for its last call's model (Codex's), to learn for the model.
pub fn recorded_window(engine: Engine, session: &str) -> Option<u64> {
    last_call(engine, session).and_then(|last| last.window)
}

/// The context windows Filo has learned, by engine and model, for the whole app (`context-windows.json` in the agent
/// chats' folder): a model's window is the same in every chat, so one a chat switches to counts as known once any chat
/// has run on it. Keyed by the model a turn asks for ("" for the CLI's default), as the turn knows it before it runs.
/// Learned from Claude Code's stream (`WindowWatch`) and Codex's record (`recorded_window`) after each turn.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Windows(std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>>);

impl Windows {
    fn file(root: &Path) -> PathBuf {
        root.join("context-windows.json")
    }

    /// The windows learned so far (none from a file that can't be read: they're learned again).
    pub fn load(root: &Path) -> Windows {
        crate::files::read_json(&Windows::file(root)).ok().flatten().unwrap_or_default()
    }

    /// Keeps `window` as what the CLI just said for `engine`'s `model`. Each turn runs in a process of its own, so the
    /// file is changed under a lock beside it (`context-windows.lock`): turns ending together keep both their windows.
    pub fn learn(root: &Path, engine: Engine, model: &str, window: u64) {
        if window == 0 || Windows::load(root).0.get(engine.key()).and_then(|known| known.get(model)) == Some(&window) {
            return;
        }
        let kept = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join("context-windows.lock"))
            .and_then(|lock| crate::lock::flock(&lock, libc::LOCK_EX).map(|()| lock))
            .and_then(|_lock| {
                let mut all = Windows::load(root);
                all.0.entry(engine.key().to_string()).or_default().insert(model.to_string(), window);
                let bytes = serde_json::to_vec_pretty(&all).map_err(std::io::Error::other)?;
                crate::files::write_atomic(&Windows::file(root), &bytes)
            });
        if let Err(error) = kept {
            eprintln!("filo compaction: couldn't keep the context windows: {error}");
        }
    }

    /// The window a turn on `engine`'s `model` goes by: the one its CLI said, else a current Claude model's
    /// (`known_window`). Else, after a switch to it (`switched`: the session's last turn asked for another model),
    /// `SMALL_WINDOW`; on the session's own model, none: it isn't judged full until its CLI has said one.
    pub fn window_for(&self, engine: Engine, model: &str, switched: bool) -> Option<u64> {
        let learned = self.0.get(engine.key()).and_then(|known| known.get(model)).copied();
        learned.or_else(|| known_window(engine, model)).or(switched.then_some(SMALL_WINDOW))
    }
}

/// The window of a model Filo knows without its CLI saying it: 1M for Claude Code's current models (`CLAUDE_1M`) and
/// any `[1m]` one.
fn known_window(engine: Engine, model: &str) -> Option<u64> {
    (engine == Engine::Claude && (model.ends_with("[1m]") || CLAUDE_1M.contains(&model))).then_some(1_000_000)
}

/// The context window a chat turn's Claude Code says in its stream: its `result` gives it by model (`modelUsage`), and
/// the one that counts is the model the turn ran on, as its `init` says it (the same name, "claude-opus-5-5[1m]"), not a
/// side model's (Haiku's for small jobs, a subagent's), which may be bigger. Without the `init`, the model that read the
/// most context.
#[derive(Debug, Default)]
pub struct WindowWatch {
    model: Option<String>,
    window: Option<u64>,
}

impl WindowWatch {
    pub fn line(&mut self, line: &str) {
        if line.contains("\"subtype\":\"init\"") {
            if let Some(event) = serde_json::from_str::<Value>(line).ok().filter(|event| event["type"] == "system") {
                self.model = event["model"].as_str().map(str::to_string).or(self.model.take());
            }
            return;
        }
        if !line.contains("\"contextWindow\"") {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else { return };
        let Some(usage) = event["modelUsage"].as_object().filter(|_| event["type"] == "result") else { return };
        let read = |usage: &Value| {
            ["inputTokens", "cacheReadInputTokens", "cacheCreationInputTokens"]
                .iter()
                .map(|key| usage[key].as_u64().unwrap_or(0))
                .sum::<u64>()
        };
        let main = self
            .model
            .as_deref()
            .and_then(|model| usage.get(model))
            .or_else(|| usage.values().max_by_key(|usage| read(usage)));
        self.window =
            main.and_then(|usage| usage["contextWindow"].as_u64()).filter(|window| *window > 0).or(self.window);
    }

    /// The window it said last, if it did.
    pub fn window(&self) -> Option<u64> {
        self.window
    }
}

/// The CLI's record of the session on this Mac, and how a model call is read from one of its lines.
fn record(engine: Engine, session: &str) -> Option<(PathBuf, fn(&Value) -> Option<LastCall>)> {
    let homes = homes();
    match engine {
        Engine::Claude => Some((claude_file(&homes.claude, session)?, claude_call)),
        Engine::Codex => Some((codex_file(&homes.codex, session)?, codex_call)),
        Engine::Muse | Engine::OpenCode => None,
    }
}

/// The session's last model call, from the CLI's record of it on this Mac.
pub(crate) fn last_call(engine: Engine, session: &str) -> Option<LastCall> {
    #[cfg(feature = "dev")]
    if let Some((_, call)) = PRETENDED.lock().unwrap().iter().find(|(id, _)| id == session) {
        return Some(*call);
    }
    let (file, pick) = record(engine, session)?;
    from_the_end(&file, CHUNK, pick)
}

/// Where the session started: the size of its first model call, or of the first after the CLI last compacted it.
fn first_tokens(engine: Engine, session: &str) -> Option<u64> {
    let (file, pick) = record(engine, session)?;
    use std::io::BufRead as _;
    let mut start = None;
    for entry in std::io::BufReader::new(File::open(file).ok()?)
        .split(b'\n')
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_slice::<Value>(&line).ok())
    {
        // Claude Code's `compact_boundary`, Codex's `compacted`.
        if entry["subtype"] == "compact_boundary" || entry["type"] == "compacted" {
            start = None;
        } else if start.is_none() {
            start = pick(&entry).map(|call| call.tokens);
        }
    }
    start
}

/// Sessions whose last call is as a check says (`pretend_last_call`), not as their CLI's record has it.
#[cfg(feature = "dev")]
static PRETENDED: std::sync::Mutex<Vec<(String, LastCall)>> = std::sync::Mutex::new(Vec::new());

/// For checks and tests: `session`'s last call is `call` from now on, in this process, whatever its CLI's record says
/// (or whether it has one), so a turn or round finds it cold without waiting an hour or touching a real session.
#[cfg(feature = "dev")]
pub fn pretend_last_call(session: &str, call: LastCall) {
    let mut pretended = PRETENDED.lock().unwrap();
    pretended.retain(|(id, _)| id != session);
    pretended.push((session.to_string(), call));
}

/// Where the CLIs keep their sessions: Claude Code's folder and Codex's home.
#[derive(Clone, Debug)]
struct Homes {
    claude: PathBuf,
    codex: PathBuf,
}

fn homes() -> Homes {
    #[cfg(test)]
    if let Some(homes) = TEST_HOMES.with(|homes| homes.borrow().clone()) {
        return homes;
    }
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".claude"));
    Homes { claude, codex: crate::engine::codex_home() }
}

#[cfg(test)]
thread_local! {
    /// A test's stand-in folders for the CLIs' sessions, on its own thread (`with_homes`).
    static TEST_HOMES: std::cell::RefCell<Option<Homes>> = const { std::cell::RefCell::new(None) };
}

/// Runs `body` with the CLIs' sessions looked for in `claude` and `codex` (on this thread), not this Mac's.
#[cfg(test)]
pub(crate) fn with_homes<T>(claude: &Path, codex: &Path, body: impl FnOnce() -> T) -> T {
    TEST_HOMES.with(|homes| *homes.borrow_mut() = Some(Homes { claude: claude.into(), codex: codex.into() }));
    let result = body();
    TEST_HOMES.with(|homes| *homes.borrow_mut() = None);
    result
}

/// A session or thread id as the CLIs make them (a UUID), so it's safe in a file name.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Claude Code's file for the session, in whichever project folder it is (the folder it ran in).
fn claude_file(home: &Path, session: &str) -> Option<PathBuf> {
    if !plain_id(session) {
        return None;
    }
    let name = format!("{session}.jsonl");
    std::fs::read_dir(home.join("projects"))
        .ok()?
        .flatten()
        .map(|dir| dir.path().join(&name))
        .find(|file| file.is_file())
}

/// Codex's file for the thread: in the folder of the day it started, read from its id, else in any day's.
fn codex_file(home: &Path, thread: &str) -> Option<PathBuf> {
    if !plain_id(thread) {
        return None;
    }
    let sessions = home.join("sessions");
    let ending = format!("-{thread}.jsonl");
    let in_folder = |dir: &Path| -> Option<PathBuf> {
        std::fs::read_dir(dir).ok()?.flatten().map(|entry| entry.path()).find(|file| {
            file.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&ending))
        })
    };
    if let Some(started) = uuid_v7_ms(thread) {
        use chrono::TimeZone as _;
        // The day on this Mac's clock; the days around it in case the time zone changed since.
        for offset in [0, -1, 1] {
            if let Some(day) = chrono::Local.timestamp_millis_opt(started + offset * 86_400_000).single()
                && let Some(file) = in_folder(&sessions.join(day.format("%Y/%m/%d").to_string()))
            {
                return Some(file);
            }
        }
    }
    let sorted = |dir: &Path| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort_unstable_by(|a, b| b.cmp(a));
        dirs
    };
    sorted(&sessions)
        .iter()
        .flat_map(|year| sorted(year))
        .flat_map(|month| sorted(&month))
        .find_map(|day| in_folder(&day))
}

/// When a UUID v7 was made (its first 48 bits, in ms); None for another kind of id.
fn uuid_v7_ms(id: &str) -> Option<i64> {
    let hex: String = id.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || hex.as_bytes()[12] != b'7' {
        return None;
    }
    i64::from_str_radix(&hex[..12], 16).ok()
}

/// The last line of a JSONL file that `pick` takes, read from the end a chunk at a time: a long session's file isn't
/// read whole for its last lines.
fn from_the_end<T>(path: &Path, chunk: usize, mut pick: impl FnMut(&Value) -> Option<T>) -> Option<T> {
    let mut file = File::open(path).ok()?;
    let mut end = file.metadata().ok()?.len();
    // What the chunk read last began with: the end of a line that starts in an earlier chunk.
    let mut rest: Vec<u8> = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(chunk as u64);
        let mut bytes = vec![0; (end - start) as usize];
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(&mut bytes).ok()?;
        bytes.extend_from_slice(&rest);
        // Up to the first newline goes on from an earlier chunk, unless this is the file's start; with no newline at
        // all, the whole chunk does.
        let (rest_ends, lines_start) = match bytes.iter().position(|byte| *byte == b'\n') {
            _ if start == 0 => (0, 0),
            Some(at) => (at, at + 1),
            None => (bytes.len(), bytes.len()),
        };
        for line in bytes[lines_start..].split(|byte| *byte == b'\n').rev() {
            if let Some(found) = serde_json::from_slice::<Value>(line).ok().and_then(|value| pick(&value)) {
                return Some(found);
            }
        }
        bytes.truncate(rest_ends);
        rest = bytes;
        end = start;
    }
    None
}

fn at_ms(entry: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(entry["timestamp"].as_str()?).ok().map(|at| at.timestamp_millis())
}

/// One of Claude Code's entries, when it's a model call of the main conversation (not a subagent's, nor a message
/// Claude Code made up itself) or the point it compacted at.
fn claude_call(entry: &Value) -> Option<LastCall> {
    match entry["type"].as_str()? {
        "assistant" if entry["isSidechain"] != true && entry["message"]["model"] != "<synthetic>" => {
            let usage = &entry["message"]["usage"];
            let tokens: u64 =
                ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"]
                    .iter()
                    .map(|key| usage[key].as_u64().unwrap_or(0))
                    .sum();
            (tokens > 0).then_some(LastCall { at_ms: at_ms(entry)?, tokens, window: None })
        }
        "system" if entry["subtype"] == "compact_boundary" => Some(LastCall {
            at_ms: at_ms(entry)?,
            tokens: entry["compactMetadata"]["postTokens"].as_u64().unwrap_or(0),
            window: None,
        }),
        _ => None,
    }
}

/// One of Codex's entries, when it counts a call's tokens (after a compaction, the size of what's left), with the
/// window it counts them against.
fn codex_call(entry: &Value) -> Option<LastCall> {
    if entry["type"] != "event_msg" || entry["payload"]["type"] != "token_count" {
        return None;
    }
    let info = &entry["payload"]["info"];
    let tokens = info["last_token_usage"]["total_tokens"].as_u64()?;
    Some(LastCall {
        at_ms: at_ms(entry)?,
        tokens,
        window: info["model_context_window"].as_u64().filter(|window| *window > 0),
    })
}

/// A session to compact.
#[derive(Clone, Debug)]
pub struct Spec {
    pub engine: Engine,
    pub program: PathBuf,
    /// Claude Code's session or Codex's thread.
    pub session: String,
    /// The folder it runs in: Claude Code finds a session by it.
    pub cwd: PathBuf,
    pub path_env: Option<String>,
    /// The model its turns run on (a 1M-token session needs one that takes it); none: the CLI's default.
    pub model: Option<String>,
}

/// How a compaction went.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub compacted: bool,
    /// The session's size before and after, as the CLI said.
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub stopped: bool,
    /// Why it didn't compact.
    pub error: Option<String>,
}

/// Compacts the session, blocking until it's done (a minute or so for a big one). `on_start` gets a Stopper as soon
/// as the CLI runs.
pub fn run(spec: &Spec, on_start: impl FnOnce(Stopper)) -> Outcome {
    run_for(spec, TIMEOUT, on_start)
}

fn args(spec: &Spec) -> Vec<String> {
    match spec.engine {
        Engine::Claude => {
            let mut args: Vec<String> = [
                "-p",
                "--resume",
                &spec.session,
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--strict-mcp-config",
            ]
            .map(str::to_string)
            .to_vec();
            if let Some(model) = &spec.model {
                args.extend(["--model".into(), model.clone()]);
            }
            args
        }
        // No MCP servers: compacting calls none, and the user's own would start for nothing.
        _ => ["app-server", "-c", "mcp_servers={}"].map(str::to_string).to_vec(),
    }
}

fn run_for(spec: &Spec, timeout: Duration, on_start: impl FnOnce(Stopper)) -> Outcome {
    if !matches!(spec.engine, Engine::Claude | Engine::Codex) {
        return Outcome { error: Some(format!("{} doesn't compact here", spec.engine.label())), ..Outcome::default() };
    }
    let mut started = match crate::engine::spawn_cli(
        spec.engine,
        &spec.program,
        &spec.cwd,
        spec.path_env.as_deref(),
        false,
        &args(spec),
        Vec::new(),
        20,
    ) {
        Ok(started) => started,
        Err(error) => return Outcome { error: Some(error), ..Outcome::default() },
    };
    let stopper = started.stopper.clone();
    on_start(stopper.clone());
    let mut talk = Talk {
        spec,
        stdin: started.stdin.take(),
        outcome: Outcome::default(),
        done: false,
        asked: false,
        compacting: false,
    };
    talk.begin();
    let crate::engine::Drove { status, timed_out, stderr } = started.drive(&mut talk, timeout);

    let mut outcome = talk.outcome;
    if stopper.is_stopped() && !timed_out && !talk.done {
        return Outcome { stopped: true, ..Outcome::default() };
    }
    if timed_out {
        outcome.compacted = false;
        outcome.error = Some(format!("{} took longer than {} minutes", spec.engine.label(), timeout.as_secs() / 60));
    } else if !outcome.compacted && outcome.error.is_none() {
        let code =
            status.and_then(|status| status.code()).map_or("a signal".to_string(), |code| format!("status {code}"));
        outcome.error = Some(match stderr.iter().rev().find(|line| !line.trim().is_empty()) {
            Some(detail) => format!("{} stopped with {code}: {}", spec.engine.label(), detail.trim()),
            None => format!("{} ended without compacting ({code})", spec.engine.label()),
        });
    }
    outcome
}

/// The conversation with the CLI while it compacts.
struct Talk<'a> {
    spec: &'a Spec,
    stdin: Option<ChildStdin>,
    outcome: Outcome,
    /// It's over (compacted or not): its stdin closes.
    done: bool,
    /// Codex: it took `thread/compact/start` (the turn that ends next is the compaction's).
    asked: bool,
    /// Codex: its compaction item started (the token counts after it are the size after).
    compacting: bool,
}

impl crate::engine::Conversation for Talk<'_> {
    fn line(&mut self, line: &str) -> bool {
        if let Ok(message) = serde_json::from_str::<Value>(line) {
            match self.spec.engine {
                Engine::Claude => self.claude(&message),
                _ => self.codex(&message),
            }
        }
        false
    }

    fn finished(&self) -> bool {
        self.done
    }

    fn close(&mut self) {
        self.stdin = None;
    }
}

impl Talk<'_> {
    fn write(&mut self, message: &Value) {
        crate::engine::write_line(&mut self.stdin, message);
    }

    fn request(&mut self, id: i64, method: &str, params: Value) {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn begin(&mut self) {
        match self.spec.engine {
            // The command as the message; it exits at its result once stdin is closed.
            Engine::Claude => {
                let command = format!("/compact {KEEP}");
                self.write(&json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": command }] } }));
                self.stdin = None;
            }
            _ => {
                let params = json!({
                    "clientInfo": { "name": "filo_desktop", "title": "Filo", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "experimentalApi": true, "requestAttestation": false },
                });
                self.request(1, "initialize", params);
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.outcome.error.get_or_insert(error);
        self.done = true;
    }

    fn claude(&mut self, event: &Value) {
        match (event["type"].as_str(), event["subtype"].as_str()) {
            (Some("system"), Some("compact_boundary")) => {
                let sizes = &event["compact_metadata"];
                self.outcome.compacted = true;
                self.outcome.before = sizes["pre_tokens"].as_u64();
                self.outcome.after = sizes["post_tokens"].as_u64();
            }
            (Some("system"), Some("status")) => {
                if let Some(result) = event["compact_result"].as_str().filter(|result| *result != "success") {
                    let why = event["compact_error"].as_str().unwrap_or(result).to_string();
                    self.outcome.error.get_or_insert(format!("Claude Code couldn't compact: {why}"));
                }
            }
            (Some("result"), _) => {
                if !self.outcome.compacted {
                    let said = event["result"]
                        .as_str()
                        .map(str::trim)
                        .filter(|said| !said.is_empty())
                        .unwrap_or("it said nothing");
                    self.outcome.error.get_or_insert(format!("Claude Code didn't compact: {said}"));
                }
                self.done = true;
            }
            _ => {}
        }
    }

    fn codex(&mut self, message: &Value) {
        let error = message["error"]["message"].as_str().map(str::to_string);
        let thread = self.spec.session.clone();
        match (message["id"].as_i64(), message["method"].as_str()) {
            (Some(1), None) => {
                self.write(&json!({ "jsonrpc": "2.0", "method": "initialized" }));
                let mut params =
                    json!({ "threadId": thread, "cwd": self.spec.cwd.display().to_string(), "excludeTurns": true });
                if let Some(model) = &self.spec.model {
                    params["model"] = json!(model);
                }
                self.request(2, "thread/resume", params);
            }
            (Some(2), None) => match error {
                Some(why) => self.fail(format!("Codex couldn't open the thread: {why}")),
                None => {
                    let keep = json!({ "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": KEEP }] });
                    self.request(3, "thread/inject_items", json!({ "threadId": thread, "items": [keep] }));
                }
            },
            // Compacted even if the note didn't go in: it keeps its own idea of what matters then.
            (Some(3), None) => self.request(4, "thread/compact/start", json!({ "threadId": thread })),
            (Some(4), None) => match error {
                Some(why) => self.fail(format!("Codex couldn't compact: {why}")),
                None => self.asked = true,
            },
            (None, Some("item/started")) if message["params"]["item"]["type"] == "contextCompaction" => {
                self.compacting = true
            }
            (None, Some("item/completed")) if message["params"]["item"]["type"] == "contextCompaction" => {
                self.outcome.compacted = true
            }
            (None, Some("thread/tokenUsage/updated")) => {
                let size = message["params"]["tokenUsage"]["last"]["totalTokens"].as_u64();
                if self.compacting {
                    self.outcome.after = size;
                } else {
                    self.outcome.before = size;
                }
            }
            (None, Some("turn/completed")) if self.asked => {
                let turn = &message["params"]["turn"];
                if turn["status"] != "completed" && !self.outcome.compacted {
                    let why = turn["error"]["message"].as_str().unwrap_or("its turn didn't complete").to_string();
                    self.outcome.error.get_or_insert(format!("Codex couldn't compact: {why}"));
                }
                self.done = true;
            }
            _ => {}
        }
    }
}

/// A session's size in a few characters ("640k tokens").
pub fn tokens_label(tokens: u64) -> String {
    match tokens {
        0..1_000 => format!("{tokens} tokens"),
        _ => format!("{}k tokens", (tokens + 500) / 1_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{script, temp_dir};

    const HOUR: i64 = 3_600_000;

    #[test]
    fn a_session_is_cold_once_its_cache_expired_and_only_when_its_big() {
        let at = 1_000 * HOUR;
        let big = LastCall { at_ms: at, tokens: 640_000, window: None };
        assert!(!big.is_cold(at + HOUR - 1), "still cached");
        assert!(big.is_cold(at + HOUR));
        assert!(!LastCall { tokens: MIN_TOKENS - 1, ..big }.is_cold(at + 10 * HOUR), "small enough to resend");
        assert!(LastCall { tokens: MIN_TOKENS, ..big }.is_cold(at + 10 * HOUR));
        assert_eq!(
            (tokens_label(640_412), tokens_label(1_973), tokens_label(950)),
            ("640k tokens".into(), "2k tokens".into(), "950 tokens".into())
        );
    }

    /// A chat's session is full 150k short of its window: 850k on a 1M window, 325k on Codex's 475k. On a small window,
    /// from half of it (100k on 200k), so one turn's long tool output still fits before Claude Code would compact (167k).
    /// Only once it has grown a quarter of the window past its first call: a fresh session that started at 90k on 200k
    /// isn't full until 140k, not at its next turn. And never later than 50k short of the window: one that started at
    /// 117k is full at 150k, before Claude Code's 167k.
    #[test]
    fn a_session_is_full_150k_short_of_its_window_or_half_a_small_one_once_its_grown() {
        let full = |tokens: u64, window: u64, start: Option<u64>| tokens >= full_from(window, start);
        assert!(!full(849_999, 1_000_000, None) && full(850_000, 1_000_000, None));
        assert!(!full(324_999, 475_000, None) && full(325_000, 475_000, None));
        assert!(!full(99_999, 200_000, None) && full(100_000, 200_000, None));
        assert!(!full(63_999, 128_000, None) && full(64_000, 128_000, None));
        // Where it started: a plain session's 30k changes nothing; a fresh one's 90k moves it to 140k, 117k to 150k.
        assert!(full(100_000, 200_000, Some(30_000)));
        assert!(
            !full(118_000, 200_000, Some(90_000))
                && !full(139_999, 200_000, Some(90_000))
                && full(140_000, 200_000, Some(90_000))
        );
        assert_eq!((full_from(200_000, Some(117_488)), full_from(200_000, Some(190_000))), (150_000, 150_000));
        assert!(!full(849_999, 1_000_000, Some(114_000)) && full(850_000, 1_000_000, Some(114_000)), "no change on 1M");
        assert_eq!(full_from(1_000_000, Some(800_000)), 950_000);
    }

    /// Windows are learned for the whole app by engine and model; current Claude models are 1M without being learned. A
    /// model that's neither is taken for 200k only after a switch to it, and isn't judged on the session's own model.
    #[test]
    fn a_models_window_is_learned_app_wide_or_known_and_small_only_after_a_switch() {
        let root = temp_dir("windows");
        assert_eq!(Windows::load(&root), Windows::default());
        Windows::learn(&root, Engine::Claude, "", 1_000_000);
        Windows::learn(&root, Engine::Codex, "gpt-6.1-sol", 475_000);
        Windows::learn(&root, Engine::Claude, "sonnet", 200_000);
        let windows = Windows::load(&root);
        assert_eq!(windows.window_for(Engine::Claude, "", false), Some(1_000_000), "learned, in any chat");
        assert_eq!(
            windows.window_for(Engine::Claude, "sonnet", false),
            Some(200_000),
            "what the CLI said (1M turned off) over the table"
        );
        assert_eq!(windows.window_for(Engine::Codex, "gpt-6.1-sol", true), Some(475_000));
        // Current Claude models, by id, alias or `[1m]`, before any turn on them.
        for model in ["claude-opus-5-5", "opus", "haiku", "claude-fable-5-1", "opus[1m]"] {
            assert_eq!(Windows::default().window_for(Engine::Claude, model, true), Some(1_000_000), "{model}");
        }
        // Neither: 200k after a switch to it; on the session's own model, not judged.
        assert_eq!(windows.window_for(Engine::Claude, "claude-opus-4-6", true), Some(SMALL_WINDOW));
        assert_eq!(windows.window_for(Engine::Claude, "claude-opus-4-6", false), None);
        assert_eq!(windows.window_for(Engine::Codex, "gpt-6.1-mini", true), Some(SMALL_WINDOW));
        assert_eq!(Windows::default().window_for(Engine::Codex, "opus", false), None, "the table is Claude's");
        // What the CLI says again replaces it.
        Windows::learn(&root, Engine::Claude, "sonnet", 1_000_000);
        assert_eq!(Windows::load(&root).window_for(Engine::Claude, "sonnet", false), Some(1_000_000));
    }

    /// Turns run in processes of their own: learning windows at once under the lock, none is lost.
    #[test]
    fn windows_learned_at_once_are_all_kept() {
        let root = temp_dir("windows-at-once");
        let threads: Vec<_> = (0..16u64)
            .map(|index| {
                let root = root.to_path_buf();
                std::thread::spawn(move || {
                    Windows::learn(&root, Engine::Codex, &format!("model-{index}"), 100_000 + index)
                })
            })
            .collect();
        threads.into_iter().for_each(|thread| thread.join().unwrap());
        let windows = Windows::load(&root);
        assert!(
            (0..16u64)
                .all(|index| windows.window_for(Engine::Codex, &format!("model-{index}"), false)
                    == Some(100_000 + index)),
            "{windows:?}"
        );
    }

    /// Only a chat's turn starts afresh when the session is full; a coding round compacts only a cold one (`due`). How
    /// big the session was at its first call comes from the CLI's record.
    #[test]
    fn a_full_session_is_due_for_a_reseed_but_not_a_compaction() {
        let home = temp_dir("claude-full");
        let project = home.join("projects/-workspace");
        std::fs::create_dir_all(&project).unwrap();
        let now = 1_000 * HOUR;
        let at = chrono::DateTime::from_timestamp_millis(now - 5 * 60_000).unwrap().to_rfc3339();
        let reply = |read: u64| {
            json!({ "type": "assistant", "timestamp": at, "message": { "model": "claude-opus-5-5", "usage": { "input_tokens": 2, "cache_read_input_tokens": read, "output_tokens": 98 } } }).to_string()
        };
        let user = json!({ "type": "user", "timestamp": at, "message": { "content": "hi" } }).to_string();
        with_homes(&home, &home.join("no-codex"), || {
            std::fs::write(
                project.join("s-full.jsonl"),
                [user.clone(), reply(30_000), reply(900_000)].join("\n") + "\n",
            )
            .unwrap();
            assert_eq!(first_tokens(Engine::Claude, "s-full"), Some(30_100));
            assert_eq!(
                reseed_due(Engine::Claude, "s-full", now, Some(1_000_000)).map(|last| last.tokens),
                Some(900_100)
            );
            assert_eq!(due(Engine::Claude, "s-full", now), None, "in use: a coding round resumes it");
            std::fs::write(project.join("s-full.jsonl"), [reply(30_000), reply(600_000)].join("\n") + "\n").unwrap();
            assert_eq!(reseed_due(Engine::Claude, "s-full", now, Some(1_000_000)), None);
            assert!(reseed_due(Engine::Claude, "s-full", now, Some(200_000)).is_some());
            assert_eq!(recorded_window(Engine::Claude, "s-full"), None, "Claude Code's record doesn't say it");
            assert_eq!(reseed_due(Engine::Claude, "s-full", now, None), None, "no window known: not judged full");
            // A fresh session that started at 114k on a 200k window, at its next turn: not full yet.
            std::fs::write(project.join("s-full.jsonl"), [reply(114_000), reply(120_000)].join("\n") + "\n").unwrap();
            assert_eq!(reseed_due(Engine::Claude, "s-full", now, Some(200_000)), None);
            std::fs::write(project.join("s-full.jsonl"), [reply(114_000), reply(170_000)].join("\n") + "\n").unwrap();
            assert!(reseed_due(Engine::Claude, "s-full", now, Some(200_000)).is_some());
            // Cold, as before.
            assert!(reseed_due(Engine::Claude, "s-full", now + 2 * HOUR, None).is_some());
            // Compacted by Claude Code since: it starts again from the first call after that.
            let boundary = json!({ "type": "system", "subtype": "compact_boundary", "timestamp": at, "compactMetadata": { "postTokens": 3_000 } }).to_string();
            std::fs::write(
                project.join("s-full.jsonl"),
                [reply(114_000), boundary, reply(20_000), reply(105_000)].join("\n") + "\n",
            )
            .unwrap();
            assert_eq!(first_tokens(Engine::Claude, "s-full"), Some(20_100));
            assert!(reseed_due(Engine::Claude, "s-full", now, Some(200_000)).is_some());
        });
    }

    /// The window is the turn's model's, as its `init` names it (as Claude Code 2.1.300 says them), not a side model's.
    #[test]
    fn claude_codes_window_is_its_turns_models_in_its_result() {
        let watched = |lines: &[Value]| {
            let mut watch = WindowWatch::default();
            for line in lines {
                watch.line(&line.to_string());
            }
            watch.window()
        };
        let init = |model: &str| json!({ "type": "system", "subtype": "init", "session_id": "s-1", "model": model, "tools": [] });
        let result = json!({ "type": "result", "subtype": "success", "modelUsage": {
            "claude-sonnet-4-5": { "inputTokens": 2, "cacheReadInputTokens": 90_000, "contextWindow": 200_000, "maxOutputTokens": 64_000, "canonicalModel": "claude-sonnet-4-5" },
            "claude-haiku-5-5": { "inputTokens": 9, "cacheReadInputTokens": 120_000, "contextWindow": 1_000_000, "canonicalModel": "claude-haiku-5-5" },
        } });
        assert_eq!(
            watched(&[init("claude-sonnet-4-5"), result.clone()]),
            Some(200_000),
            "the side model's 1M doesn't count"
        );
        // No init: the model that read the most context.
        assert_eq!(watched(&[result.clone()]), Some(1_000_000));
        let one_m = json!({ "type": "result", "modelUsage": { "claude-opus-5-5[1m]": { "inputTokens": 2, "contextWindow": 1_000_000, "canonicalModel": "claude-opus-5-5" } } });
        assert_eq!(watched(&[init("claude-opus-5-5[1m]"), one_m]), Some(1_000_000));
        // A later result without one keeps the last.
        assert_eq!(
            watched(&[
                init("claude-sonnet-4-5"),
                result,
                json!({ "type": "result", "subtype": "success", "result": "hi" })
            ]),
            Some(200_000)
        );
        assert_eq!(watched(&[json!({ "type": "assistant", "message": { "contextWindow": 5 } })]), None);
        let mut watch = WindowWatch::default();
        watch.line("not json \"contextWindow\"");
        assert_eq!(watch.window(), None);
    }

    #[test]
    fn claude_codes_last_call_is_its_last_main_thread_reply_or_where_it_compacted() {
        let home = temp_dir("claude");
        let project = home.join("projects/-Users-me-project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(home.join("projects/-Users-me-other")).unwrap();
        let reply = |at: &str, read: u64, model: &str, side: bool| {
            json!({ "type": "assistant", "timestamp": at, "isSidechain": side, "message": { "model": model, "usage": { "input_tokens": 2, "cache_creation_input_tokens": 1_000, "cache_read_input_tokens": read, "output_tokens": 98 } } }).to_string()
        };
        let lines = [
            reply("2026-10-13T09:00:00Z", 10_000, "claude-opus-5-5", false),
            reply("2026-10-13T09:10:00.000Z", 640_000, "claude-opus-5-5", false),
            // A subagent's call, and one Claude Code made up (an error it shows as a reply), don't count.
            reply("2026-10-13T09:20:00Z", 5_000, "claude-haiku-5-5", true),
            reply("2026-10-13T09:21:00Z", 0, "<synthetic>", false),
            json!({ "type": "user", "timestamp": "2026-10-13T09:22:00Z", "message": { "content": "thanks" } })
                .to_string(),
            json!({ "type": "cost-state", "sessionId": "s-1" }).to_string(),
        ];
        std::fs::write(project.join("s-1.jsonl"), lines.join("\n") + "\n").unwrap();
        let ten_past = chrono::DateTime::parse_from_rfc3339("2026-10-13T09:10:00Z").unwrap().timestamp_millis();
        let expected = LastCall { at_ms: ten_past, tokens: 641_100, window: None };
        assert_eq!(claude_file(&home, "s-1"), Some(project.join("s-1.jsonl")));
        assert_eq!(from_the_end(&project.join("s-1.jsonl"), CHUNK, claude_call), Some(expected));
        // Read a few bytes at a time, lines split across chunks: the same.
        assert_eq!(from_the_end(&project.join("s-1.jsonl"), 7, claude_call), Some(expected));
        with_homes(&home, &home.join("no-codex"), || {
            assert_eq!(last_call(Engine::Claude, "s-1"), Some(expected));
            assert_eq!(due(Engine::Claude, "s-1", ten_past + HOUR + 1), Some(expected));
            assert_eq!(due(Engine::Claude, "s-1", ten_past + HOUR / 2), None);
            assert_eq!(last_call(Engine::Claude, "missing"), None);
            assert_eq!(last_call(Engine::Claude, "../projects/-Users-me-project/s-1"), None);
            assert_eq!(last_call(Engine::Muse, "s-1"), None);
        });

        // Compacted since: the size after is what's left, and the session isn't cold-and-big anymore.
        let compacted = json!({ "type": "system", "subtype": "compact_boundary", "timestamp": "2026-10-13T11:00:00Z", "compactMetadata": { "trigger": "manual", "preTokens": 641_100, "postTokens": 1_973 } });
        std::fs::write(project.join("s-1.jsonl"), lines.join("\n") + "\n" + &compacted.to_string() + "\n").unwrap();
        let after = from_the_end(&project.join("s-1.jsonl"), 64, claude_call).unwrap();
        assert_eq!(after.tokens, 1_973);
        assert!(!after.is_cold(after.at_ms + 10 * HOUR));
    }

    #[test]
    fn codexs_last_call_is_its_last_token_count_found_by_the_day_its_thread_started() {
        let home = temp_dir("codex");
        // A UUID v7 made at 2026-10-13T09:14:07.373Z.
        let thread = "01a1348a-5ad0-7d01-8572-3acf8009fd78";
        assert_eq!(uuid_v7_ms(thread), Some(0x01a1_348a_5ad0));
        assert_eq!(uuid_v7_ms("38dc713a-b729-4cec-87c3-d7ecee3d2ef0"), None, "a v4 has no time");
        use chrono::TimeZone as _;
        let day = chrono::Local.timestamp_millis_opt(0x01a1_348a_5ad0).unwrap().format("%Y/%m/%d").to_string();
        let folder = home.join("sessions").join(day);
        std::fs::create_dir_all(&folder).unwrap();
        let count = |at: &str, total: Value| {
            json!({ "timestamp": at, "type": "event_msg", "payload": { "type": "token_count", "info": if total.is_null() { Value::Null } else { json!({ "last_token_usage": { "input_tokens": 98_690, "total_tokens": total }, "model_context_window": 475_000 }) } } }).to_string()
        };
        let lines = [
            json!({ "timestamp": "2026-10-13T09:14:07Z", "type": "session_meta", "payload": { "id": thread } })
                .to_string(),
            count("2026-10-13T09:16:03Z", json!(95_072)),
            count("2026-10-13T09:16:07.089Z", json!(98_695)),
            // Only the rate limits: no usage to read.
            count("2026-10-13T09:16:08Z", Value::Null),
            json!({ "timestamp": "2026-10-13T09:16:08Z", "type": "event_msg", "payload": { "type": "task_complete" } })
                .to_string(),
        ];
        let file = folder.join(format!("rollout-2026-10-13T14-14-07-{thread}.jsonl"));
        std::fs::write(&file, lines.join("\n") + "\n").unwrap();
        // Another thread's file the same day isn't it.
        std::fs::write(folder.join("rollout-2026-10-13T14-00-00-01a1348a-0000-7000-8000-000000000000.jsonl"), "")
            .unwrap();
        assert_eq!(codex_file(&home, thread), Some(file.clone()));
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-13T09:16:07.089Z").unwrap().timestamp_millis();
        with_homes(&home.join("no-claude"), &home, || {
            assert_eq!(
                last_call(Engine::Codex, thread),
                Some(LastCall { at_ms: at, tokens: 98_695, window: Some(475_000) })
            );
            assert_eq!(due(Engine::Codex, thread, at + 2 * HOUR), None, "under MIN_TOKENS");
            assert_eq!(recorded_window(Engine::Codex, thread), Some(475_000), "learned for the model");
            assert_eq!(first_tokens(Engine::Codex, thread), Some(95_072), "where it started");
        });

        // A thread whose id doesn't say when it started is looked for in every day's folder.
        let other = folder.parent().unwrap().parent().unwrap().join("09/30");
        std::fs::create_dir_all(&other).unwrap();
        let old = "old-thread-1";
        std::fs::write(
            other.join(format!("rollout-2026-09-30T10-00-00-{old}.jsonl")),
            count("2026-09-30T10:05:00Z", json!(420_000)) + "\n",
        )
        .unwrap();
        with_homes(&home.join("no-claude"), &home, || {
            let last = last_call(Engine::Codex, old).unwrap();
            assert_eq!(last.tokens, 420_000);
            assert_eq!(due(Engine::Codex, old, last.at_ms + 3 * HOUR), Some(last));
        });
        // Compacted since it started: where it started is the first count after that.
        let compacted =
            json!({ "timestamp": "2026-09-30T11:00:00Z", "type": "compacted", "payload": { "message": "" } })
                .to_string();
        let counts = [
            count("2026-09-30T10:00:00Z", json!(95_000)),
            compacted,
            count("2026-09-30T11:00:01Z", json!(11_015)),
            count("2026-09-30T11:05:00Z", json!(60_000)),
        ];
        std::fs::write(other.join("rollout-2026-09-30T10-00-00-compacted-1.jsonl"), counts.join("\n") + "\n").unwrap();
        with_homes(&home.join("no-claude"), &home, || {
            assert_eq!(first_tokens(Engine::Codex, "compacted-1"), Some(11_015))
        });
    }

    /// A stand-in `claude` that compacts: it checks its command line and message, says so as Claude Code does, and
    /// leaves what it was given beside it.
    const FAKE_CLAUDE: &str = r#"#!/bin/sh
echo "$@" > "$0.args"
read -r message
printf '%s\n' "$message" > "$0.message"
case "$message" in *'"text":"/compact When you summarize this conversation, keep what the user asked for'*) ;; *) echo 'Error: not a compact' >&2; exit 2;; esac
echo '{"type":"system","subtype":"status","status":"compacting","session_id":"s-1"}'
echo '{"type":"system","subtype":"status","status":null,"compact_result":"success","session_id":"s-1"}'
echo '{"type":"system","subtype":"init","session_id":"s-1"}'
echo '{"type":"system","subtype":"compact_boundary","session_id":"s-1","compact_metadata":{"trigger":"manual","pre_tokens":641100,"post_tokens":1973}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"","session_id":"s-1"}'
"#;

    #[test]
    fn claude_code_compacts_with_its_compact_command() {
        let dir = temp_dir("claude-run");
        let fake = script(&dir, "claude", FAKE_CLAUDE);
        let spec = Spec {
            engine: Engine::Claude,
            program: fake.clone(),
            session: "s-1".into(),
            cwd: dir.to_path_buf(),
            path_env: None,
            model: Some("opus[1m]".into()),
        };
        let started = std::cell::Cell::new(false);
        let outcome = run_for(&spec, Duration::from_secs(20), |_| started.set(true));
        assert_eq!(
            outcome,
            Outcome { compacted: true, before: Some(641_100), after: Some(1_973), stopped: false, error: None }
        );
        assert!(started.get());
        let args = std::fs::read_to_string(dir.join("claude.args")).unwrap();
        assert_eq!(
            args.trim(),
            "-p --resume s-1 --input-format stream-json --output-format stream-json --verbose --strict-mcp-config --model opus[1m]"
        );
        let message: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("claude.message")).unwrap()).unwrap();
        assert_eq!(message["message"]["content"][0]["text"], format!("/compact {KEEP}"));

        // A session Claude Code doesn't have: it says so, and nothing's compacted.
        let gone = script(&dir, "gone", "#!/bin/sh\necho 'No conversation found with session ID: s-1' >&2\nexit 1\n");
        let outcome = run_for(&Spec { program: gone, ..spec.clone() }, Duration::from_secs(20), |_| {});
        assert!(
            !outcome.compacted
                && outcome.error.as_deref()
                    == Some("Claude Code stopped with status 1: No conversation found with session ID: s-1"),
            "{outcome:?}"
        );
        // One that ends without compacting (too little to compact) says what it said.
        let little = script(
            &dir,
            "little",
            "#!/bin/sh\nread -r message\necho '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Not enough messages to compact.\"}'\n",
        );
        let outcome = run_for(&Spec { program: little, ..spec.clone() }, Duration::from_secs(20), |_| {});
        assert_eq!(outcome.error.as_deref(), Some("Claude Code didn't compact: Not enough messages to compact."));
        // Stopped while it compacts: stopped, no error; one that takes too long is stopped with one.
        let slow = script(
            &dir,
            "slow",
            "#!/bin/sh\nread -r message\necho '{\"type\":\"system\",\"subtype\":\"status\",\"status\":\"compacting\"}'\nsleep 30\n",
        );
        let began = std::time::Instant::now();
        let outcome = run_for(&Spec { program: slow.clone(), ..spec.clone() }, Duration::from_secs(20), |stopper| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                stopper.stop();
            });
        });
        assert_eq!(outcome, Outcome { stopped: true, ..Outcome::default() });
        let outcome = run_for(&Spec { program: slow, ..spec }, Duration::from_millis(500), |_| {});
        assert!(
            !outcome.stopped
                && outcome.error.as_deref().is_some_and(|error| error.starts_with("Claude Code took longer than")),
            "{outcome:?}"
        );
        assert!(began.elapsed() < Duration::from_secs(15));
    }

    /// A stand-in `codex app-server`: it expects Filo's requests in order (each line it hears goes beside it), answers
    /// them, and compacts the thread.
    const FAKE_CODEX: &str = r#"#!/bin/sh
echo "$@" > "$0.args"
expect() { while read -r line; do echo "$line" >> "$0.heard"; case "$line" in $1) return 0;; esac; done; echo "never: $1" >&2; exit 3; }
expect '*"initialize"*'
echo '{"id":1,"result":{"userAgent":"codex"}}'
echo '{"method":"account/updated","params":{"authMode":"chatgpt"}}'
expect '*"thread/resume"*'
echo '{"id":2,"result":{"thread":{"id":"th-1"}}}'
echo '{"method":"thread/tokenUsage/updated","params":{"threadId":"th-1","tokenUsage":{"last":{"totalTokens":420000}}}}'
expect '*"thread/inject_items"*'
echo '{"id":3,"result":{}}'
expect '*"thread/compact/start"*'
echo '{"id":4,"result":{}}'
echo '{"method":"turn/started","params":{"turn":{"id":"tu-1","status":"inProgress"}}}'
echo '{"method":"item/started","params":{"item":{"type":"contextCompaction","id":"c-1"}}}'
echo '{"method":"thread/tokenUsage/updated","params":{"threadId":"th-1","tokenUsage":{"last":{"totalTokens":13589}}}}'
echo '{"method":"item/completed","params":{"item":{"type":"contextCompaction","id":"c-1"}}}'
echo '{"method":"turn/completed","params":{"turn":{"id":"tu-1","status":"completed","error":null}}}'
read -r line
"#;

    #[test]
    fn codex_compacts_through_its_app_server_told_what_to_keep() {
        let dir = temp_dir("codex-run");
        let fake = script(&dir, "codex", FAKE_CODEX);
        let spec = Spec {
            engine: Engine::Codex,
            program: fake,
            session: "th-1".into(),
            cwd: dir.to_path_buf(),
            path_env: None,
            model: None,
        };
        let outcome = run_for(&spec, Duration::from_secs(20), |_| {});
        assert_eq!(
            outcome,
            Outcome { compacted: true, before: Some(420_000), after: Some(13_589), stopped: false, error: None }
        );
        assert_eq!(std::fs::read_to_string(dir.join("codex.args")).unwrap().trim(), "app-server -c mcp_servers={}");
        let heard: Vec<Value> = std::fs::read_to_string(dir.join("codex.heard"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let methods: Vec<&str> = heard.iter().filter_map(|message| message["method"].as_str()).collect();
        assert_eq!(
            methods,
            ["initialize", "initialized", "thread/resume", "thread/inject_items", "thread/compact/start"]
        );
        assert_eq!((&heard[2]["params"]["threadId"], &heard[2]["params"]["model"]), (&json!("th-1"), &Value::Null));
        assert_eq!(
            heard[3]["params"]["items"],
            json!([{ "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": KEEP }] }])
        );
        assert_eq!(heard[4]["params"], json!({ "threadId": "th-1" }));

        // A thread Codex doesn't have.
        let gone = script(
            &dir,
            "gone",
            "#!/bin/sh\nread -r line\necho '{\"id\":1,\"result\":{}}'\nread -r line\nread -r line\necho '{\"id\":2,\"error\":{\"code\":-32600,\"message\":\"no rollout found for thread id th-1\"}}'\nread -r line\n",
        );
        let outcome = run_for(&Spec { program: gone, ..spec.clone() }, Duration::from_secs(20), |_| {});
        assert_eq!(
            outcome.error.as_deref(),
            Some("Codex couldn't open the thread: no rollout found for thread id th-1")
        );
        assert!(!outcome.compacted);
        // Muse Code and OpenCode don't compact here.
        assert!(
            run_for(&Spec { engine: Engine::Muse, ..spec }, Duration::from_secs(1), |_| panic!("started"))
                .error
                .is_some()
        );
    }
}

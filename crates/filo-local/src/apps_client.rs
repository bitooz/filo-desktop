// Reaching the user's apps' MCP servers (apps.rs): a local one is a command Filo runs and talks to over stdio, a
// remote one a URL spoken to with MCP's Streamable HTTP (or, for an older server, HTTP with SSE). Two things use it:
//
// - `check`: the Apps page's status and tool list. Starts or reaches the server, says hello (`initialize`), lists its
//   tools and lets it go. A server that wants a sign-in it doesn't have says so (`NEEDS_SIGN_IN`).
// - `proxy_main`: what the coding tools run for each app in a turn (`filo --local-mcp --app <id> --db <filo.db>`).
//   It passes their messages through unchanged, with three things added: the app's secrets (the API key in its header
//   or environment, the sign-in's token, refreshed when it runs out), its tools the user turned off left out of
//   `tools/list` and their calls refused, and HTTP turned into the stdio both CLIs start. Secrets stay in this process:
//   neither CLI's settings nor its command line ever has them.

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio as Piped};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::apps::{self, App, Check, Tool};
use crate::apps_oauth;
use crate::db::Db;
use crate::secrets::{self, Secrets};

/// The MCP version Filo speaks first; servers answer with the one they speak.
pub const PROTOCOL: &str = crate::mcp::PROTOCOL;
/// How long a check waits for a server to start and answer (a local one may download itself first).
const START_TIMEOUT: Duration = Duration::from_secs(90);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// A tool call through the proxy that takes longer fails.
const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Where a server's messages to Filo go, as they come.
type Sink = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
enum SendError {
    /// The server wants a sign-in, or refused the one it got: its `WWW-Authenticate` header.
    Unauthorized(String),
    Failed(String),
}

/// A connection to an app's server, either way.
trait Upstream: Send + Sync {
    /// Sends one JSON-RPC message; what the server says back goes to the sink.
    fn send(&self, message: &Value) -> Result<(), SendError>;
    fn close(&self);
}

fn is_initialize(message: &Value) -> bool {
    message.get("method").and_then(Value::as_str) == Some("initialize")
}

fn snippet(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 200 { format!("{}…", line.chars().take(200).collect::<String>()) } else { line }
}

// ---- stdio ----

/// A local server: the command, run with the app's environment, one JSON message per line each way.
struct Stdio {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    own_group: bool,
    stderr_tail: Arc<Mutex<Vec<String>>>,
}

/// `command` as a path: as it is with a slash, else the first match on `path_env` (the login shell's PATH) or PATH.
pub fn resolve_command(command: &str, path_env: Option<&str>) -> Option<PathBuf> {
    if command.contains('/') {
        let path = PathBuf::from(command);
        return path.is_file().then_some(path);
    }
    let own = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    path_env
        .unwrap_or("")
        .split(':')
        .chain(own.split(':'))
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .chain([home.join(".local/bin"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()])
        .map(|dir| dir.join(command))
        .find(|candidate| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

impl Stdio {
    /// Starts the server. `own_group`: in a process group of its own, ended whole on close (a check); the proxy's
    /// stays in the CLI's, which Stop ends. `on_exit` runs when it stops answering.
    pub fn spawn(
        app: &App,
        env: &[(String, String)],
        path_env: Option<&str>,
        own_group: bool,
        sink: Sink,
        on_exit: Option<Box<dyn Fn() + Send>>,
    ) -> Result<Stdio, String> {
        use std::os::unix::process::CommandExt as _;
        let program = resolve_command(&app.command, path_env)
            .ok_or_else(|| format!("`{}` isn't installed on this Mac (it isn't on your shell's PATH).", app.command))?;
        // Without Filo's macOS permissions, as the coding tools (disclaim.rs).
        let mut command = crate::disclaim::command(&program, false);
        command.args(&app.args).stdin(Piped::piped()).stdout(Piped::piped()).stderr(Piped::piped());
        if let Some(path) = path_env {
            command.env("PATH", path);
        }
        for (key, value) in &app.env {
            command.env(key, value);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        if let Some(home) = std::env::var_os("HOME") {
            command.current_dir(home);
        }
        if own_group {
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|error| format!("Couldn't start `{}`: {error}", app.command))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        {
            let tail = stderr_tail.clone();
            let echo = !own_group;
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    // The proxy passes the server's log on to the CLI's.
                    if echo {
                        eprintln!("{line}");
                    }
                    let mut tail = tail.lock().unwrap();
                    tail.push(line);
                    let excess = tail.len().saturating_sub(20);
                    tail.drain(..excess);
                }
            });
        }
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str::<Value>(line.trim()) {
                    sink(message);
                }
            }
            if let Some(on_exit) = on_exit {
                on_exit();
            }
        });
        Ok(Stdio { stdin: Mutex::new(child.stdin.take()), child: Mutex::new(child), own_group, stderr_tail })
    }

    /// Its last lines on stderr, for an error.
    pub fn stderr(&self) -> String {
        self.stderr_tail.lock().unwrap().iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join("\n")
    }
}

impl Upstream for Stdio {
    fn send(&self, message: &Value) -> Result<(), SendError> {
        let mut stdin = self.stdin.lock().unwrap();
        let Some(pipe) = stdin.as_mut() else { return Err(SendError::Failed("The server has stopped.".into())) };
        let mut line = message.to_string();
        line.push('\n');
        pipe.write_all(line.as_bytes()).and_then(|_| pipe.flush()).map_err(|_| {
            let said = self.stderr();
            SendError::Failed(if said.is_empty() {
                "The server stopped.".into()
            } else {
                format!("The server stopped: {}", snippet(&said))
            })
        })
    }

    fn close(&self) {
        self.stdin.lock().unwrap().take();
        let mut child = self.child.lock().unwrap();
        // A moment to leave on its own once its stdin closes, as MCP's stdio transport asks.
        for _ in 0..10 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = child.id() as i32;
        unsafe {
            libc::kill(if self.own_group { -pid } else { pid }, libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(300));
        if !matches!(child.try_wait(), Ok(Some(_))) {
            unsafe {
                libc::kill(if self.own_group { -pid } else { pid }, libc::SIGKILL);
            }
        }
        let _ = child.wait();
    }
}

// ---- HTTP ----

/// How a remote server's requests are signed.
enum HttpAuth {
    None,
    /// An API key's header.
    Header {
        name: String,
        value: String,
    },
    /// A sign-in's access token: `false` for the one kept (refreshed if it ran out), `true` for a new one after the
    /// server refused it.
    Token(Box<dyn Fn(bool) -> Result<String, String> + Send + Sync>),
}

#[derive(Default)]
struct HttpState {
    session: Option<String>,
    protocol: Option<String>,
    /// The client's `initialize`, sent again when the server forgets the session.
    init: Option<Value>,
    legacy: Option<Arc<Legacy>>,
}

/// A remote server over Streamable HTTP: each message a POST, the answer as JSON or as an SSE stream.
struct Http {
    client: reqwest::blocking::Client,
    url: String,
    auth: HttpAuth,
    sink: Sink,
    state: Mutex<HttpState>,
    /// Replies Filo asked for itself (a new session's `initialize`), not passed on.
    swallow: Arc<Mutex<HashSet<String>>>,
    next: AtomicU64,
}

enum PostError {
    Send(SendError),
    /// The server forgot the session (404): start another.
    SessionGone,
    /// It doesn't take POSTed messages (an older SSE server).
    NotStreamable(String),
}

impl From<SendError> for PostError {
    fn from(error: SendError) -> Self {
        PostError::Send(error)
    }
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_else(|| url.to_string())
}

pub fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("Filo/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|error| error.to_string())
}

/// Reads an SSE stream: `on_event(event, data)` for each, until it ends or `on_event` says stop.
fn read_events(reader: impl BufRead, mut on_event: impl FnMut(&str, &str) -> bool) {
    let mut event = String::new();
    let mut data = String::new();
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            if !data.is_empty() || !event.is_empty() {
                let kind = if event.is_empty() { "message" } else { event.as_str() };
                if !on_event(kind, data.strip_suffix('\n').unwrap_or(&data)) {
                    return;
                }
            }
            event.clear();
            data.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line
            .split_once(':')
            .map(|(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)))
            .unwrap_or((line, ""));
        match field {
            "event" => event = value.to_string(),
            "data" => {
                data.push_str(value);
                data.push('\n');
            }
            _ => {}
        }
    }
    if !data.is_empty() {
        let kind = if event.is_empty() { "message" } else { event.as_str() };
        on_event(kind, data.strip_suffix('\n').unwrap_or(&data));
    }
}

impl Http {
    pub fn new(url: &str, auth: HttpAuth, timeout: Duration, sink: Sink) -> Result<Http, String> {
        Ok(Http {
            client: http_client(timeout)?,
            url: url.to_string(),
            auth,
            sink,
            state: Mutex::new(HttpState::default()),
            swallow: Arc::new(Mutex::new(HashSet::new())),
            next: AtomicU64::new(1),
        })
    }

    fn auth_header(&self, fresh: bool) -> Result<Option<(String, String)>, SendError> {
        match &self.auth {
            HttpAuth::None => Ok(None),
            HttpAuth::Header { name, value } => Ok(Some((name.clone(), value.clone()))),
            HttpAuth::Token(token) => token(fresh)
                .map(|token| Some(("Authorization".into(), format!("Bearer {token}"))))
                .map_err(SendError::Unauthorized),
        }
    }

    /// What the server sent: passed on, unless Filo asked for it itself. A reply to `initialize` names the version
    /// later requests carry.
    fn receive(&self, message: Value) {
        if let Some(version) = message.pointer("/result/protocolVersion").and_then(Value::as_str) {
            self.state.lock().unwrap().protocol = Some(version.to_string());
        }
        if let Some(id) = message.get("id").filter(|_| message.get("method").is_none())
            && self.swallow.lock().unwrap().remove(&id.to_string())
        {
            return;
        }
        (self.sink)(message);
    }

    fn post(&self, message: &Value, retried: bool) -> Result<(), PostError> {
        let initialize = is_initialize(message);
        let (session, protocol) = {
            let mut state = self.state.lock().unwrap();
            if initialize {
                state.init = Some(message.clone());
                state.session = None;
            }
            (state.session.clone(), state.protocol.clone())
        };
        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(message.to_string());
        if let Some(session) = &session {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(protocol) = protocol.filter(|_| !initialize) {
            request = request.header("MCP-Protocol-Version", protocol);
        }
        if let Some((name, value)) = self.auth_header(retried)? {
            request = request.header(name, value);
        }
        let response = request.send().map_err(|error| {
            SendError::Failed(format!("Couldn't reach {}: {}", host_of(&self.url), reqwest_reason(&error)))
        })?;
        let status = response.status().as_u16();
        if status == 401 {
            let challenge = response
                .headers()
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            if matches!(self.auth, HttpAuth::Token(_)) && !retried {
                return self.post(message, true);
            }
            return Err(SendError::Unauthorized(challenge).into());
        }
        if status == 404 && session.is_some() && !initialize {
            self.state.lock().unwrap().session = None;
            return Err(PostError::SessionGone);
        }
        if initialize && matches!(status, 400 | 404 | 405) {
            return Err(PostError::NotStreamable(format!("{status}")));
        }
        if let Some(session) = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()) {
            self.state.lock().unwrap().session = Some(session.to_string());
        }
        if status == 202 || status == 204 {
            return Ok(());
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(200..300).contains(&status) {
            let body = response.text().unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|body| {
                    body.pointer("/error/message")
                        .or_else(|| body.get("error_description"))
                        .or_else(|| body.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| snippet(&body));
            return Err(SendError::Failed(format!(
                "{} answered {status}{}",
                host_of(&self.url),
                if detail.is_empty() { String::new() } else { format!(": {detail}") }
            ))
            .into());
        }
        if content_type.contains("text/event-stream") {
            read_events(BufReader::new(response), |_, data| {
                if let Ok(message) = serde_json::from_str::<Value>(data) {
                    self.receive(message);
                }
                true
            });
            return Ok(());
        }
        let body = response
            .text()
            .map_err(|error| SendError::Failed(format!("{} stopped answering: {error}", host_of(&self.url))))?;
        if body.trim().is_empty() {
            return Ok(());
        }
        match serde_json::from_str::<Value>(&body) {
            Ok(Value::Array(messages)) => messages.into_iter().for_each(|message| self.receive(message)),
            Ok(message) => self.receive(message),
            Err(_) => {
                return Err(SendError::Failed(format!(
                    "{} didn't answer like an MCP server: {}",
                    host_of(&self.url),
                    snippet(&body)
                ))
                .into());
            }
        }
        Ok(())
    }

    /// The server forgot the session: a new one, said hello to again with the client's own `initialize`.
    fn new_session(&self) -> Result<(), SendError> {
        let Some(mut init) = self.state.lock().unwrap().init.clone() else {
            return Err(SendError::Failed("The server ended the session.".into()));
        };
        let id = json!(format!("filo-session-{}", self.next.fetch_add(1, Ordering::SeqCst)));
        init["id"] = id.clone();
        self.swallow.lock().unwrap().insert(id.to_string());
        let sent = self
            .post(&init, false)
            .and_then(|_| self.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), false));
        sent.map_err(|error| match error {
            PostError::Send(error) => error,
            _ => SendError::Failed("The server ended the session and wouldn't start another.".into()),
        })
    }

    fn legacy(&self) -> Option<Arc<Legacy>> {
        self.state.lock().unwrap().legacy.clone()
    }

    /// Falls back to the older HTTP with SSE: a GET stream that names where to POST, the answers on the stream.
    fn start_legacy(&self) -> Result<Arc<Legacy>, SendError> {
        let mut request = self.client.get(&self.url).header("Accept", "text/event-stream");
        if let Some((name, value)) = self.auth_header(false)? {
            request = request.header(name, value);
        }
        // The stream stays open for as long as the connection does.
        let response = request.timeout(Duration::from_secs(24 * 60 * 60)).send().map_err(|error| {
            SendError::Failed(format!("Couldn't reach {}: {}", host_of(&self.url), reqwest_reason(&error)))
        })?;
        let status = response.status().as_u16();
        if status == 401 {
            let challenge = response
                .headers()
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string();
            return Err(SendError::Unauthorized(challenge));
        }
        let is_stream = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream"));
        if !(200..300).contains(&status) || !is_stream {
            return Err(SendError::Failed(format!(
                "{} isn't an MCP server (it answered {status}).",
                host_of(&self.url)
            )));
        }
        let legacy =
            Arc::new(Legacy { endpoint: Mutex::new(None), ready: Condvar::new(), closed: AtomicBool::new(false) });
        {
            let (legacy, sink, swallow, base) =
                (legacy.clone(), self.sink.clone(), self.swallow.clone(), self.url.clone());
            std::thread::spawn(move || {
                read_events(BufReader::new(response), |event, data| {
                    if legacy.closed.load(Ordering::SeqCst) {
                        return false;
                    }
                    if event == "endpoint" {
                        let endpoint = url::Url::parse(&base)
                            .and_then(|base| base.join(data.trim()))
                            .map(|url| url.to_string())
                            .ok();
                        *legacy.endpoint.lock().unwrap() = endpoint;
                        legacy.ready.notify_all();
                    } else if let Ok(message) = serde_json::from_str::<Value>(data) {
                        let mine = message
                            .get("id")
                            .filter(|_| message.get("method").is_none())
                            .is_some_and(|id| swallow.lock().unwrap().remove(&id.to_string()));
                        if !mine {
                            sink(message);
                        }
                    }
                    true
                });
                legacy.closed.store(true, Ordering::SeqCst);
                legacy.ready.notify_all();
            });
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut endpoint = legacy.endpoint.lock().unwrap();
        while endpoint.is_none() && !legacy.closed.load(Ordering::SeqCst) && Instant::now() < deadline {
            endpoint = legacy.ready.wait_timeout(endpoint, Duration::from_millis(500)).unwrap().0;
        }
        if endpoint.is_none() {
            return Err(SendError::Failed(format!("{} didn't say where to send messages.", host_of(&self.url))));
        }
        drop(endpoint);
        self.state.lock().unwrap().legacy = Some(legacy.clone());
        Ok(legacy)
    }

    fn post_legacy(&self, legacy: &Legacy, message: &Value) -> Result<(), SendError> {
        let endpoint = legacy
            .endpoint
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| SendError::Failed("The server's stream closed.".into()))?;
        if legacy.closed.load(Ordering::SeqCst) {
            return Err(SendError::Failed("The server's stream closed.".into()));
        }
        let mut request =
            self.client.post(&endpoint).header("Content-Type", "application/json").body(message.to_string());
        if let Some((name, value)) = self.auth_header(false)? {
            request = request.header(name, value);
        }
        let response = request.send().map_err(|error| {
            SendError::Failed(format!("Couldn't reach {}: {}", host_of(&self.url), reqwest_reason(&error)))
        })?;
        match response.status().as_u16() {
            401 => Err(SendError::Unauthorized(
                response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .to_string(),
            )),
            status if (200..300).contains(&status) => Ok(()),
            status => Err(SendError::Failed(format!("{} answered {status}", host_of(&self.url)))),
        }
    }
}

struct Legacy {
    endpoint: Mutex<Option<String>>,
    ready: Condvar,
    closed: AtomicBool,
}

fn reqwest_reason(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "it took too long to answer".into();
    }
    if error.is_connect() {
        return "couldn't connect".into();
    }
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        text = inner.to_string();
        source = inner.source();
    }
    text
}

impl Upstream for Http {
    fn send(&self, message: &Value) -> Result<(), SendError> {
        if let Some(legacy) = self.legacy() {
            return self.post_legacy(&legacy, message);
        }
        match self.post(message, false) {
            Ok(()) => Ok(()),
            Err(PostError::Send(error)) => Err(error),
            Err(PostError::SessionGone) => {
                self.new_session()?;
                self.post(message, false).map_err(|error| match error {
                    PostError::Send(error) => error,
                    _ => SendError::Failed("The server ended the session.".into()),
                })
            }
            Err(PostError::NotStreamable(status)) => {
                let legacy = self.start_legacy().map_err(|error| match error {
                    SendError::Failed(_) => {
                        SendError::Failed(format!("{} isn't an MCP server (it answered {status}).", host_of(&self.url)))
                    }
                    other => other,
                })?;
                self.post_legacy(&legacy, message)
            }
        }
    }

    fn close(&self) {
        if let Some(legacy) = self.legacy() {
            legacy.closed.store(true, Ordering::SeqCst);
        }
        // Tells the server the session is over; it's fine if it doesn't care.
        let session = self.state.lock().unwrap().session.clone();
        if let Some(session) = session {
            let mut request =
                self.client.delete(&self.url).header("Mcp-Session-Id", session).timeout(Duration::from_secs(3));
            if let Ok(Some((name, value))) = self.auth_header(false) {
                request = request.header(name, value);
            }
            let _ = request.send();
        }
    }
}

// ---- Connecting ----

/// The proxy's server stopped: so does the proxy, and the CLI sees the app go.
fn exit_now() {
    std::process::exit(0)
}

/// Opens the app's server with its secrets. `lock_dir`: where a sign-in's refresh is locked, so turns running at once
/// don't each spend the refresh token. `proxy`: run for a CLI (the server's log goes to its stderr, and ending it ends
/// the proxy).
fn connect(
    app: &App,
    secrets: &Secrets,
    lock_dir: &Path,
    path_env: Option<&str>,
    proxy: bool,
    sink: Sink,
) -> Result<Box<dyn Upstream>, String> {
    let key = || -> Result<String, String> {
        secrets
            .get_for(&secrets::account(&app.id, apps::SECRET_KEY), &apps::key_destination(app))?
            .filter(|key| !key.is_empty())
            .ok_or_else(|| "Add the API key first.".to_string())
    };
    if app.transport == apps::STDIO {
        let mut env = Vec::new();
        if app.auth == apps::AUTH_KEY {
            env.push((app.key_name.clone(), key()?));
        }
        let on_exit: Option<Box<dyn Fn() + Send>> = if proxy { Some(Box::new(exit_now)) } else { None };
        return Ok(Box::new(Stdio::spawn(app, &env, path_env, !proxy, sink, on_exit)?));
    }
    let auth = match app.auth.as_str() {
        apps::AUTH_KEY => {
            HttpAuth::Header { name: app.key_name.clone(), value: format!("{}{}", app.key_prefix, key()?) }
        }
        apps::AUTH_OAUTH => {
            let (app, secrets, lock_dir) = (app.clone(), secrets.clone(), lock_dir.to_path_buf());
            HttpAuth::Token(Box::new(move |fresh| apps_oauth::access_token(&app, &secrets, &lock_dir, fresh)))
        }
        _ => HttpAuth::None,
    };
    Ok(Box::new(Http::new(&app.url, auth, if proxy { CALL_TIMEOUT } else { ANSWER_TIMEOUT }, sink)?))
}

/// Requests and their answers over an upstream (a check).
struct Client {
    upstream: Box<dyn Upstream>,
    incoming: mpsc::Receiver<Value>,
    next: u64,
}

impl Client {
    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, SendError> {
        let id = self.next;
        self.next += 1;
        self.upstream.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = match self.incoming.recv_timeout(left) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(SendError::Failed(format!("It didn't answer in {} seconds.", timeout.as_secs())));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SendError::Failed("The server stopped.".into()));
                }
            };
            // A request from the server: a ping is answered, anything else declined (a check offers nothing).
            if let (Some(method), Some(their_id)) = (message.get("method").and_then(Value::as_str), message.get("id")) {
                let reply = if method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": their_id, "result": {} })
                } else {
                    json!({ "jsonrpc": "2.0", "id": their_id, "error": { "code": -32601, "message": "Not supported" } })
                };
                let _ = self.upstream.send(&reply);
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error.get("message").and_then(Value::as_str).unwrap_or("error");
                return Err(SendError::Failed(format!("The server said: {}", snippet(text))));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&self, method: &str) {
        let _ = self.upstream.send(&json!({ "jsonrpc": "2.0", "method": method }));
    }
}

/// What to tell the user when a server wants a sign-in, by how the app signs in.
fn sign_in_detail(app: &App) -> String {
    match app.auth.as_str() {
        apps::AUTH_OAUTH if app.has_secret => "The sign-in ran out or was revoked. Sign in again.".into(),
        apps::AUTH_OAUTH => "Sign in to your account to use it.".into(),
        apps::AUTH_KEY if app.has_secret => "The server didn't accept the API key. Check it and save it again.".into(),
        apps::AUTH_KEY => "Add the API key to use it.".into(),
        _ => "This server wants a sign-in. Edit the app and pick Account or API key.".into(),
    }
}

/// Starts or reaches the app's server, says hello and lists its tools, then lets it go. Blocks: up to a minute and a
/// half for a local server to start.
pub fn check(app: &App, secrets: &Secrets, lock_dir: &Path, path_env: Option<&str>) -> Check {
    let failed = |detail: String| Check { status: apps::FAILED.into(), detail, tools: None, server_name: None };
    if !app.credentials_ready() {
        return Check {
            status: apps::NEEDS_SIGN_IN.into(),
            detail: sign_in_detail(app),
            tools: None,
            server_name: None,
        };
    }
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |message| {
        let _ = tx.lock().unwrap().send(message);
    });
    let upstream = match connect(app, secrets, lock_dir, path_env, false, sink) {
        Ok(upstream) => upstream,
        Err(error) => return failed(error),
    };
    let mut client = Client { upstream, incoming: rx, next: 1 };
    let result = (|| -> Result<Check, SendError> {
        let hello = json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "filo", "title": "Filo", "version": env!("CARGO_PKG_VERSION") },
        });
        let start = if app.transport == apps::STDIO { START_TIMEOUT } else { ANSWER_TIMEOUT };
        let init = client.request("initialize", hello, start)?;
        client.notify("notifications/initialized");
        let server_name = init
            .pointer("/serverInfo/title")
            .or_else(|| init.pointer("/serverInfo/name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let has_tools = init.pointer("/capabilities/tools").is_some();
        let mut tools: Vec<Tool> = Vec::new();
        if has_tools {
            let mut cursor: Option<String> = None;
            for _ in 0..20 {
                let params = cursor.as_ref().map(|cursor| json!({ "cursor": cursor })).unwrap_or_else(|| json!({}));
                let page = client.request("tools/list", params, ANSWER_TIMEOUT)?;
                tools.extend(
                    page.get("tools").and_then(Value::as_array).into_iter().flatten().filter_map(apps::tool_from),
                );
                cursor = page
                    .get("nextCursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                    .map(str::to_string);
                if cursor.is_none() {
                    break;
                }
            }
        }
        let detail = if has_tools { String::new() } else { "It has no tools for the agents.".into() };
        Ok(Check { status: apps::READY.into(), detail, tools: Some(tools), server_name })
    })();
    client.upstream.close();
    match result {
        Ok(check) => check,
        Err(SendError::Unauthorized(_)) => {
            Check { status: apps::NEEDS_SIGN_IN.into(), detail: sign_in_detail(app), tools: None, server_name: None }
        }
        Err(SendError::Failed(message)) => failed(message),
    }
}

// ---- The proxy a turn runs ----

/// Which tools the agents may use, as the proxy decides it: the user's choices, then every tool for an app that starts
/// them all on, else the server's read-only marks as it lists them in this session.
struct Permissions {
    enabled: Vec<String>,
    disabled: Vec<String>,
    /// Every tool the user hasn't turned off is on (apps.rs `App::all_tools`).
    all_tools: bool,
    /// Its preset's checked reading tools, on though unmarked (apps.rs `on_by_default`).
    read_tools: &'static [&'static str],
    /// Tools on before the user says anything, as the server listed them in this session.
    read_only: Mutex<HashSet<String>>,
}

impl Permissions {
    pub fn of(app: &App) -> Permissions {
        Permissions {
            enabled: app.enabled_tools.clone(),
            disabled: app.disabled_tools.clone(),
            all_tools: app.all_tools,
            read_tools: app.read_tools(),
            read_only: Mutex::new(HashSet::new()),
        }
    }

    pub fn allows(&self, tool: &str) -> bool {
        apps::tool_allowed(
            tool,
            self.all_tools || self.read_only.lock().unwrap().contains(tool),
            &self.enabled,
            &self.disabled,
        )
    }

    /// A `tools/list` reply with only the tools the agents may use (the read-only marks noted on the way).
    fn filter_list(&self, mut message: Value) -> Value {
        if let Some(tools) = message.pointer_mut("/result/tools").and_then(Value::as_array_mut) {
            {
                let mut read_only = self.read_only.lock().unwrap();
                for tool in tools.iter() {
                    if let Some(name) = tool.get("name").and_then(Value::as_str) {
                        if apps::tool_from(tool).is_some_and(|known| apps::on_by_default(&known, self.read_tools)) {
                            read_only.insert(name.to_string());
                        } else {
                            read_only.remove(name);
                        }
                    }
                }
            }
            tools.retain(|tool| tool.get("name").and_then(Value::as_str).is_some_and(|name| self.allows(name)));
        }
        message
    }
}

/// A tool turned off: called anyway, it answers with an error the model reads.
fn refused_call(id: &Value, tool: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": [{ "type": "text", "text": format!("The tool `{tool}` isn't allowed: the user hasn't turned it on in Filo (Extensions › Apps). Tell them, if they want it used.") }], "isError": true },
    })
}

/// `filo --local-mcp --app <id> --db <filo.db> [--agent <id>]`: the app's server for a CLI, on stdin/stdout until the
/// CLI closes stdin, for that agent (Filo without one). Returns the exit code.
pub fn proxy_main(args: &[String]) -> i32 {
    let (mut id, mut db_path, mut agent) = (None, None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--app" => id = args.next().cloned(),
            "--db" => db_path = args.next().map(PathBuf::from),
            "--agent" => agent = args.next().cloned(),
            _ => {}
        }
    }
    let (Some(id), Some(db_path)) = (id, db_path) else {
        eprintln!("filo app: --app <id> --db <filo.db> are required");
        return 2;
    };
    let app = match Db::open(&db_path).and_then(|db| apps::find(&db, &id)) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("filo app: {error}");
            return 1;
        }
    };
    // Checked again here, as the app is now: turned off, or this agent's access taken away, since the turn started.
    let agent = agent.unwrap_or_else(|| apps::FILO_AGENT.to_string());
    if !app.enabled || !app.allows_agent(&agent) {
        eprintln!("filo app {}: not allowed for this agent (Extensions › Apps)", app.name);
        return 1;
    }
    let lock_dir = db_path.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let write = {
        let stdout = stdout.clone();
        move |message: &Value| {
            let mut out = stdout.lock().unwrap();
            let _ = serde_json::to_writer(&mut *out, message);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        }
    };
    let permissions = Arc::new(Permissions::of(&app));
    let listing: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let sink: Sink = {
        let (write, permissions, listing) = (write.clone(), permissions.clone(), listing.clone());
        Arc::new(move |message: Value| {
            let listed = message
                .get("id")
                .filter(|_| message.get("method").is_none())
                .is_some_and(|id| listing.lock().unwrap().remove(&id.to_string()));
            let message = if listed { permissions.filter_list(message) } else { message };
            write(&message);
        })
    };
    let upstream: Arc<dyn Upstream> = match connect(&app, &Secrets::system(), &lock_dir, None, true, sink) {
        Ok(upstream) => Arc::from(upstream),
        Err(error) => {
            eprintln!("filo app {}: {error}", app.name);
            return 1;
        }
    };
    let stdio = app.transport == apps::STDIO;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        if let Some(id) = &id {
            match method.as_str() {
                "tools/list" => {
                    listing.lock().unwrap().insert(id.to_string());
                }
                "tools/call" => {
                    let tool = message.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
                    if !permissions.allows(tool) {
                        write(&refused_call(id, tool));
                        continue;
                    }
                }
                _ => {}
            }
        }
        let (upstream, write, name) = (upstream.clone(), write.clone(), app.name.clone());
        let deliver = move || {
            if let Err(error) = upstream.send(&message)
                && let (Some(id), false) = (id, method.is_empty())
            {
                let text = match &error {
                    SendError::Unauthorized(_) => {
                        format!("{name} needs you to sign in again in Filo (Extensions › Apps).")
                    }
                    SendError::Failed(message) => format!("{name}: {message}"),
                };
                write(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": text } }));
            }
        };
        // Over HTTP each message waits for its answer, so they go on threads of their own (calls at once stay at once);
        // a pipe takes them in order.
        if stdio {
            deliver();
        } else {
            std::thread::spawn(deliver);
        }
    }
    upstream.close();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_streams_are_read_event_by_event() {
        let stream = "event: endpoint\ndata: /messages?s=1\n\n: comment\ndata: {\"a\":1}\n\ndata: line one\r\ndata: line two\r\n\r\ndata: last";
        let mut seen = Vec::new();
        read_events(BufReader::new(stream.as_bytes()), |event, data| {
            seen.push((event.to_string(), data.to_string()));
            true
        });
        assert_eq!(
            seen,
            [
                ("endpoint".to_string(), "/messages?s=1".to_string()),
                ("message".to_string(), "{\"a\":1}".to_string()),
                ("message".to_string(), "line one\nline two".to_string()),
                ("message".to_string(), "last".to_string()),
            ]
        );
    }

    #[test]
    fn a_new_apps_tools_are_all_offered_but_those_turned_off() {
        let db = Db::in_memory().unwrap();
        let app = apps::add(
            &db,
            &apps::Fields {
                name: "Git".into(),
                transport: apps::HTTP.into(),
                url: "https://example.com/mcp".into(),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        let app = apps::set_tool_enabled(&db, &app.id, "delete_repo", false, 2).unwrap();
        let permissions = Permissions::of(&app);
        // Nothing listed yet: every tool but the one turned off.
        assert!(permissions.allows("create_issue") && !permissions.allows("delete_repo"));
        let listed = permissions.filter_list(json!({ "id": 2, "result": { "tools": [
            { "name": "list_repos", "annotations": { "readOnlyHint": true } },
            { "name": "create_issue" },
            { "name": "delete_repo", "annotations": { "destructiveHint": true } },
            { "name": "push", "annotations": { "readOnlyHint": false } },
        ] } }));
        let names: Vec<&str> =
            listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_repos", "create_issue", "push"]);
        assert!(permissions.allows("a_tool_added_later") && !permissions.allows("delete_repo"));
    }

    /// An app as one connected before every tool started on (`all_tools` came in off for it).
    fn connected_before(app: App) -> App {
        App { all_tools: false, ..app }
    }

    #[test]
    fn a_presets_checked_tools_are_offered_and_new_ones_wait() {
        let db = Db::in_memory().unwrap();
        let app = apps::add(&db, &apps::preset_fields(apps::preset("deepwiki").unwrap()), 1).unwrap();
        let permissions = Permissions::of(&connected_before(app));
        let listed = permissions.filter_list(json!({ "id": 2, "result": { "tools": [
            { "name": "ask_wiki_question" },
            { "name": "read_wiki_contents" },
            { "name": "a_tool_added_later" },
        ] } }));
        let names: Vec<&str> =
            listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["ask_wiki_question", "read_wiki_contents"]);
        assert!(!permissions.allows("a_tool_added_later"));
    }

    #[test]
    fn only_read_only_and_allowed_tools_are_offered() {
        let db = Db::in_memory().unwrap();
        let app = apps::add(
            &db,
            &apps::Fields {
                name: "Git".into(),
                transport: apps::HTTP.into(),
                url: "https://example.com/mcp".into(),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        let app = apps::set_tool_enabled(&db, &app.id, "create_issue", true, 2).unwrap();
        let app = apps::set_tool_enabled(&db, &app.id, "search", false, 3).unwrap();
        let permissions = Permissions::of(&connected_before(app));
        // Nothing listed yet: only what the user turned on.
        assert!(permissions.allows("create_issue") && !permissions.allows("list_repos"));
        let read_only = json!({ "readOnlyHint": true });
        let listed = permissions.filter_list(json!({ "id": 2, "result": { "tools": [
            { "name": "list_repos", "annotations": read_only },
            { "name": "search", "annotations": read_only },
            { "name": "create_issue" },
            { "name": "delete_repo", "annotations": { "readOnlyHint": false } },
            // Unmarked, on a server of the user's own: off however it's named.
            { "name": "get_repo" },
        ] } }));
        let names: Vec<&str> =
            listed["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_repos", "create_issue"]);
        assert!(
            permissions.allows("list_repos") && !permissions.allows("delete_repo") && !permissions.allows("search")
        );
        let refused = refused_call(&json!(3), "delete_repo");
        assert_eq!(refused["result"]["isError"], json!(true));
        assert_eq!(refused["id"], json!(3));
    }

    #[test]
    fn commands_are_found_on_the_given_path() {
        assert!(resolve_command("sh", Some("/bin")).is_some());
        assert!(resolve_command("/bin/sh", None).is_some());
        assert!(resolve_command("surely-not-a-command-here", Some("/bin")).is_none());
    }
}

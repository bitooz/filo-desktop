// The user's apps end to end, against stand-ins on this Mac:
//
// - A remote server with an account sign-in: a mock MCP server on 127.0.0.1 that answers 401 until signed in, with its
//   protected-resource and authorization-server metadata, dynamic client registration, an authorization page that
//   redirects straight back, and a token endpoint. Filo signs in (PKCE, loopback redirect), checks it, refreshes a
//   revoked token, and the proxy a CLI runs offers only the tools the user allows.
// - A local server with an API key: a Python stdio server that only works with the key in its environment.
// - A whole turn with a stand-in `claude` that finds the app in its --mcp-config, calls its tool through the proxy and
//   sends what came back with send_message.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use filo_local::apps::{self, Fields};
use filo_local::db::{self, Db};
use filo_local::secrets::Secrets;
use filo_local::turn::{self, Report, Runtime};
use filo_local::{Engine, Store, apps_client, apps_oauth};

#[derive(Default)]
struct Mock {
    /// Access tokens the server takes.
    valid: Vec<String>,
    issued: usize,
    registered: usize,
    redirect: Option<String>,
    calls: Vec<String>,
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((key, value)) = header.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length: usize = headers.get("content-length").and_then(|value| value.parse().ok()).unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, headers, body: String::from_utf8_lossy(&body).into_owned() })
}

fn reply(stream: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &str) {
    let mut text = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (key, value) in headers {
        text.push_str(&format!("{key}: {value}\r\n"));
    }
    text.push_str("\r\n");
    text.push_str(body);
    let _ = stream.write_all(text.as_bytes());
}

fn tools() -> Value {
    json!([
        { "name": "list_issues", "description": "Lists issues", "annotations": { "readOnlyHint": true }, "inputSchema": { "type": "object" } },
        { "name": "delete_issue", "description": "Deletes an issue", "inputSchema": { "type": "object" } },
    ])
}

fn serve(listener: TcpListener, mock: Arc<Mutex<Mock>>) {
    let base = format!("http://{}", listener.local_addr().unwrap());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let (mock, base) = (mock.clone(), base.clone());
        std::thread::spawn(move || {
            let Some(request) = read_request(&mut stream) else { return };
            let json_header = ("Content-Type", "application/json".to_string());
            let path = request.path.split('?').next().unwrap_or("").to_string();
            match (request.method.as_str(), path.as_str()) {
                ("POST", "/mcp") => {
                    let token = request
                        .headers
                        .get("authorization")
                        .and_then(|value| value.strip_prefix("Bearer "))
                        .unwrap_or("")
                        .to_string();
                    if !mock.lock().unwrap().valid.contains(&token) {
                        let challenge = format!(
                            r#"Bearer realm="mock", resource_metadata="{base}/.well-known/oauth-protected-resource/mcp", scope="issues""#
                        );
                        return reply(&mut stream, "401 Unauthorized", &[("WWW-Authenticate", challenge)], "");
                    }
                    let message: Value = serde_json::from_str(&request.body).unwrap();
                    let id = message.get("id").cloned();
                    match message["method"].as_str().unwrap_or("") {
                        "initialize" => {
                            let body = json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2025-06-18", "capabilities": { "tools": {} }, "serverInfo": { "name": "mock-issues", "title": "Mock Issues" } } });
                            reply(
                                &mut stream,
                                "200 OK",
                                &[json_header, ("Mcp-Session-Id", "s-1".into())],
                                &body.to_string(),
                            );
                        }
                        "tools/list" => {
                            // As an SSE stream, the way many servers answer.
                            let body = format!(
                                "event: message\ndata: {}\n\n",
                                json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools() } })
                            );
                            reply(&mut stream, "200 OK", &[("Content-Type", "text/event-stream".into())], &body);
                        }
                        "tools/call" => {
                            let name =
                                message.pointer("/params/name").and_then(Value::as_str).unwrap_or("").to_string();
                            mock.lock().unwrap().calls.push(name.clone());
                            let body = json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": format!("{name}: 3 open issues") }] } });
                            reply(&mut stream, "200 OK", &[json_header], &body.to_string());
                        }
                        _ if id.is_none() => reply(&mut stream, "202 Accepted", &[], ""),
                        _ => reply(
                            &mut stream,
                            "200 OK",
                            &[json_header],
                            &json!({ "jsonrpc": "2.0", "id": id, "result": {} }).to_string(),
                        ),
                    }
                }
                ("DELETE", "/mcp") => reply(&mut stream, "204 No Content", &[], ""),
                ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                    let body = json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base] });
                    reply(&mut stream, "200 OK", &[json_header], &body.to_string());
                }
                ("GET", "/.well-known/oauth-authorization-server") => {
                    let body = json!({
                        "issuer": base,
                        "authorization_endpoint": format!("{base}/authorize"),
                        "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register"),
                        "code_challenge_methods_supported": ["S256"],
                    });
                    reply(&mut stream, "200 OK", &[json_header], &body.to_string());
                }
                ("POST", "/register") => {
                    let body: Value = serde_json::from_str(&request.body).unwrap();
                    let mut mock = mock.lock().unwrap();
                    mock.registered += 1;
                    mock.redirect = body["redirect_uris"][0].as_str().map(str::to_string);
                    reply(
                        &mut stream,
                        "201 Created",
                        &[json_header],
                        &json!({ "client_id": format!("client-{}", mock.registered) }).to_string(),
                    );
                }
                ("GET", "/authorize") => {
                    let url = url::Url::parse(&format!("{base}{}", request.path)).unwrap();
                    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
                    assert_eq!(query["code_challenge_method"], "S256");
                    assert_eq!(query["resource"], format!("{base}/mcp"));
                    assert_eq!(query["scope"], "issues");
                    let back = format!("{}?code=code-1&state={}", query["redirect_uri"], query["state"]);
                    reply(&mut stream, "302 Found", &[("Location", back)], "");
                }
                ("POST", "/token") => {
                    let form: HashMap<String, String> =
                        url::form_urlencoded::parse(request.body.as_bytes()).into_owned().collect();
                    let mut mock = mock.lock().unwrap();
                    let ok = match form["grant_type"].as_str() {
                        "authorization_code" => form["code"] == "code-1" && form.contains_key("code_verifier"),
                        "refresh_token" => form["refresh_token"].starts_with("refresh-"),
                        _ => false,
                    };
                    if !ok {
                        return reply(&mut stream, "400 Bad Request", &[json_header], r#"{"error":"invalid_grant"}"#);
                    }
                    mock.issued += 1;
                    let token = format!("token-{}", mock.issued);
                    mock.valid.push(token.clone());
                    let body = json!({ "access_token": token, "token_type": "Bearer", "expires_in": 3600, "refresh_token": format!("refresh-{}", mock.issued) });
                    reply(&mut stream, "200 OK", &[json_header], &body.to_string());
                }
                _ => reply(&mut stream, "404 Not Found", &[], ""),
            }
        });
    }
}

/// Talks to the proxy the way a CLI does: one request, its answer.
struct Proxy {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Proxy {
    fn start(app_id: &str, db_path: &Path, secrets_dir: &Path) -> Proxy {
        let mut child = Command::new(env!("CARGO_BIN_EXE_filo-local"))
            .arg("mcp")
            .args(apps::proxy_args(app_id, db_path, apps::FILO_AGENT))
            .env("FILO_SECRETS_DIR", secrets_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Proxy { child, stdout }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn close(mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn hello(proxy: &mut Proxy) -> Value {
    let init = proxy.request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "cli", "version": "0" } }),
    );
    proxy.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    init
}

fn names(list: &Value) -> Vec<String> {
    list["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_remote_app_signs_in_with_no_filo_account_and_its_proxy_guards_its_tools() {
    let root = common::temp_root("oauth");
    let db_path = root.join(db::FILE);
    let db = Db::open(&db_path).unwrap();
    let secrets_dir = root.join("secrets");
    let secrets = Secrets::Files(secrets_dir.clone());
    let mock = Arc::new(Mutex::new(Mock::default()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    {
        let mock = mock.clone();
        std::thread::spawn(move || serve(listener, mock));
    }

    let app = apps::add(
        &db,
        &Fields {
            name: "Issues".into(),
            transport: apps::HTTP.into(),
            url: format!("{base}/mcp"),
            auth: apps::AUTH_OAUTH.into(),
            ..Default::default()
        },
        1,
    )
    .unwrap();
    // Not signed in: the check says so without asking the server.
    let check = apps_client::check(&app, &secrets, &root, None);
    assert_eq!(check.status, apps::NEEDS_SIGN_IN);

    // The "browser": follows the authorization page's redirect back to Filo's listener.
    let open = |url: &str| -> Result<(), String> {
        let url = url.to_string();
        std::thread::spawn(move || {
            let _ = reqwest::blocking::get(url);
        });
        Ok(())
    };
    let cancel = AtomicBool::new(false);
    apps_oauth::sign_in(&db, &secrets, &app.id, &cancel, &open).unwrap();
    let app = apps::find(&db, &app.id).unwrap();
    assert!(app.has_secret && app.oauth.registered_client_id.as_deref() == Some("client-1"));
    assert!(mock.lock().unwrap().redirect.as_deref().unwrap().starts_with("http://127.0.0.1:"));
    // Nothing secret in the database.
    let row = serde_json::to_string(&app).unwrap();
    assert!(!row.contains("token-1") && !row.contains("refresh-1"));

    let check = apps_client::check(&app, &secrets, &root, None);
    assert_eq!(
        (check.status.as_str(), check.server_name.as_deref()),
        (apps::READY, Some("Mock Issues")),
        "{}",
        check.detail
    );
    let found = check.tools.clone().unwrap();
    assert_eq!(
        found.iter().map(|tool| (tool.name.as_str(), tool.read_only)).collect::<Vec<_>>(),
        [("list_issues", true), ("delete_issue", false)]
    );
    apps::record_check(&db, &app.id, &check, 2).unwrap();

    // The server stops taking the token: the next request refreshes it, once.
    mock.lock().unwrap().valid.clear();
    let check = apps_client::check(&apps::find(&db, &app.id).unwrap(), &secrets, &root, None);
    assert_eq!(check.status, apps::READY, "{}", check.detail);
    assert_eq!(mock.lock().unwrap().issued, 2);

    // A CLI's proxy: a new app's every tool, the one that changes things too.
    let mut proxy = Proxy::start(&app.id, &db_path, &secrets_dir);
    assert_eq!(hello(&mut proxy)["result"]["serverInfo"]["name"], "mock-issues");
    assert_eq!(names(&proxy.request(2, "tools/list", json!({}))), ["list_issues", "delete_issue"]);
    let answered = proxy.request(3, "tools/call", json!({ "name": "list_issues", "arguments": {} }));
    assert_eq!(answered["result"]["content"][0]["text"], "list_issues: 3 open issues");
    proxy.close();
    assert_eq!(mock.lock().unwrap().calls, ["list_issues"]);

    // Turned off by the user: left out of the list, and its call refused before it reaches the server.
    apps::set_tool_enabled(&db, &app.id, "delete_issue", false, 3).unwrap();
    let mut proxy = Proxy::start(&app.id, &db_path, &secrets_dir);
    hello(&mut proxy);
    assert_eq!(names(&proxy.request(2, "tools/list", json!({}))), ["list_issues"]);
    let refused = proxy.request(3, "tools/call", json!({ "name": "delete_issue", "arguments": {} }));
    assert_eq!(refused["result"]["isError"], json!(true));
    proxy.close();
    assert_eq!(mock.lock().unwrap().calls, ["list_issues"]);

    // Signing in again reuses the client Filo registered when its port is free, else registers another.
    apps_oauth::sign_in(&db, &secrets, &app.id, &cancel, &open).unwrap();
    assert!(mock.lock().unwrap().registered <= 2);

    // Signed out: no tokens kept, and turns leave it out.
    let app = apps::sign_out(&db, &secrets, &app.id, 4).unwrap();
    assert!(!app.has_secret && app.status == apps::NEEDS_SIGN_IN);
    assert!(apps::plugged(&db, Engine::Claude, apps::FILO_AGENT).unwrap().is_empty());
    assert!(
        std::fs::read_dir(&secrets_dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|file| !file.file_name().to_string_lossy().contains("tokens"))
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn a_server_that_cant_register_filo_says_what_to_do() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    {
        let base = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let base = base.clone();
                std::thread::spawn(move || {
                    let Some(request) = read_request(&mut stream) else { return };
                    let json_header = ("Content-Type", "application/json".to_string());
                    match request.path.as_str() {
                        "/mcp" => reply(&mut stream, "401 Unauthorized", &[("WWW-Authenticate", format!(r#"Bearer resource_metadata="{base}/prm""#))], ""),
                        "/prm" => reply(&mut stream, "200 OK", &[json_header], &json!({ "authorization_servers": [base] }).to_string()),
                        // Like GitHub's: endpoints, but no registration.
                        "/.well-known/oauth-authorization-server" => reply(
                            &mut stream,
                            "200 OK",
                            &[json_header],
                            &json!({ "issuer": base, "authorization_endpoint": format!("{base}/a"), "token_endpoint": format!("{base}/t") }).to_string(),
                        ),
                        _ => reply(&mut stream, "404 Not Found", &[], ""),
                    }
                });
            }
        });
    }
    let root = common::temp_root("no-dcr");
    let db = Db::open(&root.join(db::FILE)).unwrap();
    let app = apps::add(
        &db,
        &Fields {
            name: "Locked".into(),
            transport: apps::HTTP.into(),
            url: format!("{base}/mcp"),
            auth: apps::AUTH_OAUTH.into(),
            ..Default::default()
        },
        1,
    )
    .unwrap();
    let opened = AtomicBool::new(false);
    let open = |_: &str| -> Result<(), String> {
        opened.store(true, Ordering::SeqCst);
        Ok(())
    };
    let error = apps_oauth::sign_in(&db, &Secrets::memory(), &app.id, &AtomicBool::new(false), &open).unwrap_err();
    assert!(
        error.contains("doesn’t let apps register themselves") && error.contains("http://127.0.0.1:33418/callback"),
        "{error}"
    );
    assert!(!opened.load(Ordering::SeqCst));
    std::fs::remove_dir_all(root).ok();
}

const KEYED_SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message["method"]
    if method == "initialize":
        if os.environ.get("NOTES_TOKEN") != "sekrit":
            sys.stderr.write("NOTES_TOKEN is missing or wrong\n")
            sys.exit(1)
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "notes-local"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "find_note", "annotations": {"readOnlyHint": True}, "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "found: " + json.dumps(message["params"].get("arguments", {}))}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#;

/// A stand-in `claude` that calls the app's tool through its server in --mcp-config, then sends what it got.
const FAKE_CLAUDE: &str = r##"#!/usr/bin/env python3
import json, subprocess, sys
args = sys.argv[1:]
servers = json.loads(args[args.index("--mcp-config") + 1])["mcpServers"]
allowed = [arg for arg in args if arg.startswith("--allowedTools=")][0].split("=", 1)[1].split(",")
# Stdin stays open while the turn runs (stream-json input): read up to the message.
for line in iter(sys.stdin.readline, ""):
    if line.strip() and json.loads(line)["type"] == "user":
        break
def start(config):
    return subprocess.Popen([config["command"]] + config["args"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def call(server, id, method, params):
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    server.stdin.flush()
    return json.loads(server.stdout.readline())
hello = {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}}
app = start(servers["filo-notes"])
call(app, 1, "initialize", hello)
tools = [tool["name"] for tool in call(app, 2, "tools/list", {})["result"]["tools"]]
found = call(app, 3, "tools/call", {"name": "find_note", "arguments": {"q": "milk"}})["result"]["content"][0]["text"]
app.stdin.close()
app.wait()
filo = start(servers["filo"])
call(filo, 1, "initialize", hello)
seen = {"tools": tools, "found": found, "allowed": "mcp__filo-notes" in allowed}
call(filo, 2, "tools/call", {"name": "send_message", "arguments": {"text": json.dumps(seen)}})
filo.stdin.close()
filo.wait()
print(json.dumps({"type": "system", "subtype": "init", "session_id": "s"}))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "", "session_id": "s"}))
"##;

#[test]
fn a_local_app_gets_its_key_and_a_turns_cli_uses_it() {
    let root = common::temp_root("stdio");
    let store = Store::at(&root);
    let db_path = root.join(db::FILE);
    let db = Db::open(&db_path).unwrap();
    let bin = root.join("bin");
    common::stand_in(&bin, "notes-server", KEYED_SERVER);
    common::stand_in(&bin, "claude", FAKE_CLAUDE);
    let secrets_dir = root.join("secrets");
    let secrets = Secrets::Files(secrets_dir.clone());

    let fields = Fields {
        name: "Notes".into(),
        transport: apps::STDIO.into(),
        command: bin.join("notes-server").display().to_string(),
        auth: apps::AUTH_KEY.into(),
        key_name: "NOTES_TOKEN".into(),
        ..Default::default()
    };
    let app = apps::add(&db, &fields, 1).unwrap();
    assert_eq!(apps_client::check(&app, &secrets, &root, None).status, apps::NEEDS_SIGN_IN);
    let app = apps::set_api_key(&db, &secrets, &app.id, "wrong", 2).unwrap();
    let check = apps_client::check(&app, &secrets, &root, None);
    assert_eq!(check.status, apps::FAILED);
    let app = apps::set_api_key(&db, &secrets, &app.id, "sekrit", 3).unwrap();
    let check = apps_client::check(&app, &secrets, &root, None);
    assert_eq!((check.status.as_str(), check.tools.as_ref().map(Vec::len)), (apps::READY, Some(1)), "{}", check.detail);
    apps::record_check(&db, &app.id, &check, 4).unwrap();

    // A turn: the stand-in CLI reaches the app through the proxy (which reads the key) and replies with what it found.
    // SAFETY: set before any thread of this test starts a child that reads it.
    unsafe { std::env::set_var("FILO_SECRETS_DIR", &secrets_dir) };
    let runtime = common::runtime(Some(common::path_with(&bin)), Some(db_path.clone()), Duration::from_secs(60));
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "find my milk note", "turn-1").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-1", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    let reply = store.messages(&chat.id).unwrap().into_iter().last().unwrap();
    let seen: Value = serde_json::from_str(&reply.text).unwrap();
    assert_eq!(seen, json!({ "tools": ["find_note"], "found": "found: {\"q\": \"milk\"}", "allowed": true }));

    // Off for Claude Code: the next turn's CLI doesn't get it.
    apps::set_engine(&db, &app.id, Engine::Claude, false, 5).unwrap();
    assert!(
        apps::servers_for(&db_path, Engine::Claude, apps::FILO_AGENT, &runtime.mcp_program, &runtime.mcp_args)
            .is_empty()
    );
    assert_eq!(
        apps::servers_for(&db_path, Engine::Codex, apps::FILO_AGENT, &runtime.mcp_program, &runtime.mcp_args).len(),
        1
    );

    // Only for Chef: Filo's chats (a turn with no agent is Filo's) and other agents don't get it, Chef's turns do.
    apps::set_engine(&db, &app.id, Engine::Claude, true, 6).unwrap();
    apps::set_agent_access(&db, &app.id, false, &["chef".into()], 7).unwrap();
    let servers =
        |agent: &str| apps::servers_for(&db_path, Engine::Claude, agent, &runtime.mcp_program, &runtime.mcp_args);
    assert!(servers(apps::FILO_AGENT).is_empty() && servers("tutor").is_empty());
    assert_eq!(servers("chef").len(), 1);

    let as_chef = Runtime { agent: Some("chef".into()), ..runtime.clone() };
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "find my milk note", "turn-2").unwrap();
    let report = turn::run(&store, &as_chef, &chat.id, "turn-2", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    // As Filo the CLI has no `filo-notes` server: the stand-in fails looking for it, and nothing reaches the app.
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "find my milk note", "turn-3").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-3", |_| {});
    assert_eq!(report.replies, 0);

    // The proxy checks too, when it starts: not for Tutor.
    let refused = Command::new(env!("CARGO_BIN_EXE_filo-local"))
        .arg("mcp")
        .args(apps::proxy_args(&app.id, &db_path, "tutor"))
        .env("FILO_SECRETS_DIR", &secrets_dir)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("not allowed for this agent")
    );
    std::fs::remove_dir_all(root).ok();
}

/// The real services: Connect on Linear's, Notion's, Upsy's and Executor's presets finds their sign-in, registers Filo
/// (no Filo account, no client made by hand) and gets as far as the browser's approval page, then stops
/// (`cargo test -- --ignored`).
#[test]
#[ignore]
fn ready_made_accounts_connect_straight_to_their_approval_page() {
    for id in ["linear", "notion", "upsy", "executor"] {
        let root = common::temp_root(id);
        let db = Db::open(&root.join(db::FILE)).unwrap();
        let app = apps::add(&db, &apps::preset_fields(apps::preset(id).unwrap()), 1).unwrap();
        assert_eq!(app.auth, apps::AUTH_OAUTH);
        let opened: Mutex<Option<String>> = Mutex::new(None);
        let cancel = AtomicBool::new(false);
        let open = |url: &str| -> Result<(), String> {
            *opened.lock().unwrap() = Some(url.to_string());
            // Nobody approves: the sign-in stops here.
            cancel.store(true, Ordering::SeqCst);
            Ok(())
        };
        let error = apps_oauth::sign_in(&db, &Secrets::memory(), &app.id, &cancel, &open).unwrap_err();
        assert!(error.contains("cancelled"), "{id}: {error}");
        let url = url::Url::parse(opened.lock().unwrap().as_deref().expect("the browser opened")).unwrap();
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        println!(
            "{id}: {}://{}{} client_id={}… redirect={} resource={} pkce={}",
            url.scheme(),
            url.host_str().unwrap(),
            url.path(),
            &query["client_id"][..8.min(query["client_id"].len())],
            query["redirect_uri"],
            query["resource"],
            query["code_challenge_method"]
        );
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
        assert!(!query["client_id"].is_empty());
        assert!(apps::find(&db, &app.id).unwrap().oauth.registered_client_id.is_some());
        std::fs::remove_dir_all(root).ok();
    }
}

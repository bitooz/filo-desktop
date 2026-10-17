// Signing in to an app's account (apps.rs, auth `oauth`) the way MCP's authorization spec has a native app do it, with
// no Filo account or server in between:
//
// 1. Find the sign-in: the server's 401 names its protected-resource metadata (or it's at the well-known address),
//    which names the authorization server, whose metadata has its endpoints. An older server that has neither is its
//    own authorization server at `/authorize`, `/token` and `/register`.
// 2. Have a client: the one the user made at the provider (its id in the app's settings, its secret in the Keychain,
//    redirect `http://127.0.0.1:33418/callback`), else one Filo registers for itself with dynamic client registration,
//    kept and reused while its redirect's port is free. A provider that offers neither (GitHub, Google…) can't be
//    signed in to until the user makes a client there; the error says so.
// 3. Sign in: the browser opens the authorization page (PKCE S256, a random state, the server as `resource`), and a
//    one-shot listener on 127.0.0.1 takes the redirect, then the code is traded for tokens.
// 4. Keep them: tokens in the Keychain. The proxy (apps_client.rs) refreshes the access token when it runs out or the
//    server refuses it, under a file lock so turns running at once don't each spend a rotating refresh token.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::apps::{self, App};
use crate::apps_client;
use crate::db::Db;
use crate::secrets::{self, Secrets};

/// The redirect's port for a client the user made at the provider: one fixed address they can register.
pub const FIXED_PORT: u16 = 33418;
/// The secret of a client Filo registered itself.
const SECRET_REGISTERED: &str = "registered-secret";
/// How long the browser has to come back.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// An access token this close to running out is refreshed first.
const EXPIRY_MARGIN_MS: i64 = 60_000;

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

/// What the Keychain keeps for a sign-in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// When the access token runs out (ms), when the server said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Where and how a server's sign-in works.
#[derive(Clone, Debug, Default, PartialEq)]
struct Discovery {
    /// The server as the token's audience (`resource`).
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes: Option<String>,
    pub token_auth_methods: Vec<String>,
}

/// `Bearer realm="x", resource_metadata="https://…", scope="a b"` → its parameters.
fn challenge_params(header: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let rest = header.trim();
    let rest = rest.strip_prefix("Bearer").or_else(|| rest.strip_prefix("bearer")).unwrap_or(rest);
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect::<String>().trim().to_ascii_lowercase();
        if key.is_empty() {
            break;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => value.extend(chars.next()),
                    '"' => break,
                    c => value.push(c),
                }
            }
        } else {
            while let Some(c) = chars.peek().copied() {
                if c == ',' || c.is_whitespace() {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
        params.insert(key, value);
    }
    params
}

/// RFC 8414 / 9728 addresses: the well-known part between the host and the path (`https://h/.well-known/x/path`).
fn well_known(url: &url::Url, suffix: &str) -> String {
    let path = url.path().trim_end_matches('/');
    format!(
        "{}://{}/.well-known/{suffix}{path}",
        url.scheme(),
        url.host_str().unwrap_or_default().to_string() + &url.port().map(|port| format!(":{port}")).unwrap_or_default()
    )
}

fn origin(url: &url::Url) -> String {
    format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port().map(|port| format!(":{port}")).unwrap_or_default()
    )
}

fn get_json(http: &reqwest::blocking::Client, url: &str) -> Option<Value> {
    let response = http
        .get(url)
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", apps_client::PROTOCOL)
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().ok().filter(Value::is_object)
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_else(|| url.to_string())
}

/// Finds the server's sign-in (step 1).
fn discover(http: &reqwest::blocking::Client, server_url: &str) -> Result<Discovery, String> {
    let server = url::Url::parse(server_url).map_err(|_| "That isn't a web address.".to_string())?;
    let host = host_of(server_url);
    let hello = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": apps_client::PROTOCOL, "capabilities": {}, "clientInfo": { "name": "filo", "version": env!("CARGO_PKG_VERSION") } } });
    let response = http
        .post(server_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(hello.to_string())
        .send()
        .map_err(|error| format!("Couldn't reach {host}: {error}"))?;
    let challenge = match response.status().as_u16() {
        401 | 403 => response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .map(challenge_params)
            .unwrap_or_default(),
        status if (200..300).contains(&status) => {
            return Err(format!("{host} answers without a sign-in. Pick “No sign-in” for it."));
        }
        _ => HashMap::new(),
    };
    let mut candidates: Vec<String> = challenge.get("resource_metadata").cloned().into_iter().collect();
    candidates.push(well_known(&server, "oauth-protected-resource"));
    candidates.push(format!("{}/.well-known/oauth-protected-resource", origin(&server)));
    candidates.dedup();
    let resource_metadata = candidates
        .iter()
        .find_map(|url| get_json(http, url).filter(|meta| meta.get("authorization_servers").is_some()));
    let issuer = resource_metadata
        .as_ref()
        .and_then(|meta| meta.get("authorization_servers")?.as_array()?.first()?.as_str().map(str::to_string))
        .unwrap_or_else(|| origin(&server));
    let resource = resource_metadata
        .as_ref()
        .and_then(|meta| meta.get("resource")?.as_str().map(str::to_string))
        .unwrap_or_else(|| server_url.to_string());
    let scopes = challenge.get("scope").cloned().filter(|scope| !scope.trim().is_empty()).or_else(|| {
        let listed: Vec<String> = resource_metadata
            .as_ref()?
            .get("scopes_supported")?
            .as_array()?
            .iter()
            .filter_map(|scope| scope.as_str().map(str::to_string))
            .collect();
        (!listed.is_empty()).then(|| listed.join(" "))
    });
    let issuer_url =
        url::Url::parse(&issuer).map_err(|_| format!("{host} names a sign-in that isn't a web address."))?;
    let has_path = !issuer_url.path().trim_matches('/').is_empty();
    let mut meta_candidates =
        vec![well_known(&issuer_url, "oauth-authorization-server"), well_known(&issuer_url, "openid-configuration")];
    if has_path {
        meta_candidates.push(format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/')));
    }
    let meta = meta_candidates.iter().find_map(|url| {
        get_json(http, url)
            .filter(|meta| meta.get("authorization_endpoint").is_some() && meta.get("token_endpoint").is_some())
    });
    let field = |meta: &Value, key: &str| meta.get(key).and_then(Value::as_str).map(str::to_string);
    let discovery = match meta {
        Some(meta) => Discovery {
            resource,
            issuer: field(&meta, "issuer").unwrap_or(issuer),
            authorization_endpoint: field(&meta, "authorization_endpoint").unwrap_or_default(),
            token_endpoint: field(&meta, "token_endpoint").unwrap_or_default(),
            registration_endpoint: field(&meta, "registration_endpoint"),
            scopes,
            token_auth_methods: meta
                .get("token_endpoint_auth_methods_supported")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|method| method.as_str().map(str::to_string))
                .collect(),
        },
        // An older server (MCP 2025-03-26) is its own authorization server at the default paths, if it has one at all.
        None if resource_metadata.is_none() && !challenge.is_empty() => {
            let base = origin(&server);
            Discovery {
                resource,
                issuer: base.clone(),
                authorization_endpoint: format!("{base}/authorize"),
                token_endpoint: format!("{base}/token"),
                registration_endpoint: Some(format!("{base}/register")),
                scopes,
                token_auth_methods: Vec::new(),
            }
        }
        None if challenge.is_empty() && resource_metadata.is_none() => {
            return Err(format!(
                "{host} didn’t ask for a sign-in or say how to sign in. Try “No sign-in” or an API key."
            ));
        }
        None => return Err(format!("Couldn’t find {host}’s sign-in (its authorization server has no metadata).")),
    };
    Ok(discovery)
}

/// What to say when the provider lets no app register itself and the user hasn't made a client there.
fn no_registration(host: &str) -> String {
    format!(
        "{host} doesn’t let apps register themselves for sign-in (no dynamic client registration), so Filo can’t sign in to it on its own, \
         and Filo has no account or hosted client to borrow. Make an OAuth app in {host}’s developer settings with the redirect URL {}, \
         then paste its client ID (and secret, if it gives one) under Sign-in client. Or connect with an API key, if {host} offers one.",
        redirect_uri(FIXED_PORT)
    )
}

/// A client Filo registered (step 2).
#[derive(Clone, Debug, PartialEq)]
struct Registered {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_method: Option<String>,
}

fn register(
    http: &reqwest::blocking::Client,
    endpoint: &str,
    redirect: &str,
    scopes: Option<&str>,
) -> Result<Registered, String> {
    let mut body = json!({
        "client_name": "Filo",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": "native",
    });
    if let Some(scopes) = scopes {
        body["scope"] = json!(scopes);
    }
    let host = host_of(endpoint);
    let response = http
        .post(endpoint)
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .map_err(|error| format!("Couldn't reach {host} to register Filo: {error}"))?;
    let status = response.status();
    let reply: Value = response.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let reason = reply
            .get("error_description")
            .or_else(|| reply.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("it refused");
        return Err(format!(
            "{host} wouldn’t register Filo for sign-in ({reason}). {}",
            "Make an OAuth app there yourself and paste its client ID under Sign-in client."
        ));
    }
    let client_id = reply
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("{host} registered Filo but gave no client id."))?;
    Ok(Registered {
        client_id: client_id.to_string(),
        client_secret: reply
            .get("client_secret")
            .and_then(Value::as_str)
            .filter(|secret| !secret.is_empty())
            .map(str::to_string),
        auth_method: reply.get("token_endpoint_auth_method").and_then(Value::as_str).map(str::to_string),
    })
}

fn random_token() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn code_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// The client a token request signs with.
#[derive(Clone, Debug)]
struct ClientAuth {
    pub client_id: String,
    pub client_secret: Option<String>,
    /// `client_secret_post` sends the secret in the form; otherwise HTTP Basic.
    pub method: Option<String>,
}

/// Posts to the token endpoint and reads the tokens.
fn token_request(
    http: &reqwest::blocking::Client,
    endpoint: &str,
    client: &ClientAuth,
    form: &[(&str, &str)],
) -> Result<Tokens, String> {
    let mut fields: Vec<(&str, &str)> = form.to_vec();
    fields.push(("client_id", &client.client_id));
    let mut request = http.post(endpoint).header("Accept", "application/json");
    if let Some(secret) = &client.client_secret {
        if client.method.as_deref() == Some("client_secret_post") {
            fields.push(("client_secret", secret));
        } else {
            let encode = |text: &str| url::form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>();
            let basic = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                encode(&client.client_id),
                encode(secret)
            ));
            request = request.header("Authorization", format!("Basic {basic}"));
        }
    }
    let host = host_of(endpoint);
    let response = request.form(&fields).send().map_err(|error| format!("Couldn't reach {host}: {error}"))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    // JSON, or the form-encoded answer some providers still give.
    let reply: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
        Value::Object(
            url::form_urlencoded::parse(text.as_bytes())
                .map(|(key, value)| (key.into_owned(), Value::String(value.into_owned())))
                .collect(),
        )
    });
    let access = reply.get("access_token").and_then(Value::as_str).filter(|token| !token.is_empty());
    let Some(access) = access.filter(|_| status.is_success()) else {
        let reason = reply
            .get("error_description")
            .or_else(|| reply.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("no token came back");
        return Err(format!("{host} didn’t sign you in: {reason}."));
    };
    let expires_in = reply.get("expires_in").and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()));
    Ok(Tokens {
        access_token: access.to_string(),
        refresh_token: reply
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string),
        expires_at: expires_in.map(|seconds| crate::now_ms() as i64 + seconds * 1000),
        scope: reply.get("scope").and_then(Value::as_str).map(str::to_string),
    })
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn page(title: &str, line: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title><body style=\"font:16px -apple-system,sans-serif;display:grid;place-items:center;height:90vh;color:#333\"><div style=\"text-align:center\"><h2>{title}</h2><p>{line}</p></div>",
        title = html_escape(title),
        line = html_escape(line)
    )
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// Waits for the browser to come back to `/callback` with the code for `state`. Other requests (a favicon) get a 404.
fn wait_for_code(listener: &TcpListener, state: &str, name: &str, cancel: &AtomicBool) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("Sign-in cancelled.".into());
        }
        if Instant::now() > deadline {
            return Err("The sign-in took too long. Try again.".into());
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(format!("The sign-in's listener failed: {error}")),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut request = Vec::new();
        let mut buffer = [0u8; 2048];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") && request.len() < 16 * 1024 {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => request.extend_from_slice(&buffer[..count]),
            }
        }
        let request = String::from_utf8_lossy(&request);
        let target = request.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("");
        let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(&mut stream, "400 Bad Request", "");
            continue;
        };
        if url.path() != "/callback" {
            respond(&mut stream, "404 Not Found", "");
            continue;
        }
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if query.get("state").map(String::as_str) != Some(state) {
            respond(
                &mut stream,
                "400 Bad Request",
                &page("That sign-in isn’t Filo’s", "Start the sign-in again from Filo."),
            );
            continue;
        }
        if let Some(error) = query.get("error") {
            let reason = query.get("error_description").cloned().unwrap_or_else(|| error.clone());
            respond(
                &mut stream,
                "200 OK",
                &page("Not signed in", &format!("{name}: {reason}. You can close this tab.")),
            );
            return Err(if error == "access_denied" {
                "You didn’t allow the sign-in.".into()
            } else {
                format!("The sign-in failed: {reason}.")
            });
        }
        let Some(code) = query.get("code").filter(|code| !code.is_empty()) else {
            respond(
                &mut stream,
                "400 Bad Request",
                &page("Not signed in", "No code came back. Start the sign-in again from Filo."),
            );
            return Err("The sign-in came back without a code.".into());
        };
        respond(
            &mut stream,
            "200 OK",
            &page(&format!("Signed in to {name}"), "You can close this tab and go back to Filo."),
        );
        return Ok(code.clone());
    }
}

fn bind(port: u16) -> Option<TcpListener> {
    TcpListener::bind(("127.0.0.1", port)).ok()
}

fn port_of(redirect: &str) -> Option<u16> {
    url::Url::parse(redirect).ok()?.port()
}

/// Signs in to the app's account (steps 1 to 4): opens the browser with `open` and blocks until the browser comes back,
/// `cancel` is set or five minutes pass. The tokens are in the Keychain after.
pub fn sign_in(
    db: &Db,
    secrets: &Secrets,
    app_id: &str,
    cancel: &AtomicBool,
    open: &dyn Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    let app = apps::find(db, app_id)?;
    if app.auth != apps::AUTH_OAUTH || app.transport != apps::HTTP {
        return Err("This app doesn't sign in with an account.".into());
    }
    let http = apps_client::http_client(Duration::from_secs(30))?;
    let found = discover(&http, &app.url)?;
    let host = host_of(&app.url);
    let scopes = app.oauth.scopes.clone().or_else(|| found.scopes.clone());
    let mut oauth = app.oauth.clone();
    let (client, listener, redirect) = match &app.oauth.client_id {
        Some(client_id) => {
            let listener = bind(FIXED_PORT).ok_or_else(|| format!("Port {FIXED_PORT} on this Mac is busy, and your client's redirect is http://127.0.0.1:{FIXED_PORT}/callback. Close what uses it and try again."))?;
            let secret = if app.oauth.client_secret {
                secrets.get_for(&secrets::account(app_id, apps::SECRET_CLIENT), &apps::origin_of(&app.url))?
            } else {
                None
            };
            let method = found
                .token_auth_methods
                .iter()
                .find(|method| method.as_str() == "client_secret_basic" || method.as_str() == "client_secret_post")
                .cloned();
            (
                ClientAuth { client_id: client_id.clone(), client_secret: secret, method },
                listener,
                redirect_uri(FIXED_PORT),
            )
        }
        None => {
            // The client Filo registered before, if it was for this sign-in and its port is free.
            let reused = match (&oauth.registered_client_id, &oauth.registered_redirect) {
                (Some(client_id), Some(redirect)) if oauth.issuer.as_deref() == Some(found.issuer.as_str()) => {
                    port_of(redirect).and_then(bind).map(|listener| (client_id.clone(), redirect.clone(), listener))
                }
                _ => None,
            };
            match reused {
                Some((client_id, redirect, listener)) => {
                    let secret =
                        secrets.get_for(&secrets::account(app_id, SECRET_REGISTERED), &apps::origin_of(&app.url))?;
                    (
                        ClientAuth { client_id, client_secret: secret, method: oauth.registered_auth_method.clone() },
                        listener,
                        redirect,
                    )
                }
                None => {
                    let endpoint = found.registration_endpoint.clone().ok_or_else(|| no_registration(&host))?;
                    let listener = bind(0).ok_or("Couldn't listen on this Mac for the sign-in's redirect.")?;
                    let port = listener.local_addr().map_err(|error| error.to_string())?.port();
                    let redirect = redirect_uri(port);
                    let registered = register(&http, &endpoint, &redirect, scopes.as_deref())?;
                    let account = secrets::account(app_id, SECRET_REGISTERED);
                    match &registered.client_secret {
                        Some(secret) => secrets.set_for(&account, secret, &apps::origin_of(&app.url))?,
                        None => secrets.delete(&account)?,
                    }
                    oauth.registered_client_id = Some(registered.client_id.clone());
                    oauth.registered_redirect = Some(redirect.clone());
                    oauth.registered_auth_method = registered.auth_method.clone();
                    oauth.issuer = Some(found.issuer.clone());
                    apps::save_oauth(db, app_id, &oauth, crate::now_ms() as i64)?;
                    (
                        ClientAuth {
                            client_id: registered.client_id,
                            client_secret: registered.client_secret,
                            method: registered.auth_method,
                        },
                        listener,
                        redirect,
                    )
                }
            }
        }
    };
    let verifier = random_token();
    let state = random_token();
    let mut authorize = url::Url::parse(&found.authorization_endpoint)
        .map_err(|_| format!("{host}'s sign-in page isn't a web address."))?;
    {
        let mut query = authorize.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("state", &state)
            .append_pair("code_challenge", &code_challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &found.resource);
        if let Some(scopes) = &scopes {
            query.append_pair("scope", scopes);
        }
    }
    open(authorize.as_str())?;
    let code = wait_for_code(&listener, &state, &app.name, cancel)?;
    drop(listener);
    let tokens = token_request(
        &http,
        &found.token_endpoint,
        &client,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
            ("resource", &found.resource),
        ],
    )?;
    let to = apps::sign_in_destination(&app.url, Some(&found.token_endpoint));
    secrets.set_for(
        &secrets::account(app_id, apps::SECRET_TOKENS),
        &serde_json::to_string(&tokens).map_err(|error| error.to_string())?,
        &to,
    )?;
    oauth.issuer = Some(found.issuer.clone());
    oauth.token_endpoint = Some(found.token_endpoint.clone());
    oauth.resource = Some(found.resource.clone());
    oauth.token_auth_method = client.method.clone();
    oauth.signed_in_at = Some(crate::now_ms() as i64);
    let now = crate::now_ms() as i64;
    apps::save_oauth(db, app_id, &oauth, now)?;
    apps::set_has_secret(db, app_id, true)?;
    Ok(())
}

/// Holds an exclusive lock on a file while it lives (`flock`), across processes.
struct FileLock(std::fs::File);

impl FileLock {
    fn acquire(path: &Path) -> Result<FileLock, String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|error| format!("Couldn't lock the sign-in: {error}"))?;
        crate::lock::flock(&file, libc::LOCK_EX).map_err(|error| format!("Couldn't lock the sign-in: {error}"))?;
        Ok(FileLock(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        crate::lock::unlock(&self.0);
    }
}

/// The app's tokens, while they're for its server and token endpoint as its row has them now.
fn read_tokens(secrets: &Secrets, app: &App) -> Result<Option<Tokens>, String> {
    let to = apps::sign_in_destination(&app.url, app.oauth.token_endpoint.as_deref());
    Ok(secrets
        .get_for(&secrets::account(&app.id, apps::SECRET_TOKENS), &to)?
        .and_then(|text| serde_json::from_str(&text).ok()))
}

fn usable(tokens: &Tokens, now: i64) -> bool {
    tokens.expires_at.is_none_or(|at| at - EXPIRY_MARGIN_MS > now)
}

/// The app's access token for a request: the one kept while it's good, else (or with `fresh`, after the server refused
/// it) a refreshed one. Another process's refresh, done while this one waited for the lock, is used as it is.
pub fn access_token(app: &App, secrets: &Secrets, lock_dir: &Path, fresh: bool) -> Result<String, String> {
    let signed_out = || format!("Not signed in to {}. Sign in again in Filo (Extensions › Apps).", app.name);
    let tokens = read_tokens(secrets, app)?.ok_or_else(signed_out)?;
    let now = crate::now_ms() as i64;
    if !fresh && usable(&tokens, now) {
        return Ok(tokens.access_token);
    }
    let _lock = FileLock::acquire(&lock_dir.join("apps").join(format!("{}.lock", app.id)))?;
    let current = read_tokens(secrets, app)?.ok_or_else(signed_out)?;
    if current.access_token != tokens.access_token && usable(&current, now) {
        return Ok(current.access_token);
    }
    let refresh = current
        .refresh_token
        .clone()
        .ok_or_else(|| format!("The sign-in to {} ran out. Sign in again in Filo (Extensions › Apps).", app.name))?;
    let endpoint = app.oauth.token_endpoint.clone().ok_or_else(signed_out)?;
    let client = match &app.oauth.client_id {
        Some(client_id) => ClientAuth {
            client_id: client_id.clone(),
            client_secret: if app.oauth.client_secret {
                secrets.get_for(&secrets::account(&app.id, apps::SECRET_CLIENT), &apps::origin_of(&app.url))?
            } else {
                None
            },
            method: app.oauth.token_auth_method.clone(),
        },
        None => ClientAuth {
            client_id: app.oauth.registered_client_id.clone().ok_or_else(signed_out)?,
            client_secret: secrets
                .get_for(&secrets::account(&app.id, SECRET_REGISTERED), &apps::origin_of(&app.url))?,
            method: app.oauth.registered_auth_method.clone(),
        },
    };
    let http = apps_client::http_client(Duration::from_secs(30))?;
    let mut form: Vec<(&str, &str)> = vec![("grant_type", "refresh_token"), ("refresh_token", &refresh)];
    if let Some(resource) = &app.oauth.resource {
        form.push(("resource", resource));
    }
    let mut renewed = token_request(&http, &endpoint, &client, &form)
        .map_err(|error| format!("{error} Sign in again in Filo (Extensions › Apps)."))?;
    if renewed.refresh_token.is_none() {
        renewed.refresh_token = Some(refresh);
    }
    let to = apps::sign_in_destination(&app.url, Some(&endpoint));
    secrets.set_for(
        &secrets::account(&app.id, apps::SECRET_TOKENS),
        &serde_json::to_string(&renewed).map_err(|error| error.to_string())?,
        &to,
    )?;
    Ok(renewed.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenges_and_well_known_addresses_are_read_like_the_specs() {
        let params = challenge_params(
            r#"Bearer realm="OAuth", resource_metadata="https://mcp.linear.app/.well-known/oauth-protected-resource/mcp", scope="read write", error=invalid_token"#,
        );
        assert_eq!(params["resource_metadata"], "https://mcp.linear.app/.well-known/oauth-protected-resource/mcp");
        assert_eq!(params["scope"], "read write");
        assert_eq!(params["error"], "invalid_token");
        let server = url::Url::parse("https://mcp.linear.app/mcp").unwrap();
        assert_eq!(
            well_known(&server, "oauth-protected-resource"),
            "https://mcp.linear.app/.well-known/oauth-protected-resource/mcp"
        );
        let issuer = url::Url::parse("https://access.stripe.com/mcp").unwrap();
        assert_eq!(
            well_known(&issuer, "oauth-authorization-server"),
            "https://access.stripe.com/.well-known/oauth-authorization-server/mcp"
        );
        let root = url::Url::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(
            well_known(&root, "oauth-authorization-server"),
            "http://127.0.0.1:8080/.well-known/oauth-authorization-server"
        );
        // S256: base64url of the verifier's SHA-256, no padding (checked against Python's hashlib).
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mJ92K9fMjQRrDWHmuhCVPx4Ix4BJoQ"),
            "y_iTV_YU8cAefelNEDcTD1-KGPiQAsHatby0Wd3QYOA"
        );
        assert!(no_registration("github.com").contains("http://127.0.0.1:33418/callback"));
    }
}

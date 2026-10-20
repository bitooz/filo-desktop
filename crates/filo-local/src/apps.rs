// The user's apps: MCP servers connected once in Filo (Extensions › Apps) that the coding tools (Claude Code, Codex, Muse
// Code, OpenCode) then use in every chat, agent conversation and group turn, without anything written to their own
// settings.
//
// An app is a local server (a command Filo runs, stdio) or a remote one (a URL, Streamable HTTP or the older SSE), and
// signs in one of three ways: not at all, with an account (OAuth, apps_oauth.rs, with no Filo account: Filo registers
// itself with the server's sign-in, or uses a client the user made at the provider), or with an API key (in a header
// for a remote server, an environment variable for a local one). Its settings are a row of Filo's database; its secrets
// are in the Keychain (secrets.rs), and the row only says one is there.
//
// A turn plugs in each app that's on for its engine as one more MCP server, named `filo-<slug>`: the app's own binary as
// a proxy (`filo --local-mcp --app <id>`, apps_client.rs), which reads the secrets, runs or reaches the server, hides
// the tools the user turned off and refuses their calls. Claude Code gets it in `--mcp-config` (its tools allowed with
// `mcp__filo-<slug>`), Codex with `-c mcp_servers.filo-<slug>.…` (engine.rs), Muse Code and OpenCode with their session
// (coding_muse.rs, coding_opencode.rs), pi through Filo's extension (coding_pi.rs).

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension as _, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::Db;
use crate::engine::Engine;
use crate::secrets::{self, Secrets};

/// The apps table (db.rs `MIGRATIONS`). `IF NOT EXISTS`: `ensure` makes it too, so a database whose migration count
/// was taken by another page's table first still gets it.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS apps (
    id             TEXT PRIMARY KEY NOT NULL,
    slug           TEXT NOT NULL UNIQUE,
    name           TEXT NOT NULL,
    preset         TEXT,
    transport      TEXT NOT NULL,
    command        TEXT NOT NULL DEFAULT '',
    args           TEXT NOT NULL DEFAULT '[]',
    env            TEXT NOT NULL DEFAULT '{}',
    url            TEXT NOT NULL DEFAULT '',
    auth           TEXT NOT NULL DEFAULT 'none',
    key_name       TEXT NOT NULL DEFAULT '',
    key_prefix     TEXT NOT NULL DEFAULT '',
    has_secret     INTEGER NOT NULL DEFAULT 0,
    oauth          TEXT NOT NULL DEFAULT '{}',
    enabled        INTEGER NOT NULL DEFAULT 1,
    claude         INTEGER NOT NULL DEFAULT 1,
    codex          INTEGER NOT NULL DEFAULT 1,
    muse           INTEGER NOT NULL DEFAULT 1,
    opencode       INTEGER NOT NULL DEFAULT 1,
    pi             INTEGER NOT NULL DEFAULT 1,
    agent_scope    TEXT NOT NULL DEFAULT 'all',
    agents         TEXT NOT NULL DEFAULT '[]',
    enabled_tools  TEXT NOT NULL DEFAULT '[]',
    disabled_tools TEXT NOT NULL DEFAULT '[]',
    all_tools      INTEGER NOT NULL DEFAULT 1,
    tools          TEXT NOT NULL DEFAULT '[]',
    status         TEXT NOT NULL DEFAULT 'unchecked',
    status_detail  TEXT NOT NULL DEFAULT '',
    server_name    TEXT NOT NULL DEFAULT '',
    checked_at     INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
";

/// Columns added after the table first shipped, each added to a table made without it (`ensure`). `all_tools` comes
/// in off: apps connected before it keep their tools as they were, only the read-only ones on by themselves. Muse Code,
/// OpenCode and pi come in on, as Claude Code and Codex are for a new app.
const ADDED_COLUMNS: [(&str, &str); 7] = [
    ("enabled_tools", "TEXT NOT NULL DEFAULT '[]'"),
    ("agent_scope", "TEXT NOT NULL DEFAULT 'all'"),
    ("agents", "TEXT NOT NULL DEFAULT '[]'"),
    ("all_tools", "INTEGER NOT NULL DEFAULT 0"),
    ("muse", "INTEGER NOT NULL DEFAULT 1"),
    ("opencode", "INTEGER NOT NULL DEFAULT 1"),
    ("pi", "INTEGER NOT NULL DEFAULT 1"),
];

/// The Filo agent (Agents tab), which Filo's own chats and the group's `filo` speaker are: the id it's made with
/// (local_backend.rs `DEFAULT_AGENT_ID`), kept for its access even when the user removed it from the Agents tab.
pub const FILO_AGENT: &str = "00000000-0000-4000-8000-00000000f110";

/// Which agents may use an app: all of them (new ones too), or only those listed.
const ALL_AGENTS: &str = "all";
pub const SOME_AGENTS: &str = "some";

pub const STDIO: &str = "stdio";
pub const HTTP: &str = "http";
const AUTH_NONE: &str = "none";
pub const AUTH_OAUTH: &str = "oauth";
pub const AUTH_KEY: &str = "api_key";

/// Not checked yet (just added or changed).
pub const UNCHECKED: &str = "unchecked";
/// It answered and listed its tools.
pub const READY: &str = "ready";
/// It wants a sign-in or a key it doesn't have (or refused the one it has).
pub const NEEDS_SIGN_IN: &str = "needs_sign_in";
/// It couldn't be started or reached, or answered with an error.
pub const FAILED: &str = "failed";

/// Secrets' kinds (secrets.rs `account`).
pub const SECRET_KEY: &str = "api-key";
pub const SECRET_TOKENS: &str = "tokens";
pub const SECRET_CLIENT: &str = "client-secret";

const MAX_NAME: usize = 60;
const MAX_SLUG: usize = 32;
const MAX_FIELD: usize = 2048;
/// Server names Filo uses itself, which an app can't take.
const RESERVED: [&str; 3] = ["filo", "feed", "app"];

/// A tool an app's server offers, as it last listed them, with what the server says it does (its annotations).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Marked read-only by the server (`readOnlyHint`): it only looks. On an app connected before every tool started
    /// on (`App::all_tools`), only these are on before the user says so.
    #[serde(default)]
    pub read_only: bool,
    /// Marked as able to delete or overwrite (`destructiveHint`, which MCP assumes when a tool isn't read-only and
    /// doesn't say).
    #[serde(default)]
    pub destructive: bool,
    /// Reaches outside the app (`openWorldHint`): the web, other people.
    #[serde(default)]
    pub open_world: bool,
    /// The server said what it does (`readOnlyHint` or `destructiveHint`); without that, the above are MCP's defaults.
    #[serde(default)]
    pub marked: bool,
}

/// Whether a tool the server lists as `value` only reads (its `readOnlyHint`).
fn read_only_hint(value: &Value) -> bool {
    value.pointer("/annotations/readOnlyHint").and_then(Value::as_bool).unwrap_or(false)
}

/// Whether a tool is on before the user says anything: the server marks it read-only, or it marks nothing and it's one
/// of the tools its preset lists as only reading (`read_tools`). Names alone don't count: a custom server's unmarked
/// tool waits for the user, and one the server marks as changing things always does.
pub fn on_by_default(tool: &Tool, read_tools: &[&str]) -> bool {
    tool.read_only || (!tool.marked && read_tools.contains(&tool.name.as_str()))
}

/// An account sign-in's settings, none of them secret: the client Filo signs in as and where the server's sign-in is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuth {
    /// The client id the user made at the provider; none: Filo registers one (`registered_client_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Whether the user's client has a secret (in the Keychain).
    #[serde(default)]
    pub client_secret: bool,
    /// Scopes the user asked for; none: what the server says it needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    /// The client Filo registered itself (dynamic client registration), and the redirect it registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_redirect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_auth_method: Option<String>,
    /// Where the last sign-in went: the authorization server and its endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// How the token endpoint takes a client secret (`client_secret_basic` or `client_secret_post`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_auth_method: Option<String>,
    /// When the user last signed in (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_in_at: Option<i64>,
}

/// An app as the pages read it (secrets left out).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    pub id: String,
    /// The MCP server's name for the CLIs is `filo-<slug>`.
    pub slug: String,
    pub name: String,
    pub preset: Option<String>,
    /// `stdio` or `http`.
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    /// Environment for a local server, not secret (an API key goes in the Keychain).
    pub env: BTreeMap<String, String>,
    pub url: String,
    /// `none`, `oauth` or `api_key`.
    pub auth: String,
    /// API key: the header it goes in (remote) or the environment variable (local).
    pub key_name: String,
    /// API key, remote: what goes in front of it in the header (`Bearer `).
    pub key_prefix: String,
    /// The API key or the sign-in's tokens are in the Keychain.
    pub has_secret: bool,
    pub oauth: OAuth,
    /// Off: no agent gets it, and it's kept as it is.
    pub enabled: bool,
    /// Which engines may run it, whoever the agent is.
    pub claude: bool,
    pub codex: bool,
    pub muse: bool,
    pub opencode: bool,
    pub pi: bool,
    /// Which agents (the Agents tab's, Filo among them) may use it: every one, new ones too (`all`), or those in
    /// `agents` (`some`).
    pub agent_scope: String,
    pub agents: Vec<String>,
    /// Tools the user turned on and off. Any other is on when `all_tools` is, else only when the server marks it
    /// read-only.
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    /// Every tool the user hasn't turned off is on, those the server adds later too: how an app is connected now. Off
    /// for an app connected before, whose tools that change things waited for the user's yes and still do.
    pub all_tools: bool,
    pub tools: Vec<Tool>,
    pub status: String,
    pub status_detail: String,
    /// What the server calls itself.
    pub server_name: String,
    pub checked_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl App {
    /// The MCP server's name in a turn's coding tool.
    pub fn server_name_for_cli(&self) -> String {
        server_name(&self.slug)
    }

    pub fn allows(&self, engine: Engine) -> bool {
        match engine {
            Engine::Claude => self.claude,
            Engine::Codex => self.codex,
            Engine::Muse => self.muse,
            Engine::OpenCode => self.opencode,
            Engine::Pi => self.pi,
        }
    }

    /// Whether it has what it needs to sign in: a key, or a sign-in's tokens.
    pub fn credentials_ready(&self) -> bool {
        self.auth == AUTH_NONE || self.has_secret
    }

    /// Whether `agent` (an Agents-tab agent's id; Filo is `FILO_AGENT`) may use it.
    pub fn allows_agent(&self, agent: &str) -> bool {
        self.agent_scope != SOME_AGENTS || self.agents.iter().any(|allowed| allowed == agent)
    }

    /// Whether a turn of `agent` on `engine` plugs it in: on, allowed for that agent and engine, and not waiting for a
    /// key or a sign-in.
    fn plugged_for(&self, engine: Engine, agent: &str) -> bool {
        self.enabled && self.allows(engine) && self.allows_agent(agent) && self.credentials_ready()
    }

    /// Whether the agents may use `tool`: the user's choice, else on when every tool is (`all_tools`) or the server
    /// marks it read-only.
    pub fn tool_enabled(&self, tool: &str) -> bool {
        let read_tools = self.read_tools();
        let by_default =
            self.all_tools || self.tools.iter().any(|known| known.name == tool && on_by_default(known, read_tools));
        tool_allowed(tool, by_default, &self.enabled_tools, &self.disabled_tools)
    }

    /// The tools its preset says only read (`Preset::read_tools`); none for an app of the user's own.
    pub fn read_tools(&self) -> &'static [&'static str] {
        self.preset.as_deref().and_then(preset).map(|preset| preset.read_tools).unwrap_or(&[])
    }
}

/// The rule for a tool: turned off stays off, turned on is on, and one the user hasn't touched is as its app starts
/// it (`by_default`: every tool, or the read-only ones).
pub fn tool_allowed(tool: &str, by_default: bool, enabled: &[String], disabled: &[String]) -> bool {
    if disabled.iter().any(|name| name == tool) {
        return false;
    }
    enabled.iter().any(|name| name == tool) || by_default
}

pub fn server_name(slug: &str) -> String {
    format!("filo-{slug}")
}

/// What `apps:add` and `apps:update` are given (empty fields keep what's there on an update).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Fields {
    pub name: String,
    pub preset: Option<String>,
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
    pub auth: String,
    pub key_name: String,
    pub key_prefix: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Option<String>,
}

const COLUMNS: &str = "id, slug, name, preset, transport, command, args, env, url, auth, key_name, key_prefix, has_secret, oauth, enabled, claude, codex, enabled_tools, disabled_tools, tools, status, status_detail, server_name, checked_at, created_at, updated_at, agent_scope, agents, all_tools, muse, opencode, pi";

fn json_column<T: for<'de> Deserialize<'de> + Default>(row: &Row, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

fn app(row: &Row) -> rusqlite::Result<App> {
    Ok(App {
        id: row.get(0)?,
        slug: row.get(1)?,
        name: row.get(2)?,
        preset: row.get(3)?,
        transport: row.get(4)?,
        command: row.get(5)?,
        args: json_column(row, 6)?,
        env: json_column(row, 7)?,
        url: row.get(8)?,
        auth: row.get(9)?,
        key_name: row.get(10)?,
        key_prefix: row.get(11)?,
        has_secret: row.get(12)?,
        oauth: json_column(row, 13)?,
        enabled: row.get(14)?,
        claude: row.get(15)?,
        codex: row.get(16)?,
        enabled_tools: json_column(row, 17)?,
        disabled_tools: json_column(row, 18)?,
        tools: json_column(row, 19)?,
        status: row.get(20)?,
        status_detail: row.get(21)?,
        server_name: row.get(22)?,
        checked_at: row.get(23)?,
        created_at: row.get(24)?,
        updated_at: row.get(25)?,
        agent_scope: row.get(26)?,
        agents: json_column(row, 27)?,
        all_tools: row.get(28)?,
        muse: row.get(29)?,
        opencode: row.get(30)?,
        pi: row.get(31)?,
    })
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

/// Makes the table when the migrations didn't (see `SCHEMA`).
fn ensure(db: &Db) -> Result<(), String> {
    db.with(|conn| {
        conn.execute_batch(SCHEMA)?;
        let have: Vec<String> = conn
            .prepare_cached("SELECT name FROM pragma_table_info('apps')")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (column, declaration) in ADDED_COLUMNS {
            if !have.iter().any(|name| name == column) {
                conn.execute_batch(&format!("ALTER TABLE apps ADD COLUMN {column} {declaration}"))?;
            }
        }
        Ok(())
    })
}

/// Every app, in the order they were added.
pub fn list(db: &Db) -> Result<Vec<App>, String> {
    ensure(db)?;
    db.with(|conn| {
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM apps ORDER BY created_at, name"))?
            .query_map([], app)?
            .collect()
    })
}

pub fn get(db: &Db, id: &str) -> Result<Option<App>, String> {
    ensure(db)?;
    db.with(|conn| {
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM apps WHERE id = ?1"))?.query_row([id], app).optional()
    })
}

/// The app or a message saying it's gone.
pub fn find(db: &Db, id: &str) -> Result<App, String> {
    get(db, id)?.ok_or_else(|| "That app isn't connected anymore.".to_string())
}

/// The apps a turn of `agent` on `engine` plugs in.
pub fn plugged(db: &Db, engine: Engine, agent: &str) -> Result<Vec<App>, String> {
    Ok(list(db)?.into_iter().filter(|app| app.plugged_for(engine, agent)).collect())
}

fn one_line(text: &str, max: usize) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// "GitHub Issues" → "github-issues": lowercase letters, digits and dashes, as MCP server names take.
fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(MAX_SLUG).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() { "app".into() } else { slug }
}

fn unique_slug(db: &Db, name: &str, except: Option<&str>) -> Result<String, String> {
    let base = slug_of(name);
    let taken: Vec<String> =
        list(db)?.into_iter().filter(|app| Some(app.id.as_str()) != except).map(|app| app.slug).collect();
    let free = |slug: &str| !taken.iter().any(|taken| taken == slug) && !RESERVED.contains(&slug);
    if free(&base) {
        return Ok(base);
    }
    (2..1000)
        .map(|n| format!("{}-{n}", base.chars().take(MAX_SLUG - 4).collect::<String>()))
        .find(|slug| free(slug))
        .ok_or_else(|| "Too many apps with that name.".into())
}

/// A remote server's address: https, or http on this Mac only.
fn clean_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    let parsed = url::Url::parse(url).map_err(|_| "That isn't a web address (https://…).".to_string())?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match parsed.scheme() {
        "https" => {}
        "http" if local => {}
        "http" => return Err("A remote server needs https:// (http:// only for one on this Mac).".into()),
        _ => return Err("That isn't a web address (https://…).".into()),
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("That isn't a web address (https://…).".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Put a key in the API key field, not in the address.".into());
    }
    Ok(url.chars().take(MAX_FIELD).collect())
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 128
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
}

/// Splits a command line the way a shell would for plain words and quotes ("a 'b c'" → a, b c), no expansion.
fn split_command_line(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some(next) => word.push(next),
                None => return Err("A command line ends in the middle of a quote.".into()),
            },
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                    in_word = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("A command line ends in the middle of a quote.".into());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// `fields` checked and tidied, as an app's settings (`base` is the app being changed).
fn settle(fields: &Fields, base: Option<&App>) -> Result<Settled, String> {
    let name = one_line(&fields.name, MAX_NAME);
    let name = if name.is_empty() { base.map(|app| app.name.clone()).unwrap_or_default() } else { name };
    if name.is_empty() {
        return Err("Give the app a name.".into());
    }
    let transport = match (fields.transport.as_str(), base) {
        ("", Some(app)) => app.transport.clone(),
        (STDIO | HTTP, _) => fields.transport.clone(),
        ("sse" | "remote" | "streamable-http", _) => HTTP.into(),
        ("local", _) => STDIO.into(),
        _ => return Err("Pick a local command or a remote server.".into()),
    };
    let auth = match (fields.auth.as_str(), base) {
        ("", Some(app)) => app.auth.clone(),
        ("", None) => AUTH_NONE.into(),
        (AUTH_NONE | AUTH_OAUTH | AUTH_KEY, _) => fields.auth.clone(),
        ("oauth2" | "account", _) => AUTH_OAUTH.into(),
        ("key" | "apiKey", _) => AUTH_KEY.into(),
        _ => return Err("Pick how it signs in: no sign-in, an account or an API key.".into()),
    };
    let mut settled = Settled { name, transport: transport.clone(), auth: auth.clone(), ..Default::default() };
    if transport == HTTP {
        let url = if fields.url.trim().is_empty() {
            base.map(|app| app.url.clone()).unwrap_or_default()
        } else {
            fields.url.clone()
        };
        settled.url = clean_url(&url)?;
    } else {
        if auth == AUTH_OAUTH {
            return Err(
                "Account sign-in is for remote servers. A local command takes an API key in its environment.".into()
            );
        }
        let command = if fields.command.trim().is_empty() {
            base.map(|app| app.command.clone()).unwrap_or_default()
        } else {
            fields.command.trim().to_string()
        };
        if command.is_empty() {
            return Err("Give the command that starts the server (like npx).".into());
        }
        // A whole command line typed in the command field: its first word is the command.
        let (command, mut args) = if fields.args.is_empty() && command.contains(char::is_whitespace) {
            let mut words = split_command_line(&command)?;
            let first = words.remove(0);
            (first, words)
        } else {
            (command, fields.args.clone())
        };
        if args.is_empty() && fields.command.trim().is_empty() {
            args = base.map(|app| app.args.clone()).unwrap_or_default();
        }
        if command.len() > MAX_FIELD || args.len() > 64 || args.iter().any(|arg| arg.len() > MAX_FIELD) {
            return Err("That command line is too long.".into());
        }
        for key in fields.env.keys() {
            if !valid_env_name(key) {
                return Err(format!("“{key}” isn't an environment variable name (letters, digits and _)."));
            }
        }
        settled.command = command;
        settled.args = args;
        settled.env = if fields.env.is_empty() && fields.command.trim().is_empty() {
            base.map(|app| app.env.clone()).unwrap_or_default()
        } else {
            fields.env.clone()
        };
    }
    if auth == AUTH_KEY {
        let key_name = if fields.key_name.trim().is_empty() {
            base.filter(|app| app.transport == transport).map(|app| app.key_name.clone()).unwrap_or_default()
        } else {
            fields.key_name.trim().to_string()
        };
        if transport == HTTP {
            let key_name = if key_name.is_empty() { "Authorization".to_string() } else { key_name };
            if !valid_header_name(&key_name) {
                return Err(format!("“{key_name}” isn't a header name."));
            }
            settled.key_prefix = match &fields.key_prefix {
                Some(prefix) => prefix.chars().filter(|c| !c.is_control()).take(32).collect(),
                None if key_name.eq_ignore_ascii_case("authorization") => base
                    .map(|app| app.key_prefix.clone())
                    .filter(|prefix| !prefix.is_empty())
                    .unwrap_or_else(|| "Bearer ".into()),
                None => base.map(|app| app.key_prefix.clone()).unwrap_or_default(),
            };
            settled.key_name = key_name;
        } else {
            if !valid_env_name(&key_name) {
                return Err("Name the environment variable the key goes in (like GITHUB_TOKEN).".into());
            }
            settled.key_name = key_name;
        }
    }
    if auth == AUTH_OAUTH {
        let mut oauth = base.map(|app| app.oauth.clone()).unwrap_or_default();
        if let Some(client_id) = &fields.client_id {
            let client_id = client_id.trim();
            let changed = oauth.client_id.as_deref().unwrap_or("") != client_id;
            oauth.client_id = (!client_id.is_empty()).then(|| client_id.chars().take(256).collect());
            if changed {
                oauth.client_secret = false;
            }
        }
        if let Some(scopes) = &fields.scopes {
            let scopes = one_line(scopes, 512);
            oauth.scopes = (!scopes.is_empty()).then_some(scopes);
        }
        // Another server: what Filo registered at the old one doesn't hold there.
        if base.is_some_and(|app| app.url != settled.url) {
            oauth.registered_client_id = None;
            oauth.registered_redirect = None;
            oauth.registered_auth_method = None;
        }
        settled.oauth = oauth;
    }
    Ok(settled)
}

#[derive(Default)]
struct Settled {
    name: String,
    transport: String,
    auth: String,
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    url: String,
    key_name: String,
    key_prefix: String,
    oauth: OAuth,
}

/// Adds an app, every one of its tools on; its id. It's checked (or signed in) after.
pub fn add(db: &Db, fields: &Fields, now: i64) -> Result<App, String> {
    let settled = settle(fields, None)?;
    let slug = unique_slug(db, &settled.name, None)?;
    let id = uuid::Uuid::new_v4().to_string();
    let preset = fields.preset.as_deref().map(str::trim).filter(|preset| !preset.is_empty()).map(str::to_string);
    db.write(|tx| {
        tx.execute(
            "INSERT INTO apps (id, slug, name, preset, transport, command, args, env, url, auth, key_name, key_prefix, oauth, all_tools, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, ?14)",
            params![
                id,
                slug,
                settled.name,
                preset,
                settled.transport,
                settled.command,
                to_json(&settled.args),
                to_json(&settled.env),
                settled.url,
                settled.auth,
                settled.key_name,
                settled.key_prefix,
                to_json(&settled.oauth),
                now
            ],
        )
    })?;
    find(db, &id)
}

/// Changes an app's settings. One that now reaches another server or signs in another way is checked again, and a
/// sign-in or key that no longer fits is forgotten (`secrets`).
pub fn update(db: &Db, secrets: &Secrets, id: &str, fields: &Fields, now: i64) -> Result<App, String> {
    let before = find(db, id)?;
    let settled = settle(fields, Some(&before))?;
    let slug =
        if settled.name != before.name { unique_slug(db, &settled.name, Some(id))? } else { before.slug.clone() };
    let moved = settled.transport != before.transport
        || settled.url != before.url
        || settled.command != before.command
        || settled.args != before.args;
    let auth_changed = settled.auth != before.auth
        || (settled.auth == AUTH_OAUTH && settled.oauth.client_id != before.oauth.client_id);
    let mut has_secret = before.has_secret;
    if auth_changed || (moved && before.auth == AUTH_OAUTH) {
        forget_secrets(secrets, id)?;
        has_secret = false;
    }
    let recheck = moved
        || auth_changed
        || settled.env != before.env
        || settled.key_name != before.key_name
        || settled.key_prefix != before.key_prefix;
    db.write(|tx| {
        tx.execute(
            "UPDATE apps SET slug = ?2, name = ?3, transport = ?4, command = ?5, args = ?6, env = ?7, url = ?8, auth = ?9, key_name = ?10,
             key_prefix = ?11, oauth = ?12, has_secret = ?13, updated_at = ?14 WHERE id = ?1",
            params![
                id,
                slug,
                settled.name,
                settled.transport,
                settled.command,
                to_json(&settled.args),
                to_json(&settled.env),
                settled.url,
                settled.auth,
                settled.key_name,
                settled.key_prefix,
                to_json(&settled.oauth),
                has_secret,
                now
            ],
        )?;
        if recheck {
            tx.execute("UPDATE apps SET status = ?2, status_detail = '' WHERE id = ?1", params![id, UNCHECKED])?;
        }
        Ok(())
    })?;
    find(db, id)
}

fn set_flag(db: &Db, id: &str, column: &str, on: bool, now: i64) -> Result<App, String> {
    let changed = db.write(|tx| {
        tx.execute(&format!("UPDATE apps SET {column} = ?2, updated_at = ?3 WHERE id = ?1"), params![id, on, now])
    })?;
    if changed == 0 {
        return Err("That app isn't connected anymore.".into());
    }
    find(db, id)
}

/// On or off for every agent, keeping its settings and sign-in.
pub fn set_enabled(db: &Db, id: &str, on: bool, now: i64) -> Result<App, String> {
    set_flag(db, id, "enabled", on, now)
}

/// On or off for one agent.
pub fn set_engine(db: &Db, id: &str, engine: Engine, on: bool, now: i64) -> Result<App, String> {
    set_flag(db, id, engine.key(), on, now)
}

/// Which agents may use the app: all of them (`agents` ignored), or only `agents`.
pub fn set_agent_access(db: &Db, id: &str, all: bool, agents: &[String], now: i64) -> Result<App, String> {
    let mut agents: Vec<String> = agents
        .iter()
        .map(|agent| agent.trim().to_string())
        .filter(|agent| !agent.is_empty() && agent.len() <= 128)
        .collect();
    agents.sort();
    agents.dedup();
    let scope = if all { ALL_AGENTS } else { SOME_AGENTS };
    let changed = db.write(|tx| {
        tx.execute(
            "UPDATE apps SET agent_scope = ?2, agents = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, scope, to_json(&agents), now],
        )
    })?;
    if changed == 0 {
        return Err("That app isn't connected anymore.".into());
    }
    find(db, id)
}

/// Turns tools on or off for the agents, as the user said: kept as their choice, whatever the server marks them.
pub fn set_tools_enabled(db: &Db, id: &str, tools: &[String], on: bool, now: i64) -> Result<App, String> {
    let app = find(db, id)?;
    let tools: Vec<String> = tools.iter().map(|tool| tool.chars().take(256).collect()).collect();
    let mut enabled: Vec<String> = app.enabled_tools.into_iter().filter(|name| !tools.contains(name)).collect();
    let mut disabled: Vec<String> = app.disabled_tools.into_iter().filter(|name| !tools.contains(name)).collect();
    if on {
        enabled.extend(tools)
    } else {
        disabled.extend(tools)
    }
    enabled.sort();
    disabled.sort();
    db.write(|tx| {
        tx.execute(
            "UPDATE apps SET enabled_tools = ?2, disabled_tools = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, to_json(&enabled), to_json(&disabled), now],
        )
    })?;
    find(db, id)
}

pub fn set_tool_enabled(db: &Db, id: &str, tool: &str, on: bool, now: i64) -> Result<App, String> {
    set_tools_enabled(db, id, &[tool.to_string()], on, now)
}

/// What a check found (apps_client.rs `probe`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    pub status: String,
    pub detail: String,
    /// The tools, when it listed them (kept as they were otherwise).
    pub tools: Option<Vec<Tool>>,
    pub server_name: Option<String>,
}

pub fn record_check(db: &Db, id: &str, check: &Check, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "UPDATE apps SET status = ?2, status_detail = ?3, checked_at = ?4 WHERE id = ?1",
            params![id, check.status, check.detail.chars().take(600).collect::<String>(), now],
        )?;
        if let Some(tools) = &check.tools {
            tx.execute("UPDATE apps SET tools = ?2 WHERE id = ?1", params![id, to_json(tools)])?;
        }
        if let Some(name) = &check.server_name {
            tx.execute("UPDATE apps SET server_name = ?2 WHERE id = ?1", params![id, one_line(name, 120)])?;
        }
        Ok(())
    })
}

/// A URL's origin (`https://host:port`), where a secret for it may go (secrets.rs `get_for`); the URL itself when it
/// isn't one.
pub fn origin_of(url: &str) -> String {
    url::Url::parse(url).map_or_else(|_| url.to_string(), |url| url.origin().ascii_serialization())
}

/// Where the app's API key goes: its server's origin, or the program (with its arguments) a local app runs, which gets
/// it in its environment.
pub fn key_destination(app: &App) -> String {
    match app.transport.as_str() {
        STDIO => format!("the program {}", serde_json::to_string(&(&app.command, &app.args)).unwrap_or_default()),
        _ => origin_of(&app.url),
    }
}

/// Where an account's sign-in goes: its server (the access token) and its token endpoint (the refresh token).
pub fn sign_in_destination(url: &str, token_endpoint: Option<&str>) -> String {
    format!("{} and {}", origin_of(url), token_endpoint.map_or_else(|| "no token endpoint".to_string(), origin_of))
}

/// Keeps the API key (or forgets it, given an empty one).
pub fn set_api_key(db: &Db, secrets: &Secrets, id: &str, key: &str, now: i64) -> Result<App, String> {
    let app = find(db, id)?;
    if app.auth != AUTH_KEY {
        return Err("This app doesn't sign in with an API key.".into());
    }
    let key = key.trim();
    if key.chars().any(char::is_control) || key.len() > 8192 {
        return Err("That doesn't look like an API key.".into());
    }
    let account = secrets::account(id, SECRET_KEY);
    if key.is_empty() {
        secrets.delete(&account)?
    } else {
        secrets.set_for(&account, key, &key_destination(&app))?
    }
    db.write(|tx| {
        tx.execute(
            "UPDATE apps SET has_secret = ?2, status = ?3, status_detail = '', updated_at = ?4 WHERE id = ?1",
            params![id, !key.is_empty(), UNCHECKED, now],
        )
    })?;
    find(db, id)
}

/// Keeps the secret of the client the user made at the provider (or forgets it, given an empty one).
pub fn set_client_secret(db: &Db, secrets: &Secrets, id: &str, secret: &str, now: i64) -> Result<App, String> {
    let mut app = find(db, id)?;
    let secret = secret.trim();
    let account = secrets::account(id, SECRET_CLIENT);
    if secret.is_empty() {
        secrets.delete(&account)?
    } else {
        secrets.set_for(&account, secret, &origin_of(&app.url))?
    }
    app.oauth.client_secret = !secret.is_empty();
    save_oauth(db, id, &app.oauth, now)?;
    find(db, id)
}

pub fn save_oauth(db: &Db, id: &str, oauth: &OAuth, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("UPDATE apps SET oauth = ?2, updated_at = ?3 WHERE id = ?1", params![id, to_json(oauth), now])
    })
    .map(|_| ())
}

pub fn set_has_secret(db: &Db, id: &str, has: bool) -> Result<(), String> {
    db.write(|tx| tx.execute("UPDATE apps SET has_secret = ?2 WHERE id = ?1", params![id, has])).map(|_| ())
}

/// Signs out of an account (its tokens forgotten) or forgets the API key; the app stays, waiting for a new one.
pub fn sign_out(db: &Db, secrets: &Secrets, id: &str, now: i64) -> Result<App, String> {
    let mut app = find(db, id)?;
    secrets.delete(&secrets::account(id, SECRET_TOKENS))?;
    secrets.delete(&secrets::account(id, SECRET_KEY))?;
    app.oauth.signed_in_at = None;
    db.write(|tx| {
        tx.execute(
            "UPDATE apps SET has_secret = 0, oauth = ?2, status = ?3, status_detail = '', updated_at = ?4 WHERE id = ?1",
            params![id, to_json(&app.oauth), if app.auth == AUTH_NONE { UNCHECKED } else { NEEDS_SIGN_IN }, now],
        )
    })?;
    find(db, id)
}

fn forget_secrets(secrets: &Secrets, id: &str) -> Result<(), String> {
    for kind in [SECRET_KEY, SECRET_TOKENS, SECRET_CLIENT] {
        secrets.delete(&secrets::account(id, kind))?;
    }
    Ok(())
}

/// Removes the app and its secrets. False when it wasn't there.
pub fn remove(db: &Db, secrets: &Secrets, id: &str) -> Result<bool, String> {
    ensure(db)?;
    forget_secrets(secrets, id)?;
    Ok(db.write(|tx| tx.execute("DELETE FROM apps WHERE id = ?1", [id]))? > 0)
}

/// A ready-made app on the Apps page. Only servers anyone can connect from this Mac: no Filo account, no client
/// someone has to register first (each sign-in one registers Filo itself, checked against the server).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub transport: &'static str,
    pub url: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub auth: &'static str,
    pub key_name: &'static str,
    pub key_prefix: &'static str,
    /// Where to make the key, for one that takes a key.
    pub key_url: &'static str,
    /// What to say about signing in (the key's scopes, what's needed).
    pub note: &'static str,
    /// The program a local one needs on PATH (`npx`).
    pub needs: &'static str,
    /// Tools Filo checked only read (the service's own tools, by name), on from the start though the server doesn't mark
    /// them read-only. A mark from the server still wins.
    pub read_tools: &'static [&'static str],
}

const fn remote(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    url: &'static str,
    auth: &'static str,
) -> Preset {
    Preset {
        id,
        name,
        description,
        transport: HTTP,
        url,
        command: "",
        args: &[],
        auth,
        key_name: "",
        key_prefix: "",
        key_url: "",
        note: "",
        needs: "",
        read_tools: &[],
    }
}

pub const PRESETS: &[Preset] = &[
    remote("linear", "Linear", "Issues, projects and cycles", "https://mcp.linear.app/mcp", AUTH_OAUTH),
    remote("notion", "Notion", "Search, read and edit your pages", "https://mcp.notion.com/mcp", AUTH_OAUTH),
    remote("sentry", "Sentry", "Errors, issues and releases", "https://mcp.sentry.dev/mcp", AUTH_OAUTH),
    remote("vercel", "Vercel", "Projects, deployments and logs", "https://mcp.vercel.com", AUTH_OAUTH),
    remote("clickup", "ClickUp", "Read and manage your tasks", "https://mcp.clickup.com/mcp", AUTH_OAUTH),
    remote("stripe", "Stripe", "Customers, payments and docs", "https://mcp.stripe.com", AUTH_OAUTH),
    remote("upsy", "Upsy", "Gmail, Slack and 40 more apps in one place", "https://upsy.ai/connect", AUTH_OAUTH),
    remote("executor", "Executor", "Your own tools, skills and automations", "https://mcp.executor.sh/mcp", AUTH_OAUTH),
    Preset {
        key_name: "Authorization",
        key_prefix: "Bearer ",
        key_url: "https://github.com/settings/personal-access-tokens/new",
        note: "GitHub doesn’t let apps register their own sign-in, so it takes a personal access token: make a fine-grained one with access to the repos you want.",
        ..remote("github", "GitHub", "Repos, issues and pull requests", "https://api.githubcopilot.com/mcp/", AUTH_KEY)
    },
    remote("context7", "Context7", "Up-to-date docs for libraries", "https://mcp.context7.com/mcp", AUTH_NONE),
    // Its tools carry no marks; all three only answer about public repos' docs.
    Preset {
        read_tools: &["read_wiki_structure", "read_wiki_contents", "ask_wiki_question"],
        ..remote("deepwiki", "DeepWiki", "Ask about any public GitHub repo", "https://mcp.deepwiki.com/mcp", AUTH_NONE)
    },
    remote("huggingface", "Hugging Face", "Models, datasets and Spaces", "https://huggingface.co/mcp", AUTH_NONE),
    remote(
        "cloudflare-docs",
        "Cloudflare Docs",
        "Search Cloudflare’s documentation",
        "https://docs.mcp.cloudflare.com/mcp",
        AUTH_NONE,
    ),
    Preset {
        id: "playwright",
        name: "Playwright",
        description: "A browser the agents drive on this Mac",
        transport: STDIO,
        url: "",
        command: "npx",
        args: &["-y", "@playwright/mcp@latest"],
        auth: AUTH_NONE,
        key_name: "",
        key_prefix: "",
        key_url: "",
        note: "",
        needs: "npx",
        read_tools: &[],
    },
];

/// An app's settings as a preset has them.
pub fn preset_fields(preset: &Preset) -> Fields {
    Fields {
        name: preset.name.into(),
        preset: Some(preset.id.into()),
        transport: preset.transport.into(),
        command: preset.command.into(),
        args: preset.args.iter().map(|arg| arg.to_string()).collect(),
        url: preset.url.into(),
        auth: preset.auth.into(),
        key_name: preset.key_name.into(),
        key_prefix: (!preset.key_prefix.is_empty()).then(|| preset.key_prefix.into()),
        ..Default::default()
    }
}

/// The app made from `preset`, if one was.
pub fn from_preset(db: &Db, preset: &str) -> Result<Option<App>, String> {
    Ok(list(db)?.into_iter().find(|app| app.preset.as_deref() == Some(preset)))
}

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// The MCP proxy's arguments for an app (after the program's own, like `--local-mcp`).
/// The proxy's arguments for an app in a turn of `agent` (after the program's own, like `--local-mcp`).
pub fn proxy_args(app_id: &str, db: &std::path::Path, agent: &str) -> Vec<String> {
    vec!["--app".into(), app_id.into(), "--db".into(), db.display().to_string(), "--agent".into(), agent.into()]
}

/// The servers a turn of `agent` on `engine` plugs in besides Filo's: each app that's on for both, through the proxy.
pub fn servers_for(
    db_path: &std::path::Path,
    engine: Engine,
    agent: &str,
    program: &std::path::Path,
    program_args: &[String],
) -> Vec<crate::engine::McpServer> {
    let Ok(db) = Db::open(db_path) else { return Vec::new() };
    plugged(&db, engine, agent)
        .unwrap_or_default()
        .into_iter()
        .map(|app| {
            let mut args = program_args.to_vec();
            args.extend(proxy_args(&app.id, db_path, agent));
            crate::engine::McpServer { name: app.server_name_for_cli(), program: program.to_path_buf(), args }
        })
        .collect()
}

/// A tool's `inputSchema` and the rest as the server gave them, for a page that shows the tool.
pub fn tool_from(value: &Value) -> Option<Tool> {
    let name = value.get("name")?.as_str()?.to_string();
    let title = value
        .get("title")
        .or_else(|| value.get("annotations").and_then(|annotations| annotations.get("title")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let description = value.get("description").and_then(Value::as_str).unwrap_or_default();
    let read_only = read_only_hint(value);
    let hint = |key: &str| value.pointer(&format!("/annotations/{key}")).and_then(Value::as_bool);
    Some(Tool {
        name,
        title,
        description: description.chars().take(600).collect(),
        read_only,
        // MCP's defaults: a tool that isn't read-only may be destructive and reach the world unless it says otherwise.
        destructive: !read_only && hint("destructiveHint").unwrap_or(true),
        open_world: hint("openWorldHint").unwrap_or(true),
        marked: hint("readOnlyHint").is_some() || hint("destructiveHint").is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// How many of its tools the agents may use.
    fn enabled_count(app: &App) -> usize {
        app.tools.iter().filter(|tool| app.tool_enabled(&tool.name)).count()
    }

    fn remote_fields(name: &str, url: &str, auth: &str) -> Fields {
        Fields { name: name.into(), transport: HTTP.into(), url: url.into(), auth: auth.into(), ..Default::default() }
    }

    /// Makes `id` an app connected before every tool started on, as `ensure` leaves one (`all_tools` off).
    fn connected_before(db: &Db, id: &str) {
        db.write(|tx| tx.execute("UPDATE apps SET all_tools = 0 WHERE id = ?1", [id])).unwrap();
    }

    #[test]
    fn apps_are_added_named_and_tidied() {
        let db = Db::in_memory().unwrap();
        let linear = add(&db, &remote_fields(" Linear ", "https://mcp.linear.app/mcp", AUTH_OAUTH), 1).unwrap();
        assert_eq!(
            (linear.slug.as_str(), linear.name.as_str(), linear.status.as_str()),
            ("linear", "Linear", UNCHECKED)
        );
        assert!(linear.enabled && linear.claude && linear.codex && !linear.has_secret);
        assert_eq!(linear.server_name_for_cli(), "filo-linear");
        // Same name: a slug of its own. Filo's own names are taken.
        assert_eq!(
            add(&db, &remote_fields("Linear", "https://mcp.linear.app/mcp", AUTH_NONE), 2).unwrap().slug,
            "linear-2"
        );
        assert_eq!(add(&db, &remote_fields("Filo", "https://example.com/mcp", AUTH_NONE), 3).unwrap().slug, "filo-2");
        let local = add(
            &db,
            &Fields {
                name: "Files".into(),
                transport: STDIO.into(),
                command: "npx -y @modelcontextprotocol/server-filesystem '/Users/me/My Docs'".into(),
                ..Default::default()
            },
            4,
        )
        .unwrap();
        assert_eq!(local.command, "npx");
        assert_eq!(local.args, ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/My Docs"]);
        assert_eq!(list(&db).unwrap().len(), 4);

        for (fields, says) in [
            (remote_fields("", "https://x.dev/mcp", AUTH_NONE), "name"),
            (remote_fields("X", "http://x.dev/mcp", AUTH_NONE), "https"),
            (remote_fields("X", "ftp://x.dev", AUTH_NONE), "web address"),
            (remote_fields("X", "https://me:pw@x.dev", AUTH_NONE), "API key"),
            (
                Fields {
                    name: "X".into(),
                    transport: STDIO.into(),
                    command: "srv".into(),
                    auth: AUTH_OAUTH.into(),
                    ..Default::default()
                },
                "remote servers",
            ),
            (
                Fields {
                    name: "X".into(),
                    transport: STDIO.into(),
                    command: "srv".into(),
                    auth: AUTH_KEY.into(),
                    key_name: "my key".into(),
                    ..Default::default()
                },
                "environment variable",
            ),
            (
                Fields {
                    name: "X".into(),
                    transport: STDIO.into(),
                    command: "srv".into(),
                    env: [("A B".to_string(), "1".to_string())].into(),
                    ..Default::default()
                },
                "environment variable",
            ),
        ] {
            let error = add(&db, &fields, 5).unwrap_err();
            assert!(error.contains(says), "{error}");
        }
        assert!(add(&db, &remote_fields("Dev", "http://localhost:3000/mcp", AUTH_NONE), 6).is_ok());
    }

    #[test]
    fn keys_go_to_the_secrets_and_the_row_only_says_so() {
        let db = Db::in_memory().unwrap();
        let secrets = Secrets::memory();
        let github = add(
            &db,
            &Fields {
                key_name: String::new(),
                ..remote_fields("GitHub", "https://api.githubcopilot.com/mcp/", AUTH_KEY)
            },
            1,
        )
        .unwrap();
        assert_eq!((github.key_name.as_str(), github.key_prefix.as_str()), ("Authorization", "Bearer "));
        assert!(!github.credentials_ready() && !github.plugged_for(Engine::Claude, FILO_AGENT));
        let github = set_api_key(&db, &secrets, &github.id, " ghp_secret ", 2).unwrap();
        assert!(github.has_secret && github.plugged_for(Engine::Claude, FILO_AGENT));
        let account = secrets::account(&github.id, SECRET_KEY);
        assert_eq!(secrets.get_for(&account, &key_destination(&github)).unwrap().as_deref(), Some("ghp_secret"));
        let row = serde_json::to_string(&github).unwrap();
        assert!(!row.contains("ghp_secret"));
        // Kept for GitHub's server alone: a row pointed elsewhere (a copy of the database, or the URL changed) gets an
        // error, not the key.
        let elsewhere = App { url: "https://attacker.example/mcp".into(), ..github.clone() };
        assert!(
            secrets
                .get_for(&account, &key_destination(&elsewhere))
                .unwrap_err()
                .contains("is for https://api.githubcopilot.com")
        );
        assert_eq!(secrets.value(&account).unwrap().as_deref(), Some("ghp_secret"));
        // A key kept before keys were bound is bound where it's first used.
        secrets.set(&account, "ghp_old").unwrap();
        assert_eq!(secrets.get_for(&account, &key_destination(&github)).unwrap().as_deref(), Some("ghp_old"));
        assert!(secrets.get_for(&account, &key_destination(&elsewhere)).is_err());
        // A local app's key goes to its program and arguments alone.
        let local = App {
            transport: STDIO.into(),
            command: "notes-server".into(),
            args: vec!["--stdio".into()],
            ..github.clone()
        };
        let swapped =
            App { command: "/bin/sh".into(), args: vec!["-c".into(), "curl -d $KEY".into()], ..local.clone() };
        assert_ne!(key_destination(&local), key_destination(&swapped));
        // Signing in another way forgets the key.
        let github =
            update(&db, &secrets, &github.id, &Fields { auth: AUTH_OAUTH.into(), ..Default::default() }, 3).unwrap();
        assert!(!github.has_secret && github.status == UNCHECKED);
        assert_eq!(secrets.get(&secrets::account(&github.id, SECRET_KEY)).unwrap(), None);
        assert!(remove(&db, &secrets, &github.id).unwrap());
        assert!(!remove(&db, &secrets, &github.id).unwrap());
    }

    #[test]
    fn switches_decide_which_agents_get_an_app_and_which_tools() {
        let db = Db::in_memory().unwrap();
        let app = add(&db, &remote_fields("Docs", "https://mcp.context7.com/mcp", AUTH_NONE), 1).unwrap();
        assert_eq!(plugged(&db, Engine::Codex, FILO_AGENT).unwrap().len(), 1);
        set_engine(&db, &app.id, Engine::Codex, false, 2).unwrap();
        assert!(plugged(&db, Engine::Codex, FILO_AGENT).unwrap().is_empty());
        assert_eq!(plugged(&db, Engine::Claude, FILO_AGENT).unwrap().len(), 1);
        // Muse Code and OpenCode get it too, each with a switch of its own.
        assert_eq!(
            (
                plugged(&db, Engine::Muse, FILO_AGENT).unwrap().len(),
                plugged(&db, Engine::OpenCode, FILO_AGENT).unwrap().len()
            ),
            (1, 1)
        );
        let muse_off = set_engine(&db, &app.id, Engine::Muse, false, 2).unwrap();
        assert!(!muse_off.muse && muse_off.opencode && muse_off.claude);
        assert!(plugged(&db, Engine::Muse, FILO_AGENT).unwrap().is_empty());
        assert_eq!(plugged(&db, Engine::OpenCode, FILO_AGENT).unwrap().len(), 1);
        set_enabled(&db, &app.id, false, 3).unwrap();
        assert!(plugged(&db, Engine::Claude, FILO_AGENT).unwrap().is_empty());
        let tools = [
            json!({ "name": "search", "annotations": { "readOnlyHint": true } }),
            json!({ "name": "create_issue", "annotations": { "destructiveHint": false } }),
            json!({ "name": "delete_repo" }),
        ];
        let tools: Vec<Tool> = tools.iter().filter_map(tool_from).collect();
        assert!(tools[0].read_only && !tools[1].read_only && !tools[1].destructive && tools[2].destructive);
        record_check(
            &db,
            &app.id,
            &Check {
                status: READY.into(),
                detail: String::new(),
                tools: Some(tools),
                server_name: Some("Docs".into()),
            },
            4,
        )
        .unwrap();
        let app = find(&db, &app.id).unwrap();
        assert_eq!(
            (app.status.as_str(), app.tools.len(), app.server_name.as_str(), app.checked_at),
            (READY, 3, "Docs", Some(4))
        );
        // A new app: every tool on, those that change things too, until the user turns one off.
        assert!(
            app.all_tools
                && app.tool_enabled("search")
                && app.tool_enabled("create_issue")
                && app.tool_enabled("delete_repo")
        );
        assert_eq!(enabled_count(&app), 3);
        let app = set_tools_enabled(&db, &app.id, &["create_issue".into(), "delete_repo".into()], false, 5).unwrap();
        assert!(app.tool_enabled("search") && !app.tool_enabled("create_issue") && !app.tool_enabled("delete_repo"));
        let app = set_tool_enabled(&db, &app.id, "create_issue", true, 6).unwrap();
        assert!(app.tool_enabled("create_issue") && !app.tool_enabled("delete_repo"));
        // One the server adds later is on too.
        assert!(app.tool_enabled("added_later"));
        // An app connected before: only what reads is on until the user says otherwise.
        let old = add(&db, &remote_fields("Old", "https://mcp.context7.com/mcp", AUTH_NONE), 7).unwrap();
        record_check(
            &db,
            &old.id,
            &Check { status: READY.into(), detail: String::new(), tools: Some(app.tools.clone()), server_name: None },
            8,
        )
        .unwrap();
        connected_before(&db, &old.id);
        let old = find(&db, &old.id).unwrap();
        assert!(
            !old.all_tools
                && old.tool_enabled("search")
                && !old.tool_enabled("create_issue")
                && !old.tool_enabled("delete_repo")
                && !old.tool_enabled("added_later")
        );
        assert_eq!(enabled_count(&old), 1);
        let old = set_tool_enabled(&db, &old.id, "create_issue", true, 9).unwrap();
        let old = set_tool_enabled(&db, &old.id, "search", false, 10).unwrap();
        assert!(!old.tool_enabled("search") && old.tool_enabled("create_issue") && !old.tool_enabled("delete_repo"));
        assert_eq!(old.disabled_tools, ["search"]);
    }

    #[test]
    fn agents_get_an_app_only_as_the_user_allows() {
        let db = Db::in_memory().unwrap();
        let app = add(&db, &remote_fields("Docs", "https://mcp.context7.com/mcp", AUTH_NONE), 1).unwrap();
        // Every agent by default, new ones too.
        assert_eq!((app.agent_scope.as_str(), app.agents.len()), (ALL_AGENTS, 0));
        assert!(app.allows_agent(FILO_AGENT) && app.allows_agent("chef") && app.allows_agent("an-agent-made-later"));
        let app =
            set_agent_access(&db, &app.id, false, &["chef".into(), " chef ".into(), FILO_AGENT.into()], 2).unwrap();
        assert_eq!(app.agents, [FILO_AGENT.to_string(), "chef".to_string()]);
        assert!(app.allows_agent("chef") && app.allows_agent(FILO_AGENT) && !app.allows_agent("tutor"));
        assert_eq!(plugged(&db, Engine::Claude, "chef").unwrap().len(), 1);
        assert!(plugged(&db, Engine::Claude, "tutor").unwrap().is_empty());
        // Only the agents chosen, even none: then no turn gets it.
        let app = set_agent_access(&db, &app.id, false, &[], 3).unwrap();
        assert!(!app.allows_agent(FILO_AGENT) && plugged(&db, Engine::Codex, FILO_AGENT).unwrap().is_empty());
        // The engine switch still counts on its own.
        let app = set_agent_access(&db, &app.id, true, &[], 4).unwrap();
        set_engine(&db, &app.id, Engine::Codex, false, 5).unwrap();
        assert!(
            plugged(&db, Engine::Codex, "chef").unwrap().is_empty()
                && plugged(&db, Engine::Claude, "chef").unwrap().len() == 1
        );
        let proxy = proxy_args(&app.id, std::path::Path::new("/d/filo.db"), "chef");
        assert_eq!(proxy[proxy.len() - 2..], ["--agent".to_string(), "chef".to_string()]);
    }

    #[test]
    fn a_table_made_before_agent_access_gets_its_columns() {
        let db = Db::in_memory().unwrap();
        db.with(|conn| {
            conn.execute_batch("DROP TABLE IF EXISTS apps;")?;
            conn.execute_batch(&SCHEMA.replace("    agent_scope    TEXT NOT NULL DEFAULT 'all',\n    agents         TEXT NOT NULL DEFAULT '[]',\n", "").replace("    enabled_tools  TEXT NOT NULL DEFAULT '[]',\n", ""))
        })
        .unwrap();
        let columns = |db: &Db| {
            db.with(|conn| {
                conn.prepare("SELECT name FROM pragma_table_info('apps')")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
        };
        assert!(
            !columns(&db).contains(&"agent_scope".to_string()) && !columns(&db).contains(&"enabled_tools".to_string())
        );
        let app = add(&db, &remote_fields("Old", "https://example.com/mcp", AUTH_NONE), 1).unwrap();
        assert!(columns(&db).contains(&"agents".to_string()));
        assert!(app.allows_agent("anyone") && app.enabled_tools.is_empty());
    }

    #[test]
    fn apps_connected_before_every_tool_started_on_keep_their_tools() {
        let db = Db::in_memory().unwrap();
        // The table as it was, with an app the user had set up: one tool turned on, one off.
        let tools = to_json(
            &[
                json!({ "name": "search", "annotations": { "readOnlyHint": true } }),
                json!({ "name": "list" }),
                json!({ "name": "create_issue" }),
                json!({ "name": "delete_repo" }),
            ]
            .iter()
            .filter_map(tool_from)
            .collect::<Vec<_>>(),
        );
        db.with(|conn| {
            conn.execute_batch("DROP TABLE IF EXISTS apps;")?;
            conn.execute_batch(&SCHEMA.replace("    all_tools      INTEGER NOT NULL DEFAULT 1,\n", ""))?;
            conn.execute(
                "INSERT INTO apps (id, slug, name, transport, url, status, enabled_tools, disabled_tools, tools, created_at, updated_at)
                 VALUES ('old', 'old', 'Old', 'http', 'https://example.com/mcp', 'ready', '[\"create_issue\"]', '[\"search\"]', ?1, 1, 1)",
                [&tools],
            )
        })
        .unwrap();
        let old = find(&db, "old").unwrap();
        assert!(!old.all_tools);
        // As before: the one turned on, not the one turned off, and of the rest only what reads (none here).
        assert!(
            old.tool_enabled("create_issue")
                && !old.tool_enabled("search")
                && !old.tool_enabled("list")
                && !old.tool_enabled("delete_repo")
        );
        assert_eq!(enabled_count(&old), 1);
        // One connected now starts with every tool on, in the same table.
        let new = add(&db, &remote_fields("New", "https://example.com/mcp", AUTH_NONE), 2).unwrap();
        record_check(
            &db,
            &new.id,
            &Check {
                status: READY.into(),
                detail: String::new(),
                tools: Some(serde_json::from_str(&tools).unwrap()),
                server_name: None,
            },
            3,
        )
        .unwrap();
        let new = find(&db, &new.id).unwrap();
        assert!(new.all_tools && enabled_count(&new) == 4);
        assert!(!find(&db, "old").unwrap().all_tools);
    }

    #[test]
    fn on_an_app_connected_before_tools_that_only_read_are_on_and_others_wait() {
        let tool = |value: Value| tool_from(&value).unwrap();
        let checked: &[&str] = &["ask_wiki_question", "list_issues"];
        // The server's mark wins, whatever the name or the list.
        assert!(on_by_default(
            &tool(json!({ "name": "delete_everything", "annotations": { "readOnlyHint": true } })),
            &[]
        ));
        assert!(!on_by_default(
            &tool(json!({ "name": "list_issues", "annotations": { "readOnlyHint": false } })),
            checked
        ));
        assert!(!on_by_default(
            &tool(json!({ "name": "list_issues", "annotations": { "destructiveHint": true } })),
            checked
        ));
        // Unmarked: only a preset's checked tools. A name that sounds like reading isn't enough.
        assert!(on_by_default(&tool(json!({ "name": "ask_wiki_question" })), checked));
        for name in ["list_repos", "get_page", "search", "read_file"] {
            assert!(!on_by_default(&tool(json!({ "name": name })), checked), "{name}");
            assert!(!on_by_default(&tool(json!({ "name": name })), &[]), "{name}");
        }
        let db = Db::in_memory().unwrap();
        let deepwiki = add(&db, &preset_fields(preset("deepwiki").unwrap()), 1).unwrap();
        connected_before(&db, &deepwiki.id);
        let listed = vec![
            tool(json!({ "name": "ask_wiki_question" })),
            tool(json!({ "name": "read_wiki_structure" })),
            tool(json!({ "name": "new_unknown_tool" })),
        ];
        record_check(
            &db,
            &deepwiki.id,
            &Check { status: READY.into(), detail: String::new(), tools: Some(listed.clone()), server_name: None },
            2,
        )
        .unwrap();
        let deepwiki = find(&db, &deepwiki.id).unwrap();
        assert!(
            deepwiki.tool_enabled("ask_wiki_question")
                && deepwiki.tool_enabled("read_wiki_structure")
                && !deepwiki.tool_enabled("new_unknown_tool")
        );
        // The same tools on a server of the user's own wait to be turned on.
        let custom = add(&db, &remote_fields("Wiki", "https://mcp.deepwiki.com/mcp", AUTH_NONE), 3).unwrap();
        connected_before(&db, &custom.id);
        record_check(
            &db,
            &custom.id,
            &Check { status: READY.into(), detail: String::new(), tools: Some(listed), server_name: None },
            4,
        )
        .unwrap();
        assert_eq!(enabled_count(&find(&db, &custom.id).unwrap()), 0);
    }

    #[test]
    fn presets_make_apps_with_their_own_settings() {
        let db = Db::in_memory().unwrap();
        for preset in PRESETS {
            let app = add(&db, &preset_fields(preset), 1).unwrap();
            assert_eq!(
                (app.auth.as_str(), app.url.as_str(), app.preset.as_deref()),
                (preset.auth, preset.url, Some(preset.id)),
                "{}",
                preset.id
            );
        }
        let linear = from_preset(&db, "linear").unwrap().unwrap();
        assert_eq!((linear.auth.as_str(), linear.transport.as_str()), (AUTH_OAUTH, HTTP));
        let github = from_preset(&db, "github").unwrap().unwrap();
        assert_eq!((github.key_name.as_str(), github.key_prefix.as_str()), ("Authorization", "Bearer "));
    }

    #[test]
    fn command_lines_split_like_a_shell() {
        assert_eq!(split_command_line(r#"npx -y "a b" 'c d' e\ f"#).unwrap(), ["npx", "-y", "a b", "c d", "e f"]);
        assert_eq!(split_command_line("  ").unwrap(), Vec::<String>::new());
        assert!(split_command_line("npx 'open").is_err());
        assert_eq!(slug_of("GitHub — Issues!"), "github-issues");
        assert_eq!(slug_of("!!!"), "app");
    }
}

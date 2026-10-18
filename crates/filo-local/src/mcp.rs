// The MCP server with Filo's tools, over stdio (newline-delimited JSON-RPC 2.0, MCP's stdio transport).
//
// The CLI starts one for each turn (`filo --local-mcp …` from the app, `filo-local mcp …` from a terminal) with the
// chat and turn it serves, so everything it does lands in that chat. Its tools:
//
// - `send_message { text, files }`: a message to the user in the chat. The only way a reply reaches the chat. `files`
//   are paths on this Mac sent with it, shown where they are with no copy (attachments.rs `in_place`) as the user's own
//   attachments are (pictures and videos as tiles, the rest as file chips). In a turn of the
//   agents' group chat (`--group-speaker <filo or an agent's id>`), it posts in the group in that member's name
//   (group.rs) instead, with an optional `ask` list of the members (or everyone) who answer next: only they do, never
//   one its text @-names; the other tools still belong to the member's own conversation (`--chat`). A group turn run in
//   the member's own chat (`--own-chat` too, local_backend/group.rs) also leaves a line there for each post, which
//   links to it (store.rs `Message::in_group`).
// - `save_memory`, `update_memory`, `forget_memory`, `search_memories`: what Filo remembers about the user, in Filo's
//   database (memories.rs), the same the Memories page shows. The turn's prompt lists them with their ids.
// - `create_reminder`, `create_routine`, `schedule_check_in` and their list / change / cancel tools: scheduled work in
//   Filo's database (schedule_mcp.rs), which fires on this Mac even when the app is closed. A check-in planned in a
//   group turn comes back in the group.
// - `publish_artifact`, `open_artifact`, `list_artifacts`: reports, pages, diagrams and tools in the user's Artifacts
//   library, published from an HTML file the agent built (artifacts_mcp.rs), shown as cards in this conversation (in
//   the group, for a group turn).
// - `list_notes`, `read_note`, `create_note`, `update_note`, `delete_note`: the user's notes (notes_mcp.rs), the Notes
//   tab's.
// - `start_work`, `list_coding_projects`, `watch_coding_task`, `tell_coding_task`, `stop_coding_task`,
//   `answer_coding_request`, `ask_user_about_coding_request`: coding work handed to a coding tool on this Mac,
//   owned by this conversation (the group's, in a group turn; the member's own, in one run there), which hears when it
//   asks something or finishes
//   (coding_mcp.rs, coding_handoff.rs).
// - `show_glance`: a small visual above the reply (glances_mcp.rs), as a message with its card; in the group, for a
//   group turn.
// - `publish_feed_post`, `update_feed_post`, `list_feed_posts`, `remove_feed_post`, `list_feed_topics`,
//   `publish_feed_glance`, `update_feed_glance`: posts in the user's Feed, shared in this conversation's name
//   (feed_mcp.rs).
// - `generate_images`, `save_pet`: pictures made with the user's Codex, shown in the chat as it drew them, and a desktop
//   pet made of four of them, saved among the user's characters (characters_mcp.rs, the desktop-pet skill).
// - `ask_agents { message, ask }`: from one of the user's chats (not the group), Filo or an agent asks the others it
//   lists in `ask` (by name, or everyone): in the group chat while it's on and both are in it (group.rs `ask`), else
//   each in its own chat (agent_asks.rs); the asker gets a turn back in this chat when they've answered. Offered while
//   there's someone to ask.
//
// Engines load most of these on demand (Claude Code through its tool search, Codex inside its code mode's `exec`);
// `PINNED` are always loaded: the core tools above (send_message to ask_agents), create_note (so "take a note" lands on
// the Notes tab rather than in a memory), the coding hand-off and show_glance.
// Claude Code reads the pin from each tool's `_meta` ("anthropic/alwaysLoad"); Codex has no per-tool pin, so a Codex
// turn starts this server twice, once with `--tools pinned` (`filo_core`, which Codex gives the model directly) and
// once with `--tools deferred` (`filo`), engine.rs.
//
// Feed and Notes are optional pages (pages.rs): while one is off, its tools aren't listed, allowed or pinned, a call to
// one is refused, and the prompt (engine.rs `capabilities`) leaves it out (for Notes, it says Notes is off, so a note
// asked for isn't kept as a memory instead). `pages` is the one place that's read.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::db::{self, Db};
use crate::memories;
use crate::pages::{Page, Pages};
use crate::store::{self, Store};

pub const SERVER_NAME: &str = "filo";
/// The second server a Codex turn starts, with only the pinned tools (`--tools pinned`).
pub const CORE_SERVER_NAME: &str = "filo_core";
/// The tools every turn has loaded from the start; the rest are found when needed. The first six are the core, then
/// create_note, the coding hand-off (coding_mcp.rs) and show_glance.
const PINNED: [&str; 15] = [
    "send_message",
    "save_memory",
    "update_memory",
    "forget_memory",
    "search_memories",
    "ask_agents",
    "create_note",
    "list_coding_projects",
    "start_work",
    "watch_coding_task",
    "tell_coding_task",
    "stop_coding_task",
    "answer_coding_request",
    "ask_user_about_coding_request",
    crate::glances_mcp::NAME,
];
/// The MCP version Filo's servers speak when a client asks for none, and its proxy asks the apps' servers for.
pub const PROTOCOL: &str = "2025-06-18";
/// One message at most this long (a runaway tool call shouldn't fill the disk).
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// Files one send_message carries at most, as the user's composer sends them.
const MAX_FILES: usize = crate::attachments::MAX_PER_MESSAGE;

/// Which chat and turn a server writes to.
#[derive(Clone, Debug)]
pub struct Target {
    pub store: Store,
    pub chat_id: String,
    pub turn_id: Option<String>,
    /// Filo's database, for the memories.
    pub db: PathBuf,
    /// A turn in the agents' group chat: who's speaking (an agent's id). send_message posts there.
    pub group_speaker: Option<String>,
    /// That group turn runs in the speaker's own chat (`chat_id`, local_backend/group.rs): each post leaves a line
    /// there, and its coding tasks belong there.
    pub own_chat: bool,
    /// Which of the tools it lists.
    pub part: Part,
}

/// Which of Filo's tools a server lists: all of them, or the pinned or deferred ones (`--tools pinned|deferred`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Part {
    #[default]
    All,
    Pinned,
    Deferred,
}

impl Part {
    fn parse(value: &str) -> Option<Part> {
        match value {
            "pinned" => Some(Part::Pinned),
            "deferred" => Some(Part::Deferred),
            _ => None,
        }
    }

    fn lists(self, tool: &str) -> bool {
        match self {
            Part::All => true,
            Part::Pinned => PINNED.contains(&tool),
            Part::Deferred => !PINNED.contains(&tool),
        }
    }
}

impl Target {
    /// `--chat <id> [--turn <id>] [--dir <store root>] [--db <database>] [--group-speaker <id> [--own-chat]] [--tools pinned|deferred]`;
    /// the database is `filo.db` in the store's
    /// folder unless given (an agent's conversation is in a folder of its own, the database isn't).
    pub fn from_args(args: &[String]) -> Result<Target, String> {
        let mut chat_id = None;
        let mut turn_id = None;
        let mut dir = None;
        let mut db_path = None;
        let mut group_speaker = None;
        let mut own_chat = false;
        let mut part = Part::All;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--own-chat" {
                own_chat = true;
                continue;
            }
            let mut value = || args.next().cloned().ok_or_else(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--chat" => chat_id = Some(value()?),
                "--turn" => turn_id = Some(value()?),
                "--dir" => dir = Some(PathBuf::from(value()?)),
                "--db" => db_path = Some(PathBuf::from(value()?)),
                "--group-speaker" => group_speaker = Some(value()?),
                "--tools" => part = Part::parse(&value()?).ok_or("--tools is pinned or deferred")?,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        let chat_id = chat_id.ok_or("--chat is required")?;
        let ids = [Some(&chat_id), turn_id.as_ref(), group_speaker.as_ref()];
        if ids.into_iter().flatten().any(|id| !store::valid_id(id)) {
            return Err("chat, turn and speaker ids are letters, digits and dashes".into());
        }
        let store = dir.map(Store::at).unwrap_or_else(Store::open_default);
        // A full path, so the data folder beside it (`data_dir`) is one wherever the server runs from.
        let db = db_path.unwrap_or_else(|| store.root().join(db::FILE));
        let db = std::path::absolute(&db).map_err(|error| format!("--db {}: {error}", db.display()))?;
        if own_chat && group_speaker.is_none() {
            return Err("--own-chat goes with --group-speaker".into());
        }
        Ok(Target { store, chat_id, turn_id, db, group_speaker, own_chat, part })
    }
}

/// The arguments that make a server for this chat and turn (after the program's own, like `--local-mcp`).
pub fn args_for(store: &Store, chat_id: &str, turn_id: &str) -> Vec<String> {
    vec![
        "--chat".into(),
        chat_id.into(),
        "--turn".into(),
        turn_id.into(),
        "--dir".into(),
        store.root().display().to_string(),
    ]
}

/// And for a group turn `speaker` takes in its own chat (`Message::group_call`).
pub fn own_chat_group_args(speaker: &str) -> Vec<String> {
    vec!["--group-speaker".into(), speaker.into(), "--own-chat".into()]
}

/// Runs a server on stdin/stdout until the client closes stdin. Returns the process exit code.
pub fn main_from_args(args: &[String]) -> i32 {
    // `--app <id> --db <filo.db>`: one of the user's apps for the CLI, through Filo's proxy (apps_client.rs).
    if args.iter().any(|arg| arg == "--app") {
        return crate::apps_client::proxy_main(args);
    }
    // `--computer --dir <local>`: the apps on this Mac (computer.rs), when "Your computer" is on.
    if args.iter().any(|arg| arg == "--computer") {
        return crate::computer::main_from_args(args);
    }
    let target = match Target::from_args(args) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("filo mcp: {error}");
            return 2;
        }
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    match serve_stdio(&target, stdin.lock(), stdout.lock()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("filo mcp: {error}");
            1
        }
    }
}

/// An MCP server Filo serves on stdin/stdout: a chat's tools (here), the Feed's on their own (feed_mcp.rs) and the
/// computer's (computer.rs). `serve_stdio` speaks the protocol for all three; each says who it is and does its tools.
pub(crate) trait StdioServer {
    /// `serverInfo`'s name and title, and the instructions the client is given.
    fn info(&self) -> (&'static str, &'static str, &'static str);
    /// What `tools/list` answers: the tools, as an array.
    fn tools(&self) -> Value;
    /// A `tools/call` request's result (`tool_result`).
    fn call(&self, params: &Value) -> Value;
}

impl StdioServer for Target {
    fn info(&self) -> (&'static str, &'static str, &'static str) {
        (
            SERVER_NAME,
            "Filo",
            "Filo's tools for the chat the user is having with you in the Filo app. Reply to the user with send_message: your plain output is not shown to them.",
        )
    }

    fn tools(&self) -> Value {
        tools_for(self)
    }

    fn call(&self, params: &Value) -> Value {
        call_tool(self, params)
    }
}

/// Newline-delimited JSON-RPC 2.0 until the client closes `input`: requests one by one or in batches, a reply to
/// each that has an id.
pub(crate) fn serve_stdio(server: &impl StdioServer, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(|request| handle(server, request)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(request) => handle(server, &request),
            Err(_) => Some(error_reply(Value::Null, -32700, "Parse error")),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// One request's reply; `None` for notifications (no id).
pub(crate) fn handle(server: &impl StdioServer, request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str);
    let id = request.get("id").cloned();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let id = match (method, id) {
        // A notification (`notifications/initialized`, `notifications/cancelled`, …) or a stray reply: nothing to say.
        (_, None) | (None, _) => return None,
        (Some(_), Some(id)) => id,
    };
    let result = match method.unwrap_or_default() {
        "initialize" => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
            let (name, title, instructions) = server.info();
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": name, "title": title, "version": env!("CARGO_PKG_VERSION") },
                "instructions": instructions,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": server.tools() }),
        "tools/call" => server.call(&params),
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        other => return Some(error_reply(id, -32601, &format!("Method not found: {other}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool's text argument, as given.
pub(crate) fn text_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// A tool's `id` argument, trimmed; `missing` says what to give when there's none.
pub(crate) fn id_arg<'a>(args: &'a Value, missing: &str) -> Result<&'a str, String> {
    text_arg(args, "id").map(str::trim).filter(|id| !id.is_empty()).ok_or_else(|| missing.to_string())
}

/// A tool call's result: its text, an error when it failed.
pub(crate) fn tool_result(outcome: Result<String, String>) -> Value {
    let (text, failed) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    tool_content(vec![json!({ "type": "text", "text": text })], failed)
}

/// A tool call's result made of `content` (text, images).
pub(crate) fn tool_content(content: Vec<Value>, failed: bool) -> Value {
    json!({ "content": content, "isError": failed })
}

/// Which optional pages are on, as the database at `db` says now: the tools, the allow-list and the prompt follow it,
/// from the next turn.
pub fn pages(db: &Path) -> Pages {
    Db::open(db).map(|db| crate::pages::get(&db)).unwrap_or_default()
}

/// The page whose tool `tool` is, while that page is off.
fn page_off(pages: Pages, tool: &str) -> Option<Page> {
    let page = if crate::notes_mcp::handles(tool) {
        Page::Notes
    } else if crate::feed_mcp::handles(tool) {
        Page::Feed
    } else {
        return None;
    };
    (!pages.on(page)).then_some(page)
}

/// `--allowedTools` for Claude Code: every tool here (`mcp__filo__<tool>`) the pages on leave, comma separated.
pub fn claude_allowed_tools(pages: Pages) -> String {
    let tools = tools(pages);
    let names = tools.as_array().into_iter().flatten().filter_map(|tool| tool.get("name")?.as_str());
    names.map(|name| format!("mcp__{SERVER_NAME}__{name}")).collect::<Vec<_>>().join(",")
}

/// Every tool, less those of the pages that are off.
fn tools(pages: Pages) -> Value {
    let kinds = memories::TYPES;
    let save_memory = if pages.notes {
        format!(
            "{SAVE_MEMORY} Not for notes: when the user asks you to take, make or save a note, that's create_note. Returns the memory's id."
        )
    } else {
        format!("{SAVE_MEMORY} Returns the memory's id.")
    };
    let mut tools = json!([{
        "name": "send_message",
        "title": "Send message",
        "description": "Send a message to the user in this Filo chat. This is the only way the user sees anything from you: your plain text output is not shown. Markdown works. Send your whole reply in one message unless it reads better as a few short ones. To send pictures, videos or other files, list their paths in `files`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The message, in Markdown." },
                "files": files_property()
            },
            "required": ["text"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    }, {
        "name": "save_memory",
        "title": "Save memory",
        "description": save_memory,
        "inputSchema": {
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "The memory, one short statement (at most 500 characters)." },
                "type": { "type": "string", "enum": kinds, "description": "What kind of memory it is." },
                "explanation": { "type": "string", "description": "Where it came from, shown to the user, like \"You mentioned it while planning a trip\"." },
                "confirmed": { "type": "boolean", "description": "True when the user said it outright; false (the default) when you inferred it." }
            },
            "required": ["content"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    }, {
        "name": "update_memory",
        "title": "Update memory",
        "description": "Correct a memory that changed or was wrong, by its id (from the list of what you remember, or search_memories). Only what's given changes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "content": { "type": "string", "description": "The memory as it is now." },
                "type": { "type": "string", "enum": kinds },
                "explanation": { "type": "string" },
                "confirmed": { "type": "boolean", "description": "True when the user confirmed or corrected it." }
            },
            "required": ["id"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    }, {
        "name": "forget_memory",
        "title": "Forget memory",
        "description": "Forget a memory for good, by its id: when the user asks you to forget something, or it's no longer true and there's nothing to keep.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
    }, {
        "name": "search_memories",
        "title": "Search memories",
        "description": "Look up what you remember about the user: the memories containing every word of `query` (empty for all), newest first, with their ids.",
        "inputSchema": {
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    }]);
    // Reminders, routines and check-ins (schedule_mcp.rs).
    if let Some(list) = tools.as_array_mut() {
        list.extend(crate::schedule_mcp::tools());
        // The Feed's posts (feed_mcp.rs).
        list.extend(crate::feed_mcp::tools());
        // Artifacts: reports, pages, diagrams and tools (artifacts_mcp.rs).
        list.extend(crate::artifacts_mcp::tools());
        // The Notes tab's notes (notes_mcp.rs).
        list.extend(crate::notes_mcp::tools());
        // Glances: small visuals above the reply (glances_mcp.rs).
        list.extend(crate::glances_mcp::tools());
        // Coding work handed to a coding tool on this Mac, and followed (coding_mcp.rs).
        list.extend(crate::coding_mcp::tools());
        // Pictures and desktop pets (characters_mcp.rs).
        list.extend(crate::characters_mcp::tools());
        // Filo's browser, in the app (browser_mcp.rs).
        list.extend(crate::browser_mcp::tools());
        list.push(ask_agents_tool());
        list.retain(|tool| tool["name"].as_str().is_some_and(|name| page_off(pages, name).is_none()));
        for tool in list.iter_mut().filter(|tool| tool["name"].as_str().is_some_and(|name| PINNED.contains(&name))) {
            tool["_meta"] = json!({ ALWAYS_LOAD: true });
        }
    }
    tools
}

/// save_memory's description, before what it says of notes.
const SAVE_MEMORY: &str = "Remember something lasting about the user (a preference, fact, goal, relationship, life event or ongoing situation) for later conversations, as one short statement in the third person, like \"Prefers metric units\". The user sees it on Filo's Memories page. Saving one you already have just keeps it.";

/// Claude Code's mark for a tool it loads from the start rather than through tool search.
const ALWAYS_LOAD: &str = "anthropic/alwaysLoad";

fn ask_agents_tool() -> Value {
    json!({
        "name": "ask_agents",
        "title": "Ask agents",
        "description": ASK_AGENTS,
        "inputSchema": {
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "The message for the group (at most 4000 characters)." },
                "ask": ask_property()
            },
            "required": ["message", "ask"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    })
}

/// ask_agents' description; the turn's list adds who can be asked.
const ASK_AGENTS: &str = "Bring in Filo (the user's main assistant) or another of the agents the user made. Write the message, and list each one you want in `ask` by name, like message \"Plan dinners around 140 g of protein\" with ask [\"Chef\"], or ask [\"everyone\"] to ask all of them. Only those in `ask` answer: an @Name in the message is just a name. They answer in the group chat you share while it's on (the user reads it in the Agents tab), else each in its own chat, and you get a turn back in this chat with what they said, to pass it on: so tell the user only briefly that you asked. Use it when the user wants something from another agent, or when another agent's job fits part of the request better.";

/// The tools a turn is offered: `ask_agents` lists who can be asked, and goes in the group (where send_message takes
/// the ask list) or with nobody to ask. A server for part of them lists only that part, and none lists an off page's.
fn tools_for(target: &Target) -> Value {
    let mut tools = tools(pages(&target.db));
    if let Some(list) = tools.as_array_mut() {
        list.retain(|tool| tool["name"].as_str().is_some_and(|name| target.part.lists(name)));
        if target.group_speaker.is_some()
            && let Some(tool) = list.iter_mut().find(|tool| tool["name"] == "send_message")
        {
            tool["description"] = json!(if target.own_chat {
                format!("{GROUP_SEND_MESSAGE} {OWN_CHAT_LINE}")
            } else {
                GROUP_SEND_MESSAGE.to_string()
            });
            tool["inputSchema"]["properties"]["ask"] = ask_property();
            tool["inputSchema"]["properties"]["report"] = report_property();
        }
        let others = if target.group_speaker.is_none() { ask_targets(target) } else { Default::default() };
        if others.is_empty() {
            list.retain(|tool| tool["name"] != "ask_agents");
        } else if let Some(tool) = list.iter_mut().find(|tool| tool["name"] == "ask_agents") {
            tool["description"] = json!(format!(
                "{ASK_AGENTS} You can ask: {}.",
                others.iter().map(|name| format!("@{name}")).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    tools
}

fn call_tool(target: &Target, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    if let Some(page) = page_off(pages(&target.db), name) {
        return tool_result(Err(format!("{} is off in Settings", page.label())));
    }
    // Its answer can carry a picture (a screenshot).
    if crate::browser_mcp::handles(name) {
        return crate::browser_mcp::call(target, name, &arguments);
    }
    let outcome = match name {
        "send_message" => send_message(target, &arguments),
        "save_memory" | "update_memory" | "forget_memory" | "search_memories" => memory_tool(target, name, &arguments),
        name if crate::schedule_mcp::handles(name) => {
            crate::schedule_mcp::call(&target.db, &target.chat_id, group_chat(target).as_deref(), name, &arguments)
        }
        name if crate::feed_mcp::handles(name) => crate::feed_mcp::call(
            &target.db,
            &crate::feed_mcp::origin_in_chat(&target.db, &target.chat_id),
            name,
            &arguments,
        ),
        name if crate::artifacts_mcp::handles(name) => crate::artifacts_mcp::call(
            &target.db,
            &target.store.root().join("workspace"),
            &artifact_origin(target),
            name,
            &arguments,
        ),
        name if crate::notes_mcp::handles(name) => crate::notes_mcp::call(&target.db, name, &arguments),
        name if crate::coding_mcp::handles(name) => crate::coding_mcp::call(target, name, &arguments),
        name if crate::glances_mcp::handles(name) => crate::glances_mcp::call(target, &arguments),
        name if crate::characters_mcp::handles(name) => {
            crate::characters_mcp::call(target, &data_dir(target), name, &arguments)
        }
        "ask_agents" => ask_agents(target, &arguments),
        other => Err(format!("Unknown tool: {other}")),
    };
    tool_result(outcome)
}

/// The group's conversation, in a group turn: where its artifact cards show and its check-ins come back.
fn group_chat(target: &Target) -> Option<String> {
    target.group_speaker.as_ref()?;
    crate::group::session(&Db::open(&target.db).ok()?).ok().flatten()
}

/// Who an artifact saved from this turn says made it (the speaker, in a group turn), and the conversation and turn whose
/// card shows it: this chat's, or the group's.
fn artifact_origin(target: &Target) -> crate::artifacts::Origin {
    let author = crate::feed_mcp::origin_in_chat(&target.db, &target.chat_id).author;
    let chat_id = group_chat(target).unwrap_or_else(|| target.chat_id.clone());
    crate::artifacts::Origin { author, chat_id: Some(chat_id), turn_id: target.turn_id.clone() }
}

/// The local data folder: the database's. generate_images' pictures are kept in its attachments/, with the user's
/// files.
pub(crate) fn data_dir(target: &Target) -> PathBuf {
    target.db.parent().map(PathBuf::from).unwrap_or_default()
}

/// Whom this chat's speaker can ask (by name): everyone in the group but itself, while there's an agent.
fn ask_targets(target: &Target) -> Vec<String> {
    let Ok(db) = Db::open(&target.db) else { return Vec::new() };
    let members = crate::group::roster(&db);
    if members.len() < 2 {
        return Vec::new();
    }
    let asker = crate::group::speaker_of_chat(&db, &target.chat_id);
    members.into_iter().filter(|member| member.id != asker).map(|member| member.name).collect()
}

fn ask_agents(target: &Target, arguments: &Value) -> Result<String, String> {
    if target.group_speaker.is_some() {
        return Err("You're in the group already: list who you ask in send_message's `ask`.".into());
    }
    let message = arguments.get("message").and_then(Value::as_str).unwrap_or_default();
    let ask = ask_list(arguments);
    let db = Db::open(&target.db).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    let all = crate::group::roster(&db);
    if all.len() < 2 {
        return Err("The user has no agents to ask yet.".into());
    }
    let asker = crate::group::speaker_of_chat(&db, &target.chat_id);
    let asks = crate::group::asks(&ask, &asker, &all)?;
    let asked: Vec<crate::group::Member> = all
        .iter()
        .filter(|member| member.id != asker && (asks.everyone || asks.members.contains(&member.id)))
        .cloned()
        .collect();
    if asked.is_empty() {
        let names = all
            .iter()
            .filter(|member| member.id != asker)
            .map(|member| format!("\"{}\"", member.name))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Nobody was asked: list who you ask in `ask`, by name, or \"everyone\". You can ask {names}."
        ));
    }
    // With the group on, the members of it it asks answer there, when it's one of them; the rest, each in its own chat.
    let members = crate::group::members(&db);
    let in_group = crate::group::is_on(&db) && members.iter().any(|member| member.id == asker);
    let (there, apart): (Vec<crate::group::Member>, Vec<crate::group::Member>) =
        asked.into_iter().partition(|member| in_group && members.iter().any(|other| other.id == member.id));
    let now = crate::now_ms() as i64;
    let mut said = Vec::new();
    if !there.is_empty() {
        let names: Vec<&str> = there.iter().map(|member| member.name.as_str()).collect();
        let outcome = crate::group::ask(&db, &members, &target.chat_id, &asker, message, &names, now)?;
        said.push(format!("Asked {} in the group chat: they answer there.", outcome.asked.join(", ")));
    }
    if !apart.is_empty() {
        let names = crate::agent_asks::ask(
            &db,
            &target.store,
            &target.chat_id,
            &asker,
            &apart,
            message,
            target.turn_id.as_deref(),
            now,
        )?;
        said.push(format!("Asked {}: each works on it in its own chat.", names.join(", ")));
    }
    Ok(format!(
        "{} You get a turn back in this chat with what they said: tell the user briefly that you asked.",
        said.join(" ")
    ))
}

/// send_message's description in a turn of the agents' group chat.
const GROUP_SEND_MESSAGE: &str = "Post a message in the group chat of Filo and the user's agents, in your name. This is the only way anyone (the user included) sees anything from you: your plain text output is not shown. Markdown works. Keep `text` to one or two lines; the full report (what you did, checked or found, with its details) goes in `report`, which the group opens as a page from a row under your message. To ask members something, list them in `ask`: only they answer next. \"everyone\" reaches every member, each answering only if it's for them. An @Name in the text is just a name: it asks no one.";
/// The `ask` list of send_message in the group and of ask_agents (group.rs `asks`).
fn ask_property() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Who you ask, by name (like \"Chef\"), or \"everyone\" for all of them: only they answer next. Leave it out to ask no one."
    })
}

/// send_message's `report` in the group: the full report under the short post.
fn report_property() -> Value {
    json!({
        "type": "string",
        "description": "The full report behind your short message, in Markdown: what you did, checked or found, with its details. The group shows a row under your message that opens it as a page. Leave it out when the message says it all."
    })
}

/// send_message's `files`: files on this Mac sent with the message.
fn files_property() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "maxItems": MAX_FILES,
        "description": format!("Files on this Mac to send with the message, by their full paths (like \"/Users/me/Desktop/chart.png\"), up to {MAX_FILES}. Pictures and videos show in the chat as tiles the user opens to look at or play, other files as file chips they open. The chat shows each file where it is, without a copy: save what you send somewhere it stays, like your workspace (the folder you work in), not a temp folder, and don't delete it afterwards. A file that's renamed or moved on the same disk is still found; one that's deleted shows as no longer there. `text` can be empty when you send files.")
    })
}

/// The files sent with send_message (`files`), each where it is (its canonical path). Every path is checked first, so a
/// bad one sends nothing.
fn sent_files(arguments: &Value) -> Result<Vec<store::Attachment>, String> {
    let paths: Vec<&str> = match arguments.get("files") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(path)) => vec![path.as_str()],
        Some(Value::Array(paths)) => paths
            .iter()
            .map(|path| path.as_str().ok_or("`files` is a list of paths, each a string."))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("`files` is a list of paths to files on this Mac.".into()),
    };
    if paths.len() > MAX_FILES {
        return Err(format!(
            "Up to {MAX_FILES} files per message ({} given): send the rest in another message.",
            paths.len()
        ));
    }
    let paths = paths.into_iter().map(checked_file).collect::<Result<Vec<_>, _>>()?;
    paths
        .iter()
        .map(|path| {
            crate::attachments::in_place(path)
                .map_err(|error| format!("Couldn't open {} (from `files`): {error}. Nothing was sent.", path.display()))
        })
        .collect()
}

/// A path in `files`, as a file that's there: a full path (`~/` is the home folder) to a file, not a folder. Its
/// canonical path, so a link or `..` names the file itself.
fn checked_file(given: &str) -> Result<PathBuf, String> {
    let trimmed = given.trim();
    let path = match trimmed.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").filter(|home| !home.is_empty()).ok_or_else(|| {
            format!("\"{given}\" in `files` starts with ~/, but the home folder isn't known here: give its full path, like /Users/me/Desktop/chart.png. Nothing was sent.")
        })?)
        .join(rest),
        None => PathBuf::from(trimmed),
    };
    if !path.is_absolute() {
        return Err(format!(
            "\"{given}\" in `files` isn't a full path: give each file's absolute path, like /Users/me/Desktop/chart.png. Nothing was sent."
        ));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => path
            .canonicalize()
            .map_err(|error| format!("Couldn't open {} (from `files`): {error}. Nothing was sent.", path.display())),
        Ok(meta) if meta.is_dir() => Err(format!(
            "{} in `files` is a folder, not a file: send the files in it, or a .zip of it. Nothing was sent.",
            path.display()
        )),
        Ok(_) => Err(format!("{} in `files` isn't a regular file. Nothing was sent.", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(format!("There's no file at {} (from `files`). Nothing was sent.", path.display()))
        }
        Err(error) => Err(format!("Couldn't open {} (from `files`): {error}. Nothing was sent.", path.display())),
    }
}

/// The `ask` list sent with send_message or ask_agents: its names, one name given alone too.
fn ask_list(arguments: &Value) -> Vec<&str> {
    match arguments.get("ask") {
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(name)) => vec![name.as_str()],
        _ => Vec::new(),
    }
}
/// And when the group turn runs in the speaker's own chat.
const OWN_CHAT_LINE: &str = "Your own chat with the user shows it too, labelled as posted in the group.";

fn send_message(target: &Target, arguments: &Value) -> Result<String, String> {
    let text = arguments.get("text").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let sending_files = arguments.get("files").is_some_and(|files| match files {
        Value::Array(paths) => !paths.is_empty(),
        Value::Null => false,
        _ => true,
    });
    if text.is_empty() && !sending_files {
        return Err("send_message needs non-empty `text` (or `files` to send).".into());
    }
    if target.group_speaker.is_none() && text.len() > MAX_MESSAGE_BYTES {
        return Err(format!(
            "The message is too long ({} KB, at most {} KB). Send it in parts.",
            text.len() / 1024,
            MAX_MESSAGE_BYTES / 1024
        ));
    }
    if let Some(speaker) = &target.group_speaker {
        let db = Db::open(&target.db).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
        let members = crate::group::members(&db);
        let asks = crate::group::asks(&ask_list(arguments), speaker, &members)?;
        let asked = crate::group::asked_names(&asks, speaker, &members);
        let files = sent_files(arguments)?;
        let shown = shown_files(&files);
        let posted = crate::group::post_files(
            &db,
            speaker,
            text,
            files,
            asks,
            target.turn_id.as_deref(),
            crate::now_ms() as i64,
        )?;
        if let Some(report) =
            arguments.get("report").and_then(Value::as_str).map(str::trim).filter(|report| !report.is_empty())
        {
            crate::group::set_report(&db, &posted.id, report)?;
        }
        if target.own_chat {
            // The chat's line linking to the post; its text is what the chat's turn and later context go by, its files
            // the post's (shown while the post is gone too).
            let mut line = target.store.agent_message(&target.chat_id, posted.text, target.turn_id.clone());
            line.in_group = Some(posted.id);
            line.attachments = posted.attachments;
            target
                .store
                .append(&target.chat_id, &line)
                .map_err(|error| format!("Posted in the group, but couldn't note it in your chat: {error}"))?;
        }
        let posted = match asked {
            Some(asked) => format!("Posted in the group, asking {asked}: they answer next."),
            None => "Posted in the group. Everyone there sees it; it asks no one.".into(),
        };
        return Ok(format!("{posted}{shown}"));
    }
    let files = sent_files(arguments)?;
    let shown = shown_files(&files);
    let mut message = target.store.agent_message(&target.chat_id, text, target.turn_id.clone());
    message.attachments = files;
    target.store.append(&target.chat_id, &message).map_err(|error| format!("Couldn't save the message: {error}"))?;
    Ok(format!("Sent. The user sees it in the chat.{shown}"))
}

/// What send_message's result says of the files it sent: which, from where (so the agent can find and change them
/// later), and to leave them there. Empty without files.
fn shown_files(files: &[store::Attachment]) -> String {
    let at: Vec<String> = files.iter().map(|file| file.file.display().to_string()).collect();
    match at.as_slice() {
        [] => String::new(),
        [one] => format!(" Its file shows from where it is, {one}: leave it there."),
        all => format!(" Its {} files show from where they are, {}: leave them there.", all.len(), all.join(", ")),
    }
}

fn memory_tool(target: &Target, name: &str, arguments: &Value) -> Result<String, String> {
    let text = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::to_string);
    let confirmed = arguments.get("confirmed").and_then(Value::as_bool);
    let id = || text("id").filter(|id| !id.trim().is_empty()).ok_or_else(|| format!("{name} needs the memory's `id`."));
    let db = Db::open(&target.db).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    match name {
        "save_memory" => {
            let new = memories::New {
                content: text("content").unwrap_or_default(),
                kind: text("type"),
                source: "conversation".into(),
                source_id: Some(target.chat_id.clone()),
                explanation: text("explanation").unwrap_or_default(),
                confirmation: if confirmed == Some(true) { "confirmed" } else { "inferred" }.into(),
            };
            let id = memories::create(&db, &new)?;
            Ok(format!("Saved (id {id}). The user sees it on the Memories page."))
        }
        "update_memory" => {
            let change = memories::Change {
                content: text("content"),
                kind: text("type"),
                explanation: text("explanation"),
                confirmation: confirmed.map(|confirmed| if confirmed { "corrected" } else { "inferred" }.into()),
            };
            if memories::update(&db, id()?.trim(), &change)? {
                Ok("Updated.".into())
            } else {
                Err("There's no memory with that id.".into())
            }
        }
        "forget_memory" => {
            if memories::remove(&db, id()?.trim())? {
                Ok("Forgotten.".into())
            } else {
                Err("There's no memory with that id.".into())
            }
        }
        _ => {
            let found = memories::search(&db, &text("query").unwrap_or_default())?;
            if found.is_empty() {
                return Ok("No memories match.".into());
            }
            let lines: Vec<String> = found
                .iter()
                .take(100)
                .map(|memory| format!("- [{}] {} ({})", memory.id, memory.content, memory.kind.replace('_', " ")))
                .collect();
            let more = found.len().saturating_sub(lines.len());
            Ok(if more > 0 {
                format!("{}\n({more} more: narrow the query.)", lines.join("\n"))
            } else {
                lines.join("\n")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::store::Role;
    use crate::store::tests::temp_store;

    fn run(target: &Target, input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve_stdio(target, input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    #[test]
    fn handshake_list_and_send_message() {
        let store = temp_store("mcp");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("turn-1".into()),
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"  Hello **you**  "}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"   "}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"bogus"}"#,
            "not json",
        ]
        .join("\n");
        let replies = run(&target, &input);
        assert_eq!(replies.len(), 7, "{replies:#?}");
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "filo");
        assert_eq!(replies[1]["result"]["tools"][0]["name"], "send_message");
        assert_eq!(replies[2]["result"]["isError"], false);
        assert_eq!(replies[3]["result"]["isError"], true);
        assert_eq!(replies[4]["result"]["isError"], true);
        assert_eq!(replies[5]["error"]["code"], -32601);
        assert_eq!(replies[6]["error"]["code"], -32700);

        let messages = store.messages(&chat.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].text, "Hello **you**");
        assert_eq!(messages[0].turn.as_deref(), Some("turn-1"));
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_missing_chat_is_a_tool_error_and_args_parse() {
        let store = temp_store("mcp-missing");
        let target = Target {
            store: store.clone(),
            chat_id: "no-such-chat".into(),
            turn_id: None,
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let replies = run(
            &target,
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"send_message","arguments":{"text":"hi"}}}"#,
        );
        assert_eq!(replies[0]["id"], "a");
        assert_eq!(replies[0]["result"]["isError"], true);

        let args = args_for(&store, "chat-1", "turn-2");
        let parsed = Target::from_args(&args).unwrap();
        assert_eq!((parsed.chat_id.as_str(), parsed.turn_id.as_deref()), ("chat-1", Some("turn-2")));
        assert_eq!(parsed.store.root(), store.root());
        assert_eq!(parsed.db, store.root().join(db::FILE));
        let mut with_db = args.clone();
        with_db.extend(["--db".into(), "/elsewhere/filo.db".into()]);
        assert_eq!(Target::from_args(&with_db).unwrap().db, PathBuf::from("/elsewhere/filo.db"));
        assert!(Target::from_args(&["--chat".into(), "../x".into()]).is_err());
        assert!(Target::from_args(&[]).is_err());
    }

    #[test]
    fn memory_tools_save_update_search_and_forget() {
        let store = temp_store("mcp-memories");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("t".into()),
            db: store.root().join("data").join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let call = |id: u32, name: &str, arguments: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
            run(&target, &request.to_string()).remove(0)["result"].clone()
        };
        let text = |result: &Value| result["content"][0]["text"].as_str().unwrap().to_string();

        let saved = call(
            1,
            "save_memory",
            json!({ "content": "Has a cat named Miso", "type": "fact", "explanation": "Mentioned it in a chat" }),
        );
        assert_eq!(saved["isError"], false, "{saved}");
        let db = Db::open(&target.db).unwrap();
        let memory = memories::all(&db).unwrap().remove(0);
        assert!(text(&saved).contains(&memory.id));
        assert_eq!(
            (memory.kind.as_str(), memory.confirmation.as_str(), memory.provenance.source.as_str()),
            ("fact", "inferred", "conversation")
        );
        assert_eq!(memory.provenance.source_id.as_deref(), Some(chat.id.as_str()));

        assert_eq!(
            call(
                2,
                "update_memory",
                json!({ "id": memory.id, "content": "Has two cats, Miso and Tofu", "confirmed": true })
            )["isError"],
            false
        );
        let found = call(3, "search_memories", json!({ "query": "tofu" }));
        assert!(text(&found).contains(&format!("[{}] Has two cats, Miso and Tofu (fact)", memory.id)), "{found}");
        assert_eq!(memories::get(&db, &memory.id).unwrap().unwrap().confirmation, "corrected");
        assert_eq!(text(&call(4, "search_memories", json!({ "query": "dog" }))), "No memories match.");

        assert_eq!(call(5, "forget_memory", json!({ "id": memory.id }))["isError"], false);
        assert_eq!(call(6, "forget_memory", json!({ "id": memory.id }))["isError"], true);
        assert_eq!(call(7, "save_memory", json!({ "content": "  " }))["isError"], true);
        assert_eq!(call(8, "update_memory", json!({}))["isError"], true);
        assert!(memories::all(&db).unwrap().is_empty());

        let allowed = claude_allowed_tools(Pages::default());
        assert!(
            allowed.starts_with("mcp__filo__send_message,")
                && allowed.contains("mcp__filo__save_memory")
                && allowed.contains("mcp__filo__search_memories")
        );
        std::fs::remove_dir_all(store.root()).ok();
    }
    fn tool_names(replies: &[Value]) -> Vec<String> {
        replies[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_pinned_tools_load_from_the_start_and_the_rest_on_demand() {
        let store = temp_store("mcp-pinned");
        let chat = store.create_chat(Engine::Claude).unwrap();
        crate::app_data::tests::add_agents(
            &Db::open(&store.root().join(db::FILE)).unwrap(),
            json!([{ "id": "chef", "name": "Chef", "sessionId": "other", "createdAt": 1.0 }, { "id": crate::apps::FILO_AGENT, "name": "Filo", "sessionId": chat.id, "createdAt": 2.0 }]),
        );
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let target = |part| Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("t".into()),
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part,
        };

        // Every pinned tool is one Filo has, and with someone to ask, all fifteen are listed.
        let all = run(&target(Part::All), list).remove(0)["result"]["tools"].as_array().unwrap().clone();
        let pinned: Vec<&str> = all
            .iter()
            .filter(|tool| tool["_meta"][ALWAYS_LOAD] == true)
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(pinned.len(), PINNED.len(), "{pinned:?}");
        assert!(PINNED.iter().all(|name| pinned.contains(name)));
        assert!(
            all.len() > PINNED.len() + 10
                && all.iter().any(|tool| tool["name"] == "create_reminder" && tool.get("_meta").is_none())
        );

        // Codex's two servers split them: the pinned ones on one, the rest on the other, none on both.
        let names = |part| tool_names(&run(&target(part), list));
        let (core, rest) = (names(Part::Pinned), names(Part::Deferred));
        assert_eq!(core.len(), PINNED.len());
        assert!(
            core.iter().all(|name| PINNED.contains(&name.as_str()))
                && !rest.iter().any(|name| PINNED.contains(&name.as_str()))
        );
        assert_eq!(core.len() + rest.len(), all.len());
        assert!(
            rest.contains(&"list_notes".to_string())
                && core.contains(&"create_note".to_string())
                && core.contains(&"send_message".to_string())
                && core.contains(&"show_glance".to_string())
        );

        let mut args = args_for(&store, &chat.id, "t");
        args.extend(["--tools".into(), "pinned".into()]);
        assert_eq!(Target::from_args(&args).unwrap().part, Part::Pinned);
        assert!(Target::from_args(&["--chat".into(), "c".into(), "--tools".into(), "some".into()]).is_err());
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn notes_are_listed_created_edited_and_deleted_through_the_server() {
        let store = temp_store("mcp-notes");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("t".into()),
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let call = |name: &str, arguments: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
            run(&target, &request.to_string()).remove(0)["result"].clone()
        };
        let text = |result: &Value| result["content"][0]["text"].as_str().unwrap().to_string();
        let names = tool_names(&run(&target, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#));
        for name in ["list_notes", "read_note", "create_note", "update_note", "delete_note"] {
            assert!(names.iter().any(|tool| tool == name), "{name} in {names:?}");
        }
        assert!(claude_allowed_tools(Pages::default()).contains("mcp__filo__create_note"));

        assert_eq!(
            call("create_note", json!({ "title": "Trip", "body": "Passport", "tags": ["travel"] }))["isError"],
            false
        );
        let db = Db::open(&target.db).unwrap();
        let id = crate::notes::list(&db).unwrap()[0].id.clone();
        assert!(text(&call("list_notes", json!({ "query": "passport" }))).contains(&id));
        assert_eq!(call("update_note", json!({ "id": id, "body": "Passport, tickets" }))["isError"], false);
        assert_eq!(crate::notes::get(&db, &id).unwrap().unwrap().body, "Passport, tickets");
        assert_eq!(call("delete_note", json!({ "id": id }))["isError"], false);
        assert!(crate::notes::list(&db).unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn an_off_page_has_no_tools_and_its_calls_are_refused_while_its_data_stays() {
        let store = temp_store("mcp-pages");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("t".into()),
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let db = Db::open(&target.db).unwrap();
        let note = crate::notes::create(
            &db,
            &crate::notes::Fields { title: "Trip".into(), body: "Passport".into(), ..Default::default() },
        )
        .unwrap();
        for page in Page::ALL {
            crate::pages::set(&db, &chrono::Utc, page, false, 1).unwrap();
        }
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let call = |name: &str, arguments: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
            run(&target, &request.to_string()).remove(0)["result"].clone()
        };

        let all = run(&target, list).remove(0)["result"]["tools"].as_array().unwrap().clone();
        let names: Vec<&str> = all.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        assert!(
            !names.iter().any(|name| crate::notes_mcp::handles(name) || crate::feed_mcp::handles(name)),
            "{names:?}"
        );
        assert!(names.contains(&"save_memory") && names.contains(&"show_glance") && names.contains(&"create_reminder"));
        let save_memory = all.iter().find(|tool| tool["name"] == "save_memory").unwrap();
        assert!(!save_memory["description"].as_str().unwrap().contains("note"), "{save_memory}");
        // Not pinned on Codex's core server, nor allowed for Claude Code.
        let core = tool_names(&run(&Target { part: Part::Pinned, ..target.clone() }, list));
        assert!(core.contains(&"send_message".to_string()) && !core.contains(&"create_note".to_string()));
        let allowed = claude_allowed_tools(pages(&target.db));
        assert!(
            allowed.contains("mcp__filo__save_memory") && !allowed.contains("note") && !allowed.contains("feed"),
            "{allowed}"
        );

        let refused = call("read_note", json!({ "id": note }));
        assert_eq!(
            (refused["isError"].as_bool(), refused["content"][0]["text"].as_str()),
            (Some(true), Some("Notes is off in Settings"))
        );
        assert_eq!(call("list_feed_posts", json!({}))["content"][0]["text"], "Feed is off in Settings");

        // On again, the note is there as it was.
        crate::pages::set(&db, &chrono::Utc, Page::Notes, true, 2).unwrap();
        let read = call("read_note", json!({ "id": note }));
        assert_eq!(read["isError"], false, "{read}");
        assert!(read["content"][0]["text"].as_str().unwrap().contains("Passport"));
        assert!(claude_allowed_tools(pages(&target.db)).contains("mcp__filo__create_note"));
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn ask_agents_is_offered_in_chats_with_someone_to_ask_and_asks_in_the_group_or_their_own_chats() {
        let store = temp_store("mcp-ask");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("t".into()),
            db: store.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        // No agents: nobody to ask.
        assert!(!tool_names(&run(&target, list)).contains(&"ask_agents".to_string()));

        let agents = json!([
            { "id": "chef", "name": "Chef", "instructions": "Cooks.", "sessionId": "chef-chat", "createdAt": 2.0 },
            { "id": crate::apps::FILO_AGENT, "name": "Filo", "instructions": "Helps.", "sessionId": chat.id, "createdAt": 1.0 },
        ]);
        crate::app_data::tests::add_agents(&Db::open(&target.db).unwrap(), agents);
        let replies = run(&target, list);
        let tool = replies[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "ask_agents")
            .cloned()
            .unwrap();
        assert!(tool["description"].as_str().unwrap().ends_with("You can ask: @Chef."), "{tool}");

        let call = |target: &Target, message: &str, ask: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "ask_agents", "arguments": { "message": message, "ask": ask } } });
            run(target, &request.to_string()).remove(0)["result"].clone()
        };
        // Its message @-naming Chef asks no one: only the ask list does.
        let nobody = call(&target, "hello @Chef", json!([]));
        assert_eq!(nobody["isError"], true);
        assert!(nobody["content"][0]["text"].as_str().unwrap().contains("You can ask \"Chef\"."), "{nobody}");
        let unknown = call(&target, "hello", json!(["Baker"]));
        assert!(
            unknown["isError"] == true && unknown["content"][0]["text"].as_str().unwrap().contains("\"Baker\""),
            "{unknown}"
        );
        // The group off: Chef works on it in its own chat, and this chat says it asked.
        let apart = call(&target, "What's for lunch?", json!(["chef"]));
        assert!(
            apart["content"][0]["text"].as_str().unwrap().contains("Asked Chef: each works on it in its own chat."),
            "{apart}"
        );
        let db = Db::open(&target.db).unwrap();
        let queued = crate::agent_asks::in_state(&db, "queued").unwrap();
        assert_eq!(
            (queued.len(), queued[0].agent.as_str(), queued[0].origin_chat.as_str()),
            (1, "chef", chat.id.as_str())
        );
        assert!(crate::group::messages(&db, 10).unwrap().0.is_empty());
        let line = store.messages(&chat.id).unwrap().pop().unwrap();
        assert_eq!(line.group_link.and_then(|link| link.agent).as_deref(), Some("chef"));
        // On: it asks in the group.
        crate::group::switch(&db, true, 1).unwrap();
        let asked = call(&target, "What's for dinner?", json!(["chef"]));
        assert_eq!(asked["isError"], false, "{asked}");
        let (posted, _) = crate::group::messages(&db, 10).unwrap();
        // The Filo agent's chat asks as Filo, and reports back to this chat.
        assert_eq!(
            (posted[0].speaker.as_deref(), posted[0].text.as_str()),
            (Some(crate::apps::FILO_AGENT), "What's for dinner?")
        );
        assert_eq!(crate::group::active(&db).unwrap()[0].speaker, "chef");
        let origin: String =
            db.with(|conn| conn.query_row("SELECT origin_chat FROM group_asks", [], |row| row.get(0))).unwrap();
        assert_eq!(origin, chat.id);

        // In the group, send_message takes the ask list: no ask_agents.
        let in_group = Target { group_speaker: Some("chef".into()), ..target.clone() };
        assert!(!tool_names(&run(&in_group, list)).contains(&"ask_agents".to_string()));
        assert_eq!(call(&in_group, "hi", json!(["Filo"]))["isError"], true);
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn in_a_group_turn_send_message_asks_only_its_ask_list() {
        let store = temp_store("mcp-group-ask");
        let own = store.create_chat(Engine::Claude).unwrap();
        let db_path = store.root().join(db::FILE);
        let db = Db::open(&db_path).unwrap();
        crate::app_data::tests::add_agents(
            &db,
            json!([{ "id": "chef", "name": "Chef", "sessionId": own.id, "createdAt": 1.0 }]),
        );
        let target = Target {
            store: store.clone(),
            chat_id: own.id.clone(),
            turn_id: None,
            db: db_path,
            group_speaker: Some("chef".into()),
            own_chat: false,
            part: Part::All,
        };
        let listed = run(&target, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let tool = listed[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "send_message")
            .cloned()
            .unwrap();
        assert_eq!(tool["inputSchema"]["properties"]["ask"]["type"], "array", "{tool}");
        assert!(tool["description"].as_str().unwrap().contains("An @Name in the text is just a name"), "{tool}");
        let send = |arguments: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "send_message", "arguments": arguments } });
            run(&target, &request.to_string()).remove(0)["result"].clone()
        };
        let mentioned = send(json!({ "text": "As @Filo said, pasta." }));
        assert_eq!(mentioned["content"][0]["text"], "Posted in the group. Everyone there sees it; it asks no one.");
        let asked = send(json!({ "text": "Can you shop?", "ask": ["Filo"] }));
        assert_eq!(asked["content"][0]["text"], "Posted in the group, asking Filo: they answer next.");
        assert_eq!(send(json!({ "text": "Hi", "ask": ["Baker"] }))["isError"], true);
        let (posted, _) = crate::group::messages(&db, 10).unwrap();
        let asks: Vec<(&str, &[String])> =
            posted.iter().map(|message| (message.text.as_str(), message.asks.members.as_slice())).collect();
        assert_eq!(
            asks,
            [("As @Filo said, pasta.", &[][..]), ("Can you shop?", &[crate::apps::FILO_AGENT.to_string()][..])]
        );
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn in_a_group_turn_artifact_cards_and_check_ins_belong_to_the_group() {
        let store = temp_store("mcp-group-place");
        let own = store.create_chat(Engine::Claude).unwrap();
        let db_path = store.root().join(db::FILE);
        let db = Db::open(&db_path).unwrap();
        let group_session = crate::group::ensure(&db, 1).unwrap();
        crate::artifacts::ensure(&db).unwrap();
        crate::app_data::tests::add_agents(
            &db,
            json!([{ "id": "chef", "name": "Chef", "sessionId": own.id, "createdAt": 1.0 }]),
        );
        let target = Target {
            store: store.clone(),
            chat_id: own.id.clone(),
            turn_id: Some("group-turn".into()),
            db: db_path.clone(),
            group_speaker: Some("chef".into()),
            own_chat: false,
            part: Part::All,
        };
        let call = |name: &str, arguments: Value| {
            let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
            run(&target, &request.to_string()).remove(0)["result"].clone()
        };
        // A page in the turn's working folder, by its path there.
        std::fs::create_dir_all(store.workspace().unwrap().join("pages")).unwrap();
        std::fs::write(store.workspace().unwrap().join("pages/menu.html"), "<html><body>Pasta</body></html>").unwrap();
        assert_eq!(call("publish_artifact", json!({ "path": "pages/menu.html", "title": "Menu" }))["isError"], false);
        let made = crate::artifacts::list(&db, 1).unwrap().remove(0);
        assert_eq!((made.chat_id.as_deref(), made.author.as_str()), (Some(group_session.as_str()), "Chef"));
        assert_eq!(crate::artifacts::chat_cards(&db, &group_session).unwrap().len(), 1);
        assert!(crate::artifacts::chat_cards(&db, &own.id).unwrap().is_empty());

        assert_eq!(
            call("schedule_check_in", json!({ "title": "Did they cook it?", "in_minutes": 60 }))["isError"],
            false
        );
        let wake = crate::schedule::wakes(&db, None).unwrap().remove(0);
        assert_eq!((wake.chat_id.as_deref(), wake.agent_id.as_deref()), (Some(group_session.as_str()), Some("chef")));
        // Out of the group, the same chat's check-in comes back in it.
        let alone = Target { group_speaker: None, turn_id: Some("t".into()), ..target.clone() };
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "schedule_check_in", "arguments": { "title": "x", "in_minutes": 5 } } });
        run(&alone, &request.to_string());
        assert!(
            crate::schedule::wakes(&db, None)
                .unwrap()
                .iter()
                .any(|wake| wake.chat_id.as_deref() == Some(own.id.as_str()))
        );
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn in_a_group_turn_send_message_posts_in_the_group_as_the_speaker() {
        let store = temp_store("mcp-group");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let db_path = store.root().join(db::FILE);
        let db = Db::open(&db_path).unwrap();
        crate::group::add_user_message(&db, "thread-1", "@Chef dinner?", 1).unwrap();
        let turn = crate::group::enqueue(&db, "thread-1", "chef", 1, "asked", true, 1).unwrap().unwrap();
        let mut args = args_for(&store, &chat.id, &turn);
        args.extend(["--db".into(), db_path.display().to_string(), "--group-speaker".into(), "chef".into()]);
        let target = Target::from_args(&args).unwrap();
        assert_eq!(target.group_speaker.as_deref(), Some("chef"));
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"Pasta tonight. @Filo ok?"}}}"#,
        ]
        .join("\n");
        let replies = run(&target, &input);
        assert!(
            replies[0]["result"]["tools"][0]["description"]
                .as_str()
                .unwrap()
                .starts_with("Post a message in the group chat")
        );
        assert_eq!(replies[1]["result"]["isError"], false, "{:?}", replies[1]);
        let (messages, _) = crate::group::messages(&db, 10).unwrap();
        let posted = messages.last().unwrap();
        assert_eq!(
            (posted.speaker.as_deref(), posted.text.as_str(), posted.thread.as_deref()),
            (Some("chef"), "Pasta tonight. @Filo ok?", Some("thread-1"))
        );
        assert_eq!(posted.turn.as_deref(), Some(turn.as_str()));
        // Nothing went to the speaker's own chat.
        assert!(store.messages(&chat.id).unwrap().is_empty());
        assert!(Target::from_args(&["--chat".into(), "c".into(), "--group-speaker".into(), "../x".into()]).is_err());
        assert!(Target::from_args(&["--chat".into(), "c".into(), "--own-chat".into()]).is_err());
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_group_turn_in_the_speakers_own_chat_posts_in_the_group_and_leaves_a_line_there() {
        let store = temp_store("mcp-group-own");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let db_path = store.root().join(db::FILE);
        let db = Db::open(&db_path).unwrap();
        crate::group::add_user_message(&db, "thread-1", "@Chef dinner?", 1).unwrap();
        let turn = crate::group::enqueue(&db, "thread-1", "chef", 1, "asked", true, 1).unwrap().unwrap();
        let mut args = args_for(&store, &chat.id, &turn);
        args.extend(own_chat_group_args("chef"));
        args.extend(["--db".into(), db_path.display().to_string()]);
        let target = Target::from_args(&args).unwrap();
        assert!(target.own_chat);
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"send_message","arguments":{"text":"Pasta tonight."}}}"#,
        ]
        .join("\n");
        let replies = run(&target, &input);
        assert!(replies[0]["result"]["tools"][0]["description"].as_str().unwrap().ends_with(OWN_CHAT_LINE));
        assert_eq!(replies[1]["result"]["isError"], false, "{:?}", replies[1]);
        let (messages, _) = crate::group::messages(&db, 10).unwrap();
        let posted = messages.last().unwrap();
        assert_eq!((posted.speaker.as_deref(), posted.turn.as_deref()), (Some("chef"), Some(turn.as_str())));
        // The line in the speaker's chat is of the turn (so the chat's turn counts it as its reply), pointing at the post,
        // with what it said for later turns' context.
        let line = store.messages(&chat.id).unwrap().pop().unwrap();
        assert_eq!(
            (line.role, line.text.as_str(), line.turn.as_deref(), line.in_group.as_deref()),
            (Role::Assistant, "Pasta tonight.", Some(turn.as_str()), Some(posted.id.as_str()))
        );
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    /// Calls send_message with `arguments`; its result.
    fn send(target: &Target, arguments: Value) -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "send_message", "arguments": arguments } });
        run(target, &request.to_string()).remove(0)["result"].clone()
    }

    #[test]
    fn send_message_shows_files_where_they_are_and_refuses_bad_paths() {
        // As in the app: the data folder (`data`, with filo.db) and the agents' chats in agents/ below it.
        let data = temp_store("mcp-files");
        let store = Store::at(data.root().join("agents"));
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("turn-1".into()),
            db: data.root().join(db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let listed = run(&target, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let schema = &listed[0]["result"]["tools"][0]["inputSchema"];
        assert_eq!(
            (schema["properties"]["files"]["type"].as_str(), schema["required"].clone()),
            (Some("array"), json!(["text"]))
        );
        // Told to keep what it sends somewhere lasting.
        assert!(schema["properties"]["files"]["description"].as_str().unwrap().contains("like your workspace"));
        // The agent's files, in its workspace.
        let work = store.root().canonicalize().unwrap().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let (picture, video, report) = (work.join("Chart.PNG"), work.join("clip.mov"), work.join("report.pdf"));
        for (path, bytes) in [(&picture, &b"\x89PNG\r\n\x1a\n"[..]), (&video, b"mov bytes"), (&report, b"pdf bytes")] {
            std::fs::write(path, bytes).unwrap();
        }
        let paths = [&picture, &video, &report].map(|path| path.display().to_string());
        let sent = send(&target, json!({ "text": "Here it is.", "files": paths }));
        assert_eq!(sent["isError"], false, "{sent}");
        assert_eq!(
            sent["content"][0]["text"],
            format!(
                "Sent. The user sees it in the chat. Its 3 files show from where they are, {}: leave them there.",
                paths.join(", ")
            )
        );
        let message = store.messages(&chat.id).unwrap().pop().unwrap();
        assert_eq!((message.role, message.text.as_str()), (Role::Assistant, "Here it is."));
        let shown: Vec<(&str, &str, u64, &Path)> = message
            .attachments
            .iter()
            .map(|file| (file.name.as_str(), file.content_type.as_str(), file.size, file.file.as_path()))
            .collect();
        assert_eq!(
            shown,
            [
                ("Chart.PNG", "image/png", 8, picture.as_path()),
                ("clip.mov", "video/quicktime", 9, video.as_path()),
                ("report.pdf", "application/pdf", 9, report.as_path())
            ]
        );
        // Nothing copied: no storage id, nothing in the data folder's attachments/, and a bookmark to find each by.
        assert!(message.attachments.iter().all(|file| file.storage_id.is_empty() && file.bookmark.is_some()));
        assert!(!data.root().join(crate::attachments::DIR).exists());

        // Files alone; `~/` the home folder; a link or `..` is sent as the file it names.
        let link = work.join("latest.png");
        std::os::unix::fs::symlink(&picture, &link).unwrap();
        let roundabout = work.join("../work/report.pdf");
        let alone = send(
            &target,
            json!({ "text": "", "files": [link.display().to_string(), roundabout.display().to_string()] }),
        );
        assert_eq!(alone["isError"], false, "{alone}");
        let files = store.messages(&chat.id).unwrap().pop().unwrap().attachments;
        assert_eq!(
            files.iter().map(|file| (file.name.as_str(), file.file.as_path())).collect::<Vec<_>>(),
            [("Chart.PNG", picture.as_path()), ("report.pdf", report.as_path())]
        );
        assert!(checked_file("~/").is_err_and(|error| error.contains("folder")));

        // A bad path sends nothing, and says which and why.
        let before = store.messages(&chat.id).unwrap().len();
        let ok = picture.display().to_string();
        let refused = |arguments: Value, says: &str| {
            let result = send(&target, arguments);
            let text = result["content"][0]["text"].as_str().unwrap_or_default().to_string();
            assert!(result["isError"] == true && text.contains(says), "{says}: {result}");
        };
        refused(json!({ "text": "x", "files": [ok, "chart.png"] }), "\"chart.png\" in `files` isn't a full path");
        let missing = work.join("gone.png").display().to_string();
        refused(json!({ "text": "x", "files": [ok, missing] }), &format!("There's no file at {missing}"));
        refused(json!({ "text": "x", "files": [work.display().to_string()] }), "is a folder, not a file");
        refused(json!({ "text": "x", "files": vec![ok.clone(); 6] }), "Up to 5 files per message (6 given)");
        refused(json!({ "text": "x", "files": [ok, 7] }), "each a string");
        refused(json!({ "text": "x", "files": { "path": ok } }), "list of paths");
        refused(json!({ "text": " ", "files": [] }), "needs non-empty `text` (or `files` to send)");
        assert_eq!(store.messages(&chat.id).unwrap().len(), before);
        std::fs::remove_dir_all(data.root()).ok();
    }

    #[test]
    fn a_group_post_carries_its_files_in_the_group_and_the_speakers_own_chat() {
        let store = temp_store("mcp-group-files");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let db_path = store.root().join(db::FILE);
        let db = Db::open(&db_path).unwrap();
        crate::group::add_user_message(&db, "thread-1", "@Chef a photo?", 1).unwrap();
        let turn = crate::group::enqueue(&db, "thread-1", "chef", 1, "asked", true, 1).unwrap().unwrap();
        let mut args = args_for(&store, &chat.id, &turn);
        args.extend(own_chat_group_args("chef"));
        args.extend(["--db".into(), db_path.display().to_string()]);
        let target = Target::from_args(&args).unwrap();
        let photo = store.root().canonicalize().unwrap().join("pasta.jpg");
        std::fs::write(&photo, b"\xFF\xD8\xFF jpg").unwrap();
        let sent = send(&target, json!({ "text": "", "files": [photo.display().to_string()] }));
        assert_eq!(sent["isError"], false, "{sent}");
        assert!(
            sent["content"][0]["text"]
                .as_str()
                .unwrap()
                .ends_with(&format!("Its file shows from where it is, {}: leave it there.", photo.display()))
        );
        let (messages, _) = crate::group::messages(&db, 10).unwrap();
        let posted = messages.last().unwrap();
        assert_eq!((posted.speaker.as_deref(), posted.text.as_str()), (Some("chef"), ""));
        assert_eq!(
            posted.attachments.iter().map(|file| (file.content_type.as_str(), file.file.as_path())).collect::<Vec<_>>(),
            [("image/jpeg", photo.as_path())]
        );
        // Members read where it is; a preview names it.
        assert!(posted.with_files("").contains(&format!("pasta.jpg (image/jpeg), saved at {}", photo.display())));
        assert_eq!(posted.summary(), "pasta.jpg");
        let line = store.messages(&chat.id).unwrap().pop().unwrap();
        assert_eq!((line.in_group.as_deref(), &line.attachments), (Some(posted.id.as_str()), &posted.attachments));
        // A bad path posts nothing.
        let refused = send(&target, json!({ "text": "Another", "files": ["/nowhere/at/all.png"] }));
        assert_eq!(refused["isError"], true);
        assert_eq!(crate::group::messages(&db, 10).unwrap().0.len(), messages.len());
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }
}

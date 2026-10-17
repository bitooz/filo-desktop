// `filo-local`: local chats from a terminal, with the same code the app runs.
//
//   filo-local mcp --chat <id> [--turn <id>] [--dir <root>]   the MCP server (what the CLI starts)
//   filo-local feed-mcp [--db <filo.db>] [--author <name>]      the Feed's tools on their own (feed_mcp.rs)
//   filo-local send [--engine claude|codex] [--chat <id>] [--file <path>…] <text…>   a message (with files) and its
//                                                              turn; prints the replies
//   filo-local list                                            chats, newest first
//   filo-local show <chat id>                                  a chat's messages
//   filo-local glance-document < glance.json                   a glance's page ({title, asOf, html, notes, standalone}
//                                                              on stdin), as glances:get gives it (glance_document.rs)
//
// The store is `FILO_LOCAL_DIR`, else ~/.config/filo/local (the app's). `FILO_LOCAL_HUNG_AFTER=<seconds>`: how long
// `send` waits on a turn that prints nothing before it gives up (turn.rs `HUNG_AFTER`).

use std::time::Duration;

use filo_local::turn::{self, Runtime};
use filo_local::{Engine, Role, Store};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("mcp") => filo_local::mcp::main_from_args(&args[1..]),
        Some("feed-mcp") => filo_local::feed_mcp::main_from_args(&args[1..], filo_local::feed_mcp::default_db()),
        Some("send") => send(&args[1..]),
        Some("list") => list(),
        Some("show") => show(args.get(1)),
        Some("glance-document") => glance_document(),
        _ => {
            eprintln!(
                "usage: filo-local mcp|feed-mcp|send|list|show|glance-document (see the top of src/bin/filo-local.rs)"
            );
            2
        }
    };
    std::process::exit(code);
}

fn glance_document() -> i32 {
    let input: serde_json::Value = match serde_json::from_reader(std::io::stdin().lock()) {
        Ok(input) => input,
        Err(error) => {
            eprintln!("glance-document: stdin isn't a glance as JSON: {error}");
            return 2;
        }
    };
    let text = |key: &str| input.get(key).and_then(serde_json::Value::as_str);
    let glance = filo_local::glance_document::Input {
        title: text("title").unwrap_or_default(),
        as_of: text("asOf"),
        html: text("html").unwrap_or_default(),
        notes: text("notes"),
        standalone: input.get("standalone").and_then(serde_json::Value::as_bool).unwrap_or(false),
    };
    print!("{}", filo_local::glance_document::document(&glance));
    0
}

fn send(args: &[String]) -> i32 {
    let store = Store::open_default();
    let mut engine = None;
    let mut chat_id = None;
    let mut words = Vec::new();
    let mut paths = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--engine" => engine = args.next().and_then(|value| Engine::parse(value)),
            "--chat" => chat_id = args.next().cloned(),
            "--file" => paths.extend(args.next().map(std::path::PathBuf::from)),
            _ => words.push(arg.clone()),
        }
    }
    let text = words.join(" ");
    if text.trim().is_empty() && paths.is_empty() {
        eprintln!("usage: filo-local send [--engine claude|codex] [--chat <id>] [--file <path>…] <text…>");
        return 2;
    }
    // Kept as the app keeps a file of the user's: a clone, never read.
    let mut files = Vec::new();
    for path in paths {
        let content_type = content_type_for(&path);
        let saved = std::fs::metadata(&path).and_then(|meta| {
            let storage_id = filo_local::attachments::keep(store.root(), &path, content_type, true)?;
            Ok((storage_id, meta.len()))
        });
        let (storage_id, size) = match saved {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("couldn't attach {}: {error}", path.display());
                return 1;
            }
        };
        let file = filo_local::attachments::file(store.root(), &storage_id).expect("just saved");
        let name = path.file_name().map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
        files.push(filo_local::store::Attachment {
            storage_id,
            name,
            content_type: content_type.into(),
            size,
            file,
            local_path: Some(path.display().to_string()),
        });
    }
    let chat = match chat_id {
        Some(id) => match store.chat(&id) {
            Ok(Some(chat)) => chat,
            _ => {
                eprintln!("no chat {id}");
                return 2;
            }
        },
        None => match store.create_chat(engine.unwrap_or(Engine::Claude)) {
            Ok(chat) => chat,
            Err(error) => {
                eprintln!("couldn't create a chat in {}: {error}", store.root().display());
                return 1;
            }
        },
    };
    if let Some(engine) = engine.filter(|engine| *engine != chat.engine) {
        let _ = store.update_chat(&chat.id, |chat| chat.engine = engine);
    }
    let turn_id = uuid::Uuid::new_v4().to_string();
    if let Err(error) = store.add_user_message_with(&chat.id, &text, &turn_id, files) {
        eprintln!("couldn't save the message: {error}");
        return 1;
    }
    let runtime = Runtime {
        mcp_program: std::env::current_exe().expect("own path"),
        mcp_args: vec!["mcp".into()],
        path_env: std::env::var("PATH").ok(),
        hung_after: Duration::from_secs(
            std::env::var("FILO_LOCAL_HUNG_AFTER")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(turn::HUNG_AFTER.as_secs()),
        ),
        // The app's database beside the chats: the user's skills (and memories) come along.
        db: Some(store.root().join(filo_local::db::FILE)),
        // A terminal's chats are Filo's own.
        agent: None,
    };
    eprintln!(
        "chat {} · {} · turn {turn_id}",
        chat.id,
        store.chat(&chat.id).ok().flatten().map_or(chat.engine, |chat| chat.engine).label()
    );
    let report = turn::run(&store, &runtime, &chat.id, &turn_id, |_| {});
    for message in store
        .messages(&chat.id)
        .unwrap_or_default()
        .iter()
        .filter(|message| message.turn.as_deref() == Some(turn_id.as_str()) && message.role != Role::User)
    {
        println!("[{}] {}", if message.role == Role::Assistant { "filo" } else { "error" }, message.text);
    }
    eprintln!("replies: {} · stopped: {} · error: {}", report.replies, report.stopped, report.error.is_some());
    if report.error.is_some() || report.replies == 0 { 1 } else { 0 }
}

/// A file's content type by its extension, as the app's composer tells it.
fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()).unwrap_or_default().to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn list() -> i32 {
    let store = Store::open_default();
    for row in store.list_chats() {
        println!(
            "{}  {:<11}  {}  {}",
            row.chat.id,
            row.chat.engine.label(),
            if row.chat.title.is_empty() { "New chat" } else { &row.chat.title },
            row.preview.unwrap_or_default()
        );
    }
    0
}

fn show(chat_id: Option<&String>) -> i32 {
    let Some(chat_id) = chat_id else {
        eprintln!("usage: filo-local show <chat id>");
        return 2;
    };
    match Store::open_default().messages(chat_id) {
        Ok(messages) => {
            for message in messages {
                println!("[{:?}] {}", message.role, message.text);
            }
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

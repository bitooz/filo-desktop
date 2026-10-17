// Replaying real chats' idle gaps against the reseed (reseed.rs): for each case (a chat, and the message the user sent
// after it sat idle), a copy of the chat up to that message goes in a folder of its own, with the first message a
// fresh session would get there (turn.rs `prompt_for`, the time it was sent in front, as a turn has it) and the
// instructions it would run with (the chat's own, with Filo's capabilities and the history line). The copy is laid out
// as Filo's own folder is (`agents/chats`), with every other agent's chat as it was at that moment, and the history line
// lists the agents (read from the source folder's filo.db, only read) with their chats in the copy. A driver then runs the engine on them without Filo's
// tools, to see what it would have said and done. Only reads the chats.
//
// `FILO_REPLAY_CASES=cases.json FILO_REPLAY_DIR=/tmp/replay cargo test -p filo-local --test reseed_replay -- --ignored`,
// cases being `[{ "case": "c01", "chat": "<chat folder>", "index": 9, "engine": "codex" }, …]`; each case's folder gets
// `prompt.txt`, `instructions.txt` and `case.json` (with the workspace and model it runs in).

use std::path::PathBuf;

use chrono::TimeZone as _;
use filo_local::store::Session;
use filo_local::{Engine, Message, Store, engine, turn};

#[test]
#[ignore]
fn replay_seeds_for_idle_gaps() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(std::env::var("FILO_REPLAY_CASES").expect("FILO_REPLAY_CASES")).unwrap())
            .unwrap();
    let out = PathBuf::from(std::env::var("FILO_REPLAY_DIR").expect("FILO_REPLAY_DIR"));
    for case in &cases {
        let name = case["case"].as_str().unwrap();
        let from = PathBuf::from(case["chat"].as_str().unwrap());
        let index = case["index"].as_u64().unwrap() as usize;
        let engine = Engine::parse(case["engine"].as_str().unwrap()).unwrap();
        let local = out.join(name);
        let root = local.join("agents");
        let chat_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(from.join("chat.json")).unwrap()).unwrap();
        let id = chat_json["id"].as_str().unwrap().to_string();
        let dir = root.join("chats").join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(from.join("chat.json"), dir.join("chat.json")).unwrap();
        // The chat as it was when the message came: up to it.
        let lines: Vec<String> = std::fs::read_to_string(from.join("messages.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect();
        std::fs::write(dir.join("messages.jsonl"), lines[..=index].join("\n") + "\n").unwrap();
        // The other agents' chats as they were then, and the list of agents with their chats in the copy.
        let then = serde_json::from_str::<serde_json::Value>(&lines[index]).unwrap()["at"].as_u64().unwrap();
        let chats = from.parent().unwrap();
        let source = chats
            .parent()
            .and_then(|agents| agents.parent())
            .map(|from_local| from_local.join(filo_local::db::FILE))
            .filter(|file| file.is_file());
        let others: Vec<(String, PathBuf)> = source
            .map(|db| {
                let db = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                let mut statement = db
                    .prepare("SELECT name, session_id FROM agents WHERE session_id IS NOT NULL ORDER BY created_at")
                    .unwrap();
                let rows =
                    statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap();
                rows.flatten()
                    .map(|(name, chat)| (name, root.join("chats").join(chat).join("messages.jsonl")))
                    .collect()
            })
            .unwrap_or_default();
        for other in std::fs::read_dir(chats)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|other| *other != from && other.join("chat.json").is_file())
        {
            let copy = root.join("chats").join(other.file_name().unwrap());
            std::fs::create_dir_all(&copy).unwrap();
            std::fs::copy(other.join("chat.json"), copy.join("chat.json")).unwrap();
            let text = std::fs::read_to_string(other.join("messages.jsonl")).unwrap_or_default();
            let kept: Vec<&str> = text
                .lines()
                .filter(|line| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .ok()
                        .and_then(|message| message["at"].as_u64())
                        .is_some_and(|at| at <= then)
                })
                .collect();
            std::fs::write(copy.join("messages.jsonl"), kept.join("\n") + "\n").unwrap();
        }
        let store = Store::at(root.clone());
        let chat = store.chat(&id).unwrap().unwrap();
        store
            .update_chat(&id, |chat| {
                chat.session = Some(Session { engine, id: "cold".into(), instructions: None, model: None })
            })
            .unwrap();
        let messages: Vec<Message> = store.messages(&id).unwrap();
        let target = &messages[index];
        let record = store.messages_file(&id).unwrap();
        let prompt = turn::prompt_for(&messages, target.turn.as_deref().unwrap(), false, Some(&record));
        let at = chrono::Local.timestamp_millis_opt(target.at as i64).unwrap();
        let prompt =
            format!("It's now {}, the user's local time.\n\n{prompt}", at.format("%a %-d %b %Y, %H:%M (UTC%:z)"));
        let instructions = format!(
            "{} {}",
            engine::with_capabilities(
                chat.instructions.as_deref().unwrap_or(engine::INSTRUCTIONS),
                engine,
                "auto",
                &[],
                None,
                Default::default()
            ),
            turn::history_line(engine, &record, &others)
        );
        std::fs::write(local.join("prompt.txt"), &prompt).unwrap();
        std::fs::write(local.join("instructions.txt"), &instructions).unwrap();
        let model = chat.model.clone().filter(|_| chat.engine == engine);
        let meta = serde_json::json!({
            "case": name, "chat": chat.title, "engine": engine.label(), "index": index, "model": model,
            "workspace": store.workspace().unwrap(), "seedShown": prompt.matches(" · message ").count(), "seedTokens": filo_local::reseed::tokens(&prompt),
        });
        std::fs::write(local.join("case.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
        println!(
            "{name}: {} message {index}, seed {} messages, ~{}k tokens",
            chat.title,
            meta["seedShown"],
            meta["seedTokens"].as_u64().unwrap() / 1000
        );
    }
}

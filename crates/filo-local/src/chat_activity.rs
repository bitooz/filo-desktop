// What a chat turn is doing while it answers, for the line beside the chat's typing dots: "Running a command" while
// the agent's shell runs one (Claude Code's Bash, Codex's command execution), a fixed label, never the command itself;
// and "Compacting the conversation" while the CLI summarizes its session to make room, which can take a minute (a
// session that went cold isn't compacted first: the turn starts a fresh one, turn.rs). The turn runs in a worker
// process (turn.rs), so the label goes through a file beside the chat's messages, `chats/<id>/activity.json`
// ({ turn, text }), which the app reads while the turn runs and its watcher checks for changes (local_backend.rs). The turn clears it when it ends; one left behind by a turn that was
// killed doesn't show, since only a running turn's activity does.
//
// One of Filo's own tools says what it's doing in a file of its own, `activity-tool.json` ("Codex is drawing 3
// pictures" while generate_images runs, characters_mcp.rs), written by the turn's MCP server: it shows over the CLI's
// label while it's there, and the CLI's events, which don't see it, can't clear it.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::Engine;
use crate::store::Store;

pub const RUNNING_A_COMMAND: &str = "Running a command";
const COMPACTING: &str = "Compacting the conversation";
const FILE: &str = "activity.json";
/// What one of Filo's tools is doing (`set_tool`).
const TOOL_FILE: &str = "activity-tool.json";

/// generate_images' line while Codex draws (characters_mcp.rs).
pub fn drawing(count: usize) -> String {
    if count == 1 { "Codex is drawing a picture".into() } else { format!("Codex is drawing {count} pictures") }
}

/// Follows a CLI's JSON events for the shell commands running now, and its compactions.
pub struct Tracker {
    engine: Engine,
    running: HashSet<String>,
    compacting: bool,
    compacted: bool,
}

impl Tracker {
    pub fn new(engine: Engine) -> Tracker {
        Tracker { engine, running: HashSet::new(), compacting: false, compacted: false }
    }

    /// What shows now.
    pub fn label(&self) -> Option<&'static str> {
        if self.compacting { Some(COMPACTING) } else { (!self.running.is_empty()).then_some(RUNNING_A_COMMAND) }
    }

    /// Whether the CLI compacted its session during the turn.
    pub fn compacted(&self) -> bool {
        self.compacted
    }

    /// Reads one line of the CLI's output; the label when it changed with it.
    pub fn line(&mut self, line: &str) -> Option<Option<&'static str>> {
        let Ok(event) = serde_json::from_str::<Value>(line) else { return None };
        let before = self.label();
        let id = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_string);
        match self.engine {
            Engine::Claude => {
                let kind = event.get("type").and_then(Value::as_str);
                let content = event.pointer("/message/content").and_then(Value::as_array).cloned().unwrap_or_default();
                for block in &content {
                    match (kind, block.get("type").and_then(Value::as_str)) {
                        (Some("assistant"), Some("tool_use"))
                            if block.get("name").and_then(Value::as_str) == Some("Bash") =>
                        {
                            self.running.extend(id(block.get("id")));
                        }
                        (Some("user"), Some("tool_result")) => {
                            if let Some(id) = id(block.get("tool_use_id")) {
                                self.running.remove(&id);
                            }
                        }
                        _ => {}
                    }
                }
                // Claude Code says it's compacting, then where the summary starts (`compact_boundary`).
                if kind == Some("system") {
                    match (event.get("subtype").and_then(Value::as_str), event.get("status").and_then(Value::as_str)) {
                        (Some("status"), Some("compacting")) => self.compacting = true,
                        (Some("compact_boundary"), _) => (self.compacting, self.compacted) = (false, true),
                        _ => {}
                    }
                }
                // The turn's over: nothing runs past it.
                if kind == Some("result") {
                    self.running.clear();
                    self.compacting = false;
                }
            }
            // Its app-server's notifications (coding_session.rs).
            Engine::Codex => {
                let method = event.get("method").and_then(Value::as_str);
                let item = event.pointer("/params/item").unwrap_or(&Value::Null);
                let started = match method {
                    Some("item/started") => Some(true),
                    Some("item/completed") => Some(false),
                    _ => None,
                };
                match (item.get("type").and_then(Value::as_str), started) {
                    (Some("commandExecution"), Some(true)) => {
                        self.running.insert(id(item.get("id")).unwrap_or_default());
                    }
                    (Some("commandExecution"), Some(false)) => {
                        self.running.remove(&id(item.get("id")).unwrap_or_default());
                    }
                    (Some("contextCompaction"), Some(true)) => self.compacting = true,
                    (Some("contextCompaction"), Some(false)) => (self.compacting, self.compacted) = (false, true),
                    _ => {}
                }
                if method == Some("turn/completed") {
                    self.running.clear();
                    self.compacting = false;
                }
            }
            // A session's steps as `start_turn` hands them on: a command runs until it does or says anything else.
            Engine::Muse | Engine::OpenCode | Engine::Pi => {
                self.running.clear();
                if event.get("filo_step").and_then(Value::as_str) == Some("command") {
                    self.running.insert("command".into());
                }
            }
        }
        let after = self.label();
        (after != before).then_some(after)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Saved {
    turn: String,
    text: String,
}

fn file(store: &Store, chat_id: &str, name: &str) -> Option<PathBuf> {
    store.chat_dir(chat_id).ok().map(|dir| dir.join(name))
}

/// Saves what `turn_id` is doing now (`None`: nothing to show). A write that fails only loses the line.
pub fn set(store: &Store, chat_id: &str, turn_id: &str, text: Option<&str>) {
    write(file(store, chat_id, FILE), turn_id, text);
}

/// Saves what one of Filo's tools is doing for `turn_id` now (`None`: it's done). It shows over `set`'s line.
pub fn set_tool(store: &Store, chat_id: &str, turn_id: &str, text: Option<&str>) {
    write(file(store, chat_id, TOOL_FILE), turn_id, text);
}

fn write(path: Option<PathBuf>, turn_id: &str, text: Option<&str>) {
    let Some(path) = path else { return };
    match text {
        Some(text) => {
            let Ok(bytes) = serde_json::to_vec(&Saved { turn: turn_id.into(), text: text.into() }) else { return };
            // Whole or not at all: the app may read it at any moment.
            let _ = crate::files::write_atomic(&path, &bytes);
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// What `turn_id` is doing now, as its turn (or one of Filo's tools in it) last saved it.
pub fn get(store: &Store, chat_id: &str, turn_id: &str) -> Option<String> {
    let read = |name: &str| -> Option<String> {
        let saved: Saved = serde_json::from_slice(&std::fs::read(file(store, chat_id, name)?).ok()?).ok()?;
        (saved.turn == turn_id).then_some(saved.text)
    };
    read(TOOL_FILE).or_else(|| read(FILE))
}

/// The chat's activity files as they are now, to notice them change (their contents: they're small).
pub fn version(store: &Store, chat_id: &str) -> Option<Vec<u8>> {
    let read = |name: &str| file(store, chat_id, name).and_then(|path| std::fs::read(path).ok());
    match (read(FILE), read(TOOL_FILE)) {
        (None, None) => None,
        (turn, tool) => Some([turn.unwrap_or_default(), b"\n".to_vec(), tool.unwrap_or_default()].concat()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;

    #[test]
    fn a_compaction_shows_while_it_runs_and_is_remembered() {
        let mut tracker = Tracker::new(Engine::Claude);
        assert!(!tracker.compacted());
        assert_eq!(
            tracker.line(r#"{"type":"system","subtype":"status","status":"compacting"}"#),
            Some(Some(COMPACTING))
        );
        // It wins over a command while it lasts.
        assert_eq!(tracker.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#), None);
        assert_eq!(tracker.line(r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"auto","pre_tokens":900000}}"#), Some(Some(RUNNING_A_COMMAND)));
        assert!(tracker.compacted());

        let mut codex = Tracker::new(Engine::Codex);
        assert_eq!(
            codex.line(r#"{"method":"item/started","params":{"item":{"id":"c1","type":"contextCompaction"}}}"#),
            Some(Some(COMPACTING))
        );
        assert_eq!(
            codex.line(r#"{"method":"item/completed","params":{"item":{"id":"c1","type":"contextCompaction"}}}"#),
            Some(None)
        );
        assert!(codex.compacted());
    }

    /// Muse Code's and OpenCode's turns hand on their steps as `{"filo_step": …}` (engine.rs `start_turn`): a command shows
    /// until they do or say anything else.
    #[test]
    fn a_sessions_command_shows_until_its_next_step() {
        for engine in [Engine::Muse, Engine::OpenCode] {
            let mut tracker = Tracker::new(engine);
            assert_eq!(tracker.line(r#"{"filo_step":"command"}"#), Some(Some(RUNNING_A_COMMAND)));
            assert_eq!(tracker.line(r#"{"filo_step":"command"}"#), None);
            assert_eq!(tracker.line(r#"{"filo_step":"tool"}"#), Some(None));
            assert_eq!(tracker.line(r#"{"filo_step":"tool"}"#), None);
        }
    }

    #[test]
    fn claude_codes_bash_shows_while_it_runs() {
        let mut tracker = Tracker::new(Engine::Claude);
        assert_eq!(tracker.line(r#"{"type":"system","subtype":"init","session_id":"s"}"#), None);
        // Reading a file isn't a command.
        assert_eq!(tracker.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"/a"}}]}}"#), None);
        let started = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls ~/secret"}}]}}"#;
        assert_eq!(tracker.line(started), Some(Some(RUNNING_A_COMMAND)));
        // Another command at once: still the same line.
        assert_eq!(tracker.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b2","name":"Bash","input":{"command":"date"}}]}}"#), None);
        assert_eq!(
            tracker.line(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"x"}]}}"#
            ),
            None
        );
        assert_eq!(
            tracker.line(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b2","content":"y"}]}}"#
            ),
            Some(None)
        );
        // The turn ending clears one whose result never came.
        tracker.line(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b3","name":"Bash","input":{"command":"sleep 9"}}]}}"#);
        assert_eq!(
            tracker.line(r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#),
            Some(None)
        );
    }

    #[test]
    fn codexs_commands_show_while_they_run() {
        let mut tracker = Tracker::new(Engine::Codex);
        let started = r#"{"method":"item/started","params":{"item":{"id":"call_1","type":"commandExecution","command":"/bin/zsh -lc 'curl -s example.com'","status":"inProgress"}}}"#;
        assert_eq!(tracker.line(started), Some(Some(RUNNING_A_COMMAND)));
        assert_eq!(tracker.line(r#"{"method":"item/completed","params":{"item":{"id":"call_2","type":"mcpToolCall","server":"filo_core","tool":"send_message"}}}"#), None);
        assert_eq!(tracker.line(r#"{"method":"item/completed","params":{"item":{"id":"call_1","type":"commandExecution","exitCode":0,"status":"completed"}}}"#), Some(None));
        tracker.line(started);
        assert_eq!(
            tracker.line(r#"{"method":"turn/completed","params":{"turn":{"id":"t","status":"failed"}}}"#),
            Some(None)
        );
        assert_eq!(tracker.line("not json"), None);
    }

    #[test]
    fn the_file_holds_one_turns_label_and_never_the_command() {
        let store = temp_store("chat-activity");
        let chat = store.create_chat(Engine::Claude).unwrap();
        assert_eq!(get(&store, &chat.id, "t1"), None);
        set(&store, &chat.id, "t1", Some(RUNNING_A_COMMAND));
        assert_eq!(get(&store, &chat.id, "t1").as_deref(), Some(RUNNING_A_COMMAND));
        // Another turn's (one killed before it cleared it) doesn't show.
        assert_eq!(get(&store, &chat.id, "t2"), None);
        let before = version(&store, &chat.id);
        set(&store, &chat.id, "t1", None);
        assert_eq!((get(&store, &chat.id, "t1"), version(&store, &chat.id)), (None, None));
        assert!(before.is_some());
        // Not a chat id: nothing written anywhere.
        set(&store, "../x", "t1", Some(RUNNING_A_COMMAND));
        assert_eq!(get(&store, "../x", "t1"), None);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn a_tools_line_shows_over_the_turns_until_it_is_done() {
        let store = temp_store("chat-activity-tool");
        let chat = store.create_chat(Engine::Claude).unwrap();
        set(&store, &chat.id, "t1", Some(RUNNING_A_COMMAND));
        let before = version(&store, &chat.id);
        set_tool(&store, &chat.id, "t1", Some(&drawing(3)));
        assert_eq!(get(&store, &chat.id, "t1").as_deref(), Some("Codex is drawing 3 pictures"));
        assert_ne!(version(&store, &chat.id), before);
        // The CLI's events clearing their own line don't take it away.
        set(&store, &chat.id, "t1", None);
        assert_eq!(get(&store, &chat.id, "t1").as_deref(), Some("Codex is drawing 3 pictures"));
        assert!(version(&store, &chat.id).is_some());
        // Another turn's doesn't show.
        assert_eq!(get(&store, &chat.id, "t2"), None);
        set(&store, &chat.id, "t1", Some(RUNNING_A_COMMAND));
        set_tool(&store, &chat.id, "t1", None);
        assert_eq!(get(&store, &chat.id, "t1").as_deref(), Some(RUNNING_A_COMMAND));
        set(&store, &chat.id, "t1", None);
        assert_eq!(version(&store, &chat.id), None);
        assert_eq!(drawing(1), "Codex is drawing a picture");
        std::fs::remove_dir_all(store.root()).ok();
    }
}

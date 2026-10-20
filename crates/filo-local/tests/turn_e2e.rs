// A whole turn with a stand-in `claude`: it reads its --mcp-config, starts the real MCP server program (this crate's
// `filo-local mcp`) the way Claude Code would, calls send_message over stdio, and the reply lands in the chat. With a
// skill saved, it says which skills it may see, what its SKILL.md holds and what it was given. Asked to run a command, it reports a Bash call
// that takes a moment, and the chat says "Running a command" meanwhile. Asked to share, it says whether it has the
// Artifact tool and, when it does, publishes the page it was given. Asked to start a server, it starts one detached (as
// the instructions say), leaves a task in the background and, like Claude Code, goes on while its stdin is open. Told to
// work until told otherwise, it takes a message sent meanwhile (with a stand-in Codex app-server too), and the reply
// answers that message; one it doesn't take before its turn ends waits for it, and resumes the session it left. Told to
// keep working, it's busy for longer than the turn's `hung_after`, saying what it
// does all along; told to hang, it says nothing at all. Asked how hard it thinks, it says its --effort and whether a
// shared note came with the message.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use filo_local::db::{self, Db};
use filo_local::store::{CHECK_IN, RoutineRun};
use filo_local::turn::{self, Report, Runtime};
use filo_local::{Engine, Role, Store, chat_activity, engine, skills};

const FAKE_CLAUDE: &str = r##"#!/usr/bin/env python3
import json, os, subprocess, sys, time
args = sys.argv[1:]
config = json.loads(args[args.index("--mcp-config") + 1])["mcpServers"]["filo"]
# Claude Code reads its stream-json input as it comes, and stdin stays open while the turn runs: read up to the message.
lines = []
for line in iter(sys.stdin.readline, ""):
    if line.strip():
        lines.append(json.loads(line))
        if lines[-1]["type"] == "user":
            break
init = [line for line in lines if line["type"] == "control_request"][0]["request"]
prompt = [line for line in lines if line["type"] == "user"][0]["message"]["content"][0]["text"]
server = subprocess.Popen([config["command"]] + config["args"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def call(id, method, params):
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    server.stdin.flush()
    return json.loads(server.stdout.readline())
call(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}})
session = args[args.index("--session-id") + 1] if "--session-id" in args else args[args.index("--resume") + 1]
# Like Claude Code, its turn starts by saying so.
print(json.dumps({"type": "system", "subtype": "init", "session_id": session}), flush=True)
waits = False
if "use the skill" in prompt:
    system = args[args.index("--append-system-prompt") + 1]
    tools = [tool["name"] for tool in call(3, "tools/list", {})["result"]["tools"]]
    skill = open(".claude/skills/weekly-report/SKILL.md").read()
    seen = {"tools": tools, "skill": skill, "skills": init["skills"], "listed": "weekly-report" in system,
            "attached": "# Skill: weekly-report" in prompt, "script": os.path.isfile(".claude/skills/web-demo/scripts/record.sh")}
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": json.dumps(seen)}})
elif "install the skill at " in prompt:
    # As an agent would with its shell: copy the folder where its instructions say skills go.
    system = args[args.index("--append-system-prompt") + 1]
    folder = system.split("are folders in ")[1].split(", one per skill")[0]
    source = prompt.splitlines()[-1].split("install the skill at ")[1].strip()
    subprocess.run(["cp", "-R", source, folder + "/"], check=True)
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "installed in " + folder}})
elif "run a command" in prompt:
    print(json.dumps({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "ls ~/private"}}]}}), flush=True)
    time.sleep(1.5)
    print(json.dumps({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "b1", "content": "a"}]}}), flush=True)
    print(json.dumps({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "b2", "name": "Bash", "input": {"command": "sleep 60"}}]}}), flush=True)
    time.sleep(0.5)
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "ran it"}})
elif "start a server on port " in prompt:
    port = prompt.splitlines()[-1].split("start a server on port ")[1]
    started = subprocess.run(["sh", "-c", "nohup python3 -m http.server %s --bind 127.0.0.1 > server.log 2>&1 < /dev/null & echo $!" % port], capture_output=True, text=True)
    print(json.dumps({"type": "system", "subtype": "background_tasks_changed", "tasks": [{"task_id": "b1", "task_type": "local_bash", "description": "Watch the server log"}]}), flush=True)
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "serving on port %s, pid %s" % (port, started.stdout.strip())}})
    waits = True
elif "share it: " in prompt:
    system = args[args.index("--append-system-prompt") + 1]
    allowed = [arg for arg in args if arg.startswith("--allowedTools=")][0].split("=", 1)[1].split(",")
    seen = {"tools": "Artifact" in args[args.index("--tools") + 1].split(","), "allowed": "Artifact" in allowed,
            "env": os.environ.get("CLAUDE_CODE_ARTIFACT"), "told": "Artifact tool" in system}
    if seen["tools"]:
        page = prompt.splitlines()[-1].split("share it: ")[1]
        print(json.dumps({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "a1", "name": "Artifact", "input": {"file_path": page}}]}}), flush=True)
        print(json.dumps({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "a1", "content": "Published " + page + " at https://claude.ai/artifact/abc123 (Version 1)"}]}}), flush=True)
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": json.dumps(seen)}})
elif "how hard" in prompt:
    effort = args[args.index("--effort") + 1] if "--effort" in args else None
    noted = "Material that comes with this message" in prompt and "Title: Groceries" in prompt
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": json.dumps({"effort": effort, "noted": noted})}})
elif "what did you do" in prompt:
    resumed = args[args.index("--resume") + 1] if "--resume" in args else "nothing"
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "resumed " + resumed}})
elif prompt.splitlines()[-1] == "keep working":
    for step in range(20):
        print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "step %d" % step}]}}), flush=True)
        time.sleep(0.3)
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "worked"}})
elif prompt.splitlines()[-1] == "hang":
    time.sleep(60)
elif prompt.splitlines()[-1] in ("work until I say", "work a moment until I say", "work until I say, then fail"):
    # Busy, a step at a time, until a message comes on stdin: like Claude Code, it replays it as it takes it (with
    # --replay-user-messages) and acts on it from its next step. Told to, it fails once it has.
    import queue, threading
    incoming = queue.Queue()
    threading.Thread(target=lambda: [incoming.put(line) for line in iter(sys.stdin.readline, "")], daemon=True).start()
    heard = None
    for step in range(10 if "a moment" in prompt else 100):
        print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "step %d" % step}]}}), flush=True)
        try:
            note = json.loads(incoming.get(timeout=0.2))
        except queue.Empty:
            continue
        if note.get("type") == "user":
            if "--replay-user-messages" in args:
                print(json.dumps({"type": "user", "message": note["message"], "isReplay": True}), flush=True)
            heard = note["message"]["content"]
            print(json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "on it"}]}}), flush=True)
            break
    time.sleep(0.5)
    if prompt.splitlines()[-1].endswith("then fail"):
        print(json.dumps({"type": "result", "subtype": "error_during_execution", "is_error": True, "result": "API Error: overloaded", "session_id": session}), flush=True)
        sys.exit(1)
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "heard: " + (heard.splitlines()[-1] if heard else "nothing")}})
elif "silent" not in prompt:
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": "echo: " + prompt.splitlines()[-1]}})
server.stdin.close()
server.wait()
print(json.dumps({"type": "system", "subtype": "init", "session_id": session}))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "plain text", "session_id": session}), flush=True)
if waits:
    # With work in the background, Claude Code goes on while its stdin is open, and exits at once when it's closed.
    import select
    if not select.select([sys.stdin], [], [], 20)[0]:
        print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "still waiting", "session_id": session}))
"##;

/// The stand-in `claude` in the store's `bin`, and a runtime that finds it and uses the store's database.
fn stand_in(store: &Store) -> (Runtime, String) {
    let bin = store.root().join("bin");
    common::stand_in(&bin, "claude", FAKE_CLAUDE);

    let path_env = common::path_with(&bin);
    let runtime = common::runtime(Some(path_env.clone()), Some(store.root().join(db::FILE)), Duration::from_secs(30));
    (runtime, path_env)
}

#[test]
fn a_turn_with_a_stand_in_cli_and_the_real_mcp_server() {
    let store = Store::at(common::temp_root("e2e"));
    let (runtime, path_env) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "hello", "turn-1").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-1", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    let session = store.chat(&chat.id).unwrap().unwrap().session.expect("session kept");
    assert_eq!(session.engine, Engine::Claude);

    // The next turn resumes that session; this one never calls send_message.
    store.add_user_message(&chat.id, "silent please", "turn-2").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-2", |_| {});
    assert_eq!(report.replies, 0);
    assert!(report.error.as_deref().unwrap().contains("without sending a message"), "{report:?}");
    assert_eq!(store.chat(&chat.id).unwrap().unwrap().session.unwrap().id, session.id);

    // A routine's run that the routine told to stay quiet with nothing to report ends the same way, and that's fine.
    let routine = RoutineRun { title: "Check PRs".into(), when: "every 30 minutes".into() };
    store
        .add_routine_message(
            &chat.id,
            "⏰ Routine “Check PRs” (every 30 minutes). Finish silent when nothing changed.",
            "turn-routine",
            routine.clone(),
        )
        .unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-routine", |_| {});
    assert_eq!(report, Report { replies: 0, stopped: false, error: None, ..Report::default() });

    // What a restarted app reads back.
    let reopened = Store::at(store.root());
    let messages = reopened.messages(&chat.id).unwrap();
    let shape: Vec<(Role, &str)> = messages.iter().map(|m| (m.role, m.text.lines().next().unwrap_or(""))).collect();
    assert_eq!(shape[..3], [(Role::User, "hello"), (Role::Assistant, "echo: hello"), (Role::User, "silent please")]);
    // The reply sent through the MCP server answers the message its turn was for.
    assert_eq!(messages[1].reply_to.as_deref(), Some(messages[0].id.as_str()));
    assert_eq!(shape[3].0, Role::Error);
    assert_eq!(messages[4].routine, Some(routine));
    assert_eq!(messages.len(), 5, "no error after the quiet routine run");
    assert_eq!(reopened.list_chats()[0].chat.title, "hello");

    // Codex picked but not installed (only checked where no `codex` is in a usual place).
    store.update_chat(&chat.id, |chat| chat.engine = Engine::Codex).unwrap();
    if engine::find_program(Engine::Codex, Some(&path_env)).is_none() {
        store.add_user_message(&chat.id, "hi codex", "turn-3").unwrap();
        let report = turn::run(&store, &runtime, &chat.id, "turn-3", |_| {});
        assert!(report.error.unwrap().contains("isn't installed"));
    }
    std::fs::remove_dir_all(store.root()).ok();
}

/// An agent's own check-in may end without a message when there's nothing new, run again with Retry too, and one saved
/// before check-ins had their mark (only its words) the same; one with something to say still says it.
#[test]
fn a_check_in_may_end_quietly_and_one_that_sends_still_does() {
    let store = Store::at(common::temp_root("check-in"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    let quiet = Report { replies: 0, stopped: false, error: None, ..Report::default() };
    let check_in = format!("{CHECK_IN}Anything new on the render? (stay silent if not)");
    let saved = store.add_check_in_message(&chat.id, &check_in, "turn-quiet").unwrap();
    assert!(saved.check_in && saved.from_app());
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-quiet", |_| {}), quiet);
    // Retry runs the same turn again.
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-quiet", |_| {}), quiet);
    store.add_user_message(&chat.id, &check_in, "turn-old").unwrap();
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-old", |_| {}), quiet);
    store.add_check_in_message(&chat.id, &format!("{CHECK_IN}Did the export finish?"), "turn-sends").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-sends", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });

    let messages = Store::at(store.root()).messages(&chat.id).unwrap();
    assert!(messages.iter().all(|message| message.role != Role::Error), "{messages:?}");
    let replies: Vec<(&str, Option<&str>)> = messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| (message.text.as_str(), message.turn.as_deref()))
        .collect();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].1, Some("turn-sends"));
    // The check-in keeps its mark on disk, and the chat still has its line.
    assert!(messages[0].check_in && messages[0].text == check_in);
    std::fs::remove_dir_all(store.root()).ok();
}

#[test]
fn the_chat_says_running_a_command_while_the_agent_runs_one() {
    let store = Store::at(common::temp_root("activity"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "please run a command", "turn-1").unwrap();
    let running = {
        let (store, chat_id) = (store.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-1", |_| {}))
    };
    // While the command runs: the fixed label, never the command.
    let began = std::time::Instant::now();
    let mut seen = None;
    while seen.is_none() && began.elapsed() < Duration::from_secs(20) {
        seen = chat_activity::get(&store, &chat.id, "turn-1");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(seen.as_deref(), Some("Running a command"));
    let saved = std::fs::read_to_string(store.root().join("chats").join(&chat.id).join("activity.json")).unwrap();
    assert!(!saved.contains("private"), "{saved}");
    // A command still running when the turn ends (the stand-in never says the second one finished) clears with it.
    let report = running.join().unwrap();
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    assert_eq!(chat_activity::get(&store, &chat.id, "turn-1"), None);
    assert!(!store.root().join("chats").join(&chat.id).join("activity.json").exists());
    std::fs::remove_dir_all(store.root()).ok();
}

/// A message the user sends while a turn works goes into it, on Claude Code (a user message on its stdin), Codex and
/// Muse Code (a `turn/steer`): its CLI acts on it at its next step, the reply answers that message, and the message's own
/// turn ends without running, with nothing to say. A Muse Code that refuses the steer has its turn interrupted, and the
/// next turn in the same session starts with the message, with the same outcome.
#[test]
fn a_message_sent_mid_turn_goes_into_it_and_the_reply_answers_it() {
    for (engine, refuse) in
        [(Engine::Claude, false), (Engine::Codex, false), (Engine::Muse, false), (Engine::Muse, true)]
    {
        let store = Store::at(common::temp_root(&format!("steer-{}", engine.program())));
        let (runtime, _) = stand_in(&store);
        let bin = store.root().join("bin");
        common::stand_in(&bin, "codex", FAKE_CODEX);
        common::stand_in(&bin, "muse", FAKE_MUSE);
        if refuse {
            std::fs::write(bin.join("muse.refuse-steer"), "").unwrap();
        }
        let chat = store.create_chat(engine).unwrap();
        store.add_user_message(&chat.id, "work until I say", "turn-1").unwrap();
        let (started, starts) = std::sync::mpsc::channel();
        let first = {
            let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
            std::thread::spawn(move || {
                turn::run(&store, &runtime, &chat_id, "turn-1", move |_| started.send(()).unwrap())
            })
        };
        // Its Stopper for the wait, then its CLI's: it holds the chat.
        starts.recv().unwrap();
        starts.recv().unwrap();
        let steered = store.add_user_message(&chat.id, "Don't send without my approval", "turn-2").unwrap();
        let second = {
            let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
            std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-2", |_| {}))
        };
        assert_eq!(second.join().unwrap(), Report::default(), "{engine:?}");
        // The message's turn ends once the turn it went into has: had that one failed, it would have run after all.
        assert!(first.is_finished(), "{engine:?}");
        assert_eq!(
            first.join().unwrap(),
            Report { replies: 1, stopped: false, error: None, ..Report::default() },
            "{engine:?}"
        );
        let said: Vec<(Role, Option<String>, String, Option<String>)> = store
            .messages(&chat.id)
            .unwrap()
            .into_iter()
            .filter(|message| message.role != Role::User)
            .map(|message| (message.role, message.turn, message.text, message.reply_to))
            .collect();
        assert_eq!(
            said,
            [(
                Role::Assistant,
                Some("turn-1".into()),
                "heard: Don't send without my approval".into(),
                Some(steered.id.clone())
            )],
            "{engine:?}"
        );
        // Its claim went with its turn: none is left behind.
        assert!(filo_local::steer::all(&store, &chat.id).is_empty(), "{engine:?}");
        if engine == Engine::Muse {
            // Interrupted only when it refused, then carried on in the same session.
            let heard = std::fs::read_to_string(bin.join("muse.heard")).unwrap();
            assert_eq!(heard.contains("turn/interrupt"), refuse, "{heard}");
            assert_eq!(heard.matches("\"method\":\"session/").count(), 1, "{heard}");
        }
        std::fs::remove_dir_all(store.root()).ok();
    }
}

/// A message that went into a turn that then failed isn't lost: its own turn runs after all, in the session the failed
/// one left, and answers it, and the failed turn keeps its error for a retry of its own message.
#[test]
fn a_message_taken_into_a_turn_that_fails_gets_its_own_turn() {
    let store = Store::at(common::temp_root("steer-failed"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "work until I say, then fail", "turn-1").unwrap();
    let (started, starts) = std::sync::mpsc::channel();
    let first = {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-1", move |_| started.send(()).unwrap()))
    };
    starts.recv().unwrap();
    starts.recv().unwrap();
    let steered = store.add_user_message(&chat.id, "Don't send without my approval", "turn-2").unwrap();
    let second = turn::run(&store, &runtime, &chat.id, "turn-2", |_| {});
    let first = first.join().unwrap();
    assert!(first.error.as_deref().is_some_and(|error| error.contains("overloaded")), "{first:?}");
    assert_eq!(second, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    let said: Vec<(Role, Option<String>, Option<String>)> = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role != Role::User)
        .map(|message| (message.role, message.turn, message.reply_to))
        .collect();
    assert_eq!(
        said,
        [
            (Role::Error, Some("turn-1".into()), None),
            (Role::Assistant, Some("turn-2".into()), Some(steered.id.clone()))
        ]
    );
    assert!(filo_local::steer::all(&store, &chat.id).is_empty());
    std::fs::remove_dir_all(store.root()).ok();
}

/// A turn the agents' group gave the agent, running in its own chat, doesn't take what the user sends there meanwhile
/// (its replies go to the group): the user's message waits for its own turn.
#[test]
fn a_group_turn_leaves_the_users_own_message_for_its_own_turn() {
    let store = Store::at(common::temp_root("steer-group"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    let call = filo_local::store::GroupCall {
        speaker: "chef".into(),
        from: None,
        message_id: None,
        ask: "Dinner?".into(),
        context: String::new(),
        answers_user: false,
        after_question: false,
        checking: false,
    };
    let asked = filo_local::store::Message {
        group_call: Some(call),
        ..filo_local::store::Message::new(Role::User, "work a moment until I say", Some("turn-1".into()))
    };
    store.append(&chat.id, &asked).unwrap();
    let (started, starts) = std::sync::mpsc::channel();
    let first = {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-1", move |_| started.send(()).unwrap()))
    };
    starts.recv().unwrap();
    starts.recv().unwrap();
    let own = store.add_user_message(&chat.id, "Don't send without my approval", "turn-2").unwrap();
    let second = turn::run(&store, &runtime, &chat.id, "turn-2", |_| {});
    assert!(first.join().unwrap().error.is_none());
    assert_eq!(second, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    let reply = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .find(|message| message.role == Role::Assistant && message.turn.as_deref() == Some("turn-2"))
        .expect("turn-2's reply");
    assert_eq!(
        (reply.text.as_str(), reply.reply_to.as_deref()),
        ("echo: Don't send without my approval", Some(own.id.as_str()))
    );
    std::fs::remove_dir_all(store.root()).ok();
}

/// A message sent while a turn answers whose CLI doesn't take it before its turn ends (here it never reads it) waits
/// for it, and resumes the session it left.
#[test]
fn a_message_the_cli_never_takes_waits_for_the_turn_and_resumes_its_session() {
    let store = Store::at(common::temp_root("queue"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "please run a command", "turn-1").unwrap();
    let run = |turn: &'static str| {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, turn, |_| {}))
    };
    let first = run("turn-1");
    let began = std::time::Instant::now();
    while chat_activity::get(&store, &chat.id, "turn-1").is_none() && began.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    // The first turn is answering. A message the user stops while it waits ends without running.
    store.add_user_message(&chat.id, "never mind", "turn-stopped").unwrap();
    assert_eq!(
        turn::run(&store, &runtime, &chat.id, "turn-stopped", |stopper| stopper.stop()),
        Report { replies: 0, stopped: true, error: None, ..Report::default() }
    );
    // Two more, back to back; the later one's worker happens to start first, and still goes after the earlier one.
    store.add_user_message(&chat.id, "what did you do", "turn-2").unwrap();
    store.add_user_message(&chat.id, "and then", "turn-3").unwrap();
    let third = run("turn-3");
    std::thread::sleep(Duration::from_millis(100));
    let second = run("turn-2");
    for turn in [first, second, third] {
        assert_eq!(turn.join().unwrap(), Report { replies: 1, stopped: false, error: None, ..Report::default() });
    }
    // Each answered after the one before had ended, the second resuming the session the first saved: it knows what
    // the first replied and handed off.
    let session = store.chat(&chat.id).unwrap().unwrap().session.unwrap().id;
    let replies: Vec<(String, String)> = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| (message.turn.unwrap(), message.text))
        .collect();
    let expected = [
        ("turn-1", "ran it".to_string()),
        ("turn-2", format!("resumed {session}")),
        ("turn-3", "echo: and then".to_string()),
    ];
    assert_eq!(replies, expected.map(|(turn, text)| (turn.to_string(), text)));
    std::fs::remove_dir_all(store.root()).ok();
}

/// A turn takes as long as its work does: one busy for longer than `hung_after` finishes; one that says nothing for that
/// long has hung and is stopped, saying so in the chat, and the message sent behind it still gets its answer.
#[test]
fn a_busy_turn_goes_on_and_a_hung_one_is_stopped() {
    let store = Store::at(common::temp_root("hung"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    // First with the usual time: a binary just built takes seconds to start the first time.
    store.add_user_message(&chat.id, "hello", "turn-0").unwrap();
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-0", |_| {}).replies, 1);
    let runtime = Runtime { hung_after: Duration::from_secs(3), ..runtime };
    store.add_user_message(&chat.id, "keep working", "turn-1").unwrap();
    let began = std::time::Instant::now();
    assert_eq!(
        turn::run(&store, &runtime, &chat.id, "turn-1", |_| {}),
        Report { replies: 1, stopped: false, error: None, ..Report::default() }
    );
    assert!(began.elapsed() > Duration::from_secs(5), "{:?}", began.elapsed());

    store.add_user_message(&chat.id, "hang", "turn-2").unwrap();
    let (started, starts) = std::sync::mpsc::channel();
    let hung = {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-2", move |_| started.send(()).unwrap()))
    };
    // Its Stopper for the wait, then its CLI's: it holds the chat.
    starts.recv().unwrap();
    starts.recv().unwrap();
    store.add_user_message(&chat.id, "and then", "turn-3").unwrap();
    let next = {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-3", |_| {}))
    };
    let stuck = "Claude Code seemed stuck: nothing came from it for 3 seconds, so it was stopped.";
    assert_eq!(
        hung.join().unwrap(),
        Report { replies: 0, stopped: false, error: Some(stuck.into()), ..Report::default() }
    );
    assert_eq!(next.join().unwrap(), Report { replies: 1, stopped: false, error: None, ..Report::default() });
    assert!(began.elapsed() < Duration::from_secs(40), "{:?}", began.elapsed());
    let said: Vec<(Role, String, String)> = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role != Role::User)
        .map(|message| (message.role, message.turn.unwrap(), message.text))
        .collect();
    let expected = [
        (Role::Assistant, "turn-0", "echo: hello"),
        (Role::Assistant, "turn-1", "worked"),
        (Role::Error, "turn-2", stuck),
        (Role::Assistant, "turn-3", "echo: and then"),
    ];
    assert_eq!(said, expected.map(|(role, turn, text)| (role, turn.to_string(), text.to_string())));
    std::fs::remove_dir_all(store.root()).ok();
}

/// A turn that starts a server ends once it has replied, though it left work in the background, and the server it
/// started detached keeps running: the next message is answered while it serves.
#[test]
fn a_turn_that_starts_a_server_ends_once_it_has_replied_and_the_server_stays_up() {
    let store = Store::at(common::temp_root("server"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    store.add_user_message(&chat.id, &format!("please start a server on port {port}"), "turn-1").unwrap();
    let began = std::time::Instant::now();
    assert_eq!(
        turn::run(&store, &runtime, &chat.id, "turn-1", |_| {}),
        Report { replies: 1, stopped: false, error: None, ..Report::default() }
    );
    assert!(began.elapsed() < Duration::from_secs(15), "the turn waited on its background work: {:?}", began.elapsed());
    assert_eq!(chat_activity::get(&store, &chat.id, "turn-1"), None);
    let reply =
        store.messages(&chat.id).unwrap().into_iter().rfind(|message| message.role == Role::Assistant).unwrap().text;
    let pid: u32 = reply.rsplit("pid ").next().unwrap().parse().unwrap_or_else(|_| panic!("{reply}"));
    let serves = || {
        use std::io::{Read as _, Write as _};
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else { return false };
        let mut answer = String::new();
        stream.write_all(b"GET / HTTP/1.0\r\n\r\n").is_ok()
            && stream.read_to_string(&mut answer).is_ok()
            && answer.starts_with("HTTP/1.0 200")
    };
    let up = std::time::Instant::now();
    while !serves() && up.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(serves(), "the server isn't up after its turn");
    store.add_user_message(&chat.id, "and then", "turn-2").unwrap();
    assert_eq!(
        turn::run(&store, &runtime, &chat.id, "turn-2", |_| {}),
        Report { replies: 1, stopped: false, error: None, ..Report::default() }
    );
    assert!(serves(), "the server stopped with the next turn");
    std::process::Command::new("kill").arg(pid.to_string()).status().unwrap();
    std::fs::remove_dir_all(store.root()).ok();
}

/// A real turn on the installed `FILO_REAL_ENGINE` (claude by default, or codex; `FILO_REAL_MODE` for its Coding page
/// mode, Auto by default) asked for a web server: it starts one that outlives the turn, the turn ends once it has
/// replied, and the next message is answered while the server keeps serving. Uses the tool's sign-in:
/// `cargo test -p filo-local --test turn_e2e starts_a_server -- --ignored --nocapture` (`FILO_REAL_LOG=<file>` keeps the chat).
#[test]
#[ignore]
fn a_real_turn_that_starts_a_server() {
    let engine =
        Engine::parse(&std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "claude".into())).expect("an engine");
    let store = Store::at(common::temp_root(&format!("real-server-{}", engine.program())));
    let db_path = store.root().join(db::FILE);
    let db = Db::open(&db_path).unwrap();
    let mode = std::env::var("FILO_REAL_MODE").unwrap_or_else(|_| "auto".into());
    filo_local::coding::update_tool(&db, filo_local::coding::tool_of(engine), |settings| {
        settings.mode = mode.clone();
        Ok(())
    })
    .unwrap();
    let runtime = common::runtime(std::env::var("PATH").ok(), Some(db_path), Duration::from_secs(300));
    let chat = store.create_chat(engine).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let serves = || {
        std::process::Command::new("curl")
            .args(["-sf", "-o", "/dev/null", &format!("http://127.0.0.1:{port}/")])
            .status()
            .is_ok_and(|status| status.success())
    };
    let mut log = String::new();
    let mut ask = |turn: &str, text: &str| -> (Report, Duration) {
        store.add_user_message(&chat.id, text, turn).unwrap();
        let began = std::time::Instant::now();
        let report = turn::run(&store, &runtime, &chat.id, turn, |_| {});
        let took = began.elapsed();
        let replies: Vec<String> = store
            .messages(&chat.id)
            .unwrap()
            .into_iter()
            .filter(|message| message.turn.as_deref() == Some(turn) && message.role != Role::User)
            .map(|message| format!("{:?}: {}", message.role, message.text))
            .collect();
        let line = format!(
            "> {text}\n{}\n(took {took:.1?}; server up after it: {}; {report:?})\n",
            replies.join("\n"),
            serves()
        );
        println!("{line}");
        log.push_str(&line);
        (report, took)
    };
    let (first, took) = ask(
        "turn-1",
        &format!(
            "Start a local web server for your working folder on port {port} with python3's http.server, so I can open it in my browser."
        ),
    );
    let (second, _) = ask("turn-2", "Thanks! Reply with send_message: is it still running, and how do I stop it?");
    let up = serves();
    if let Ok(file) = std::env::var("FILO_REAL_LOG") {
        std::fs::write(file, format!("{} chat ({mode})\n\n{log}", engine.label())).unwrap();
    }
    // Whatever happened, the server goes.
    std::process::Command::new("sh")
        .args(["-c", &format!("lsof -ti tcp:{port} -sTCP:LISTEN | xargs kill")])
        .status()
        .ok();
    assert!(first.error.is_none() && first.replies >= 1, "{first:?}");
    assert!(took < Duration::from_secs(240), "{took:?}");
    assert!(second.error.is_none() && second.replies >= 1, "{second:?}");
    assert!(up, "the server didn't outlive its turn");
    std::fs::remove_dir_all(store.root()).ok();
}

/// A real turn on the installed `FILO_REAL_ENGINE` (claude by default, or codex, muse or opencode; `FILO_REAL_MODE`,
/// Auto by default; `FILO_REAL_MODEL` for the chat's model) told to run three slow commands one after another, and,
/// while the first runs, a message changing the plan: stop after it and say BANANA. The turn takes the message at its
/// next step, doesn't run the rest, and the reply answers that message; the message's own turn ends without running.
/// Uses the tool's sign-in:
/// `cargo test -p filo-local --test turn_e2e takes_a_message_sent_mid_turn -- --ignored --nocapture`
/// (`FILO_REAL_LOG=<file>` keeps what happened).
#[test]
#[ignore]
fn a_real_turn_takes_a_message_sent_mid_turn() {
    let engine =
        Engine::parse(&std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "claude".into())).expect("an engine");
    let store = Store::at(common::temp_root(&format!("real-steer-{}", engine.program())));
    let db_path = store.root().join(db::FILE);
    let db = Db::open(&db_path).unwrap();
    let mode = std::env::var("FILO_REAL_MODE").unwrap_or_else(|_| "auto".into());
    filo_local::coding::update_tool(&db, filo_local::coding::tool_of(engine), |settings| {
        settings.mode = mode.clone();
        Ok(())
    })
    .unwrap();
    let runtime = common::runtime(std::env::var("PATH").ok(), Some(db_path), Duration::from_secs(300));
    let chat = store.create_chat(engine).unwrap();
    store.update_chat(&chat.id, |chat| chat.model = std::env::var("FILO_REAL_MODEL").ok()).unwrap();
    // In its working folder, where its shell writes in either mode.
    let marks = store.workspace().unwrap().join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let ask = format!(
        "Run these three shell commands one at a time, each in its own tool call, waiting for each to finish: `sleep 8; touch {0}/one`, then `sleep 8; touch {0}/two`, then `sleep 8; touch {0}/three`. When all three are done, send me one short message saying so.",
        marks.display()
    );
    store.add_user_message(&chat.id, &ask, "turn-1").unwrap();
    let began = std::time::Instant::now();
    let first = {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-1", |_| {}))
    };
    // Once its first command runs, the plan changes.
    while chat_activity::get(&store, &chat.id, "turn-1").is_none() && began.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let sent_after = began.elapsed();
    let change = "Change of plan: don't run any more of those commands after the one that's running now. Just reply with the single word BANANA.";
    let steered = store.add_user_message(&chat.id, change, "turn-2").unwrap();
    let second = {
        let (store, runtime, chat_id) = (store.clone(), runtime.clone(), chat.id.clone());
        std::thread::spawn(move || turn::run(&store, &runtime, &chat_id, "turn-2", |_| {}))
    };
    let second = second.join().unwrap();
    let first = first.join().unwrap();
    let took = began.elapsed();
    let ran: Vec<&str> = ["one", "two", "three"].into_iter().filter(|mark| marks.join(mark).exists()).collect();
    let messages = store.messages(&chat.id).unwrap();
    let said: Vec<String> = messages
        .iter()
        .filter(|message| message.role != Role::User)
        .map(|message| {
            let answers = messages
                .iter()
                .find(|asked| Some(&asked.id) == message.reply_to.as_ref())
                .map_or("nothing".to_string(), |asked| {
                    format!("{:?}", asked.text.chars().take(40).collect::<String>())
                });
            format!(
                "{:?} in {}, answering {answers}: {}",
                message.role,
                message.turn.as_deref().unwrap_or("-"),
                message.text
            )
        })
        .collect();
    let log = format!(
        "{} chat ({mode})\n\n> {ask}\n(the first command started after {sent_after:.1?})\n> {change}\n\n{}\n\ncommands that ran: {ran:?}\nturn-1: {first:?}\nturn-2: {second:?}\ntook {took:.1?}\nsteered: {:?}\n",
        engine.label(),
        said.join("\n"),
        filo_local::steer::all(&store, &chat.id)
    );
    println!("{log}");
    if let Ok(file) = std::env::var("FILO_REAL_LOG") {
        std::fs::write(file, &log).unwrap();
    }
    assert!(first.error.is_none() && first.replies >= 1, "{first:?}");
    assert_eq!(second, Report::default());
    assert!(!ran.contains(&"three"), "it ran all three: {ran:?}");
    let banana = messages
        .iter()
        .find(|message| message.role == Role::Assistant && message.text.contains("BANANA"))
        .expect("a BANANA reply");
    assert_eq!((banana.turn.as_deref(), banana.reply_to.as_deref()), (Some("turn-1"), Some(steered.id.as_str())));
    std::fs::remove_dir_all(store.root()).ok();
}

/// A real turn on the installed `FILO_REAL_ENGINE` (claude by default, or codex; `FILO_REAL_MODE`, Auto by default) as
/// a launch agent asked for a launch plan, then told the user is busy for half an hour: what it says comes next is done
/// in the reply, handed off or scheduled as a check-in, or offered as a question, since nothing wakes it after its turn
/// (Product Launch, Oct 13: "Next I'll draft the listing text", then idle). Catches the plain promises ("next I'll",
/// "I'll check back") left with no check-in or coding task; the replies are printed to read:
/// `cargo test -p filo-local --test turn_e2e follow_up -- --ignored --nocapture` (`FILO_REAL_LOG=<file>` keeps the chat).
#[test]
#[ignore]
fn a_real_turn_does_or_sets_up_its_follow_up_work() {
    let engine =
        Engine::parse(&std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "claude".into())).expect("an engine");
    let store = Store::at(common::temp_root(&format!("real-follow-up-{}", engine.program())));
    let db_path = store.root().join(db::FILE);
    let db = Db::open(&db_path).unwrap();
    let mode = std::env::var("FILO_REAL_MODE").unwrap_or_else(|_| "auto".into());
    filo_local::coding::update_tool(&db, filo_local::coding::tool_of(engine), |settings| {
        settings.mode = mode.clone();
        Ok(())
    })
    .unwrap();
    let runtime = common::runtime(std::env::var("PATH").ok(), Some(db_path), Duration::from_secs(300));
    let chat = store.create_chat(engine).unwrap();
    store
        .update_chat(&chat.id, |chat| {
            chat.instructions = Some(
                "You are Product Launch, an agent the user made in the Filo desktop app on their Mac, talking with them in your own conversation. \
The user only sees what you send with the `send_message` tool (from the `filo` MCP server); your plain text output is never shown. \
Answer every message by calling `send_message` with your reply in Markdown, usually once per turn.\n\n\
Who you are and how to work, in the user's words:\n\nYou run the Product Hunt launch of my app: plan it, write the copy and keep it on track."
                    .into(),
            )
        })
        .unwrap();
    let mut log = String::new();
    let mut promises = Vec::new();
    let asks = [
        "We launch Filo on Product Hunt next Tuesday. Filo is a Mac app where your AI agents, running on Claude Code or Codex, chat with you and with \
each other in one group, hand coding work to coding tasks and check in on their own. Lay out what we need to get ready, and keep it short.",
        "Good. I'm recording the demo video now, it'll take me about half an hour. After that we'll want captions for the gallery images.",
    ];
    for (n, text) in asks.iter().enumerate() {
        let turn = format!("turn-{}", n + 1);
        store.add_user_message(&chat.id, text, &turn).unwrap();
        let began = std::time::Instant::now();
        let report = turn::run(&store, &runtime, &chat.id, &turn, |_| {});
        let took = began.elapsed();
        let replies: Vec<String> = store
            .messages(&chat.id)
            .unwrap()
            .into_iter()
            .filter(|message| message.turn.as_deref() == Some(turn.as_str()) && message.role != Role::User)
            .map(|message| message.text)
            .collect();
        let wakes: Vec<String> = filo_local::schedule::wakes(&db, None)
            .unwrap()
            .into_iter()
            .filter(|wake| wake.status == "pending")
            .map(|wake| format!("{} ({})", wake.title, wake.note))
            .collect();
        let tasks = filo_local::coding::tasks(&db, 10).unwrap().len();
        let line = format!(
            "> {text}\n{}\n(took {took:.1?}; check-ins: {wakes:?}; coding tasks: {tasks}; {report:?})\n",
            replies.join("\n---\n")
        );
        println!("{line}");
        log.push_str(&line);
        assert!(report.error.is_none() && report.replies >= 1, "{report:?}");
        let said = replies.join("\n").to_lowercase().replace('\u{2019}', "'");
        let promised = [
            "next i'll",
            "next, i'll",
            "then i'll",
            "i'll check back",
            "i'll check in",
            "i'll follow up",
            "i'll get back",
            "i'll come back",
            "i'll circle back",
            "i'll ping you",
        ]
        .into_iter()
        .filter(|promise| said.contains(promise))
        .collect::<Vec<_>>();
        if !promised.is_empty() && wakes.is_empty() && tasks == 0 {
            promises.push(format!("{turn}: {promised:?}"));
        }
    }
    if let Ok(file) = std::env::var("FILO_REAL_LOG") {
        std::fs::write(file, format!("{} chat ({mode})\n\n{log}", engine.label())).unwrap();
    }
    assert!(promises.is_empty(), "said it would do something later with nothing set up to bring it back: {promises:?}");
    std::fs::remove_dir_all(store.root()).ok();
}

#[test]
fn a_turn_gets_the_users_skills() {
    let store = Store::at(common::temp_root("skills"));
    let (runtime, _) = stand_in(&store);
    let dir = skills::folder_for(runtime.db.as_ref().unwrap());
    let fields = skills::Fields {
        name: "Weekly Report".into(),
        description: "When asked for a weekly report".into(),
        body: "1. Group by project.".into(),
    };
    assert_eq!(skills::save(&dir, &fields).unwrap(), "weekly-report");
    // A skill folder from a repository, for the agent to install with its own tools.
    let repo = store.root().join("repo/web-demo");
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::write(
        repo.join("SKILL.md"),
        "---\nname: web-demo\ndescription: When recording a demo\n---\nRun scripts/record.sh.\n",
    )
    .unwrap();
    std::fs::write(repo.join("scripts/record.sh"), "echo hi").unwrap();
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, &format!("install the skill at {}", repo.display()), "turn-0").unwrap();
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-0", |_| {}).replies, 1);
    assert_eq!(store.messages(&chat.id).unwrap().last().unwrap().text, format!("installed in {}", dir.display()));
    assert_eq!(skills::read(&dir, "web-demo").unwrap().files, ["scripts/record.sh"]);

    store.add_user_message(&chat.id, "/weekly-report use the skill", "turn-1").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-1", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });

    let reply = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role == Role::Assistant)
        .next_back()
        .unwrap();
    let seen: serde_json::Value = serde_json::from_str(&reply.text).unwrap();
    // No use_skill and no list in the instructions: the skill is Claude Code's own, in the folder it runs in, and with
    // the document and chart ones the only skills it may see. Named in the message, its instructions come along.
    assert!(!seen["tools"].as_array().unwrap().iter().any(|tool| tool == "use_skill"), "{seen}");
    assert_eq!(
        seen["skill"],
        json!(
            "---\nname: weekly-report\ndescription: \"When asked for a weekly report\"\n---\n\n1. Group by project.\n"
        ),
        "{seen}"
    );
    // The installed one is a skill of the next turn, its script beside it.
    assert_eq!(
        seen["skills"],
        json!(["web-demo", "weekly-report", "pdf", "docx", "xlsx", "pptx", "dataviz", "desktop-pet", "artifacts"]),
        "{seen}"
    );
    assert_eq!(
        (&seen["listed"], &seen["attached"], &seen["script"]),
        (&json!(false), &json!(true), &json!(true)),
        "{seen}"
    );
    std::fs::remove_dir_all(store.root()).ok();
}

/// A stand-in `codex app-server`: keeps its command line and each turn's first words in `calls.jsonl` beside it, with
/// the thread it resumed; starts the real MCP server with the pinned tools from its `-c mcp_servers.filo_core.*` options
/// and replies with send_message. Told to work until told otherwise, it's busy running a command until a `turn/steer`
/// comes, which it acts on.
const FAKE_CODEX: &str = r##"#!/usr/bin/env python3
import json, os, queue, subprocess, sys, threading, time
args = sys.argv[1:]
if args[:1] != ["app-server"]:
    sys.exit("not the app-server: %s" % args)
options = [args[i + 1] for i, arg in enumerate(args) if arg == "-c"]
value = lambda key: json.loads([option for option in options if option.startswith(key + "=")][0].split("=", 1)[1])
out = threading.Lock()
def send(message):
    with out:
        print(json.dumps(message), flush=True)
notify = lambda method, params: send({"jsonrpc": "2.0", "method": method, "params": params})
steers = queue.Queue()
state = {"thread": None, "resumed": None, "turn": None, "turns": 0}
def work(text):
    server = subprocess.Popen([value("mcp_servers.filo_core.command")] + value("mcp_servers.filo_core.args"), stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    def call(id, method, params):
        server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
        server.stdin.flush()
        return json.loads(server.stdout.readline())
    call(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}})
    tools = [tool["name"] for tool in call(3, "tools/list", {})["result"]["tools"]]
    if "send_message" not in tools or "create_note" not in tools or "list_notes" in tools:
        sys.exit("filo_core lists " + ",".join(tools))
    if text.splitlines()[-1] == "work until I say":
        notify("item/started", {"item": {"id": "call_1", "type": "commandExecution", "command": "/bin/zsh -lc 'sleep 60'", "status": "inProgress"}})
        try:
            said = steers.get(timeout=20).splitlines()[-1]
            # As Codex does: the message goes into the conversation, and the turn's next step follows it.
            notify("item/started", {"item": {"id": "user_2", "type": "userMessage"}})
            notify("item/started", {"item": {"id": "reasoning_2", "type": "reasoning"}})
        except queue.Empty:
            said = "nothing"
        notify("item/completed", {"item": {"id": "call_1", "type": "commandExecution", "status": "completed", "exitCode": 0}})
        time.sleep(0.5)
        call(2, "tools/call", {"name": "send_message", "arguments": {"text": "heard: " + said}})
    else:
        call(2, "tools/call", {"name": "send_message", "arguments": {"text": "ok"}})
    server.stdin.close()
    server.wait()
    turn, state["turn"] = state["turn"], None
    notify("item/completed", {"item": {"id": "msg", "type": "agentMessage", "text": "done"}})
    notify("turn/completed", {"turn": {"id": turn, "status": "completed"}})
for line in iter(sys.stdin.readline, ""):
    message = json.loads(line)
    method, id, params = message.get("method"), message.get("id"), message.get("params", {})
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": id, "result": {}})
    elif method in ("thread/start", "thread/resume"):
        state["thread"] = params.get("threadId", "thread-1")
        state["resumed"] = params.get("threadId")
        send({"jsonrpc": "2.0", "id": id, "result": {"thread": {"id": state["thread"]}}})
    elif method == "turn/start":
        text = params["input"][0]["text"]
        with open(os.path.join(os.path.dirname(__file__), "calls.jsonl"), "a") as log:
            log.write(json.dumps({"args": args, "stdin": text, "resume": state["resumed"], "thread": params}) + "\n")
        state["turns"] += 1
        state["turn"] = "turn-%d" % state["turns"]
        send({"jsonrpc": "2.0", "id": id, "result": {"turn": {"id": state["turn"]}}})
        threading.Thread(target=work, args=(text,)).start()
    elif method == "turn/steer":
        if state["turn"] and params.get("expectedTurnId") == state["turn"]:
            send({"jsonrpc": "2.0", "id": id, "result": {"turnId": state["turn"]}})
            steers.put(params["input"][0]["text"])
        else:
            send({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "no active turn to steer"}})
"##;

#[test]
fn a_resumed_codex_session_gets_instructions_that_changed() {
    let store = Store::at(common::temp_root("codex"));
    let (runtime, _) = stand_in(&store);
    let bin = store.root().join("bin");
    common::stand_in(&bin, "codex", FAKE_CODEX);
    let calls = || -> Vec<serde_json::Value> {
        std::fs::read_to_string(bin.join("calls.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };

    let chat = store.create_chat(Engine::Codex).unwrap();
    store.update_chat(&chat.id, |chat| chat.instructions = Some("You are Pip, a cheerful tutor.".into())).unwrap();
    let turn = |n: usize, text: &str| {
        store.add_user_message(&chat.id, text, &format!("turn-{n}")).unwrap();
        let report = turn::run(&store, &runtime, &chat.id, &format!("turn-{n}"), |_| {});
        assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
        calls().pop().unwrap()
    };

    // A new session starts with the instructions, what the turn can use after them. Codex is in Auto on the Coding
    // page until the user picks Full access, and the turn's shell works as it does.
    let first = turn(1, "hello");
    let stdin = first["stdin"].as_str().unwrap();
    assert!(
        stdin.starts_with("You are Pip, a cheerful tutor.\n\nWhat you can use here:")
            && stdin.contains("a shell on this Mac, sandboxed to your working folder"),
        "{stdin}"
    );
    let args: Vec<&str> = first["args"].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()).collect();
    assert!(
        args.contains(&"notify=[]")
            && args.contains(&"web_search=\"live\"")
            && args.contains(&"sandbox_mode=\"workspace-write\"")
            && args.contains(&"approvals_reviewer=\"auto_review\""),
        "{args:?}"
    );
    let session = store.chat(&chat.id).unwrap().unwrap().session.unwrap();
    assert_eq!(session.id, "thread-1");
    assert!(session.instructions.is_some());

    // Resumed with the same instructions: only the reminder.
    let second = turn(2, "and again");
    assert_eq!(second["resume"], "thread-1");
    assert!(!second["stdin"].as_str().unwrap().contains("Pip"), "{second}");

    // The agent edited: the resumed session gets the current instructions, once.
    store.update_chat(&chat.id, |chat| chat.instructions = Some("You are Pip, a strict tutor.".into())).unwrap();
    let third = turn(3, "what now");
    let stdin = third["stdin"].as_str().unwrap();
    assert!(
        stdin.contains("have changed") && stdin.contains("You are Pip, a strict tutor.") && !stdin.contains("cheerful"),
        "{stdin}"
    );
    assert!(stdin.ends_with("what now"), "{stdin}");
    assert!(!turn(4, "thanks")["stdin"].as_str().unwrap().contains("strict"));

    // A session kept before the instructions were marked gets them once too.
    store.update_chat(&chat.id, |chat| chat.session.as_mut().unwrap().instructions = None).unwrap();
    assert!(turn(5, "still there?")["stdin"].as_str().unwrap().contains("You are Pip, a strict tutor."));

    // Codex put in Full access on the Coding page: the next turn's shell runs anything, and it's told so.
    let db = Db::open(&store.root().join(db::FILE)).unwrap();
    filo_local::coding::update_tool(&db, "codex", |tool| {
        let _: () = tool.mode = "full_access".into();
        Ok(())
    })
    .unwrap();
    let sixth = turn(6, "clean up my downloads");
    let stdin = sixth["stdin"].as_str().unwrap();
    assert!(stdin.contains("have changed") && stdin.contains("a shell that runs any command"), "{stdin}");
    let args: Vec<&str> = sixth["args"].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()).collect();
    assert!(
        args.contains(&"sandbox_mode=\"danger-full-access\"")
            && args.contains(&"approval_policy=\"never\"")
            && !args.iter().any(|arg| arg.starts_with("approvals_reviewer")),
        "{args:?}"
    );
    std::fs::remove_dir_all(store.root()).ok();
}

/// "Share to claude.ai": off by default, a Claude Code turn goes as before; on (and signed in to claude.ai), it gets the
/// Artifact tool and is told when to use it, and a page of an artifact it publishes keeps the link. Codex never gets it.
#[test]
fn sharing_to_claude_ai_is_off_until_turned_on_and_never_for_codex() {
    use filo_local::{artifact_share, artifacts};
    let store = Store::at(common::temp_root("share"));
    let (runtime, _) = stand_in(&store);
    // Claude Code signed in to a claude.ai account (only this test reads it).
    let config = store.root().join("claude-config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"me@example.com"}}"#).unwrap();
    // SAFETY: no other test reads CLAUDE_CONFIG_DIR, and none but this one turns sharing on.
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &config) };
    let db_path = runtime.db.clone().unwrap();
    let dir = artifacts::folder_for(&db_path);
    let made = {
        let db = Db::open(&db_path).unwrap();
        let new =
            artifacts::New { title: "Plan".into(), html: "<title>Plan</title><p>Hi</p>".into(), ..Default::default() };
        artifacts::create(&db, &dir, &new, &artifacts::Origin::default(), 1).unwrap()
    };
    // What the turn is told to publish: the page's copy for claude.ai, which it makes while sharing is on.
    let page = dir.join(&made.id).join("claude").join("index.html");
    let chat = store.create_chat(Engine::Claude).unwrap();
    let mut n = 0;
    let mut share = |chat_id: &str| -> serde_json::Value {
        n += 1;
        let turn_id = format!("turn-{n}");
        store.add_user_message(chat_id, &format!("share it: {}", page.display()), &turn_id).unwrap();
        let report = turn::run(&store, &runtime, chat_id, &turn_id, |_| {});
        assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
        let reply = store
            .messages(chat_id)
            .unwrap()
            .into_iter()
            .filter(|message| message.role == Role::Assistant)
            .next_back()
            .unwrap();
        serde_json::from_str(&reply.text).unwrap_or(json!(reply.text))
    };

    // Off by default: no tool, no variable, nothing said, no link.
    assert_eq!(share(&chat.id), json!({ "tools": false, "allowed": false, "env": null, "told": false }));
    assert_eq!(artifact_share::link(&dir, &made.id), None);
    assert!(!page.exists());

    // On: the tool, its variable and when to use it; the page it published keeps the link, with the version.
    std::fs::write(store.root().join(artifact_share::SETTINGS_FILE), r#"{"shareToClaude":true}"#).unwrap();
    assert_eq!(share(&chat.id), json!({ "tools": true, "allowed": true, "env": "1", "told": true }));
    assert!(page.is_file());
    let link = artifact_share::link(&dir, &made.id).expect("link kept");
    assert_eq!((link.url.as_str(), link.version), ("https://claude.ai/artifact/abc123", 1));

    // Signed in with an API key only (no claude.ai account): nothing changes.
    std::fs::write(config.join(".claude.json"), r#"{"primaryApiKey":"sk"}"#).unwrap();
    assert_eq!(share(&chat.id)["tools"], json!(false));
    std::fs::write(config.join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"me@example.com"}}"#).unwrap();

    // Codex, with sharing on: as always.
    let bin = store.root().join("bin");
    common::stand_in(&bin, "codex", FAKE_CODEX);
    let codex = store.create_chat(Engine::Codex).unwrap();
    store.add_user_message(&codex.id, "hello", "codex-1").unwrap();
    assert_eq!(
        turn::run(&store, &runtime, &codex.id, "codex-1", |_| {}),
        Report { replies: 1, stopped: false, error: None, ..Report::default() }
    );
    let logged = std::fs::read_to_string(bin.join("calls.jsonl")).unwrap();
    // (Filo's own Artifacts library and its publish_artifact are every engine's; Claude Code's Artifact tool isn't.)
    assert!(
        !logged.contains("Artifact tool") && !logged.contains(",Artifact") && !logged.contains("claude.ai"),
        "{logged}"
    );
    std::fs::remove_dir_all(store.root()).ok();
}

/// A stand-in `muse serve`. Told to work until told otherwise, it's busy running a command until a `turn/steer` comes,
/// which it acts on; with a `muse.refuse-steer` file beside it, it refuses the steer, and once interrupted, acts on the
/// message the next turn starts with. What it was sent goes in `muse.heard`.
const FAKE_MUSE: &str = r##"#!/usr/bin/env python3
import json, os, subprocess, sys, time
here = os.path.dirname(os.path.abspath(__file__))
def send(message):
    print(json.dumps(message), flush=True)
notify = lambda method, params: send({"jsonrpc": "2.0", "method": method, "params": params})
server = None
def call(id, method, params):
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    server.stdin.flush()
    return json.loads(server.stdout.readline())
def reply(turn, said):
    notify("item/started", {"item": {"itemId": "c1", "kind": "toolCall", "tool": "bash", "turnId": turn, "args": "{\"command\":\"echo hi\"}"}})
    call(2, "tools/call", {"name": "send_message", "arguments": {"text": said}})
    notify("item/completed", {"item": {"itemId": "m1", "kind": "agentMessage", "turnId": turn, "text": "sent"}})
    notify("turn/completed", {"turnId": turn, "terminal": "completed"})
turns, working = 0, None
for line in iter(sys.stdin.readline, ""):
    with open(os.path.join(here, "muse.heard"), "a") as heard:
        heard.write(line)
    message = json.loads(line)
    method, params = message.get("method"), message.get("params", {})
    if method == "initialize":
        # A chat's MCP servers come with its session, which needs asking for.
        if "sessionMcp" not in params["capabilities"].get("requestedCapabilities", []):
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32602, "message": "session MCP configuration requires the sessionMcp capability"}})
            continue
        send({"jsonrpc": "2.0", "id": message["id"], "result": {}})
    elif method in ("session/start", "session/resume"):
        filo = params["config"]["mcpServers"]["filo"]
        server = subprocess.Popen([filo["command"]] + filo["args"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        call(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}})
        resumed = params.get("sessionId")
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"session": {"sessionId": resumed or "ms-1", "modelId": "muse-spark-1.3"}}})
    elif method == "turn/start":
        text = params["input"][0]["text"]
        turns += 1
        turn = "tu-%d" % turns
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"turnId": turn}})
        if text.splitlines()[-1] == "hang":
            continue
        if text.splitlines()[-1] == "work until I say":
            working = turn
            notify("item/started", {"item": {"itemId": "w1", "kind": "toolCall", "tool": "bash", "turnId": turn, "args": "{\"command\":\"sleep 60\"}"}})
            continue
        if text.splitlines()[-1] == "keep working":
            for step in range(20):
                notify("item/started", {"item": {"itemId": "w%d" % step, "kind": "toolCall", "tool": "bash", "turnId": turn, "args": "{\"command\":\"make\"}"}})
                time.sleep(0.3)
        # A turn started after an interruption, with the message.
        if "stopped mid-turn" in text:
            reply(turn, "heard: " + text.splitlines()[-1])
            continue
        reply(turn, ("resumed " + resumed) if resumed else ("echo: " + text.splitlines()[-1]))
    elif method == "turn/steer":
        if os.path.exists(os.path.join(here, "muse.refuse-steer")) or params.get("expectedTurnId") != working:
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32030, "message": "turn/steer rejected"}})
            continue
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"status": "accepted", "turnId": working}})
        turn, working = working, None
        time.sleep(0.5)
        reply(turn, "heard: " + params["input"][0]["text"].splitlines()[-1])
    elif method == "turn/interrupt":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {}})
        if working == params.get("turnId"):
            notify("turn/completed", {"turnId": working, "terminal": "cancelled"})
            working = None
if server:
    server.stdin.close()
    server.wait()
"##;

/// A chat on Muse Code with a stand-in `muse serve`: it starts the real MCP server Filo gave its session, replies with
/// send_message, and the next message resumes its session.
#[test]
fn a_muse_code_chat_turn_with_the_real_mcp_server() {
    let store = Store::at(common::temp_root("muse"));
    let (runtime, _) = stand_in(&store);
    let bin = store.root().join("bin");
    std::fs::write(bin.join("muse"), FAKE_MUSE).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(bin.join("muse"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let chat = store.create_chat(Engine::Muse).unwrap();
    store.add_user_message(&chat.id, "hello muse", "turn-1").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-1", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    let session = store.chat(&chat.id).unwrap().unwrap().session.expect("session kept");
    assert_eq!((session.engine, session.id.as_str()), (Engine::Muse, "ms-1"));
    store.add_user_message(&chat.id, "again", "turn-2").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-2", |_| {});
    assert_eq!(report.replies, 1, "{report:?}");
    let replies: Vec<String> = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| message.text)
        .collect();
    assert_eq!(replies, ["echo: hello muse", "resumed ms-1"]);

    // Busy for longer than `hung_after`, it goes on; quiet for that long, it has hung, and the next message is answered.
    let runtime = Runtime { hung_after: Duration::from_secs(3), ..runtime };
    store.add_user_message(&chat.id, "keep working", "turn-3").unwrap();
    assert_eq!(
        turn::run(&store, &runtime, &chat.id, "turn-3", |_| {}),
        Report { replies: 1, stopped: false, error: None, ..Report::default() }
    );
    store.add_user_message(&chat.id, "hang", "turn-4").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-4", |_| {});
    assert_eq!(
        report.error.as_deref(),
        Some("Muse Code seemed stuck: nothing came from it for 3 seconds, so it was stopped."),
        "{report:?}"
    );
    store.add_user_message(&chat.id, "again", "turn-5").unwrap();
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-5", |_| {}).replies, 1);
    std::fs::remove_dir_all(store.root()).ok();
}

/// A chat on the installed pi, its model the scripted stand-in of tests/pi/scripted-provider.ts (no sign-in, nothing
/// called): Filo's real MCP server comes in through Filo's extension, the reply goes out with send_message, and the
/// next message resumes the session. `cargo test -p filo-local --test turn_e2e real_pi_scripted -- --ignored`.
#[test]
#[ignore]
fn real_pi_scripted_chat_turn_with_the_real_mcp_server() {
    let store = Store::at(common::temp_root("pi"));
    let (runtime, _) = stand_in(&store);
    let pi = engine::find_program(Engine::Pi, None).expect("pi is installed");
    let real = pi.canonicalize().unwrap_or(pi.clone());
    let bins: Vec<String> =
        [pi.parent(), real.parent()].into_iter().flatten().map(|dir| dir.display().to_string()).collect();
    let script = json!([
        [{ "tool": "mcp__filo__send_message", "args": { "text": "Hi from pi" } }],
        [{ "text": "sent" }],
        [{ "tool": "mcp__filo__send_message", "args": { "text": "Hi again" } }],
        [{ "text": "sent" }]
    ]);
    let provider = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pi/scripted-provider.ts");
    common::stand_in(
        &store.root().join("bin"),
        "pi",
        &format!(
            "#!/bin/sh\nexport PATH='{}':\"$PATH\"\nexport FILO_PI_SCRIPT='{script}'\nexec '{}' -e '{}' --session-dir '{}' \"$@\"\n",
            bins.join(":"),
            pi.display(),
            provider.display(),
            store.root().join("pi-sessions").display()
        ),
    );
    let chat = store.create_chat(Engine::Pi).unwrap();
    store.update_chat(&chat.id, |chat| chat.model = Some("filo-test/script".into())).unwrap();
    store.add_user_message(&chat.id, "hello pi", "turn-1").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-1", |_| {});
    assert_eq!(report, Report { replies: 1, stopped: false, error: None, ..Report::default() });
    let session = store.chat(&chat.id).unwrap().unwrap().session.expect("session kept");
    assert_eq!(session.engine, Engine::Pi);
    store.add_user_message(&chat.id, "again", "turn-2").unwrap();
    let report = turn::run(&store, &runtime, &chat.id, "turn-2", |_| {});
    assert_eq!(report.replies, 1, "{report:?}");
    assert_eq!(store.chat(&chat.id).unwrap().unwrap().session.unwrap().id, session.id);
    let replies: Vec<String> = store
        .messages(&chat.id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| message.text)
        .collect();
    assert_eq!(replies, ["Hi from pi", "Hi again"]);
    std::fs::remove_dir_all(store.root()).ok();
}

/// A real chat on the installed `FILO_REAL_ENGINE` (muse, opencode or pi; `FILO_REAL_MODEL` and `FILO_REAL_EFFORT` for its
/// model and reasoning level, `FILO_REAL_MODE` for its Coding page mode, Auto by default) with the real MCP server: it
/// runs a shell command and replies with Filo's send_message, then answers a follow-up in the same session. Uses the
/// tool's sign-in: `FILO_REAL_ENGINE=muse cargo test -p filo-local --test turn_e2e real_chat -- --ignored --nocapture`
/// (`FILO_REAL_LOG=<file>` keeps the chat).
#[test]
#[ignore]
fn real_chat_turns() {
    let engine =
        Engine::parse(&std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "muse".into())).expect("an engine");
    let store = Store::at(common::temp_root(&format!("real-chat-{}", engine.program())));
    let db_path = store.root().join(db::FILE);
    let db = Db::open(&db_path).unwrap();
    let tool = filo_local::coding::tool_of(engine);
    let mode = std::env::var("FILO_REAL_MODE").unwrap_or_else(|_| "auto".into());
    filo_local::coding::update_tool(&db, tool, |settings| {
        settings.mode = mode.clone();
        settings.effort = std::env::var("FILO_REAL_EFFORT").ok();
        Ok(())
    })
    .unwrap();
    let runtime = common::runtime(std::env::var("PATH").ok(), Some(db_path.clone()), Duration::from_secs(300));
    let chat = store.create_chat(engine).unwrap();
    store.update_chat(&chat.id, |chat| chat.model = std::env::var("FILO_REAL_MODEL").ok()).unwrap();
    let mut log = String::new();
    let mut ask = |turn: &str, text: &str| -> Report {
        store.add_user_message(&chat.id, text, turn).unwrap();
        // What shows beside the typing dots while it works.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let watching = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let watcher = {
            let (store, chat_id, turn_id, seen, watching) =
                (Store::at(store.root()), chat.id.clone(), turn.to_string(), seen.clone(), watching.clone());
            std::thread::spawn(move || {
                while watching.load(std::sync::atomic::Ordering::SeqCst) {
                    if let Some(label) = chat_activity::get(&store, &chat_id, &turn_id) {
                        let mut seen = seen.lock().unwrap();
                        if seen.last() != Some(&label) {
                            seen.push(label);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
        };
        let report = turn::run(&store, &runtime, &chat.id, turn, |_| {});
        watching.store(false, std::sync::atomic::Ordering::SeqCst);
        watcher.join().unwrap();
        let replies: Vec<String> = store
            .messages(&chat.id)
            .unwrap()
            .into_iter()
            .filter(|message| message.turn.as_deref() == Some(turn) && message.role != Role::User)
            .map(|message| format!("{:?}: {}", message.role, message.text))
            .collect();
        let session = store.chat(&chat.id).unwrap().unwrap().session.map(|session| session.id).unwrap_or_default();
        let line = format!(
            "> {text}\n{}\n(activity: {:?}; session {session}; {report:?})\n",
            replies.join("\n"),
            seen.lock().unwrap()
        );
        println!("{line}");
        log.push_str(&line);
        report
    };
    let first = ask(
        "turn-1",
        "Run the shell command `echo filo-$((6*7))` and reply to me with send_message, quoting exactly what it printed.",
    );
    let session = store.chat(&chat.id).unwrap().unwrap().session.expect("its session");
    assert_eq!(session.engine, engine);
    let second = ask("turn-2", "What number did that command print? Reply with send_message, just the number.");
    if let Ok(file) = std::env::var("FILO_REAL_LOG") {
        std::fs::write(
            file,
            format!("{} chat ({mode}, model {:?})\n\n{log}", engine.label(), std::env::var("FILO_REAL_MODEL").ok()),
        )
        .unwrap();
    }
    let messages = store.messages(&chat.id).unwrap();
    let reply = |turn: &str| {
        messages
            .iter()
            .filter(|message| message.role == Role::Assistant && message.turn.as_deref() == Some(turn))
            .map(|message| message.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(first.error.is_none() && first.replies >= 1, "{first:?}");
    assert!(reply("turn-1").contains("filo-42"), "{}", reply("turn-1"));
    assert!(second.error.is_none() && second.replies >= 1, "{second:?}");
    assert!(reply("turn-2").contains("42"), "{}", reply("turn-2"));
    // The follow-up carried on in the same session.
    assert_eq!(store.chat(&chat.id).unwrap().unwrap().session.unwrap().id, session.id);
    std::fs::remove_dir_all(store.root()).ok();
}

/// The reasoning level picked in the chat's composer reaches Claude Code (`--effort`), and a note shared into the chat
/// reaches the turn's prompt as material that came with the message.
#[test]
fn a_turn_thinks_as_hard_as_its_chat_says_and_reads_the_note_shared_with_it() {
    let store = Store::at(common::temp_root("effort"));
    let (runtime, _) = stand_in(&store);
    let chat = store.create_chat(Engine::Claude).unwrap();
    store.add_user_message(&chat.id, "how hard do you think?", "turn-1").unwrap();
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-1", |_| {}).replies, 1);
    store
        .update_chat(&chat.id, |chat| (chat.reasoning, chat.effort) = (Some("max".into()), Some("max".into())))
        .unwrap();
    store.add_user_message(&chat.id, "how hard now? add milk", "turn-2").unwrap();
    turn::save_context(
        &store,
        &chat.id,
        "turn-2",
        "A note the user shared from their Notes:\nTitle: Groceries\n\n- eggs",
    )
    .unwrap();
    assert_eq!(turn::run(&store, &runtime, &chat.id, "turn-2", |_| {}).replies, 1);
    let replies: Vec<serde_json::Value> = store
        .messages(&chat.id)
        .unwrap()
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .map(|message| serde_json::from_str(&message.text).unwrap())
        .collect();
    assert_eq!(replies, [json!({ "effort": null, "noted": false }), json!({ "effort": "max", "noted": true })]);
    std::fs::remove_dir_all(store.root()).ok();
}

/// A copy of a real chat whose session went cold, or is nearly full, starts its next turn on a fresh session seeded with
/// the chat's own messages (reseed.rs), on the installed `FILO_REAL_ENGINE` (claude or codex; `FILO_REAL_MODEL` for its
/// model). The chat in `FILO_REAL_CHAT` (a chat's folder, only read) is copied into `FILO_REAL_DIR`, a new folder, and
/// given a made-up session (`compaction::pretend_last_call`), so no real session is resumed or touched: its last call two
/// hours ago at 719k tokens, or with `FILO_REAL_WHY=full` five minutes ago and 150k short of its window (900k of Claude
/// Code's 1M, 400k of Codex's 475k: in use, but where its CLI would soon compact it). Then `FILO_REAL_ASK` is asked. It
/// logs the seed, the reply, how long the turn took and what its calls read and wrote, from the CLI's record of the new
/// session, for comparing with compaction, and checks the CLI didn't compact that session. Uses the tool's sign-in:
/// `FILO_REAL_ENGINE=codex FILO_REAL_CHAT=~/.config/filo/local/agents/chats/<id> FILO_REAL_DIR=/tmp/reseed-codex
/// FILO_REAL_ASK='…' cargo test -p filo-local --test turn_e2e real_reseed -- --ignored --nocapture` (`FILO_REAL_LOG=<file>`
/// keeps what happened; the seed is written beside the copy, `seed.txt`).
#[test]
#[ignore]
fn real_reseed_starts_fresh_from_the_chats_own_messages() {
    use filo_local::{compaction, reseed};
    let engine =
        Engine::parse(&std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "claude".into())).expect("an engine");
    let from = PathBuf::from(std::env::var("FILO_REAL_CHAT").expect("FILO_REAL_CHAT: the chat's folder to copy"));
    let root = PathBuf::from(std::env::var("FILO_REAL_DIR").expect("FILO_REAL_DIR: a new folder for the copy"));
    let ask = std::env::var("FILO_REAL_ASK").expect("FILO_REAL_ASK: what to ask");
    let full = std::env::var("FILO_REAL_WHY").is_ok_and(|why| why == "full");
    assert!(!root.exists(), "{} is there already: give a new folder", root.display());
    let say = |line: String| {
        println!("{line}");
        if let Ok(file) = std::env::var("FILO_REAL_LOG") {
            use std::io::Write as _;
            let mut log = std::fs::OpenOptions::new().create(true).append(true).open(file).unwrap();
            writeln!(log, "{line}").unwrap();
        }
    };

    // The copy: its chat.json and messages, nothing else; a made-up session that went cold two hours ago, or is full.
    let chat_json: serde_json::Value = serde_json::from_slice(&std::fs::read(from.join("chat.json")).unwrap()).unwrap();
    let id = chat_json["id"].as_str().expect("the chat's id").to_string();
    let dir = root.join("chats").join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    for file in ["chat.json", "messages.jsonl"] {
        std::fs::copy(from.join(file), dir.join(file)).unwrap();
    }
    let store = Store::at(root.clone());
    let original = store.chat(&id).unwrap().expect("the copied chat");
    let cold = format!("cold-{}", uuid::Uuid::new_v4());
    let model =
        std::env::var("FILO_REAL_MODEL").ok().or_else(|| original.model.clone().filter(|_| original.engine == engine));
    store
        .update_chat(&id, |chat| {
            chat.engine = engine;
            chat.model = model.clone();
            chat.session =
                Some(filo_local::store::Session { engine, id: cold.clone(), instructions: None, model: None });
        })
        .unwrap();
    let now = filo_local::now_ms() as i64;
    let last = match (full, engine) {
        (false, _) => compaction::LastCall { at_ms: now - 2 * 3_600_000, tokens: 718_745, window: None },
        (true, Engine::Codex) => {
            compaction::LastCall { at_ms: now - 5 * 60_000, tokens: 400_000, window: Some(475_000) }
        }
        (true, _) => compaction::LastCall { at_ms: now - 5 * 60_000, tokens: 900_000, window: None },
    };
    compaction::pretend_last_call(&cold, last);
    assert!(compaction::reseed_due(engine, &cold, now, last.window.or(Some(1_000_000))).is_some());
    // A full one isn't cold: only a chat's turn starts afresh, a coding round would resume it.
    assert_eq!(compaction::due(engine, &cold, now).is_none(), full);

    let turn = format!("reseed-check-{}", uuid::Uuid::new_v4());
    store.add_user_message(&id, &ask, &turn).unwrap();
    let messages = store.messages(&id).unwrap();
    let record = store.messages_file(&id).unwrap();
    let seed = turn::prompt_for(&messages, &turn, false, Some(&record));
    std::fs::write(root.join("seed.txt"), &seed).unwrap();
    say(format!(
        "{} on a copy of \"{}\" ({} messages, about {}k tokens of text): the seed has {} of them, about {}k tokens (seed.txt).",
        engine.label(),
        original.title,
        messages.len() - 1,
        messages.iter().map(|message| reseed::tokens(&message.text)).sum::<usize>() / 1000,
        seed.matches(" · message ").count(),
        reseed::tokens(&seed) / 1000
    ));

    let runtime = common::runtime(std::env::var("PATH").ok(), None, Duration::from_secs(600));
    let began = std::time::Instant::now();
    let report = turn::run(&store, &runtime, &id, &turn, |_| {});
    let took = began.elapsed().as_secs_f64();
    let chat = store.chat(&id).unwrap().unwrap();
    let reply = store
        .messages(&id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role == Role::Assistant && message.turn.as_deref() == Some(turn.as_str()))
        .map(|message| message.text)
        .collect::<Vec<_>>()
        .join("\n");
    let session = chat.session.clone().expect("the new session").id;
    let compactions = cli_record(engine, &session)
        .iter()
        .filter(|entry| entry["subtype"] == "compact_boundary" || entry["type"] == "compacted")
        .count();
    say(format!(
        "The turn took {took:.0} s; {report:?}.\nIts session: {session} (window {:?}), compacted {compactions} times. {}\nReplied:\n{reply}",
        compaction::Windows::load(store.root()).window_for(engine, model.as_deref().unwrap_or_default(), false),
        usage(engine, &session)
    ));
    assert!(report.error.is_none() && report.replies >= 1, "{report:?}");
    assert_ne!(session, cold, "it resumed the made-up session");
    assert!(chat.reseeded_turns.contains(&turn) && !chat.compacted_turns.contains(&turn));
    assert_eq!(compactions, 0, "its CLI compacted the new session");
}

/// How a real agent tells the user a finished coding round, on the installed `FILO_REAL_ENGINE` (claude or codex;
/// `FILO_REAL_MODEL` for its model): a copy of the database in `FILO_REAL_DB` (a filo.db, only read) gives round
/// `FILO_REAL_ROUND` (the latest by default) of task `FILO_REAL_TASK`, and a copy of its conversation's chat
/// (`FILO_REAL_CHAT`, the chat's folder, only read) up to that round's end is woken with the round's news as
/// coding_handoff.rs words it now, in a fresh session seeded with the chat's messages. Both copies go into
/// `FILO_REAL_DIR`, a new folder. It logs the reply, for reading against the one the user got. Uses the tool's sign-in:
/// `FILO_REAL_ENGINE=codex FILO_REAL_DB=/tmp/filo.db FILO_REAL_TASK=<id> FILO_REAL_CHAT=~/.config/filo/local/agents/chats/<id>
/// FILO_REAL_DIR=/tmp/round-codex cargo test -p filo-local --test turn_e2e real_round_report -- --ignored --nocapture`
/// (`FILO_REAL_LOG=<file>` keeps the reply). Only Filo's tools work on the copies: the agent's own shell and file tools
/// run unsandboxed, as in the app, and can reach the task's real project folder, so run it only for a task whose project
/// it may look around in (a throwaway checkout, or one you'd let it change).
#[test]
#[ignore]
fn real_round_report_tells_the_user_what_changed() {
    use filo_local::{coding, coding_handoff as handoff};
    let engine =
        Engine::parse(&std::env::var("FILO_REAL_ENGINE").unwrap_or_else(|_| "claude".into())).expect("an engine");
    let db_from = PathBuf::from(std::env::var("FILO_REAL_DB").expect("FILO_REAL_DB: a filo.db to copy"));
    let task_id = std::env::var("FILO_REAL_TASK").expect("FILO_REAL_TASK: the task whose round to tell");
    let from =
        PathBuf::from(std::env::var("FILO_REAL_CHAT").expect("FILO_REAL_CHAT: its conversation's chat folder to copy"));
    let root = PathBuf::from(std::env::var("FILO_REAL_DIR").expect("FILO_REAL_DIR: a new folder for the copies"));
    assert!(!root.exists(), "{} is there already: give a new folder", root.display());
    std::fs::create_dir_all(&root).unwrap();
    let db_path = root.join(db::FILE);
    std::fs::copy(&db_from, &db_path).unwrap();
    let db = Db::open(&db_path).unwrap();
    let task = coding::task(&db, &task_id).unwrap().expect("the task");
    let owner = handoff::get(&db, &task_id).unwrap().expect("its hand-off");
    let ended = match std::env::var("FILO_REAL_ROUND") {
        Ok(round) => handoff::round(&db, &task_id, round.parse().unwrap()).unwrap(),
        Err(_) => handoff::latest_round(&db, &task_id).unwrap(),
    }
    .expect("the round");
    let wake = handoff::round_message(&task, &owner, &ended, &handoff::round_proof(&db, &ended).unwrap(), &[]);

    // The chat as it was when the round ended, without a session: the turn starts fresh from its messages.
    let chat_json: serde_json::Value = serde_json::from_slice(&std::fs::read(from.join("chat.json")).unwrap()).unwrap();
    let id = chat_json["id"].as_str().expect("the chat's id").to_string();
    let dir = root.join("chats").join(&id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(from.join("chat.json"), dir.join("chat.json")).unwrap();
    let until = ended.finished_at.expect("a finished round");
    let history = std::fs::read_to_string(from.join("messages.jsonl")).unwrap();
    let before: Vec<&str> = history
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|message| message["at"].as_i64().is_some_and(|at| at < until))
        })
        .collect();
    std::fs::write(dir.join("messages.jsonl"), before.join("\n") + "\n").unwrap();
    let store = Store::at(root.clone());
    let model = std::env::var("FILO_REAL_MODEL")
        .ok()
        .or_else(|| store.chat(&id).unwrap().expect("the copied chat").model.filter(|_| engine == Engine::Claude));
    store
        .update_chat(&id, |chat| {
            chat.engine = engine;
            chat.model = model.clone();
            chat.session = None;
        })
        .unwrap();

    let turn = format!("round-report-{}", uuid::Uuid::new_v4());
    store.add_hidden_message(&id, &wake, &turn, None).unwrap();
    let runtime = common::runtime(std::env::var("PATH").ok(), Some(db_path), Duration::from_secs(600));
    let began = std::time::Instant::now();
    let report = turn::run(&store, &runtime, &id, &turn, |_| {});
    let reply = store
        .messages(&id)
        .unwrap()
        .into_iter()
        .filter(|message| message.role == Role::Assistant && message.turn.as_deref() == Some(turn.as_str()))
        .map(|message| message.text)
        .collect::<Vec<_>>()
        .join("\n\n");
    let line = format!(
        "{} ({model:?}) on \"{}\" round {}, from a copy of {} messages; the turn took {:.0} s, {report:?}.\nReplied:\n{reply}\n",
        engine.label(),
        task.title,
        ended.round,
        before.len(),
        began.elapsed().as_secs_f64()
    );
    println!("{line}");
    if let Ok(file) = std::env::var("FILO_REAL_LOG") {
        std::fs::write(file, format!("{line}\nThe news it was given:\n{wake}\n")).unwrap();
    }
    assert!(report.error.is_none() && report.replies >= 1, "{report:?}");
}

/// A session's entries, from its CLI's record on this Mac (Claude Code's session file, Codex's rollout).
fn cli_record(engine: Engine, session: &str) -> Vec<serde_json::Value> {
    let find = |dir: PathBuf, ending: &str| -> Option<PathBuf> {
        let mut stack = vec![dir];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(ending)) {
                    return Some(path);
                }
            }
        }
        None
    };
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let file = match engine {
        Engine::Claude => find(
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"))
                .join("projects"),
            &format!("{session}.jsonl"),
        ),
        _ => find(
            std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex")).join("sessions"),
            &format!("-{session}.jsonl"),
        ),
    };
    file.and_then(|file| std::fs::read_to_string(file).ok())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What a session's calls read and wrote, from its CLI's record on this Mac: Claude Code's each call's prompt (new to
/// the cache, read from it) and output; Codex's totals.
fn usage(engine: Engine, session: &str) -> String {
    match engine {
        Engine::Claude => {
            let mut seen = std::collections::HashSet::new();
            let calls: Vec<String> = cli_record(engine, session)
                .into_iter()
                .filter(|entry| {
                    entry["type"] == "assistant"
                        && entry["isSidechain"] != true
                        && entry["message"]["model"] != "<synthetic>"
                })
                .filter(|entry| seen.insert(entry["message"]["id"].to_string()))
                .map(|entry| {
                    let usage = &entry["message"]["usage"];
                    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
                    format!(
                        "{}k new + {}k cached in, {} out",
                        (count("input_tokens") + count("cache_creation_input_tokens")) / 1000,
                        count("cache_read_input_tokens") / 1000,
                        count("output_tokens")
                    )
                })
                .collect();
            format!("{} calls: {}", calls.len(), calls.join("; "))
        }
        _ => {
            let counts: Vec<serde_json::Value> = cli_record(engine, session)
                .into_iter()
                .filter(|entry| entry["payload"]["type"] == "token_count" && entry["payload"]["info"].is_object())
                .collect();
            let first = counts
                .first()
                .map(|entry| entry["payload"]["info"]["last_token_usage"].to_string())
                .unwrap_or_default();
            let total = counts
                .last()
                .map(|entry| entry["payload"]["info"]["total_token_usage"].to_string())
                .unwrap_or_default();
            format!("{} calls; the first {first}; in all {total}", counts.len())
        }
    }
}

// A thread in the agents' group chat with a stand-in `claude`: it starts the real MCP server program (`filo-local mcp
// --group-speaker …`) the way Claude Code would and posts with send_message. The user names no one, so Filo answers
// and asks Chef in send_message's `ask`, whose turn comes next and reads Filo's question in the group's transcript;
// Chef asks nobody (its "@Filo" is just a name), so the thread ends. Everything is in Filo's database, so a reopened one reads the same. "@everyone" has every member check
// the message at the same time, and only the one it's for answers; the rest end their turn quietly, which isn't a
// failure.

mod common;

use std::time::Duration;

use filo_local::apps::FILO_AGENT as FILO;
use filo_local::db::{self, Db};
use filo_local::group::{self, Member, Setup};
use filo_local::{Engine, Role};

const FAKE_CLAUDE: &str = r##"#!/usr/bin/env python3
import json, re, subprocess, sys
args = sys.argv[1:]
config = json.loads(args[args.index("--mcp-config") + 1])["mcpServers"]["filo"]
system = args[args.index("--append-system-prompt") + 1]
# Claude Code reads its stream-json input as it comes, and stdin stays open while the turn runs: read up to the message.
lines = []
for line in iter(sys.stdin.readline, ""):
    if line.strip():
        lines.append(json.loads(line))
        if lines[-1]["type"] == "user":
            break
init = [line for line in lines if line["type"] == "control_request"][0]["request"]
prompt = [line for line in lines if line["type"] == "user"][0]["message"]["content"][0]["text"]
me = re.search(r"^- @(.+?) \(you\):", prompt, re.M).group(1)
server = subprocess.Popen([config["command"]] + config["args"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def call(id, method, params):
    server.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    server.stdin.flush()
    return json.loads(server.stdout.readline())
call(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake", "version": "0"}})
if "--session-id" in args or "--resume" in args:
    text = "a group turn shouldn't resume a session"
elif "Every member gets it at the same time" in prompt:
    # Only Chef works on voice messages; the others have nothing to add.
    text = "Voice messages are mine." if me == "Chef" and "Answer only if it's for you" in prompt else ""
elif me == "Filo":
    text, ask = "Chef, what's for dinner?", ["Chef"]
else:
    saw = "] User: plan dinner" in prompt and "] Filo: Chef, what's for dinner?" in prompt and "You are Chef" in system and "send_message's `ask`" in prompt
    text = "Pasta, as @Filo asked. (saw the thread: %s)" % saw
# Asked to stay quiet now (not in an earlier message of the transcript).
if text and "quiet" not in prompt.split("Why you're speaking now:")[1]:
    arguments = {"text": text, "ask": ask} if me == "Filo" and text.startswith("Chef") else {"text": text}
    posted = call(2, "tools/call", {"name": "send_message", "arguments": arguments})
    assert not posted["result"]["isError"], posted
server.stdin.close()
server.wait()
print(json.dumps({"type": "system", "subtype": "init", "session_id": "fake-" + me}))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "plain text", "session_id": "fake-" + me}))
"##;

#[test]
fn filo_asks_an_agent_and_the_agent_answers_with_the_thread_in_view() {
    let root = common::temp_root("group");
    let bin = root.join("bin");
    common::stand_in(&bin, "claude", FAKE_CLAUDE);
    let db_path = root.join(db::FILE);
    let runtime = common::runtime(Some(common::path_with(&bin)), Some(db_path.clone()), Duration::from_secs(30));
    let members = vec![
        Member { id: FILO.into(), name: "Filo".into(), about: String::new() },
        Member { id: "chef".into(), name: "Chef".into(), about: "Cooks.".into() },
    ];

    let db = Db::open(&db_path).unwrap();
    group::ensure(&db, 1).unwrap();
    // The MCP server reads the group from the database: Chef is one of the user's agents.
    let chef = filo_local::app_data::Agent {
        id: "chef".into(),
        name: "Chef".into(),
        instructions: "Cooks.".into(),
        picture: String::new(),
        background: None,
        session_id: None,
        engine: None,
        model: None,
        earlier_chat: None,
        created_at: 1.0,
    };
    filo_local::app_data::add_agent(&db, &chef).unwrap();
    group::add_user_message(&db, "thread-1", "plan dinner", 1).unwrap();
    assert_eq!(group::start_thread(&db, "thread-1", "plan dinner", None, &members, 1).unwrap(), [FILO]);

    let mut spoke = Vec::new();
    while let Some(turn) = group::claim_next(&db, filo_local::now_ms() as i64).unwrap() {
        let member = members.iter().find(|member| member.id == turn.speaker).unwrap();
        let setup = Setup {
            engine: Engine::Claude,
            model: None,
            instructions: group::instructions(member, &members),
            prompt: group::setup_prompt(&turn, &members, &group::Work::new(), &db, "", filo_local::now_ms() as i64)
                .unwrap(),
            workspace: root.join("workspace"),
            chat_id: "own-chat".into(),
            store_root: root.clone(),
        };
        let report = group::run(&db, &db_path, &runtime, &setup, &turn, |_| {});
        assert_eq!((report.replies, report.error.as_deref()), (1, None), "{report:?}");
        group::finish(&db, &turn.id, "completed", None, 2).unwrap();
        group::follow(&db, &turn, &members, 2).unwrap();
        spoke.push(turn.speaker);
        assert!(spoke.len() < 5, "the thread should end");
    }
    assert_eq!(spoke, [FILO, "chef"]);

    // A reopened database (the app after a restart) has the thread.
    drop(db);
    let db = Db::open(&db_path).unwrap();
    let (messages, more) = group::messages(&db, 10).unwrap();
    let shape: Vec<(Role, Option<&str>, &str)> =
        messages.iter().map(|m| (m.role, m.speaker.as_deref(), m.text.as_str())).collect();
    assert_eq!(
        shape,
        [
            (Role::User, None, "plan dinner"),
            (Role::Assistant, Some(FILO), "Chef, what's for dinner?"),
            (Role::Assistant, Some("chef"), "Pasta, as @Filo asked. (saw the thread: True)"),
        ]
    );
    assert!(!more);
    assert!(group::turns_of(&db, "thread-1").unwrap().iter().all(|turn| turn.state == "completed"));

    // Asked to answer the user and posting nothing is a failure; only chiming in, it's fine.
    group::add_user_message(&db, "thread-2", "@Chef stay quiet", 3).unwrap();
    group::start_thread(&db, "thread-2", "@Chef stay quiet", None, &members, 3).unwrap();
    let turn = group::claim_next(&db, 4).unwrap().unwrap();
    let setup = Setup {
        engine: Engine::Claude,
        model: None,
        instructions: group::instructions(&members[1], &members),
        prompt: group::setup_prompt(&turn, &members, &group::Work::new(), &db, "", 4).unwrap(),
        workspace: root.join("workspace"),
        chat_id: "own-chat".into(),
        store_root: root.clone(),
    };
    let report = group::run(&db, &db_path, &runtime, &setup, &turn, |_| {});
    assert!(report.error.as_deref().is_some_and(|error| error.contains("without posting")), "{report:?}");
    let chiming = group::Turn { answers_user: false, ..turn.clone() };
    assert_eq!(group::run(&db, &db_path, &runtime, &setup, &chiming, |_| {}).error, None);
    group::finish(&db, &turn.id, "completed", None, 5).unwrap();

    // "@everyone": every member checks it at the same time, Filo has nothing to add and says nothing, Chef answers.
    let everyone = "@everyone which one of you works on voice messages?";
    group::add_user_message(&db, "thread-3", everyone, 6).unwrap();
    assert_eq!(group::start_thread(&db, "thread-3", everyone, None, &members, 6).unwrap(), [FILO, "chef"]);
    let first = group::claim_next(&db, 7).unwrap().unwrap();
    let mut turns = group::claim_alongside(&db, &first, 7).unwrap();
    turns.insert(0, first);
    assert!(group::claim_next(&db, 7).unwrap().is_none());
    let mut checked: Vec<(String, usize)> = std::thread::scope(|scope| {
        let running: Vec<_> = turns
            .iter()
            .map(|turn| {
                let member = members.iter().find(|member| member.id == turn.speaker).unwrap();
                let setup = Setup {
                    engine: Engine::Claude,
                    model: None,
                    instructions: group::instructions(member, &members),
                    prompt: group::setup_prompt(turn, &members, &group::Work::new(), &db, "", 7).unwrap(),
                    workspace: root.join("workspace"),
                    chat_id: "own-chat".into(),
                    store_root: root.clone(),
                };
                let (db, db_path, runtime) = (&db, &db_path, &runtime);
                scope.spawn(move || (turn.speaker.clone(), group::run(db, db_path, runtime, &setup, turn, |_| {})))
            })
            .collect();
        running
            .into_iter()
            .map(|running| running.join().unwrap())
            .map(|(speaker, report)| {
                assert_eq!(report.error, None, "{report:?}");
                (speaker, report.replies)
            })
            .collect()
    });
    for turn in &turns {
        group::finish(&db, &turn.id, "completed", None, 8).unwrap();
        group::follow(&db, turn, &members, 8).unwrap();
    }
    checked.sort();
    assert_eq!(checked, [(FILO.to_string(), 0), ("chef".to_string(), 1)]);
    let said: Vec<(Option<String>, String)> =
        group::thread_messages(&db, "thread-3").unwrap().into_iter().map(|m| (m.speaker, m.text)).collect();
    assert_eq!(
        said,
        [(None, everyone.to_string()), (Some("chef".to_string()), "Voice messages are mine.".to_string())]
    );
    assert!(
        group::turns_of(&db, "thread-3").unwrap().iter().all(|turn| turn.state == "completed" && turn.error.is_none())
    );
    assert!(group::failures(&db).unwrap().is_empty());
    drop(db);
    std::fs::remove_dir_all(&root).ok();
}

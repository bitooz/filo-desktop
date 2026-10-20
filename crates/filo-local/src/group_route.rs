// Who answers a post in the group that names no one: one quick, cheap call to the model the user picked in the group's
// settings (group.rs `Settings::route`; none: the cheapest fast one of the tools there are, `cheapest_first`). It reads
// the new post, the group's recent messages and what each member is working on, and answers with the members it's for,
// or nobody (`prompt`, `parse`). The call is the tool's own one-off command line with no tools of its own (`call`).
//
// A post waiting to be routed is a row in `group_routes` (`queue`), so the group shows "Picking who answers" until it's
// done (local_backend/group.rs runs it, and again after a restart). @Names, @everyone and the chat's Reply skip it: they
// say who answers themselves (group.rs `needs_route`).

use std::path::Path;
use std::time::Duration;

use chrono::TimeZone;
use rusqlite::{OptionalExtension as _, params};

use crate::db::Db;
use crate::engine::{Engine, ModelOption};
use crate::group::{GroupMessage, Member, Work};

/// The table (group.rs `ensure_columns` makes it).
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS group_routes (
    thread     TEXT PRIMARY KEY NOT NULL,
    state      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    note       TEXT,
    picked     TEXT
);
";

/// How hard the routing model thinks when the user picked no level (group.rs `RouteModel::reasoning`).
pub const DEFAULT_REASONING: &str = "low";
/// How long the call may take before the main agent takes the post instead.
pub const TIMEOUT: Duration = Duration::from_secs(90);
/// How much of each member's instructions the call reads.
const ABOUT_CHARS: usize = 240;

/// The user's post in `thread` waits for the call; `note` is what its speakers are told besides (group.rs `route`).
pub fn queue(db: &Db, thread: &str, note: Option<&str>, now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute(
            "INSERT OR IGNORE INTO group_routes (thread, state, created_at, note) VALUES (?1, 'queued', ?2, ?3)",
            params![thread, now, note],
        )
    })
    .map(drop)
}

/// Takes a waiting post's thread to route, with its note (once: whoever claims it routes it).
pub fn claim(db: &Db) -> Result<Option<(String, Option<String>)>, String> {
    db.write(|tx| {
        tx.query_row(
            "UPDATE group_routes SET state = 'running' WHERE thread = (SELECT thread FROM group_routes WHERE state = 'queued' ORDER BY created_at LIMIT 1) RETURNING thread, note",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
    })
}

/// Routes the app left running when it closed wait again.
pub fn requeue_running(db: &Db) -> Result<usize, String> {
    db.write(|tx| tx.execute("UPDATE group_routes SET state = 'queued' WHERE state = 'running'", []))
}

/// Whether `thread`'s post is still waiting for the call or in it.
pub fn routing(db: &Db, thread: &str) -> bool {
    db.with(|conn| {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM group_routes WHERE thread = ?1 AND state IN ('queued', 'running'))",
            params![thread],
            |row| row.get(0),
        )
    })
    .unwrap_or(false)
}

/// The call picked `picked` (agent ids; empty: nobody) for `thread`.
pub fn finish(db: &Db, thread: &str, picked: &[String]) -> Result<(), String> {
    let picked = serde_json::to_string(picked).unwrap_or_else(|_| "[]".into());
    db.write(|tx| {
        tx.execute("UPDATE group_routes SET state = 'done', picked = ?2 WHERE thread = ?1", params![thread, picked])
    })
    .map(drop)
}

// --- The models it can use ---

/// Words in a model's name that say it's a small, fast or free one, and a big, slow one.
const SMALL: [&str; 12] =
    ["haiku", "luna", "mini", "nano", "flash", "lite", "small", "spark", "lightning", "fast", "tiny", "free"];
const BIG: [&str; 8] = ["opus", "fable", "astra", "pro", "ultra", "max", "large", "wonder"];

/// How costly a model reads by its name: 0 small and fast, 1 the middle, 2 the biggest.
fn tier(option: &ModelOption) -> u8 {
    let name = format!("{} {}", option.id, option.label).to_lowercase();
    let words: Vec<&str> = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|word| !word.is_empty()).collect();
    if words.iter().any(|word| SMALL.contains(word)) {
        0
    } else if words.iter().any(|word| BIG.contains(word)) {
        2
    } else {
        1
    }
}

/// `rows` (one tool's or several, in the picker's order) with the cheapest fast models first; the tool's own order
/// otherwise, newest first.
fn cheapest_first(rows: &[ModelOption]) -> Vec<ModelOption> {
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(tier);
    sorted
}

/// The model routing uses when none was picked (or the one picked is gone): the cheapest of the first tool in `ready`
/// (the tools installed and signed in, in the app's order) that has models listed.
pub fn default_model(rows: &[ModelOption], ready: &[Engine]) -> Option<crate::group::RouteModel> {
    ready.iter().find_map(|engine| {
        let own: Vec<ModelOption> = rows.iter().filter(|row| row.engine == *engine).cloned().collect();
        cheapest_first(&own).into_iter().next().map(|row| crate::group::RouteModel {
            engine: *engine,
            model: row.id,
            reasoning: None,
        })
    })
}

// --- The call ---

/// What the model reads: the members (what each does and is busy with), the recent messages, the new post, and how to
/// answer.
pub fn prompt<Tz: TimeZone>(
    message: &str,
    messages: &[GroupMessage],
    members: &[Member],
    work: &Work,
    group_name: Option<&str>,
    tz: &Tz,
    now: i64,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let lead = crate::group::leader(members);
    let roster: Vec<String> = members
        .iter()
        .map(|member| {
            let main = if member.id == lead { " (the user's main agent)" } else { "" };
            let about = crate::store::cut(member.about.trim(), ABOUT_CHARS);
            let doing: String =
                work.get(&member.id).into_iter().flatten().map(|line| format!("\n  - {line}")).collect();
            format!("- {}{main}: {about}{doing}", member.name)
        })
        .collect();
    let group = group_name.map_or_else(|| "the group chat".to_string(), |name| format!("\"{name}\", the group chat"));
    let quoted = message.trim();
    [
        format!("You pick who answers the user's new post in {group} of their agents in the Filo app. You don't answer it yourself."),
        format!("Members:\n{}", roster.join("\n")),
        format!("Recent messages:\n{}", crate::group::transcript(messages, members, tz, now)),
        format!("The user's new post:\n\"{quoted}\""),
        format!(
            "Pick the members it's for: whoever's job or current work it's about, or who was just talking with the user about it. Usually one. \
If it's for no one in particular, pick {}. Pick nobody when it needs no answer (thanks, ok, a note to themselves).\n\n\
Reply with JSON only, no other text: {{\"answer\": [\"Name\"]}}, names exactly as under Members, or {{\"answer\": []}} for nobody.",
            crate::group::speaker_name(lead, members)
        ),
    ]
    .join("\n\n")
}

/// The members the model's answer `text` picks, by agent id, in its order (empty: nobody); none when the answer can't
/// be read. Names nobody has are left out.
pub fn parse(text: &str, members: &[Member]) -> Option<Vec<String>> {
    let value = json_in(text)?;
    let names = value.get("answer")?.as_array()?;
    let mut picked: Vec<String> = Vec::new();
    for name in names.iter().filter_map(serde_json::Value::as_str) {
        let name = name.trim().trim_start_matches('@').trim();
        let found = members.iter().find(|member| member.name.trim().eq_ignore_ascii_case(name)).or_else(|| {
            name.eq_ignore_ascii_case("filo")
                .then(|| members.iter().find(|member| member.id == crate::apps::FILO_AGENT))
                .flatten()
        });
        if let Some(member) = found.filter(|member| !picked.contains(&member.id)) {
            picked.push(member.id.clone());
        }
    }
    Some(picked)
}

/// The first JSON object with an "answer" in `text` (models wrap it in a code block or a sentence now and then).
fn json_in(text: &str) -> Option<serde_json::Value> {
    let mut from = 0;
    while let Some(start) = text[from..].find('{').map(|at| at + from) {
        let mut depth = 0;
        for (offset, c) in text[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[start..=start + offset])
                            && value.get("answer").is_some()
                        {
                            return Some(value);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        from = start + 1;
    }
    None
}

/// Asks `model` on `engine` (its CLI `program`) once, with no tools, in the empty folder `workspace`, thinking at
/// `reasoning` (a level the model takes; none: the tool's own): what it answered.
pub fn call(
    engine: Engine,
    program: &Path,
    model: &str,
    reasoning: Option<&str>,
    prompt: &str,
    path_env: Option<&str>,
    workspace: &Path,
) -> Result<String, String> {
    std::fs::create_dir_all(workspace).map_err(|error| format!("Couldn't make the routing folder: {error}"))?;
    let mut command = crate::engine::cli_command(program, workspace, path_env, false);
    let mut stdin = Some(prompt);
    let answer_file = workspace.join(format!("answer-{}.txt", uuid::Uuid::new_v4()));
    let prompt_file = workspace.join(format!("prompt-{}.txt", uuid::Uuid::new_v4()));
    match engine {
        Engine::Claude => {
            command.args(["-p", "--model", model, "--tools", "", "--strict-mcp-config", "--output-format", "text"]);
            if let Some(level) = reasoning {
                command.args(["--effort", level]);
            }
            command.env("CLAUDE_CODE_PROMPT_CACHE_TTL", "5m");
        }
        Engine::Codex => {
            command.args([
                "exec",
                "--skip-git-repo-check",
                "--ignore-user-config",
                "--ignore-rules",
                "-s",
                "read-only",
                "-m",
                model,
            ]);
            if let Some(level) = reasoning {
                command.arg("-c").arg(format!("model_reasoning_effort=\"{level}\""));
            }
            command.arg("--output-last-message").arg(&answer_file).arg("-");
        }
        Engine::Muse => {
            std::fs::write(&prompt_file, prompt)
                .map_err(|error| format!("Couldn't write the routing prompt: {error}"))?;
            command.args(["exec", "--preset", "empty", "--model", model]);
            if let Some(level) = reasoning {
                command.args(["--reasoning-effort", level]);
            }
            command.arg("--prompt-file").arg(&prompt_file);
            stdin = None;
        }
        Engine::OpenCode => {
            // Its level is the model's variant, `provider/model#variant`, unless the model names one.
            let model = match reasoning.filter(|_| !model.contains('#')) {
                Some(level) => format!("{model}#{level}"),
                None => model.to_string(),
            };
            command.args(["run", "-m", &model, prompt]);
            stdin = None;
        }
        Engine::Pi => {
            command.args([
                "-p",
                "--no-session",
                "--no-tools",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
            ]);
            command.args(["--no-context-files", "--model", model]);
            if let Some(level) = reasoning {
                command.args(["--thinking", level]);
            }
            command.args(["--", prompt]).env("PI_OFFLINE", "1");
            stdin = None;
        }
    }
    let ran = crate::engine::run_for(&mut command, stdin, TIMEOUT);
    let _ = std::fs::remove_file(&prompt_file);
    let ran = ran.map_err(|error| format!("Couldn't start {}: {error}", engine.label()))?;
    let answer = std::fs::read_to_string(&answer_file).ok();
    let _ = std::fs::remove_file(&answer_file);
    match ran.status {
        None => Err(format!("{} took too long to pick who answers.", engine.label())),
        Some(status) if !status.success() => {
            let stderr = String::from_utf8_lossy(&ran.stderr);
            Err(format!(
                "{} couldn't pick who answers: {}",
                engine.label(),
                stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("it stopped").trim()
            ))
        }
        Some(_) => Ok(answer.unwrap_or_else(|| String::from_utf8_lossy(&ran.stdout).into_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(engine: Engine, id: &str, label: &str) -> ModelOption {
        ModelOption { engine, id: id.into(), label: label.into(), default: false, aliases: Vec::new() }
    }

    fn members() -> Vec<Member> {
        vec![
            Member { id: crate::apps::FILO_AGENT.into(), name: "Filo".into(), about: String::new() },
            Member { id: "dev".into(), name: "Developer".into(), about: "Writes the app's code.".into() },
            Member { id: "rm".into(), name: "Release Manager".into(), about: "Ships builds.".into() },
        ]
    }

    #[test]
    fn the_cheapest_fast_models_come_first_and_the_first_tools_cheapest_is_the_default() {
        let claude = vec![
            option(Engine::Claude, "fable", "Fable 5.1"),
            option(Engine::Claude, "claude-opus-5-5", "Opus 5.5"),
            option(Engine::Claude, "sonnet", "Sonnet 5.5"),
            option(Engine::Claude, "haiku", "Haiku 5.5"),
        ];
        let labels: Vec<String> = cheapest_first(&claude).into_iter().map(|row| row.label).collect();
        assert_eq!(labels, ["Haiku 5.5", "Sonnet 5.5", "Fable 5.1", "Opus 5.5"]);
        let codex = vec![
            option(Engine::Codex, "gpt-6.1-sol", "GPT-6.1-Sol"),
            option(Engine::Codex, "gpt-6-astra", "GPT-6-Astra"),
            option(Engine::Codex, "gpt-6-luna", "GPT-6-Luna"),
        ];
        let labels: Vec<String> = cheapest_first(&codex).into_iter().map(|row| row.label).collect();
        assert_eq!(labels, ["GPT-6-Luna", "GPT-6.1-Sol", "GPT-6-Astra"]);
        let rows: Vec<ModelOption> = claude.iter().chain(&codex).cloned().collect();
        assert_eq!(
            default_model(&rows, &[Engine::Claude, Engine::Codex]).map(|route| route.model).as_deref(),
            Some("haiku")
        );
        // Only Codex signed in: its cheapest.
        assert_eq!(default_model(&rows, &[Engine::Codex]).map(|route| route.model).as_deref(), Some("gpt-6-luna"));
        assert_eq!(default_model(&rows, &[]), None);
    }

    #[test]
    fn the_answer_names_members_or_nobody() {
        let members = members();
        assert_eq!(parse("{\"answer\": [\"Developer\"]}", &members), Some(vec!["dev".to_string()]));
        assert_eq!(
            parse(
                "Sure:\n```json\n{\"answer\": [\"release manager\", \"@Developer\", \"Nobody Here\"]}\n```",
                &members
            ),
            Some(vec!["rm".to_string(), "dev".to_string()])
        );
        assert_eq!(parse("{\"answer\": []}", &members), Some(Vec::new()));
        assert_eq!(parse("{\"answer\": [\"Filo\"]}", &members), Some(vec![crate::apps::FILO_AGENT.to_string()]));
        assert_eq!(parse("I think Developer.", &members), None);
        assert_eq!(parse("{\"who\": 1} then {\"answer\": [\"Developer\"]}", &members), Some(vec!["dev".to_string()]));
    }

    #[test]
    fn the_prompt_has_the_post_the_members_and_their_work() {
        let members = members();
        let mut work = Work::new();
        work.insert("dev".into(), vec!["Coding task \"Group settings\" in filo: running".into()]);
        let text = prompt("Can build 18 go out tonight?", &[], &members, &work, Some("Team"), &chrono::Utc, 0);
        assert!(text.contains("\"Can build 18 go out tonight?\""));
        assert!(text.contains("- Release Manager: Ships builds."));
        assert!(text.contains("Coding task \"Group settings\""));
        assert!(text.contains("\"Team\", the group chat"));
        assert!(text.contains("pick Filo"));
    }

    #[test]
    fn the_call_thinks_at_the_level_picked_on_every_tool() {
        let dir = crate::test_support::temp_dir("route-call");
        // Says what it was started with, as the answer.
        let program = crate::test_support::script(&dir, "tool", "#!/bin/sh\necho \"$@\"\n");
        let workspace = dir.join("route");
        let asked = |engine, model, reasoning| {
            call(engine, &program, model, reasoning, "Who answers?", None, &workspace).unwrap()
        };
        assert!(
            asked(Engine::Claude, "haiku", Some("medium"))
                .contains("--model haiku --tools  --strict-mcp-config --output-format text --effort medium")
        );
        assert!(!asked(Engine::Claude, "haiku", None).contains("--effort"));
        assert!(
            asked(Engine::Codex, "gpt-6-luna", Some("low"))
                .contains("-m gpt-6-luna -c model_reasoning_effort=\"low\" --output-last-message")
        );
        assert!(
            asked(Engine::Muse, "muse-spark-1.3", Some("minimal"))
                .contains("--model muse-spark-1.3 --reasoning-effort minimal --prompt-file")
        );
        assert!(
            asked(Engine::OpenCode, "openrouter/muse-wonder", Some("high"))
                .contains("run -m openrouter/muse-wonder#high Who answers?")
        );
        // A model that names its variant keeps it.
        assert!(
            asked(Engine::OpenCode, "openrouter/muse-wonder#max", Some("high"))
                .contains("-m openrouter/muse-wonder#max ")
        );
    }

    #[test]
    fn a_post_waits_for_the_call_until_its_done() {
        let db = Db::in_memory().unwrap();
        queue(&db, "t1", Some("From the artifact page."), 1).unwrap();
        assert!(routing(&db, "t1"));
        assert_eq!(claim(&db).unwrap(), Some(("t1".to_string(), Some("From the artifact page.".to_string()))));
        assert_eq!(claim(&db).unwrap(), None);
        // The app closed while it ran: it waits again.
        assert_eq!(requeue_running(&db).unwrap(), 1);
        assert_eq!(claim(&db).unwrap().map(|(thread, _)| thread).as_deref(), Some("t1"));
        finish(&db, "t1", &["dev".to_string()]).unwrap();
        assert!(!routing(&db, "t1"));
    }
}

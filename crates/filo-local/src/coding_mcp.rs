// The tools that hand coding work from a conversation to a coding tool on this Mac (Claude Code, Codex, Muse Code or
// OpenCode), and follow it: in every Filo chat, agent conversation and group turn (mcp.rs).
//
// - `list_coding_projects`: which tools are plugged in and installed, and the project folders.
// - `start_work`: a task in a project with a tool (the one asked for, else the conversation's own engine), from a calm
//   brief (goal, done when, notes); `continues` adds to a task instead. The app runs it (src/local_backend/coding.rs)
//   and tells this conversation when the tool asks something or finishes a round (coding_handoff.rs).
// - `watch_coding_task`, `tell_coding_task` (a note while it works, the next round once it's done), `stop_coding_task`.
// - `answer_coding_request`, `ask_user_about_coding_request`: what the tool asked, answered here or handed to the user
//   as a card with answer buttons.
//
// Everything goes through Filo's database, which the app watches; nothing here runs a tool itself.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::coding::{self, NewTask, Project};
use crate::coding_handoff::{self as handoff, NewHandoff};
use crate::db::Db;
use crate::engine::{self, Engine};
use crate::mcp::Target;

const NAMES: [&str; 7] = [
    "list_coding_projects",
    "start_work",
    "watch_coding_task",
    "tell_coding_task",
    "stop_coding_task",
    "answer_coding_request",
    "ask_user_about_coding_request",
];

/// Lengths the brief's parts are held to.
const TITLE_MAX: usize = 80;
const GOAL_MAX: usize = 600;
const NOTES_MAX: usize = 600;

pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn tools() -> Vec<Value> {
    let id = json!({ "type": "string", "description": "The task's id (taskId), as start_work or watch_coding_task gave it." });
    vec![
        json!({
            "name": "list_coding_projects",
            "title": "List coding projects",
            "description": "Shows what you can code with on the user's Mac: which coding tools (Claude Code, Codex, Muse Code, OpenCode, pi) are plugged in on Filo's Coding page and installed, and the project folders Filo knows (the git repos it found, and folders the user added). Call it before start_work when you don't know the project's exact name. Any other folder can be given to start_work by its full path. If no tool is ready, tell the user what's missing (plug a tool in on the Coding page, install or sign it in) rather than starting.",
            "inputSchema": {
                "type": "object",
                "properties": { "search": { "type": "string", "description": "Only projects whose name or path has this." } },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "start_work",
            "title": "Start work",
            "description": "Hands work in one of the user's projects to a coding tool on their Mac (Claude Code, Codex, Muse Code, OpenCode or pi): their code, a review of it, research inside it. Do small things yourself; hand work off when it needs the project (editing code, running it, its tests) or would take a while. One piece of work per task: two unrelated asks are two hand-offs. \
        Say what the work is, not how to do it: title (a few words for its row), goal (the outcome in one line, in the user's terms), doneWhen (what finished looks like, in a sentence; name a check only when it is the point, like a failing test that should pass), and changes (false when it only looks: investigate, review, research; it then can't edit anything). notes, when needed at all, is a line or two it can't learn from the code or the goal (where the problem shows, what must stay as it is): never workspace, git, permission or testing rules, which it already has. Write plainly, in sentence case: capitalised words for emphasis are refused. Name a file the user sent where it matters, by its path on their Mac. \
        project is a project's name from list_coding_projects or the full path of any folder. tool is claude-code, codex, muse (Muse Code), opencode (OpenCode) or pi: the one the user named, else leave it out for your own engine. model, effort and workspace follow the Coding page's settings: pass them only when the user asked (workspace \"worktree\": its own branch in its own worktree; \"checkout\": their folder as it is). \
        To add to a task already under way or finished, pass its id as continues (the next round in the same session) instead of starting it again; a hand-off that looks like one under way in this conversation is refused unless you pass separate: true. \
        You are told in this conversation, in a turn of its own, when the tool asks something or finishes a round. Tell the user in a line that it started; don't check on it unless they ask.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A few words for the task's row." },
                    "goal": { "type": "string", "description": "The outcome in one line, in the user's terms." },
                    "doneWhen": { "type": "string", "description": "What finished looks like, in a sentence." },
                    "changes": { "type": "boolean", "description": "False when it only looks (investigate, review, research)." },
                    "project": { "type": "string", "description": "A project's name (list_coding_projects) or a folder's full path." },
                    "notes": { "type": "string", "description": "Optional: a line or two it can't learn from the code or the goal." },
                    "tool": { "type": "string", "enum": ["default", "claude-code", "codex", "muse", "opencode", "pi"], "description": "The coding tool: the one the user named; default (or left out) is your own engine." },
                    "model": { "type": "string", "description": "Only when the user asked for a model." },
                    "effort": { "type": "string", "enum": std::iter::once("default").chain(crate::coding::EFFORT_ORDER).collect::<Vec<_>>(), "description": "Only when the user asked for a reasoning level." },
                    "workspace": { "type": "string", "enum": ["default", "worktree", "checkout"], "description": "Only when the user said where the work goes." },
                    "continues": { "type": "string", "description": "A task's id to add this to as its next round, instead of starting a new task." },
                    "separate": { "type": "boolean", "description": "True when this is different work from a similar task under way here." },
                    "userSaidSo": { "type": "boolean", "description": "With continues, after 5 rounds: the user said to keep going." }
                },
                "required": ["title", "goal", "doneWhen", "changes"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "watch_coding_task",
            "title": "Watch coding task",
            "description": "Looks over a coding task's shoulder: its status (queued, running, done, failed or stopped), what the tool is doing now, its open questions, its rounds, its last message, the proof it saved and the latest of its thread. Use it when the user asks how it's going or before you answer about it.",
            "inputSchema": { "type": "object", "properties": { "taskId": id }, "required": ["taskId"], "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "tell_coding_task",
            "title": "Tell coding task",
            "description": "Says something short to the tool, in its thread: a sentence or two, never the brief or its rules again. While it works, it reads your note at its next step: steer it, add something it needs, or correct it. Once a round has finished, your message starts the next round in the same session. After 5 rounds ask the user whether to keep going, and pass userSaidSo once they say yes.",
            "inputSchema": {
                "type": "object",
                "properties": { "taskId": id, "message": { "type": "string" }, "userSaidSo": { "type": "boolean" } },
                "required": ["taskId", "message"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "stop_coding_task",
            "title": "Stop coding task",
            "description": "Stops a coding task now, when the user calls it off or it went the wrong way. Its changes stay, and its branch and worktree when it has them; a later tell_coding_task or start_work with continues picks it up again.",
            "inputSchema": { "type": "object", "properties": { "taskId": id }, "required": ["taskId"], "additionalProperties": false },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "answer_coding_request",
            "title": "Answer coding request",
            "description": "Answers a coding tool's question or permission request, which it waits on. Answer yourself when what you know settles it (the user's words, your memory, this conversation), in a sentence or two like a coworker. For a permission request the answer is allow, allow_always (for the rest of its session) or deny: allow what fits what the user asked for, deny what goes beyond it (merging, deleting, other projects). What only the user can decide goes to them with ask_user_about_coding_request; if they reply in words, answer with their reply.",
            "inputSchema": {
                "type": "object",
                "properties": { "requestId": { "type": "string" }, "answer": { "type": "string" } },
                "required": ["requestId", "answer"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "ask_user_about_coding_request",
            "title": "Ask the user about a coding request",
            "description": "Hands a coding tool's question or permission request you can't decide to the user. They see it in this conversation in a panel above the message box (Allow, Allow always, Deny for a permission request; your options plus their own answer for a question), and their pick answers the tool directly. Ask in plain words, saying which project it's about; for a question give two to four short options, each a short label with a description of what picking it means, and mark the one you'd suggest as recommended. Then tell them in one short line that the tool needs their decision, without repeating the question or the options.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "requestId": { "type": "string" },
                    "question": { "type": "string" },
                    "options": {
                        "type": "array",
                        "items": {
                            "anyOf": [
                                { "type": "string" },
                                {
                                    "type": "object",
                                    "properties": { "label": { "type": "string" }, "description": { "type": "string" }, "recommended": { "type": "boolean" } },
                                    "required": ["label"],
                                    "additionalProperties": false
                                }
                            ]
                        },
                        "maxItems": 4
                    }
                },
                "required": ["requestId", "question"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
    ]
}

/// Who is asking: the conversation the work belongs to and who in it.
#[derive(Clone, Debug, PartialEq)]
struct Origin {
    /// The conversation tasks belong to: this chat, or the group's conversation in a group turn (unless it runs in the
    /// speaker's own chat: that one).
    chat_id: String,
    agent_id: Option<String>,
    speaker: Option<String>,
    /// The name the tool and the Coding page know them by.
    owner: String,
    /// The engine this conversation runs on (the default tool).
    engine: Option<Engine>,
}

fn origin(target: &Target, db: &Db) -> Result<Origin, String> {
    let owner = crate::feed_mcp::origin_of_chat(db, &target.chat_id).author;
    let engine = target.store.chat(&target.chat_id).ok().flatten().map(|chat| chat.engine);
    let agent_id = crate::app_data::agent_of_chat(db, &target.chat_id).ok().flatten().map(|agent| agent.id);
    match &target.group_speaker {
        // A group turn in the speaker's own chat: the task is the chat's, and its news is the speaker's turn in the group.
        Some(speaker) if target.own_chat => {
            Ok(Origin { chat_id: target.chat_id.clone(), agent_id, speaker: Some(speaker.clone()), owner, engine })
        }
        Some(speaker) => {
            let group = crate::group::session(db)?.ok_or("The group chat isn't there anymore.")?;
            Ok(Origin { chat_id: group, agent_id, speaker: Some(speaker.clone()), owner, engine })
        }
        None => Ok(Origin { chat_id: target.chat_id.clone(), agent_id, speaker: None, owner, engine }),
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    text(args, key).ok_or_else(|| format!("`{key}` is required."))
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `default` or empty means "as the Coding page has it".
fn chosen<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    text(args, key).filter(|value| *value != "default")
}

pub fn call(target: &Target, name: &str, args: &Value) -> Result<String, String> {
    let db = Db::open(&target.db).map_err(|error| format!("Filo's database couldn't open: {error}"))?;
    let now = crate::now_ms() as i64;
    let reply = |value: Value| serde_json::to_string_pretty(&value).unwrap_or_default();
    match name {
        "list_coding_projects" => list(&db, text(args, "search")).map(reply),
        "start_work" => {
            let origin = origin(target, &db)?;
            match text(args, "continues") {
                Some(task_id) => {
                    let message = handoff::render_brief(
                        required(args, "goal")?,
                        required(args, "doneWhen")?,
                        text(args, "notes"),
                    );
                    continue_task(&db, &origin, task_id, &message, flag(args, "userSaidSo"), now)
                        .map(|done| asked(&db, target, done, now))
                        .map(reply)
                }
                None => start(&db, &origin, args, now).map(|done| asked(&db, target, done, now)).map(reply),
            }
        }
        "watch_coding_task" => {
            let task = coding::task(&db, required(args, "taskId")?)?.ok_or("There's no coding task with that id.")?;
            let handoff = handoff::get(&db, &task.id)?;
            handoff::status_json(&db, &task, handoff.as_ref(), 12).map(reply)
        }
        "tell_coding_task" => {
            let origin = origin(target, &db)?;
            continue_task(
                &db,
                &origin,
                required(args, "taskId")?,
                required(args, "message")?,
                flag(args, "userSaidSo"),
                now,
            )
            .map(|done| asked(&db, target, done, now))
            .map(reply)
        }
        "stop_coding_task" => {
            let task = coding::task(&db, required(args, "taskId")?)?.ok_or("There's no coding task with that id.")?;
            if coding::is_finished(&task.status) {
                return Ok(reply(json!({ "ok": true, "status": task.status, "note": "It had already ended." })));
            }
            if handoff::get(&db, &task.id)?.is_none() {
                let origin = origin(target, &db)?;
                handoff::adopt(
                    &db,
                    &task,
                    &origin.chat_id,
                    origin.agent_id.as_deref(),
                    origin.speaker.as_deref(),
                    now,
                )?;
            }
            handoff::request_stop(&db, &task.id)?;
            Ok(reply(
                json!({ "ok": true, "note": "Stopping it now. Its changes stay, and its branch and worktree when it has them." }),
            ))
        }
        "answer_coding_request" => {
            let answered = handoff::answer(&db, required(args, "requestId")?, required(args, "answer")?, "agent", now)?;
            Ok(reply(
                json!({ "ok": true, "answer": answered.answer, "note": "Answered: the tool carries on with it." }),
            ))
        }
        "ask_user_about_coding_request" => {
            let (options, notes) = user_options(args.get("options"));
            let id = required(args, "requestId")?;
            handoff::ask_user(&db, id, required(args, "question")?, &options, now)?;
            handoff::add_option_notes(&db, id, &options, &notes)?;
            Ok(reply(json!({
                "ok": true,
                "note": "The user sees it in this conversation as a card with answer buttons; their tap answers the tool. Tell them in one short line that the tool needs their decision, without repeating the question or the options."
            })))
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

/// `done` (what start_work or tell_coding_task did), with the ask noted: this turn asked its task for work, so the reply
/// reporting the round answers the message that asked (coding_handoff.rs `asking_turns`).
fn asked(db: &Db, target: &Target, done: Value, now: i64) -> Value {
    if let (Some(turn), Some(task)) = (target.turn_id.as_deref(), done.get("taskId").and_then(Value::as_str)) {
        let _ = handoff::add_ask(db, task, turn, now);
    }
    done
}

/// The options handed to the user: plain labels, or `{label, description, recommended}` (the recommended one's label
/// says so, as the coding tools' own do). Returns the labels and what each means.
fn user_options(options: Option<&Value>) -> (Vec<String>, Vec<String>) {
    options
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| match option {
            Value::String(label) => Some((label.clone(), String::new())),
            Value::Object(_) => {
                let label = option["label"].as_str()?.trim();
                let label = if option["recommended"].as_bool() == Some(true)
                    && !label.to_lowercase().contains("(recommended)")
                {
                    format!("{label} (Recommended)")
                } else {
                    label.to_string()
                };
                Some((label, option["description"].as_str().unwrap_or_default().to_string()))
            }
            _ => None,
        })
        .filter(|(label, _)| !label.trim().is_empty())
        .unzip()
}

/// The tools, whether this Mac takes tasks, and the projects.
fn list(db: &Db, search: Option<&str>) -> Result<Value, String> {
    let path_env = std::env::var("PATH").ok();
    let tools: Vec<Value> = coding::tools(db)?
        .into_iter()
        .map(|settings| {
            let engine = coding::engine_of(&settings.id).unwrap_or(Engine::Claude);
            let installed = engine::find_program(engine, path_env.as_deref()).is_some();
            json!({ "tool": settings.id, "name": engine.label(), "pluggedIn": settings.plugged, "installed": installed, "mode": settings.mode, "model": settings.model, "effort": settings.effort })
        })
        .collect();
    let projects = coding::projects(db, search)?;
    let shown: Vec<Value> = projects
        .iter()
        .take(80)
        .map(|project| json!({ "name": project.name, "path": project.path, "branch": project.branch, "git": !project.plain_folder, "here": Path::new(&project.path).is_dir() }))
        .collect();
    let mut answer = json!({
        "workspace": coding::workspace(db)?,
        "tools": tools,
        "projects": shown,
    });
    if projects.len() > shown.len() {
        answer["more"] = json!(format!("{} more: narrow it with search.", projects.len() - shown.len()));
    }
    Ok(answer)
}

/// The project `wanted` names: a known project's name (or part of it), or a folder's full path, which is added.
fn find_project(db: &Db, wanted: &str, now: i64) -> Result<Project, String> {
    let wanted = wanted.trim();
    if wanted.starts_with('/') || wanted.starts_with('~') {
        let path = match wanted.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
            None => PathBuf::from(wanted),
        };
        let canonical = path.canonicalize().map_err(|_| format!("There's no folder at {wanted} on this Mac."))?;
        if !canonical.is_dir() {
            return Err(format!("{wanted} isn't a folder."));
        }
        return match coding::project(db, &canonical.display().to_string())? {
            Some(project) => Ok(project),
            None => coding::add_project(db, &canonical, now),
        };
    }
    let all = coding::projects(db, None)?;
    let lower = wanted.to_lowercase();
    let exact: Vec<&Project> = all.iter().filter(|project| project.name.to_lowercase() == lower).collect();
    let matches: Vec<&Project> = if exact.is_empty() {
        all.iter().filter(|project| project.name.to_lowercase().contains(&lower)).collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!(
            "No project named \"{wanted}\" on this Mac. list_coding_projects shows them; any other folder goes by its full path."
        )),
        several => Err(format!(
            "Several projects match \"{wanted}\": {}. Pass the full path of the one you mean.",
            several.iter().take(8).map(|project| project.path.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Whether the tool's CLI is on this Mac (the PATH the CLI running this server got from the app). Tests don't depend on
/// what the Mac running them has installed.
fn installed(engine: Engine) -> bool {
    cfg!(test) || engine::find_program(engine, std::env::var("PATH").ok().as_deref()).is_some()
}

/// The tool a hand-off goes to: the one asked for, else this conversation's engine, else whichever is plugged in.
fn pick_tool(db: &Db, origin: &Origin, asked: Option<&str>) -> Result<String, String> {
    let tools = coding::tools(db)?;
    let plugged = |id: &str| tools.iter().any(|tool| tool.id == id && tool.plugged);
    if let Some(asked) = asked {
        let engine = coding::engine_of(asked)
            .ok_or_else(|| format!("Not a coding tool: {asked}. Use claude-code, codex, muse, opencode or pi."))?;
        if !plugged(asked) {
            return Err(format!(
                "{} is unplugged on Filo's Coding page. Tell the user to plug it in there, or use another tool if they don't mind.",
                engine.label()
            ));
        }
        return Ok(asked.to_string());
    }
    let own = origin.engine.map(coding::tool_of);
    own.filter(|id| plugged(id))
        .or_else(|| coding::TOOLS.iter().copied().find(|id| plugged(id)))
        .map(str::to_string)
        .ok_or_else(|| {
            "Every coding tool is unplugged on Filo's Coding page. Tell the user to plug one in there.".into()
        })
}

fn start(db: &Db, origin: &Origin, args: &Value, now: i64) -> Result<Value, String> {
    let (title, goal, done_when) = (required(args, "title")?, required(args, "goal")?, required(args, "doneWhen")?);
    let notes = text(args, "notes");
    let changes = args
        .get("changes")
        .and_then(Value::as_bool)
        .ok_or("`changes` is required: true when it may edit, false when it only looks.")?;
    if title.chars().count() > TITLE_MAX
        || goal.chars().count() > GOAL_MAX
        || done_when.chars().count() > GOAL_MAX
        || notes.is_some_and(|notes| notes.chars().count() > NOTES_MAX)
    {
        return Err(format!(
            "Keep it short: title up to {TITLE_MAX} characters, goal and doneWhen up to {GOAL_MAX}, notes up to {NOTES_MAX}."
        ));
    }
    if let Some(problem) = handoff::brief_problem(title, goal, done_when, notes) {
        return Err(problem);
    }
    let tool = pick_tool(db, origin, chosen(args, "tool"))?;
    let engine = coding::engine_of(&tool).unwrap_or(Engine::Claude);
    if !installed(engine) {
        return Err(format!("{} isn't installed on this Mac. Tell the user, or use another tool.", engine.label()));
    }
    let model = chosen(args, "model").map(str::to_string);
    let effort = chosen(args, "effort").map(str::to_string);
    if let Some(bad) = [&model, &effort].into_iter().flatten().find(|value| !engine::valid_model(value)) {
        return Err(format!("Not a name the tool takes: {bad}"));
    }
    let project = find_project(db, required(args, "project").map_err(|_| "Work on this Mac needs its project: pass project (a name from list_coding_projects, or any folder's full path).".to_string())?, now)?;
    if !Path::new(&project.path).is_dir() {
        return Err(format!("{} isn't there anymore.", project.path));
    }
    if !flag(args, "separate") {
        let same = handoff::for_chat(db, &origin.chat_id)?.into_iter().find(|(task, _)| {
            !coding::is_finished(&task.status) && task.project_path == project.path && same_work(&task.title, title)
        });
        if let Some((task, _)) = same {
            return Err(format!(
                "This looks like work already under way here: \"{}\" ({}, {}, id {}). To add to it, pass continues: \"{}\". If it is different work, say how in notes and pass separate: true.",
                task.title,
                handoff::tool_name(&task.tool),
                task.status,
                task.id,
                task.id
            ));
        }
    }
    let worktree = match chosen(args, "workspace") {
        Some("worktree") => true,
        Some("checkout") => false,
        Some(other) => return Err(format!("workspace is worktree or checkout, not {other}.")),
        None => coding::workspace(db)? == "worktree",
    };
    let brief = handoff::render_brief(goal, done_when, notes);
    let task = coding::create_task(
        db,
        &NewTask { brief, tool: tool.clone(), project: project.clone(), worktree, owner: origin.owner.clone() },
        now,
    )?;
    let new = NewHandoff {
        chat_id: origin.chat_id.clone(),
        agent_id: origin.agent_id.clone(),
        speaker: origin.speaker.clone(),
        title: title.to_string(),
        goal: goal.to_string(),
        done_when: done_when.to_string(),
        notes: notes.map(str::to_string),
        look_only: !changes,
        model,
        effort,
    };
    let task = handoff::create(db, &task.id, &new, now)?;
    let place = if project.plain_folder {
        "in the folder as it is (not a git repo)".to_string()
    } else if worktree {
        "in a worktree of its own, on a new branch".to_string()
    } else {
        "in the user's checkout".to_string()
    };
    Ok(json!({
        "ok": true,
        "taskId": task.id,
        "tool": tool,
        "project": project.name,
        "path": project.path,
        "note": format!(
            "Started: {} works on it in {} {place}{}. You'll be told in this conversation, in a turn of its own, when it asks something or finishes a round. Tell the user in a line that it started; don't check on it unless they ask.",
            engine.label(),
            project.name,
            if changes { "" } else { ", only looking" }
        ),
    }))
}

/// Words a title can have or not and still name the same work.
const SMALL_WORDS: [&str; 16] =
    ["a", "an", "the", "to", "of", "and", "or", "for", "in", "on", "at", "by", "with", "its", "it", "this"];

/// Two titles name the same work: mostly the same words, whatever their case, punctuation and small words ("Remove the
/// "From Filo" feed tab" and "Remove From Filo feed tab").
fn same_work(a: &str, b: &str) -> bool {
    let words = |title: &str| -> std::collections::BTreeSet<String> {
        title
            .split(|c: char| !c.is_alphanumeric())
            .map(str::to_lowercase)
            .filter(|word| !word.is_empty() && !SMALL_WORDS.contains(&word.as_str()))
            .collect()
    };
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return a == b;
    }
    // Three in four of all their words shared.
    a.intersection(&b).count() * 4 >= a.union(&b).count() * 3
}

/// More for a task: a note while it works, its next round once it's done.
fn continue_task(
    db: &Db,
    origin: &Origin,
    task_id: &str,
    message: &str,
    user_said_so: bool,
    now: i64,
) -> Result<Value, String> {
    let task = coding::task(db, task_id)?.ok_or("There's no coding task with that id.")?;
    if let Some(problem) = handoff::brief_problem("", message, "", None) {
        return Err(problem);
    }
    if handoff::get(db, &task.id)?.is_none() {
        handoff::adopt(db, &task, &origin.chat_id, origin.agent_id.as_deref(), origin.speaker.as_deref(), now)?;
    }
    if !coding::is_finished(&task.status) {
        handoff::add_note(db, &task.id, message, now)?;
        let note = if task.status == "monitoring" {
            "It's waiting on background work: your note reaches it now, and it carries on with it."
        } else {
            "It's working: it reads your note at its next step."
        };
        return Ok(json!({ "ok": true, "taskId": task.id, "note": note }));
    }
    if task.rounds >= handoff::MAX_ROUNDS && !user_said_so {
        return Err(format!(
            "It has run {} rounds. Ask the user whether to keep going, then pass userSaidSo: true.",
            task.rounds
        ));
    }
    coding::follow_up(db, &task.id, message, now)?;
    Ok(
        json!({ "ok": true, "taskId": task.id, "note": "Sent as its next round, in the same session. You'll be told here when it asks something or finishes." }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;

    fn target(store: &crate::store::Store, chat_id: &str) -> Target {
        Target {
            store: store.clone(),
            chat_id: chat_id.into(),
            turn_id: Some("t1".into()),
            db: store.root().join(crate::db::FILE),
            group_speaker: None,
            own_chat: false,
            part: crate::mcp::Part::All,
        }
    }

    fn reply(result: Result<String, String>) -> Value {
        serde_json::from_str(&result.unwrap()).unwrap()
    }

    #[test]
    fn start_work_makes_a_task_this_conversation_owns() {
        let store = temp_store("coding-mcp");
        let chat = store.create_chat(Engine::Codex).unwrap();
        let target = target(&store, &chat.id);
        let db = Db::open(&target.db).unwrap();
        let folder = store.root().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let args = json!({ "title": "Wrap table text", "goal": "Long table cells wrap", "doneWhen": "A long cell wraps in the Result view", "changes": true, "project": folder.display().to_string() });
        assert!(
            call(
                &target,
                "start_work",
                &json!({ "title": "x", "goal": "Do NOT break it", "doneWhen": "y", "changes": true, "project": "app" })
            )
            .unwrap_err()
            .contains("sentence case")
        );
        let started = reply(call(&target, "start_work", &args));
        let id = started["taskId"].as_str().unwrap().to_string();
        // Its own engine (Codex) when none was named; the folder became a project.
        assert_eq!((started["tool"].as_str(), started["project"].as_str()), (Some("codex"), Some("app")));
        let task = coding::task(&db, &id).unwrap().unwrap();
        assert_eq!(
            (task.title.as_str(), task.status.as_str(), task.owner.as_str()),
            ("Wrap table text", "queued", "Filo")
        );
        assert!(task.brief.starts_with("Goal: Long table cells wrap\n\nDone when:"));
        assert_eq!(handoff::get(&db, &id).unwrap().unwrap().chat_id, chat.id);
        // The same work again is refused; a new name works, by project name too.
        assert!(call(&target, "start_work", &args).unwrap_err().contains("already under way"));
        let again = reply(call(
            &target,
            "start_work",
            &json!({ "title": "Review the parser", "goal": "Find why parsing is slow", "doneWhen": "The cause is named", "changes": false, "project": "app", "tool": "claude-code" }),
        ));
        assert_eq!(again["tool"], "claude-code");
        assert!(handoff::get(&db, again["taskId"].as_str().unwrap()).unwrap().unwrap().look_only);

        // A note while it waits or works; a request answered; Stop.
        assert_eq!(
            reply(call(&target, "tell_coding_task", &json!({ "taskId": id, "message": "Keep the header as it is" })))["ok"],
            true
        );
        assert_eq!(handoff::undelivered_notes(&db, &id).unwrap().len(), 1);
        let request = handoff::add_request(&db, &id, 1, "approval", "Run `npm install`", None, &[], 1).unwrap();
        assert!(call(&target, "answer_coding_request", &json!({ "requestId": request, "answer": "perhaps" })).is_err());
        assert_eq!(
            reply(call(&target, "answer_coding_request", &json!({ "requestId": request, "answer": "allow" })))["answer"],
            "allow"
        );
        let question = handoff::add_request(&db, &id, 1, "question", "Which port?", None, &[], 1).unwrap();
        reply(call(
            &target,
            "ask_user_about_coding_request",
            &json!({ "requestId": question, "question": "Which port should the app use?", "options": ["3000", "8080"] }),
        ));
        assert!(handoff::request(&db, &question).unwrap().unwrap().asked_user);
        // Options with what they mean, one recommended: its label says so, and each one's description is kept.
        let described = handoff::add_request(&db, &id, 1, "question", "Which port?", None, &[], 1).unwrap();
        let options = json!([{ "label": "3000", "description": "The usual one", "recommended": true }, "8080", { "label": "4000" }]);
        reply(call(
            &target,
            "ask_user_about_coding_request",
            &json!({ "requestId": described, "question": "Which port?", "options": options }),
        ));
        let described = handoff::request(&db, &described).unwrap().unwrap();
        assert_eq!(described.user_options, ["3000 (Recommended)", "8080", "4000"]);
        assert_eq!(described.option_notes.get("3000 (Recommended)").map(String::as_str), Some("The usual one"));
        assert_eq!(described.option_notes.len(), 1);
        handoff::answer(&db, &described.id, "8080", "user", 2).unwrap();
        let watched = reply(call(&target, "watch_coding_task", &json!({ "taskId": id })));
        assert_eq!(
            (watched["status"].as_str(), watched["openQuestions"].as_array().unwrap().len()),
            (Some("queued"), 1)
        );
        reply(call(&target, "stop_coding_task", &json!({ "taskId": id })));
        assert_eq!(handoff::take_stop_requests(&db).unwrap(), [id.clone()]);

        // A finished task takes its next round; after five, only with the user's yes.
        coding::finish_round(&db, &id, "done", None, Some("Done"), 5).unwrap();
        let next = reply(call(
            &target,
            "start_work",
            &json!({ "continues": id, "title": "More", "goal": "Also the footer", "doneWhen": "The footer wraps", "changes": true }),
        ));
        assert!(next["note"].as_str().unwrap().contains("next round"));
        assert_eq!(coding::next_message(&db, &id).unwrap(), "Goal: Also the footer\n\nDone when: The footer wraps");
        db.write(|tx| tx.execute("UPDATE coding_tasks SET status = 'done', rounds = 5 WHERE id = ?1", [&id])).unwrap();
        assert!(
            call(&target, "tell_coding_task", &json!({ "taskId": id, "message": "Once more" }))
                .unwrap_err()
                .contains("userSaidSo")
        );
        assert_eq!(
            reply(call(
                &target,
                "tell_coding_task",
                &json!({ "taskId": id, "message": "Once more", "userSaidSo": true })
            ))["ok"],
            true
        );

        let listed = reply(call(&target, "list_coding_projects", &json!({})));
        assert_eq!(listed["projects"][0]["name"], "app");
        let tools: Vec<(&str, bool)> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| (tool["name"].as_str().unwrap(), tool["pluggedIn"].as_bool().unwrap()))
            .collect();
        assert_eq!(
            tools,
            [("Claude Code", true), ("Codex", true), ("Muse Code", false), ("OpenCode", false), ("pi", false)]
        );
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn the_same_work_in_other_words_is_refused_and_points_at_the_running_task() {
        let store = temp_store("coding-mcp-same-work");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let target = target(&store, &chat.id);
        let folder = store.root().join("filo");
        std::fs::create_dir_all(&folder).unwrap();
        let brief = |title: &str| json!({ "title": title, "goal": "The Feed shows only For you", "doneWhen": "No tab switcher on the Feed", "changes": true, "project": folder.display().to_string() });
        let first = reply(call(&target, "start_work", &brief("Remove the \"From Filo\" feed tab")));
        let id = first["taskId"].as_str().unwrap();
        // The second message's turn, in other words: refused, naming the task under way and how to add to it.
        let refused = call(&target, "start_work", &brief("Remove From Filo feed tab")).unwrap_err();
        assert!(
            refused.contains("already under way") && refused.contains(&format!("continues: \"{id}\"")),
            "{refused}"
        );
        // Different work, or the agent saying it is, starts.
        assert!(call(&target, "start_work", &brief("Remove the For you feed tab")).is_ok());
        let mut separate = brief("Remove From Filo feed tab");
        separate["separate"] = json!(true);
        assert!(call(&target, "start_work", &separate).is_ok());
        // Once it has ended, the same title starts again.
        let db = Db::open(&target.db).unwrap();
        coding::finish_round(&db, id, "done", None, Some("Done"), 5).unwrap();
        db.write(|tx| {
            tx.execute("UPDATE coding_tasks SET status = 'done' WHERE title = 'Remove From Filo feed tab'", [])
        })
        .unwrap();
        assert!(call(&target, "start_work", &brief("Remove From Filo feed tab")).is_ok());

        assert!(same_work("Fix the login bug", "fix login bug!"));
        assert!(!same_work("Fix the login bug", "Fix the signup bug"));
        assert!(!same_work("The", "Wrap table text"));
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn in_the_group_the_task_belongs_to_the_group_and_its_member() {
        let store = temp_store("coding-mcp-group");
        let chat = store.create_chat(Engine::Claude).unwrap();
        let mut target = target(&store, &chat.id);
        target.group_speaker = Some("chef".into());
        let db = Db::open(&target.db).unwrap();
        let group = crate::group::ensure(&db, 1).unwrap();
        let origin = origin(&target, &db).unwrap();
        assert_eq!((origin.chat_id.as_str(), origin.speaker.as_deref()), (group.as_str(), Some("chef")));
        // A group turn run in the member's own chat: the task is that chat's, its news the member's turn in the group.
        let in_own = Target { own_chat: true, ..target.clone() };
        let in_own = super::origin(&in_own, &db).unwrap();
        assert_eq!((in_own.chat_id.as_str(), in_own.speaker.as_deref()), (chat.id.as_str(), Some("chef")));
        let folder = store.root().join("site");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(find_project(&db, "site", 1).is_err());
        let project = find_project(&db, &folder.display().to_string(), 1).unwrap();
        assert_eq!(find_project(&db, "SIT", 2).unwrap(), project);
        drop(db);
        std::fs::remove_dir_all(store.root()).ok();
    }
}

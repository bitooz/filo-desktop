// The tools behind the desktop-pet skill (desktop_pet_skill.md): `generate_images` draws pictures, `save_pet` turns four
// of them into one of the user's characters (characters.rs).
//
// Neither Claude Code nor a chat's Codex can make pictures (Codex's image making is off in chats, engine.rs
// `CODEX_FEATURES_OFF`), so `generate_images` asks the user's Codex for each one on its own: `codex exec` with its
// image tool, signed in with their ChatGPT account, no key needed. Codex saves what it makes in
// `$CODEX_HOME/generated_images/<its session>/`; the picture is copied from there to where the agent asked. Several run
// at once.
//
// So the user sees it happen and sees what Codex drew: while they're drawn, the chat's line beside its typing dots says
// "Codex is drawing 3 pictures" (chat_activity.rs `set_tool`); when they're done, they show in the chat as a message of
// Filo's own with them attached, copies the agent's later changes to its files don't touch. The agent is told to call
// anything it redraws from them its own version, not Codex's.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use crate::characters::{self, POSES};
use crate::engine::{self, Engine};
use crate::mcp::Target;
use crate::store::Attachment;

const NAMES: &[&str] = &["generate_images", "save_pet"];
/// Pictures one call makes at most.
const MAX_IMAGES: usize = 4;
/// One picture takes about a minute; past this it's given up.
const IMAGE_TIMEOUT: Duration = Duration::from_secs(8 * 60);
const MAX_PROMPT: usize = 4000;

pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "generate_images",
            "title": "Generate images",
            "description": "Make pictures with image generation (the user's Codex, signed in with ChatGPT), each saved as a PNG where you say. Each picture costs the user money: make only the ones the task needs. Up to 4 per call, made at the same time; each takes about a minute, and the chat shows the user that Codex is drawing. When it returns, the pictures show in the chat as Codex drew them. Those are the originals: show or describe them before any change. If you then trace, vectorize, recolour or redraw one yourself, call yours a traced or redrawn version (\"traced from Codex's picture\"), never \"Codex-drawn\"; only what this tool made is Codex's. To keep a character the same across pictures, pass an earlier picture in `references`. For a desktop pet, follow the desktop-pet skill.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "images": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_IMAGES,
                        "items": {
                            "type": "object",
                            "properties": {
                                "prompt": { "type": "string", "description": "What to draw." },
                                "path": { "type": "string", "description": "Absolute path of the new PNG to write (its folder is made if needed); nothing may be there yet." },
                                "references": { "type": "array", "items": { "type": "string" }, "description": "Absolute paths of pictures to draw from (PNG, JPEG or WebP)." }
                            },
                            "required": ["prompt", "path"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["images"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true }
        }),
        json!({
            "name": "save_pet",
            "title": "Save desktop pet",
            "description": "Turn four pictures of one character (resting, working, waving, sleeping) into a desktop pet: each pose is cut out on a 400px square, sized to the resting one and placed feet down, its eyes found for blinking, and the motion data worked out. With check_only it only reports, with a contact sheet to look at (eye boxes in red); otherwise it saves the pet among the user's characters (Settings › Desktop pet, and Characters on App icon) and, unless make_it_the_pet is false, makes it the desktop pet. Follow the desktop-pet skill.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "A short name for the pet, at most 40 characters (\"Astronaut cat\")." },
                    "idle": { "type": "string", "description": "Absolute path of the resting pose's picture." },
                    "working": { "type": "string", "description": "Absolute path of the working pose's picture (typing on a laptop)." },
                    "waving": { "type": "string", "description": "Absolute path of the waving pose's picture." },
                    "sleeping": { "type": "string", "description": "Absolute path of the sleeping pose's picture." },
                    "check_only": { "type": "boolean", "description": "Only cut out and report; nothing is saved." },
                    "make_it_the_pet": { "type": "boolean", "description": "Make it the desktop pet right away (true unless given)." }
                },
                "required": ["name", "idle", "working", "waving", "sleeping"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
    ]
}

/// `dir` is the local folder (the database's), where the characters are kept.
pub fn call(target: &Target, dir: &Path, name: &str, arguments: &Value) -> Result<String, String> {
    match name {
        "generate_images" => generate_images(target, arguments),
        "save_pet" => save_pet(dir, arguments),
        other => Err(format!("Unknown tool: {other}")),
    }
}

struct Request {
    prompt: String,
    path: PathBuf,
    references: Vec<PathBuf>,
}

fn absolute(value: &str, what: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value.trim());
    if !path.is_absolute() {
        return Err(format!("{what} must be an absolute path: {value}"));
    }
    Ok(path)
}

fn requests(arguments: &Value) -> Result<Vec<Request>, String> {
    let images = arguments
        .get("images")
        .and_then(Value::as_array)
        .filter(|images| !images.is_empty())
        .ok_or("`images` needs at least one picture")?;
    if images.len() > MAX_IMAGES {
        return Err(format!("At most {MAX_IMAGES} pictures per call."));
    }
    images
        .iter()
        .map(|image| {
            let prompt = image
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
                .ok_or("Each picture needs a `prompt`.")?;
            if prompt.chars().count() > MAX_PROMPT {
                return Err(format!("A prompt is at most {MAX_PROMPT} characters."));
            }
            let path =
                absolute(image.get("path").and_then(Value::as_str).ok_or("Each picture needs a `path`.")?, "`path`")?;
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("png"))
            {
                return Err(format!("`path` must end in .png: {}", path.display()));
            }
            // Only a new file: never over one that's there, or through a link (`generate_one` makes it new too).
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(format!(
                    "There's already something at {}: give a new file's path (pictures are only written as new files).",
                    path.display()
                ));
            }
            let references = image
                .get("references")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|reference| {
                    let path = absolute(reference.as_str().unwrap_or_default(), "A reference")?;
                    if path.is_file() { Ok(path) } else { Err(format!("No picture at {}", path.display())) }
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Request { prompt: prompt.to_string(), path, references })
        })
        .collect()
}

fn generate_images(target: &Target, arguments: &Value) -> Result<String, String> {
    let requests = requests(arguments)?;
    let codex = engine::find_program(Engine::Codex, None).ok_or(
        "Making pictures needs Codex (OpenAI's coding tool) on this Mac, signed in with a ChatGPT account: its image generation draws them. It isn't installed. Tell the user that, and that once it's set up you can make the pictures.",
    )?;
    let activity = |text: Option<&str>| {
        if let Some(turn) = &target.turn_id {
            crate::chat_activity::set_tool(&target.store, &target.chat_id, turn, text);
        }
    };
    activity(Some(&crate::chat_activity::drawing(requests.len())));
    let results: Vec<Result<String, String>> = std::thread::scope(|scope| {
        let runs: Vec<_> = requests.iter().map(|request| scope.spawn(|| generate_one(&codex, request))).collect();
        runs.into_iter().map(|run| run.join().unwrap_or_else(|_| Err("The picture failed.".into()))).collect()
    });
    activity(None);
    let mut lines: Vec<String> = requests
        .iter()
        .zip(&results)
        .map(|(request, result)| match result {
            Ok(done) => format!("{}: {done}", request.path.display()),
            Err(error) => format!("{}: not made: {error}", request.path.display()),
        })
        .collect();
    if results.iter().all(Result::is_err) {
        return Err(lines.join("\n"));
    }
    let made: Vec<&Path> = requests
        .iter()
        .zip(&results)
        .filter(|(_, result)| result.is_ok())
        .map(|(request, _)| request.path.as_path())
        .collect();
    lines.extend(told_after(target, &made));
    Ok(lines.join("\n"))
}

/// What generate_images' result says after its pictures: they're in the chat now (`show_in_chat`), or, in a group
/// turn, whose posts are the member's own, to post them itself.
fn told_after(target: &Target, made: &[&Path]) -> Vec<String> {
    const OWN_VERSION: &str = "call yours a traced or redrawn version, not Codex's.";
    if target.group_speaker.is_some() {
        return vec![
            "In the group, post them yourself: send_message with `files` set to these paths.".into(),
            format!("Show the user Codex's pictures before any version of your own, and {OWN_VERSION}"),
        ];
    }
    vec![match show_in_chat(target, made) {
        Ok(()) => format!(
            "The user sees these in the chat now, as Codex drew them. If you change one yourself (trace, redraw, edit), {OWN_VERSION}"
        ),
        Err(why) => format!(
            "They aren't shown in the chat ({why}): show the user Codex's pictures before any version of your own, and {OWN_VERSION}"
        ),
    }]
}

/// The pictures Codex made, shown in the turn's chat as a message of their own with them attached: copies (clones
/// where the disk can), so what the user sees stays what Codex drew. Not for a group turn, whose member posts them.
fn show_in_chat(target: &Target, pictures: &[&Path]) -> Result<(), String> {
    // With the user's files, not in the chats' folder.
    let root = crate::mcp::data_dir(target);
    let mut kept: Vec<Attachment> = Vec::new();
    for path in pictures {
        match keep_picture(&root, path) {
            Ok(picture) => kept.push(picture),
            Err(error) => {
                forget(&kept);
                return Err(format!("couldn't keep a copy of {}: {error}", path.display()));
            }
        }
    }
    let mut message = target.store.agent_message(&target.chat_id, drawn_caption(kept.len()), target.turn_id.clone());
    message.attachments = kept;
    target.store.append(&target.chat_id, &message).map_err(|error| {
        forget(&message.attachments);
        format!("couldn't save the message: {error}")
    })
}

/// Filo's copy of Codex's picture at `path`, in `root`'s attachments/ (attachments.rs `keep`, which doesn't read it).
fn keep_picture(root: &Path, path: &Path) -> std::io::Result<Attachment> {
    let storage_id = crate::attachments::keep(root, path, "image/png", false)?;
    let file = root.join(crate::attachments::DIR).join(&storage_id);
    Ok(Attachment {
        size: std::fs::metadata(&file).map_or(0, |meta| meta.len()),
        name: path.file_name().map_or_else(|| "picture.png".into(), |name| name.to_string_lossy().into_owned()),
        content_type: "image/png".into(),
        storage_id,
        file,
        local_path: None,
        bookmark: None,
        inode: None,
    })
}

/// Removes the copies kept for pictures that weren't shown.
fn forget(pictures: &[Attachment]) {
    for picture in pictures {
        let _ = std::fs::remove_file(&picture.file);
    }
}

fn drawn_caption(count: usize) -> String {
    if count == 1 { "Drawn by Codex.".into() } else { format!("Drawn by Codex ({count} pictures).") }
}

/// What a `codex exec` is told: make the one picture, nothing else.
fn codex_prompt(request: &Request) -> String {
    let mut prompt = String::from(
        "Make exactly one picture with your image generation tool (imagegen), then stop. Don't run commands, don't edit files, and don't make more than one picture.\n",
    );
    if !request.references.is_empty() {
        prompt.push_str(&format!(
            "Pass these files to it as referenced_image_paths: {}\n",
            request
                .references
                .iter()
                .map(|path| serde_json::to_string(&path.display().to_string()).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    prompt.push_str(&format!("\nPicture prompt: {}\n", request.prompt));
    prompt
}

/// One picture: `codex exec` in an empty folder, then its picture copied to the request's path. What was made.
fn generate_one(codex: &Path, request: &Request) -> Result<String, String> {
    let work = std::env::temp_dir().join(format!("filo-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|error| format!("couldn't make a folder to work in: {error}"))?;
    let outcome = run_codex(codex, &work, &codex_prompt(request));
    let _ = std::fs::remove_dir_all(&work);
    let made = outcome?;
    let picture = image::open(&made).map_err(|error| format!("Codex's picture couldn't be read: {error}"))?;
    if let Some(folder) = request.path.parent() {
        std::fs::create_dir_all(folder).map_err(|error| format!("couldn't make {}: {error}", folder.display()))?;
    }
    let mut png = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut png, image::ImageFormat::Png).map_err(|error| format!("couldn't make the PNG: {error}"))?;
    write_new(&request.path, png.get_ref()).map_err(|error| format!("couldn't write it: {error}"))?;
    Ok(format!("made, {}x{}", picture.width(), picture.height()))
}

/// Writes `bytes` as a new file at `path`: never over a file that's there, nor through a link at the path, so an
/// agent can't have a picture written over a file of the user's (`O_EXCL`, which doesn't follow a link).
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes).inspect_err(|_| _ = std::fs::remove_file(path))
}

/// Runs `codex exec --json` with `prompt` and finds the picture its session saved.
fn run_codex(codex: &Path, work: &Path, prompt: &str) -> Result<PathBuf, String> {
    let args: Vec<String> = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        // Not the user's MCP servers, rules or plugins: only the image tool is needed.
        "--ignore-user-config",
        "--ignore-rules",
        "-c",
        "sandbox_mode=\"read-only\"",
        "-c",
        "approval_policy=\"never\"",
        "-c",
        "model_reasoning_effort=\"low\"",
        "-c",
        "features.image_generation=true",
        "-C",
    ]
    .iter()
    .map(|arg| arg.to_string())
    .chain([work.display().to_string(), "-".into()])
    .collect();
    let ran = engine::run_for(Command::new(codex).args(&args).current_dir(work), Some(prompt), IMAGE_TIMEOUT)
        .map_err(|error| format!("Codex couldn't start: {error}"))?;
    let status = ran.status;
    let (events, errors) =
        (String::from_utf8_lossy(&ran.stdout).into_owned(), String::from_utf8_lossy(&ran.stderr).into_owned());
    let (thread, said) = read_events(&events);
    if let Some(picture) =
        thread.and_then(|thread| newest_picture(&engine::codex_home().join("generated_images").join(thread)))
    {
        return Ok(picture);
    }
    let why = said.or_else(|| errors.lines().rev().find(|line| !line.trim().is_empty()).map(str::to_string));
    Err(match (status, why) {
        (None, _) => "Codex didn't finish the picture in time.".into(),
        (_, Some(why)) => format!("Codex made no picture: {}", why.trim()),
        (_, None) if errors.contains("login") || errors.contains("auth") => {
            "Codex isn't signed in: the user can sign in by running `codex login`.".into()
        }
        _ => "Codex made no picture.".into(),
    })
}

/// The session `codex exec --json` started, and what went wrong if anything (an error, or the last thing it said).
fn read_events(events: &str) -> (Option<String>, Option<String>) {
    let mut thread = None;
    let mut error = None;
    let mut said = None;
    for event in events.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
        match event["type"].as_str() {
            Some("thread.started") => {
                thread = event["thread_id"].as_str().filter(|id| crate::store::valid_id(id)).map(str::to_string)
            }
            Some("error") | Some("turn.failed") => {
                error = event["message"]
                    .as_str()
                    .or_else(|| event["error"]["message"].as_str())
                    .map(str::to_string)
                    .or(error);
            }
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                said = event["item"]["text"]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string)
                    .or(said);
            }
            _ => {}
        }
    }
    (thread, error.or(said))
}

fn newest_picture(folder: &Path) -> Option<PathBuf> {
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "png"))
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map(|entry| entry.path())
}

fn save_pet(dir: &Path, arguments: &Value) -> Result<String, String> {
    let name = characters::clean_name(arguments.get("name").and_then(Value::as_str).unwrap_or_default());
    if name.is_empty() {
        return Err("The pet needs a `name`.".into());
    }
    let path = |pose: &str| -> Result<PathBuf, String> {
        let path = absolute(
            arguments.get(pose).and_then(Value::as_str).ok_or_else(|| format!("`{pose}` is required"))?,
            &format!("`{pose}`"),
        )?;
        if path.is_file() { Ok(path) } else { Err(format!("No picture at {}", path.display())) }
    };
    let sources = [path(POSES[0])?, path(POSES[1])?, path(POSES[2])?, path(POSES[3])?];
    let prepared = characters::prepare(&sources)?;
    let mut report = prepared.lines.clone();
    report.extend(prepared.warnings.iter().map(|warning| format!("warning: {warning}")));
    if arguments.get("check_only").and_then(Value::as_bool).unwrap_or(false) {
        let sheet = sources[0].with_file_name("preview.png");
        prepared
            .preview
            .save_with_format(&sheet, image::ImageFormat::Png)
            .map_err(|error| format!("Couldn't write the contact sheet: {error}"))?;
        report.push(format!(
            "Contact sheet (the poses side by side, eye boxes in red): {}. Nothing is saved yet.",
            sheet.display()
        ));
        return Ok(report.join("\n"));
    }
    let make_it_the_pet = arguments.get("make_it_the_pet").and_then(Value::as_bool).unwrap_or(true);
    let id = characters::save(dir, &name, &prepared, make_it_the_pet)?;
    report.push(format!(
        "Saved \"{name}\" (custom:{id}) among the user's characters: Settings › Desktop pet and Characters on App icon list it.{}",
        if make_it_the_pet { " It's the desktop pet now." } else { "" }
    ));
    Ok(report.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Role;

    #[test]
    fn codex_events_give_the_session_and_why_nothing_came() {
        let events = r#"{"type":"thread.started","thread_id":"01a13050-c3df-7512-8816-cb53946d294a"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I can't draw that."}}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":""}}"#;
        assert_eq!(
            read_events(events),
            (Some("01a13050-c3df-7512-8816-cb53946d294a".into()), Some("I can't draw that.".into()))
        );
        let failed = r#"{"type":"thread.started","thread_id":"../x"}
{"type":"turn.failed","error":{"message":"usage limit reached"}}"#;
        assert_eq!(read_events(failed), (None, Some("usage limit reached".into())));
    }

    #[test]
    fn codexs_pictures_show_in_the_chat_as_drawn() {
        use crate::mcp::Part;
        // As in the app: the data folder (`data`, with filo.db) and the agents' chats in agents/ below it.
        let data = crate::store::tests::temp_store("characters-mcp");
        let store = crate::store::Store::at(data.root().join("agents"));
        let chat = store.create_chat(Engine::Claude).unwrap();
        let mut target = Target {
            store: store.clone(),
            chat_id: chat.id.clone(),
            turn_id: Some("t1".into()),
            db: data.root().join(crate::db::FILE),
            group_speaker: None,
            own_chat: false,
            part: Part::All,
        };
        let drawn = store.root().join("pet/idle.png");
        std::fs::create_dir_all(drawn.parent().unwrap()).unwrap();
        std::fs::write(&drawn, b"codex's png").unwrap();
        show_in_chat(&target, &[drawn.as_path()]).unwrap();
        // The agent overwriting its file later doesn't change what the chat shows.
        std::fs::write(&drawn, b"traced svg render").unwrap();
        let messages = store.messages(&chat.id).unwrap();
        let shown = messages.last().unwrap();
        assert_eq!(
            (shown.role, shown.text.as_str(), shown.turn.as_deref()),
            (Role::Assistant, "Drawn by Codex.", Some("t1"))
        );
        assert_eq!(
            (shown.attachments.len(), shown.attachments[0].name.as_str(), shown.attachments[0].content_type.as_str()),
            (1, "idle.png", "image/png")
        );
        assert_eq!(std::fs::read(&shown.attachments[0].file).unwrap(), b"codex's png");
        // Kept with the user's files (data/attachments/), not in the chats' folder.
        let kept = &shown.attachments[0];
        assert_eq!(crate::attachments::file(data.root(), &kept.storage_id).as_ref(), Some(&kept.file));
        assert!(crate::attachments::file(store.root(), &kept.storage_id).is_none());
        assert_eq!(drawn_caption(3), "Drawn by Codex (3 pictures).");
        // A picture that can't be kept shows none of them, and leaves no copy behind.
        let before = std::fs::read_dir(data.root().join(crate::attachments::DIR)).unwrap().count();
        let missing = store.root().join("pet/gone.png");
        assert!(show_in_chat(&target, &[drawn.as_path(), missing.as_path()]).is_err());
        assert_eq!(std::fs::read_dir(data.root().join(crate::attachments::DIR)).unwrap().count(), before);
        assert_eq!(store.messages(&chat.id).unwrap().len(), messages.len());
        // In the group, the agent posts them itself with send_message's `files`, told so on a line of its own.
        target.group_speaker = Some(crate::apps::FILO_AGENT.into());
        let told = told_after(&target, &[drawn.as_path()]);
        assert_eq!(told[0], "In the group, post them yourself: send_message with `files` set to these paths.");
        assert_eq!(store.messages(&chat.id).unwrap().len(), messages.len());
        std::fs::remove_dir_all(data.root()).ok();
    }

    #[test]
    fn requests_are_checked() {
        let one = |image: Value| requests(&json!({ "images": [image] }));
        assert!(one(json!({ "prompt": "a cat", "path": "/tmp/x/idle.png" })).is_ok());
        assert!(one(json!({ "prompt": "a cat", "path": "idle.png" })).is_err());
        assert!(one(json!({ "prompt": "a cat", "path": "/tmp/idle.jpg" })).is_err());
        assert!(one(json!({ "prompt": " ", "path": "/tmp/idle.png" })).is_err());
        assert!(
            one(json!({ "prompt": "a cat", "path": "/tmp/idle.png", "references": ["/nowhere/at/all.png"] })).is_err()
        );
        assert!(requests(&json!({ "images": [] })).is_err());
        // Only new files: not over one that's there, nor through a link, even one to nothing yet.
        let dir = crate::test_support::temp_dir("generate-new-only");
        std::fs::create_dir_all(&dir).unwrap();
        let (theirs, link, dangling) = (dir.join("theirs.png"), dir.join("link.png"), dir.join("dangling.png"));
        std::fs::write(&theirs, b"the user's").unwrap();
        std::os::unix::fs::symlink(&theirs, &link).unwrap();
        std::os::unix::fs::symlink(dir.join("nothing-yet.txt"), &dangling).unwrap();
        for path in [&theirs, &link, &dangling] {
            let refused = one(json!({ "prompt": "a cat", "path": path.display().to_string() }));
            assert!(refused.is_err_and(|why| why.contains("There's already something at")), "{}", path.display());
            assert!(write_new(path, b"png").is_err());
        }
        assert_eq!(std::fs::read(&theirs).unwrap(), b"the user's");
        assert!(!dir.join("nothing-yet.txt").exists());
        write_new(&dir.join("new.png"), b"png").unwrap();
        assert_eq!(std::fs::read(dir.join("new.png")).unwrap(), b"png");
        std::fs::remove_dir_all(&dir).ok();
        let five = json!({ "images": vec![json!({ "prompt": "a", "path": "/tmp/a.png" }); 5] });
        assert!(requests(&five).is_err());
        let prompt = codex_prompt(&Request {
            prompt: "a cat".into(),
            path: "/tmp/a.png".into(),
            references: vec!["/tmp/idle.png".into()],
        });
        assert!(
            prompt.contains("referenced_image_paths: \"/tmp/idle.png\"") && prompt.ends_with("Picture prompt: a cat\n")
        );
    }
}

// The user's skills: folders on this Mac (`<root>/skills/<name>/`, beside filo.db), each a skill in the Agent Skills
// format: a `SKILL.md` and whatever else it holds (scripts, templates, references), kept exactly as they are. That
// folder is the only copy. The Skills page (`skills:list/save/remove/import`) reads and edits it, and an agent installs
// a skill by copying its folder there with its own tools (`install_line` tells it where).
//
// A `SKILL.md` is front matter (the skill's name: lowercase letters, digits and dashes, at most 64, the same as its
// folder's; a line on when to use it, at most 1024 characters, as the standard allows; any other fields, kept as
// written) and the instructions after it (at most 20 000 UTF-16 units from the page, as it counts). Saving a name that
// exists replaces its name, description and instructions and leaves the rest of the folder alone. Names are tidied the
// way the page expects: "Weekly Report" is saved as "weekly-report".
//
// Each is also a real skill of both CLIs: a copy of its whole folder in the workspace they run in (store.rs
// `write_skills`), which Claude Code finds in `.claude/skills` and Codex in `.agents/skills`. A turn sees those, Filo's
// own (`FILO_SKILLS`: making a desktop pet, making artifacts) and the document and chart skills (`BUILT_IN`), nothing
// else (engine.rs), and gets the instructions of the skills its message names (`@skills:name`, or `/name`) with the
// message. Skills were rows of Filo's database before; `migrate` moves them into folders once, as the app opens.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::Db;

/// The skills table skills were kept in before they were folders (db.rs `MIGRATIONS`): emptied by `migrate`.
pub const SCHEMA: &str = "
CREATE TABLE skills (
    name        TEXT PRIMARY KEY NOT NULL,
    description TEXT NOT NULL,
    body        TEXT NOT NULL,
    source      TEXT NOT NULL DEFAULT 'owner',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
";

/// The skills' folder, beside the database.
pub const DIR: &str = "skills";
pub const MAX_NAME: usize = 64;
/// The Agent Skills standard's limit.
pub const MAX_DESCRIPTION: usize = 1024;
/// In UTF-16 units, like the skill page's limit.
pub const MAX_BODY: usize = 20_000;
/// What starts the name of a file Filo is writing (renamed into place when done): left alone by every copy.
pub const TEMP: &str = ".filo-";
/// How deep a skill's folders are copied.
const MAX_DEPTH: usize = 16;

/// A skill as the pages read it: its `SKILL.md`, and what else its folder holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// Its folder's name.
    pub name: String,
    pub description: String,
    pub body: String,
    pub folder: PathBuf,
    /// The folder's other files, by their path in it ("scripts/check.py"), sorted.
    pub files: Vec<String>,
    /// When its `SKILL.md` last changed, in ms.
    pub updated_at: i64,
}

/// What `skills:save` is given.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Fields {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body: String,
}

/// The skills' folder for the database at `db_path`.
pub fn folder_for(db_path: &Path) -> PathBuf {
    db_path.parent().unwrap_or(Path::new(".")).join(DIR)
}

// ---- SKILL.md ----

/// A `SKILL.md` as read: the front matter's name and description (on one line), its other fields as written, and the
/// instructions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillFile {
    pub name: Option<String>,
    pub description: Option<String>,
    /// The front matter's other top-level fields (license, metadata, allowed-tools…), each with its own lines.
    pub other: Vec<String>,
    pub body: String,
}

/// Reads a `SKILL.md`: YAML front matter between `---` lines, then the instructions. Only what Filo needs of YAML is
/// read (plain, quoted and block values, indented lines going with the field above); the rest is kept as it is.
pub fn parse(text: &str) -> SkillFile {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let whole = || SkillFile { body: text.trim().to_string(), ..SkillFile::default() };
    let Some(rest) = text.strip_prefix("---\n") else { return whole() };
    let lines: Vec<&str> = rest.split('\n').collect();
    let Some(end) = lines.iter().position(|line| matches!(line.trim_end(), "---" | "...")) else { return whole() };
    let mut file = SkillFile { body: lines[end + 1..].join("\n").trim().to_string(), ..SkillFile::default() };
    // Each field: a line at the left edge, and the indented (or blank) lines under it.
    let mut fields: Vec<Vec<&str>> = Vec::new();
    for line in &lines[..end] {
        let continues = line.trim().is_empty() || line.starts_with([' ', '\t']);
        match fields.last_mut() {
            Some(field) if continues => field.push(line),
            _ => fields.push(vec![line]),
        }
    }
    for field in fields {
        let key = field[0].split_once(':').map(|(key, value)| (key.trim(), value));
        match key {
            Some(("name", value)) => file.name = Some(scalar(value, &field[1..])),
            Some(("description", value)) => file.description = Some(scalar(value, &field[1..])),
            _ => {
                let lines = field.join("\n");
                if !lines.trim().is_empty() {
                    file.other.push(lines.trim_end().to_string());
                }
            }
        }
    }
    file
}

/// A field's value on one line: `first` is what follows its colon, `more` the lines under it.
fn scalar(first: &str, more: &[&str]) -> String {
    let first = first.trim();
    let joined = std::iter::once(first).chain(more.iter().map(|line| line.trim())).collect::<Vec<_>>().join(" ");
    let value = if first.starts_with(['|', '>']) {
        more.iter().map(|line| line.trim()).collect::<Vec<_>>().join(" ")
    } else if joined.starts_with('"') {
        let quoted = joined.trim_end();
        serde_json::from_str::<String>(quoted).unwrap_or_else(|_| quoted.trim_matches('"').to_string())
    } else if joined.starts_with('\'') {
        joined.trim_end().trim_matches('\'').replace("''", "'")
    } else {
        joined
    };
    one_line(&value)
}

/// A `SKILL.md` for `name`: its name and description in the front matter (the description quoted as JSON, which YAML
/// reads the same), the `other` fields it had, then the instructions.
fn file_text(name: &str, description: &str, other: &[String], body: &str) -> String {
    let quoted = serde_json::to_string(description).unwrap_or_else(|_| "\"\"".into());
    let other: String = other.iter().map(|field| format!("{field}\n")).collect();
    format!("---\nname: {name}\ndescription: {quoted}\n{other}---\n\n{}\n", body.trim_end())
}

/// Lowercase ASCII letters and digits, other runs of characters as one dash, none at the ends, at most 64.
pub fn clean_name(name: &str) -> String {
    let mut clean = String::new();
    for char in name.trim().chars() {
        if char.is_ascii_alphanumeric() {
            clean.push(char.to_ascii_lowercase());
        } else if (char.is_whitespace() || "-_.".contains(char)) && !clean.is_empty() && !clean.ends_with('-') {
            clean.push('-');
        }
    }
    let clean: String = clean.chars().take(MAX_NAME).collect();
    clean.trim_end_matches('-').to_string()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `\n` line ends, trimmed.
fn clean_body(body: &str) -> String {
    body.replace("\r\n", "\n").replace('\r', "\n").trim().to_string()
}

// ---- the folders ----

/// Every skill in `dir`, by name: each folder (or link to one) named as a skill is, with a `SKILL.md`.
pub fn list(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut skills: Vec<Skill> =
        entries.flatten().filter_map(|entry| read(dir, &entry.file_name().to_string_lossy())).collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// The skill `name` in `dir`, if it's there.
pub fn read(dir: &Path, name: &str) -> Option<Skill> {
    if name.is_empty() || clean_name(name) != name {
        return None;
    }
    let folder = dir.join(name);
    let skill_md = folder.join("SKILL.md");
    let text = fs::read_to_string(&skill_md).ok()?;
    let file = parse(&text);
    let updated_at = fs::metadata(&skill_md)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_millis() as i64);
    let mut files = Vec::new();
    files_in(&folder, "", 0, &mut files);
    files.retain(|file| file != "SKILL.md");
    files.sort();
    Some(Skill {
        name: name.to_string(),
        description: file.description.unwrap_or_default(),
        body: file.body,
        folder,
        files,
        updated_at,
    })
}

fn files_in(folder: &Path, prefix: &str, depth: usize, files: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if skipped(&name) || depth > MAX_DEPTH {
            continue;
        }
        let path = format!("{prefix}{}", name.to_string_lossy());
        match fs::metadata(entry.path()) {
            Ok(meta) if meta.is_dir() => files_in(&entry.path(), &format!("{path}/"), depth + 1, files),
            Ok(meta) if meta.is_file() => files.push(path),
            _ => {}
        }
    }
}

/// What a skill's copies leave out: a repository's history, Finder's files and Filo's own half-written ones.
fn skipped(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name == ".git" || name == ".DS_Store" || name.starts_with(TEMP)
}

/// A mark that changes when a skill in `dir` comes, goes or has its `SKILL.md` changed (for the live Skills page).
pub fn version(dir: &Path) -> Vec<(String, Option<std::time::SystemTime>)> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut marks: Vec<_> = entries
        .flatten()
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().to_string(),
                fs::metadata(entry.path().join("SKILL.md")).and_then(|meta| meta.modified()).ok(),
            )
        })
        .collect();
    marks.sort();
    marks
}

/// Makes the skill, or changes the one with its name (its other files and front matter fields stay); the name it's
/// saved under.
pub fn save(dir: &Path, fields: &Fields) -> Result<String, String> {
    let name = clean_name(&fields.name);
    let description = one_line(&fields.description);
    let body = clean_body(&fields.body);
    if name.is_empty() || description.is_empty() || body.is_empty() {
        return Err(
            "A skill needs a name (letters, digits and dashes), a line on when to use it and its instructions.".into(),
        );
    }
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(format!("“When to use it” is too long (at most {MAX_DESCRIPTION} characters)."));
    }
    if body.encode_utf16().count() > MAX_BODY {
        return Err(format!("The instructions are too long (at most {MAX_BODY} characters)."));
    }
    let folder = dir.join(&name);
    let other = fs::read_to_string(folder.join("SKILL.md")).map(|text| parse(&text).other).unwrap_or_default();
    write_atomic(&folder, "SKILL.md", &file_text(&name, &description, &other, &body))
        .map_err(|error| format!("Couldn't save the skill in {}: {error}", folder.display()))?;
    Ok(name)
}

/// Removes the skill's folder (a link to one: just the link). False when there's no such skill.
pub fn remove(dir: &Path, name: &str) -> Result<bool, String> {
    let name = clean_name(name);
    if name.is_empty() {
        return Ok(false);
    }
    let folder = dir.join(&name);
    let removed = match fs::symlink_metadata(&folder) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => Err(error),
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(&folder),
        Ok(_) => fs::remove_file(&folder),
    };
    removed.map(|_| true).map_err(|error| format!("Couldn't remove {}: {error}", folder.display()))
}

/// Copies the skill folder at `from` (a `SKILL.md` and all beside it) into `dir` as a new skill, named as its front
/// matter says (else as its folder is); the name it got. A skill by that name already there is left alone.
pub fn import(dir: &Path, from: &Path) -> Result<String, String> {
    let text = fs::read_to_string(from.join("SKILL.md")).map_err(|_| format!("{} has no SKILL.md.", from.display()))?;
    let file = parse(&text);
    let named = file
        .name
        .clone()
        .filter(|name| !clean_name(name).is_empty())
        .or_else(|| from.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_default();
    let name = clean_name(&named);
    if name.is_empty() {
        return Err("That skill has no name (letters, digits and dashes) in its SKILL.md or its folder.".into());
    }
    let folder = dir.join(&name);
    if fs::symlink_metadata(&folder).is_ok() {
        return Err(format!("There's already a skill named {name}. Remove it first to import this one."));
    }
    fs::create_dir_all(dir).map_err(|error| format!("Couldn't make {}: {error}", dir.display()))?;
    let temp = dir.join(format!("{TEMP}import-{}", uuid::Uuid::new_v4()));
    let copied = mirror(from, &temp).and_then(|_| {
        // Its name as its folder's, as the standard has it.
        if file.name.as_deref() != Some(name.as_str()) {
            write_atomic(
                &temp,
                "SKILL.md",
                &file_text(&name, &file.description.clone().unwrap_or_default(), &file.other, &file.body),
            )?;
        }
        fs::rename(&temp, &folder)
    });
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&temp);
        return Err(format!("Couldn't copy the skill from {}: {error}", from.display()));
    }
    Ok(name)
}

/// `to` as a copy of the folder `from`, every file in it byte for byte (with its permissions, so scripts still run):
/// what `from` lacks goes, and only a file that changed is written (through a temporary file and a rename, so a CLI
/// starting meanwhile never reads half of one). Links are followed.
pub fn mirror(from: &Path, to: &Path) -> io::Result<()> {
    mirror_at(from, to, 0)
}

fn mirror_at(from: &Path, to: &Path, depth: usize) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            fs::remove_file(to)?;
            fs::create_dir_all(to)?;
        }
        Err(_) => fs::create_dir_all(to)?,
    }
    let mut kept: HashSet<std::ffi::OsString> = HashSet::new();
    for entry in fs::read_dir(from)?.flatten() {
        let name = entry.file_name();
        if skipped(&name) {
            continue;
        }
        // A broken link, or something that's neither a file nor a folder, isn't copied.
        let Ok(meta) = fs::metadata(entry.path()) else { continue };
        let target = to.join(&name);
        if meta.is_dir() && depth < MAX_DEPTH {
            mirror_at(&entry.path(), &target, depth + 1)?;
        } else if meta.is_file() {
            copy_if_changed(&entry.path(), &target, &meta)?;
        } else {
            continue;
        }
        kept.insert(name);
    }
    for entry in fs::read_dir(to)?.flatten() {
        let name = entry.file_name();
        if !kept.contains(&name) && !name.to_string_lossy().starts_with(TEMP) {
            remove_path(&entry.path());
        }
    }
    Ok(())
}

fn copy_if_changed(from: &Path, to: &Path, meta: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(old) = fs::symlink_metadata(to) {
        if old.is_file()
            && old.len() == meta.len()
            && old.permissions().mode() == meta.permissions().mode()
            && fs::read(to)? == fs::read(from)?
        {
            return Ok(());
        }
        if old.is_dir() {
            fs::remove_dir_all(to)?;
        }
    }
    let temp = to.with_file_name(format!("{TEMP}{}", uuid::Uuid::new_v4()));
    fs::copy(from, &temp)?;
    fs::rename(&temp, to)
}

/// `folder/file` holding `text`, the folder made if missing, through a temporary file and a rename.
pub fn write_atomic(folder: &Path, file: &str, text: &str) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    let temp = folder.join(format!("{TEMP}{file}.{}", uuid::Uuid::new_v4()));
    fs::write(&temp, text)?;
    fs::rename(&temp, folder.join(file))
}

/// Removes a file, link or folder, as best it can.
pub fn remove_path(path: &Path) {
    let _ = if path.is_dir() && !path.is_symlink() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
}

/// `app_state`: the skills table was emptied into folders (`migrate`), so it's never read again.
const MOVED: &str = "skills_moved";

/// Moves the skills the database still has (from before they were folders) into `dir`, each as a folder with its
/// `SKILL.md`, then empties the table and marks it done (`MOVED`); how many moved. A folder already there by a skill's
/// name is the one kept. The app runs it as it opens; once done, it reads nothing.
pub fn migrate(db: &Db, dir: &Path) -> Result<usize, String> {
    if db.with(|conn| crate::app_data::state(conn, MOVED))?.is_some() {
        return Ok(0);
    }
    let rows: Vec<(String, String, String)> = db.with(|conn| {
        conn.prepare_cached("SELECT name, description, body FROM skills ORDER BY name")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect()
    })?;
    let mut moved = 0;
    for (name, description, body) in &rows {
        if name.is_empty() || clean_name(name) != *name {
            continue;
        }
        let folder = dir.join(name);
        if fs::symlink_metadata(folder.join("SKILL.md")).is_ok() {
            continue;
        }
        write_atomic(&folder, "SKILL.md", &file_text(name, &one_line(description), &[], body))
            .map_err(|error| format!("Couldn't move the skill {name} to {}: {error}", folder.display()))?;
        moved += 1;
    }
    db.write(|tx| {
        tx.execute("DELETE FROM skills", [])?;
        crate::app_data::set_state(tx, MOVED, "1")
    })?;
    Ok(moved)
}

// ---- in a turn ----

/// The skills for a turn whose database is at `db_path`: those in its skills folder (the app moved any the database
/// had there as it opened: `migrate`).
pub fn load(db_path: &Path) -> Vec<Skill> {
    list(&folder_for(db_path))
}

/// For a turn's instructions: where the user's skills are and how one is installed, with the turn's own tools.
pub fn install_line(dir: &Path) -> String {
    format!(
        "The user's skills (the Agent Skills format) are folders in {dir}, one per skill and the only copy of it: \
each holds its SKILL.md (front matter with `name`, the folder's own name in lowercase letters, digits and dashes, and `description`, \
when to use it in at most 1024 characters, then the instructions) and anything beside it (scripts, templates, references). \
To install a skill the user asks for, copy its whole folder there as it is, keeping every file; to change one, edit its files there. \
Filo's Skills page shows them, and they're yours as skills from your next turn. \
The .claude/skills and .agents/skills folders where you work are copies Filo makes again before each turn: anything put there is lost.",
        dir = dir.display()
    )
}

/// The skills a chat turn has besides the user's (engine.rs): Claude Code's own (pdf, docx, xlsx and pptx synced from
/// claude.ai, dataviz bundled); for Codex, its document plugins and Filo's dataviz (`DATAVIZ`).
pub const BUILT_IN: [&str; 5] = ["pdf", "docx", "xlsx", "pptx", "dataviz"];

/// Filo's own dataviz skill, for Codex, which has none (Claude Code's is bundled).
pub const DATAVIZ: &str = include_str!("dataviz_skill.md");

/// Filo's skill for making a desktop pet (characters_mcp.rs).
const DESKTOP_PET_NAME: &str = "desktop-pet";
pub(crate) const DESKTOP_PET: &str = include_str!("desktop_pet_skill.md");

/// Filo's skill for making and changing artifacts (artifacts_mcp.rs), modelled on the project-artifact and
/// web-artifacts-builder skills: a page built as a file from its template (the theme marker and Filo's house style),
/// checked for overflow (its `check_page.py`) and published with publish_artifact.
pub const ARTIFACTS_NAME: &str = "artifacts";
pub(crate) const ARTIFACTS: &str = include_str!("artifacts_skill/SKILL.md");
pub(crate) const ARTIFACTS_TEMPLATE: &str = include_str!("artifacts_skill/template.html");
const ARTIFACTS_CHECK: &str = include_str!("artifacts_skill/check_page.py");

/// Filo's own skills, which every turn of each CLI has beside the user's (store.rs `write_skills`): each one's name and
/// the files in its folder. A skill of the user's by one of these names takes its place.
pub const FILO_SKILLS: [(&str, &[(&str, &str)]); 2] = [
    (DESKTOP_PET_NAME, &[("SKILL.md", DESKTOP_PET)]),
    (
        ARTIFACTS_NAME,
        &[("SKILL.md", ARTIFACTS), ("template.html", ARTIFACTS_TEMPLATE), ("check_page.py", ARTIFACTS_CHECK)],
    ),
];

/// Whether `name` is one of Filo's own skills.
pub fn is_filo_skill(name: &str) -> bool {
    FILO_SKILLS.iter().any(|(own, _)| *own == name)
}

/// The skills `text` names: `@skills:name` anywhere, or `/name` starting a word; in the order named, once each.
fn mentioned<'a>(text: &str, skills: &'a [Skill]) -> Vec<&'a Skill> {
    let mut named: Vec<&Skill> = Vec::new();
    let mut previous: Option<char> = None;
    for (at, char) in text.char_indices() {
        let starts_word = previous.is_none_or(char::is_whitespace);
        previous = Some(char);
        let rest = if char == '@' {
            match text[at + 1..].strip_prefix("skills:").or_else(|| text[at + 1..].strip_prefix("skill:")) {
                Some(rest) => rest,
                None => continue,
            }
        } else if char == '/' && starts_word {
            &text[at + 1..]
        } else {
            continue;
        };
        let word: String = rest.chars().take_while(|char| char.is_ascii_alphanumeric() || *char == '-').collect();
        let word = word.trim_end_matches('-');
        if let Some(skill) = skills.iter().find(|skill| skill.name == word)
            && !named.iter().any(|seen| seen.name == skill.name)
        {
            named.push(skill);
        }
    }
    named
}

/// A skill's instructions, as a turn whose message names it gets them: with its folder when it has files besides.
fn instructions_of(skill: &Skill) -> String {
    let folder = if skill.files.is_empty() {
        String::new()
    } else {
        format!("Its folder, with the files the instructions name: {}\n\n", skill.folder.display())
    };
    format!("# Skill: {}\n\nWhen to use it: {}\n\n{folder}{}", skill.name, skill.description, skill.body)
}

/// The turn's prompt with the instructions of the skills its message names after it.
pub fn with_mentioned(prompt: &str, message: &str, skills: &[Skill]) -> String {
    let named = mentioned(message, skills);
    if named.is_empty() {
        return prompt.to_string();
    }
    let attached = named.iter().map(|skill| instructions_of(skill)).collect::<Vec<_>>().join("\n\n");
    format!(
        "{prompt}\n\n---\nThe user's message names these skills of theirs. Follow their instructions for it:\n\n{attached}"
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::tests::temp_path;

    fn fields(name: &str, description: &str, body: &str) -> Fields {
        Fields { name: name.into(), description: description.into(), body: body.into() }
    }

    /// An empty folder for skills, gone with its parent at the end of the test.
    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("filo-local-test-{name}-{}", uuid::Uuid::new_v4())).join(DIR);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn done(dir: &Path) {
        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    /// A skill folder like one from a repository: front matter with fields Filo doesn't use, scripts (one runnable),
    /// a binary file and references in folders of their own.
    pub(crate) fn repo_skill(at: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let description = format!(
            "Record a narrated demo video of a page: {}",
            "framed as a phone, scripted taps, voiceover. ".repeat(9)
        );
        fs::create_dir_all(at.join("scripts")).unwrap();
        fs::create_dir_all(at.join("references/deep")).unwrap();
        fs::write(at.join("SKILL.md"), format!("---\nname: {name}\ndescription: {description}\nlicense: MIT\nmetadata:\n  author: bitooz\n  version: \"1.2\"\n---\n\n# Demo\n\nRun `scripts/record.sh`.\n")).unwrap();
        fs::write(at.join("scripts/record.sh"), "#!/bin/sh\necho recorded\n").unwrap();
        fs::set_permissions(at.join("scripts/record.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(at.join("scripts/parts.tar.gz"), [0x1f, 0x8b, 0, 0xff, 0x10, 0]).unwrap();
        fs::write(at.join("references/deep/notes.md"), "Notes.").unwrap();
        fs::create_dir_all(at.join(".git")).unwrap();
        fs::write(at.join(".git/HEAD"), "ref").unwrap();
    }

    #[test]
    fn skills_are_folders_made_changed_and_removed() {
        let dir = temp_dir("skills");
        assert!(list(&dir).is_empty());
        assert_eq!(
            save(&dir, &fields("Weekly Report", "  When asked for\n a report ", "\r\n1. Group by project.\r\n"))
                .unwrap(),
            "weekly-report"
        );
        assert_eq!(
            save(&dir, &fields("release-notes", "When writing release notes", "Group by screen.")).unwrap(),
            "release-notes"
        );
        let names: Vec<String> = list(&dir).into_iter().map(|skill| skill.name).collect();
        assert_eq!(names, ["release-notes", "weekly-report"]);
        let report = read(&dir, "weekly-report").unwrap();
        assert_eq!(
            (report.description.as_str(), report.body.as_str()),
            ("When asked for a report", "1. Group by project.")
        );
        assert_eq!(
            fs::read_to_string(dir.join("weekly-report/SKILL.md")).unwrap(),
            "---\nname: weekly-report\ndescription: \"When asked for a report\"\n---\n\n1. Group by project.\n"
        );
        assert_eq!((report.folder.clone(), report.files.clone()), (dir.join("weekly-report"), Vec::<String>::new()));

        // Saving the name again changes it; the folder's other files and front matter fields stay.
        fs::write(dir.join("weekly-report/SKILL.md"), "---\nlicense: MIT\nname: weekly-report\ndescription: >\n  When asked\n  for a report\nmetadata:\n  author: me\n---\n1. Group by project.\n").unwrap();
        fs::create_dir_all(dir.join("weekly-report/scripts")).unwrap();
        fs::write(dir.join("weekly-report/scripts/dates.py"), "print()").unwrap();
        assert_eq!(read(&dir, "weekly-report").unwrap().description, "When asked for a report");
        save(
            &dir,
            &fields(
                "weekly-report",
                "When asked for a \"weekly\" report: by project",
                "1. Group by project.\n2. Add dates.",
            ),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("weekly-report/SKILL.md")).unwrap(),
            "---\nname: weekly-report\ndescription: \"When asked for a \\\"weekly\\\" report: by project\"\nlicense: MIT\nmetadata:\n  author: me\n---\n\n1. Group by project.\n2. Add dates.\n"
        );
        let changed = read(&dir, "weekly-report").unwrap();
        assert_eq!(
            (changed.description.as_str(), changed.body.as_str()),
            ("When asked for a \"weekly\" report: by project", "1. Group by project.\n2. Add dates.")
        );
        assert_eq!(changed.files, ["scripts/dates.py"]);
        assert_eq!(list(&dir).len(), 2);

        assert!(remove(&dir, "weekly-report").unwrap());
        assert!(!remove(&dir, "weekly-report").unwrap());
        assert!(!dir.join("weekly-report").exists());
        assert_eq!(read(&dir, "weekly-report"), None);
        assert_eq!(list(&dir).len(), 1);
        // A folder that isn't a skill (no SKILL.md, or a name that isn't one) isn't listed.
        fs::create_dir_all(dir.join("empty")).unwrap();
        fs::create_dir_all(dir.join("Not A Name")).unwrap();
        fs::write(dir.join("Not A Name/SKILL.md"), "x").unwrap();
        assert_eq!(list(&dir).into_iter().map(|skill| skill.name).collect::<Vec<_>>(), ["release-notes"]);
        done(&dir);
    }

    #[test]
    fn what_cant_be_saved_is_refused() {
        let dir = temp_dir("skills-refused");
        assert!(save(&dir, &fields("", "When", "Do")).unwrap_err().contains("needs a name"));
        assert!(save(&dir, &fields("ü!", "When", "Do")).unwrap_err().contains("needs a name"));
        assert!(save(&dir, &fields("ok", " ", "Do")).is_err());
        assert!(save(&dir, &fields("ok", "When", "\n")).is_err());
        // The standard's 1024 characters, not Filo's old 300.
        assert!(save(&dir, &fields("ok", &"x".repeat(MAX_DESCRIPTION + 1), "Do")).unwrap_err().contains("too long"));
        assert!(save(&dir, &fields("long", &"x".repeat(MAX_DESCRIPTION), "Do")).is_ok());
        // Cut by UTF-16 units: an emoji is two.
        assert!(save(&dir, &fields("ok", "When", &"😀".repeat(MAX_BODY / 2 + 1))).unwrap_err().contains("too long"));
        assert!(save(&dir, &fields("ok", "When", &"😀".repeat(MAX_BODY / 2))).is_ok());
        assert_eq!(list(&dir).len(), 2);
        // Nothing half-written is left about.
        assert!(fs::read_dir(dir.join("ok")).unwrap().flatten().all(|entry| entry.file_name() == "SKILL.md"));
        done(&dir);
    }

    #[test]
    fn skill_files_are_read_as_written() {
        let file = parse(
            "---\nname: release-notes\ndescription: \"When writing notes: \\\"by screen\\\".\"\nlicense: MIT\n---\n\n1. Group by screen.\n",
        );
        assert_eq!(file.name.as_deref(), Some("release-notes"));
        assert_eq!(file.description.as_deref(), Some("When writing notes: \"by screen\"."));
        assert_eq!((file.other, file.body.as_str()), (vec!["license: MIT".to_string()], "1. Group by screen."));
        assert_eq!(parse("Just steps."), SkillFile { body: "Just steps.".into(), ..SkillFile::default() });
        assert_eq!(parse("---\nname: x\nno end").body, "---\nname: x\nno end");
        // Plain values that go on under them, single quotes, block values, comments and CRLF.
        let file = parse(
            "\u{feff}---\r\n# A comment\r\nname: 'it''s'\r\ndescription: Use when\r\n  asked: twice\r\nallowed-tools: Bash\r\n---\r\nBody\r\n",
        );
        assert_eq!((file.name.as_deref(), file.description.as_deref()), (Some("it's"), Some("Use when asked: twice")));
        assert_eq!(file.other, ["# A comment", "allowed-tools: Bash"]);
        assert_eq!(
            parse("---\ndescription: |-\n  Line one.\n\n  Line two.\n---\n").description.as_deref(),
            Some("Line one. Line two.")
        );
        // Written back: the same fields.
        let text = file_text("its", "Use when asked: twice", &file.other, "Body");
        let again = parse(&text);
        assert_eq!(
            (again.name.as_deref(), again.description.as_deref(), again.other, again.body.as_str()),
            (Some("its"), Some("Use when asked: twice"), file.other, "Body")
        );
    }

    #[test]
    fn names_are_tidied_like_the_page_expects() {
        assert_eq!(clean_name("  Weekly   Report "), "weekly-report");
        assert_eq!(clean_name("--png_to.svg--"), "png-to-svg");
        assert_eq!(clean_name("Café Notes!"), "caf-notes");
        assert_eq!(clean_name(&"a".repeat(80)).len(), MAX_NAME);
        assert_eq!(clean_name(&format!("{}-b", "a".repeat(63))), "a".repeat(63));
    }

    #[test]
    fn a_folder_from_a_repository_is_imported_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("skills-import");
        let repo = dir.parent().unwrap().join("repo");
        repo_skill(&repo.join("web-demo-video"), "web-demo-video");
        assert_eq!(import(&dir, &repo.join("web-demo-video")).unwrap(), "web-demo-video");
        let skill = read(&dir, "web-demo-video").unwrap();
        // Its description is past Filo's old 300 characters, and kept whole.
        assert!(
            skill.description.chars().count() > 300
                && skill.description.starts_with("Record a narrated demo video of a page: framed"),
            "{}",
            skill.description
        );
        assert_eq!(skill.files, ["references/deep/notes.md", "scripts/parts.tar.gz", "scripts/record.sh"]);
        assert_eq!(
            fs::read(dir.join("web-demo-video/SKILL.md")).unwrap(),
            fs::read(repo.join("web-demo-video/SKILL.md")).unwrap()
        );
        assert_eq!(fs::read(dir.join("web-demo-video/scripts/parts.tar.gz")).unwrap(), [0x1f, 0x8b, 0, 0xff, 0x10, 0]);
        assert_eq!(
            fs::metadata(dir.join("web-demo-video/scripts/record.sh")).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!dir.join("web-demo-video/.git").exists());
        // Not twice.
        assert!(
            import(&dir, &repo.join("web-demo-video")).unwrap_err().contains("already a skill named web-demo-video")
        );
        // A folder whose name and front matter don't make a skill name: tidied, and its SKILL.md says so.
        fs::create_dir_all(repo.join("Odd Folder")).unwrap();
        fs::write(repo.join("Odd Folder/SKILL.md"), "---\ndescription: When odd\nlicense: MIT\n---\nDo it.").unwrap();
        assert_eq!(import(&dir, &repo.join("Odd Folder")).unwrap(), "odd-folder");
        let text = fs::read_to_string(dir.join("odd-folder/SKILL.md")).unwrap();
        assert_eq!(text, "---\nname: odd-folder\ndescription: \"When odd\"\nlicense: MIT\n---\n\nDo it.\n");
        assert!(import(&dir, &repo).unwrap_err().contains("no SKILL.md"));
        assert!(
            fs::read_dir(&dir).unwrap().flatten().all(|entry| !entry.file_name().to_string_lossy().starts_with(TEMP))
        );
        done(&dir);
    }

    #[test]
    fn skills_in_the_database_move_into_folders_once() {
        let path = temp_path("skills-migrate");
        let dir = folder_for(&path);
        {
            let db = Db::open(&path).unwrap();
            db.write(|tx| {
                tx.execute(
                    "INSERT INTO skills (name, description, body, source, created_at, updated_at) VALUES
                     ('png-to-svg', 'When turning a picture into SVG', 'Trace it.', 'owner', 1, 1),
                     ('kept', 'Old', 'Old body.', 'owner', 1, 1)",
                    [],
                )
            })
            .unwrap();
            // A folder already there by a skill's name is the skill.
            save(&dir, &fields("kept", "New", "New body.")).unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(migrate(&db, &dir).unwrap(), 1);
        let skills = load(&path);
        assert_eq!(
            skills.iter().map(|skill| (skill.name.as_str(), skill.body.as_str())).collect::<Vec<_>>(),
            [("kept", "New body."), ("png-to-svg", "Trace it.")]
        );
        assert_eq!(
            fs::read_to_string(dir.join("png-to-svg/SKILL.md")).unwrap(),
            "---\nname: png-to-svg\ndescription: \"When turning a picture into SVG\"\n---\n\nTrace it.\n"
        );
        // No copy left in the database: removing the folder removes the skill.
        assert_eq!(
            db.with(|conn| conn.query_row("SELECT count(*) FROM skills", [], |row| row.get::<_, i64>(0))).unwrap(),
            0
        );
        remove(&dir, "png-to-svg").unwrap();
        assert_eq!(load(&path).len(), 1);
        // Done once: the table isn't read again.
        db.write(|tx| tx.execute("INSERT INTO skills (name, description, body, source, created_at, updated_at) VALUES ('late', 'Late', 'Late.', 'owner', 1, 1)", [])).unwrap();
        assert_eq!(migrate(&db, &dir).unwrap(), 0);
        assert_eq!(load(&path).len(), 1);
        drop(db);
        fs::remove_dir_all(path.parent().unwrap()).ok();
        // No database yet: no skills, and none made.
        assert!(load(&path).is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn a_turn_gets_skill_folders_and_the_skills_its_message_names() {
        let dir = temp_dir("skills-turn");
        save(&dir, &fields("weekly-report", "When asked for a weekly report", "1. Group by project.")).unwrap();
        save(&dir, &fields("release-notes", "When writing release notes", "Group by screen.")).unwrap();
        fs::write(dir.join("release-notes/template.md"), "# Notes").unwrap();
        let skills = list(&dir);
        assert!(DATAVIZ.starts_with("---\nname: dataviz\ndescription: "));
        assert!(DESKTOP_PET.starts_with(&format!("---\nname: {DESKTOP_PET_NAME}\ndescription: \"")));

        let names =
            |text: &str| mentioned(text, &skills).into_iter().map(|skill| skill.name.as_str()).collect::<Vec<_>>();
        assert_eq!(
            names("Use @skills:weekly-report, then /release-notes. Again @skills:weekly-report"),
            ["weekly-report", "release-notes"]
        );
        assert_eq!(names("/weekly-report please"), ["weekly-report"]);
        // Not a skill, or a slash inside a word (a path): nothing.
        assert!(names("see a/weekly-report and @skills:nope or /weekly").is_empty());

        let prompt = with_mentioned("Write it.", "/weekly-report for this week", &skills);
        assert!(prompt.starts_with("Write it.\n\n---\nThe user's message names these skills"));
        assert!(prompt.ends_with(
            "# Skill: weekly-report\n\nWhen to use it: When asked for a weekly report\n\n1. Group by project."
        ));
        // One with files says where they are.
        let prompt = with_mentioned("Write it.", "/release-notes", &skills);
        assert!(
            prompt.ends_with(&format!(
                "Its folder, with the files the instructions name: {}\n\nGroup by screen.",
                dir.join("release-notes").display()
            )),
            "{prompt}"
        );
        assert_eq!(with_mentioned("Hi.", "hi", &skills), "Hi.");

        let line = install_line(&dir);
        assert!(
            line.contains(&format!("folders in {}", dir.display()))
                && line.contains("copy its whole folder there")
                && line.contains("1024"),
            "{line}"
        );
        done(&dir);
    }

    #[test]
    fn a_mirror_follows_its_folder_exactly() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("skills-mirror");
        let from = dir.join("web-demo-video");
        repo_skill(&from, "web-demo-video");
        let to = dir.parent().unwrap().join("copy/web-demo-video");
        fs::create_dir_all(to.join("stale")).unwrap();
        fs::write(to.join("old.txt"), "x").unwrap();
        mirror(&from, &to).unwrap();
        let mut copied = Vec::new();
        files_in(&to, "", 0, &mut copied);
        copied.sort();
        assert_eq!(copied, ["SKILL.md", "references/deep/notes.md", "scripts/parts.tar.gz", "scripts/record.sh"]);
        assert_eq!(fs::metadata(to.join("scripts/record.sh")).unwrap().permissions().mode() & 0o777, 0o755);
        // An unchanged file isn't written again; a changed one is, and one gone from the folder goes.
        let before = fs::metadata(to.join("SKILL.md")).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(from.join("scripts/record.sh"), "#!/bin/sh\necho again\n").unwrap();
        fs::remove_dir_all(from.join("references/deep")).unwrap();
        fs::write(from.join("references/new.md"), "New.").unwrap();
        mirror(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("scripts/record.sh")).unwrap(), "#!/bin/sh\necho again\n");
        assert!(!to.join("references/deep").exists() && to.join("references/new.md").exists());
        assert_eq!(fs::metadata(to.join("SKILL.md")).unwrap().modified().unwrap(), before);
        // The source is never touched.
        assert!(from.join(".git/HEAD").exists());
        done(&dir);
    }
}

// What the composer offers while an "@" or a "/" is typed: the chat's running tasks (its Working rows), agents (as
// before), Filo's browser ("@browser", always) and the computer ("@computer", while its switch is on) (crates/filo-local
// mentions.rs, which tells the turn which tools they mean), the user's apps that are connected ("@gmail"), their skills ("@skills:name", "/name") and the Coding page's
// projects ("@projects:name"). Picking one types it in, and the message's text is what counts: the backend attaches
// the skills it names (crates/filo-local skills.rs), and the agent reads the rest. A task isn't typed: picking it puts
// its chip on the composer (work_share.rs). Apps and skills load the first time an "@" or "/" is typed: the apps from
// the apps store (Extensions › Apps), the skills and the computer's switch from the local backend. The projects load
// only while "@projects:" (or the start of it) is typed.

use std::collections::HashSet;

use gpui::{App, AppContext as _, Context, Entity, EntityId, Global, Hsla, ImageSource, Task};
use serde::Deserialize;
use serde_json::json;

use super::work_share::SharedWork;
use crate::backend;
use crate::screens::connectors_store;

// "Your computer" and Filo's browser; the user's own apps (Extensions › Apps) come from the apps store, by their slug.
use filo_local::mentions::{BROWSER, COMPUTER, Named};

/// An app that's on.
#[derive(Clone, Debug, PartialEq)]
pub struct AppOn {
    pub id: String,
    pub name: String,
    pub handle: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SkillRow {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// A git repo on the Coding page (`coding:projects`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRow {
    pub name: String,
    pub path: String,
}

/// An agent "@" can name: its name, picture and the colour behind it (when the user chose one).
pub type Mention = (String, ImageSource, Option<Hsla>);

/// One row of the list.
#[derive(Clone)]
pub enum Suggestion {
    Agent {
        name: String,
        picture: ImageSource,
        background: Option<Hsla>,
    },
    App {
        id: String,
        name: String,
        handle: String,
    },
    /// "@skills:" itself, which then lists the skills.
    Skills,
    Skill {
        name: String,
        description: String,
        slash: bool,
    },
    /// A task running in this chat: picking it takes the "@" away and points the message at it.
    Work(SharedWork),
    /// "@projects:" itself, which then lists the projects.
    Projects,
    /// `handle`: what's typed after "@projects:", its name, or its path from home when another project has the name.
    Project {
        name: String,
        path: String,
        handle: String,
    },
}

impl Suggestion {
    /// What picking it types in place of the trigger and what follows it; a space after it unless more is to come.
    pub fn insert(&self) -> String {
        match self {
            Suggestion::Agent { name, .. } => format!("@{name} "),
            Suggestion::App { handle, .. } => format!("@{handle} "),
            Suggestion::Skills => "@skills:".to_string(),
            Suggestion::Skill { name, slash: true, .. } => format!("/{name} "),
            Suggestion::Skill { name, slash: false, .. } => format!("@skills:{name} "),
            Suggestion::Work(_) => String::new(),
            Suggestion::Projects => "@projects:".to_string(),
            Suggestion::Project { handle, .. } => format!("@projects:{handle} "),
        }
    }
}

/// Whether what follows an "@" is "@projects:" or on its way there ("@pro"), so the projects should load.
pub fn wants_projects(typed: &str) -> bool {
    let typed = typed.to_lowercase();
    match typed.split_once(':') {
        Some((handle, _)) => matches!(handle, "projects" | "project"),
        None => !typed.is_empty() && "projects".starts_with(&typed),
    }
}

/// The projects whose name or path holds `query`, those whose name starts with it first, as the Coding page's search.
fn project_rows(query: &str, projects: &[ProjectRow]) -> Vec<Suggestion> {
    let holds = |project: &ProjectRow| {
        project.name.to_lowercase().contains(query)
            || crate::screens::coding::short_path(&project.path).to_lowercase().contains(query)
    };
    let (mut first, rest): (Vec<&ProjectRow>, Vec<&ProjectRow>) = projects
        .iter()
        .filter(|project| holds(project))
        .partition(|project| project.name.to_lowercase().starts_with(query));
    first.extend(rest);
    first
        .into_iter()
        .map(|project| {
            let short = crate::screens::coding::short_path(&project.path);
            let shared = projects
                .iter()
                .any(|other| other.path != project.path && other.name.eq_ignore_ascii_case(&project.name));
            let handle = if shared { short.clone() } else { project.name.clone() };
            Suggestion::Project { name: project.name.clone(), path: short, handle }
        })
        .collect()
}

/// The rows for `typed` after `trigger` ('@' or '/'). `agents` and the running `work` are offered only with '@' and
/// no ':'.
#[allow(clippy::too_many_arguments)]
pub fn suggestions(
    trigger: char,
    typed: &str,
    agents: &[Mention],
    apps: &[AppOn],
    skills: &[SkillRow],
    work: &[SharedWork],
    projects: &[ProjectRow],
    max: usize,
) -> Vec<Suggestion> {
    let typed = typed.to_lowercase();
    let skill_rows = |prefix: &str, slash: bool| -> Vec<Suggestion> {
        skills
            .iter()
            .filter(|skill| skill.name.starts_with(prefix))
            .map(|skill| Suggestion::Skill { name: skill.name.clone(), description: skill.description.clone(), slash })
            .collect()
    };
    let rows = match trigger {
        '/' if typed.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => skill_rows(&typed, true),
        '@' => match typed.split_once(':') {
            Some(("skills" | "skill", prefix)) => skill_rows(prefix, false),
            Some(("projects" | "project", query)) => project_rows(query, projects),
            Some(_) => Vec::new(),
            // The chat's running tasks first (what's happening now, and few), then apps, then skills, then agents: with
            // many agents they'd fill the list.
            None => {
                let mut rows: Vec<Suggestion> =
                    work.iter().filter(|task| task.matches(&typed)).cloned().map(Suggestion::Work).collect();
                for app in apps
                    .iter()
                    .filter(|app| app.handle.starts_with(&typed) || app.name.to_lowercase().starts_with(&typed))
                {
                    rows.push(Suggestion::App {
                        id: app.id.clone(),
                        name: app.name.clone(),
                        handle: app.handle.clone(),
                    });
                }
                if !skills.is_empty() && "skills".starts_with(&typed) {
                    rows.push(Suggestion::Skills);
                }
                if "projects".starts_with(&typed) {
                    rows.push(Suggestion::Projects);
                }
                rows.extend(agents.iter().filter(|(name, _, _)| name.to_lowercase().starts_with(&typed)).map(
                    |(name, picture, background)| Suggestion::Agent {
                        name: name.clone(),
                        picture: picture.clone(),
                        background: *background,
                    },
                ));
                rows
            }
        },
        _ => Vec::new(),
    };
    rows.into_iter().take(max).collect()
}

/// The skills and the computer's switch, loaded once for every chat when first needed; the projects while a chat wants
/// them (the last list stays, so the next "@projects:" shows it at once).
pub struct MentionSources {
    pub skills: Vec<SkillRow>,
    pub computer: bool,
    pub projects: Vec<ProjectRow>,
    /// The chats typing "@projects:".
    wanting_projects: HashSet<EntityId>,
    projects_sub: Option<Task<()>>,
    _subs: Vec<Task<()>>,
}

impl MentionSources {
    /// Chat `chat` is typing "@projects:" (or isn't any more): the projects' query runs while any chat is.
    pub fn want_projects(&mut self, chat: EntityId, on: bool, cx: &mut Context<Self>) {
        let changed = if on { self.wanting_projects.insert(chat) } else { self.wanting_projects.remove(&chat) };
        if !changed {
            return;
        }
        if self.wanting_projects.is_empty() {
            self.projects_sub = None;
        } else if self.projects_sub.is_none() {
            self.projects_sub = Some(backend::subscribe(
                cx,
                "coding:projects",
                json!({}),
                |this: &mut MentionSources, result: Result<Vec<ProjectRow>, String>, cx| {
                    if let Ok(projects) = result
                        && projects != this.projects
                    {
                        this.projects = projects;
                        cx.notify();
                    }
                },
            ));
        }
    }
}

struct SourcesGlobal(Entity<MentionSources>);

impl Global for SourcesGlobal {}

/// The shared sources, and the connectors store asked for fresh statuses (it keeps them a while).
pub fn sources(cx: &mut App) -> Entity<MentionSources> {
    connectors_store::store(cx);
    if let Some(existing) = cx.try_global::<SourcesGlobal>() {
        return existing.0.clone();
    }
    let entity = cx.new(|cx: &mut Context<MentionSources>| MentionSources {
        skills: Vec::new(),
        computer: false,
        projects: Vec::new(),
        wanting_projects: HashSet::new(),
        projects_sub: None,
        _subs: vec![
            backend::subscribe(
                cx,
                "skills:list",
                json!({}),
                |this: &mut MentionSources, result: Result<Vec<SkillRow>, String>, cx| {
                    if let Ok(skills) = result {
                        this.skills = skills;
                        cx.notify();
                    }
                },
            ),
            backend::subscribe(
                cx,
                "computer:appsOn",
                json!({}),
                |this: &mut MentionSources, result: Result<bool, String>, cx| {
                    this.computer = result.unwrap_or(false);
                    cx.notify();
                },
            ),
        ],
    });
    cx.set_global(SourcesGlobal(entity.clone()));
    entity
}

/// Filo's own that a message can name: the computer when its switch is on, then the browser, which every agent has.
fn built_in(computer: bool) -> Vec<AppOn> {
    [computer.then_some(&COMPUTER), Some(&BROWSER)]
        .into_iter()
        .flatten()
        .map(|named: &Named| AppOn { id: named.id.into(), name: named.name.into(), handle: named.handle.into() })
        .collect()
}

/// The apps that are on: Filo's own (`built_in`), then the user's apps that are connected, on and open to `agent` (the
/// conversation's; unknown: only those open to every agent), by their slug.
pub fn apps_on(sources: &MentionSources, agent: Option<&str>, cx: &App) -> Vec<AppOn> {
    let mut apps = built_in(sources.computer);
    if let Some(store) = connectors_store::store_if_loaded(cx) {
        apps.extend(
            store
                .read(cx)
                .apps()
                .iter()
                .filter(|app| {
                    app.enabled && app.status == connectors_store::READY && app.allows_agent(agent.unwrap_or(""))
                })
                .map(|app| AppOn { id: app.id.clone(), name: app.name.clone(), handle: app.slug.clone() }),
        );
    }
    apps
}

/// Where `text` names what "@" or "/" offers, to draw it in the accent colour. `words` are as picking them types them
/// ("@Filo", "@gmail", "/tone"). One counts at the start or after a space, and not when it runs on into a longer word
/// ("@Filomena"); one without a ":" also takes a ":" and what follows it ("@skills:tone", "@projects:site"). The
/// longest word at a place wins ("@Code Reviewer" over "@Code").
pub fn mention_ranges(text: &str, words: &[String]) -> Vec<std::ops::Range<usize>> {
    let runs_on =
        |rest: &str| rest.chars().next().is_some_and(|next| next.is_alphanumeric() || matches!(next, '_' | '-' | '/'));
    let mut ranges = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find(['@', '/']) {
        let start = at + found;
        let after_space = text[..start].chars().next_back().is_none_or(char::is_whitespace);
        let end = after_space
            .then(|| {
                words
                    .iter()
                    .filter(|word| !word.is_empty() && text[start..].starts_with(word.as_str()))
                    .filter_map(|word| {
                        let mut end = start + word.len();
                        if !word.contains(':') && text[end..].starts_with(':') {
                            let tail = &text[end..];
                            let tail = &tail[..tail.find(char::is_whitespace).unwrap_or(tail.len())];
                            end += tail.trim_end_matches(['.', ',', '!', '?', ';', ':', ')', '"', '\'']).len();
                        }
                        (!runs_on(&text[end..])).then_some(end)
                    })
                    .max()
            })
            .flatten();
        match end {
            Some(end) => {
                ranges.push(start..end);
                at = end;
            }
            None => at = start + 1,
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &'static str, handle: &'static str) -> AppOn {
        AppOn { id: id.into(), name: id.into(), handle: handle.into() }
    }

    fn inserts(rows: Vec<Suggestion>) -> Vec<String> {
        rows.iter().map(Suggestion::insert).collect()
    }

    #[test]
    fn at_offers_apps_and_skills() {
        let apps = [app("gmail", "gmail"), app("google-calendar", "calendar")];
        let skills = [SkillRow { name: "weekly-report".into(), description: String::new() }];
        let agents = [("Atlas".to_string(), gpui::ImageSource::from("a.png"), None)];
        assert_eq!(
            inserts(suggestions('@', "", &agents, &apps, &skills, &[], &[], 10)),
            ["@gmail ", "@calendar ", "@skills:", "@projects:", "@Atlas "]
        );
        assert_eq!(inserts(suggestions('@', "gm", &[], &apps, &skills, &[], &[], 10)), ["@gmail "]);
        assert!(suggestions('@', "gmail:w", &[], &apps, &skills, &[], &[], 10).is_empty());
        assert_eq!(
            inserts(suggestions('@', "skills:we", &[], &apps, &skills, &[], &[], 10)),
            ["@skills:weekly-report "]
        );
    }

    #[test]
    fn at_offers_the_browser_always_and_the_computer_while_on() {
        let handles = |computer| built_in(computer).into_iter().map(|app| app.handle).collect::<Vec<_>>();
        assert_eq!(handles(true), ["computer", "browser"]);
        assert_eq!(handles(false), ["browser"]);
        let mut apps = built_in(true);
        apps.push(app("gmail", "gmail"));
        assert_eq!(
            inserts(suggestions('@', "", &[], &apps, &[], &[], &[], 10)),
            ["@computer ", "@browser ", "@gmail ", "@projects:"]
        );
        assert_eq!(inserts(suggestions('@', "br", &[], &apps, &[], &[], &[], 10)), ["@browser "]);
        assert_eq!(inserts(suggestions('@', "filo", &[], &apps, &[], &[], &[], 10)), ["@browser "]);
    }

    #[test]
    fn an_agent_keeps_its_colour() {
        let blue = gpui::rgb(0x3F7DF0).into();
        let agents = [
            ("Atlas".to_string(), gpui::ImageSource::from("a.png"), Some(blue)),
            ("Bea".to_string(), gpui::ImageSource::from("b.png"), None),
        ];
        let backgrounds: Vec<_> = suggestions('@', "", &agents, &[], &[], &[], &[], 10)
            .into_iter()
            .filter_map(|row| match row {
                Suggestion::Agent { background, .. } => Some(background),
                _ => None,
            })
            .collect();
        assert_eq!(backgrounds, [Some(blue), None]);
    }

    #[test]
    fn at_offers_the_running_tasks_first_and_picking_one_types_nothing() {
        let task = SharedWork {
            id: "c1".into(),
            title: "Fix the login page".into(),
            by: "Codex".into(),
            status: "Working".into(),
            tool: "codex".into(),
        };
        let apps = [app("gmail", "gmail")];
        let rows = suggestions('@', "", &[], &apps, &[], std::slice::from_ref(&task), &[], 10);
        assert!(matches!(&rows[0], Suggestion::Work(work) if work.id == "c1"));
        assert_eq!(rows[0].insert(), "");
        assert_eq!(inserts(suggestions('@', "gm", &[], &apps, &[], std::slice::from_ref(&task), &[], 10)), ["@gmail "]);
        assert!(matches!(
            suggestions('@', "login", &[], &apps, &[], std::slice::from_ref(&task), &[], 10).as_slice(),
            [Suggestion::Work(_)]
        ));
        assert!(suggestions('/', "fix", &[], &apps, &[], std::slice::from_ref(&task), &[], 10).is_empty());
    }

    #[test]
    fn projects_filter_as_typed_and_a_shared_name_types_the_path() {
        let project = |name: &str, path: &str| ProjectRow { name: name.into(), path: path.into() };
        let projects = [
            project("harbor", "/Users/me/Desktop/Work/harbor"),
            project("notes-app", "/Users/me/Desktop/side_projects/notes-app"),
            project("web", "/Users/me/a/web"),
            project("web", "/Users/me/b/web"),
            project("orchard-web", "/Users/me/Desktop/side_projects/orchard/orchard-web"),
        ];
        assert_eq!(inserts(suggestions('@', "pro", &[], &[], &[], &[], &projects, 10)), ["@projects:"]);
        assert_eq!(
            inserts(suggestions('@', "projects:", &[], &[], &[], &[], &projects, 3)),
            ["@projects:harbor ", "@projects:notes-app ", "@projects:~/a/web "]
        );
        assert_eq!(
            inserts(suggestions('@', "projects:NOTES", &[], &[], &[], &[], &projects, 10)),
            ["@projects:notes-app "]
        );
        // Names that start with it first, then names and paths that hold it.
        assert_eq!(
            inserts(suggestions('@', "projects:web", &[], &[], &[], &[], &projects, 10)),
            ["@projects:~/a/web ", "@projects:~/b/web ", "@projects:orchard-web "]
        );
        assert_eq!(
            inserts(suggestions('@', "project:side_proj", &[], &[], &[], &[], &projects, 10)),
            ["@projects:notes-app ", "@projects:orchard-web "]
        );
        assert!(wants_projects("projects:a") && wants_projects("pro") && wants_projects("Project:"));
        assert!(!wants_projects("") && !wants_projects("gmail") && !wants_projects("skills:p"));
    }

    #[test]
    fn slash_offers_skills_only() {
        let skills = [
            SkillRow { name: "weekly-report".into(), description: String::new() },
            SkillRow { name: "tone".into(), description: String::new() },
        ];
        assert_eq!(inserts(suggestions('/', "w", &[], &[], &skills, &[], &[], 10)), ["/weekly-report "]);
        assert!(suggestions('/', "src/app", &[], &[], &skills, &[], &[], 10).is_empty());
    }

    #[test]
    fn mentions_are_found_whole_after_a_space() {
        let words: Vec<String> = ["@Filo", "@Code", "@Code Reviewer", "@gmail", "@skills", "/tone"]
            .iter()
            .map(|word| word.to_string())
            .collect();
        let found = |text: &str| {
            mention_ranges(text, &words).into_iter().map(|range| text[range].to_string()).collect::<Vec<_>>()
        };
        assert_eq!(found("@Filo hi"), ["@Filo"]);
        assert_eq!(found("@Filo: hi"), ["@Filo"]);
        assert_eq!(found("ask @Filo, then @Code Reviewer."), ["@Filo", "@Code Reviewer"]);
        assert_eq!(found("@Filomena me@Filo @Filo-x"), Vec::<String>::new());
        assert_eq!(
            found("mail @gmail:me@x.com, and @skills:weekly-report"),
            ["@gmail:me@x.com", "@skills:weekly-report"]
        );
        assert_eq!(found("/tone now and/tone /tones /tmp"), ["/tone"]);
        assert_eq!(found("“@Filo” é @Filo"), ["@Filo"]);
        assert!(mention_ranges("@Filo", &[]).is_empty());
    }
}

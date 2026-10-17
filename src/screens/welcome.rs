// The welcome (desktop only): the first time Filo for Mac opens, it sets up what a first chat needs, one screen at a
// time with nothing beside it:
//
//   Welcome         checks every coding tool on this Mac as it shows (`local_backend::tool_status`), so most people go
//                   straight on to their first agent. Each one installed is a row with where it stands, Claude Code
//                   and Codex first; the others fold into one quiet line ("Also works with Muse Code and OpenCode")
//                   that opens to the same rows, with Install (or Sign in) on each;
//   Set up a tool   only when none is signed in: any of them, one at a time, Install and Sign in as two numbered steps
//                   run in Terminal. Filo looks again every few seconds, and Continue wakes up once one is ready;
//   Make it yours   "How it runs": two of the Coding page's settings as switches, titles only, drawn by that page with
//                   its state and calls (coding.rs `keep_on_quit_switch`, `own_branch_switch`). And "Pages", Feed and
//                   Notes as switches, Settings' own section (pages.rs `section`): a new install starts with both off;
//   First agent     three starters (or from scratch) and the Edit agent page's own pieces (agents.rs: the picture and
//                   its colour popover, the name, the model on any tool signed in, the instructions). It becomes the
//                   main agent, the one that
//                   leads the group, answers Ask on Feed posts and owns what's no other agent's, and the tool it runs
//                   on answers by default (local_backend.rs). A main agent the user made before opens there to edit instead.
//
// It ends in that agent's chat, with three things to try that match its starter (agents.rs `open_first_chat`). While
// the agents are only the Filo agent fresh data starts with, never talked to, it goes as the setup ends
// (`agents:removeStandIn`), so the user's own agent is the only one, and skipping lands on the Agents tab's empty
// state. A dot a step in the footer says how far along it is. There's no account:
// everything it sets is kept on this Mac.
//
// Keyboard: → or Enter moves on, ← goes back, Esc skips, 1–3 pick a starter (while no field has the keyboard; Enter in
// the name creates the agent).
//
// Shown once: desktop-welcome.json in the config folder remembers it was seen. Settings › Welcome tour shows it again,
// and so does FILO_OPEN=welcome at launch; FILO_OPEN=welcome-demo plays it by itself (for recordings) without
// changing anything or remembering it (both in `dev` builds).

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use filo_local::Engine;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, ClipboardItem, Context, Div, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::WindowExt as _;
use gpui_component::input::{Input, InputEvent, InputState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::AppShell;
use crate::backend;
use crate::local_backend::ToolStatus;
use crate::nav::{NavGlobal, Tab};
use crate::screens::agents::{self, AgentRow, AgentsScreen, PictureDone, ProfilePicture};
use crate::screens::chat::conversation::{self, ModelChoice};
use crate::screens::coding::CodingScreen;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Welcome,
    /// Set up a tool: only when no coding tool is signed in.
    Tool,
    /// Make it yours: how coding runs, and the optional pages.
    Yours,
    Agent,
}

/// A step comes in with a short fade and a small rise.
const STEP_IN: Duration = Duration::from_millis(260);
const COLUMN_WIDTH: f32 = 560.;
/// How often the tools are looked at again while one isn't ready (`claude auth status` and `codex login status` take a
/// quarter of a second each, Muse Code's and OpenCode's sign-in is read from their files).
const RECHECK: Duration = Duration::from_millis(2500);

// --- The coding tools ---

/// The tools the first run checks and sets up: every one the Coding page supports, Claude Code and Codex first.
const TOOLS: [Engine; 4] = Engine::ALL;

/// What it takes to set a tool up: its maker and the official install command (`Engine::sign_in_command` signs in).
struct Setup {
    maker: &'static str,
    install: &'static str,
}

fn setup(engine: Engine) -> Setup {
    match engine {
        // https://code.claude.com/docs/en/setup (the native installer)
        Engine::Claude => Setup { maker: "Anthropic", install: "curl -fsSL https://claude.ai/install.sh | bash" },
        // https://github.com/openai/codex (the standalone script)
        Engine::Codex => Setup { maker: "OpenAI", install: "curl -fsSL https://chatgpt.com/codex/install.sh | sh" },
        // https://dev.meta.ai (Muse Code's install script)
        Engine::Muse => Setup { maker: "Meta", install: "curl -fsSL https://dev.meta.ai/install.sh | bash" },
        // https://opencode.ai/docs (the install script)
        Engine::OpenCode => Setup { maker: "OpenCode", install: "curl -fsSL https://opencode.ai/install | bash" },
    }
}

/// "A", "A and B", "A, B and C".
fn list_words(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The line under Welcome's tools: what's next, by how many are signed in (`signed`) and installed but not signed in
/// (`waiting`).
fn tools_note(signed: &[&str], waiting: &[&str]) -> String {
    match (signed, waiting) {
        ([], []) => "Filo needs one coding tool signed in. The next step takes about a minute.".into(),
        ([], [one]) => {
            format!("{one} is installed but not signed in. Sign in here, or the next step walks you through it.")
        }
        ([], more) => format!(
            "{} are installed but not signed in. Sign in to one here, or the next step walks you through it.",
            list_words(more)
        ),
        ([one], _) => {
            format!("{one} is signed in: that’s all Filo needs. Others can be added later on the Coding page.")
        }
        (more, _) => format!("{} are signed in. Your first agent picks which one it runs on.", list_words(more)),
    }
}

/// The official install command (a problem report's Install fix, report.rs).
pub(crate) fn install_command(engine: Engine) -> &'static str {
    setup(engine).install
}

/// Opens a Terminal window running `command` (a `.command` file, which Terminal runs without asking for automation
/// rights), then a line saying Filo notices on its own.
pub(crate) fn run_in_terminal(name: &str, title: &str, command: &str) -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("filo-{name}.command"));
    std::fs::write(&path, terminal_script(title, command)).map_err(|error| error.to_string())?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    }
    let status = std::process::Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(&path)
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("open exited with {status}")) }
}

/// The script: a login shell (so the user's PATH and npm are there), the title, the command, and what to do next.
fn terminal_script(title: &str, command: &str) -> String {
    format!(
        "#!/bin/zsh -l\nclear\necho {title}\necho\n{command}\necho\necho 'Done. Filo sees it on its own: you can close this window.'\n",
        title = shell_quote(title),
    )
}

pub(crate) fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

// --- Your first agent ---

/// A starting point for the first agent: a card that fills the form, and what its chat offers to try first.
struct Starter {
    name: &'static str,
    picture: &'static str,
    /// The line under its name on its card.
    tag: &'static str,
    instructions: &'static str,
    tries: [&'static str; 3],
}

const STARTERS: [Starter; 3] = [
    Starter {
        name: "Builder",
        picture: "panda-waving",
        tag: "Builds and fixes in your projects",
        instructions: "Builds and fixes things in my projects with me. Explains each change in plain words and commits after every round.",
        tries: [
            "Look at a project folder on my Desktop and tell me what it does",
            "Start a small website in a new folder and open it",
            "Find what’s taking space in my Downloads",
        ],
    },
    Starter {
        name: "Researcher",
        picture: "cat-waving",
        tag: "Finds and compares, with links",
        instructions: "Finds and compares sources on the web, then gives me a short summary with links. Says when something is uncertain.",
        tries: [
            "Compare three note-taking apps for Mac, with links",
            "What changed in macOS this year?",
            "Find the best way to back up a Mac to a NAS",
        ],
    },
    Starter {
        name: "Writer",
        picture: "rabbit-waving",
        tag: "Drafts in your voice",
        instructions: "Drafts emails, posts and docs in my voice: short, clear, no filler. Shows the draft first, then asks what to change.",
        tries: [
            "Draft a short email declining a meeting",
            "Rewrite my bio in forty words",
            "Turn my notes into a post",
        ],
    },
];

/// What the chat of an agent made from scratch offers to try first.
const SCRATCH_TRIES: [&str; 3] = [
    "What can you do on this Mac?",
    "Look at a folder on my Desktop and tell me what’s in it",
    "Draft a short email declining a meeting",
];

/// What an agent made with its instructions left empty follows (an agent needs some); its page changes them.
const PLAIN_INSTRUCTIONS: &str =
    "Help me with whatever I bring. Be clear and concise, and ask when something important is unclear.";

/// The first agent (or the main agent made before, edited), as the Edit agent page edits one.
struct AgentDraft {
    name: Entity<InputState>,
    instructions: Entity<InputState>,
    look: ProfilePicture,
    /// The model picked; it counts while its tool is one offered (`offered_engines`), else that tool's default answers.
    model: Option<ModelChoice>,
    /// The engine a main agent made before is tied to (its conversation has a session): its models only.
    locked: Option<Engine>,
    /// The main agent the user made before and what it was saved with: the step saves changes to it.
    existing: Option<(String, Made)>,
    saving: bool,
    /// The name field takes the keyboard when the step shows next.
    focus_name: bool,
}

/// An agent as the step sends it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Made {
    name: String,
    instructions: String,
    picture: String,
    background: Option<String>,
    model: ModelChoice,
}

/// The agent the setup ends with, whose chat opens.
struct Opened {
    id: String,
    name: String,
    engine: Engine,
    tries: Vec<&'static str>,
    /// Made here (not the main agent made before): the toast says it's the main agent now.
    made: bool,
}

/// The Agents tab's screen: it holds the picture sheet and knows the user's own avatars.
fn agents_screen(cx: &App) -> Option<Entity<AgentsScreen>> {
    cx.try_global::<NavGlobal>()?.0.upgrade()?.read(cx).agents_screen()
}

// --- Seen once ---

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    seen: bool,
}

const SAVED: &str = "desktop-welcome.json";

fn saved_path() -> PathBuf {
    crate::config::config_dir().join(SAVED)
}

fn seen() -> bool {
    std::fs::read_to_string(saved_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Saved>(&text).ok())
        .is_some_and(|saved| saved.seen)
}

/// Remembers the setup was seen, the file's other settings kept; on failure (a full disk, a read-only config folder, a
/// file that isn't a JSON object, which is never saved over) says why, as it'll show again.
fn mark_seen() -> Result<(), String> {
    crate::config::set_settings(
        SAVED,
        serde_json::Map::from_iter([("seen".to_string(), serde_json::Value::Bool(true))]),
    )
}

fn demo_requested() -> bool {
    cfg!(feature = "dev") && std::env::var("FILO_OPEN").is_ok_and(|open| open == "welcome-demo")
}

/// Whether the setup opens with the window: the first launch, or FILO_OPEN=welcome / welcome-demo / welcome-drive.
pub fn at_launch() -> bool {
    cfg!(feature = "dev")
        && std::env::var("FILO_OPEN")
            .is_ok_and(|open| matches!(open.as_str(), "welcome" | "welcome-demo" | "welcome-drive"))
        || !seen()
}

/// Shows the setup again (Settings › Welcome tour).
pub fn show(window: &mut Window, cx: &mut App) {
    with_shell(window, cx, |shell, window, cx| shell.show_welcome(window, cx));
}

fn with_shell(
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut AppShell, &mut Window, &mut Context<AppShell>) + 'static,
) {
    let Some(shell) = cx.try_global::<NavGlobal>().map(|nav| nav.0.clone()) else { return };
    window.defer(cx, move |window, cx| {
        shell.update(cx, |shell, cx| f(shell, window, cx)).ok();
    });
}

/// Closes the setup on the Agents tab (`agents`) or wherever the user was, and opens the agent it ended with.
fn close(opened: Option<Opened>, agents: bool, window: &mut Window, cx: &mut App) {
    with_shell(window, cx, move |shell, window, cx| {
        shell.close_welcome(cx);
        if agents || opened.is_some() {
            shell.open_tab(Tab::Agents, window, cx);
        }
        let Some(opened) = opened else { return };
        if let Some(screen) = shell.agents_screen() {
            screen.update(cx, |screen, cx| screen.open_first_chat(opened.id, opened.tries, window, cx));
        }
        if opened.made {
            ui::toast(window, cx, format!("{} is your main agent", opened.name));
        }
    });
}

pub struct WelcomeTour {
    step: Step,
    /// Bumped on every step change so the entrance plays again.
    turn: usize,
    focus: FocusHandle,
    /// Where each tool stands, once looked at (the login shell's PATH).
    tools: Option<HashMap<Engine, ToolStatus>>,
    /// The tool Set up a tool shows the steps of; none until that step showed.
    picked: Option<Engine>,
    /// The install command just copied, for a moment.
    copied: Option<Engine>,
    /// Welcome's tools that aren't on this Mac, folded into one line, shown.
    also_open: bool,
    /// The Coding tab's screen, whose switches Make it yours shows (watched, so a change there shows here), once the
    /// shell has handed it over; and whether it's been asked for.
    coding: Option<(Entity<CodingScreen>, Subscription)>,
    coding_asked: bool,
    agent: AgentDraft,
    /// Opened before the setup was ever seen: it ends on the Agents tab, skipped or not.
    first_run: bool,
    /// The agents are only the Filo agent fresh data starts with, never talked to: it goes as the setup ends.
    fresh: bool,
    /// Ending (the stand-in going, then the setup closing): nothing more is taken.
    ending: bool,
    /// FILO_OPEN=welcome-demo: it plays itself and saves nothing.
    #[cfg(feature = "dev")]
    demo: bool,
    _agent_subs: [Subscription; 3],
    _check: Task<()>,
    _copied: Option<Task<()>>,
    _advance: Option<Task<()>>,
}

impl WelcomeTour {
    /// The self-playing demo (`demo_requested`); never in the packaged app.
    fn demo(&self) -> bool {
        #[cfg(feature = "dev")]
        return self.demo;
        #[cfg(not(feature = "dev"))]
        false
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let demo = demo_requested();
        let name = cx.new(|cx| agents::name_input(window, cx));
        let instructions = cx.new(|cx| agents::instructions_input(window, cx));
        let animal = &crate::avatars::ANIMALS[(crate::format::now_ms() as usize / 7) % crate::avatars::ANIMALS.len()];
        let (look, hue_sub) = ProfilePicture::new(
            animal.avatar_id("waving"),
            None,
            |this: &mut Self| Some(&mut this.agent.look),
            window,
            cx,
        );
        let _agent_subs = [
            cx.subscribe_in(&name, window, |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    ui::cap_length(input, agents::NAME_MAX, window, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.primary(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&instructions, window, |_, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    ui::cap_length(input, agents::INSTRUCTIONS_MAX, window, cx);
                    cx.notify();
                }
            }),
            hue_sub,
        ];
        let agent = AgentDraft {
            name,
            instructions,
            look,
            model: None,
            locked: None,
            existing: None,
            saving: false,
            focus_name: false,
        };
        backend::call_in(
            window,
            cx,
            "agents:list",
            json!({}),
            |this: &mut Self, result: Result<Vec<AgentRow>, String>, window, cx| {
                let Ok(rows) = result else { return };
                this.fresh = matches!(rows.as_slice(), [only] if only.id == crate::local_backend::DEFAULT_AGENT_ID && only.session_id.is_none());
                // A main agent the user made (not Filo's own, which leads until they do): the step edits it.
                let main = rows.into_iter().find(|row| row.main && row.id != crate::local_backend::DEFAULT_AGENT_ID);
                if let Some(row) = main.filter(|_| this.agent.existing.is_none()) {
                    this.edit_main(row, window, cx);
                }
            },
        );
        let mut tour = WelcomeTour {
            step: Step::Welcome,
            turn: 0,
            focus,
            tools: None,
            picked: None,
            copied: None,
            also_open: false,
            coding: None,
            coding_asked: false,
            agent,
            first_run: !seen() && !demo,
            fresh: false,
            ending: false,
            #[cfg(feature = "dev")]
            demo,
            _agent_subs,
            _check: Self::keep_checking(cx),
            _copied: None,
            _advance: None,
        };
        // A new install: Feed and Notes start off (pages.rs), for the switches on Make it yours to turn on. A test run
        // that opens another page keeps them as they are, for its pages.
        let testing_other =
            crate::test_run::active() && !std::env::var("FILO_OPEN").is_ok_and(|open| open.starts_with("welcome"));
        if tour.first_run && !testing_other {
            crate::pages::start_off(cx);
        }
        if demo {
            tour.play_demo(window, cx);
        } else if let Some(keys) = std::env::var("FILO_WELCOME_KEYS").ok().filter(|_| cfg!(feature = "dev")) {
            // `dev` builds, `FILO_WELCOME_KEYS="right 1 enter"`: those keys pressed for real, a second and a half apart
            // (a scripted check of the setup, what it makes kept).
            let keys: Vec<String> = keys.split_whitespace().map(str::to_string).collect();
            tour._advance = Some(cx.spawn_in(window, async move |this, cx| {
                for key in keys {
                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                    let pressed = this.update_in(cx, |this, window, cx| {
                        let used = this.press(&key, window, cx);
                        eprintln!(
                            "welcome: {key} ({}) → {:?}, tools {:?}, agent {:?} on {:?}",
                            if used { "used" } else { "unused" },
                            this.step,
                            this.tools,
                            this.agent_name(cx),
                            this.agent_model(),
                        );
                    });
                    if pressed.is_err() {
                        return;
                    }
                }
            }));
        }
        tour
    }

    /// Looks at the tools at once, then again every few seconds while one isn't ready and Welcome or Set up a tool
    /// shows, so an install or a sign-in in Terminal shows up on its own. Off the UI thread, each tool at once: the login
    /// shell answers in a second or so.
    fn keep_checking(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let wanted = match this.update(cx, |this, _| {
                    matches!(this.step, Step::Welcome | Step::Tool)
                        && !TOOLS.into_iter().all(|engine| this.ready(engine))
                }) {
                    Ok(wanted) => first || wanted,
                    Err(_) => return,
                };
                if wanted {
                    let found = cx
                        .background_executor()
                        .spawn(async {
                            std::thread::scope(|scope| {
                                let checks: Vec<_> = TOOLS
                                    .into_iter()
                                    .map(|engine| {
                                        (engine, scope.spawn(move || crate::local_backend::tool_status(engine)))
                                    })
                                    .collect();
                                checks
                                    .into_iter()
                                    .map(|(engine, check)| (engine, check.join().unwrap_or(ToolStatus::NotInstalled)))
                                    .collect::<HashMap<_, _>>()
                            })
                        })
                        .await;
                    let updated = this.update(cx, |this, cx| {
                        if this.tools.as_ref() != Some(&found) {
                            this.tools = Some(found);
                            cx.notify();
                        }
                    });
                    if updated.is_err() {
                        return;
                    }
                }
                first = false;
                cx.background_executor().timer(RECHECK).await;
            }
        })
    }

    /// Where it is, for smoke.rs's welcome-drive: the step, the tools as looked at, the tool Set up a tool shows, the
    /// primary button, and the agent's name and model.
    #[cfg(feature = "dev")]
    pub fn drive_state(&self, cx: &App) -> String {
        let tools: Vec<String> =
            TOOLS.into_iter().map(|engine| format!("{}={:?}", engine.program(), self.status(engine))).collect();
        format!(
            "{:?} [{}]{} picked {:?}, button {:?} ({}), agent {:?} {:?}{}",
            self.step,
            tools.join(" "),
            if self.also_open { " also open," } else { "" },
            self.picked,
            self.primary_label(cx),
            if self.can_go_on(cx) { "on" } else { "off" },
            self.agent_name(cx),
            self.agent_model(),
            if self.agent.existing.is_some() { " (editing the main agent)" } else { "" },
        )
    }

    fn status(&self, engine: Engine) -> Option<ToolStatus> {
        self.tools.as_ref().and_then(|tools| tools.get(&engine).copied())
    }

    fn ready(&self, engine: Engine) -> bool {
        self.status(engine) == Some(ToolStatus::Ready)
    }

    /// The tools signed in, the one answering by default first.
    fn ready_tools(&self) -> Vec<Engine> {
        let preferred = crate::local_backend::default_engine();
        let mut ready: Vec<Engine> = TOOLS.into_iter().filter(|engine| self.ready(*engine)).collect();
        ready.sort_by_key(|engine| *engine != preferred);
        ready
    }

    /// Looked, and no coding tool is signed in: Set up a tool comes next.
    fn needs_setup(&self) -> bool {
        self.tools.is_some() && self.ready_tools().is_empty()
    }

    /// Set up a tool is part of this run: needed, or it showed.
    fn with_setup(&self) -> bool {
        self.needs_setup() || self.picked.is_some()
    }

    /// The steps this run goes through, for the dots.
    fn steps(&self) -> Vec<Step> {
        if self.with_setup() {
            vec![Step::Welcome, Step::Tool, Step::Yours, Step::Agent]
        } else {
            vec![Step::Welcome, Step::Yours, Step::Agent]
        }
    }

    fn go_to(&mut self, step: Step, cx: &mut Context<Self>) {
        if step == self.step {
            return;
        }
        match step {
            // The one installed, if one is, else Claude Code.
            Step::Tool if self.picked.is_none() => {
                self.picked = Some(
                    TOOLS
                        .into_iter()
                        .find(|engine| self.status(*engine) == Some(ToolStatus::NeedsSignIn))
                        .unwrap_or(Engine::Claude),
                );
            }
            Step::Agent => {
                conversation::refresh_models(cx);
                self.agent.focus_name = !self.demo();
            }
            _ => {}
        }
        self.step = step;
        self.turn += 1;
        cx.notify();
    }

    /// Whether the primary button and Enter can move on: Welcome once it has looked, Set up a tool with a tool
    /// signed in, the first agent with a name.
    fn can_go_on(&self, cx: &App) -> bool {
        match self.step {
            Step::Welcome => self.tools.is_some(),
            Step::Tool => !self.ready_tools().is_empty(),
            Step::Yours => true,
            Step::Agent => !self.agent.saving && !self.agent_name(cx).is_empty(),
        }
    }

    /// The step's main action: the footer's primary button, → and Enter.
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ending || !self.can_go_on(cx) {
            return;
        }
        match self.step {
            Step::Welcome => self.go_to(if self.needs_setup() { Step::Tool } else { Step::Yours }, cx),
            Step::Tool => self.go_to(Step::Yours, cx),
            Step::Yours => self.go_to(Step::Agent, cx),
            Step::Agent => self.save_agent(window, cx),
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        match self.step {
            Step::Welcome => {}
            Step::Tool => self.go_to(Step::Welcome, cx),
            Step::Yours => self.go_to(if self.with_setup() { Step::Tool } else { Step::Welcome }, cx),
            Step::Agent => self.go_to(Step::Yours, cx),
        }
    }

    /// Ends the setup: remembered as seen, the agent's tool answering by default, the stand-in gone on a fresh run, and
    /// then the agent's chat, or (skipped) the Agents tab on the first run and wherever the user was otherwise.
    fn finish(&mut self, opened: Option<Opened>, window: &mut Window, cx: &mut Context<Self>) {
        if self.ending {
            return;
        }
        self.ending = true;
        let agents = self.first_run;
        if self.demo() {
            close(None, false, window, cx);
            return;
        }
        if let Err(error) = mark_seen() {
            ui::toast_error(
                window,
                cx,
                "Couldn’t remember that you took the setup",
                format!("It may show again next time Filo opens ({error})."),
            );
        }
        if let Some(opened) = &opened
            && opened.engine != crate::local_backend::default_engine()
        {
            crate::local_backend::set_default_engine(opened.engine);
        }
        if !self.fresh {
            close(opened, agents, window, cx);
            return;
        }
        backend::call_in(
            window,
            cx,
            "agents:removeStandIn",
            json!({}),
            move |_: &mut Self, result: Result<Value, String>, window, cx| {
                if let Err(error) = result {
                    eprintln!("filo welcome: the Filo agent stays ({error})");
                }
                close(opened, agents, window, cx);
            },
        );
    }

    fn skip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(None, window, cx);
    }

    // --- Your first agent ---

    fn agent_name(&self, cx: &App) -> String {
        self.agent.name.read(cx).value().trim().to_string()
    }

    /// The tools the agent can run on: the one its conversation is tied to, else the ones signed in (and a main agent
    /// made before keeps its own).
    fn offered_engines(&self) -> Vec<Engine> {
        if let Some(locked) = self.agent.locked {
            return vec![locked];
        }
        let mut engines = self.ready_tools();
        if let Some((_, made)) = &self.agent.existing
            && !engines.contains(&made.model.0)
        {
            engines.insert(0, made.model.0);
        }
        if engines.is_empty() {
            engines.push(crate::local_backend::default_engine());
        }
        engines
    }

    /// Who answers as the agent: the model picked, if its tool is offered, else the first tool's own default.
    fn agent_model(&self) -> ModelChoice {
        let engines = self.offered_engines();
        match &self.agent.model {
            Some(model) if engines.contains(&model.0) => model.clone(),
            _ => (engines[0], None),
        }
    }

    /// The main agent the user made before, in the step's fields: the step saves changes to it.
    fn edit_main(&mut self, row: AgentRow, window: &mut Window, cx: &mut Context<Self>) {
        let engine = row.locked_engine.or(row.engine).unwrap_or_else(crate::local_backend::default_engine);
        self.agent.name.update(cx, |input, cx| input.set_value(row.name.clone(), window, cx));
        self.agent.instructions.update(cx, |input, cx| input.set_value(row.instructions.clone(), window, cx));
        self.agent.look.reset(row.picture.clone(), row.background.clone(), window, cx);
        self.agent.model = Some((engine, row.model.clone()));
        self.agent.locked = row.locked_engine;
        let made = Made {
            name: row.name.trim().to_string(),
            instructions: row.instructions.trim().to_string(),
            picture: row.picture.clone(),
            background: row.background.clone(),
            model: (engine, row.model.clone()),
        };
        self.agent.existing = Some((row.id, made));
        cx.notify();
    }

    /// The agent as the step shows it now.
    fn agent_now(&self, cx: &App) -> Made {
        Made {
            name: self.agent_name(cx),
            instructions: self.agent.instructions.read(cx).value().trim().to_string(),
            picture: self.agent.look.picture.clone(),
            background: self.agent.look.background.clone(),
            model: self.agent_model(),
        }
    }

    /// The main agent made before, with nothing changed.
    fn agent_unchanged(&self, cx: &App) -> bool {
        self.agent.existing.as_ref().is_some_and(|(_, made)| *made == self.agent_now(cx))
    }

    /// The starter the fields are, while they're still its name and instructions.
    fn starter(&self, cx: &App) -> Option<usize> {
        let now = self.agent_now(cx);
        STARTERS.iter().position(|starter| now.name == starter.name && now.instructions == starter.instructions)
    }

    /// What the agent's chat offers to try: its starter's (by its instructions, whatever its name), else the ones for
    /// any agent.
    fn tries(made: &Made) -> Vec<&'static str> {
        STARTERS
            .iter()
            .find(|starter| starter.instructions == made.instructions)
            .map_or(SCRATCH_TRIES, |starter| starter.tries)
            .to_vec()
    }

    fn use_starter(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(starter) = STARTERS.get(index) else { return };
        self.agent.name.update(cx, |input, cx| input.set_value(starter.name, window, cx));
        self.agent.instructions.update(cx, |input, cx| input.set_value(starter.instructions, window, cx));
        self.agent.look.picture = starter.picture.to_string();
        self.agent.focus_name = !self.demo();
        cx.notify();
    }

    /// From scratch: the name and instructions empty, the picture kept.
    fn from_scratch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent.name.update(cx, |input, cx| input.set_value("", window, cx));
        self.agent.instructions.update(cx, |input, cx| input.set_value("", window, cx));
        self.agent.focus_name = true;
        cx.notify();
    }

    /// Create agent: makes it the main agent (or saves what changed on the one made before), then ends in its chat.
    fn save_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = self.agent_now(cx);
        if self.agent.saving || now.name.is_empty() || self.demo() {
            return;
        }
        let tries = Self::tries(&now);
        let engine = now.model.0;
        let existing = self.agent.existing.as_ref().map(|(id, _)| id.clone());
        if let Some(id) = existing.clone().filter(|_| self.agent_unchanged(cx)) {
            self.finish(Some(Opened { id, name: now.name, engine, tries, made: false }), window, cx);
            return;
        }
        let (engine, model) = agents::saved_model(&now.model, &conversation::model_rows(cx));
        let instructions = if now.instructions.is_empty() { PLAIN_INSTRUCTIONS } else { now.instructions.as_str() };
        let (path, args) = match &existing {
            // `null` resets the colour to the theme's.
            Some(id) => (
                "agents:update",
                json!({ "id": id, "name": now.name, "instructions": instructions, "picture": now.picture, "background": now.background, "engine": engine, "model": model }),
            ),
            None => {
                let mut args = json!({ "name": now.name, "instructions": instructions, "picture": now.picture, "engine": engine, "model": model });
                if let Some(background) = &now.background {
                    args["background"] = json!(background);
                }
                ("agents:create", args)
            }
        };
        self.agent.saving = true;
        cx.notify();
        let name = now.name.clone();
        backend::call_in(window, cx, path, args, move |this: &mut Self, result: Result<Value, String>, window, cx| {
            let id = match (result, existing) {
                (Ok(_), Some(id)) => {
                    this.agent.saving = false;
                    this.finish(Some(Opened { id, name, engine, tries, made: false }), window, cx);
                    return;
                }
                (Ok(value), None) => value.as_str().map(str::to_string).unwrap_or_default(),
                (Err(error), existing) => {
                    ui::toast_error(
                        window,
                        cx,
                        if existing.is_some() { "Couldn’t save the agent" } else { "Couldn’t create the agent" },
                        error,
                    );
                    this.agent.saving = false;
                    cx.notify();
                    return;
                }
            };
            // A new one becomes the main agent, as its menu's Make main agent does.
            let args = json!({ "agentId": id });
            backend::call_in(
                window,
                cx,
                "users:setMainAgent",
                args,
                move |this: &mut Self, result: Result<Value, String>, window, cx| {
                    if let Err(error) = result {
                        ui::toast_error(window, cx, "Couldn’t make it your main agent", error);
                    }
                    this.agent.saving = false;
                    this.finish(Some(Opened { id, name, engine, tries, made: true }), window, cx);
                },
            );
        });
    }

    /// The colour popover's Change picture…: the Edit agent page's picture sheet, its Done picking the step's picture.
    fn change_picture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agents) = agents_screen(cx).filter(|_| !self.demo()) else { return };
        let view = cx.entity().downgrade();
        let done: PictureDone = Rc::new(move |picture, _, cx| {
            view.update(cx, |this, cx| {
                this.agent.look.picture = picture;
                cx.notify();
            })
            .ok();
        });
        let (picture, background) = (self.agent.look.picture.clone(), self.agent.look.background.clone());
        agents.update(cx, |agents, cx| agents.open_picker_for(picture, background, done, window, cx));
    }

    /// Whether a field or menu of the first agent's step has the keyboard: keys are then theirs, not the setup's.
    fn typing(&self, window: &Window) -> bool {
        self.step == Step::Agent && !self.focus.is_focused(window)
    }

    /// One key, from the keyboard or the demo. Returns whether it was used.
    fn press(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match (key, self.step) {
            ("escape", Step::Agent) if self.agent.look.colors_open() => {
                self.agent.look.set_colors_open(false);
                cx.notify();
            }
            ("escape", _) => self.skip(window, cx),
            ("enter" | "right", _) => self.primary(window, cx),
            ("left", _) => self.back(cx),
            ("1" | "2" | "3", Step::Agent) => self.use_starter(key.parse::<usize>().unwrap_or(1) - 1, window, cx),
            _ => return false,
        }
        true
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform
            || modifiers.control
            || modifiers.alt
            || modifiers.function
            || window.has_active_dialog(cx)
        {
            return;
        }
        if self.typing(window) && event.keystroke.key != "escape" {
            return;
        }
        if self.press(&event.keystroke.key, window, cx) {
            cx.stop_propagation();
        }
    }

    /// FILO_OPEN=welcome-demo: walks through the setup with the same keys a person would press, and stays on the first
    /// agent's step (it makes nothing). It holds while the window is in the background, so a recording sees every step.
    fn play_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const SCRIPT: [(u64, &str); 5] = [(4000, "right"), (2600, "right"), (2000, "1"), (1800, "2"), (1800, "3")];
        self._advance = Some(cx.spawn_in(window, async move |this, cx| {
            for (wait, key) in SCRIPT {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                // Waits while the window is behind another app (macOS stops drawing a covered window).
                while this.update_in(cx, |_, window, _| !window.is_window_active()).unwrap_or(false) {
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                }
                if this.update_in(cx, |this, window, cx| this.press(key, window, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    // --- Steps ---

    /// The step's title and the line under it, if it has one.
    fn heading(&self, title: &str, subtitle: Option<&str>, cx: &App) -> Div {
        let colors = *cx.colors();
        div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(div().large_title().child(title.to_string()))
            .when_some(subtitle, |this, subtitle| {
                this.child(div().callout().text_color(colors.text_secondary).child(subtitle.to_string()))
            })
    }

    fn welcome_step(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let colors = *cx.colors();
        let named = |status: ToolStatus| {
            TOOLS
                .into_iter()
                .filter(|engine| self.status(*engine) == Some(status))
                .map(Engine::label)
                .collect::<Vec<_>>()
        };
        let note = match self.tools {
            None => "Looking in your login shell…".to_string(),
            Some(_) => tools_note(&named(ToolStatus::Ready), &named(ToolStatus::NeedsSignIn)),
        };
        // The ones installed, each a row; the others in one line that opens to theirs. Claude Code and Codex while
        // Filo looks.
        let (installed, missing): (Vec<Engine>, Vec<Engine>) = match self.tools {
            Some(_) => TOOLS.into_iter().partition(|engine| self.status(*engine) != Some(ToolStatus::NotInstalled)),
            None => (vec![Engine::Claude, Engine::Codex], vec![]),
        };
        let rows: Vec<AnyElement> = installed.iter().map(|engine| self.tool_row(*engine, cx)).collect();
        let mut tools = div().flex().flex_col().gap(px(space::SM)).child(
            div()
                .px(px(space::SM))
                .footnote()
                .semibold()
                .text_color(colors.text_secondary)
                .child("Coding tools on this Mac"),
        );
        if !rows.is_empty() {
            tools = tools.child(ui::section(None, None, rows, cx));
        }
        if !missing.is_empty() {
            tools = tools.child(self.also_line(&missing, !installed.is_empty(), cx));
            if self.also_open {
                let rows: Vec<AnyElement> = missing.iter().map(|engine| self.tool_row(*engine, cx)).collect();
                tools = tools.child(ui::section(None, None, rows, cx));
            }
        }
        tools = tools.child(div().px(px(space::SM)).footnote().text_color(colors.text_tertiary).child(note));
        vec![
            ui::filo_mark(48., cx).into_any_element(),
            self.heading("Welcome to Filo", Some("Agents on your Mac that run on the coding tool you already use. No account: your agents and chats stay on this Mac."), cx)
                .into_any_element(),
            tools.into_any_element(),
        ]
    }

    /// Make it yours: "How it runs" and "Pages", under a title alone.
    fn yours_step(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut pieces = vec![self.heading("Make it yours", None, cx).into_any_element()];
        pieces.extend(self.how_it_runs(cx));
        pieces.push(crate::pages::section("welcome-pages", cx));
        pieces
    }

    /// "How it runs": "Keep coding after Filo quits" and where tasks work, as the Coding page's own switches in the
    /// welcome's words. Nothing until the shell has handed that page over (asked for as the setup first shows).
    fn how_it_runs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let coding = self.coding.as_ref()?.0.clone();
        let rows = coding.update(cx, |coding, cx| {
            vec![
                coding.keep_on_quit_switch("welcome-keep-on-quit", "Keep working after you quit Filo", cx),
                coding.own_branch_switch("welcome-own-branch", "Give each task its own branch", cx),
            ]
        });
        Some(ui::section(Some("How it runs"), None, rows, cx).into_any_element())
    }

    /// Asks the shell for the Coding tab's screen, once (it's made if it hasn't been), and watches it.
    fn ask_for_coding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.coding_asked || cx.try_global::<NavGlobal>().is_none() {
            return;
        }
        self.coding_asked = true;
        let view = cx.entity().downgrade();
        with_shell(window, cx, move |shell, window, cx| {
            let Some(coding) = shell.coding_screen_now(window, cx) else { return };
            view.update(cx, |this, cx| {
                let watch = cx.observe(&coding, |_, _, cx| cx.notify());
                this.coding = Some((coding, watch));
                cx.notify();
            })
            .ok();
        });
    }

    /// The tools that aren't on this Mac, as one quiet line ("Also works with Muse Code and OpenCode") that opens to
    /// their rows and closes again.
    fn also_line(&self, missing: &[Engine], others: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let names = list_words(&missing.iter().map(|engine| engine.label()).collect::<Vec<_>>());
        let line = div()
            .id("welcome-also")
            .px(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .cursor_pointer()
            .footnote()
            .text_color(colors.text_tertiary)
            .hover(|style| style.text_color(colors.text_secondary))
            .child(if others { format!("Also works with {names}") } else { format!("Works with {names}") })
            .child(ui::icon_sized(
                if self.also_open { "chevron-up" } else { "chevron-down" },
                13.,
                colors.text_tertiary,
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.also_open = !this.also_open;
                cx.notify();
            }));
        div().child(crate::smoke::tracked("welcome-also", line)).into_any_element()
    }

    /// A tool is installed but not signed in (and none is): Welcome's button says Sign in.
    fn signing_in(&self) -> bool {
        TOOLS.into_iter().any(|engine| self.status(engine) == Some(ToolStatus::NeedsSignIn))
    }

    /// A tool on Welcome: its mark, its name, where it stands, and the next thing to do (Install or Sign in…, in
    /// Terminal) until it's signed in.
    fn tool_row(&self, engine: Engine, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let status = self.status(engine);
        let detail = match status {
            None => "Looking on this Mac",
            Some(ToolStatus::Ready) => "Signed in on this Mac",
            Some(ToolStatus::NeedsSignIn) => "Installed, not signed in",
            Some(ToolStatus::NotInstalled) => "Not on this Mac",
        };
        let program = engine.program();
        let action = match status {
            Some(ToolStatus::NotInstalled) => Some(
                Button::secondary(SharedString::from(format!("welcome-row-install-{program}")), "Install")
                    .small()
                    .icon("square-terminal")
                    .on_click(cx.listener(move |this, _, window, cx| this.install(engine, window, cx))),
            ),
            Some(ToolStatus::NeedsSignIn) => Some(
                Button::secondary(SharedString::from(format!("welcome-row-sign-in-{program}")), "Sign in…")
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| this.sign_in(engine, window, cx))),
            ),
            _ => None,
        };
        div()
            .id(SharedString::from(format!("welcome-tool-{program}")))
            .min_h(px(60.))
            .px(px(space::LG - 2.))
            .py(px(space::MD - 2.))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(crate::screens::coding::tool_tile(program, 36., cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().callout().medium().child(engine.label()))
                            .child(status_chip(status, cx)),
                    )
                    .child(
                        div()
                            .footnote()
                            .text_color(colors.text_secondary)
                            .child(format!("{} · {detail}", setup(engine).maker)),
                    ),
            )
            .children(action)
            .into_any_element()
    }

    /// Runs the tool's official install command in Terminal (not in the demo).
    fn install(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        if self.demo() {
            return;
        }
        if let Err(error) = run_in_terminal(
            &format!("install-{}", engine.program()),
            &format!("Installing {}", engine.label()),
            setup(engine).install,
        ) {
            ui::toast_error(
                window,
                cx,
                "Couldn’t open Terminal",
                format!("Copy the command and run it in Terminal ({error})."),
            );
        }
    }

    /// Signs the tool in, in Terminal (not in the demo).
    fn sign_in(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        if self.demo() {
            return;
        }
        let sign_in = engine.sign_in_args();
        // The program where Filo found it: a fresh install's folder may not be on Terminal's PATH yet.
        let program = filo_local::engine::find_program(engine, Some(crate::local_backend::login_path()))
            .map(|path| shell_quote(&path.display().to_string()))
            .unwrap_or_else(|| engine.program().to_string());
        let command = format!("{program} {sign_in}");
        if let Err(error) = run_in_terminal(
            &format!("sign-in-{}", engine.program()),
            &format!("Signing in to {}", engine.label()),
            &command,
        ) {
            ui::toast_error(
                window,
                cx,
                "Couldn’t open Terminal",
                format!("Run `{} {sign_in}` in Terminal ({error}).", engine.program()),
            );
        }
    }

    fn tool_step(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let colors = *cx.colors();
        let engine = self.picked.unwrap_or(Engine::Claude);
        let setup = setup(engine);
        let status = self.status(engine);
        let installed = matches!(status, Some(ToolStatus::NeedsSignIn | ToolStatus::Ready));
        let signed_in = status == Some(ToolStatus::Ready);
        // Two to a row, Claude Code and Codex first.
        let mut options =
            TOOLS.into_iter().map(|other| self.tool_option(other, other == engine, cx)).collect::<Vec<_>>().into_iter();
        let mut grid = div().flex().flex_col().gap(px(space::MD));
        while let Some(first) = options.next() {
            grid = grid.child(div().flex().gap(px(space::MD)).child(first).children(options.next()));
        }
        let install = (!installed).then(|| self.install_body(engine, &setup, cx));
        let sign_in = (installed && !signed_in).then(|| self.sign_in_body(engine, cx));
        let steps = div()
            .px(px(space::XS + 2.))
            .flex()
            .flex_col()
            .gap(px(space::LG - 2.))
            .child(setup_step(1, "Install", installed, !installed, install, cx))
            .child(setup_step(2, "Sign in", signed_in, installed && !signed_in, sign_in, cx));
        let mut pieces = vec![
            self.heading(
                "Set up a coding tool",
                Some("Pick one and follow the two steps. Filo checks every few seconds and notices on its own."),
                cx,
            )
            .into_any_element(),
            crate::smoke::tracked("welcome-options", grid),
            steps.into_any_element(),
        ];
        if signed_in {
            pieces.push(
                div()
                    .px(px(space::XS + 2.))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .callout()
                    .text_color(colors.text_secondary)
                    .child(ui::icon_sized("check", 16., colors.success))
                    .child(format!("{} is signed in on this Mac.", engine.label()))
                    .into_any_element(),
            );
        }
        pieces
    }

    /// A tool to set up, as a card to pick; the one picked has the accent ring.
    fn tool_option(&self, engine: Engine, picked: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        div()
            .id(SharedString::from(format!("welcome-option-{}", engine.program())))
            .relative()
            .flex_1()
            .min_w_0()
            .px(px(space::LG))
            .py(px(space::LG - 2.))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .rounded(px(radius::LG))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .hover(|style| style.bg(colors.surface_strong))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.picked = Some(engine);
                cx.notify();
            }))
            .child(crate::screens::coding::tool_tile(engine.program(), 36., cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().callout().medium().child(engine.label()))
                            .child(status_chip(self.status(engine), cx)),
                    )
                    .child(div().footnote().text_color(colors.text_secondary).child(setup(engine).maker)),
            )
            // Picked: an accent ring over the border, so nothing shifts.
            .when(picked, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(-1.))
                        .left(px(-1.))
                        .right(px(-1.))
                        .bottom(px(-1.))
                        .rounded(px(radius::LG))
                        .border_2()
                        .border_color(colors.accent.opacity(0.7)),
                )
            })
            .into_any_element()
    }

    /// Install, while the tool isn't installed: the command to copy, and a button that runs it in Terminal.
    fn install_body(&self, engine: Engine, setup: &Setup, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let command = setup.install;
        let copied = self.copied == Some(engine);
        let line = div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .pl(px(space::MD))
            .pr(px(space::XS))
            .py(px(space::XS))
            .rounded(px(radius::SM))
            .bg(colors.background)
            .border_1()
            .border_color(colors.border)
            .child(crate::screens::coding::mono(command, cx).flex_1().min_w_0().truncate().text_color(colors.text))
            .child(
                Button::ghost(
                    SharedString::from(format!("welcome-copy-{}", engine.program())),
                    if copied { "Copied" } else { "Copy" },
                )
                .small()
                .icon(if copied { "check" } else { "copy" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(command.to_string()));
                    this.copied = Some(engine);
                    this._copied = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(1500)).await;
                        this.update(cx, |this, cx| {
                            this.copied = None;
                            cx.notify();
                        })
                        .ok();
                    }));
                    cx.notify();
                })),
            );
        let actions = div()
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(
                Button::secondary(
                    SharedString::from(format!("welcome-install-{}", engine.program())),
                    "Run in Terminal",
                )
                .small()
                .icon("square-terminal")
                .on_click(cx.listener(move |this, _, window, cx| this.install(engine, window, cx))),
            )
            .child(div().footnote().text_color(colors.text_tertiary).child("or paste it in Terminal yourself"));
        div().flex().flex_col().gap(px(space::SM)).child(line).child(actions).into_any_element()
    }

    /// Sign in, once installed: in Terminal.
    fn sign_in_body(&self, engine: Engine, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let shown = engine.sign_in_command();
        div()
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(
                Button::primary(SharedString::from(format!("welcome-sign-in-{}", engine.program())), "Sign in…")
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| this.sign_in(engine, window, cx))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::XS))
                    .footnote()
                    .text_color(colors.text_tertiary)
                    .child("Opens Terminal with")
                    .child(crate::screens::coding::mono(shown, cx).text_color(colors.text_secondary)),
            )
            .into_any_element()
    }

    /// Make your first agent: the starters, then the Edit agent page's head (the picture with its ✎ and colour popover,
    /// the name in large type, the model and the tool it runs on) and its Instructions.
    fn agent_step(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let colors = *cx.colors();
        let starter = self.starter(cx);
        let blank = self.agent_name(cx).is_empty() && self.agent.instructions.read(cx).value().trim().is_empty();
        let cards: Vec<AnyElement> = STARTERS
            .iter()
            .enumerate()
            .map(|(index, card)| {
                let picture = agents::agent_picture(agents::picture_source(card.picture), None, 36., cx);
                starter_card(
                    SharedString::from(format!("welcome-starter-{index}")),
                    picture,
                    card.name,
                    card.tag,
                    starter == Some(index),
                    false,
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| this.use_starter(index, window, cx)))
                .into_any_element()
            })
            .collect();
        let scratch = {
            let dash = div()
                .size(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::FULL))
                .border_1()
                .border_dashed()
                .border_color(colors.border_strong)
                .child(ui::icon_sized("plus", 16., colors.text_tertiary));
            starter_card(
                "welcome-starter-scratch".into(),
                dash,
                "From scratch",
                "Your own name and instructions",
                starter.is_none() && !blank,
                true,
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| this.from_scratch(window, cx)))
            .into_any_element()
        };
        let picture = self.agent.look.render(
            agents::picture_source(&self.agent.look.picture),
            |this: &mut Self| Some(&mut this.agent.look),
            Self::change_picture,
            cx,
        );
        let header = div().flex().items_center().gap(px(space::LG + 2.)).child(picture).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(agents::name_field(&self.agent.name, window, cx))
                .child(self.model_line(cx)),
        );
        let instructions = div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(div().footnote().semibold().text_color(colors.text_secondary).child("Instructions"))
            .child(Input::new(&self.agent.instructions))
            .child(div().footnote().text_color(colors.text_secondary).child("Left empty, it helps with anything."));
        let heading = match self.agent.existing {
            Some(_) => self.heading("Your main agent", Some("It answers when you don’t pick another. Change what you like, or keep it as it is; all of it can change later."), cx),
            None => self.heading(
                "Make your first agent",
                Some("It becomes your main agent, the one that answers when you don’t pick another. Pick a starter or start from scratch; all of it can change later."),
                cx,
            ),
        };
        vec![
            heading.into_any_element(),
            crate::smoke::tracked("welcome-starters", div().flex().gap(px(space::SM)).children(cards).child(scratch)),
            header.into_any_element(),
            instructions.into_any_element(),
        ]
    }

    /// Who answers as the agent, as the Edit agent page shows it (the model by its logo, then the tool), offering the
    /// models of every tool signed in. A tool whose models Filo hasn't asked about yet is there as its Default model.
    fn model_line(&self, cx: &mut Context<Self>) -> Div {
        use gpui_component::Sizable as _;
        use gpui_component::button::Button as MenuButton;
        let colors = *cx.colors();
        let model = self.agent_model();
        let engines = self.offered_engines();
        let mut rows: Vec<_> =
            conversation::model_rows(cx).iter().filter(|row| engines.contains(&row.engine)).cloned().collect();
        for engine in &engines {
            if !rows.iter().any(|row| row.engine == *engine) {
                rows.push(filo_local::engine::ModelOption {
                    engine: *engine,
                    id: String::new(),
                    label: "Default model".into(),
                    default: true,
                    aliases: vec![],
                });
            }
        }
        rows.sort_by_key(|row| row.engine as usize);
        let rows = std::sync::Arc::new(rows);
        let button = MenuButton::new("welcome-model")
            .outline()
            .small()
            .rounded(px(radius::FULL))
            .child(ui::icon_sized(conversation::engine_logo(model.0), 13., colors.text_secondary))
            .child(conversation::model_label(&model, &rows).unwrap_or_else(|| "Default model".into()))
            .dropdown_caret(true);
        let entity = cx.entity().downgrade();
        let picker = conversation::model_picker(
            "welcome-model-menu",
            button,
            gpui::Corner::TopLeft,
            rows,
            &model,
            move |choice, cx| {
                entity
                    .update(cx, |this, cx| {
                        // A tool's stand-in Default model row is its own default.
                        this.agent.model = Some((choice.0, choice.1.filter(|id| !id.is_empty())));
                        cx.notify();
                    })
                    .ok();
            },
        );
        div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(picker)
            .child(div().footnote().text_color(colors.text_secondary).child(format!("on {}", model.0.label())))
    }

    /// How far along it is: a dot a step, the current one larger.
    fn dots(&self, cx: &App) -> Div {
        let steps = self.steps();
        let at = steps.iter().position(|step| *step == self.step).unwrap_or(0);
        div().flex().items_center().gap(px(6.)).children(step_dots(steps.len(), at, cx))
    }

    /// The footer's primary button.
    fn primary_label(&self, cx: &App) -> &'static str {
        match self.step {
            Step::Welcome if self.tools.is_none() => "Checking…",
            Step::Welcome if !self.ready_tools().is_empty() => "Continue",
            Step::Welcome if self.signing_in() => "Sign in",
            Step::Welcome => "Set up a tool",
            Step::Tool | Step::Yours => "Continue",
            Step::Agent if self.agent.saving => {
                if self.agent.existing.is_some() {
                    "Saving…"
                } else {
                    "Creating…"
                }
            }
            Step::Agent if self.agent.existing.is_none() => "Create agent",
            Step::Agent if self.agent_unchanged(cx) => "Continue",
            Step::Agent => "Save changes",
        }
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let colors = *cx.colors();
        let label = self.primary_label(cx);
        let skip = match self.step {
            Step::Welcome => "Skip setup",
            Step::Tool => "Skip for now",
            Step::Yours => "Skip setup",
            Step::Agent if self.agent.existing.is_some() => "Skip",
            Step::Agent => "Skip, I’ll make one later",
        };
        div()
            .flex_none()
            .w_full()
            .px(px(space::XXXL))
            .py(px(space::LG))
            .flex()
            .justify_center()
            .border_t_1()
            .border_color(colors.border.opacity(0.5))
            .child(
                div()
                    .w_full()
                    .max_w(px(COLUMN_WIDTH))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap(px(space::MD + 2.))
                            .child(
                                div().ml(px(-space::SM)).child(crate::smoke::tracked(
                                    "welcome-skip",
                                    Button::new("welcome-skip", skip)
                                        .variant(ui::Variant::Quiet)
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| this.skip(window, cx))),
                                )),
                            )
                            .child(self.dots(cx)),
                    )
                    .when(self.step != Step::Welcome, |this| {
                        this.child(crate::smoke::tracked(
                            "welcome-back",
                            Button::ghost("welcome-back", "Back").on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                        ))
                    })
                    .child(crate::smoke::tracked(
                        "welcome-next",
                        Button::primary("welcome-next", label)
                            .disabled(self.ending || !self.can_go_on(cx))
                            .on_click(cx.listener(|this, _, window, cx| this.primary(window, cx))),
                    )),
            )
    }
}

// --- Pieces ---

/// How far along a setup is, a dot a step: the current one larger, the ones done softer. The welcome's, and the group's
/// setup in the Agents tab (screens/agents.rs).
pub fn step_dots(count: usize, at: usize, cx: &App) -> Vec<Div> {
    let colors = *cx.colors();
    (0..count)
        .map(|index| {
            let size = if index == at { 7.5 } else { 6. };
            div().size(px(size)).rounded(px(radius::FULL)).bg(if index == at {
                colors.accent
            } else if index < at {
                colors.accent.opacity(0.45)
            } else {
                colors.border_strong
            })
        })
        .collect()
}

/// Where a tool stands, as a chip.
fn status_chip(status: Option<ToolStatus>, cx: &App) -> Div {
    let (label, tone) = match status {
        None => ("Checking…", ui::Tone::Neutral),
        Some(ToolStatus::Ready) => ("Signed in", ui::Tone::Success),
        Some(ToolStatus::NeedsSignIn) => ("Not signed in", ui::Tone::Warning),
        Some(ToolStatus::NotInstalled) => ("Not installed", ui::Tone::Neutral),
    };
    ui::chip(label, tone, true, cx)
}

/// A starter as a card: its picture, its name and a line; `on` while the form is it, `scratch` dashed.
fn starter_card(
    id: SharedString,
    picture: impl IntoElement,
    name: &str,
    tag: &str,
    on: bool,
    scratch: bool,
    cx: &App,
) -> gpui::Stateful<Div> {
    let colors = *cx.colors();
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .min_h(px(104.))
        .p(px(space::MD))
        .flex()
        .flex_col()
        .items_start()
        .gap(px(space::SM))
        .rounded(px(radius::MD))
        .border_1()
        .map(|this| {
            if on {
                this.border_color(colors.accent).bg(colors.accent_soft)
            } else if scratch {
                this.border_dashed().border_color(colors.border).hover(|style| style.bg(colors.surface))
            } else {
                this.border_color(colors.border).bg(colors.surface).hover(|style| style.bg(colors.surface_strong))
            }
        })
        .cursor_pointer()
        .child(picture)
        .child(div().callout().medium().child(name.to_string()))
        .child(div().footnote().text_color(colors.text_secondary).child(tag.to_string()))
}

/// A numbered step of setting a tool up: done (a check), the current one (its label in full colour, then what to do
/// under it) or one still to come.
fn setup_step(number: usize, label: &str, done: bool, current: bool, body: Option<AnyElement>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let marker =
        div().size(px(18.)).flex_none().flex().items_center().justify_center().rounded(px(radius::FULL)).map(|this| {
            if done {
                this.bg(colors.success).child(ui::icon_sized("check", 11., colors.on_success))
            } else {
                this.border_1()
                    .border_color(if current { colors.text_secondary } else { colors.border_strong })
                    .caption()
                    .text_color(if current { colors.text } else { colors.text_tertiary })
                    .child(number.to_string())
            }
        });
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .child(
            div().flex().items_center().gap(px(space::SM + 2.)).child(marker).child(
                div()
                    .subhead()
                    .when(current, |this| this.medium())
                    .text_color(if current { colors.text } else { colors.text_secondary })
                    .child(label.to_string()),
            ),
        )
        .when_some(body, |this, body| this.child(div().pl(px(28.)).child(body)))
        .into_any_element()
}

impl Render for WelcomeTour {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keys reach the setup even after a click on plain content (not while the picture sheet is open).
        if !self.focus.contains_focused(window, cx) && !window.has_active_dialog(cx) {
            self.focus.focus(window);
        }
        // The first agent's step starts at the name.
        if self.step == Step::Agent && std::mem::take(&mut self.agent.focus_name) {
            self.agent.name.update(cx, |input, cx| input.focus(window, cx));
        }
        let colors = *cx.colors();
        self.ask_for_coding(window, cx);
        let pieces = match self.step {
            Step::Welcome => self.welcome_step(cx),
            Step::Tool => self.tool_step(cx),
            Step::Yours => self.yours_step(cx),
            Step::Agent => self.agent_step(window, cx),
        };
        let content = div()
            .relative()
            .w_full()
            .max_w(px(COLUMN_WIDTH))
            .flex()
            .flex_col()
            .gap(px(space::XXL))
            .children(pieces)
            .with_animation(
                SharedString::from(format!("welcome-in-{}", self.turn)),
                Animation::new(STEP_IN).with_easing(gpui::ease_out_quint()),
                |content, t| content.opacity(t).top(px((1. - t) * 8.)),
            );
        div()
            .id("welcome")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.on_key(event, window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            // Room for the traffic lights; this band drags the window.
            .child(crate::chrome::drag_region(div().id("welcome-drag").h(px(40.)).w_full().flex_none()))
            .child(
                div().id("welcome-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                    div()
                        .w_full()
                        .min_h_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .px(px(space::XXXL))
                        .pt(px(space::SM))
                        .pb(px(space::XXL))
                        .child(content),
                ),
            )
            .child(self.footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_script_runs_its_command() {
        let script = std::env::temp_dir().join(format!("filo-welcome-test-{}.command", std::process::id()));
        std::fs::write(
            &script,
            terminal_script("Signing in to Bob's tool", &format!("{} it", shell_quote("/bin/echo"))),
        )
        .unwrap();
        let output =
            std::process::Command::new("/bin/zsh").arg("-f").arg(&script).env("TERM", "dumb").output().unwrap();
        let _ = std::fs::remove_file(&script);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{text}");
        assert!(
            text.contains("Signing in to Bob's tool") && text.contains("\nit\n") && text.contains("close this window"),
            "{text}"
        );
    }

    #[test]
    fn every_starter_has_a_picture_instructions_and_things_to_try() {
        for starter in STARTERS {
            assert!(crate::avatars::is_avatar_id(starter.picture), "{}: {}", starter.name, starter.picture);
            assert!(starter.name.chars().count() <= agents::NAME_MAX && !starter.instructions.is_empty());
            assert!(starter.tries.iter().all(|text| !text.is_empty()));
        }
    }

    #[test]
    fn the_chat_offers_the_starters_things_to_try_whatever_its_name() {
        let made = |name: &str, instructions: &str| Made {
            name: name.into(),
            instructions: instructions.into(),
            picture: "panda-waving".into(),
            background: None,
            model: (Engine::Claude, None),
        };
        assert_eq!(WelcomeTour::tries(&made("Builder", STARTERS[0].instructions)), STARTERS[0].tries);
        assert_eq!(WelcomeTour::tries(&made("Scribe", STARTERS[2].instructions)), STARTERS[2].tries);
        assert_eq!(WelcomeTour::tries(&made("Builder", "Helps me cook.")), SCRATCH_TRIES);
        assert_eq!(WelcomeTour::tries(&made("Pal", "")), SCRATCH_TRIES);
    }

    #[test]
    fn the_line_under_the_tools_follows_how_many_are_ready() {
        assert!(tools_note(&[], &[]).starts_with("Filo needs one coding tool signed in"));
        assert!(tools_note(&[], &["Muse Code"]).starts_with("Muse Code is installed but not signed in"));
        assert!(
            tools_note(&[], &["Codex", "OpenCode"]).starts_with("Codex and OpenCode are installed but not signed in")
        );
        assert!(tools_note(&["OpenCode"], &["Codex"]).starts_with("OpenCode is signed in: that’s all Filo needs"));
        assert!(
            tools_note(&["Claude Code", "Codex", "Muse Code"], &[])
                .starts_with("Claude Code, Codex and Muse Code are signed in")
        );
    }

    #[test]
    fn every_tool_has_an_official_install_command() {
        for engine in Engine::ALL {
            let setup = setup(engine);
            assert!(setup.install.starts_with("curl -fsSL https://"), "{}", setup.install);
            assert!(engine.sign_in_command().starts_with(engine.program()));
        }
    }
}

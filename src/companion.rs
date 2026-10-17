// Filo outside its window: the menu bar item (menu_bar.rs), the pet that sits on the desktop (pet.rs) and the
// shortcuts that work in any app (hotkeys.rs). Once the window has started, `Companion` follows what Filo is doing
// (replying, working, a reminder that came due, replies nobody has seen) for all three, and what the user picks in any
// of them comes back as a `Command`, carried out in the main window.
//
// Their settings live in their own file (desktop-companion.json): the theme saves desktop.json whole.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{AnyWindowHandle, App, AppContext as _, Context, Entity, EventEmitter, Global, ImageSource, Task, Window};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::AppShell;
use crate::avatars::{self, CustomAvatar, Look};
use crate::backend;
use crate::format;
use crate::nav::{NavGlobal, Route, Tab};
use crate::screens::agents::AgentRow;

/// `coding:workingChats`: a chat with a coding task working, since its task began.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkingChat {
    session_id: String,
    since: f64,
    /// The agents whose tasks those are, wherever the chat is (an agent's task started in the group, from before).
    #[serde(default)]
    agent_ids: Vec<String>,
}

/// Who an agent is, from `agents:list`: the tray's name and picture for it.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    pub picture: String,
    pub background: Option<String>,
}

/// A reply or a finished reminder: the pet shows its heart pose this long.
const CELEBRATE_MS: f64 = 2_500.;
/// A reminder that was already due this long before Filo started doesn't pop up.
const DUE_GRACE_MS: f64 = 60_000.;
/// What "snooze" on the pet's reminder bubble gives.
pub const SNOOZE_MINUTES: f64 = 10.;
/// How many of the next Scheduled items the menu lists.
const UPCOMING: usize = 3;
/// How many unseen chats the menu lists.
const MENU_CHATS: usize = 5;
/// After talking with the shortcut, the pet says the reply that comes within this long.
const TALK_REPLY_MS: f64 = 5. * 60_000.;
/// A coding task that finished stays in the pet's tray this long, as done, until it's opened.
const FINISHED_LINGER_MS: f64 = 90_000.;

/// `isNight` (avatar-mood.ts).
const NIGHT_FROM_HOUR: u32 = 23;
const NIGHT_UNTIL_HOUR: u32 = 6;
/// How often the app asks whether Filo is using the apps (each turn's `use_computer` helper runs in another process).
const USING_APPS_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
/// How often the menu bar asks how many tasks keep the Mac awake.
const KEEPING_AWAKE_EVERY: std::time::Duration = std::time::Duration::from_secs(3);

// ---------------------------------------------------------------------------------------------------------------
// Settings

/// A shortcut that works while another app is in front (hotkeys.rs).
pub struct Shortcut {
    pub id: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
    /// GPUI's keystroke notation ("alt-space").
    pub default: &'static str,
}

pub const OPEN: &str = "open";
pub const PET: &str = "pet";
pub const SCREEN: &str = "screen";
pub const STOP_APPS: &str = "stop-apps";

pub const SHORTCUTS: [Shortcut; 4] = [
    Shortcut {
        id: OPEN,
        label: "Open Filo",
        detail: "Press to open Filo. Hold to talk to Filo, let go to send.",
        default: "ctrl-alt",
    },
    Shortcut {
        id: SCREEN,
        label: "Ask about your screen",
        detail: "Hold to talk about what's on screen: Filo takes a screenshot and sends it with your voice message.",
        default: "alt-cmd",
    },
    Shortcut { id: PET, label: "Show or hide the pet", detail: "Filo on your desktop.", default: "ctrl-alt-p" },
    Shortcut {
        id: STOP_APPS,
        label: "Stop Filo using your apps",
        detail: "Stops Filo clicking and typing in your apps at once; it asks you before it starts again.",
        default: "ctrl-alt-cmd-.",
    },
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub menu_bar: bool,
    pub pet: bool,
    /// Where the pet was left: the bottom-left corner of its picture, in AppKit screen points (y goes up).
    pub pet_origin: Option<(f64, f64)>,
    /// Changed shortcuts, by id; "" turns one off. Missing ones use their default.
    pub shortcuts: BTreeMap<String, String>,
    /// The pet's tray folded into a pill (it opens again on its own when something needs the user).
    pub tray_collapsed: bool,
    /// The pet's character (Settings › Desktop pet): an animal ("fox") or a custom avatar ("custom:<id>"). Unset, it's
    /// the main agent's picture.
    pub pet_character: Option<String>,
    /// Where voice held from another app goes (Settings › Keyboard shortcuts; voice_route.rs).
    pub voice_to: VoiceTo,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            menu_bar: true,
            pet: true,
            pet_origin: None,
            shortcuts: BTreeMap::new(),
            tray_collapsed: false,
            pet_character: None,
            voice_to: VoiceTo::Main,
        }
    }
}

/// Where a voice message held with the open shortcut goes while another app is in front. One that starts with an
/// agent's name or the group's ("Group") goes there instead (voice_route.rs), and in Filo's window it's always the open chat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceTo {
    /// The main agent, whichever it is when the message is sent.
    #[default]
    Main,
    /// The chat open in Filo's window, as before there was a choice (the main agent while none is).
    OpenChat,
    /// An agent, by id (the main agent once it's deleted).
    Agent(String),
}

impl Settings {
    /// The keystroke for a shortcut, or None when it's off.
    pub fn shortcut(&self, id: &str) -> Option<String> {
        match self.shortcuts.get(id) {
            Some(keys) if keys.is_empty() => None,
            Some(keys) => Some(keys.clone()),
            None => SHORTCUTS.iter().find(|shortcut| shortcut.id == id).map(|shortcut| shortcut.default.to_string()),
        }
    }
}

/// `desktop-companion.json`; a newer Filo's file is never saved over (config.rs `load_settings`).
const SETTINGS_FILE: &str = "desktop-companion.json";

pub fn load_settings() -> Settings {
    crate::config::load_settings(SETTINGS_FILE)
}

fn save_settings(settings: &Settings) {
    if let Err(error) = crate::config::save_settings(SETTINGS_FILE, settings) {
        eprintln!("companion: {error}");
    }
}

pub struct SettingsGlobal(pub Settings);

impl Global for SettingsGlobal {}

pub fn settings(cx: &App) -> Settings {
    cx.try_global::<SettingsGlobal>().map(|global| global.0.clone()).unwrap_or_default()
}

/// Changes the settings, saves them and puts them into effect: the menu bar item, the pet, the shortcuts.
pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut settings = settings(cx);
    let before = settings.clone();
    change(&mut settings);
    if settings == before {
        return;
    }
    save_settings(&settings);
    cx.set_global(SettingsGlobal(settings.clone()));
    if settings.menu_bar != before.menu_bar {
        crate::menu_bar::show(settings.menu_bar);
    }
    if settings.pet != before.pet {
        crate::pet::sync(cx);
    }
    if settings.pet_character != before.pet_character
        && let Some(companion) = companion(cx)
    {
        companion.update(cx, |companion, cx| companion.pick_pet(cx));
    }
    if settings.shortcuts != before.shortcuts {
        crate::hotkeys::register(&settings);
    }
    refresh_menu(cx);
}

/// Shows what changed in the menu bar (and on the pet, once there's one).
fn refresh_menu(cx: &mut App) {
    if let Some(companion) = companion(cx) {
        companion.update(cx, |companion, cx| companion.changed(cx));
    } else {
        crate::menu_bar::set(menu_state(None, cx));
    }
}

/// The apps Filo is using now (none named when unsaid), while a turn's computer helper runs.
struct UsingApps(Option<Vec<String>>);

impl Global for UsingApps {}

fn using_apps(cx: &App) -> Option<Vec<String>> {
    cx.try_global::<UsingApps>().and_then(|using| using.0.clone())
}

/// The apps Filo is using, kept short: "Safari", "Safari, Notes", "Safari, Notes +2"; `None` when none is named.
pub fn apps_label(apps: &[String]) -> Option<String> {
    const NAMED: usize = 2;
    let mut label = apps.iter().take(NAMED).map(|app| short(app, 20)).collect::<Vec<_>>().join(", ");
    if apps.len() > NAMED {
        label.push_str(&format!(" +{}", apps.len() - NAMED));
    }
    (!label.is_empty()).then_some(label)
}

fn set_using_apps(using: Option<Vec<String>>, cx: &mut App) {
    if cx.try_global::<UsingApps>().map(|current| &current.0) == Some(&using) {
        return;
    }
    cx.set_global(UsingApps(using));
    refresh_menu(cx);
}

/// `computer:status`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComputerStatus {
    apps_on: bool,
    using: Option<Vec<String>>,
    stopped_until: Option<f64>,
}

impl ComputerStatus {
    /// The apps Filo is using: a helper that's been stopped, or whose calls the switch refuses, no longer counts.
    fn using_apps(self) -> Option<Vec<String>> {
        self.using.filter(|_| self.apps_on && self.stopped_until.is_none())
    }
}

/// Watches whether Filo is using the apps, for the menu bar's "Using your apps" and its Stop.
fn watch_using_apps(cx: &mut App) {
    let backend = backend::backend(cx);
    cx.spawn(async move |cx| {
        loop {
            let status = backend
                .call_raw("computer:status", json!({}))
                .await
                .and_then(|value| serde_json::from_value::<ComputerStatus>(value).map_err(|error| error.to_string()));
            if let Ok(status) = status
                && cx.update(|cx| set_using_apps(status.using_apps(), cx)).is_err()
            {
                break;
            }
            cx.background_executor().timer(USING_APPS_EVERY).await;
        }
    })
    .detach();
}

/// How many coding tasks and routines keep the Mac awake now ("Keep Mac awake while tasks run" on, and some working):
/// the menu says so and offers to let it sleep. `None` when nothing does.
struct KeepingAwake(Option<usize>);

impl Global for KeepingAwake {}

fn keeping_awake(cx: &App) -> Option<usize> {
    cx.try_global::<KeepingAwake>().and_then(|keeping| keeping.0)
}

fn set_keeping_awake(keeping: Option<usize>, cx: &mut App) {
    if cx.try_global::<KeepingAwake>().map(|current| current.0) == Some(keeping) {
        return;
    }
    cx.set_global(KeepingAwake(keeping));
    refresh_menu(cx);
}

/// `coding:keepingAwake`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeepingAwakeStatus {
    keep_awake: bool,
    running: usize,
    /// What keeps it awake ("task:<id>", "turn:<id>"), for telling what a sleep paused (slept.rs).
    #[serde(default)]
    work: Vec<String>,
    /// "Keep running with the lid closed".
    #[serde(default)]
    keep_awake_lid: bool,
}

impl KeepingAwakeStatus {
    /// The tasks keeping the Mac awake: none while the setting is off.
    fn keeping(self) -> Option<usize> {
        (self.keep_awake && self.running > 0).then_some(self.running)
    }
}

/// The menu's line while tasks keep the Mac awake.
pub fn keeping_awake_label(running: usize) -> String {
    match running {
        1 => "Keeping your Mac awake · 1 task running".into(),
        count => format!("Keeping your Mac awake · {count} tasks running"),
    }
}

/// Watches how many coding tasks, routines and replies keep the Mac awake (their workers hold the assertions, not the
/// app: local_backend/awake.rs), for the menu bar's line and its Let Mac Sleep; and says so when a sleep paused them
/// (slept.rs).
fn watch_keeping_awake(cx: &mut App) {
    let backend = backend::backend(cx);
    cx.spawn(async move |cx| {
        let mut slept = crate::slept::Watch::default();
        loop {
            let status = backend.call_raw("coding:keepingAwake", json!({})).await.and_then(|value| {
                serde_json::from_value::<KeepingAwakeStatus>(value).map_err(|error| error.to_string())
            });
            if let Ok(status) = status {
                if let Some(message) = slept.look(&status.work, status.keep_awake, status.keep_awake_lid) {
                    crate::notifications::show_scheduled("filo-slept", "Filo", &message);
                }
                if cx.update(|cx| set_keeping_awake(status.keeping(), cx)).is_err() {
                    break;
                }
            }
            cx.background_executor().timer(KEEPING_AWAKE_EVERY).await;
        }
    })
    .detach();
}

/// At launch: the settings, the menu bar item and the shortcuts (the pet waits for the companion).
pub fn init(cx: &mut App) {
    let settings = load_settings();
    cx.set_global(SettingsGlobal(settings.clone()));
    crate::menu_bar::install(cx);
    crate::menu_bar::show(settings.menu_bar);
    crate::menu_bar::set(menu_state(None, cx));
    watch_using_apps(cx);
    watch_keeping_awake(cx);
    crate::hotkeys::install(cx);
    crate::hotkeys::register(&settings);
}

// ---------------------------------------------------------------------------------------------------------------
// Commands

/// What the user picked in the menu bar, on the pet or with a shortcut.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Open,
    /// The shortcut: hides Filo when it's already in front.
    Toggle,
    OpenChat(String),
    /// An agent's chat, or the agents' group (`agents::GROUP`), in the Agents tab.
    OpenAgent(String),
    /// A coding round's result page: the task, and the round (the latest without one).
    OpenCodingResult(String, Option<u32>),
    /// Answers what a coding tool asks (`coding:answerRequest`): the request, and allow or deny.
    AnswerCodingRequest(String, String),
    Scheduled,
    Shortcuts,
    TogglePet,
    TalkStart,
    /// Talking about the screen: a screenshot goes with the voice message.
    TalkScreenStart,
    TalkEnd,
    /// The shortcut turned out to be part of another one (⌥⌘I): the recording goes.
    TalkCancel,
    /// Stops Filo using the user's apps (computer use).
    StopApps,
    /// Turns "Keep Mac awake while tasks run" off: the tasks carry on, and the Mac sleeps when idle.
    LetMacSleep,
    ReminderDone(String),
    ReminderSnooze(String),
    DismissReminder(String),
    Quit,
}

/// The main window, for commands that come from outside it.
pub struct MainWindow(pub AnyWindowHandle);

impl Global for MainWindow {}

/// Whether Filo is the app in front.
pub fn app_active() -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: a read-only AppKit message on the main thread.
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        msg_send![app, isActive]
    }
}

/// Runs `f` on the shell in the main window, after the current event.
fn with_shell(cx: &mut App, f: impl FnOnce(&mut AppShell, &mut Window, &mut Context<AppShell>) + 'static) {
    let Some(shell) = cx.try_global::<NavGlobal>().map(|nav| nav.0.clone()) else { return };
    let Some(handle) = cx.try_global::<MainWindow>().map(|main| main.0) else { return };
    cx.defer(move |cx| {
        handle.update(cx, |_, window, cx| shell.update(cx, |shell, cx| f(shell, window, cx))).ok();
    });
}

/// Brings Filo's window to the front.
fn show(cx: &mut App) {
    cx.activate(true);
    if let Some(handle) = cx.try_global::<MainWindow>().map(|main| main.0) {
        handle.update(cx, |_, window, _| window.activate_window()).ok();
    }
}

pub fn run(command: Command, cx: &mut App) {
    match command {
        Command::Open => {
            show(cx);
            with_shell(cx, |shell, window, cx| shell.focus_message_box(window, cx));
        }
        Command::Toggle if app_active() => cx.hide(),
        Command::Toggle => run(Command::Open, cx),
        Command::OpenChat(session_id) => {
            show(cx);
            with_shell(cx, move |shell, window, cx| shell.open_chat(session_id, window, cx));
        }
        Command::OpenAgent(agent) => {
            show(cx);
            with_shell(cx, move |shell, window, cx| {
                shell.open_tab(Tab::Agents, window, cx);
                if let Some(agents) = shell.agents_screen() {
                    agents.update(cx, |agents, cx| agents.open_agent(agent, window, cx));
                }
            });
        }
        Command::OpenCodingResult(task_id, round) => {
            show(cx);
            with_shell(cx, move |shell, window, cx| shell.push(Route::CodingResult(task_id, round), window, cx));
        }
        Command::AnswerCodingRequest(request_id, answer) => {
            backend::fire(cx, "coding:answerRequest", json!({ "requestId": request_id, "answer": answer }));
            if let Some(companion) = companion(cx) {
                companion.update(cx, |companion, cx| {
                    // Off the tray now; `coding:trayTasks` agrees a moment later.
                    companion.answered.insert(request_id);
                    companion.count_working();
                    companion.changed(cx);
                });
            }
        }
        Command::Scheduled => {
            show(cx);
            with_shell(cx, |shell, window, cx| shell.push(Route::Scheduled, window, cx));
        }
        Command::Shortcuts => {
            show(cx);
            with_shell(cx, |shell, window, cx| shell.push(Route::Shortcuts, window, cx));
        }
        Command::TogglePet => update_settings(cx, |settings| settings.pet = !settings.pet),
        // Every turn's computer helper closes, and `use_computer` is refused for a while (filo_local `computer`).
        Command::StopApps => {
            backend::fire(cx, "computer:stop", json!({}));
            set_using_apps(None, cx);
        }
        Command::LetMacSleep => {
            backend::fire(cx, "coding:setKeepAwake", json!({ "keep": false }));
            set_keeping_awake(None, cx);
        }
        Command::TalkStart => with_shell(cx, |shell, window, cx| {
            let recording = shell.talk(true, None, window, cx);
            set_listening(recording, false, cx);
        }),
        Command::TalkScreenStart => {
            // Taken now, before the pet's "Looking" bubble or Filo's window can get in the picture.
            let screenshot = crate::screenshot::take(cx);
            let with_screen = screenshot.is_some();
            with_shell(cx, move |shell, window, cx| {
                let recording = shell.talk(true, screenshot, window, cx);
                set_listening(recording, with_screen, cx);
            });
        }
        Command::TalkEnd => with_shell(cx, |shell, window, cx| {
            let sent = shell.talk(false, None, window, cx);
            if let Some(companion) = companion(cx) {
                companion.update(cx, |companion, cx| {
                    companion.stop_listening();
                    if sent {
                        companion.talked_at = Some(format::now_ms());
                    }
                    companion.changed(cx);
                });
            }
        }),
        Command::TalkCancel => with_shell(cx, |shell, _, cx| {
            shell.cancel_talk(cx);
            set_listening(false, false, cx);
        }),
        Command::ReminderDone(id) => {
            backend::fire(cx, "reminders:completeMine", json!({ "id": id }));
            if let Some(companion) = companion(cx) {
                companion.update(cx, |companion, cx| {
                    companion.dismiss_due(&id, cx);
                    companion.celebrate(cx);
                });
            }
        }
        Command::ReminderSnooze(id) => {
            backend::fire(cx, "reminders:snoozeMine", json!({ "id": id, "minutes": SNOOZE_MINUTES }));
            if let Some(companion) = companion(cx) {
                companion.update(cx, |companion, cx| companion.dismiss_due(&id, cx));
            }
        }
        Command::DismissReminder(id) => {
            if let Some(companion) = companion(cx) {
                companion.update(cx, |companion, cx| companion.dismiss_due(&id, cx));
            }
        }
        // Chosen in the menu bar item's or the pet's menu: deliberate, so no second press (quit_guard.rs).
        Command::Quit => crate::quit_guard::quit_deliberately(cx),
    }
}

/// Who the voice message being held from another app goes to (app.rs `talk`), once that's known.
pub fn set_listening_to(to: Option<String>, cx: &mut App) {
    if let Some(companion) = companion(cx) {
        companion.update(cx, |companion, cx| {
            if companion.listening && companion.listening_to != to {
                companion.listening_to = to;
                companion.changed(cx);
            }
        });
    }
}

/// What's been heard so far of the voice message held from another app, who it goes to now (a name it starts with
/// sends it there), and why it can't be sent, if it can't.
pub fn set_listening_heard(to: Option<String>, words: String, problem: Option<String>, cx: &mut App) {
    if let Some(companion) = companion(cx) {
        companion.update(cx, |companion, cx| {
            if companion.listening
                && (companion.listening_to != to
                    || companion.listening_words != words
                    || companion.listening_problem != problem)
            {
                companion.listening_to = to;
                companion.listening_words = words;
                companion.listening_problem = problem;
                companion.changed(cx);
            }
        });
    }
}

fn set_listening(listening: bool, screen: bool, cx: &mut App) {
    if let Some(companion) = companion(cx) {
        companion.update(cx, |companion, cx| {
            if listening {
                companion.listening = true;
                companion.listening_to_screen = screen;
                companion.listening_words.clear();
                companion.listening_problem = None;
            } else {
                companion.stop_listening();
            }
            companion.changed(cx);
        });
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Mood (avatar-mood.ts, plus what only the desktop sees: listening, a reminder that came due, a moment of joy)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Idle,
    Night,
    Working,
    Replying,
    Celebrating,
    Due,
    Listening,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Signals {
    pub listening: bool,
    pub due: bool,
    pub celebrating: bool,
    pub replying: bool,
    pub working: bool,
    pub night: bool,
}

/// Listening to the user comes first, then what needs them, then a moment of joy, a reply, background work; night
/// only when quiet.
pub fn pick_mood(signals: Signals) -> Mood {
    if signals.listening {
        Mood::Listening
    } else if signals.due {
        Mood::Due
    } else if signals.celebrating {
        Mood::Celebrating
    } else if signals.replying {
        Mood::Replying
    } else if signals.working {
        Mood::Working
    } else if signals.night {
        Mood::Night
    } else {
        Mood::Idle
    }
}

pub fn is_night(hour: u32) -> bool {
    !(NIGHT_UNTIL_HOUR..NIGHT_FROM_HOUR).contains(&hour)
}

/// Where the pet looks while resting: at the pointer when it's nearby.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glance {
    Ahead,
    Left,
    Right,
}

/// `moodAvatar`: the same animal in the mood's pose, or the user's pick when resting (glancing at the pointer
/// when the animal has the poses for it).
pub fn mood_avatar(picked: &str, mood: Mood, glance: Glance) -> String {
    let animal = picked.split_once('-').map_or(picked, |(animal, _)| animal);
    let panda = animal == "panda";
    match mood {
        Mood::Replying if panda => "panda-thinking".into(),
        Mood::Working if panda => "panda-running".into(),
        Mood::Replying | Mood::Working => format!("{animal}-laptop"),
        Mood::Due => format!("{animal}-waving"),
        Mood::Celebrating => format!("{animal}-heart"),
        Mood::Night => format!("{animal}-sleeping"),
        Mood::Idle | Mood::Listening => match glance {
            Glance::Left if !panda => format!("{animal}-left"),
            Glance::Right if !panda => format!("{animal}-right"),
            _ => picked.to_string(),
        },
    }
}

/// The picture for a look in a mood.
pub fn mood_image(look: &Look, mood: Mood, glance: Glance) -> ImageSource {
    match look {
        Look::Animal(id, color) => crate::avatar_colors::image(&mood_avatar(id, mood, glance), color),
        Look::Custom(avatar) => {
            let pose = match mood {
                Mood::Replying | Mood::Working => &avatar.poses.working,
                Mood::Due | Mood::Celebrating => &avatar.poses.waving,
                Mood::Night => &avatar.poses.sleeping,
                Mood::Idle | Mood::Listening => &avatar.poses.idle,
            };
            pose.image()
        }
    }
}

/// The pet's look: the character picked on Settings › Desktop pet, at rest (the main agent's own pose when it's the main
/// agent's animal); unpicked, or a custom pet that's gone, `own`, the main agent's picture as a character.
pub fn pet_look(picked: Option<&str>, own: &Look, custom: &[CustomAvatar]) -> Look {
    let picked = picked.and_then(|picked| match avatars::custom_id_of(picked) {
        Some(id) => custom.iter().find(|avatar| avatar.id == id).cloned().map(Look::Custom),
        None if pet_character(own) == picked => Some(own.clone()),
        None => avatars::ANIMALS
            .iter()
            .find(|animal| animal.id == picked)
            .map(|animal| Look::Animal(animal.first_avatar(), crate::avatar_colors::DEFAULT_COLOR.into())),
    });
    picked.unwrap_or_else(|| own.clone())
}

/// How a look is kept as the pet's character: its animal, or `custom:<id>`.
pub fn pet_character(look: &Look) -> String {
    match look {
        Look::Animal(id, _) => avatars::animal_of(id).map_or_else(|| id.clone(), |animal| animal.id.to_string()),
        Look::Custom(avatar) => avatars::custom_ref(&avatar.id),
    }
}

/// The pet's name: its animal ("Fox") or the custom avatar's name.
pub fn pet_name(look: &Look) -> String {
    match look {
        Look::Animal(id, _) => avatars::animal_of(id).map_or_else(|| id.clone(), |animal| animal.label.to_string()),
        Look::Custom(avatar) => avatar.name.clone(),
    }
}

/// Every picture the look can show, to load them ahead.
pub fn all_mood_images(look: &Look) -> Vec<ImageSource> {
    let moods = [Mood::Idle, Mood::Night, Mood::Working, Mood::Replying, Mood::Celebrating, Mood::Due];
    let mut keys = Vec::new();
    let mut images = Vec::new();
    for mood in moods {
        for glance in [Glance::Ahead, Glance::Left, Glance::Right] {
            let key = mood_key(look, mood, glance);
            if !keys.contains(&key) {
                keys.push(key);
                images.push(mood_image(look, mood, glance));
            }
        }
    }
    images
}

/// Which picture `mood_image` shows, to tell when it changes.
pub fn mood_key(look: &Look, mood: Mood, glance: Glance) -> String {
    match look {
        Look::Animal(id, color) => format!("{}:{color}", mood_avatar(id, mood, glance)),
        Look::Custom(avatar) => match mood {
            Mood::Replying | Mood::Working => avatar.poses.working.url.clone(),
            Mood::Due | Mood::Celebrating => avatar.poses.waving.url.clone(),
            Mood::Night => avatar.poses.sleeping.url.clone(),
            Mood::Idle | Mood::Listening => avatar.poses.idle.url.clone(),
        },
    }
}

/// Whether the look can glance left and right (the animals other than the panda).
pub fn can_glance(look: &Look) -> bool {
    matches!(look, Look::Animal(id, _) if !id.starts_with("panda-"))
}

// ---------------------------------------------------------------------------------------------------------------
// The companion

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    pub due_at: f64,
}

use crate::unseen::{Summary as UnseenSummary, Told, UnseenChat};

/// A reply for the pet to say.
#[derive(Clone, Debug)]
pub struct Reply {
    pub session_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub body: String,
    /// An agent's chat (or the agents' group).
    pub agent: bool,
}

/// `voiceSessions:working`: a chat with work running or a reply under way, of any kind.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkingRow {
    #[serde(rename = "_id")]
    id: String,
    title: String,
    /// Missing for a chat only asked about (a coding task's).
    work_started_at: Option<f64>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    agent_group: bool,
}

/// Where a working chat opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Opens {
    Chat,
    /// An agent's chat (the agent's id) or the agents' group (`agents::GROUP`), in the Agents tab.
    Agent(String),
}

/// A chat Filo works in (or an agent, or a coding tool for it), for the pet's tray: one row a chat, like the chat
/// list's "Working" rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Working {
    pub session_id: String,
    pub title: String,
    /// When its oldest work began (ms), for "3m".
    pub since: Option<f64>,
    pub opens: Opens,
}

/// `coding:trayTasks`: a coding task working, or finished a moment ago, for the pet's tray.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrayTask {
    task_id: String,
    session_id: String,
    #[serde(default)]
    agent_id: Option<String>,
    /// Started in the agents' group.
    #[serde(default)]
    group: bool,
    title: String,
    /// "queued" or "running"; "done", "failed" or "stopped" once finished.
    status: String,
    /// Working: when its round began, and what it waits on the user for.
    #[serde(default)]
    since: Option<f64>,
    #[serde(default)]
    ask: Option<Ask>,
    /// Finished: when, its report in a line, and its round.
    #[serde(default)]
    finished_at: Option<f64>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    round: Option<f64>,
}

/// What a coding tool waits on the user for: a permission (allow or deny) or a question (answered in the chat).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    pub request_id: String,
    /// "approval" or "question".
    pub kind: String,
    pub question: String,
}

impl Ask {
    pub fn is_permission(&self) -> bool {
        self.kind == "approval"
    }
}

/// How the coding tasks a tray row stands for went, finished a moment ago.
#[derive(Clone, Debug, PartialEq)]
pub struct Finished {
    /// When the newest finished.
    pub at: f64,
    /// How many.
    pub count: usize,
    /// "done", "failed" or "stopped": every one done is "done"; else "failed" if one failed.
    pub status: String,
    /// The one task's report in a line, else its title (empty for several).
    pub summary: String,
    pub round: Option<u32>,
}

/// A row of the pet's tray: one an agent (or Filo in a chat, or the agents' group) with what it works on folded in (a
/// reply and its coding tasks); one for a coding task waiting on the user; or one an agent for its tasks that just
/// finished.
#[derive(Clone, Debug, PartialEq)]
pub struct TrayRow {
    /// "agent:<id>", "group" or "chat:<id>" (and "done:" before it for finished tasks), or the waiting task's id.
    pub key: String,
    /// The waiting task, or the one finished task (its result opens).
    pub task_id: Option<String>,
    pub session_id: String,
    /// The waiting task's title; for Filo in a chat, the chat's title (an agent's row says its name).
    pub title: String,
    /// When its work began (ms), for "3m": the earliest folded in.
    pub since: Option<f64>,
    pub opens: Opens,
    /// Who works: an agent (its id); none: Filo, or the group's members.
    pub agent_id: Option<String>,
    /// The coding tasks working, folded into the row ("Working · 3 tasks").
    pub tasks: usize,
    pub ask: Option<Ask>,
    pub finished: Option<Finished>,
    /// The finished tasks the row stands for, off the tray once it's opened.
    pub dismiss: Vec<String>,
}

impl TrayRow {
    /// What a click on its row does: the one finished task's result, else its agent or chat.
    pub fn command(&self) -> Command {
        match (&self.finished, &self.task_id) {
            (Some(finished), Some(task_id)) if finished.count == 1 => {
                Command::OpenCodingResult(task_id.clone(), finished.round)
            }
            _ => self.open_chat(),
        }
    }

    /// Where its agent or chat opens (a question is answered there).
    pub fn open_chat(&self) -> Command {
        match &self.opens {
            Opens::Chat => Command::OpenChat(self.session_id.clone()),
            Opens::Agent(agent) => Command::OpenAgent(agent.clone()),
        }
    }
}

/// A reply nobody has seen, for the pet's bubble.
pub enum CompanionEvent {
    Reply(Reply),
}

pub struct Companion {
    /// The pet's look (`pet_look`), from the pick, the main agent's picture and the custom avatars; none until the agents
    /// load.
    pet_look: Option<Look>,
    custom_avatars: Vec<CustomAvatar>,
    /// The main agent's id (`agents:list`), who owns what's no agent's: none until the agents load, then none while
    /// there are no agents.
    main: Option<Option<String>>,
    /// The main agent's name, for what's no agent's (Filo, the app, while there's no agent).
    agent_name: String,
    /// Chats with work running, newest work first.
    working: Vec<Working>,
    /// The pet's tray: tasks needing the user, then what works, then tasks just finished.
    tray: Vec<TrayRow>,
    /// Each coding task (`coding:trayTasks`).
    tray_tasks: Vec<TrayTask>,
    /// Requests answered from the tray, off it before the tasks come again; finished tasks opened from it.
    answered: HashSet<String>,
    dismissed: HashSet<String>,
    /// The user's agents by id (`agents:list`): the tray's names and pictures; and each agent's chat, by its id.
    agents: HashMap<String, AgentInfo>,
    agent_chats: HashMap<String, String>,
    /// The agents' group (`agents:group`), for its name and picture as the user set them.
    group: Option<crate::screens::agents::GroupRow>,
    /// The conversation on screen while it has a reply under way: (chat id, its title).
    replying_on_screen: Option<(String, String)>,
    /// What makes `working`: every working chat (`voiceSessions:working`; None until it answers), and the chats a
    /// coding tool is working for, since when (`coding:workingChats`), which the first is asked to name.
    served: Option<Vec<WorkingRow>>,
    coding_chats: HashMap<String, f64>,
    /// The agents with a coding task working, since when, whichever chat it's in (the Agents list's "Working").
    coding_agents: HashMap<String, f64>,
    /// The `voiceSessions:working` subscription and the coding chats it names.
    working_sub: Option<Task<()>>,
    working_include: Vec<String>,
    /// `FILO_PET_DEMO=tray`: made-up working chats, to see the pet's tray without starting any work (`dev` builds).
    #[cfg(feature = "dev")]
    demo_working: Vec<Working>,
    replying: bool,
    listening: bool,
    /// The recording goes with a screenshot.
    listening_to_screen: bool,
    /// Who what's being said from another app goes to as things stand ("→ Milo"); none in Filo's window (the open
    /// chat), or until it's known.
    listening_to: Option<String>,
    /// The words heard so far of what's being said from another app (live_transcription.rs), or why there won't be any.
    listening_words: String,
    listening_problem: Option<String>,
    /// When the user last talked to Filo with the shortcut: its reply goes in the pet's bubble.
    talked_at: Option<f64>,
    night: bool,
    celebrate_until: f64,
    reminders: Vec<Reminder>,
    unseen: Vec<UnseenChat>,
    /// The replies the pet said (unseen.rs `Told`, as the banners are told).
    said: Told,
    /// Reminders that came due while Filo ran, newest first, until done, snoozed or dismissed.
    due: Vec<Reminder>,
    /// (reminder, due time) already raised, so each comes up once (a snooze moves the time).
    raised: HashSet<(String, i64)>,
    started_at: f64,
    last_menu: Option<MenuState>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<CompanionEvent> for Companion {}

pub struct CompanionGlobal(pub Entity<Companion>);

impl Global for CompanionGlobal {}

pub fn companion(cx: &App) -> Option<Entity<Companion>> {
    cx.try_global::<CompanionGlobal>().map(|global| global.0.clone())
}

/// Filo's state for the menu bar and the pet, and the pet if it's on.
pub fn start(cx: &mut App) {
    let companion = cx.new(Companion::new);
    companion.update(cx, |companion, cx| companion.subscribe_working(cx));
    cx.set_global(CompanionGlobal(companion));
    crate::pet::sync(cx);
}

impl Companion {
    fn new(cx: &mut Context<Self>) -> Self {
        let tasks = vec![
            backend::subscribe(
                cx,
                "customAvatars:list",
                json!({}),
                |this: &mut Self, result: Result<Vec<CustomAvatar>, String>, cx| {
                    if let Ok(avatars) = result {
                        this.custom_avatars = avatars;
                        this.pick_pet(cx);
                        this.changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "coding:workingChats",
                json!({}),
                |this: &mut Self, result: Result<Vec<WorkingChat>, String>, cx| {
                    if let Ok(chats) = result {
                        this.coding_agents = coding_agents(&chats);
                        this.coding_chats = chats.into_iter().map(|chat| (chat.session_id, chat.since)).collect();
                        this.subscribe_working(cx);
                        this.count_working();
                        this.changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "coding:trayTasks",
                json!({}),
                |this: &mut Self, result: Result<Vec<TrayTask>, String>, cx| {
                    if let Ok(tasks) = result {
                        this.answered.retain(|id| {
                            tasks.iter().any(|task| task.ask.as_ref().is_some_and(|ask| &ask.request_id == id))
                        });
                        this.tray_tasks = tasks;
                        this.count_working();
                        this.changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "agents:list",
                json!({}),
                |this: &mut Self, result: Result<Vec<AgentRow>, String>, cx| {
                    if let Ok(rows) = result {
                        this.agent_chats =
                            rows.iter().filter_map(|row| Some((row.session_id.clone()?, row.id.clone()))).collect();
                        let main = rows.iter().find(|row| row.main);
                        this.agent_name = main.map_or_else(|| "Filo".into(), |row| row.name.clone());
                        this.main = Some(main.map(|row| row.id.clone()));
                        this.agents = rows
                            .into_iter()
                            .map(|row| {
                                (row.id, AgentInfo { name: row.name, picture: row.picture, background: row.background })
                            })
                            .collect();
                        this.pick_pet(cx);
                        this.count_working();
                        this.changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "agents:group",
                json!({}),
                |this: &mut Self, result: Result<Option<crate::screens::agents::GroupRow>, String>, cx| {
                    if let Ok(group) = result {
                        let renamed = this.group.as_ref().map(|group| (&group.name, &group.picture, &group.background))
                            != group.as_ref().map(|group| (&group.name, &group.picture, &group.background));
                        this.group = group;
                        if renamed {
                            this.changed(cx);
                        }
                    }
                },
            ),
            backend::subscribe(
                cx,
                "voiceSessions:unseen",
                json!({}),
                |this: &mut Self, result: Result<UnseenSummary, String>, cx| {
                    if let Ok(summary) = result {
                        this.new_replies(&summary.chats, cx);
                        this.unseen = summary.chats;
                        this.unseen.sort_by(|a, b| b.latest_at.total_cmp(&a.latest_at));
                        this.changed(cx);
                    }
                },
            ),
            backend::subscribe(
                cx,
                "reminders:myReminders",
                json!({}),
                |this: &mut Self, result: Result<Vec<Reminder>, String>, cx| {
                    if let Ok(reminders) = result {
                        // Done, snoozed or cancelled elsewhere (Scheduled): its bubble goes.
                        this.due.retain(|due| {
                            reminders.iter().any(|reminder| reminder.id == due.id && reminder.due_at == due.due_at)
                        });
                        this.reminders = reminders;
                        this.check_due(cx);
                        this.changed(cx);
                    }
                },
            ),
            // Replies in the open chat, the recording, reminders coming due, night.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                    if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                        break;
                    }
                }
            }),
        ];
        Companion {
            pet_look: None,
            custom_avatars: Vec::new(),
            main: None,
            agent_name: "Filo".into(),
            working: Vec::new(),
            tray: Vec::new(),
            tray_tasks: Vec::new(),
            answered: HashSet::new(),
            dismissed: HashSet::new(),
            agents: HashMap::new(),
            agent_chats: HashMap::new(),
            group: None,
            replying_on_screen: None,
            coding_chats: HashMap::new(),
            coding_agents: HashMap::new(),
            served: None,
            working_sub: None,
            working_include: Vec::new(),
            #[cfg(feature = "dev")]
            demo_working: demo_working(),
            replying: false,
            listening: false,
            listening_to_screen: false,
            listening_to: None,
            listening_words: String::new(),
            listening_problem: None,
            talked_at: None,
            night: is_night(chrono::Timelike::hour(&chrono::Local::now())),
            celebrate_until: 0.,
            reminders: Vec::new(),
            unseen: Vec::new(),
            said: Told::default(),
            due: Vec::new(),
            raised: HashSet::new(),
            started_at: format::now_ms(),
            last_menu: None,
            _tasks: tasks,
        }
    }

    /// The main agent, who owns what's no agent's (the tray's rows of no agent).
    pub fn main_agent(&self) -> Option<&AgentInfo> {
        self.agents.get(self.main.as_ref()?.as_ref()?)
    }

    /// What the pet looks like: the character picked for it, else the main agent's picture.
    pub fn pet_look(&self) -> Option<&Look> {
        self.pet_look.as_ref()
    }

    /// Puts the pet's look together again, after the agents, the custom avatars or the pick changed: the main agent's
    /// picture as a character, else (no agent, a photo or a helper's picture) the panda.
    pub fn pick_pet(&mut self, cx: &App) {
        if self.main.is_none() {
            return;
        }
        let own = self
            .main_agent()
            .and_then(|agent| Look::of_picture(&agent.picture, &self.custom_avatars))
            .unwrap_or_else(Look::default_animal);
        self.pet_look = Some(pet_look(settings(cx).pet_character.as_deref(), &own, &self.custom_avatars));
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn mood(&self) -> Mood {
        pick_mood(Signals {
            listening: self.listening,
            due: !self.due.is_empty(),
            celebrating: format::now_ms() < self.celebrate_until,
            replying: self.replying,
            working: !self.working.is_empty(),
            night: self.night,
        })
    }

    pub fn listening(&self) -> bool {
        self.listening
    }

    pub fn listening_to_screen(&self) -> bool {
        self.listening_to_screen
    }

    /// Who the voice message being held goes to, when it's held from another app.
    pub fn listening_to(&self) -> Option<&str> {
        self.listening_to.as_deref().filter(|_| self.listening)
    }

    /// The words heard so far of the voice message held from another app.
    pub fn listening_words(&self) -> Option<&str> {
        Some(self.listening_words.as_str()).filter(|words| self.listening && !words.trim().is_empty())
    }

    /// Why the voice message held from another app can't be sent (voice messages used up for today, offline).
    pub fn listening_problem(&self) -> Option<&str> {
        self.listening_problem.as_deref().filter(|_| self.listening)
    }

    fn stop_listening(&mut self) {
        self.listening = false;
        self.listening_to_screen = false;
        self.listening_to = None;
        self.listening_words.clear();
        self.listening_problem = None;
    }

    /// The reminder the pet raises now.
    pub fn due(&self) -> Option<&Reminder> {
        self.due.first()
    }

    /// Filo's unseen replies (agents' included), for the pet's badge.
    pub fn unseen_total(&self) -> u32 {
        self.unseen.iter().map(|chat| chat.count as u32).sum()
    }

    /// The newest chat with an unseen reply.
    pub fn newest_unseen(&self) -> Option<String> {
        self.unseen.first().map(|chat| chat.session_id.clone())
    }

    /// One line on what Filo and the agents are doing, if anything (the menu bar's; the pet's tray shows it whole).
    pub fn status(&self) -> Option<String> {
        if self.listening {
            return Some(match self.listening_to() {
                Some(to) => format!("Listening → {to}… let go to send"),
                None => "Listening… let go to send".into(),
            });
        }
        tray_status(&self.tray, |row| self.who(row))
            .or_else(|| self.replying.then(|| format!("{} is working…", self.agent_name)))
    }

    /// Who works on a tray row, by name: the agent, the agents' group, or Filo.
    pub fn who(&self, row: &TrayRow) -> String {
        if let Some(agent) = row.agent_id.as_deref().and_then(|id| self.agents.get(id)) {
            return agent.name.clone();
        }
        if row.agent_id.is_none() && row.opens == Opens::Agent(crate::screens::agents::GROUP.to_string()) {
            return crate::screens::agents::group_title(self.group.as_ref().and_then(|group| group.name.as_deref()));
        }
        self.agent_name.clone()
    }

    /// Chats with work running, newest first, for the pet's tray.
    pub fn working(&self) -> &[Working] {
        &self.working
    }

    /// The pet's tray: coding tasks needing the user first, then what works, then tasks just finished.
    pub fn tray(&self) -> &[TrayRow] {
        &self.tray
    }

    /// A coding tool waits on the user (the pet shows a "!").
    pub fn needs_you(&self) -> bool {
        self.tray.iter().any(|row| row.ask.is_some())
    }

    /// The group's picture on its round tile, as the user set it.
    pub fn group_tile(&self, size: f32, cx: &App) -> gpui::Div {
        use crate::screens::agents;
        match &self.group {
            Some(group) => group.tile(size, cx),
            None => agents::round_picture(agents::group_picture(), size, cx),
        }
    }

    /// One of the user's agents, for its name and picture.
    pub fn agent(&self, id: &str) -> Option<&AgentInfo> {
        self.agents.get(id)
    }

    /// Finished tasks opened from the tray: off it.
    pub fn dismiss_finished(&mut self, task_ids: &[String], cx: &mut Context<Self>) {
        self.dismissed.extend(task_ids.iter().cloned());
        self.count_working();
        self.changed(cx);
    }

    /// The agents with a coding task working, and since when, whichever chat the task is in.
    pub fn coding_agents(&self) -> &HashMap<String, f64> {
        &self.coding_agents
    }

    /// The next few Scheduled items: (label, when).
    pub fn upcoming(&self, now: f64) -> Vec<(String, String)> {
        let mut next: Vec<&Reminder> = self.reminders.iter().filter(|reminder| reminder.due_at >= now).collect();
        next.sort_by(|a, b| a.due_at.total_cmp(&b.due_at));
        next.into_iter()
            .take(UPCOMING)
            .map(|reminder| (reminder.title.clone(), format::reminder_due(reminder.due_at, now)))
            .collect()
    }

    /// Replies nobody has seen for a few seconds (`pushedUntil`, unseen.rs's rule): the pet says them.
    fn new_replies(&mut self, chats: &[UnseenChat], cx: &mut Context<Self>) {
        let (news, _) = self.said.news(chats);
        let replies: Vec<Reply> = news
            .into_iter()
            .map(|chat| {
                let (title, subtitle, body) = chat.notice(&self.agent_name);
                Reply { session_id: chat.session_id.clone(), title, subtitle, body, agent: chat.in_agents() }
            })
            .collect();
        for reply in replies {
            self.reply(reply, cx);
        }
    }

    fn reply(&mut self, notice: Reply, cx: &mut Context<Self>) {
        self.celebrate(cx);
        // In front, Filo shows the reply in its own window (in_app_notice.rs).
        if !app_active() {
            cx.emit(CompanionEvent::Reply(notice));
        }
    }

    pub fn celebrate(&mut self, cx: &mut Context<Self>) {
        self.celebrate_until = format::now_ms() + CELEBRATE_MS;
        self.changed(cx);
    }

    pub fn dismiss_due(&mut self, id: &str, cx: &mut Context<Self>) {
        self.due.retain(|due| due.id != id);
        self.changed(cx);
    }

    fn check_due(&mut self, cx: &mut Context<Self>) {
        let now = format::now_ms();
        let mut came_due: Vec<Reminder> = self
            .reminders
            .iter()
            .filter(|reminder| reminder.due_at <= now && reminder.due_at >= self.started_at - DUE_GRACE_MS)
            .filter(|reminder| !self.raised.contains(&(reminder.id.clone(), reminder.due_at as i64)))
            .cloned()
            .collect();
        if came_due.is_empty() {
            return;
        }
        came_due.sort_by(|a, b| b.due_at.total_cmp(&a.due_at));
        for reminder in &came_due {
            self.raised.insert((reminder.id.clone(), reminder.due_at as i64));
        }
        self.due.retain(|due| !came_due.iter().any(|reminder| reminder.id == due.id));
        came_due.append(&mut self.due);
        self.due = came_due;
        self.changed(cx);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let (replying, recording) = open_conversation_state(cx);
        // The tray has a row for the reply on screen, even before (or without) the backend counting it.
        let on_screen = if replying { open_conversation_chat(cx) } else { None };
        if on_screen != self.replying_on_screen {
            self.replying_on_screen = on_screen;
            self.count_working();
        }
        let finished = self.replying && !replying;
        self.replying = replying;
        if self.listening && !recording {
            self.stop_listening();
        }
        self.night = is_night(chrono::Timelike::hour(&chrono::Local::now()));
        // A finished task's time in the tray is up.
        let now = format::now_ms();
        if self
            .tray
            .iter()
            .any(|row| row.finished.as_ref().is_some_and(|finished| now - finished.at >= FINISHED_LINGER_MS))
        {
            self.count_working();
        }
        if finished {
            self.celebrate(cx);
            // Asked from another app: the answer comes to the pet, since Filo may well count it as seen (its window
            // in view) and raise no notice.
            let asked = self.talked_at.take().is_some_and(|at| format::now_ms() - at < TALK_REPLY_MS);
            if asked
                && !app_active()
                && let Some((session_id, title, text)) = open_chat_reply(cx)
            {
                let title = if title.trim().is_empty() { self.agent_name.clone() } else { title };
                // The chat on screen is an agent's (or the group): the Agents page shows it.
                cx.emit(CompanionEvent::Reply(Reply { session_id, title, subtitle: None, body: text, agent: true }));
            }
        }
        self.check_due(cx);
        self.changed(cx);
    }

    /// (Re)subscribes to every working chat, asking it to name the coding tasks' chats; only when those change.
    fn subscribe_working(&mut self, cx: &mut Context<Self>) {
        let mut include: Vec<String> = self.coding_chats.keys().cloned().collect();
        include.sort();
        if self.working_sub.is_some() && include == self.working_include {
            return;
        }
        let args = json!({ "include": include });
        self.working_include = include;
        self.working_sub = Some(backend::subscribe(
            cx,
            "voiceSessions:working",
            args,
            |this: &mut Self, result: Result<Vec<WorkingRow>, String>, cx| {
                match result {
                    Ok(rows) => this.served = Some(rows),
                    Err(error) => eprintln!("companion: voiceSessions:working: {error}"),
                }
                this.count_working();
                this.changed(cx);
            },
        ));
    }

    /// Chats where Filo or an agent works, or a coding tool works for it.
    fn count_working(&mut self) {
        self.working = working_chats(self.served.as_deref(), &self.coding_chats, &self.agent_name);
        let tasks: Vec<TrayTask> =
            self.tray_tasks.iter().filter(|task| !self.dismissed.contains(&task.task_id)).cloned().collect();
        // Chats with a reply under way: Filo's or an agent's turn running (not only a coding task named there).
        let mut replying: HashMap<String, Option<f64>> = self
            .served
            .iter()
            .flatten()
            .filter(|row| row.work_started_at.is_some())
            .map(|row| (row.id.clone(), row.work_started_at))
            .collect();
        let mut chats = self.working.clone();
        if let Some((id, title)) = &self.replying_on_screen {
            replying.entry(id.clone()).or_insert(None);
            if !chats.iter().any(|chat| &chat.session_id == id) {
                let opens = Opens::Agent(
                    self.agent_chats.get(id).cloned().unwrap_or_else(|| crate::screens::agents::GROUP.to_string()),
                );
                let title = if title.trim().is_empty() { self.agent_name.clone() } else { title.trim().to_string() };
                chats.insert(0, Working { session_id: id.clone(), title, since: None, opens });
            }
        }
        self.tray = tray_rows(&chats, &tasks, &replying, &self.answered, format::now_ms());
        #[cfg(feature = "dev")]
        {
            self.working.extend(self.demo_working.iter().cloned());
            self.tray.extend(demo_tray());
        }
    }

    /// Redraws the pet and refreshes the menu, when something it shows changed.
    pub fn changed(&mut self, cx: &mut Context<Self>) {
        let menu = menu_state(Some(self), cx);
        if self.last_menu.as_ref() != Some(&menu) {
            crate::menu_bar::set(menu.clone());
            self.last_menu = Some(menu);
        }
        cx.notify();
    }
}

/// Whether the agent's conversation on screen has a reply under way, and whether it's recording (or the open shortcut
/// is, from another app).
fn open_conversation_state(cx: &App) -> (bool, bool) {
    let Some(shell) = cx.try_global::<NavGlobal>().and_then(|nav| nav.0.upgrade()) else { return (false, false) };
    // Held from another app, the recording isn't any chat's.
    let away = shell.read(cx).talking_away();
    let Some(conversation) = shell.read(cx).agents_screen().and_then(|agents| agents.read(cx).current_conversation())
    else {
        return (false, away);
    };
    let conversation = conversation.read(cx);
    (conversation.replying(), conversation.recording() || away)
}

/// The agent's conversation on screen: (chat id, its title, "" before its first reply is loaded).
fn open_conversation_chat(cx: &App) -> Option<(String, String)> {
    let shell = cx.try_global::<NavGlobal>()?.0.upgrade()?;
    let conversation = shell.read(cx).agents_screen()?.read(cx).current_conversation()?;
    let conversation = conversation.read(cx);
    let id = conversation.session_id()?.to_string();
    let title =
        conversation.latest_reply().filter(|(chat, _, _)| *chat == id).map(|(_, title, _)| title).unwrap_or_default();
    Some((id, title))
}

/// The newest reply in the agent's conversation on screen.
fn open_chat_reply(cx: &App) -> Option<(String, String, String)> {
    let shell = cx.try_global::<NavGlobal>()?.0.upgrade()?;
    let conversation = shell.read(cx).agents_screen()?.read(cx).current_conversation()?;
    conversation.read(cx).latest_reply()
}

/// The agents whose coding tasks work, each since its earliest.
fn coding_agents(chats: &[WorkingChat]) -> HashMap<String, f64> {
    let mut agents: HashMap<String, f64> = HashMap::new();
    for chat in chats {
        for agent in &chat.agent_ids {
            let since = agents.entry(agent.clone()).or_insert(chat.since);
            *since = since.min(chat.since);
        }
    }
    agents
}

/// Who works, one row a chat: Filo or an agent working in it, or a coding tool working for it, since the earliest.
/// Newest work first.
fn working_chats(served: Option<&[WorkingRow]>, coding: &HashMap<String, f64>, agent_name: &str) -> Vec<Working> {
    let mut rows: Vec<Working> = Vec::new();
    let mut add = |id: &str, title: Option<&str>, since: Option<f64>, opens: Option<Opens>| {
        let row = match rows.iter_mut().position(|row| row.session_id == id) {
            Some(index) => &mut rows[index],
            None => {
                rows.push(Working {
                    session_id: id.to_string(),
                    title: String::new(),
                    since: None,
                    opens: Opens::Chat,
                });
                rows.last_mut().expect("just pushed")
            }
        };
        if row.title.is_empty() {
            row.title = title.unwrap_or_default().trim().to_string();
        }
        row.since = row.since.into_iter().chain(since).reduce(f64::min);
        if let Some(opens) = opens {
            row.opens = opens;
        }
    };
    for row in served.unwrap_or_default() {
        // A chat only named for its coding task counts below, if the task still works.
        if row.work_started_at.is_none() && !coding.contains_key(&row.id) {
            continue;
        }
        let opens = if row.agent_group {
            Opens::Agent(crate::screens::agents::GROUP.to_string())
        } else if let Some(agent) = &row.agent_id {
            Opens::Agent(agent.clone())
        } else {
            Opens::Chat
        };
        add(&row.id, Some(&row.title), row.work_started_at, Some(opens));
    }
    let named: HashMap<&str, &str> =
        served.unwrap_or_default().iter().map(|row| (row.id.as_str(), row.title.as_str())).collect();
    for (id, since) in coding {
        add(id, named.get(id.as_str()).copied(), Some(*since), None);
    }
    for row in &mut rows {
        if row.title.is_empty() {
            row.title = agent_name.to_string();
        }
    }
    rows.sort_by(|a, b| {
        b.since.unwrap_or(0.).total_cmp(&a.since.unwrap_or(0.)).then_with(|| a.session_id.cmp(&b.session_id))
    });
    rows
}

/// The tray in a line: what waits on the user, else who works (or how many are at work).
fn tray_status(rows: &[TrayRow], who: impl Fn(&TrayRow) -> String) -> Option<String> {
    if let Some(row) = rows.iter().find(|row| row.ask.is_some()) {
        return Some(format!("{} needs you on “{}”", who(row), short(&row.title, 36)));
    }
    let mut at_work = rows.iter().filter(|row| row.finished.is_none());
    match (at_work.next(), at_work.count()) {
        (None, _) => None,
        (Some(row), 0) if row.tasks > 1 => Some(format!("{} is working on {} tasks", who(row), row.tasks)),
        (Some(row), 0) => Some(format!("{} is working…", who(row))),
        (Some(_), others) => Some(format!("{} at work", others + 1)),
    }
}

/// Whose a piece of work is, for folding it into a row: (key, where the row opens, the agent). An agent's (wherever
/// its chat is, the group included), the agents' group's, or Filo's in that chat.
fn tray_owner(agent: Option<&str>, opens: &Opens, session: &str) -> (String, Opens, Option<String>) {
    let group = crate::screens::agents::GROUP;
    match (agent, opens) {
        (Some(agent), _) => (format!("agent:{agent}"), Opens::Agent(agent.to_string()), Some(agent.to_string())),
        (None, Opens::Agent(id)) if id == group => ("group".into(), opens.clone(), None),
        (None, Opens::Agent(id)) => (format!("agent:{id}"), opens.clone(), Some(id.clone())),
        (None, Opens::Chat) => (format!("chat:{session}"), Opens::Chat, None),
    }
}

/// The pet's tray: a row an agent (or Filo in a chat, or the group) at work, its reply (`replying`, since when if
/// known) and its coding tasks folded in; a row for each coding task waiting on the user (`answered` ones no longer
/// wait); and a row an agent for its tasks finished in the last `FINISHED_LINGER_MS`. A working chat with no coding
/// task listed counts as at work too. Waiting first, then at
/// work newest first, then finished, newest first.
fn tray_rows(
    working: &[Working],
    tasks: &[TrayTask],
    replying: &HashMap<String, Option<f64>>,
    answered: &HashSet<String>,
    now: f64,
) -> Vec<TrayRow> {
    let blank = |key: String, session: &str, opens: Opens, agent_id: Option<String>| TrayRow {
        key,
        task_id: None,
        session_id: session.to_string(),
        title: String::new(),
        since: None,
        opens,
        agent_id,
        tasks: 0,
        ask: None,
        finished: None,
        dismiss: Vec::new(),
    };
    let earliest = |a: Option<f64>, b: Option<f64>| a.into_iter().chain(b).reduce(f64::min);
    let (mut asks, mut at_work, mut finished): (Vec<TrayRow>, Vec<TrayRow>, Vec<TrayRow>) =
        (Vec::new(), Vec::new(), Vec::new());
    let find = |rows: &mut Vec<TrayRow>, row: TrayRow| -> usize {
        match rows.iter().position(|candidate| candidate.key == row.key) {
            Some(index) => index,
            None => {
                rows.push(row);
                rows.len() - 1
            }
        }
    };
    for task in tasks {
        let opens = if task.group {
            Opens::Agent(crate::screens::agents::GROUP.to_string())
        } else {
            working
                .iter()
                .find(|chat| chat.session_id == task.session_id)
                .map_or(Opens::Chat, |chat| chat.opens.clone())
        };
        let (key, opens, agent_id) = tray_owner(task.agent_id.as_deref(), &opens, &task.session_id);
        match (task.finished_at, task.ask.clone().filter(|ask| !answered.contains(&ask.request_id))) {
            (Some(at), _) if now - at < FINISHED_LINGER_MS => {
                let index = find(&mut finished, blank(format!("done:{key}"), &task.session_id, opens, agent_id));
                let row = &mut finished[index];
                let summary = task
                    .summary
                    .clone()
                    .filter(|summary| !summary.trim().is_empty())
                    .unwrap_or_else(|| task.title.trim().to_string());
                row.finished = Some(match row.finished.take() {
                    None => Finished {
                        at,
                        count: 1,
                        status: task.status.clone(),
                        summary,
                        round: task.round.map(|round| round as u32),
                    },
                    Some(before) => Finished {
                        at: before.at.max(at),
                        count: before.count + 1,
                        status: if before.status == "failed" || task.status == "failed" {
                            "failed".into()
                        } else if before.status == "done" && task.status == "done" {
                            "done".into()
                        } else {
                            "stopped".into()
                        },
                        summary: String::new(),
                        round: None,
                    },
                });
                row.task_id = (row.dismiss.is_empty()).then(|| task.task_id.clone());
                row.dismiss.push(task.task_id.clone());
            }
            (Some(_), _) => {}
            (None, Some(ask)) => asks.push(TrayRow {
                task_id: Some(task.task_id.clone()),
                title: task.title.trim().to_string(),
                since: task.since,
                ask: Some(ask),
                ..blank(task.task_id.clone(), &task.session_id, opens, agent_id)
            }),
            (None, None) => {
                let index = find(&mut at_work, blank(key, &task.session_id, opens, agent_id));
                let row = &mut at_work[index];
                row.tasks += 1;
                row.since = earliest(row.since, task.since);
            }
        }
    }
    for chat in working {
        // A chat counts for its reply, or for work the tasks don't account for; not as the place of a coding task.
        let reply = replying.get(&chat.session_id);
        let has_tasks = tasks.iter().any(|task| task.session_id == chat.session_id && task.finished_at.is_none());
        if reply.is_none() && has_tasks {
            continue;
        }
        let (key, opens, agent_id) = tray_owner(None, &chat.opens, &chat.session_id);
        let index = find(&mut at_work, blank(key, &chat.session_id, opens, agent_id));
        let row = &mut at_work[index];
        if row.title.is_empty() && row.opens == Opens::Chat {
            row.title = chat.title.clone();
        }
        row.since = earliest(row.since, reply.copied().flatten().or(chat.since));
    }
    asks.sort_by(|a, b| b.since.unwrap_or(0.).total_cmp(&a.since.unwrap_or(0.)).then_with(|| a.key.cmp(&b.key)));
    at_work.sort_by(|a, b| b.since.unwrap_or(0.).total_cmp(&a.since.unwrap_or(0.)).then_with(|| a.key.cmp(&b.key)));
    finished.sort_by(|a, b| {
        let at = |row: &TrayRow| row.finished.as_ref().map_or(0., |finished| finished.at);
        at(b).total_cmp(&at(a)).then_with(|| a.key.cmp(&b.key))
    });
    asks.into_iter().chain(at_work).chain(finished).collect()
}

/// `FILO_PET_DEMO=tray`: the tray's kinds of rows, to see them without starting any work: Filo at work on three
/// tasks, the group replying, and two tasks just done; `=tray-asks` puts a permission and a question waiting first
/// (the question's title and text too long for the tray, so they wrap and end in "…"). In `dev` builds only.
#[cfg(feature = "dev")]
fn demo_tray() -> Vec<TrayRow> {
    let Ok(demo) = std::env::var("FILO_PET_DEMO") else { return Vec::new() };
    if demo != "tray" && demo != "tray-asks" {
        return Vec::new();
    }
    let now = format::now_ms();
    let row = |key: &str, minutes: f64, opens: Opens| TrayRow {
        key: key.into(),
        task_id: None,
        session_id: format!("demo-{key}"),
        title: String::new(),
        since: Some(now - minutes * 60_000.),
        opens,
        agent_id: None,
        tasks: 0,
        ask: None,
        finished: None,
        dismiss: Vec::new(),
    };
    let filo = TrayRow { tasks: 3, ..row("chat:demo", 4., Opens::Chat) };
    let group = row("group", 1., Opens::Agent(crate::screens::agents::GROUP.into()));
    let done = TrayRow {
        since: None,
        finished: Some(Finished { at: now, count: 2, status: "done".into(), summary: String::new(), round: None }),
        dismiss: vec!["demo-a".into(), "demo-b".into()],
        ..row("done:chat:demo", 0., Opens::Chat)
    };
    let ask = |key: &str, title: &str, kind: &str, question: &str, minutes: f64| TrayRow {
        task_id: Some(key.into()),
        title: title.into(),
        ask: Some(Ask { request_id: format!("demo-{kind}"), kind: kind.into(), question: question.into() }),
        ..row(key, minutes, Opens::Chat)
    };
    if demo == "tray-asks" {
        vec![
            ask("permission", "Tidy the tests", "approval", "Run `npm install` in filo-desktop", 2.),
            ask(
                "question",
                "Coding row: tool icon under the title, not beside it, on narrow windows",
                "question",
                "The tool's icon could sit under the title or beside it; which reads better?",
                7.,
            ),
            filo,
            done,
        ]
    } else {
        vec![filo, group, done]
    }
}

#[cfg(feature = "dev")]
fn demo_working() -> Vec<Working> {
    if !std::env::var("FILO_PET_DEMO").is_ok_and(|demo| demo == "tray") {
        return Vec::new();
    }
    let now = format::now_ms();
    [
        ("demo-1", "Fix the sign-in redirect", 4.),
        ("demo-2", "Plan the trip to Lisbon", 12.),
        ("demo-3", "Weekly review of the inbox", 0.5),
        ("demo-4", "Claude Code: tidy the tests", 2.),
    ]
    .into_iter()
    .map(|(id, title, minutes)| Working {
        session_id: id.into(),
        title: title.into(),
        since: Some(now - minutes * 60_000.),
        opens: Opens::Chat,
    })
    .collect()
}

pub fn short(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

// ---------------------------------------------------------------------------------------------------------------
// The menu

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuChat {
    pub session_id: String,
    pub label: String,
}

/// What the menu bar's menu lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuState {
    /// The companion is up (it starts with the window): the pet and shortcut items have something to act on.
    pub ready: bool,
    pub status: Option<String>,
    pub unseen_total: u32,
    pub replies: Vec<MenuChat>,
    pub upcoming: Vec<String>,
    pub pet: bool,
    /// Shortcut id → keystroke, for the items they open.
    pub shortcuts: BTreeMap<String, String>,
    /// While Filo uses the user's apps: which (none named when unsaid). The menu bar says so and offers Stop.
    pub using_apps: Option<Vec<String>>,
    /// While coding tasks and routines keep the Mac awake: how many. The menu says so and offers Let Mac Sleep.
    pub keeping_awake: Option<usize>,
}

fn menu_state(companion: Option<&Companion>, cx: &App) -> MenuState {
    let settings = settings(cx);
    let shortcuts = SHORTCUTS
        .iter()
        .filter_map(|shortcut| Some((shortcut.id.to_string(), settings.shortcut(shortcut.id)?)))
        .collect();
    let (using_apps, keeping_awake) = (using_apps(cx), keeping_awake(cx));
    let Some(companion) = companion else {
        return MenuState { pet: settings.pet, shortcuts, using_apps, keeping_awake, ..Default::default() };
    };
    let now = format::now_ms();
    let replies = companion
        .unseen
        .iter()
        .take(MENU_CHATS)
        .map(|chat| {
            let title = if chat.title.trim().is_empty() { companion.agent_name.as_str() } else { chat.title.as_str() };
            let label = if chat.preview.is_empty() {
                short(title, 48)
            } else {
                format!("{} — {}", short(title, 28), short(&chat.preview, 40))
            };
            MenuChat { session_id: chat.session_id.clone(), label }
        })
        .collect();
    MenuState {
        ready: true,
        status: companion.status(),
        unseen_total: companion.unseen_total(),
        replies,
        upcoming: companion
            .upcoming(now)
            .into_iter()
            .map(|(title, when)| format!("{} · {when}", short(&title, 36)))
            .collect(),
        pet: settings.pet,
        shortcuts,
        using_apps,
        keeping_awake,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_from_other_apps_goes_to_the_main_agent_until_picked() {
        // Settings saved before there was a choice.
        let old: Settings = serde_json::from_value(json!({ "menuBar": true, "pet": false })).unwrap();
        assert_eq!(old.voice_to, VoiceTo::Main);
        for choice in [VoiceTo::Main, VoiceTo::OpenChat, VoiceTo::Agent("milo".into())] {
            let saved = serde_json::to_value(Settings { voice_to: choice.clone(), ..Settings::default() }).unwrap();
            assert_eq!(serde_json::from_value::<Settings>(saved).unwrap().voice_to, choice);
        }
    }

    #[test]
    fn keeping_awake_while_tasks_run_and_the_setting_is_on() {
        let keeping = |value| serde_json::from_value::<KeepingAwakeStatus>(value).unwrap().keeping();
        assert_eq!(
            keeping(json!({ "keepAwake": true, "running": 2, "work": ["task:a", "turn:b"], "keepAwakeLid": true })),
            Some(2)
        );
        assert_eq!(keeping(json!({ "keepAwake": true, "running": 0 })), None);
        assert_eq!(keeping(json!({ "keepAwake": false, "running": 3 })), None);
        assert_eq!(keeping_awake_label(1), "Keeping your Mac awake · 1 task running");
        assert_eq!(keeping_awake_label(4), "Keeping your Mac awake · 4 tasks running");
    }

    #[test]
    fn using_apps_while_the_helper_may_act() {
        let status = |value| serde_json::from_value::<ComputerStatus>(value).unwrap().using_apps();
        assert_eq!(
            status(json!({ "appsOn": true, "using": ["Notes", "Safari"], "stoppedUntil": null })),
            Some(vec!["Notes".into(), "Safari".into()])
        );
        assert_eq!(status(json!({ "appsOn": true, "using": [], "stoppedUntil": null })), Some(vec![]));
        assert_eq!(status(json!({ "appsOn": true, "using": null, "stoppedUntil": null })), None);
        // Stopped: the helper is closing. Off: its calls are refused.
        assert_eq!(status(json!({ "appsOn": true, "using": ["Notes"], "stoppedUntil": 1.0e12 })), None);
        assert_eq!(status(json!({ "appsOn": false, "using": ["Notes"], "stoppedUntil": null })), None);
    }

    #[test]
    fn apps_label_stays_short() {
        let label = |apps: &[&str]| apps_label(&apps.iter().map(|app| app.to_string()).collect::<Vec<_>>());
        assert_eq!(label(&[]), None);
        assert_eq!(label(&["Safari"]).as_deref(), Some("Safari"));
        assert_eq!(label(&["Safari", "Notes"]).as_deref(), Some("Safari, Notes"));
        assert_eq!(label(&["Safari", "Notes", "Mail", "Xcode"]).as_deref(), Some("Safari, Notes +2"));
        assert_eq!(label(&["Microsoft Remote Desktop Beta"]).as_deref(), Some("Microsoft Remote De…"));
    }

    #[test]
    fn mood_puts_the_user_first() {
        let quiet = Signals::default();
        assert_eq!(pick_mood(quiet), Mood::Idle);
        assert_eq!(pick_mood(Signals { night: true, ..quiet }), Mood::Night);
        assert_eq!(pick_mood(Signals { working: true, night: true, ..quiet }), Mood::Working);
        assert_eq!(pick_mood(Signals { replying: true, working: true, ..quiet }), Mood::Replying);
        assert_eq!(pick_mood(Signals { celebrating: true, replying: true, ..quiet }), Mood::Celebrating);
        assert_eq!(pick_mood(Signals { due: true, celebrating: true, ..quiet }), Mood::Due);
        assert_eq!(pick_mood(Signals { listening: true, due: true, ..quiet }), Mood::Listening);
    }

    #[test]
    fn mood_avatar_follows_the_phone() {
        assert_eq!(mood_avatar("fox-prop", Mood::Idle, Glance::Ahead), "fox-prop");
        assert_eq!(mood_avatar("fox-prop", Mood::Replying, Glance::Ahead), "fox-laptop");
        assert_eq!(mood_avatar("panda-idea", Mood::Replying, Glance::Ahead), "panda-thinking");
        assert_eq!(mood_avatar("panda-idea", Mood::Working, Glance::Ahead), "panda-running");
        assert_eq!(mood_avatar("cat-idle", Mood::Due, Glance::Ahead), "cat-waving");
        assert_eq!(mood_avatar("cat-idle", Mood::Celebrating, Glance::Ahead), "cat-heart");
        assert_eq!(mood_avatar("owl-idle", Mood::Night, Glance::Ahead), "owl-sleeping");
        assert_eq!(mood_avatar("owl-idle", Mood::Idle, Glance::Left), "owl-left");
        // The panda has no left and right poses.
        assert_eq!(mood_avatar("panda-waving", Mood::Idle, Glance::Right), "panda-waving");
        // Every pose it picks exists.
        for animal in crate::avatars::ANIMALS.iter() {
            for mood in
                [Mood::Idle, Mood::Night, Mood::Working, Mood::Replying, Mood::Celebrating, Mood::Due, Mood::Listening]
            {
                for glance in [Glance::Ahead, Glance::Left, Glance::Right] {
                    let id = mood_avatar(&animal.first_avatar(), mood, glance);
                    assert!(crate::avatars::is_avatar_id(&id), "{id}");
                }
            }
        }
    }

    #[test]
    fn the_pet_is_the_picked_character_else_the_main_agent() {
        let pose = |url: &str| avatars::CustomPose { url: url.into() };
        let astro = CustomAvatar {
            id: "astro".into(),
            name: "Astro cat".into(),
            poses: avatars::CustomPoses {
                idle: pose("idle.png"),
                working: pose("working.png"),
                waving: pose("waving.png"),
                sleeping: pose("sleeping.png"),
            },
        };
        let custom = [astro.clone()];
        // The main agent's picture as a character: an animal in its pose, one of the user's own avatars, or none.
        let own = Look::of_picture("fox-prop", &custom).unwrap();
        assert_eq!(own, Look::Animal("fox-prop".into(), "blue".into()));
        assert_eq!(Look::of_picture("custom:astro:sleeping", &custom), Some(Look::Custom(astro.clone())));
        assert_eq!(Look::of_picture("helper:sunny", &custom), None);
        // Nothing picked: the main agent's.
        assert_eq!(pet_look(None, &own, &custom), own);
        // Another animal rests as drawn; the main agent's own animal keeps its pose.
        assert_eq!(pet_look(Some("owl"), &own, &custom), Look::Animal("owl-idle".into(), "blue".into()));
        assert_eq!(pet_look(Some("panda"), &own, &custom), Look::Animal("panda-waving".into(), "blue".into()));
        assert_eq!(pet_look(Some("fox"), &own, &custom), own);
        // A custom pet, and one that's gone.
        assert_eq!(pet_look(Some("custom:astro"), &own, &custom), Look::Custom(astro.clone()));
        assert_eq!(pet_look(Some("custom:gone"), &own, &custom), own);
        assert_eq!(pet_look(Some("unicorn"), &own, &custom), own);
        // Kept and named as the page shows it.
        assert_eq!(pet_character(&own), "fox");
        assert_eq!(pet_character(&Look::Custom(astro.clone())), "custom:astro");
        assert_eq!(pet_name(&own), "Fox");
        assert_eq!(pet_name(&Look::Custom(astro)), "Astro cat");
        // An older settings file, without the pick, still loads.
        let settings: Settings = serde_json::from_str(r#"{"pet":false}"#).unwrap();
        assert_eq!((settings.pet, settings.pet_character), (false, None));
    }

    fn served(id: &str, title: &str, started: Option<f64>, agent_id: Option<&str>, agent_group: bool) -> WorkingRow {
        WorkingRow {
            id: id.into(),
            title: title.into(),
            work_started_at: started,
            agent_id: agent_id.map(str::to_string),
            agent_group,
        }
    }

    fn chat(id: &str, title: &str, since: Option<f64>) -> Working {
        Working { session_id: id.into(), title: title.into(), since, opens: Opens::Chat }
    }

    #[test]
    fn working_chats_follow_the_chat_list() {
        let rows = [
            served("a", "Trip", Some(500.), None, false),
            served("b", "Idle chat", None, None, false),
            served("c", "", None, None, false),
            served("d", "", Some(50.), None, false),
        ];
        let coding = HashMap::from([("a".to_string(), 300.), ("c".to_string(), 700.)]);
        assert_eq!(
            working_chats(Some(&rows), &coding, "Filo"),
            vec![
                // Only a coding task: its start; an untitled chat is Filo's.
                chat("c", "Filo", Some(700.)),
                // Filo's work and a coding task: since the earlier one.
                chat("a", "Trip", Some(300.)),
                chat("d", "Filo", Some(50.)),
            ]
        );
        assert!(working_chats(Some(&rows[1..2]), &HashMap::new(), "Filo").is_empty());
        // Before `voiceSessions:working` answers, the coding tasks' chats alone.
        assert_eq!(
            working_chats(None, &coding, "Filo"),
            vec![chat("c", "Filo", Some(700.)), chat("a", "Filo", Some(300.))]
        );
    }

    #[test]
    fn working_chats_reach_past_the_recent_ones() {
        // 25 chats working, an agent's, the agents' group and a chat of Filo's own among them; one only named for its
        // coding task.
        let mut rows: Vec<WorkingRow> = (0..22)
            .map(|index| served(&format!("old-{index}"), &format!("Old {index}"), Some(f64::from(index)), None, false))
            .collect();
        rows.push(served("agent-chat", "Gym Coach", Some(100.), Some("coach"), false));
        rows.push(served("group", "Group", Some(200.), None, true));
        rows.push(served("archived", "Archived trip", Some(50.), None, false));
        rows.push(served("coded", "Tidy the tests", None, None, false));
        rows.push(served("was-coded", "Finished task", None, None, false));
        let coding = HashMap::from([("coded".to_string(), 150.)]);
        let working = working_chats(Some(&rows), &coding, "Filo");
        assert_eq!(working.len(), 26);
        assert_eq!(
            &working[..3],
            &[
                Working {
                    session_id: "group".into(),
                    title: "Group".into(),
                    since: Some(200.),
                    opens: Opens::Agent(crate::screens::agents::GROUP.into())
                },
                chat("coded", "Tidy the tests", Some(150.)),
                Working {
                    session_id: "agent-chat".into(),
                    title: "Gym Coach".into(),
                    since: Some(100.),
                    opens: Opens::Agent("coach".into())
                },
            ]
        );
        assert!(working.iter().any(|row| row.session_id == "archived" && row.opens == Opens::Chat));
        assert!(working.iter().any(|row| row.session_id == "old-0"));
        // Named once for a coding task that has since finished: not working.
        assert!(!working.iter().any(|row| row.session_id == "was-coded"));
        assert!(working_chats(Some(&[]), &HashMap::new(), "Filo").is_empty());
    }

    #[test]
    fn an_agents_coding_tasks_count_wherever_their_chat_is() {
        let chat = |session: &str, since: f64, agents: &[&str]| WorkingChat {
            session_id: session.into(),
            since,
            agent_ids: agents.iter().map(|id| id.to_string()).collect(),
        };
        let agents = coding_agents(&[
            chat("group", 300., &["dev", "vibe"]),
            chat("dev-chat", 100., &["dev"]),
            chat("filo-chat", 50., &[]),
        ]);
        assert_eq!(agents, HashMap::from([("dev".to_string(), 100.), ("vibe".to_string(), 300.)]));
    }

    fn task(id: &str, session: &str, agent: Option<&str>, since: f64) -> TrayTask {
        TrayTask {
            task_id: id.into(),
            session_id: session.into(),
            agent_id: agent.map(str::to_string),
            group: false,
            title: format!("Task {id}"),
            status: "running".into(),
            since: Some(since),
            ask: None,
            finished_at: None,
            summary: None,
            round: None,
        }
    }

    fn tray_row(rows: &[TrayRow], key: &str) -> TrayRow {
        rows.iter().find(|row| row.key == key).unwrap_or_else(|| panic!("no row {key}")).clone()
    }

    fn keys(rows: &[TrayRow]) -> Vec<&str> {
        rows.iter().map(|row| row.key.as_str()).collect()
    }

    #[test]
    fn a_tray_row_opens_its_agent_or_chat() {
        let none = HashMap::new();
        let rows = tray_rows(&[chat("s1", "Trip", None)], &[], &none, &HashSet::new(), 0.);
        assert_eq!((rows[0].key.as_str(), rows[0].title.as_str()), ("chat:s1", "Trip"));
        assert_eq!(rows[0].command(), Command::OpenChat("s1".into()));
        let agent = Working { opens: Opens::Agent("coach".into()), ..chat("s2", "Gym Coach", None) };
        let group = Working { opens: Opens::Agent(crate::screens::agents::GROUP.into()), ..chat("s3", "Group", None) };
        let rows = tray_rows(&[agent, group], &[], &none, &HashSet::new(), 0.);
        let coach = tray_row(&rows, "agent:coach");
        assert_eq!(
            (coach.command(), coach.agent_id.as_deref(), coach.title.as_str()),
            (Command::OpenAgent("coach".into()), Some("coach"), "")
        );
        assert_eq!(tray_row(&rows, "group").command(), Command::OpenAgent(crate::screens::agents::GROUP.into()));
        assert_eq!(tray_row(&rows, "group").agent_id, None);
    }

    #[test]
    fn an_agents_reply_and_tasks_are_one_row() {
        // The real app's case: App Designer replies in its chat while its coding task works there.
        let designer =
            [Working { opens: Opens::Agent("designer".into()), ..chat("designer-chat", "App Designer", Some(100.)) }];
        let tasks =
            [TrayTask { title: "Floating chat header".into(), ..task("t1", "designer-chat", Some("designer"), 100.) }];
        let reply = HashMap::from([("designer-chat".to_string(), Some(160.))]);
        let rows = tray_rows(&designer, &tasks, &reply, &HashSet::new(), 0.);
        assert_eq!(keys(&rows), ["agent:designer"]);
        assert_eq!(
            (rows[0].tasks, rows[0].since, rows[0].command()),
            (1, Some(100.), Command::OpenAgent("designer".into()))
        );
        // Tens of tasks, some started from the group: still one row, counting them.
        let many: Vec<TrayTask> = (0..30)
            .map(|index| TrayTask {
                group: index % 3 == 0,
                ..task(&format!("t{index}"), "designer-chat", Some("designer"), 100. + f64::from(index))
            })
            .collect();
        let rows = tray_rows(&designer, &many, &reply, &HashSet::new(), 0.);
        assert_eq!((keys(&rows), rows[0].tasks), (vec!["agent:designer"], 30));
        // Only the reply: the row, with no tasks.
        let rows = tray_rows(&designer, &[], &reply, &HashSet::new(), 0.);
        assert_eq!((keys(&rows), rows[0].tasks, rows[0].since), (vec!["agent:designer"], 0, Some(160.)));
        // The chat named only for a task that waits on the user: just the waiting row.
        let waiting = [TrayTask {
            ask: Some(Ask { request_id: "r".into(), kind: "approval".into(), question: "Run it?".into() }),
            ..task("t1", "designer-chat", Some("designer"), 100.)
        }];
        let rows = tray_rows(&designer, &waiting, &HashMap::new(), &HashSet::new(), 0.);
        assert_eq!(keys(&rows), ["t1"]);
    }

    #[test]
    fn the_tray_lists_waiting_tasks_then_who_works_then_what_finished() {
        let ask = Ask { request_id: "r1".into(), kind: "approval".into(), question: "Run `npm install`?".into() };
        let now = 1_000_000.;
        // An agent with two tasks in its chat, one waiting on the user; Filo's task in the group; Filo replying in a
        // chat of its own; two of Filo's tasks just done in another chat, one done long ago; one of the agent's just done.
        let tasks = vec![
            task("a", "dev-chat", Some("dev"), 100.),
            TrayTask { ask: Some(ask.clone()), title: "Badge".into(), ..task("b", "dev-chat", Some("dev"), 50.) },
            TrayTask { group: true, ..task("c", "group-chat", None, 300.) },
            TrayTask {
                status: "done".into(),
                since: None,
                finished_at: Some(now - 1_000.),
                summary: Some("Centred it".into()),
                round: Some(2.),
                ..task("d", "filo-chat", None, 0.)
            },
            TrayTask {
                status: "done".into(),
                since: None,
                finished_at: Some(now - 2_000.),
                ..task("d2", "filo-chat", None, 0.)
            },
            TrayTask {
                status: "failed".into(),
                finished_at: Some(now - FINISHED_LINGER_MS - 1.),
                ..task("e", "filo-chat", None, 0.)
            },
            TrayTask {
                status: "done".into(),
                since: None,
                finished_at: Some(now - 500.),
                ..task("f", "dev-chat", Some("dev"), 0.)
            },
        ];
        let working = vec![
            Working { opens: Opens::Agent("dev".into()), ..chat("dev-chat", "Dev", Some(50.)) },
            chat("trip", "Trip", Some(200.)),
        ];
        let trip = HashMap::from([("trip".to_string(), Some(200.))]);
        let rows = tray_rows(&working, &tasks, &trip, &HashSet::new(), now);
        assert_eq!(keys(&rows), ["b", "group", "chat:trip", "agent:dev", "done:agent:dev", "done:chat:filo-chat"]);
        let waiting = &rows[0];
        assert_eq!(
            (waiting.ask.as_ref(), waiting.title.as_str(), waiting.agent_id.as_deref()),
            (Some(&ask), "Badge", Some("dev"))
        );
        assert_eq!(
            (waiting.command(), waiting.open_chat()),
            (Command::OpenAgent("dev".into()), Command::OpenAgent("dev".into()))
        );
        assert_eq!((rows[1].tasks, rows[2].tasks, rows[2].title.as_str(), rows[3].tasks), (1, 0, "Trip", 1));
        // One finished: its result opens. Two: their chat, and both go once it's opened.
        let one = &rows[4];
        assert_eq!(
            one.finished,
            Some(Finished { at: now - 500., count: 1, status: "done".into(), summary: "Task f".into(), round: None })
        );
        assert_eq!(
            (one.command(), one.dismiss.clone()),
            (Command::OpenCodingResult("f".into(), None), vec!["f".to_string()])
        );
        let two = &rows[5];
        assert_eq!(
            two.finished,
            Some(Finished { at: now - 1_000., count: 2, status: "done".into(), summary: String::new(), round: None })
        );
        assert_eq!(
            (two.command(), two.dismiss.clone()),
            (Command::OpenChat("filo-chat".into()), vec!["d".to_string(), "d2".to_string()])
        );
        // Answered from the tray: it no longer waits, and folds into its agent's row.
        let rows = tray_rows(&working, &tasks, &trip, &HashSet::from(["r1".to_string()]), now);
        assert!(rows.iter().all(|row| row.ask.is_none()));
        assert_eq!(tray_row(&rows, "agent:dev").tasks, 2);
        // With no coding tasks: a row a working chat.
        let rows = tray_rows(&working, &[], &trip, &HashSet::new(), now);
        assert_eq!(keys(&rows), ["chat:trip", "agent:dev"]);
    }

    #[test]
    fn the_status_says_who_works() {
        let names =
            |row: &TrayRow| if row.agent_id.is_some() { "App Designer".to_string() } else { "Filo".to_string() };
        let designer =
            [Working { opens: Opens::Agent("designer".into()), ..chat("designer-chat", "App Designer", Some(1.)) }];
        let rows = tray_rows(
            &designer,
            &[task("a", "designer-chat", Some("designer"), 1.)],
            &HashMap::new(),
            &HashSet::new(),
            0.,
        );
        assert_eq!(tray_status(&rows, names).as_deref(), Some("App Designer is working…"));
        let three: Vec<TrayTask> =
            ["a", "b", "c"].iter().map(|id| task(id, "designer-chat", Some("designer"), 1.)).collect();
        let rows = tray_rows(&designer, &three, &HashMap::new(), &HashSet::new(), 0.);
        assert_eq!(tray_status(&rows, names).as_deref(), Some("App Designer is working on 3 tasks"));
        let trip = [chat("trip", "Trip to Lisbon", Some(1.))];
        let rows = tray_rows(&trip, &three, &HashMap::from([("trip".to_string(), None)]), &HashSet::new(), 0.);
        assert_eq!(tray_status(&rows, names).as_deref(), Some("2 at work"));
        let ask = Ask { request_id: "r".into(), kind: "question".into(), question: "Which?".into() };
        let tasks =
            [TrayTask { ask: Some(ask), title: "Badge".into(), ..task("b", "designer-chat", Some("designer"), 1.) }];
        let rows = tray_rows(&trip, &tasks, &HashMap::new(), &HashSet::new(), 0.);
        assert_eq!(tray_status(&rows, names).as_deref(), Some("App Designer needs you on “Badge”"));
        assert_eq!(tray_status(&[], names), None);
    }

    #[test]
    fn tray_tasks_read_as_the_backend_sends_them() {
        let working: TrayTask = serde_json::from_value(json!({
            "taskId": "t1", "sessionId": "s1", "agentId": null, "group": false, "title": "Fix it", "tool": "Claude Code", "status": "running",
            "since": 5.0, "ask": { "requestId": "r1", "kind": "question", "question": "Which?", "options": ["A", "B"] },
        }))
        .unwrap();
        assert_eq!((working.since, working.finished_at), (Some(5.), None));
        assert!(!working.ask.unwrap().is_permission());
        let finished: TrayTask = serde_json::from_value(json!({
            "taskId": "t2", "sessionId": "s1", "agentId": "dev", "group": true, "title": "Fix it", "tool": "Codex", "status": "done",
            "finishedAt": 9.0, "summary": "Fixed", "round": 3.0,
        }))
        .unwrap();
        assert_eq!(
            (finished.finished_at, finished.summary.as_deref(), finished.round, finished.group),
            (Some(9.), Some("Fixed"), Some(3.), true)
        );
    }

    #[test]
    fn night_is_late_evening_to_early_morning() {
        assert!(is_night(23) && is_night(0) && is_night(5));
        assert!(!is_night(6) && !is_night(12) && !is_night(22));
    }

    #[test]
    fn shortcuts_default_and_turn_off() {
        let mut settings = Settings::default();
        assert_eq!(settings.shortcut(OPEN).as_deref(), Some("ctrl-alt"));
        assert_eq!(settings.shortcut(SCREEN).as_deref(), Some("alt-cmd"));
        settings.shortcuts.insert(OPEN.into(), "ctrl-alt-f".into());
        settings.shortcuts.insert(PET.into(), String::new());
        assert_eq!(settings.shortcut(OPEN).as_deref(), Some("ctrl-alt-f"));
        assert_eq!(settings.shortcut(PET), None);
    }
}

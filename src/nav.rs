// Navigation between tabs and pushed pages. The shell keeps one view per tab alive (like the phone app's pre-mounted
// tabs) and a stack of pushed pages on top of the current tab.

use gpui::{App, Global, WeakEntity, Window};

use crate::app::AppShell;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Feed,
    Notes,
    Artifacts,
    Connectors,
    Agents,
    Coding,
    Settings,
}

/// The sidebar's tabs, top to bottom (Settings at its foot), when every page is on: ⌘1 – ⌘7 go to them in this order.
const SIDEBAR: [Tab; 7] =
    [Tab::Agents, Tab::Feed, Tab::Notes, Tab::Artifacts, Tab::Coding, Tab::Connectors, Tab::Settings];

/// The sidebar's tabs as it shows them: less Feed or Notes while it's off (Settings › Pages, pages.rs).
pub fn sidebar(cx: &App) -> Vec<Tab> {
    SIDEBAR.into_iter().filter(|tab| tab.shown(cx)).collect()
}

impl Tab {
    /// The optional page this tab is (Feed, Notes).
    pub fn page(self) -> Option<crate::pages::Page> {
        match self {
            Tab::Feed => Some(crate::pages::Page::Feed),
            Tab::Notes => Some(crate::pages::Page::Notes),
            _ => None,
        }
    }

    /// Whether it's in the sidebar and opens: not while its page is off.
    pub fn shown(self, cx: &App) -> bool {
        self.page().is_none_or(|page| crate::pages::get(cx).on(page))
    }

    #[cfg(feature = "dev")]
    pub const ALL: [Tab; 7] =
        [Tab::Feed, Tab::Notes, Tab::Artifacts, Tab::Connectors, Tab::Agents, Tab::Coding, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Feed => "Feed",
            Tab::Notes => "Notes",
            Tab::Artifacts => "Artifacts",
            // Called Extensions (once Abilities): apps and skills (screens/connectors.rs). The code keeps "connectors".
            Tab::Connectors => "Extensions",
            Tab::Agents => "Agents",
            Tab::Coding => "Coding",
            Tab::Settings => "Settings",
        }
    }

    /// The tinted glyph the tab is drawn with; None for Agents, drawn with a full-colour picture instead
    /// (assets/images/tab-agents.png).
    pub fn icon(self) -> Option<&'static str> {
        match self {
            Tab::Feed => Some("newspaper"),
            Tab::Notes => Some("notebook-pen"),
            // The app's own tab glyph (assets/images/tabIcons), wrapped in SVG.
            Tab::Artifacts => Some("tab-artifacts"),
            Tab::Connectors => Some("puzzle"),
            Tab::Agents => None,
            Tab::Coding => Some("square-terminal"),
            Tab::Settings => Some("settings"),
        }
    }
}

impl Route {
    /// Whether it opens: a note's or a post's page doesn't while Notes or Feed is off (pages.rs).
    pub fn shown(&self, cx: &App) -> bool {
        let tab = match self {
            Route::Note(_) => Tab::Notes,
            Route::FeedPost(_) | Route::FeedSettings => Tab::Feed,
            _ => return true,
        };
        tab.shown(cx)
    }
}

/// Pushed pages (the app's stack routes).
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    /// `/note` (new) or `/note?id=`
    Note(Option<String>),
    /// `/reminders`, titled "Scheduled"
    Scheduled,
    Routine(String),
    Memories,
    Artifact(String),
    /// The Artifacts tab's settings: Share to claude.ai.
    ArtifactSettings,
    FeedPost(String),
    FeedSettings,
    Appearance,
    Name,
    /// Settings › App icon: the Dock icon (app_icon.rs).
    AppIcon,
    /// Settings › Desktop pet: the pet's character, shown or hidden (desktop_pet.rs).
    DesktopPet,
    /// Settings › Browser: WebKit or Chromium as the browser's engine (browser_settings.rs).
    BrowserSettings,
    /// Keyboard shortcuts, the menu bar item and the pet (desktop only).
    Shortcuts,
    /// A coding task's page.
    CodingTask(String),
    /// A coding round's result (the chat's Open proof): the task, and the round (the latest without one).
    CodingResult(String, Option<u32>),
    /// An agent's full report under its post in the group, on a page like a round's result.
    PostReport(crate::screens::coding_result::PostReport),
    /// Report a problem: from Settings › Help, a chat's failed reply or a failed coding task (report.rs).
    Report(crate::report::Origin),
}

pub struct NavGlobal(pub WeakEntity<AppShell>);

impl Global for NavGlobal {}

fn with_shell(
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut AppShell, &mut Window, &mut gpui::Context<AppShell>) + 'static,
) {
    let Some(shell) = cx.try_global::<NavGlobal>().map(|nav| nav.0.clone()) else { return };
    // Deferred so a screen can navigate from inside its own update.
    window.defer(cx, move |window, cx| {
        shell.update(cx, |shell, cx| f(shell, window, cx)).ok();
    });
}

pub fn push(route: Route, window: &mut Window, cx: &mut App) {
    with_shell(window, cx, move |shell, window, cx| shell.push(route, window, cx));
}

/// Starts one bounded speculative preview without navigating or marking the artifact opened.
pub(crate) fn prepare_artifact(id: String, window: &mut Window, cx: &mut App) {
    with_shell(window, cx, move |shell, window, cx| shell.prepare_artifact(id, window, cx));
}

/// ⌘P's search box, opened or closed (screens/search.rs).
pub fn toggle_search(window: &mut Window, cx: &mut App) {
    with_shell(window, cx, |shell, window, cx| shell.toggle_search(window, cx));
}

/// Back out of a pushed page.
pub fn back(window: &mut Window, cx: &mut App) {
    with_shell(window, cx, |shell, window, cx| shell.back(window, cx));
}

/// Undoes `back`.
pub fn forward(window: &mut Window, cx: &mut App) {
    with_shell(window, cx, |shell, window, cx| shell.forward(window, cx));
}

/// Opens `session_id`'s conversation, when it's an agent's (from Feed, Artifacts, a banner…).
pub fn open_chat(session_id: String, window: &mut Window, cx: &mut App) {
    with_shell(window, cx, move |shell, window, cx| shell.open_chat(session_id, window, cx));
}

/// Opens agent `agent_id`'s conversation on the Agents tab.
pub fn open_agent(agent_id: String, window: &mut Window, cx: &mut App) {
    with_shell(window, cx, move |shell, window, cx| shell.open_agent(agent_id, window, cx));
}

/// Shares a note into the conversation of agent `agent` (the Notes tab's right-click menu).
pub fn share_note(
    agent: String,
    note: crate::screens::chat::note_share::SharedNote,
    window: &mut Window,
    cx: &mut App,
) {
    with_shell(window, cx, move |shell, window, cx| shell.share_note(agent, note, window, cx));
}

/// The agents a note can be shared to: what the Agents tab already has, so the menu asks the backend nothing.
pub fn share_targets(cx: &App) -> Vec<crate::screens::chat::note_share::AgentTarget> {
    let shell = cx.try_global::<NavGlobal>().and_then(|nav| nav.0.upgrade());
    shell
        .and_then(|shell| shell.read(cx).agents_screen())
        .map(|agents| agents.read(cx).share_targets())
        .unwrap_or_default()
}

/// Feed or Notes turned on or off (pages.rs): the shell leaves a page that's gone.
pub fn pages_changed(window: &mut Window, cx: &mut App) {
    with_shell(window, cx, |shell, window, cx| shell.pages_changed(window, cx));
}

/// Switches to `tab` (a screen pointing somewhere else: the Agents tab's way to Coding).
pub fn open_tab(tab: Tab, window: &mut Window, cx: &mut App) {
    with_shell(window, cx, move |shell, window, cx| shell.open_tab(tab, window, cx));
}

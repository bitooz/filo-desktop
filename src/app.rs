// The window's root: the welcome tour the first time, then the tab sidebar with one live view per tab and a stack of
// pushed pages.

use std::collections::HashMap;

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    Modifiers, ModifiersChangedEvent, MouseButton, ParentElement, Render, ScrollDelta, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled, Task, TouchPhase, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::Root;
use serde_json::json;

use crate::backend::{self, backend};
use crate::nav::{NavGlobal, Route, Tab};
use crate::screens;
use crate::theme::{self, ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// Wide enough that the traffic lights (about 60 px on recent macOS) sit inside
/// the sidebar card with room on both sides; see `traffic_light_position` in main.rs.
pub const SIDEBAR_WIDTH: f32 = 96.;

/// How long a tab other than Agents stays loaded once hidden.
const DROP_HIDDEN_TAB_AFTER: std::time::Duration = std::time::Duration::from_secs(120);

pub struct AppShell {
    /// The welcome tour, over everything else while it shows (screens/welcome.rs).
    welcome: Option<Entity<screens::welcome::WelcomeTour>>,
    tab: Tab,
    tabs: HashMap<Tab, AnyView>,
    /// Timers that close a tab's view once it has been hidden a while (`DROP_HIDDEN_TAB_AFTER`).
    tab_drops: HashMap<Tab, Task<()>>,
    agents: Option<Entity<screens::agents::AgentsScreen>>,
    stack: Vec<(Route, AnyView)>,
    /// Pages left with Back, for Forward; cleared by any other navigation.
    forward: Vec<(Route, AnyView)>,
    /// A few recently opened artifacts keep their WKWebView and scroll position while Artifacts is open and Filo in
    /// view. Bounded because native web views each own a WebKit process/data store.
    artifact_pages: std::collections::VecDeque<(String, Entity<screens::artifact::ArtifactScreen>)>,
    /// A single speculative preview, separate from the three opened-page caches.
    artifact_preload: Option<(String, Entity<screens::artifact::ArtifactScreen>)>,
    /// Some of the window showed on screen at the last check.
    window_in_view: bool,
    /// Horizontal travel of the current two-finger trackpad gesture, while it can still go back.
    swipe: Option<f32>,
    /// Holds key focus when nothing else does, so fn+⌃ always reaches `on_modifiers`.
    focus: FocusHandle,
    /// The sidebar's app icon, which Tab reaches (it opens App icon).
    logo_focus: FocusHandle,
    /// fn+⌃ is down.
    voice_chord: bool,
    /// Starts recording once fn+⌃ has stayed down a moment.
    _voice_hold: Option<Task<()>>,
    /// The open shortcut held (`talk`): recording in the open chat, or from another app for the chat picked after.
    talking: Option<Talking>,
    /// Redraws the Agents tab's dot when unseen counts change.
    _unseen_sub: Option<gpui::Subscription>,
    _reply_notices: Option<gpui::Subscription>,
    /// The update button above Settings, in builds that have an update feed (updater.rs).
    updates: Option<Entity<crate::updater::Updater>>,
    /// ⌘P's search box, while it's open over the window (screens/search.rs).
    search: Option<SearchOpen>,
    _tasks: Vec<Task<()>>,
}

/// The open search box, what it asks watched, and what had the keyboard before it, to have it back when it closes.
struct SearchOpen {
    panel: Entity<screens::search::SearchPanel>,
    _events: gpui::Subscription,
    focus_before: Option<FocusHandle>,
}

/// Where the open shortcut's recording is going.
enum Talking {
    /// Held in Filo's window: the open chat records it, as its own fn+⌃ does.
    InChat(Entity<screens::chat::conversation::Conversation>),
    /// Held from another app: recorded here, then sent where Settings says or the message names (voice_route.rs),
    /// with the screenshot taken for it and the chat that was open in Filo's window.
    Away {
        recording: screens::chat::conversation::AwayRecording,
        screenshot: Option<crate::screenshot::Screenshot>,
        open_chat: Option<String>,
    },
}

/// How long fn+⌃ stays down before recording starts, so brushing the keys doesn't.
const VOICE_HOLD_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// How far two fingers travel sideways before going back (right) or forward (left), like Safari's swipe.
const SWIPE_DISTANCE: f32 = 80.;

/// How often the shell checks whether the user is looking at the open chat.
const ATTENTION_CHECK: std::time::Duration = std::time::Duration::from_secs(1);

impl AppShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let shell = cx.entity().downgrade();
        cx.set_global(NavGlobal(shell));
        let appearance = cx.observe_window_appearance(window, |_, window, cx| theme::sync_appearance(window, cx));
        appearance.detach();
        // Leaving the window mid-hold: the key release goes to another app.
        cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            this.check_attention(window, cx);
            if !window.is_window_active() && std::mem::take(&mut this.voice_chord) {
                this._voice_hold = None;
                if let Some(conversation) = this.open_conversation(cx) {
                    conversation.update(cx, |conversation, cx| conversation.pause_held_voice(cx));
                }
            }
        })
        .detach();

        // Whether the user is looking at the open chat, for what counts as seen (presence.rs).
        let attention = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(ATTENTION_CHECK).await;
                if this.update_in(cx, |this, window, cx| this.check_attention(window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut tasks = vec![attention];
        // Nothing is asked at launch: the microphone on the first voice message (voice.rs), notifications the first
        // time Filo has something to show (in_app_notice.rs `ask_for_banners`).
        // `FILO_NOTIFY_TEST=1` (in Filo.app): asks for notifications, logs what macOS allows to /tmp/filo-notify.log and
        // posts a banner (`dev` builds).
        #[cfg(feature = "dev")]
        if std::env::var("FILO_NOTIFY_TEST").is_ok() {
            crate::notifications::request();
            cx.spawn_in(window, async move |_, cx| {
                // After macOS has answered the request above.
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                if let Ok(permission) = crate::notifications::permission().await {
                    let _ = std::fs::write("/tmp/filo-notify.log", format!("{permission:?}\n"));
                }
                let notice = crate::unseen::ReplyNotice {
                    session_id: "test".into(),
                    title: "Tell me more about \"Mi-Memory: A Lifecycle Memory Framework\"".into(),
                    banner: true,
                };
                let body = "Both are set: Page (v2): How Filo Remembers — The Plan — footer now has clickable links to both papers (User as Code, Mi-Memory), nothing else changed. Note: same two links added under Sources.";
                cx.update(|window, cx| {
                    crate::notifications::show_chat(&notice.session_id, &notice.title, Some("2 new messages"), body);
                    crate::in_app_notice::show(&notice, window, cx);
                })
                .ok();
            })
            .detach();
        }
        // A clicked banner opens its chat.
        if let Some((mut clicks, mut needs)) = crate::notifications::start() {
            // A reminder, routine or check-in made or coming due: the first one asks for banners.
            tasks.push(cx.spawn_in(window, async move |_, cx| {
                use futures::StreamExt as _;
                while let Some(need) = needs.next().await {
                    if cx.update(|window, cx| crate::in_app_notice::ask_for_banners(need, window, cx)).is_err() {
                        break;
                    }
                }
            }));
            tasks.push(cx.spawn_in(window, async move |this, cx| {
                use futures::StreamExt as _;
                while let Some(session_id) = clicks.next().await {
                    let opened = this.update_in(cx, |this, window, cx| {
                        cx.activate(true);
                        this.open_chat(session_id, window, cx);
                    });
                    if opened.is_err() {
                        break;
                    }
                }
            }));
        }

        let mut shell = AppShell {
            welcome: screens::welcome::at_launch().then(|| cx.new(|cx| screens::welcome::WelcomeTour::new(window, cx))),
            tab: Tab::Agents,
            tabs: HashMap::new(),
            tab_drops: HashMap::new(),
            agents: None,
            stack: Vec::new(),
            forward: Vec::new(),
            artifact_pages: std::collections::VecDeque::new(),
            artifact_preload: None,
            window_in_view: true,
            swipe: None,
            focus: cx.focus_handle(),
            logo_focus: cx.focus_handle().tab_stop(true),
            voice_chord: false,
            _voice_hold: None,
            talking: None,
            _unseen_sub: None,
            _reply_notices: None,
            updates: crate::updater::feed().map(|feed| cx.new(|cx| crate::updater::Updater::new(feed, window, cx))),
            search: None,
            _tasks: tasks,
        };
        // No account: Filo is this Mac's (backend.rs), so it starts on Agents. The first time, the welcome tour shows
        // over it.
        shell.start(window, cx);
        shell.tab_view(Tab::Agents, window, cx);
        shell
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Test runs (`dev` builds): `FILO_SMOKE` and `FILO_OPEN` (smoke/drives.rs).
        #[cfg(feature = "dev")]
        {
            if std::env::var("FILO_SMOKE").is_ok_and(|mode| mode == "update") {
                let task = crate::smoke::run_update(window, cx);
                self._tasks.push(task);
            } else if std::env::var("FILO_SMOKE").is_ok_and(|mode| mode == "app-icon") {
                let task = crate::smoke::run_app_icon(window, cx);
                self._tasks.push(task);
            } else if std::env::var("FILO_SMOKE").is_ok_and(|mode| mode == "artifacts") {
                let task = crate::smoke::run_artifacts(window, cx);
                self._tasks.push(task);
            } else if std::env::var("FILO_SMOKE").is_ok() {
                let task = crate::smoke::run(window, cx);
                self._tasks.push(task);
            }
            crate::smoke::open_from_env(window, cx);
        }
        let unseen = cx.new(crate::unseen::Unseen::new);
        self._unseen_sub = Some(cx.observe(&unseen, |_, _, cx| cx.notify()));
        // A reply nobody has seen also shows in the window, with a sound (in_app_notice.rs).
        // Not for the chat on screen while the user looks at it.
        self._reply_notices =
            Some(cx.subscribe_in(&unseen, window, |this, _, notice: &crate::unseen::ReplyNotice, window, cx| {
                // The first reply that comes in while Filo is in the background asks for banners (unless the pet says
                // replies instead).
                if !window.is_window_active() && !crate::pet::speaks_replies(cx) {
                    crate::in_app_notice::ask_for_banners(crate::notifications::Need::Reply, window, cx);
                }
                // Not over the welcome tour either.
                if this.welcome.is_none()
                    && !this.showing_chat(&notice.session_id, cx)
                    && !crate::pet::speaks_replies(cx)
                {
                    crate::in_app_notice::show(notice, window, cx);
                }
            }));
        cx.set_global(crate::unseen::UnseenGlobal(unseen));
        // Notes an older Filo kept waiting to be saved go into the database (local_backend/note_carry_over.rs).
        crate::backend::backend(cx).carry_over_old_notes();
        // The menu bar's menu and the pet follow what Filo is doing (companion.rs).
        crate::companion::start(cx);
        // A pet an agent drew becomes the desktop pet (pet_pick.rs).
        crate::pet_pick::start(cx);
        cx.notify();
    }

    /// Shows the welcome tour again (Settings › Welcome tour).
    pub fn show_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.welcome = Some(cx.new(|cx| screens::welcome::WelcomeTour::new(window, cx)));
        cx.notify();
    }

    /// The welcome while it shows (smoke.rs's welcome-drive).
    #[cfg(feature = "dev")]
    pub fn welcome(&self) -> Option<Entity<screens::welcome::WelcomeTour>> {
        self.welcome.clone()
    }

    pub fn close_welcome(&mut self, cx: &mut Context<Self>) {
        self.welcome = None;
        cx.notify();
    }

    fn render_welcome(&mut self) -> gpui::AnyElement {
        let Some(welcome) = self.welcome.clone() else { return div().into_any_element() };
        welcome.into_any_element()
    }

    fn tab_view(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        if let Some(view) = self.tabs.get(&tab) {
            return view.clone();
        }
        let view: AnyView = match tab {
            Tab::Feed => cx.new(|cx| screens::feed::FeedScreen::new(window, cx)).into(),
            Tab::Notes => cx.new(|cx| screens::notes::NotesScreen::new(window, cx)).into(),
            Tab::Artifacts => cx.new(|cx| screens::artifacts::ArtifactsScreen::new(window, cx)).into(),
            Tab::Connectors => cx.new(|cx| screens::connectors::ConnectorsScreen::new(window, cx)).into(),
            Tab::Agents => {
                let agents = cx.new(|cx| screens::agents::AgentsScreen::new(window, cx));
                self.agents = Some(agents.clone());
                agents.into()
            }
            Tab::Coding => cx.new(|cx| screens::coding::CodingScreen::new(window, cx)).into(),
            Tab::Settings => cx.new(|cx| screens::settings::SettingsScreen::new(window, cx)).into(),
        };
        self.tabs.insert(tab, view.clone());
        view
    }

    fn route_view(&mut self, route: &Route, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        use screens::*;
        match route.clone() {
            Route::Note(id) => cx.new(|cx| note::NoteScreen::new(id, window, cx)).into(),
            Route::Scheduled => cx.new(|cx| scheduled::ScheduledScreen::new(window, cx)).into(),
            Route::Routine(id) => cx.new(|cx| routine::RoutineScreen::new(id, window, cx)).into(),
            Route::Memories => cx.new(|cx| memories::MemoriesScreen::new(window, cx)).into(),
            Route::Artifact(id) => {
                let cached = self
                    .artifact_pages
                    .iter()
                    .position(|(key, _)| key == &id)
                    .and_then(|index| self.artifact_pages.remove(index))
                    .map(|(_, page)| page);
                // The preloaded page, moved to this artifact if it was for another: WebKit has its web view going.
                let prepared = || {
                    self.artifact_preload.take().map(|(key, page)| {
                        if key != id {
                            page.update(cx, |page, cx| page.retarget(id.clone(), window, cx));
                        }
                        page
                    })
                };
                let page = cached
                    .or_else(prepared)
                    .unwrap_or_else(|| cx.new(|cx| artifact::ArtifactScreen::new(id.clone(), window, cx)));
                page.update(cx, |page, cx| {
                    page.reopen(cx);
                    page.set_page_visible(true, cx);
                });
                self.artifact_pages.push_front((id, page.clone()));
                self.artifact_pages.truncate(3);
                page.into()
            }
            Route::FeedPost(id) => cx.new(|cx| feed_post::FeedPostScreen::new(id, window, cx)).into(),
            Route::FeedSettings => cx.new(|cx| feed_settings::FeedSettingsScreen::new(window, cx)).into(),
            Route::ArtifactSettings => cx.new(|cx| artifact_settings::ArtifactSettingsScreen::new(window, cx)).into(),
            Route::Appearance => cx.new(|cx| appearance::AppearanceScreen::new(window, cx)).into(),
            Route::Name => cx.new(|cx| name::NameScreen::new(window, cx)).into(),
            Route::AppIcon => cx.new(|cx| screens::app_icon::AppIconScreen::new(window, cx)).into(),
            Route::DesktopPet => cx.new(|cx| screens::desktop_pet::DesktopPetScreen::new(window, cx)).into(),
            Route::Shortcuts => cx.new(|cx| shortcuts::ShortcutsScreen::new(window, cx)).into(),
            Route::CodingTask(id) => cx.new(|cx| coding_thread::CodingThreadScreen::new(id.clone(), window, cx)).into(),
            Route::CodingResult(id, round) => {
                cx.new(|cx| coding_result::CodingResultScreen::new(id.clone(), round, window, cx)).into()
            }
            Route::PostReport(report) => {
                cx.new(|cx| coding_result::CodingResultScreen::report(report, window, cx)).into()
            }
            Route::Report(origin) => cx.new(|cx| screens::report::ReportScreen::new(origin, window, cx)).into(),
        }
    }

    /// Called after the artifact list arrives or the pointer settles on a card.
    pub(crate) fn prepare_artifact(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab != Tab::Artifacts
            || !self.stack.is_empty()
            || self.artifact_pages.iter().any(|(key, _)| key == &id)
            || self.artifact_preload.as_ref().is_some_and(|(key, _)| key == &id)
        {
            return;
        }
        // Pointing at another card moves the preloaded page there: one web view loads each card in turn.
        let page = match self.artifact_preload.take() {
            Some((_, page)) => {
                page.update(cx, |page, cx| page.retarget(id.clone(), window, cx));
                page
            }
            None => cx.new(|cx| screens::artifact::ArtifactScreen::preload(id.clone(), window, cx)),
        };
        self.artifact_preload = Some((id, page));
    }

    /// Filo out of sight (hidden, minimized or covered): artifacts keep only the page on screen's web view.
    fn release_hidden_artifacts(&mut self, cx: &mut Context<Self>) {
        self.artifact_preload = None;
        let top = self.top_page().map(|view| view.entity_id());
        for (_, page) in &self.artifact_pages {
            if Some(page.entity_id()) != top {
                page.update(cx, |page, cx| page.release_web(cx));
            }
        }
    }

    /// The cursor in the open chat's message box (the open shortcut).
    pub fn focus_message_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(conversation) = self.open_conversation(cx) {
            conversation.update(cx, |conversation, cx| conversation.focus_composer(window, cx));
        }
    }

    /// The open shortcut held in any app: records a voice message, sent when it's let go (`start` false). In Filo's
    /// window it records in the open chat; from another app it goes where Settings › Keyboard shortcuts says, or to
    /// the agent or group it starts by naming (voice_route.rs). Returns whether it's recording (started), or whether a
    /// recording was sent (let go).
    pub fn talk(
        &mut self,
        start: bool,
        screenshot: Option<crate::screenshot::Screenshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !start {
            return match self.talking.take() {
                Some(Talking::InChat(conversation)) => conversation.update(cx, |conversation, cx| {
                    let recording = conversation.recording();
                    conversation.release_voice(window, cx);
                    recording
                }),
                Some(Talking::Away { recording, screenshot, open_chat }) => match recording.finish() {
                    Ok(voice) => {
                        self.send_away(voice, screenshot, open_chat, window, cx);
                        true
                    }
                    Err(_) => false,
                },
                None => false,
            };
        }
        if let Some(talking) = self.talking.take() {
            Self::drop_talking(talking, cx);
        }
        let open = self.open_conversation(cx);
        if window.is_window_active()
            && let Some(conversation) = open
        {
            let recording = conversation.update(cx, |conversation, cx| {
                conversation.set_voice_screenshot(screenshot);
                conversation.hold_voice(window, cx);
                conversation.recording()
            });
            self.talking = Some(Talking::InChat(conversation));
            return recording;
        }
        let backend = backend(cx);
        let recording = match screens::chat::conversation::AwayRecording::start(&backend) {
            Ok(recording) => recording,
            Err(problem) => {
                eprintln!("voice from another app: {problem}");
                return false;
            }
        };
        crate::media::stop(cx);
        let open_chat = open.and_then(|conversation| conversation.read(cx).session_id().map(str::to_string));
        // For the pet while the keys are held: where it goes as things stand (a name it starts with sends it there),
        // the words heard so far, or why it can't be sent.
        let choice = crate::companion::settings(cx).voice_to;
        let chat = open_chat.clone();
        let heard = recording.heard();
        cx.spawn(async move |_, cx| {
            let Ok(directory) = crate::voice_route::Directory::load(&backend).await else { return };
            let chosen = directory.chosen(&choice, chat.as_deref());
            let to = |words: &str| {
                directory.addressed(words).or_else(|| chosen.clone()).map(|recipient| directory.label(&recipient))
            };
            cx.update(|cx| crate::companion::set_listening_to(to(""), cx)).ok();
            let Some(mut heard) = heard else { return };
            while heard.changed().await.is_ok() {
                let now = heard.borrow_and_update().clone();
                let problem = now.stopped.or(now.problem).map(|problem| problem.message());
                if cx
                    .update(|cx| crate::companion::set_listening_heard(to(&now.words), now.words, problem, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        self.talking = Some(Talking::Away { recording, screenshot, open_chat });
        true
    }

    /// Sends a voice message held from another app once it's transcribed: to the agent or group it starts by naming,
    /// else where Settings says. A failed send says so in Filo's window; the recording goes either way.
    fn send_away(
        &mut self,
        voice: screens::chat::conversation::AwayVoice,
        screenshot: Option<crate::screenshot::Screenshot>,
        open_chat: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backend = backend(cx);
        let choice = crate::companion::settings(cx).voice_to;
        cx.spawn_in(window, async move |_, cx| {
            let sent = async {
                // The words pick the chat (a name it starts with), so there are none without them.
                let text = match voice.words(&backend).await {
                    Ok(text) => text,
                    Err(problem) => {
                        voice.discard();
                        return Err(problem);
                    }
                };
                let directory = crate::voice_route::Directory::load(&backend).await?;
                let recipient = directory
                    .addressed(&text)
                    .or_else(|| directory.chosen(&choice, open_chat.as_deref()))
                    .ok_or("Make an agent first.")?;
                let session = directory.session(&backend, &recipient).await?;
                // The screenshot it goes with, uploaded while the user talked (a failed one is left out).
                let attachments = match screenshot {
                    Some(screenshot) => screenshot.await.map(|shot| json!([shot])),
                    None => None,
                };
                screens::chat::conversation::send_away(backend.clone(), session, voice, text, attachments).await
            }
            .await;
            if let Err(error) = sent {
                eprintln!("voice from another app was not sent: {error}");
                cx.update(|window, cx| ui::toast_error(window, cx, "Voice message wasn’t sent", error)).ok();
            }
        })
        .detach();
    }

    fn drop_talking(talking: Talking, cx: &mut Context<Self>) {
        match talking {
            Talking::InChat(conversation) => {
                conversation.update(cx, |conversation, cx| conversation.cancel_held_voice(cx));
            }
            Talking::Away { recording, .. } => recording.cancel(),
        }
    }

    /// The open shortcut was part of another shortcut after all: the recording it started goes.
    pub fn cancel_talk(&mut self, cx: &mut Context<Self>) {
        if let Some(talking) = self.talking.take() {
            Self::drop_talking(talking, cx);
        }
    }

    /// Recording from another app (the pet's "Listening" while no chat records).
    pub fn talking_away(&self) -> bool {
        matches!(self.talking, Some(Talking::Away { .. }))
    }

    /// The Extensions tab's Skills page (smoke tests).
    #[cfg(feature = "dev")]
    pub fn open_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(Tab::Connectors, window, cx);
        if let Some(abilities) = self
            .tabs
            .get(&Tab::Connectors)
            .and_then(|view| view.clone().downcast::<screens::connectors::ConnectorsScreen>().ok())
        {
            abilities.update(cx, |abilities, cx| abilities.show_page(screens::connectors::Page::Skills, window, cx));
        }
    }

    pub fn push(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        // A second click on the same link doesn't stack a duplicate page; a page that's off doesn't open (pages.rs).
        if self.stack.last().is_some_and(|(top, _)| *top == route) || !route.shown(cx) {
            return;
        }
        let view = self.route_view(&route, window, cx);
        self.stack.push((route, view));
        self.forward.clear();
        cx.notify();
    }

    /// The Feed tab, once it has been opened (debug views).
    #[cfg(feature = "dev")]
    pub fn feed_screen(&self) -> Option<Entity<screens::feed::FeedScreen>> {
        self.tabs.get(&Tab::Feed).and_then(|view| view.clone().downcast().ok())
    }

    /// Closes a tab's view, as if it had been hidden long enough: it loads afresh when opened again.
    #[cfg(feature = "dev")]
    pub fn close_tab_view(&mut self, tab: Tab) {
        self.tabs.remove(&tab);
    }

    pub fn agents_screen(&self) -> Option<Entity<screens::agents::AgentsScreen>> {
        self.agents.clone()
    }

    /// The Extensions tab, once opened (smoke test).
    pub fn connectors_screen(&self) -> Option<Entity<screens::connectors::ConnectorsScreen>> {
        self.tabs
            .get(&Tab::Connectors)
            .and_then(|view| view.clone().downcast::<screens::connectors::ConnectorsScreen>().ok())
    }

    /// The Coding tab, once opened (pages-bottom-drive).
    #[cfg(feature = "dev")]
    pub fn coding_screen(&self) -> Option<Entity<screens::coding::CodingScreen>> {
        self.tabs.get(&Tab::Coding).and_then(|view| view.clone().downcast::<screens::coding::CodingScreen>().ok())
    }

    /// The Coding tab's screen, made now if it hasn't been: the welcome's "How it runs" shows its switches.
    pub fn coding_screen_now(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<screens::coding::CodingScreen>> {
        self.tab_view(Tab::Coding, window, cx).downcast::<screens::coding::CodingScreen>().ok()
    }

    /// The update button's state, in builds with an update feed (smoke test).
    #[cfg(feature = "dev")]
    pub fn updates(&self) -> Option<Entity<crate::updater::Updater>> {
        self.updates.clone()
    }

    /// The tab on screen (smoke test).
    #[cfg(feature = "dev")]
    pub fn current_tab(&self) -> Tab {
        self.tab
    }

    /// The list's preloaded artifact page, if any (smoke test).
    #[cfg(feature = "dev")]
    pub(crate) fn artifact_preload(&self) -> Option<Entity<screens::artifact::ArtifactScreen>> {
        self.artifact_preload.as_ref().map(|(_, page)| page.clone())
    }

    /// The pushed page on top, if any (smoke test).
    pub fn top_page(&self) -> Option<AnyView> {
        self.stack.last().map(|(_, view)| view.clone())
    }

    /// Two-finger swipe right goes back, left goes forward. Only trackpad gestures
    /// count (a mouse wheel has no phases), once per gesture, and not the momentum after it.
    fn on_scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ScrollDelta::Pixels(delta) = event.delta else { return };
        match event.touch_phase {
            TouchPhase::Started => self.swipe = Some(0.),
            TouchPhase::Ended => self.swipe = None,
            TouchPhase::Moved => {
                let Some(travel) = self.swipe.as_mut() else { return };
                let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                // A mostly vertical move is scrolling the page: this gesture can't navigate any more.
                if dy.abs() > dx.abs() * 2. && dy.abs() > 2. {
                    self.swipe = None;
                    return;
                }
                *travel += dx;
                if travel.abs() > SWIPE_DISTANCE {
                    let back = *travel > 0.;
                    self.swipe = None;
                    if back { self.back(window, cx) } else { self.forward(window, cx) }
                }
            }
        }
    }

    /// Tells the Agents tab whether the user is looking at it: Filo in front (or in view while
    /// they use the Mac), on the Agents tab with no page over it.
    fn check_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let in_view = crate::presence::window_in_view(window);
        if std::mem::replace(&mut self.window_in_view, in_view) && !in_view {
            self.release_hidden_artifacts(cx);
        }
        let looking = self.stack.is_empty() && self.welcome.is_none() && crate::presence::user_present(window);
        if let Some(agents) = self.agents.clone() {
            agents.update(cx, |agents, cx| agents.set_attending(looking && self.tab == Tab::Agents, cx));
        }
    }

    /// The agent's conversation on screen, when no page is over it.
    fn open_conversation(&self, cx: &App) -> Option<Entity<screens::chat::conversation::Conversation>> {
        if !self.stack.is_empty() {
            return None;
        }
        match self.tab {
            Tab::Agents => self.agents.as_ref()?.read(cx).current_conversation(),
            _ => None,
        }
    }

    /// Whether `session_id`'s conversation is on screen and the user is looking at it.
    fn showing_chat(&self, session_id: &str, cx: &App) -> bool {
        self.open_conversation(cx).is_some_and(|conversation| conversation.read(cx).showing(session_id))
    }

    /// Hold fn+⌃ to record a voice message in the open chat; let go to send.
    pub fn on_modifiers(&mut self, modifiers: &Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let chord =
            modifiers.function && modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.shift;
        if chord == self.voice_chord {
            return;
        }
        self.voice_chord = chord;
        let Some(conversation) = self.open_conversation(cx) else { return };
        if chord {
            self._voice_hold = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(VOICE_HOLD_DELAY).await;
                this.update_in(cx, |this, window, cx| {
                    if this.voice_chord {
                        conversation.update(cx, |conversation, cx| conversation.hold_voice(window, cx));
                    }
                })
                .ok();
            }));
        } else {
            self._voice_hold = None;
            conversation.update(cx, |conversation, cx| conversation.release_voice(window, cx));
        }
    }

    /// Esc while holding fn+⌃ cancels the recording.
    fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        let Some(conversation) = self.open_conversation(cx) else { return };
        if conversation.update(cx, |conversation, cx| conversation.cancel_held_voice(cx)) {
            cx.stop_propagation();
        }
    }

    pub fn back(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = self.stack.pop() {
            self.forward.push(page);
            cx.notify();
        }
    }

    pub fn forward(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = self.forward.pop() {
            self.stack.push(page);
            cx.notify();
        }
    }

    /// Feed or Notes turned off (pages.rs): out of its tab, and its pages left open close.
    pub fn pages_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tab.shown(cx) {
            self.open_tab(Tab::Agents, window, cx);
        }
        self.stack.retain(|(route, _)| route.shown(cx));
        self.forward.retain(|(route, _)| route.shown(cx));
        cx.notify();
    }

    /// Builds the tab's view if needed, then shows it (sidebar clicks, smoke test); not while its page is off (pages.rs).
    pub fn open_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if !tab.shown(cx) {
            return;
        }
        self.tab_view(tab, window, cx);
        self.select_tab(tab, window, cx);
    }

    pub fn select_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        let left = self.tab;
        self.tab = tab;
        self.tab_drops.remove(&tab);
        // The Coding page kept from before: how much of each tool's plan is left, asked again as it opens.
        if tab == Tab::Coding
            && left != tab
            && let Some(coding) =
                self.tabs.get(&tab).and_then(|view| view.clone().downcast::<screens::coding::CodingScreen>().ok())
        {
            coding.update(cx, |coding, cx| coding.refresh_usage(cx));
        }
        // A hidden tab's live queries keep running again as their data changes, so a tab left for a while closes and
        // loads again when opened. Agents stays: it holds drafts and open conversations.
        if left != tab && left != Tab::Agents {
            let drop = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(DROP_HIDDEN_TAB_AFTER).await;
                this.update(cx, |this, _| {
                    if this.tab != left {
                        this.tabs.remove(&left);
                    }
                })
                .ok();
            });
            self.tab_drops.insert(left, drop);
        }
        // Cached artifact entities survive clearing navigation: hide their native views first.
        for (_, page) in &self.artifact_pages {
            page.update(cx, |page, cx| page.set_page_visible(false, cx));
        }
        // Another tab: artifacts' web views go (each holds a WebKit process); Artifacts prepares again when opened.
        if tab != Tab::Artifacts {
            self.artifact_preload = None;
            self.artifact_pages.clear();
        }
        self.stack.clear();
        self.forward.clear();
        if tab == Tab::Artifacts {
            let first = self
                .tabs
                .get(&tab)
                .and_then(|view| view.clone().downcast::<screens::artifacts::ArtifactsScreen>().ok())
                .and_then(|view| view.read(cx).first_id());
            if let Some(id) = first {
                self.prepare_artifact(id, window, cx);
            }
        }
        cx.notify();
    }

    /// A note shared from the Notes tab (right-click › Share) to agent `agent`: the agent shows with the note above its
    /// composer.
    pub fn share_note(
        &mut self,
        agent: String,
        note: screens::chat::note_share::SharedNote,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_tab(Tab::Agents, window, cx);
        if let Some(agents) = self.agents.clone() {
            agents.update(cx, |agents, cx| agents.share_note(agent, note, window, cx));
        }
    }

    /// A conversation (a banner for its reply, a link to it): an agent's opens in the Agents tab, read or not. Filo's
    /// own chats have no page, so those stay where they are.
    pub fn open_chat(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Owner {
            agent_id: Option<String>,
            #[serde(default)]
            agent_group: bool,
        }
        let args = json!({ "sessionId": session_id });
        backend::call_in(
            window,
            cx,
            "agents:ofSession",
            args,
            |this, owner: Result<Option<Owner>, String>, window, cx| {
                let agent = owner.ok().flatten().and_then(|owner| {
                    if owner.agent_group { Some(screens::agents::GROUP.to_string()) } else { owner.agent_id }
                });
                if let Some(agent) = agent {
                    this.open_agent(agent, window, cx);
                }
            },
        );
    }

    /// ⌘P: opens the search box over the window, or closes it. Not over the welcome tour or a dialog.
    pub fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.close_search(true, window, cx);
            return;
        }
        if self.welcome.is_some() || gpui_component::WindowExt::has_active_dialog(window, cx) {
            return;
        }
        self.tab_view(Tab::Agents, window, cx);
        let Some(agents) = self.agents.clone() else { return };
        let focus_before = window.focused(cx);
        let panel = cx.new(|cx| screens::search::SearchPanel::new(&agents, window, cx));
        let events = cx.subscribe_in(&panel, window, |this, _, event: &screens::search::SearchEvent, window, cx| {
            use screens::search::SearchEvent;
            match event {
                SearchEvent::Close => this.close_search(true, window, cx),
                SearchEvent::OpenChat(chat) => {
                    this.close_search(false, window, cx);
                    this.open_agent(chat.clone(), window, cx);
                }
                SearchEvent::OpenMessage { chat, message, depth } => {
                    this.close_search(false, window, cx);
                    this.tab_view(Tab::Agents, window, cx);
                    this.select_tab(Tab::Agents, window, cx);
                    if let Some(agents) = &this.agents {
                        let (chat, message, depth) = (chat.clone(), message.clone(), *depth);
                        agents.update(cx, |agents, cx| agents.open_at(chat, message, depth, window, cx));
                    }
                }
            }
        });
        self.search = Some(SearchOpen { panel, _events: events, focus_before });
        cx.notify();
    }

    /// Closes the search box; `refocus`: the keyboard goes back where it was.
    fn close_search(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search.take() else { return };
        if refocus && let Some(focus) = search.focus_before {
            window.focus(&focus);
        }
        cx.notify();
    }

    /// The search box, while it's open (the drive's checks).
    #[cfg(feature = "dev")]
    pub fn search_panel(&self) -> Option<Entity<screens::search::SearchPanel>> {
        self.search.as_ref().map(|search| search.panel.clone())
    }

    /// The Agents tab, on `agent_id`'s conversation.
    pub fn open_agent(&mut self, agent_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_view(Tab::Agents, window, cx);
        self.select_tab(Tab::Agents, window, cx);
        if let Some(agents) = &self.agents {
            agents.update(cx, |agents, cx| agents.open_agent(agent_id, window, cx));
        }
    }

    /// Compact sidebar: traffic lights on top, the app icon as picked (App icon), the tabs as icon-over-label,
    /// then the update button (CI builds) and Settings at the bottom.
    fn render_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        // The agents' unseen replies, while Agents is out of sight.
        let unseen = cx.try_global::<crate::unseen::UnseenGlobal>().map(|global| global.0.read(cx));
        let agents_dot = !(self.tab == Tab::Agents && self.stack.is_empty())
            && unseen.is_some_and(|unseen| unseen.agents_total() > 0);
        let mut selected_hover = colors.selected_fill;
        selected_hover.a = (selected_hover.a * 1.6).min(1.);
        // The App icon page on top: the icon is the open spot, not the tab under the page.
        let icon_page = self.stack.last().is_some_and(|(route, _)| *route == Route::AppIcon);
        let tab_item = |this: &mut Self, tab: Tab, cx: &mut Context<Self>| {
            let active = this.tab == tab && !icon_page;
            let color = if active { colors.accent } else { colors.text_secondary };
            let dot = tab == Tab::Agents && agents_dot;
            div()
                .id(SharedString::from(format!("tab-{}", tab.label())))
                .w_full()
                .py(px(space::SM))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::XXS))
                .rounded(px(radius::MD))
                .cursor_pointer()
                // The open tab sits on a pill, a shade stronger under the pointer.
                .when(active, |this| this.bg(colors.selected_fill).hover(move |style| style.bg(selected_hover)))
                .when(!active, |this| this.hover(|style| style.bg(colors.selected_fill.opacity(0.6))))
                .child(
                    div()
                        .relative()
                        .child(match tab.icon() {
                            Some(icon) => ui::icon(icon).size(px(24.)).text_color(color).into_any_element(),
                            // A picture can't take the tab's colour, so it dims when the tab isn't open.
                            None => {
                                use gpui::StyledImage as _;
                                gpui::img("images/tab-agents.png")
                                    .w(px(34.))
                                    .h(px(24.))
                                    .object_fit(gpui::ObjectFit::Contain)
                            }
                            .opacity(if active { 1. } else { 0.6 })
                            .into_any_element(),
                        })
                        .when(dot, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .top(px(-1.))
                                    .right(px(-3.))
                                    .size(px(9.))
                                    .rounded(px(radius::FULL))
                                    .bg(colors.accent)
                                    .border_1()
                                    .border_color(colors.sunken),
                            )
                        }),
                )
                .child(div().micro().text_color(color).when(active, |this| this.semibold()).child(tab.label()))
                .on_click(cx.listener(move |this, _, window, cx| this.open_tab(tab, window, cx)))
        };
        let tabs: Vec<_> = crate::nav::sidebar(cx)
            .into_iter()
            .filter(|tab| *tab != Tab::Settings)
            .map(|tab| tab_item(self, tab, cx))
            .collect();
        let settings = tab_item(self, Tab::Settings, cx);
        div().w(px(SIDEBAR_WIDTH)).flex_none().h_full().p(px(6.)).child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .px(px(space::XS))
                .pb(px(space::SM))
                .rounded(px(radius::LG))
                .bg(colors.sunken)
                .border_1()
                .border_color(colors.border.opacity(0.6))
                // Room for the traffic lights; this band drags the window.
                .child(crate::chrome::drag_region(div().id("sidebar-drag").h(px(40.)).w_full().flex_none()))
                .child(
                    div()
                        .id("sidebar-app-icon")
                        .track_focus(&self.logo_focus)
                        .mb(px(space::SM))
                        // The tabs' shape: as wide as them, the same padding, pill and shades.
                        .w_full()
                        .py(px(space::SM))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::MD))
                        .border_1()
                        // Tab reaches it (a ring shows); Return or Space click it.
                        .border_color(if self.logo_focus.is_focused(window) {
                            colors.accent
                        } else {
                            gpui::transparent_black()
                        })
                        .cursor_pointer()
                        .when(icon_page, |this| {
                            this.bg(colors.selected_fill).hover(move |style| style.bg(selected_hover))
                        })
                        .when(!icon_page, |this| this.hover(|style| style.bg(colors.selected_fill.opacity(0.6))))
                        .active(|style| style.opacity(0.62))
                        // A click doesn't focus it (or keep focus from Tab), so the ring only shows for the keyboard.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, _| {
                                window.prevent_default();
                                if this.logo_focus.is_focused(window) {
                                    this.focus.focus(window);
                                }
                            }),
                        )
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Change app icon").build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.push(Route::AppIcon, window, cx)))
                        // Filo's own icon is the bare mark, as before icons could be picked; the others keep their tile.
                        .child(match crate::app_icon::chosen() {
                            icon if icon == crate::app_icon::Icon::default() => {
                                ui::filo_mark(32., cx).into_any_element()
                            }
                            icon => crate::screens::app_icon::icon(&icon, 32.),
                        }),
                )
                .child(div().w_full().flex().flex_col().gap(px(space::XS)).children(tabs))
                .child(div().flex_1())
                .children(self.updates.clone())
                .child(settings),
        )
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::escape::frame(window, cx);
        ui::set_content_width(f32::from(window.viewport_size().width) - SIDEBAR_WIDTH);
        // Native web views draw above everything: only the top page may show one.
        let top = self.stack.len().saturating_sub(1);
        let pages = self.stack.iter().enumerate().map(|(index, (_, view))| (index == top, view));
        let left = self.forward.iter().map(|(_, view)| (false, view));
        for (visible, view) in pages.chain(left) {
            if let Ok(artifact) = view.clone().downcast::<screens::artifact::ArtifactScreen>() {
                artifact.update(cx, |artifact, cx| artifact.set_page_visible(visible, cx));
            } else if let Ok(note) = view.clone().downcast::<screens::note::NoteScreen>() {
                note.update(cx, |note, cx| note.set_page_visible(visible, cx));
            }
        }
        // A skill's page (Extensions › Skills) shows only while its tab is on screen with nothing pushed over it.
        if let Some(abilities) = self.connectors_screen() {
            let visible = self.tab == Tab::Connectors && self.stack.is_empty() && self.welcome.is_none();
            abilities.update(cx, |abilities, cx| abilities.set_page_visible(visible, cx));
        }
        let colors = *cx.colors();
        // The welcome tour covers everything while it shows.
        let content = if self.welcome.is_some() {
            self.render_welcome()
        } else {
            let main = match self.stack.last() {
                Some((_, view)) => view.clone(),
                None => self.tab_view(self.tab, window, cx),
            };
            div()
                .size_full()
                .flex()
                .child(self.render_sidebar(window, cx))
                .child(
                    div()
                        .id("main")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .on_scroll_wheel(cx.listener(|this, event, window, cx| this.on_scroll(event, window, cx)))
                        .child(main),
                )
                .into_any_element()
        };
        // Key focus for fn+⌃ when nothing else has it (after a click on plain content).
        if window.focused(cx).is_none() {
            self.focus.focus(window);
        }
        div()
            .track_focus(&self.focus)
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                this.on_modifiers(&event.modifiers, window, cx)
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key_down(event, cx)))
            .size_full()
            .relative()
            .font_family(cx.theme().font_family())
            .bg(colors.background)
            .text_color(colors.text)
            .child(content)
            .children(self.search.as_ref().map(|search| search.panel.clone()))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
            .children(crate::smoke::overlay(window, cx))
            // Last: hides the glance pages this frame didn't lay out (screens/chat/glance.rs).
            .child(screens::chat::conversation::glance::sweep())
    }
}

// The browser window (Window › Browser, ⇧⌘B; browser.rs): the tabs, the agents' and the user's own, with back,
// forward, reload and an address bar, and the shown tab's page under them, moved in from its window off the screen
// (and back when another tab shows or the window closes). A tab an agent asked the user to step in on
// (browser_ask_user) has a bar with the agent's reason and Done; the card in Filo's main window opens it.
//
// While an agent works in the shown tab, a bar over its page says so ("Scout is working", with its last step and,
// opened, its trail: browser/activity.rs), with Pause and Take over; paused, Resume; taken over, Hand back. Where the
// agent clicks, a ring shows on the page (browser.rs `flash`). A page's alert, confirm or prompt shows as a bar too,
// with its buttons, for whoever answers first: the user there or the agent (browser_dialog).
//
// What's brought in from Helium or Chrome (browser/import.rs): the address bar suggests its bookmarks and history as
// the user types, in a band under the toolbar (the page is a native view over the window, so nothing of GPUI's can
// cover it), a page with a login form on a site that has a saved login gets a bar offering to fill it, and the import
// panel shows in the page's place.

use std::time::Duration;

use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Focusable as _, Global, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, px, size,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::notification::Notification;
use gpui_component::{Root, Sizable as _, WindowExt as _};
use objc2::runtime::AnyObject;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

use super::Control;
use super::activity;
use super::import::{self, ImportPanel};

gpui::actions!(filo, [OpenBrowser]);

/// The window while it's open.
struct Open(Option<WindowHandle<Root>>);

impl Global for Open {}

/// Keys the help card in the main window.
struct HelpCard;

pub fn init(cx: &mut App) {
    cx.set_global(Open(None));
    cx.on_action(|_: &OpenBrowser, cx| open(None, cx));
    cx.bind_keys([gpui::KeyBinding::new("cmd-shift-b", OpenBrowser, None)]);
}

/// Opens the browser window (or brings it forward) on tab `id`, else the one it showed, else the newest.
pub fn open(id: Option<String>, cx: &mut App) {
    let pick = id
        .or_else(super::shown)
        .filter(|id| super::tab(id).is_some())
        .or_else(|| super::tabs().last().map(|tab| tab.id.clone()));
    super::set_shown(pick);
    if let Some(handle) = cx.global::<Open>().0
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(1240.), px(860.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(560.), px(420.))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("Browser".into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(16.), px(20.))),
        }),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        window.on_window_should_close(cx, |_, cx| {
            if let Some(id) = super::shown() {
                super::park(&id);
            }
            cx.global_mut::<Open>().0 = None;
            true
        });
        let view = cx.new(|cx| BrowserWindow::new(window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    });
    match opened {
        Ok(handle) => {
            cx.global_mut::<Open>().0 = Some(handle);
            cx.activate(true);
        }
        Err(error) => eprintln!("browser: couldn't open the window: {error:#}"),
    }
}

/// Closes the browser window, its page back in its own window (a check's end, `dev` builds).
#[cfg(feature = "dev")]
pub fn close(cx: &mut App) {
    if let Some(handle) = cx.global_mut::<Open>().0.take() {
        if let Some(id) = super::shown() {
            super::park(&id);
        }
        handle.update(cx, |_, window, _| window.remove_window()).ok();
    }
}

/// The card in Filo's main window when an agent asks the user to step in: a click opens the tab. Filo in the
/// background bounces in the Dock.
pub fn help_card(id: &str, agent: &str, reason: &str, cx: &mut App) {
    let Some(main) = cx.try_global::<crate::companion::MainWindow>().map(|main| main.0) else { return };
    let tab = id.to_string();
    let (title, reason) = (format!("{agent} needs you in the browser"), reason.to_string());
    main.update(cx, |_, window, cx| {
        window.push_notification(
            Notification::new()
                .id::<HelpCard>()
                .title(title)
                .message(format!("{reason} Click to open the page."))
                .autohide(false)
                .on_click(move |_, _, cx| open(Some(tab.clone()), cx)),
            cx,
        );
    })
    .ok();
    let (active, _) = super::frontmost();
    if !active {
        // NSInformationalRequest: one bounce.
        // SAFETY: an AppKit message on the main thread.
        unsafe {
            let app: *mut AnyObject = objc2::msg_send![objc2::class!(NSApplication), sharedApplication];
            let _: isize = objc2::msg_send![app, requestUserAttention: 10isize];
        }
    }
}

pub fn remove_help_card(_id: &str, cx: &mut App) {
    let Some(main) = cx.try_global::<crate::companion::MainWindow>().map(|main| main.0) else { return };
    main.update(cx, |_, window, cx| window.remove_notification::<HelpCard>(cx)).ok();
}

/// GPUI's view in `window`, which pages are put in.
fn content_view(window: &Window) -> Option<*mut AnyObject> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    Some(handle.ns_view.as_ptr() as *mut AnyObject)
}

/// A saved login offered on the page at `url` of tab `tab`.
struct Offer {
    tab: String,
    url: String,
    usernames: Vec<String>,
}

pub struct BrowserWindow {
    address: Entity<InputState>,
    /// The browser's generation and the shown page's state when last drawn.
    seen: (u64, String, String, bool),
    /// The tab whose page is in this window now.
    placed: Option<String>,
    /// The address bar's suggestions for what's typed, and the one picked with the arrow keys.
    suggestions: Vec<filo_local::browser_import::Place>,
    picked: Option<usize>,
    /// The import panel, shown in the page's place.
    import: Option<(Entity<ImportPanel>, Subscription)>,
    /// The tab and address last looked at for a login form, and the login offered there.
    looked: (String, String),
    offer: Option<Offer>,
    /// The agent's trail opened under its bar.
    trail_open: bool,
    /// What the agents' bars showed when last drawn: each tab's working, who has it and its dialog, and the steps.
    seen_work: (Vec<(bool, Control, bool)>, usize),
    _look: Option<Task<()>>,
    _poll: Task<()>,
    _address: Subscription,
    /// Clicks in the window, and what follows them (`page_took_keys`).
    _clicks: Option<(super::native::ClickWatch, Task<()>)>,
}

impl BrowserWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Search or type an address"));
        let _address =
            cx.subscribe_in(&address, window, |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let typed = match this.picked.and_then(|index| this.suggestions.get(index)) {
                        Some(place) => place.url.clone(),
                        None => input.read(cx).value().to_string(),
                    };
                    this.go_to(&typed, window, cx);
                }
                InputEvent::Change => {
                    let typed = input.read(cx).value().to_string();
                    let editing = input.read(cx).focus_handle(cx).is_focused(window);
                    this.suggestions = if editing { import::suggest(&typed) } else { Vec::new() };
                    this.picked = None;
                    cx.notify();
                }
                InputEvent::Blur => this.close_suggestions(cx),
                InputEvent::Focus => {}
            });
        // Titles, addresses and loading come from WebKit, and tabs from the agents: looked at a few times a second.
        let _poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(300)).await;
                let alive = this.update_in(cx, |this, window, cx| this.refresh(window, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        // A click in the page gives it the keyboard without GPUI seeing the click: looked at once AppKit handed it on.
        let _clicks = content_view(window).and_then(|view| {
            let (clicked, mut clicks) = futures::channel::mpsc::unbounded::<()>();
            let watch = super::native::ClickWatch::new(view, move || drop(clicked.unbounded_send(())))?;
            let task = cx.spawn_in(window, async move |this, cx| {
                while clicks.next().await.is_some() {
                    if this.update_in(cx, |this, window, cx| this.page_took_keys(window, cx)).is_err() {
                        break;
                    }
                }
            });
            Some((watch, task))
        });
        let mut this = BrowserWindow {
            address,
            seen: (u64::MAX, String::new(), String::new(), false),
            placed: None,
            suggestions: Vec::new(),
            picked: None,
            import: None,
            looked: Default::default(),
            offer: None,
            trail_open: false,
            seen_work: Default::default(),
            _look: None,
            _poll,
            _address,
            _clicks,
        };
        this.refresh(window, cx);
        this
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.look_for_login(cx);
        // An agent's keys in the shown page give it the keyboard too.
        self.page_took_keys(window, cx);
        let shown = super::shown().and_then(|id| super::tab(&id));
        let (title, url, loading) = shown.as_ref().map(|tab| tab.state()).unwrap_or_default();
        let now = (super::generation(), title.clone(), url.clone(), loading);
        // An agent stops counting as working a moment after its last step, with nothing else changing.
        let work = (
            super::tabs().iter().map(|tab| (tab.working(), tab.control(), tab.dialog().is_some())).collect(),
            activity::steps(),
        );
        if work != self.seen_work {
            self.seen_work = work;
            cx.notify();
        }
        if now == self.seen {
            return;
        }
        let url_changed = now.2 != self.seen.2 || now.0 != self.seen.0;
        // Another page shows: what was typed for the last one is moot.
        if now.2 != self.seen.2 {
            self.suggestions.clear();
            self.picked = None;
        }
        self.seen = now;
        window.set_window_title(if title.is_empty() { "Browser" } else { &title });
        let editing = self.address.read(cx).focus_handle(cx).is_focused(window);
        if url_changed && !editing {
            let shown_url = if url == "about:blank" { String::new() } else { url };
            self.address.update(cx, |input, cx| input.set_value(shown_url, window, cx));
        }
        cx.notify();
    }

    /// The page has the window's keyboard (it was clicked): nothing of GPUI's keeps its focus, or ⌘A, ⌘V and the other
    /// keys GPUI is offered first went to the address bar (and it didn't show the page's address as it changed).
    fn page_took_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page_has_keys = self.placed.as_deref().is_some_and(super::has_keyboard);
        if page_has_keys && window.focused(cx).is_some() {
            window.blur();
            cx.notify();
        }
    }

    fn go_to(&mut self, typed: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = super::address(typed);
        self.close_import();
        self.close_suggestions(cx);
        if url != "about:blank" {
            self.address.update(cx, |input, cx| input.set_value(url.clone(), window, cx));
        }
        match super::shown().and_then(|id| super::tab(&id)) {
            Some(tab) => tab.load(&url),
            None => match super::open_for_user(&url) {
                Ok(tab) => super::set_shown(Some(tab.id.clone())),
                Err(error) => ui::toast(window, cx, error),
            },
        }
        cx.notify();
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match super::open_for_user("about:blank") {
            Ok(tab) => {
                super::set_shown(Some(tab.id.clone()));
                self.address.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.focus(window, cx);
                });
            }
            Err(error) => ui::toast(window, cx, error),
        }
        cx.notify();
    }

    fn show(&mut self, id: String, cx: &mut Context<Self>) {
        self.close_import();
        super::set_shown(Some(id));
        cx.notify();
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        if !self.suggestions.is_empty() {
            self.suggestions.clear();
            self.picked = None;
            cx.notify();
        }
    }

    fn move_pick(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.suggestions.len() as isize;
        if count == 0 {
            return;
        }
        let next = match self.picked {
            None if by > 0 => 0,
            None => count - 1,
            Some(index) => index as isize + by,
        };
        self.picked = (0..count).contains(&next).then_some(next as usize);
        cx.notify();
    }

    /// Opens the import panel in the page's place, or closes it.
    pub fn toggle_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import.take().is_some() {
            self.looked = Default::default();
        } else {
            let panel = cx.new(ImportPanel::new);
            let closing = cx.subscribe_in(&panel, window, |this: &mut Self, _, _: &import::Close, _, cx| {
                this.close_import();
                cx.notify();
            });
            self.import = Some((panel, closing));
        }
        cx.notify();
    }

    fn close_import(&mut self) {
        if self.import.take().is_some() {
            // Logins may have come in: look at the page again.
            self.looked = Default::default();
        }
    }

    /// Whether the shown page has a login form on a site with a saved login, once it's loaded: then the bar offers it.
    fn look_for_login(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = super::shown().and_then(|id| super::tab(&id)) else {
            self.offer = None;
            return;
        };
        let (_, url, loading) = tab.state();
        let here = (tab.id.clone(), url.clone());
        if loading || self.looked == here || !url.starts_with("http") {
            return;
        }
        // Not read yet: the next look, once they're in.
        if import::logins(cx, |_| {}).is_none() {
            return;
        }
        self.looked = here;
        self.offer = None;
        cx.notify();
        let usernames: Vec<String> = import::logins_for(&url).into_iter().map(|login| login.username).collect();
        if usernames.is_empty() {
            return;
        }
        let id = tab.id.clone();
        // Some pages draw their form a moment after they load.
        self._look = Some(cx.spawn(async move |this, cx| {
            for attempt in 0..4 {
                if attempt > 0 {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                }
                let Some(tab) = super::tab(&id).filter(|tab| tab.state().1 == url) else { return };
                let found = super::page(cx, &tab, "login_form", serde_json::json!({})).await;
                if found.is_ok_and(|form| form["password"] == true || form["username"] == true) {
                    let offer = Offer { tab: id, url, usernames };
                    this.update(cx, |this, cx| {
                        this.offer = Some(offer);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
        }));
    }

    /// Fills the saved login of `username` into the offered page.
    fn fill_login(&mut self, username: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(offer) = self.offer.take() else { return };
        cx.notify();
        let Some(tab) = super::tab(&offer.tab).filter(|tab| tab.state().1 == offer.url) else { return };
        let Some(login) = import::logins_for(&offer.url).into_iter().find(|login| login.username == username) else {
            return;
        };
        let host =
            url::Url::parse(&offer.url).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default();
        let request = serde_json::json!({ "host": host, "username": login.username, "password": login.password });
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = super::page(cx, &tab, "fill_login", request).await {
                cx.update(|window, cx| ui::toast(window, cx, format!("Couldn't fill the login: {error}"))).ok();
            }
        })
        .detach();
    }

    fn close_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        let next = if super::shown().as_deref() == Some(id) {
            let tabs = super::tabs();
            let index = tabs.iter().position(|tab| tab.id == id);
            index
                .and_then(|index| {
                    tabs.get(index + 1).or_else(|| index.checked_sub(1).and_then(|before| tabs.get(before)))
                })
                .map(|tab| tab.id.clone())
        } else {
            super::shown()
        };
        if self.placed.as_deref() == Some(id) {
            self.placed = None;
        }
        super::close(id);
        super::set_shown(next);
        cx.notify();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let shown = super::shown().and_then(|id| super::tab(&id));
        let (back, forward) = shown.as_ref().map(|tab| tab.history()).unwrap_or_default();
        let loading = self.seen.3;
        let go = |how: &'static str| {
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut App| {
                if let Some(tab) = super::shown().and_then(|id| super::tab(&id)) {
                    tab.go(how);
                }
            }
        };
        crate::chrome::drag_region(div().id("browser-toolbar"))
            .h(px(52.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .pl(px(84.))
            .pr(px(space::MD))
            .border_b_1()
            .border_color(colors.border)
            .child(
                ui::IconButton::new("browser-back", "chevron-left")
                    .small()
                    .disabled(!back)
                    .tooltip("Back")
                    .on_click(go("back")),
            )
            .child(
                ui::IconButton::new("browser-forward", "chevron-right")
                    .small()
                    .disabled(!forward)
                    .tooltip("Forward")
                    .on_click(go("forward")),
            )
            .child(
                ui::IconButton::new("browser-reload", if loading { "x" } else { "rotate-cw" })
                    .small()
                    .disabled(shown.is_none())
                    .tooltip(if loading { "Stop" } else { "Reload" })
                    .on_click(go(if loading { "stop" } else { "reload" })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.))
                    .when(!self.suggestions.is_empty(), |field| {
                        field
                            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                                this.move_pick(-1, cx);
                                cx.stop_propagation();
                            }))
                            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                                this.move_pick(1, cx);
                                cx.stop_propagation();
                            }))
                            .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                                this.close_suggestions(cx);
                                cx.stop_propagation();
                            }))
                    })
                    .child(Input::new(&self.address).small()),
            )
            .child(
                ui::IconButton::new("browser-new-tab", "plus")
                    .small()
                    .tooltip("New tab")
                    .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx))),
            )
            .child(
                ui::IconButton::new("browser-import", "download")
                    .small()
                    .selected(self.import.is_some())
                    .tooltip("Import from Helium or Chrome")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_import(window, cx))),
            )
    }

    fn tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let shown = super::shown();
        let tabs = super::tabs();
        div()
            .id("browser-tabs")
            .flex_none()
            .h(px(38.))
            .flex()
            .items_center()
            .gap(px(space::XS))
            .px(px(space::SM))
            .overflow_x_scroll()
            .border_b_1()
            .border_color(colors.border)
            .children(tabs.iter().map(|tab| {
                let (title, url, _) = tab.state();
                let label = if !title.is_empty() {
                    title
                } else if url.is_empty() || url == "about:blank" {
                    "New tab".into()
                } else {
                    url
                };
                let selected = shown.as_deref() == Some(tab.id.as_str());
                let id = tab.id.clone();
                let close_id = tab.id.clone();
                let asks = tab.help().is_some_and(|help| help.answer.is_none()) || tab.dialog().is_some();
                let held = tab.agent_shown() && tab.control() != Control::Agent;
                let working = tab.working() && !held;
                div()
                    .id(SharedString::from(format!("browser-tab-{}", tab.id)))
                    .flex_none()
                    .h(px(28.))
                    .max_w(px(240.))
                    .flex()
                    .items_center()
                    .gap(px(space::XS))
                    .pl(px(space::SM))
                    .pr(px(space::XXS))
                    .rounded(px(radius::SM))
                    .cursor_pointer()
                    .when(selected, |tab| tab.bg(colors.selected_fill))
                    .when(!selected, |tab| tab.hover(|tab| tab.bg(colors.surface)))
                    .on_click(cx.listener(move |this, _, _, cx| this.show(id.clone(), cx)))
                    .when(asks || held, |row| {
                        row.child(div().size(px(7.)).flex_none().rounded_full().bg(colors.warning))
                    })
                    .when(working && !asks, |row| {
                        row.child(div().size(px(7.)).flex_none().rounded_full().bg(colors.accent))
                    })
                    .when_some(tab.owner.as_ref(), |row, owner| {
                        row.child(
                            div().flex_none().caption().text_color(colors.text_secondary).child(owner.agent.clone()),
                        )
                    })
                    .child(div().min_w_0().truncate().footnote().child(label))
                    .child(
                        ui::IconButton::new(SharedString::from(format!("browser-close-{}", tab.id)), "x")
                            .small()
                            .tooltip("Close tab")
                            .on_click(cx.listener(move |this, _, _, cx| this.close_tab(&close_id, cx))),
                    )
            }))
    }

    /// The address bar's suggestions, a band under the toolbar.
    fn suggestion_list(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.suggestions.is_empty() {
            return None;
        }
        let colors = *cx.colors();
        Some(
            div()
                .id("browser-suggestions")
                .flex_none()
                .flex()
                .flex_col()
                .py(px(space::XS))
                .px(px(space::SM))
                .border_b_1()
                .border_color(colors.border)
                .bg(colors.raised)
                .children(self.suggestions.iter().enumerate().map(|(index, place)| {
                    let url = place.url.clone();
                    let shown_url = place
                        .url
                        .split_once("://")
                        .map_or(place.url.as_str(), |(_, rest)| rest)
                        .trim_end_matches('/')
                        .to_string();
                    let title = if place.title.is_empty() { shown_url.clone() } else { place.title.clone() };
                    div()
                        .id(SharedString::from(format!("browser-suggestion-{index}")))
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .px(px(space::SM))
                        .rounded(px(radius::SM))
                        .cursor_pointer()
                        .when(self.picked == Some(index), |row| row.bg(colors.selected_fill))
                        .when(self.picked != Some(index), |row| row.hover(|row| row.bg(colors.surface)))
                        .child(ui::icon_sized(
                            if place.bookmark { "bookmark" } else { "clock-3" },
                            14.,
                            colors.text_tertiary,
                        ))
                        .child(div().flex_none().max_w(px(420.)).truncate().callout().child(title))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .footnote()
                                .text_color(colors.text_secondary)
                                .child(shown_url),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.go_to(&url, window, cx)))
                })),
        )
    }

    /// The bar offering a saved login on a page that asks for one.
    fn login_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = *cx.colors();
        let offer = self.offer.as_ref().filter(|offer| super::shown().as_deref() == Some(offer.tab.as_str()))?;
        let host =
            url::Url::parse(&offer.url).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default();
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .px(px(space::LG))
                .py(px(space::SM))
                .bg(colors.info_soft)
                .text_color(colors.on_info_soft)
                .child(ui::icon_sized("key-round", 15., colors.on_info_soft))
                .child(div().flex_1().min_w_0().truncate().callout().child(format!("Saved login for {host}")))
                .children(offer.usernames.iter().take(3).enumerate().map(|(index, username)| {
                    let name = username.clone();
                    let label =
                        if username.is_empty() { "Fill password".to_string() } else { format!("Fill {username}") };
                    let id = SharedString::from(format!("browser-fill-{index}"));
                    (if index == 0 { ui::Button::primary(id, label) } else { ui::Button::secondary(id, label) })
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| this.fill_login(&name, window, cx)))
                }))
                .child(ui::Button::ghost("browser-fill-no", "Not now").small().on_click(cx.listener(
                    |this, _, _, cx| {
                        this.offer = None;
                        cx.notify();
                    },
                ))),
        )
    }

    /// The bar over an agent's page while it works there or the user holds it: who, its last step, the trail, and
    /// Pause / Take over / Resume / Hand back.
    fn agent_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = *cx.colors();
        let tab = super::shown().and_then(|id| super::tab(&id)).filter(|tab| tab.agent_shown())?;
        let agent = tab.agent()?;
        let control = tab.control();
        let trail = activity::trail(&agent, if self.trail_open { 12 } else { 1 });
        let (bg, fg, dot, title, detail) = match control {
            Control::Agent => (
                colors.accent_soft,
                colors.on_accent_soft,
                colors.accent,
                format!("{agent} is working"),
                trail.last().map(|step| step.what.clone()).unwrap_or_default(),
            ),
            Control::Paused => (
                colors.warning_soft,
                colors.on_warning_soft,
                colors.warning,
                format!("{agent} is paused"),
                "Its next step waits until you resume.".to_string(),
            ),
            Control::User => (
                colors.info_soft,
                colors.on_info_soft,
                colors.info,
                "You have this page".to_string(),
                format!("{agent} waits until you hand it back."),
            ),
        };
        let id = tab.id.clone();
        let set = move |control: Control| {
            let id = id.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                if let Some(tab) = super::tab(&id) {
                    tab.set_control(control);
                }
                cx.refresh_windows();
            }
        };
        let buttons: Vec<gpui::AnyElement> = match control {
            Control::Agent => vec![
                ui::Button::ghost("browser-agent-pause", "Pause")
                    .small()
                    .icon("pause")
                    .on_click(set(Control::Paused))
                    .into_any_element(),
                ui::Button::secondary("browser-agent-take", "Take over")
                    .small()
                    .icon("user")
                    .on_click(set(Control::User))
                    .into_any_element(),
            ],
            Control::Paused => vec![
                ui::Button::secondary("browser-agent-take", "Take over")
                    .small()
                    .icon("user")
                    .on_click(set(Control::User))
                    .into_any_element(),
                ui::Button::primary("browser-agent-resume", "Resume")
                    .small()
                    .icon("play")
                    .on_click(set(Control::Agent))
                    .into_any_element(),
            ],
            Control::User => vec![
                ui::Button::primary("browser-agent-hand-back", format!("Hand back to {agent}"))
                    .small()
                    .on_click(set(Control::Agent))
                    .into_any_element(),
            ],
        };
        let shown_tab = tab.id.clone();
        let steps = self.trail_open.then(|| {
            div().flex().flex_col().gap(px(2.)).pl(px(space::LG + 7. + space::SM)).children(trail.iter().rev().map(
                |step| {
                    div()
                        .flex()
                        .gap(px(space::SM))
                        .footnote()
                        .child(
                            div()
                                .flex_none()
                                .w(px(56.))
                                .text_color(colors.text_secondary)
                                .child(activity::ago(step.at)),
                        )
                        .child(div().min_w_0().truncate().child(step.what.clone()))
                        .when(step.tab != shown_tab, |row| {
                            row.child(
                                div().flex_none().text_color(colors.text_secondary).child(format!("in {}", step.tab)),
                            )
                        })
                },
            ))
        });
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(space::XS))
                .px(px(space::LG))
                .py(px(space::SM))
                .bg(bg)
                .text_color(fg)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(div().size(px(7.)).flex_none().rounded_full().bg(dot))
                        .child(div().flex_none().callout().semibold().child(title))
                        .child(div().flex_1().min_w_0().truncate().callout().child(detail))
                        .child(
                            ui::IconButton::new("browser-agent-trail", "list")
                                .small()
                                .selected(self.trail_open)
                                .tooltip(if self.trail_open { "Hide its last steps" } else { "Its last steps" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.trail_open = !this.trail_open;
                                    cx.notify();
                                })),
                        )
                        .children(buttons),
                )
                .children(steps),
        )
    }

    /// A page's alert, confirm or prompt, with its buttons.
    fn dialog_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = *cx.colors();
        let tab = super::shown().and_then(|id| super::tab(&id))?;
        let (kind, message) = tab.dialog()?;
        let (ok, cancel) = (tab.id.clone(), tab.id.clone());
        let host = url::Url::parse(&tab.state().1)
            .ok()
            .and_then(|url| url.host_str().map(String::from))
            .unwrap_or_else(|| "The page".into());
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(space::MD))
                .px(px(space::LG))
                .py(px(space::SM))
                .bg(colors.warning_soft)
                .text_color(colors.on_warning_soft)
                .child(div().flex_1().min_w_0().callout().child(format!("{host} says: {message}")))
                .when(kind != "alert", |bar| {
                    bar.child(ui::Button::ghost("browser-dialog-cancel", "Cancel").small().on_click(move |_, _, cx| {
                        if let Some(tab) = super::tab(&cancel) {
                            tab.answer_dialog(false, None);
                        }
                        cx.refresh_windows();
                    }))
                })
                .child(ui::Button::primary("browser-dialog-ok", "OK").small().on_click(move |_, _, cx| {
                    if let Some(tab) = super::tab(&ok) {
                        tab.answer_dialog(true, None);
                    }
                    cx.refresh_windows();
                })),
        )
    }

    fn help_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let colors = *cx.colors();
        let tab = super::shown().and_then(|id| super::tab(&id))?;
        let help = tab.help().filter(|help| help.answer.is_none())?;
        let (done, declined) = (tab.id.clone(), tab.id.clone());
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(space::MD))
                .px(px(space::LG))
                .py(px(space::SM))
                .bg(colors.warning_soft)
                .text_color(colors.on_warning_soft)
                .child(div().flex_1().min_w_0().callout().child(format!("{} asks: {}", help.agent, help.reason)))
                .child(ui::Button::ghost("browser-help-no", "Not now").small().on_click(move |_, _, _| {
                    if let Some(tab) = super::tab(&declined) {
                        tab.answer_help("declined");
                    }
                }))
                .child(ui::Button::primary("browser-help-done", "Done").small().on_click(move |_, _, _| {
                    if let Some(tab) = super::tab(&done) {
                        tab.answer_help("done");
                    }
                })),
        )
    }
}

impl Render for BrowserWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        // The import panel takes the page's place.
        let shown = super::shown().filter(|id| super::tab(id).is_some() && self.import.is_none());
        // Another tab shows now: the one that was here goes back to its own window.
        if self.placed != shown
            && let Some(old) = self.placed.take()
        {
            super::park(&old);
        }
        let page = match shown.clone() {
            _ if self.import.is_some() => {
                self.import.as_ref().map(|(panel, _)| panel.clone()).expect("import").into_any_element()
            }
            Some(id) => {
                let this = cx.entity().downgrade();
                canvas(
                    move |bounds: Bounds<Pixels>, window, cx| {
                        let Some(parent) = content_view(window) else { return };
                        let f = |value: Pixels| f64::from(f32::from(value));
                        let first = !super::is_placed(&id, parent);
                        super::place(
                            &id,
                            parent,
                            (f(bounds.origin.x), f(bounds.origin.y), f(bounds.size.width), f(bounds.size.height)),
                        );
                        if first {
                            this.update(cx, |this, _| this.placed = Some(id.clone())).ok();
                        }
                    },
                    |_, _, _, _| {},
                )
                .size_full()
                .into_any_element()
            }
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::SM))
                .child(ui::icon_sized("globe", 28., colors.text_tertiary))
                .child(div().headline().child("No pages open"))
                .child(
                    div()
                        .callout()
                        .text_color(colors.text_secondary)
                        .child("Type an address above. Pages your agents open show here too."),
                )
                .child(
                    div().pt(px(space::SM)).child(
                        ui::Button::secondary("browser-empty-import", "Import from Helium or Chrome")
                            .small()
                            .icon("download")
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_import(window, cx))),
                    ),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.text)
            // A click on the window's own parts gives the keyboard back to them (a page that was clicked has it).
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                if let Some(view) = content_view(window) {
                    super::give_keys_to(view);
                }
            })
            .child(self.toolbar(cx))
            .children(self.suggestion_list(cx))
            .child(self.tab_strip(cx))
            .children(self.agent_bar(cx))
            .children(self.dialog_bar(cx))
            .children(self.help_bar(cx))
            .children(self.login_bar(cx))
            .child(div().flex_1().min_h_0().relative().child(page))
    }
}

/// What `FILO_OPEN=browser-import-check` (browser/import_check.rs) and the tools check work the window with.
#[cfg(feature = "dev")]
impl BrowserWindow {
    /// The window and its view, while it's open.
    pub fn open_view(cx: &mut App) -> Option<(WindowHandle<Root>, Entity<BrowserWindow>)> {
        let handle = cx.global::<Open>().0?;
        let view =
            handle.update(cx, |root, _, _| root.view().clone().downcast::<BrowserWindow>().ok()).ok().flatten()?;
        Some((handle, view))
    }

    /// The agent's trail opened under its bar (the browser tools check's picture).
    pub fn open_trail(&mut self, cx: &mut Context<Self>) {
        self.trail_open = true;
        cx.notify();
    }

    pub fn import_panel(&self) -> Option<Entity<ImportPanel>> {
        self.import.as_ref().map(|(panel, _)| panel.clone())
    }

    /// Types into the address bar as the keyboard does.
    pub fn type_address(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::EntityInputHandler as _;
        self.address.update(cx, |input, cx| {
            input.focus(window, cx);
            input.replace_text_in_range(None, text, window, cx);
        });
    }

    /// The address bar with GPUI's focus and the window's keyboard, as after a click in it, and `text` typed there.
    pub fn focus_address(&mut self, text: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = content_view(window) {
            super::give_keys_to(view);
        }
        self.address.update(cx, |input, cx| {
            if let Some(text) = text {
                input.set_value(text.to_string(), window, cx);
            }
            input.focus(window, cx);
        });
    }

    pub fn address_value(&self, cx: &App) -> String {
        self.address.read(cx).value().to_string()
    }

    /// The suggestions' addresses, and the one picked.
    pub fn suggested(&self) -> (Vec<String>, Option<usize>) {
        (self.suggestions.iter().map(|place| place.url.clone()).collect(), self.picked)
    }

    pub fn pick_next(&mut self, cx: &mut Context<Self>) {
        self.move_pick(1, cx);
    }

    /// The usernames the login bar offers.
    pub fn offered(&self) -> Option<Vec<String>> {
        self.offer.as_ref().map(|offer| offer.usernames.clone())
    }

    pub fn fill(&mut self, username: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.fill_login(username, window, cx);
    }
}

// Extensions tab (the code keeps "connectors": Tab::Connectors): what Filo's agents can use. The list on the left has
// two pages, Apps and Skills (skills.rs). Apps are the user's MCP servers, connected once here and used by the coding tools
// in every chat and agent conversation (crates/filo-local `apps`): ready-made ones to add, and any local
// command or remote server. The Apps page has two views, Yours (what's connected) and Discover (what can be); an app's
// page (connector.rs), a ready-made app's page before it's connected (`render_preset`), the form that adds or edits one
// (connector_form.rs) and "Your computer" (computer_connector.rs) open in its place, with a way back to Apps.

use gpui::{
    AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, Corner, Entity, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button as MenuButton, ButtonRounded};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, Sizable as _};
use serde_json::{Value, json};

use super::computer_connector::{self, ComputerConnectorScreen};
use super::connector::{self, ConnectorEvent, ConnectorScreen};
use super::connector_form::{ConnectorForm, FormEvent, KeyForm};
use super::connectors_store::{self, AUTH_KEY, AUTH_NONE, AUTH_OAUTH, AppRow, AppsData, AppsStore, PresetRow, STDIO};
use super::skills::SkillsScreen;
use crate::backend;
use crate::theme::{ActiveTheme as _, HEADER_HEIGHT, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton, Tone};

const LIST_WIDTH: f32 = 200.;
/// Below this column width Discover's list stacks in one column.
const TWO_COLUMNS_FROM: f32 = 560.;
/// The Apps toolbar's controls, one height and one corner: the Yours / Discover track (28 inside 2 of padding), the
/// search field and Add.
const TOOLBAR_HEIGHT: f32 = 32.;
const TOOLBAR_RADIUS: f32 = radius::MD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Apps,
    Skills,
}

/// The Apps page's two views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    /// What's connected.
    Yours,
    /// Ready-made apps to connect.
    Discover,
}

/// Discover's pills: how an app connects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    All,
    Account,
    Key,
    Open,
    Local,
}

impl Category {
    const ALL: [Category; 5] = [Category::All, Category::Account, Category::Key, Category::Open, Category::Local];

    fn label(self) -> &'static str {
        match self {
            Category::All => "All",
            Category::Account => "Your account",
            Category::Key => "API key",
            Category::Open => "No sign-in",
            Category::Local => "On this Mac",
        }
    }

    fn holds(self, transport: &str, auth: &str) -> bool {
        match self {
            Category::All => true,
            Category::Account => auth == AUTH_OAUTH,
            Category::Key => auth == AUTH_KEY,
            Category::Open => auth == AUTH_NONE && transport != STDIO,
            Category::Local => transport == STDIO,
        }
    }
}

/// What's open in place of the Apps page.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Open {
    Computer,
    App(String),
    /// A ready-made app that isn't connected yet: what it is, and Connect.
    Preset(String),
    /// Advanced: the form for a server of the user's own, or to change an app's settings.
    Custom,
    Edit(String),
    /// A ready-made app that takes a key: only the key (connector_form.rs `KeyForm`).
    Key(String),
}

pub struct ConnectorsScreen {
    store: Entity<AppsStore>,
    page: Page,
    view: View,
    category: Category,
    search: Entity<InputState>,
    query: String,
    open: Option<Open>,
    detail: Option<AnyView>,
    /// Built while the Skills page shows, so its query runs only then.
    skills: Option<Entity<SkillsScreen>>,
    /// "Your computer" (computer_connector.rs): whether Filo may use this Mac's apps.
    computer_on: Option<bool>,
    _computer_sub: Task<()>,
    _detail_sub: Option<Subscription>,
    /// The preset whose Connect is under way (until its app is added).
    connecting: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectorsScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = connectors_store::store(cx);
        connectors_store::refresh(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search apps"));
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    connectors_store::refresh(cx);
                }
            }),
            cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        ];
        let computer_sub = backend::subscribe(
            cx,
            "computer:appsOn",
            json!({}),
            |this: &mut Self, result: Result<bool, String>, cx| {
                this.computer_on = result.ok();
                cx.notify();
            },
        );
        #[cfg_attr(not(feature = "dev"), allow(unused_mut))] // FILO_ABILITIES below
        let mut this = ConnectorsScreen {
            store,
            page: Page::Apps,
            view: View::Yours,
            category: Category::All,
            search,
            query: String::new(),
            open: None,
            detail: None,
            skills: None,
            computer_on: None,
            _computer_sub: computer_sub,
            _detail_sub: None,
            connecting: None,
            _subscriptions: subscriptions,
        };
        // `FILO_ABILITIES=skills|skill:<name>|skill:new|discover|computer|custom|key:<preset>|preset:<preset>|<app id>`:
        // starts there, for checking a page by eye (`dev` builds).
        #[cfg(feature = "dev")]
        match std::env::var("FILO_ABILITIES").ok().as_deref() {
            Some(page) if page.starts_with("skill") => {
                this.show_page(Page::Skills, window, cx);
                if let (Some(skills), Some(name)) = (&this.skills, page.strip_prefix("skill:")) {
                    skills.update(cx, |skills, _| skills.open_when_loaded(name.to_string()));
                }
            }
            Some("discover") => this.view = View::Discover,
            Some("computer") => this.show(Open::Computer, window, cx),
            Some("custom") => this.show(Open::Custom, window, cx),
            Some(page) if page.starts_with("key:") => {
                this.show(Open::Key(page["key:".len()..].to_string()), window, cx)
            }
            Some(page) if page.starts_with("preset:") => {
                this.show(Open::Preset(page["preset:".len()..].to_string()), window, cx)
            }
            Some(id) if !id.is_empty() => this.show(Open::App(id.to_string()), window, cx),
            _ => {}
        }
        this
    }

    /// The Skills page, while it's open (smoke test).
    #[cfg(feature = "dev")]
    pub fn skills(&self) -> Option<Entity<SkillsScreen>> {
        self.skills.clone()
    }

    /// Hidden under a pushed page or another tab (the shell calls this): a skill's page is a native web view.
    pub fn set_page_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if let Some(skills) = &self.skills {
            skills.update(cx, |skills, cx| skills.set_page_visible(visible, cx));
        }
    }

    pub fn show_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.open = None;
        self.detail = None;
        self._detail_sub = None;
        self.skills = match page {
            Page::Skills => Some(self.skills.take().unwrap_or_else(|| cx.new(|cx| SkillsScreen::new(window, cx)))),
            Page::Apps => None,
        };
        // A second click on Skills goes back to its list from a skill.
        if let Some(skills) = &self.skills {
            skills.update(cx, |skills, cx| skills.show_list(cx));
        }
        cx.notify();
    }

    fn show(&mut self, open: Open, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Apps;
        self.skills = None;
        if self.open.as_ref() == Some(&open) {
            return;
        }
        let (detail, subscription): (Option<AnyView>, Option<Subscription>) = match &open {
            Open::Computer => (Some(cx.new(|cx| ComputerConnectorScreen::new(window, cx)).into()), None),
            Open::App(id) => {
                let screen = cx.new(|cx| ConnectorScreen::new(id, window, cx));
                let subscription =
                    cx.subscribe_in(&screen, window, |this, _, event: &ConnectorEvent, window, cx| match event {
                        ConnectorEvent::Edit(id) => this.show(Open::Edit(id.clone()), window, cx),
                        ConnectorEvent::Removed => this.show_page(Page::Apps, window, cx),
                    });
                (Some(screen.into()), Some(subscription))
            }
            // Drawn here: it's only what the preset says, and Connect.
            Open::Preset(_) => (None, None),
            Open::Custom => {
                let form = cx.new(|cx| ConnectorForm::new(None, window, cx));
                (Some(form.clone().into()), Some(self.form_events(&form, window, cx)))
            }
            Open::Key(id) => {
                let Some(preset) = self.preset(id, cx) else { return };
                let form = cx.new(|cx| KeyForm::new(preset, window, cx));
                let subscription =
                    cx.subscribe_in(&form, window, |this, _, event: &FormEvent, window, cx| match event {
                        FormEvent::Saved(id) => this.show(Open::App(id.clone()), window, cx),
                        FormEvent::Cancelled(_) => this.show_page(Page::Apps, window, cx),
                    });
                (Some(form.into()), Some(subscription))
            }
            Open::Edit(id) => {
                let app = self.store.read(cx).app(id).cloned();
                let form = cx.new(|cx| ConnectorForm::new(app, window, cx));
                (Some(form.clone().into()), Some(self.form_events(&form, window, cx)))
            }
        };
        self.open = Some(open);
        self.detail = detail;
        self._detail_sub = subscription;
        cx.notify();
    }

    fn form_events(&self, form: &Entity<ConnectorForm>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(form, window, |this, _, event: &FormEvent, window, cx| match event {
            FormEvent::Saved(id) => this.show(Open::App(id.clone()), window, cx),
            FormEvent::Cancelled(Some(id)) => this.show(Open::App(id.clone()), window, cx),
            FormEvent::Cancelled(None) => this.show_page(Page::Apps, window, cx),
        })
    }

    fn preset(&self, id: &str, cx: &App) -> Option<PresetRow> {
        self.store
            .read(cx)
            .loaded()
            .and_then(|data| data.presets.iter().find(|preset| preset.id == id).cloned())
            .or_else(|| connectors_store::preset_row(id))
    }

    /// A ready-made app's Connect, like a directory's: an account one is added and its sign-in opens in the browser, one
    /// with no sign-in is added and checked; one with a key asks only for the key.
    fn connect_preset(&mut self, preset: PresetRow, window: &mut Window, cx: &mut Context<Self>) {
        if preset.auth == AUTH_KEY {
            return self.show(Open::Key(preset.id), window, cx);
        }
        self.call_connect_preset(&preset.id, window, cx);
    }

    fn call_connect_preset(&mut self, preset: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.connecting = Some(preset.to_string());
        cx.notify();
        let args = json!({ "preset": preset });
        backend::call_in(
            window,
            cx,
            "apps:connectPreset",
            args,
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.connecting = None;
                match result {
                    // Its page follows the browser sign-in or the check, then shows what it can use.
                    Ok(app) => {
                        if let Some(id) = app.get("id").and_then(Value::as_str) {
                            this.show(Open::App(id.to_string()), window, cx);
                        }
                    }
                    Err(error) => ui::toast_error(window, cx, "Couldn’t connect", error),
                }
                cx.notify();
            },
        );
    }

    /// Whether Filo may use this Mac's apps but macOS hasn't allowed Accessibility yet.
    fn computer_blocked(&self) -> bool {
        self.computer_on == Some(true) && !crate::computer_use::accessibility_allowed()
    }

    /// The search field's words, lowercased; empty matches everything.
    fn needle(&self) -> String {
        self.query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
    }

    // ---- The list on the left ----

    fn nav_row(
        &self,
        id: &'static str,
        icon: &'static str,
        label: &'static str,
        page: Page,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let active = self.page == page;
        list_item(SharedString::from(id), active, cx)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show_page(page, window, cx)))
            .child(ui::icon_sized(icon, 16., if active { colors.text } else { colors.text_secondary }))
            .child(div().flex_1().min_w_0().callout().medium().truncate().child(label))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> gpui::Div {
        let colors = *cx.colors();
        let items = vec![
            self.nav_row("abilities-apps", "layout-grid", "Apps", Page::Apps, cx),
            self.nav_row("abilities-skills", "skills", "Skills", Page::Skills, cx),
        ];
        div()
            .w(px(LIST_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(colors.surface_strong)
            .child(
                crate::chrome::drag_region(div().id("connectors-list-header"))
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .px(px(space::LG))
                    .flex()
                    .items_center()
                    .child(div().title3().bold().child("Extensions")),
            )
            .child(
                div()
                    .id("connectors-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(space::SM))
                    .pb(px(space::MD))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(items),
            )
    }

    // ---- The Apps page: title, toolbar, and Yours or Discover ----

    /// Yours / Discover, with how many are connected on Yours.
    fn view_switch(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let option = |view: View, label: &'static str, count: Option<usize>, cx: &mut Context<Self>| {
            let selected = self.view == view;
            div()
                .id(SharedString::from(format!("apps-view-{label}")))
                .min_h(px(28.))
                .px(px(space::MD))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(radius::SM + 2.))
                .footnote()
                .cursor_pointer()
                .map(|this| {
                    if selected {
                        this.bg(colors.raised).border_1().border_color(colors.border).semibold().text_color(colors.text)
                    } else {
                        this.medium().text_color(colors.text_secondary).hover(|style| style.text_color(colors.text))
                    }
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.view = view;
                    cx.notify();
                }))
                .child(label)
                .when_some(count, |this, count| {
                    this.child(
                        div()
                            .min_w(px(18.))
                            .h(px(16.))
                            .px(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(radius::FULL))
                            .bg(if selected { colors.accent_soft } else { colors.background })
                            .text_color(if selected { colors.on_accent_soft } else { colors.text_secondary })
                            .micro()
                            .semibold()
                            .child(count.to_string()),
                    )
                })
        };
        div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(TOOLBAR_RADIUS))
            .bg(colors.surface_strong)
            .child(option(View::Yours, "Yours", Some(count), cx))
            .child(option(View::Discover, "Discover", None, cx))
            .into_any_element()
    }

    /// Add: a server of the user's own, by its address or a command on this Mac.
    fn add_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        MenuButton::new("apps-add")
            .outline()
            .small()
            .h(px(TOOLBAR_HEIGHT))
            .rounded(ButtonRounded::Size(px(TOOLBAR_RADIUS)))
            .icon(Icon::empty().path("icons/plus.svg"))
            .label("Add")
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
                let entity = entity.clone();
                menu.item(PopupMenuItem::new("Custom MCP server…").icon(Icon::empty().path("icons/plug.svg")).on_click(
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| this.show(Open::Custom, window, cx)).ok();
                    },
                ))
            })
            .into_any_element()
    }

    fn toolbar(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        // `Styled::h`: Input's own `h` sizes only a multi-line input.
        let search = gpui::Styled::h(Input::new(&self.search).small(), px(TOOLBAR_HEIGHT))
            .rounded(px(TOOLBAR_RADIUS))
            .cleanable(true)
            .prefix(ui::icon_sized("search", 14., colors.text_secondary));
        div()
            .pt(px(space::XL))
            .pb(px(space::MD))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(self.view_switch(count, cx))
            .child(div().flex_1())
            // 240 wide when there's room, narrower when there isn't: Add stays on the page.
            .child(div().w(px(240.)).min_w_0().child(search))
            .child(self.add_menu(cx))
            .into_any_element()
    }

    /// A row of Yours: icon, name with how it connects, a line about it, and how many of its tools are on.
    fn your_row(
        &self,
        id: SharedString,
        icon: AnyElement,
        name: SharedString,
        tag: &'static str,
        line: AnyElement,
        trailing: AnyElement,
        cx: &App,
    ) -> Stateful<gpui::Div> {
        let colors = *cx.colors();
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(space::MD))
            .min_h(px(60.))
            .px(px(space::SM))
            .py(px(space::SM))
            .rounded(px(radius::MD))
            .cursor_pointer()
            .hover(|style| style.bg(colors.selected_fill.opacity(0.5)))
            .child(icon_tile_sized(icon, 36., cx))
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
                            .child(div().min_w_0().callout().medium().truncate().child(name))
                            .child(ui::chip(tag, Tone::Neutral, true, cx)),
                    )
                    .child(line),
            )
            .child(div().flex_none().flex().items_center().gap(px(space::SM)).child(trailing).child(ui::icon_sized(
                "chevron-right",
                14.,
                colors.text_tertiary,
            )))
    }

    fn computer_your_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let trailing = if self.computer_blocked() {
            div().footnote().text_color(colors.warning).child("Needs Accessibility")
        } else {
            div().footnote().text_color(colors.text_tertiary).child("On")
        };
        let line = div().footnote().text_color(colors.text_secondary).truncate().child("Use the apps on this Mac");
        self.your_row(
            "apps-yours-computer".into(),
            ui::icon_sized("laptop", 18., colors.accent).into_any_element(),
            computer_connector::NAME.into(),
            "This Mac",
            line.into_any_element(),
            trailing.into_any_element(),
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show(Open::Computer, window, cx)))
        .into_any_element()
    }

    fn app_your_row(&self, app: &AppRow, data: &AppsData, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let id = app.id.clone();
        let (status, tone) = app.status_line();
        // What it is, unless it needs the user: then what it needs.
        let line = if app.needs_attention() || app.busy.is_some() {
            div().footnote().truncate().text_color(tone.color(cx)).child(status)
        } else {
            div().footnote().truncate().text_color(colors.text_secondary).child(app_description(app, data))
        };
        let trailing: AnyElement = if app.busy.is_some() {
            ui::spinner(14., cx).into_any_element()
        } else if !app.enabled {
            div().footnote().text_color(colors.text_tertiary).child("Off").into_any_element()
        } else if app.needs_attention() {
            ui::icon_sized("triangle-alert", 14., colors.warning).into_any_element()
        } else if app.tools.is_empty() {
            div().into_any_element()
        } else {
            div()
                .footnote()
                .text_color(colors.text_tertiary)
                .child(format!("{} of {} tools", app.allowed_tools.len(), app.tools.len()))
                .into_any_element()
        };
        self.your_row(
            SharedString::from(format!("apps-yours-{id}")),
            connectors_store::app_icon(app.preset.as_deref(), app.is_local(), 18., cx),
            app.name.clone().into(),
            connects_tag(&app.transport, &app.auth),
            line.into_any_element(),
            trailing,
            cx,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show(Open::App(id.clone()), window, cx)))
        .into_any_element()
    }

    fn render_yours(&self, data: &AppsData, loaded: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let needle = self.needle();
        let computer = self.computer_on == Some(true)
            && (needle.is_empty() || matches(&needle, computer_connector::NAME, "Use the apps on this Mac"));
        let apps: Vec<&AppRow> = data
            .apps
            .iter()
            .filter(|app| needle.is_empty() || matches(&needle, &app.name, &app_description(app, data)))
            .collect();
        if !computer && apps.is_empty() {
            if !loaded {
                return ui::skeleton_rows(3, Some(36.), cx).into_any_element();
            }
            let (title, message) = if needle.is_empty() {
                (
                    "No apps connected yet",
                    "Connect one from Discover, and your coding tools can use it in your chats and agents.",
                )
            } else {
                ("Nothing matches", "None of your apps match your search.")
            };
            return div()
                .py(px(space::HUGE))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::SM))
                .text_center()
                .child(div().headline().child(title))
                .child(div().max_w(px(360.)).subhead().text_color(colors.text_secondary).child(message))
                .when(needle.is_empty(), |this| {
                    this.child(div().pt(px(space::SM)).child(
                        Button::secondary("apps-yours-discover", "Browse apps").small().on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.view = View::Discover;
                                cx.notify();
                            },
                        )),
                    ))
                })
                .into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        if computer {
            rows.push(self.computer_your_row(cx));
        }
        for app in apps {
            rows.push(self.app_your_row(app, data, cx));
        }
        div().flex().flex_col().gap(px(space::XXS)).children(rows).into_any_element()
    }

    /// A pill over Discover's list.
    fn category_pill(&self, category: Category, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let selected = self.category == category;
        div()
            .id(SharedString::from(format!("apps-category-{}", category.label())))
            .px(px(space::MD))
            .min_h(px(28.))
            .flex()
            .items_center()
            .rounded(px(radius::FULL))
            .cursor_pointer()
            .footnote()
            .map(|this| {
                if selected {
                    this.bg(colors.selected_fill).semibold().text_color(colors.text)
                } else {
                    this.border_1()
                        .border_color(colors.border)
                        .medium()
                        .text_color(colors.text_secondary)
                        .hover(|style| style.text_color(colors.text))
                }
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.category = category;
                cx.notify();
            }))
            .child(category.label())
            .into_any_element()
    }

    /// A row of Discover: icon, name and a line, and + (a check once it's connected).
    fn discover_row(
        &self,
        id: SharedString,
        icon: AnyElement,
        name: SharedString,
        line: SharedString,
        action: AnyElement,
        cx: &App,
    ) -> Stateful<gpui::Div> {
        let colors = *cx.colors();
        div()
            .id(id)
            .w_full()
            .flex()
            .items_center()
            .gap(px(space::MD))
            .min_h(px(56.))
            .px(px(space::SM))
            .py(px(space::SM))
            .rounded(px(radius::MD))
            .cursor_pointer()
            .hover(|style| style.bg(colors.selected_fill.opacity(0.5)))
            .child(icon_tile_sized(icon, 36., cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().callout().medium().truncate().child(name))
                    .child(div().footnote().text_color(colors.text_secondary).truncate().child(line)),
            )
            .child(div().flex_none().size(px(28.)).flex().items_center().justify_center().child(action))
    }

    fn computer_discover_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let action: AnyElement = match self.computer_on {
            Some(true) => connected_mark(cx),
            _ => IconButton::new("apps-discover-add-computer", "plus")
                .small()
                .tooltip("Turn on")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.show(Open::Computer, window, cx);
                }))
                .into_any_element(),
        };
        self.discover_row(
            "apps-discover-computer".into(),
            ui::icon_sized("laptop", 18., colors.accent).into_any_element(),
            computer_connector::NAME.into(),
            "Use the apps on this Mac".into(),
            action,
            cx,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show(Open::Computer, window, cx)))
        .into_any_element()
    }

    fn preset_discover_row(&self, preset: &PresetRow, data: &AppsData, cx: &mut Context<Self>) -> AnyElement {
        let id = preset.id.clone();
        let connected =
            data.apps.iter().find(|app| app.preset.as_deref() == Some(id.as_str())).map(|app| app.id.clone());
        let action: AnyElement = if self.connecting.as_deref() == Some(id.as_str()) {
            ui::spinner(14., cx).into_any_element()
        } else if connected.is_some() {
            connected_mark(cx)
        } else {
            let row = preset.clone();
            IconButton::new(SharedString::from(format!("apps-discover-add-{id}")), "plus")
                .small()
                .tooltip(format!("Connect {}", preset.name))
                .disabled(self.connecting.is_some())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.connect_preset(row.clone(), window, cx);
                }))
                .into_any_element()
        };
        let open = match connected {
            Some(app) => Open::App(app),
            None => Open::Preset(id.clone()),
        };
        self.discover_row(
            SharedString::from(format!("apps-discover-{id}")),
            connectors_store::app_icon(Some(&preset.id), preset.transport == STDIO, 18., cx),
            preset.name.clone().into(),
            preset.description.clone().into(),
            action,
            cx,
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.show(open.clone(), window, cx)))
        .into_any_element()
    }

    fn render_discover(&self, data: &AppsData, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let needle = self.needle();
        let mut rows: Vec<AnyElement> = Vec::new();
        if self.category.holds(STDIO, AUTH_NONE)
            && (needle.is_empty() || matches(&needle, computer_connector::NAME, "Use the apps on this Mac"))
        {
            rows.push(self.computer_discover_row(cx));
        }
        for preset in
            discover_presets(data).iter().filter(|preset| self.category.holds(&preset.transport, &preset.auth))
        {
            if needle.is_empty() || matches(&needle, &preset.name, &preset.description) {
                rows.push(self.preset_discover_row(preset, data, cx));
            }
        }
        let pills = div()
            .flex()
            .flex_wrap()
            .gap(px(space::XS))
            .pb(px(space::MD))
            .children(Category::ALL.into_iter().map(|category| self.category_pill(category, cx)));
        let list = if rows.is_empty() {
            div()
                .py(px(space::XXL))
                .subhead()
                .text_color(colors.text_secondary)
                .text_center()
                .child("No apps match.")
                .into_any_element()
        } else {
            grid(rows, columns_for_pane())
        };
        div().flex().flex_col().child(pills).child(list).into_any_element()
    }

    fn render_apps(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let store = self.store.read(cx);
        let error = match &store.data {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        };
        let loaded = store.data.is_some() && self.computer_on.is_some();
        let data = store.loaded().cloned().unwrap_or_default();
        let count = data.apps.len() + usize::from(self.computer_on == Some(true));
        let body = match self.view {
            View::Yours => self.render_yours(&data, loaded, cx),
            View::Discover => self.render_discover(&data, cx),
        };
        pane(
            pane_bar("abilities-apps-bar", None),
            div().id("abilities-apps-scroll").size_full().overflow_y_scroll().when_some(crate::smoke::page_scroll("extensions"), |this, handle| this.track_scroll(&handle)).child(crate::smoke::tracked(
                "extensions-end",
                ui::page_column()
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .child(page_title("Apps", "Connect an app once, and your coding tools can use it in your chats and agents. Keys and sign-ins stay in your Keychain.", Vec::new(), cx))
                    .child(self.toolbar(count, cx))
                    .when_some(error, |this, error| this.child(div().pb(px(space::MD)).subhead().text_color(colors.danger).child(error)))
                    .child(body)
                    .children(crate::smoke::page_end("extensions")),
            )),
        )
    }

    /// A ready-made app before it's connected: what it is, how it connects, and Connect.
    fn render_preset(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let Some(preset) = self.preset(id, cx) else {
            return ui::empty_state("plug", "There’s no such app", None, None, cx).into_any_element();
        };
        let connecting = self.connecting.as_deref() == Some(preset.id.as_str());
        let action: AnyElement = if connecting {
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(ui::spinner(14., cx))
                .child(div().footnote().text_color(colors.text_secondary).child("Connecting…"))
                .into_any_element()
        } else {
            let row = preset.clone();
            Button::new("apps-preset-connect", "Connect")
                .disabled(self.connecting.is_some())
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| this.connect_preset(row.clone(), window, cx)),
                )
                .into_any_element()
        };
        let how = match (preset.transport.as_str(), preset.auth.as_str()) {
            (STDIO, _) => "Runs on this Mac".to_string(),
            (_, AUTH_OAUTH) => "Signs in with your account in the browser".to_string(),
            (_, AUTH_KEY) => "Takes an API key from the service".to_string(),
            _ => "No sign-in needed".to_string(),
        };
        let header = connector::app_header(
            connectors_store::app_icon(Some(&preset.id), preset.transport == STDIO, 30., cx),
            preset.name.clone().into(),
            preset.description.clone().into(),
            Some(div().subhead().text_color(colors.text_secondary).child(how.clone()).into_any_element()),
            action,
            cx,
        );
        let place = if preset.transport == STDIO {
            connectors_store::command_line(&preset.command, &preset.args)
        } else {
            preset.url.clone()
        };
        let mut about = vec![
            ui::ListRow::new("apps-preset-place", if preset.transport == STDIO { "Command" } else { "Server URL" })
                .subtitle(place)
                .into_any_element(),
            ui::ListRow::new("apps-preset-sign-in", "Sign-in").subtitle(how).into_any_element(),
        ];
        if !preset.needs.is_empty() {
            about.push(
                ui::ListRow::new("apps-preset-needs", "Needs")
                    .subtitle(format!("`{}` on this Mac", preset.needs))
                    .into_any_element(),
            );
        }
        let note: Option<SharedString> = (!preset.note.is_empty()).then(|| preset.note.clone().into());
        div()
            .id("apps-preset-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                ui::page_column()
                    .pt(px(space::SM))
                    .pb(px(48.))
                    .child(header)
                    .child(div().pt(px(space::XL)).child(connector::empty_tools("Its tools show once it’s connected. Every tool starts allowed; you can block any of them then.", cx)))
                    .child(div().pt(px(space::XL)).child(ui::section(Some("About"), note, about, cx))),
            )
            .into_any_element()
    }

    fn render_detail(&self, detail: Option<AnyView>, cx: &mut Context<Self>) -> AnyElement {
        let back = back_link(
            "abilities-detail-back",
            "Apps",
            cx.listener(|this, _: &ClickEvent, window, cx| this.show_page(Page::Apps, window, cx)),
            cx,
        );
        let body = match (&self.open, detail) {
            (Some(Open::Preset(id)), _) => self.render_preset(&id.clone(), cx),
            (_, Some(detail)) => detail.into_any_element(),
            _ => div().into_any_element(),
        };
        pane(pane_bar("abilities-detail-bar", Some(back)), body)
    }
}

/// How an app connects, in a word or two (Yours' tag).
fn connects_tag(transport: &str, auth: &str) -> &'static str {
    match (transport, auth) {
        (STDIO, _) => "On this Mac",
        (_, AUTH_OAUTH) => "Account",
        (_, AUTH_KEY) => "API key",
        _ => "No sign-in",
    }
}

/// Discover's ready-made apps, in their own order: those offered, and those already added (`apps:list` leaves those
/// out), which show a check.
fn discover_presets(data: &AppsData) -> Vec<PresetRow> {
    filo_local::apps::PRESETS
        .iter()
        .filter_map(|preset| match data.presets.iter().find(|row| row.id == preset.id) {
            Some(row) => Some(row.clone()),
            None if data.apps.iter().any(|app| app.preset.as_deref() == Some(preset.id)) => {
                connectors_store::preset_row(preset.id)
            }
            None => None,
        })
        .collect()
}

/// What an app is: its preset's line, else where it runs.
fn app_description(app: &AppRow, data: &AppsData) -> String {
    app.preset
        .as_deref()
        .and_then(|preset| {
            data.presets.iter().find(|row| row.id == preset).cloned().or_else(|| connectors_store::preset_row(preset))
        })
        .map(|preset| preset.description)
        .filter(|description| !description.is_empty())
        .unwrap_or_else(|| app.place())
}

fn matches(needle: &str, name: &str, description: &str) -> bool {
    name.to_lowercase().contains(needle) || description.to_lowercase().contains(needle)
}

/// A row in the list on the left: highlighted while its page is open.
fn list_item(id: SharedString, active: bool, cx: &App) -> Stateful<gpui::Div> {
    let colors = *cx.colors();
    // The open one still answers the pointer, a shade stronger than its highlight.
    let mut selected_hover = colors.selected_fill;
    selected_hover.a = (selected_hover.a * 1.6).min(1.);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(space::SM + 2.))
        .min_h(px(34.))
        .px(px(space::MD))
        .rounded(px(radius::MD))
        .cursor_pointer()
        .when(active, |this| this.bg(colors.selected_fill).hover(move |style| style.bg(selected_hover)))
        .when(!active, |this| this.hover(|style| style.bg(colors.selected_fill.opacity(0.5))))
}

/// Connected, or on: a check in the success colour.
fn connected_mark(cx: &App) -> AnyElement {
    div().px(px(space::SM)).child(ui::icon_sized("check", 16., cx.colors().success)).into_any_element()
}

/// How many columns of cards (Skills) or Discover's rows fit beside the list.
pub fn columns_for_pane() -> usize {
    if ui::column_width_in(ui::content_width() - LIST_WIDTH) >= TWO_COLUMNS_FROM { 2 } else { 1 }
}

/// Cards in rows of `columns`, the last row's gap left empty.
pub fn grid(cards: Vec<AnyElement>, columns: usize) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut cards = cards.into_iter().peekable();
    while cards.peek().is_some() {
        let row: Vec<AnyElement> = cards.by_ref().take(columns).collect();
        let missing = columns - row.len();
        rows.push(
            div()
                .flex()
                .gap(px(space::LG))
                .children(row.into_iter().map(|card| div().flex_1().min_w_0().child(card)))
                .children((0..missing).map(|_| div().flex_1()))
                .into_any_element(),
        );
    }
    div().flex().flex_col().gap(px(space::XXS)).children(rows).into_any_element()
}

/// One app or skill on a page: its icon on a tile, its name and a line about it, and what it offers at the end.
pub fn card(
    id: impl Into<SharedString>,
    icon: AnyElement,
    title: impl Into<SharedString>,
    subtitle: SharedString,
    action: Option<AnyElement>,
    cx: &App,
) -> Stateful<gpui::Div> {
    let line = div().footnote().text_color(cx.colors().text_secondary).truncate().child(subtitle).into_any_element();
    card_with(id, icon, title, line, action, cx)
}

/// `card` with its line drawn by the caller (an app's status, in its colour).
pub fn card_with(
    id: impl Into<SharedString>,
    icon: AnyElement,
    title: impl Into<SharedString>,
    subtitle: AnyElement,
    action: Option<AnyElement>,
    cx: &App,
) -> Stateful<gpui::Div> {
    let colors = *cx.colors();
    div()
        .id(gpui::ElementId::Name(id.into()))
        .w_full()
        .flex()
        .items_center()
        .gap(px(space::MD))
        .min_h(px(64.))
        .px(px(space::SM))
        .py(px(space::SM))
        .rounded(px(radius::MD))
        .cursor_pointer()
        .hover(|style| style.bg(colors.selected_fill.opacity(0.5)))
        .child(icon_tile_sized(icon, 40., cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().callout().medium().truncate().child(title.into()))
                .child(subtitle),
        )
        .when_some(action, |this, action| this.child(div().flex_none().child(action)))
}

/// A page's title, the line under it, and its buttons at the end of the title's line.
pub fn page_title(title: &'static str, subtitle: &'static str, actions: Vec<AnyElement>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    div()
        .pt(px(space::SM))
        .flex()
        .flex_col()
        .gap(px(space::XS))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(div().flex_1().min_w_0().title1().child(title))
                .children(actions),
        )
        .child(div().subhead().text_color(colors.text_secondary).child(subtitle))
        .into_any_element()
}

/// The bar over the right side: drags the window, and holds the way back from an app or skill.
pub fn pane_bar(id: &'static str, crumbs: Option<AnyElement>) -> Stateful<gpui::Div> {
    crate::chrome::drag_region(div().id(id))
        .flex_none()
        .h(px(HEADER_HEIGHT))
        .px(px(space::LG))
        .flex()
        .items_center()
        .children(crumbs)
}

/// A page of the right side: `bar` on top and `body` filling the rest. The body sits at fixed edges: a box stretched
/// in a flex column gives what's in it no definite width, so text didn't wrap and rows pushed their switches and
/// buttons past the card.
pub fn pane(bar: impl IntoElement, body: impl IntoElement) -> AnyElement {
    div()
        .size_full()
        .relative()
        .child(bar)
        .child(div().absolute().top(px(HEADER_HEIGHT)).left_0().right_0().bottom_0().child(body))
        .into_any_element()
}

/// "‹ Apps": back to the page an app's page opened from, in the bar over it.
pub fn back_link(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(space::XXS))
        .pl(px(space::XXS))
        .pr(px(space::SM))
        .py(px(space::XXS))
        .rounded(px(radius::SM))
        .cursor_pointer()
        .subhead()
        .medium()
        .text_color(colors.text_secondary)
        .hover(move |style| style.text_color(colors.text).bg(colors.selected_fill.opacity(0.5)))
        // Not a drag of the window: the bar under it drags on mouse down.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(ui::icon_sized("chevron-left", 16., colors.text_secondary))
        .child(label)
        .into_any_element()
}

/// "Skills › Weekly report": the first goes back to its page.
pub fn breadcrumb(
    id: &'static str,
    parent: &'static str,
    current: SharedString,
    on_parent: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    div()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .min_w_0()
        .subhead()
        .child(
            div()
                .id(id)
                .flex_none()
                .px(px(space::XS))
                .rounded(px(radius::SM))
                .cursor_pointer()
                .text_color(colors.text_secondary)
                .hover(move |style| style.text_color(colors.text))
                // Not a drag of the window: the bar under it drags on mouse down.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(on_parent)
                .child(parent),
        )
        .child(ui::icon_sized("chevron-right", 14., colors.text_tertiary))
        .child(div().min_w_0().truncate().medium().child(current))
        .into_any_element()
}

/// The tile behind a connector's brand icon.
pub fn icon_tile_sized(icon: impl IntoElement, size: f32, cx: &App) -> gpui::Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(if size >= 48. {
            radius::LG
        } else if size >= 40. {
            radius::MD
        } else {
            radius::SM
        }))
        .bg(cx.colors().accent_soft)
        .flex()
        .items_center()
        .justify_center()
        .child(icon)
}

impl Render for ConnectorsScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let pane: AnyElement = match (self.page, self.open.is_some(), self.skills.clone()) {
            (Page::Skills, _, Some(skills)) => skills.into_any_element(),
            (_, true, _) => self.render_detail(self.detail.clone(), cx),
            _ => self.render_apps(cx),
        };
        div()
            .size_full()
            .flex()
            .bg(colors.background)
            .child(self.render_list(cx))
            .child(div().flex_1().min_w_0().h_full().child(pane))
    }
}

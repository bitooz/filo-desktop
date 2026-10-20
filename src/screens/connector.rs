// An app's page (Extensions › Apps › <name>): a header with what it is and Connect or Disconnect, which of its tools
// the agents may use, its account, and about it; under Advanced, the app's own switch, which agents of the Agents tab
// may use it and which coding tools may run it. Tools are grouped into those that only read and those
// that change things. Each is Allow or Block (an app's tool is on or off for the agents): nobody can approve a step
// while a turn runs. Every tool of an app connected now starts allowed, until the user blocks it here; on an app
// connected before that, only read-only tools did, and any other stays blocked until the user allows it. The Extensions
// tab shows it beside its list.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, canvas, div, prelude::FluentBuilder as _, px,
};
use gpui_component::Disableable as _;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use serde_json::{Value, json};

use super::connectors::icon_tile_sized;
use super::connectors_store::{self, AUTH_KEY, AUTH_OAUTH, AppRow, AppTool, AppsStore};
use crate::backend;
use crate::theme::{ActiveTheme as _, TypeVariant, Typography as _, radius, space};
use crate::ui::{self, Button, ListRow, Tone};

/// An app with more tools than this gets a search field over its list.
const SEARCH_FROM: usize = 6;

pub enum ConnectorEvent {
    Edit(String),
    Removed,
}

/// What the agents may do with a tool: Allow is on, Block is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Allow,
    Block,
}

/// The two groups of an app's tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    ReadOnly,
    Changes,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Group::ReadOnly => "Read-only",
            Group::Changes => "Changes things",
        }
    }
}

/// Whether `tool` only reads: the server says so, or it's one Filo checked for the ready-made app.
fn reads_only(app: &AppRow, tool: &AppTool) -> bool {
    tool.read_only || app.read_tools.iter().any(|name| name == &tool.name)
}

pub struct ConnectorScreen {
    id: String,
    store: Entity<AppsStore>,
    /// A switch's new value while its change is saved (`agent:<id>`, `agents:all`, `engine:<engine>`,
    /// `tool:<name>`, `enabled`).
    saving: HashMap<String, bool>,
    /// The Advanced part is open: where it connects, how it signs in, the engines, Check again and Edit.
    advanced: bool,
    search: Entity<InputState>,
    api_key: Entity<InputState>,
    query: String,
    placeholder_count: usize,
    /// The width the tool rows' one-line texts had room for when last drawn, by key (`probe`).
    room: Rc<RefCell<HashMap<String, Pixels>>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ConnectorEvent> for ConnectorScreen {}

impl ConnectorScreen {
    /// The page of app `id`, in the Extensions tab: the name in the header, and Edit (the tab opens the form).
    pub fn new(id: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = connectors_store::store(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search tools"));
        let api_key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("API key")
                .masked(true)
                .validate(|text, _| text.chars().count() <= 8192)
        });
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        ];
        ConnectorScreen {
            id: id.to_string(),
            store,
            saving: HashMap::new(),
            advanced: false,
            search,
            api_key,
            query: String::new(),
            placeholder_count: 0,
            room: Rc::default(),
            _subscriptions: subscriptions,
        }
    }

    fn app(&self, cx: &gpui::App) -> Option<AppRow> {
        self.store.read(cx).app(&self.id).cloned()
    }

    /// Runs one of the apps' functions on this app; a failure shows as a toast.
    fn act(
        &mut self,
        path: &'static str,
        extra: Value,
        failure: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut args = json!({ "id": self.id });
        if let (Some(args), Value::Object(extra)) = (args.as_object_mut(), extra) {
            args.extend(extra);
        }
        backend::call_in(window, cx, path, args, move |_: &mut Self, result: Result<Value, String>, window, cx| {
            if let Err(error) = result {
                ui::toast_error(window, cx, failure, error);
            }
        });
    }

    /// A switch: shown at its new value while the change saves.
    fn switch(
        &mut self,
        key: String,
        path: &'static str,
        extra: Value,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving.insert(key.clone(), on);
        cx.notify();
        let mut args = json!({ "id": self.id, "enabled": on });
        if let (Some(args), Value::Object(extra)) = (args.as_object_mut(), extra) {
            args.extend(extra);
        }
        backend::call_in(window, cx, path, args, move |this: &mut Self, result: Result<Value, String>, window, cx| {
            this.saving.remove(&key);
            if let Err(error) = result {
                ui::toast_error(window, cx, "Couldn’t change it", error);
            }
            cx.notify();
        });
    }

    /// A tool allowed that can change things asks first: nothing will ask again while a turn uses it.
    fn set_tool(&mut self, app: &AppRow, tool: &AppTool, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let key = format!("tool:{}", tool.name);
        let name = tool.name.clone();
        if !on || reads_only(app, tool) {
            return self.switch(key, "apps:setToolEnabled", json!({ "tool": name }), on, window, cx);
        }
        let what = match (tool.marked, tool.destructive) {
            (false, _) => "The server doesn’t say it only reads: it may change or delete things",
            (true, true) => "It can change or delete things",
            (true, false) => "It can change things",
        };
        let reach = if tool.open_world { ", and reach people or places outside the app" } else { "" };
        let this = cx.entity();
        ui::confirm(
            window,
            cx,
            format!("Let agents use “{}”?", tool.label()),
            format!(
                "{what} in {}{reach}. Your coding tools will use it whenever they decide to, without asking you first, in every chat and agent this app is on for.",
                app.name
            ),
            "Turn on",
            "Cancel",
            false,
            move |window, cx| {
                let name = name.clone();
                this.update(cx, |this, cx| {
                    this.switch(key.clone(), "apps:setToolEnabled", json!({ "tool": name }), true, window, cx)
                });
            },
        );
    }

    /// A whole group allowed or blocked at once; allowing tools that change things asks first, once for the group.
    fn set_group(
        &mut self,
        app: &AppRow,
        group: Group,
        names: Vec<String>,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if names.is_empty() {
            return;
        }
        if !on || group == Group::ReadOnly {
            return self.switch_tools(names, on, window, cx);
        }
        let this = cx.entity();
        ui::confirm(
            window,
            cx,
            format!("Allow {} tools that change things?", names.len()),
            format!(
                "They can change, delete or send things in {}. Your coding tools will use them whenever they decide to, without asking you first, in every chat and agent this app is on for.",
                app.name
            ),
            "Allow all",
            "Cancel",
            false,
            move |window, cx| {
                let names = names.clone();
                this.update(cx, |this, cx| this.switch_tools(names, true, window, cx));
            },
        );
    }

    /// Every tool of the app allowed or blocked at once; allowing asks first, once, when it would turn on tools that
    /// change things.
    fn set_all(&mut self, app: &AppRow, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = app.tools.iter().map(|tool| tool.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let changing = app.tools.iter().filter(|tool| !reads_only(app, tool) && !self.tool_on(app, tool)).count();
        if !on || changing == 0 {
            return self.switch_tools(names, on, window, cx);
        }
        let this = cx.entity();
        ui::confirm(
            window,
            cx,
            format!("Allow all {} tools?", names.len()),
            format!(
                "{changing} of them can change, delete or send things in {}. Your coding tools will use them whenever they decide to, without asking you first, in every chat and agent this app is on for.",
                app.name
            ),
            "Allow all",
            "Cancel",
            false,
            move |window, cx| {
                let names = names.clone();
                this.update(cx, |this, cx| this.switch_tools(names, true, window, cx));
            },
        );
    }

    /// Several tools on or off in one change, each shown at its new value while it saves.
    fn switch_tools(&mut self, names: Vec<String>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = names.iter().map(|name| format!("tool:{name}")).collect();
        for key in &keys {
            self.saving.insert(key.clone(), on);
        }
        cx.notify();
        let args = json!({ "id": self.id, "tools": names, "enabled": on });
        backend::call_in(
            window,
            cx,
            "apps:setToolsEnabled",
            args,
            move |this: &mut Self, result: Result<Value, String>, window, cx| {
                for key in &keys {
                    this.saving.remove(key);
                }
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t change them", error);
                }
                cx.notify();
            },
        );
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).value().to_string();
        if key.trim().is_empty() {
            return;
        }
        self.api_key.update(cx, |state, cx| state.set_value("", window, cx));
        self.act("apps:setApiKey", json!({ "apiKey": key }), "Couldn’t save the key", window, cx);
    }

    fn remove(&mut self, app: &AppRow, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity();
        let id = app.id.clone();
        ui::confirm(
            window,
            cx,
            format!("Disconnect {}?", app.name),
            "It’s removed: your coding tools stop getting it, and its key or sign-in is deleted from your Keychain.",
            "Disconnect",
            "Cancel",
            true,
            move |window, cx| {
                let id = id.clone();
                this.update(cx, |_, cx| {
                    backend::call_in(
                        window,
                        cx,
                        "apps:remove",
                        json!({ "id": id }),
                        |_: &mut Self, result: Result<Value, String>, window, cx| match result {
                            Ok(_) => cx.emit(ConnectorEvent::Removed),
                            Err(error) => ui::toast_error(window, cx, "Couldn’t remove it", error),
                        },
                    );
                });
            },
        );
    }

    /// The header's buttons: Connect while it needs a sign-in (or Try again), what's under way, and Disconnect.
    fn render_actions(&self, app: &AppRow, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let mut buttons: Vec<AnyElement> = Vec::new();
        if app.signing_in() {
            buttons.push(ui::spinner(14., cx).into_any_element());
            buttons.push(
                div()
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child("Approve it in your browser…")
                    .into_any_element(),
            );
            buttons.push(
                Button::new("app-cancel", "Cancel")
                    .variant(ui::Variant::Quiet)
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.act("apps:cancelConnect", json!({}), "Couldn’t cancel", window, cx)
                    }))
                    .into_any_element(),
            );
        } else if app.busy.is_some() {
            buttons.push(ui::spinner(14., cx).into_any_element());
            buttons.push(div().footnote().text_color(colors.text_secondary).child("Connecting…").into_any_element());
        } else if app.auth == AUTH_OAUTH && (!app.has_secret || app.status == connectors_store::NEEDS_SIGN_IN) {
            buttons.push(
                Button::new("app-connect", "Connect")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.act("apps:connect", json!({}), "Couldn’t connect", window, cx)
                    }))
                    .into_any_element(),
            );
        } else if app.status == connectors_store::FAILED && app.credentials_ready() {
            buttons.push(
                Button::new("app-retry", "Try again")
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.act("apps:connect", json!({}), "Couldn’t connect", window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let row = app.clone();
        buttons.push(
            Button::secondary("app-remove", "Disconnect")
                .small()
                .on_click(cx.listener(move |this, _, window, cx| this.remove(&row, window, cx)))
                .into_any_element(),
        );
        div().flex().flex_wrap().justify_end().items_center().gap(px(space::SM)).children(buttons).into_any_element()
    }

    /// About: what it is, where it connects and its name for the agents, with Edit settings (the Extensions tab opens
    /// the form).
    fn render_about(&self, app: &AppRow, cx: &mut Context<Self>) -> AnyElement {
        let description = self.preset_description(app, cx);
        let mut rows = Vec::new();
        if let Some(description) = description {
            rows.push(ListRow::new("app-about", app.name.clone()).subtitle(description).into_any_element());
        }
        rows.push(
            ListRow::new("app-place", if app.is_local() { "Command" } else { "Server URL" })
                .subtitle(if app.is_local() {
                    connectors_store::command_line(&app.command, &app.args)
                } else {
                    app.url.clone()
                })
                .into_any_element(),
        );
        rows.push(
            ListRow::new("app-server-id", "Server name for the agents")
                .subtitle(format!("`{}`", app.server_id))
                .into_any_element(),
        );
        let id = app.id.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .child(ui::section(Some("About"), None, rows, cx))
            .child(
                div().flex().child(
                    Button::secondary("app-edit", "Edit settings")
                        .small()
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(ConnectorEvent::Edit(id.clone())))),
                ),
            )
            .into_any_element()
    }

    /// Its ready-made app's line, if it's one.
    fn preset_description(&self, app: &AppRow, cx: &App) -> Option<String> {
        let preset = app.preset.as_deref()?;
        let data = self.store.read(cx);
        let from_store = data
            .loaded()
            .and_then(|data| data.presets.iter().find(|row| row.id == preset))
            .map(|row| row.description.clone());
        from_store
            .or_else(|| connectors_store::preset_row(preset).map(|row| row.description))
            .filter(|description| !description.is_empty())
    }

    /// Account: how it signs in and whether it is, with Reconnect (a new sign-in, or a new check), and for a key app
    /// the key.
    fn render_account(&self, app: &AppRow, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let busy = app.busy.is_some();
        let reconnect = |cx: &mut Context<Self>, path: &'static str, disabled: bool| {
            Button::secondary("app-reconnect", "Reconnect").small().disabled(disabled).on_click(
                cx.listener(move |this, _, window, cx| this.act(path, json!({}), "Couldn’t reconnect", window, cx)),
            )
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut footer: Option<SharedString> = None;
        match app.auth.as_str() {
            AUTH_OAUTH => {
                let host = url::Url::parse(&app.url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_else(|| app.url.clone());
                let signed_in = if app.has_secret {
                    match app.oauth.signed_in_at {
                        Some(at) => format!("Signed in {}", crate::format::short_date_time(at)),
                        None => "Signed in".into(),
                    }
                } else {
                    "Not signed in".into()
                };
                let client = match (&app.oauth.client_id, &app.oauth.registered_client_id) {
                    (Some(client), _) => format!("Your account at {host}, through your own OAuth client ({client})."),
                    (None, _) => {
                        format!("Your account at {host}, signed in in the browser; no Filo account is involved.")
                    }
                };
                let buttons = div()
                    .flex()
                    .gap(px(space::XS))
                    .when(app.has_secret && !app.signing_in(), |this| {
                        this.child(
                            Button::new("app-sign-out", "Sign out").variant(ui::Variant::Quiet).small().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.act("apps:signOut", json!({}), "Couldn’t sign out", window, cx)
                                }),
                            ),
                        )
                    })
                    .child(reconnect(cx, "apps:connect", busy || !app.enabled));
                rows.push(
                    ListRow::new("app-account", signed_in)
                        .icon("circle-user-round")
                        .subtitle(client)
                        .trailing(buttons)
                        .into_any_element(),
                );
                if let Some(scopes) = &app.oauth.scopes {
                    rows.push(ListRow::new("app-scopes", "Scopes").subtitle(scopes.clone()).into_any_element());
                }
                footer = Some("Tokens stay in your Keychain and are refreshed by Filo. A service that doesn’t let apps register themselves needs an OAuth client you make there (Edit settings › Sign-in client).".into());
            }
            AUTH_KEY => {
                let place = if app.is_local() {
                    format!("the {} environment variable", app.key_name)
                } else {
                    format!("the {} header", app.key_name)
                };
                let title = if app.has_secret { "API key saved" } else { "No API key yet" };
                rows.push(
                    ListRow::new("app-account", title)
                        .icon("key-round")
                        .subtitle(format!(
                            "Sent to the server in {place}, by Filo only: your coding tools never see it."
                        ))
                        .trailing(reconnect(cx, "apps:check", busy || !app.enabled || !app.has_secret))
                        .into_any_element(),
                );
                let preset = self.store.read(cx).loaded().and_then(|data| {
                    data.presets.iter().find(|preset| Some(&preset.id) == app.preset.as_ref()).cloned()
                });
                let key_url = preset.as_ref().map(|preset| preset.key_url.clone()).filter(|url| !url.is_empty());
                rows.push(
                    div()
                        .px(px(space::LG))
                        .py(px(space::MD))
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(div().flex_1().min_w_0().child(Input::new(&self.api_key)))
                        .child(
                            Button::new("app-save-key", if app.has_secret { "Replace" } else { "Save" })
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| this.save_key(window, cx))),
                        )
                        .when_some(key_url, |this, url| {
                            this.child(
                                Button::new("app-get-key", "Get a key")
                                    .variant(ui::Variant::Quiet)
                                    .small()
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                        })
                        .when(app.has_secret, |this| {
                            this.child(
                                Button::new("app-forget-key", "Delete key")
                                    .variant(ui::Variant::Quiet)
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.act("apps:signOut", json!({}), "Couldn’t delete the key", window, cx)
                                    })),
                            )
                        })
                        .into_any_element(),
                );
                footer = preset.map(|preset| preset.note.into()).filter(|note: &SharedString| !note.is_empty());
            }
            _ => {
                let (icon, subtitle) = if app.is_local() {
                    ("square-terminal", "Runs on this Mac as a command; nothing to sign in to.")
                } else {
                    ("globe", "Open to anyone: nothing to sign in to.")
                };
                rows.push(
                    ListRow::new("app-account", "No sign-in needed")
                        .leading(ui::icon_sized(icon, 18., colors.text_secondary))
                        .subtitle(subtitle)
                        .trailing(reconnect(cx, "apps:check", busy || !app.enabled))
                        .into_any_element(),
                );
            }
        }
        ui::section(Some("Account"), footer, rows, cx).into_any_element()
    }

    /// The app's own switch: off, no agent and no engine gets it, and it's kept as it is.
    fn render_on(&self, app: &AppRow, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.saving.get("enabled").copied().unwrap_or(app.enabled);
        let row = ListRow::new("app-enabled", "On")
            .subtitle(if enabled {
                "Turn off to stop every agent using it, keeping its settings and sign-in."
            } else {
                "Off: no agent gets it."
            })
            .trailing(
                Switch::new("app-enabled-switch")
                    .checked(enabled)
                    .disabled(self.saving.contains_key("enabled"))
                    .on_click(cx.listener(|this, checked: &bool, window, cx| {
                        this.switch("enabled".into(), "apps:setEnabled", json!({}), *checked, window, cx)
                    })),
            )
            .into_any_element();
        ui::section(None, None, vec![row], cx).into_any_element()
    }

    /// Which of the Agents tab's agents may use it: every one (new ones too), or those switched on. Filo is the Filo
    /// agent: its own chats, its routines and Filo in the group.
    fn render_agents(&self, app: &AppRow, cx: &mut Context<Self>) -> AnyElement {
        let agents = self.store.read(cx).loaded().map(|data| data.agents.clone()).unwrap_or_default();
        let enabled = self.saving.get("enabled").copied().unwrap_or(app.enabled);
        let all = self.saving.get("agents:all").copied().unwrap_or(app.agent_scope != "some");
        let all_switch = Switch::new("app-agents-all-switch")
            .checked(all)
            .disabled(!enabled || self.saving.contains_key("agents:all"))
            .on_click(cx.listener(|this, checked: &bool, window, cx| {
                let on = *checked;
                this.saving.insert("agents:all".into(), on);
                cx.notify();
                let id = this.id.clone();
                backend::call_in(
                    window,
                    cx,
                    "apps:setAllAgents",
                    json!({ "id": id, "all": on }),
                    |this: &mut Self, result: Result<Value, String>, window, cx| {
                        this.saving.remove("agents:all");
                        if let Err(error) = result {
                            ui::toast_error(window, cx, "Couldn’t change it", error);
                        }
                        cx.notify();
                    },
                );
            }));
        let every = if all { "All your agents, and any you make later." } else { "Only the agents switched on below." };
        let mut rows: Vec<AnyElement> =
            vec![access_row(None, "Every agent".into(), every.into(), all_switch.into_any_element(), cx)];
        for agent in agents {
            let key = format!("agent:{}", agent.id);
            let on = self.saving.get(&key).copied().unwrap_or(app.allows_agent(&agent.id));
            let face = super::agents::agent_picture(
                super::agents::picture_source(&agent.picture),
                agent.background.as_deref(),
                28.,
                cx,
            )
            .into_any_element();
            let subtitle = match (agent.is_filo, on && enabled) {
                (true, true) => "Uses it in Filo’s chats, routines and check-ins, and in the group",
                (true, false) => "Filo’s chats and Filo in the group don’t get it",
                (false, true) => "Uses it in its chat, routines and check-ins, and in the group",
                (false, false) => "Doesn’t get it",
            };
            let agent_id = agent.id.clone();
            let switch = Switch::new(SharedString::from(format!("app-agent-switch-{}", agent.id)))
                .checked(on)
                .disabled(!enabled || self.saving.contains_key(&key) || self.saving.contains_key("agents:all"))
                .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                    this.switch(key.clone(), "apps:setAgentAccess", json!({ "agent": agent_id }), *checked, window, cx);
                }));
            rows.push(access_row(
                Some(face),
                agent.name.clone().into(),
                subtitle.into(),
                switch.into_any_element(),
                cx,
            ));
        }
        ui::section(Some("Agents"), Some("Who may use it: each agent of the Agents tab, in its own chat and in the group. Turning one off keeps it from that agent only.".into()), rows, cx)
            .into_any_element()
    }

    /// Which CLIs may run its tools, whichever agent is speaking.
    fn render_engines(&self, app: &AppRow, cx: &mut Context<Self>) -> AnyElement {
        let engines = self.store.read(cx).loaded().map(|data| data.engines.clone()).unwrap_or_default();
        let enabled = self.saving.get("enabled").copied().unwrap_or(app.enabled);
        let rows: Vec<AnyElement> = engines
            .into_iter()
            .map(|row| {
                let key = format!("engine:{}", row.engine);
                let parsed = filo_local::Engine::parse(&row.engine);
                let allowed = match parsed {
                    Some(filo_local::Engine::Codex) => app.codex,
                    Some(filo_local::Engine::Muse) => app.muse,
                    Some(filo_local::Engine::OpenCode) => app.opencode,
                    Some(filo_local::Engine::Pi) => app.pi,
                    _ => app.claude,
                };
                let on = self.saving.get(&key).copied().unwrap_or(allowed);
                let engine = row.engine.clone();
                let subtitle = if !row.installed {
                    "Engine · not installed on this Mac".to_string()
                } else if on && enabled {
                    format!("Engine · runs its tools as `{}` when it answers for an allowed agent", app.server_id)
                } else {
                    "Engine · doesn’t get it, whichever agent it answers for".to_string()
                };
                ListRow::new(SharedString::from(format!("app-engine-{engine}")), row.name.clone())
                    .leading(ui::icon_sized(
                        parsed.map_or("claude", crate::screens::chat::conversation::engine_logo),
                        18.,
                        cx.colors().text_secondary,
                    ))
                    .subtitle(subtitle)
                    .trailing(
                        Switch::new(SharedString::from(format!("app-engine-switch-{engine}")))
                            .checked(on)
                            .disabled(!enabled || self.saving.contains_key(&key))
                            .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                                this.switch(
                                    key.clone(),
                                    "apps:setEngine",
                                    json!({ "engine": engine }),
                                    *checked,
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        ui::section(
            Some("Engines"),
            Some("Filo starts its server with each turn: nothing is added to the coding tools’ own settings.".into()),
            rows,
            cx,
        )
        .into_any_element()
    }

    /// Tool permissions: the app's tools in two groups, Read-only and Changes things, each tool and each group Allow
    /// or Block.
    fn render_tools(&mut self, app: &AppRow, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let tools = app.tools.clone();
        if tools.is_empty() {
            let message = match app.status.as_str() {
                connectors_store::READY => "It offers no tools.",
                _ if app.busy.is_some() => "Looking…",
                _ => "Its tools show once it’s connected.",
            };
            return empty_tools(message, cx);
        }
        let allowed = tools.iter().filter(|tool| self.tool_on(app, tool)).count();
        // Every tool at once, like a group's: neither side while they're mixed.
        let current = if allowed == tools.len() {
            Some(Permission::Allow)
        } else if allowed == 0 {
            Some(Permission::Block)
        } else {
            None
        };
        let saving = tools.iter().any(|tool| self.saving.contains_key(&format!("tool:{}", tool.name)));
        let (row_app, this) = (app.clone(), cx.entity().downgrade());
        let control = permission_control(
            "tools-all".into(),
            current,
            saving || !app.enabled,
            move |permission, window, cx| {
                let row_app = row_app.clone();
                this.update(cx, |this, cx| this.set_all(&row_app, permission == Permission::Allow, window, cx)).ok();
            },
            cx,
        );
        let mut column =
            div().relative().flex().flex_col().gap(px(space::SM)).child(self.probe("tools".into(), cx)).child(
                div()
                    .pl(px(space::SM))
                    .pr(px(space::LG))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(div().footnote().semibold().text_color(colors.text_secondary).child("Tool permissions"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .footnote()
                            .text_color(colors.text_tertiary)
                            .truncate()
                            .child(format!("{allowed} of {} allowed", tools.len())),
                    )
                    .child(control),
            );
        if tools.len() > SEARCH_FROM {
            if self.placeholder_count != tools.len() {
                let placeholder = format!("Search {} tools", tools.len());
                self.search.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                self.placeholder_count = tools.len();
            }
            // The column's drawn width, set: left to the column (`w_full`), a narrow window shrank it to its icon.
            let width = self.room("tools").map(|width| div().w(width)).unwrap_or_else(|| div().w_full());
            column = column.child(width.child(Input::new(&self.search).cleanable(true).prefix(ui::icon_sized(
                "search",
                16.,
                colors.text_secondary,
            ))));
        }
        let needle = self.query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let matching = |tool: &AppTool| {
            needle.is_empty()
                || tool.label().to_lowercase().contains(&needle)
                || tool.name.to_lowercase().contains(&needle)
                || tool.description.to_lowercase().contains(&needle)
        };
        let mut shown = 0;
        for group in [Group::ReadOnly, Group::Changes] {
            let members: Vec<AppTool> =
                tools.iter().filter(|tool| reads_only(app, tool) == (group == Group::ReadOnly)).cloned().collect();
            if members.is_empty() {
                continue;
            }
            let visible: Vec<AppTool> = members.iter().filter(|tool| matching(tool)).cloned().collect();
            if visible.is_empty() {
                continue;
            }
            shown += visible.len();
            let header = self.group_row(app, group, &members, cx);
            let rows = visible.iter().map(|tool| self.tool_row(app, tool, cx)).collect();
            column = column.child(div().pt(px(space::XS)).child(tool_group(header, rows, cx)));
        }
        if shown == 0 {
            return column
                .child(
                    div()
                        .py(px(space::XL))
                        .subhead()
                        .text_color(colors.text_secondary)
                        .text_center()
                        .child(format!("No tools match “{}”.", self.query.trim())),
                )
                .into_any_element();
        }
        column
            .child(div().px(px(space::SM)).child(connectors_store::small_print(
                if app.all_tools {
                    "Every tool starts allowed, those the server adds later too. Block any you don’t want agents to use: a tool that may change, delete or send things runs without asking you first."
                } else {
                    "Tools that only read start allowed: those the server marks read-only, and for a ready-made app the ones Filo checked. Any other tool, one that may change, delete or send things, stays blocked until you allow it."
                },
                cx,
            )))
            .into_any_element()
    }

    /// Whether `tool` is on, or will be once its change saves.
    fn tool_on(&self, app: &AppRow, tool: &AppTool) -> bool {
        self.saving.get(&format!("tool:{}", tool.name)).copied().unwrap_or(app.tool_allowed(&tool.name))
    }

    /// A group's heading row: its name, how many are allowed, and Allow or Block for all of them.
    fn group_row(&self, app: &AppRow, group: Group, members: &[AppTool], cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let on = members.iter().filter(|tool| self.tool_on(app, tool)).count();
        let current = if on == members.len() {
            Some(Permission::Allow)
        } else if on == 0 {
            Some(Permission::Block)
        } else {
            None
        };
        let saving = members.iter().any(|tool| self.saving.contains_key(&format!("tool:{}", tool.name)));
        let names: Vec<String> = members.iter().map(|tool| tool.name.clone()).collect();
        let row_app = app.clone();
        let this = cx.entity().downgrade();
        let control = permission_control(
            SharedString::from(format!("tools-group-{}", group.label())),
            current,
            saving || !app.enabled,
            move |permission, window, cx| {
                let names = names.clone();
                let row_app = row_app.clone();
                this.update(cx, |this, cx| {
                    this.set_group(&row_app, group, names, permission == Permission::Allow, window, cx)
                })
                .ok();
            },
            cx,
        );
        div()
            .min_h(px(48.))
            .px(px(space::LG))
            .py(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::MD))
            // The card's corners (`tool_group`): its clip is square, so the tint would cover them.
            .rounded_t(px(radius::MD))
            .bg(colors.surface_strong.opacity(0.4))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(div().callout().semibold().child(group.label()))
                    .child(
                        div()
                            .footnote()
                            .text_color(colors.text_tertiary)
                            .child(format!("{on} of {} allowed", members.len())),
                    ),
            )
            .child(control)
            .into_any_element()
    }

    fn tool_row(&self, app: &AppRow, tool: &AppTool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let saving = self.saving.contains_key(&format!("tool:{}", tool.name));
        let current = if self.tool_on(app, tool) { Permission::Allow } else { Permission::Block };
        // What the server doesn't say, or that it can delete: worth a word beside the name.
        let note = if reads_only(app, tool) {
            None
        } else if !tool.marked {
            Some(ui::chip("Not marked", Tone::Warning, true, cx))
        } else if tool.destructive {
            Some(ui::chip("Can delete", Tone::Warning, true, cx))
        } else {
            None
        };
        let (row_app, row_tool) = (app.clone(), tool.clone());
        let this = cx.entity().downgrade();
        let control = permission_control(
            SharedString::from(format!("tool-{}", tool.name)),
            Some(current),
            saving || !app.enabled,
            move |permission, window, cx| {
                let (row_app, row_tool) = (row_app.clone(), row_tool.clone());
                this.update(cx, |this, cx| {
                    this.set_tool(&row_app, &row_tool, permission == Permission::Allow, window, cx)
                })
                .ok();
            },
            cx,
        );
        // The name and the description on one line each, cut with "…" to the width the column had when last drawn (and,
        // beside a chip, the room the chip leaves); until that's known, they fill the column and are clipped.
        let key = format!("tool:{}", tool.name);
        let column = self.room(&format!("{key}:column"));
        let line = |text: gpui::Div,
                    content: String,
                    width: Option<Pixels>,
                    variant: TypeVariant,
                    weight: FontWeight| match width {
            Some(width) => text.flex_none().whitespace_nowrap().child(fit(&content, width, variant, weight, cx)),
            None => text.flex_1().min_w_0().truncate().child(content),
        };
        let label = tool.label();
        let title = match note {
            None => div().flex().child(line(
                div().callout().medium(),
                label,
                column,
                TypeVariant::Callout,
                FontWeight::MEDIUM,
            )),
            Some(note) => {
                let room =
                    column.zip(self.room(&format!("{key}:chip"))).map(|(column, chip)| column - chip - px(space::SM));
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(line(div().callout().medium(), label, room, TypeVariant::Callout, FontWeight::MEDIUM))
                    .child(div().flex_none().relative().child(note).child(self.probe(format!("{key}:chip"), cx)))
            }
        };
        div()
            .min_h(px(56.))
            .px(px(space::LG))
            .py(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(self.probe(format!("{key}:column"), cx))
                    .child(title)
                    .when(!tool.description.is_empty(), |this| {
                        // One paragraph: a description of several (examples, notes) kept its line breaks.
                        let description = tool.description.split_whitespace().collect::<Vec<_>>().join(" ");
                        this.child(div().flex().child(line(
                            div().footnote().text_color(colors.text_secondary),
                            description,
                            column,
                            TypeVariant::Footnote,
                            FontWeight::NORMAL,
                        )))
                    }),
            )
            .child(control)
            .into_any_element()
    }

    /// The width last drawn for `key` (`probe`).
    /// (A probe, not gpui's `truncate`: a no-wrap text is laid out once a frame, at the first width layout asks it about,
    /// which here was before the column's, so it came out cut at the edge without the "…".)
    fn room(&self, key: &str) -> Option<Pixels> {
        self.room.borrow().get(key).copied()
    }

    /// Fills its parent and keeps the parent's drawn width under `key`; a new width draws the page again.
    fn probe(&self, key: String, cx: &Context<Self>) -> impl IntoElement {
        let (room, this) = (self.room.clone(), cx.entity().downgrade());
        canvas(
            move |bounds, _, cx| {
                if room.borrow().get(&key) != Some(&bounds.size.width) {
                    room.borrow_mut().insert(key, bounds.size.width);
                    cx.defer(move |cx| {
                        this.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

impl Render for ConnectorScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let Some(app) = self.app(cx) else {
            let loading = self.store.read(cx).data.is_none();
            let body = if loading {
                ui::skeleton_page(&[2, 4], Some(28.), cx).into_any_element()
            } else {
                ui::empty_state("plug", "This app isn’t connected", None, None, cx).into_any_element()
            };
            return div().size_full().child(body).into_any_element();
        };
        let (line, tone) = app.status_line();
        let status = div().subhead().semibold().text_color(tone.color(cx)).child(line).into_any_element();
        let description = self.preset_description(&app, cx).unwrap_or_else(|| {
            format!("{} · {}", if app.is_local() { "Runs on this Mac" } else { "Remote" }, app.place())
        });
        let actions = self.render_actions(&app, cx);
        let hero = app_header(
            connectors_store::app_icon(app.preset.as_deref(), app.is_local(), 30., cx),
            app.name.clone().into(),
            description.into(),
            Some(status),
            actions,
            cx,
        );
        let problem = (!app.status_detail.is_empty() && app.busy.is_none() && app.status != connectors_store::READY)
            .then(|| app.status_detail.clone());
        let note =
            (app.status == connectors_store::READY && !app.status_detail.is_empty()).then(|| app.status_detail.clone());
        let tools = self.render_tools(&app, window, cx);
        let account = self.render_account(&app, cx);
        let about = self.render_about(&app, cx);
        let advanced = self.advanced.then(|| {
            let on = self.render_on(&app, cx);
            let agents = self.render_agents(&app, cx);
            let engines = self.render_engines(&app, cx);
            (on, agents, engines)
        });
        let toggle = div()
            .id("app-advanced")
            .flex()
            .items_center()
            .gap(px(space::XS))
            .px(px(space::SM))
            .cursor_pointer()
            .subhead()
            .semibold()
            .text_color(colors.text_secondary)
            .hover(move |style| style.text_color(colors.text))
            .on_click(cx.listener(|this, _, _, cx| {
                this.advanced = !this.advanced;
                cx.notify();
            }))
            .child(ui::icon_sized(
                if self.advanced { "chevron-down" } else { "chevron-right" },
                14.,
                colors.text_secondary,
            ))
            .child("Advanced: agents, engines, on or off");
        // Stacked, not a flex column: taller than the page, a flex column shrank its sections and their rows overlapped.
        let scroll = div().id("connector-scroll").flex_1().min_h_0().overflow_y_scroll().child(
            ui::page_column()
                .pt(px(space::SM))
                .pb(px(48.))
                .child(hero)
                .when_some(problem, |this, problem| {
                    let tone = if app.status == connectors_store::FAILED { Tone::Danger } else { Tone::Warning };
                    this.child(stacked(ui::banner(problem, tone, cx).into_any_element()))
                })
                .when_some(note, |this, note| {
                    this.child(stacked(connectors_store::small_print(note, cx).into_any_element()))
                })
                .child(stacked(tools))
                .child(stacked(account))
                .child(stacked(about))
                .child(stacked(toggle.into_any_element()))
                .when_some(advanced, |this, (on, agents, engines)| {
                    this.child(stacked(on)).child(stacked(agents)).child(stacked(engines))
                }),
        );
        div().size_full().flex().flex_col().child(scroll).into_any_element()
    }
}

/// An app's header: its icon, name and what it is, how it's doing, and its buttons at the end (Connect, Disconnect).
pub fn app_header(
    icon: AnyElement,
    name: SharedString,
    description: SharedString,
    status: Option<AnyElement>,
    actions: AnyElement,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    div()
        .flex()
        .items_center()
        .gap(px(space::LG))
        .child(icon_tile_sized(icon, 56., cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .child(div().title2().truncate().child(name))
                .child(div().subhead().text_color(colors.text_secondary).truncate().child(description))
                .children(status),
        )
        .child(div().flex_none().child(actions))
        .into_any_element()
}

/// The tools' place before there are any to show: a line under the heading, like the section's small print (a
/// centred box sized its text to one line, which ran past the box).
pub fn empty_tools(message: &str, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .child(
            div().px(px(space::SM)).footnote().semibold().text_color(colors.text_secondary).child("Tool permissions"),
        )
        .child(div().px(px(space::SM)).child(connectors_store::small_print(message.to_string(), cx)))
        .into_any_element()
}

/// Allow · Block for a tool, a group or every tool (`current` None: some of each).
fn permission_control(
    id: SharedString,
    current: Option<Permission>,
    disabled: bool,
    on_pick: impl Fn(Permission, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    let on_pick = Rc::new(on_pick);
    let options = [(Permission::Allow, "Allow"), (Permission::Block, "Block")];
    div()
        .flex()
        .flex_none()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(radius::SM + 2.))
        .bg(colors.surface_strong)
        .when(disabled, |this| this.opacity(0.6))
        .children(options.into_iter().map(|(permission, label)| {
            let selected = current == Some(permission);
            let on_pick = on_pick.clone();
            let (bg, fg) = match permission {
                Permission::Allow => (colors.success_soft, colors.on_success_soft),
                Permission::Block => (colors.danger_soft, colors.on_danger_soft),
            };
            div()
                .id(SharedString::from(format!("{id}-{label}")))
                .min_w(px(50.))
                .h(px(24.))
                .px(px(space::SM))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::SM))
                .micro()
                .map(|this| {
                    if selected {
                        this.bg(bg).text_color(fg).semibold()
                    } else {
                        this.text_color(colors.text_secondary).medium()
                    }
                })
                .when(!selected && !disabled, |this| {
                    this.cursor_pointer()
                        .hover(|style| style.text_color(colors.text))
                        .on_click(move |_, window, cx| on_pick(permission, window, cx))
                })
                .child(label)
        }))
        .into_any_element()
}

/// A row of the Agents section: the agent's face, its name and what it gets, and its switch.
fn access_row(
    face: Option<AnyElement>,
    title: SharedString,
    subtitle: SharedString,
    switch: AnyElement,
    cx: &gpui::App,
) -> AnyElement {
    let colors = *cx.colors();
    div()
        .min_h(px(56.))
        .px(px(space::LG))
        .py(px(space::SM))
        .flex()
        .items_center()
        .gap(px(space::MD))
        .children(face)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().callout().medium().truncate().child(title))
                .child(div().footnote().text_color(colors.text_secondary).child(subtitle)),
        )
        .child(switch)
        .into_any_element()
}

/// `text` cut with "…" to fit `width` in the theme's font, like chat/working_panel.rs's titles.
fn fit(text: &str, width: Pixels, variant: TypeVariant, weight: FontWeight, cx: &App) -> String {
    let font = gpui::Font { weight, ..gpui::font(cx.theme().font_family()) };
    let mut wrapper = cx.text_system().line_wrapper(font.clone(), px(variant.size()));
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let cut = wrapper.truncate_line(text.to_string().into(), width.max(px(0.)), "…", &mut vec![run]);
    let Some(kept) = cut.strip_suffix('…') else { return cut.to_string() };
    // At the last whole word that fits (a single word too long for the line is cut where it must), with no space or
    // comma before the "…".
    let mid_word = text[kept.len()..].chars().next().is_some_and(|next| !next.is_whitespace());
    let kept = match kept.rfind(char::is_whitespace) {
        Some(space) if mid_word => &kept[..space],
        _ => kept,
    };
    format!("{}…", kept.trim_end().trim_end_matches([',', ';', ':', '.', '(', '-']))
}

/// A group of tools on a card like ui::section's: its header band closed off by a line the card's full width, then
/// the tools with the section's inset lines between them.
fn tool_group(header: AnyElement, rows: Vec<AnyElement>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let line = colors.border.opacity(0.6);
    let count = rows.len();
    let mut card = div()
        .flex()
        .flex_col()
        .rounded(px(radius::MD))
        .bg(colors.surface)
        .overflow_hidden()
        .child(header)
        .child(div().h(px(1.)).bg(line));
    for (index, row) in rows.into_iter().enumerate() {
        card = card.child(row);
        if index + 1 < count {
            card = card.child(div().ml(px(space::LG)).h(px(1.)).bg(line));
        }
    }
    card.into_any_element()
}

/// A part of the page under the one before it.
fn stacked(part: AnyElement) -> gpui::Div {
    div().pt(px(space::XL)).child(part)
}

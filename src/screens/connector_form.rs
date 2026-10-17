// Adding an app, or changing one (Extensions › Apps › Add server / Edit <name>): a remote server's address or the
// command that starts a local one, and how it signs in: not at all, with an account (OAuth: Filo registers itself with
// the service's sign-in, or uses a client the user made there), or with an API key. Keys and client secrets go to the
// Keychain (local_backend/apps.rs); this form never shows a saved one. A preset connects through `KeyForm` or
// `apps:connectPreset` instead.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputState};
use serde_json::{Value, json};

use super::connectors_store::{self, AUTH_KEY, AUTH_NONE, AUTH_OAUTH, AppRow, PresetRow, STDIO};
use crate::backend;
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, Button, Tone};

pub enum FormEvent {
    /// Added or saved: the app's id.
    Saved(String),
    /// Back to the app being edited, or to the Apps page.
    Cancelled(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind2 {
    Remote,
    Local,
}

pub struct ConnectorForm {
    editing: Option<AppRow>,
    kind: Kind2,
    auth: &'static str,
    name: Entity<InputState>,
    url: Entity<InputState>,
    command: Entity<InputState>,
    env: Entity<InputState>,
    api_key: Entity<InputState>,
    key_name: Entity<InputState>,
    client_id: Entity<InputState>,
    client_secret: Entity<InputState>,
    scopes: Entity<InputState>,
    /// Shows the sign-in client's fields (a service that lets no app register itself).
    own_client: bool,
    error: Option<String>,
    saving: bool,
}

impl EventEmitter<FormEvent> for ConnectorForm {}

fn auth_of(value: &str) -> &'static str {
    match value {
        AUTH_OAUTH => AUTH_OAUTH,
        AUTH_KEY => AUTH_KEY,
        _ => AUTH_NONE,
    }
}

/// `KEY=value` lines (blank lines and `#` comments skipped) as an environment.
pub fn parse_env(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let (key, value) = line.split_once('=').ok_or_else(|| format!("“{line}” isn’t KEY=value."))?;
        env.insert(key.trim().to_string(), value.trim().to_string());
    }
    Ok(env)
}

impl ConnectorForm {
    pub fn new(editing: Option<AppRow>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, value: String, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        let (kind, auth, name, url, command, env, key_name, client_id, scopes) = match &editing {
            Some(app) => (
                if app.is_local() { Kind2::Local } else { Kind2::Remote },
                auth_of(&app.auth),
                app.name.clone(),
                if app.is_local() { String::new() } else { app.url.clone() },
                if app.is_local() { connectors_store::command_line(&app.command, &app.args) } else { String::new() },
                app.env.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("\n"),
                app.key_name.clone(),
                app.oauth.client_id.clone().unwrap_or_default(),
                app.oauth.scopes.clone().unwrap_or_default(),
            ),
            None => (
                Kind2::Remote,
                AUTH_NONE,
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        let own_client = !client_id.is_empty();
        let has_key = editing.as_ref().is_some_and(|app| app.has_secret && app.auth == AUTH_KEY);
        ConnectorForm {
            kind,
            auth,
            name: input("Linear, My tools…", name, window, cx),
            url: input("https://example.com/mcp", url, window, cx),
            command: input("npx -y @modelcontextprotocol/server-memory", command, window, cx),
            env: cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .multi_line(true)
                    .auto_grow(2, 6)
                    .placeholder("KEY=value, one per line (not for secrets: use the API key)");
                state.set_value(env, window, cx);
                state
            }),
            api_key: cx.new(|cx| {
                InputState::new(window, cx).masked(true).placeholder(if has_key {
                    "Saved in your Keychain. Paste a new one to replace it"
                } else {
                    "Paste the key"
                })
            }),
            key_name: input("Authorization", key_name, window, cx),
            client_id: input("Client ID", client_id, window, cx),
            client_secret: cx
                .new(|cx| InputState::new(window, cx).masked(true).placeholder("Client secret, if it has one")),
            scopes: input("Scopes, if the service needs particular ones", scopes, window, cx),
            own_client,
            editing,
            error: None,
            saving: false,
        }
    }

    fn value(state: &Entity<InputState>, cx: &gpui::App) -> String {
        state.read(cx).value().to_string()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let env = match parse_env(&Self::value(&self.env, cx)) {
            Ok(env) => env,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let local = self.kind == Kind2::Local;
        let auth = if local && self.auth == AUTH_OAUTH { AUTH_NONE } else { self.auth };
        let mut args = json!({
            "name": Self::value(&self.name, cx),
            "transport": if local { "stdio" } else { "http" },
            "auth": auth,
        });
        if local {
            args["command"] = json!(Self::value(&self.command, cx));
            args["env"] = json!(env);
        } else {
            args["url"] = json!(Self::value(&self.url, cx).trim());
        }
        if auth == AUTH_KEY {
            args["keyName"] = json!(Self::value(&self.key_name, cx));
            let key = Self::value(&self.api_key, cx);
            if !key.trim().is_empty() {
                args["apiKey"] = json!(key);
            }
        }
        if auth == AUTH_OAUTH {
            let client_id = if self.own_client { Self::value(&self.client_id, cx) } else { String::new() };
            args["clientId"] = json!(client_id.trim());
            args["scopes"] = json!(Self::value(&self.scopes, cx));
            let secret = Self::value(&self.client_secret, cx);
            if self.own_client && !secret.trim().is_empty() {
                args["clientSecret"] = json!(secret);
            }
        }
        let path = match &self.editing {
            Some(app) => {
                args["id"] = json!(app.id);
                "apps:update"
            }
            None => "apps:add",
        };
        self.saving = true;
        self.error = None;
        cx.notify();
        backend::call_in(window, cx, path, args, |this: &mut Self, result: Result<Value, String>, _, cx| {
            this.saving = false;
            match result {
                Ok(app) => {
                    if let Some(id) = app.get("id").and_then(Value::as_str) {
                        cx.emit(FormEvent::Saved(id.to_string()));
                    }
                }
                Err(error) => this.error = Some(error),
            }
            cx.notify();
        });
    }

    fn field(label: &str, input: AnyElement, hint: Option<String>, cx: &gpui::App) -> AnyElement {
        let colors = *cx.colors();
        div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .child(
                div()
                    .px(px(space::XXS))
                    .footnote()
                    .semibold()
                    .text_color(colors.text_secondary)
                    .child(label.to_string()),
            )
            .child(input)
            .when_some(hint, |this, hint| {
                this.child(div().px(px(space::XXS)).footnote().text_color(colors.text_tertiary).child(hint))
            })
            .into_any_element()
    }
}

impl Render for ConnectorForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let local = self.kind == Kind2::Local;
        let fixed_redirect =
            connectors_store::store(cx).read(cx).loaded().map(|data| data.fixed_redirect.clone()).unwrap_or_default();
        let mut fields: Vec<AnyElement> = Vec::new();
        fields.push(Self::field("Name", Input::new(&self.name).into_any_element(), None, cx));
        let kind = ui::segmented(
            "app-form-kind",
            vec![
                (Kind2::Remote, "Remote server".into(), Some("globe")),
                (Kind2::Local, "Local command".into(), Some("square-terminal")),
            ],
            &self.kind,
            {
                let this = cx.entity();
                move |kind, _, cx| {
                    this.update(cx, |this, cx| {
                        this.kind = kind;
                        if kind == Kind2::Local && this.auth == AUTH_OAUTH {
                            this.auth = AUTH_NONE;
                        }
                        cx.notify();
                    })
                }
            },
            cx,
        );
        fields.push(Self::field("Where it runs", kind.into_any_element(), None, cx));
        if local {
            let command = div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(div().flex_1().min_w_0().child(Input::new(&self.command)));
            fields.push(Self::field(
                "Command",
                command.into_any_element(),
                Some("The command that starts the server, as you’d type it in Terminal. Filo runs it with your shell’s PATH for each turn that uses it.".into()),
                cx,
            ));
            fields.push(Self::field("Environment", Input::new(&self.env).into_any_element(), None, cx));
        } else {
            fields.push(Self::field(
                "Server URL",
                Input::new(&self.url).into_any_element(),
                Some("Its MCP address (Streamable HTTP, or SSE for older servers).".into()),
                cx,
            ));
        }
        let mut auth_options = vec![(AUTH_NONE, SharedString::from("No sign-in"), None)];
        if !local {
            auth_options.push((AUTH_OAUTH, "Account".into(), None));
        }
        auth_options.push((AUTH_KEY, "API key".into(), None));
        let auth = ui::segmented(
            "app-form-auth",
            auth_options,
            &self.auth,
            {
                let this = cx.entity();
                move |auth, _, cx| {
                    this.update(cx, |this, cx| {
                        this.auth = auth;
                        cx.notify();
                    })
                }
            },
            cx,
        );
        fields.push(Self::field("Sign-in", auth.into_any_element(), None, cx));
        if self.auth == AUTH_KEY {
            fields.push(Self::field(
                "API key",
                Input::new(&self.api_key).into_any_element(),
                Some("Kept in your Keychain. Only Filo sends it to the server; your coding tools never see it.".into()),
                cx,
            ));
            let (label, hint) = if local {
                ("Environment variable", "The variable the server reads the key from, like GITHUB_TOKEN.")
            } else {
                ("Header", "The header it goes in. Authorization sends it as “Bearer <key>”.")
            };
            fields.push(Self::field(label, Input::new(&self.key_name).into_any_element(), Some(hint.into()), cx));
        }
        if self.auth == AUTH_OAUTH && !local {
            fields.push(
                ui::banner(
                    "You’ll sign in in your browser. No Filo account is used: Filo registers itself with the service’s sign-in (dynamic client registration). Some services don’t allow that (GitHub, Google and others): for those, make an OAuth app there and use it below, or use an API key.",
                    Tone::Neutral,
                    cx,
                )
                .into_any_element(),
            );
            let own = self.own_client;
            fields.push(
                div()
                    .child(
                        Button::new(
                            "app-form-own-client",
                            if own { "Let Filo register itself instead" } else { "Use my own OAuth client…" },
                        )
                        .variant(ui::Variant::Quiet)
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.own_client = !this.own_client;
                            cx.notify();
                        })),
                    )
                    .into_any_element(),
            );
            if own {
                fields.push(Self::field(
                    "Sign-in client",
                    Input::new(&self.client_id).into_any_element(),
                    Some(format!("Register this redirect URL with your client: {fixed_redirect}")),
                    cx,
                ));
                fields.push(Self::field(
                    "Client secret",
                    Input::new(&self.client_secret).into_any_element(),
                    Some("Kept in your Keychain.".into()),
                    cx,
                ));
            }
            fields.push(Self::field(
                "Scopes",
                Input::new(&self.scopes).into_any_element(),
                Some("Leave empty for what the service asks for.".into()),
                cx,
            ));
        }
        let editing = self.editing.as_ref().map(|app| app.id.clone());
        let submit_label = match (&self.editing, self.auth) {
            (Some(_), _) => "Save",
            (None, AUTH_OAUTH) if !local => "Add and sign in",
            (None, _) => "Add",
        };
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(
                Button::new("app-form-submit", submit_label)
                    .loading(self.saving)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
            .child(
                Button::secondary("app-form-cancel", "Cancel")
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(FormEvent::Cancelled(editing.clone())))),
            );
        div().id("app-form-scroll").size_full().overflow_y_scroll().child(
            ui::page_column()
                .pt(px(space::SM))
                .pb(px(48.))
                .flex()
                .flex_col()
                .gap(px(space::LG))
                .child(div().title2().child(match &self.editing {
                    Some(app) => format!("Edit {}", app.name),
                    None => "Add a server".to_string(),
                }))
                .when(self.editing.is_none(), |this| {
                    this.child(connectors_store::small_print("Any MCP server: a remote one by its address, or a local one Filo starts with a command. Your coding tools get it in your chats and agents.", cx))
                })
                .children(fields)
                .when_some(self.error.clone(), |this, error| this.child(div().subhead().text_color(colors.danger).child(error)))
                .child(buttons),
        )
    }
}

/// Connecting a preset that takes a key (GitHub): only the key, with where to get one. Everything else is the preset's.
pub struct KeyForm {
    preset: PresetRow,
    key: Entity<InputState>,
    error: Option<String>,
    saving: bool,
}

impl EventEmitter<FormEvent> for KeyForm {}

impl KeyForm {
    pub fn new(preset: PresetRow, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = format!("Paste your {} key", preset.name);
        let key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(placeholder));
        KeyForm { preset, key, error: None, saving: false }
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key.read(cx).value().to_string();
        if self.saving || key.trim().is_empty() {
            return;
        }
        self.saving = true;
        self.error = None;
        cx.notify();
        let args = json!({ "preset": self.preset.id, "apiKey": key });
        backend::call_in(
            window,
            cx,
            "apps:connectPreset",
            args,
            |this: &mut Self, result: Result<Value, String>, _, cx| {
                this.saving = false;
                match result {
                    Ok(app) => {
                        if let Some(id) = app.get("id").and_then(Value::as_str) {
                            cx.emit(FormEvent::Saved(id.to_string()));
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            },
        );
    }
}

impl Render for KeyForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let preset = self.preset.clone();
        let key_url = (!preset.key_url.is_empty()).then(|| preset.key_url.clone());
        div().id("app-key-scroll").size_full().overflow_y_scroll().child(
            ui::page_column()
                .pt(px(space::SM))
                .pb(px(48.))
                .flex()
                .flex_col()
                .gap(px(space::LG))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::LG))
                        .child(super::connectors::icon_tile_sized(
                            connectors_store::app_icon(Some(&preset.id), preset.transport == STDIO, 30., cx),
                            56.,
                            cx,
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(space::XXS))
                                .child(div().title2().child(format!("Connect {}", preset.name)))
                                .child(
                                    div().subhead().text_color(colors.text_secondary).child(preset.description.clone()),
                                ),
                        ),
                )
                .when(!preset.note.is_empty(), |this| {
                    this.child(div().callout().text_color(colors.text_secondary).child(preset.note.clone()))
                })
                .child(Input::new(&self.key))
                .child(connectors_store::small_print(
                    "Kept in your Keychain. Only Filo sends it to the service; your coding tools never see it.",
                    cx,
                ))
                .when_some(self.error.clone(), |this, error| {
                    this.child(div().subhead().text_color(colors.danger).child(error))
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(
                            Button::new("app-key-connect", "Connect")
                                .loading(self.saving)
                                .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                        )
                        .when_some(key_url, |this, url| {
                            this.child(
                                Button::secondary("app-key-get", "Get a key")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                        })
                        .child(
                            Button::new("app-key-cancel", "Cancel")
                                .variant(ui::Variant::Quiet)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Cancelled(None)))),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environments_are_key_value_lines() {
        let env = parse_env("A=1\n\n# note\n B = two words \nURL=https://x?a=b").unwrap();
        assert_eq!(env.get("B").map(String::as_str), Some("two words"));
        assert_eq!(env.get("URL").map(String::as_str), Some("https://x?a=b"));
        assert!(parse_env("just text").is_err());
    }
}

// The user's apps as the pages read them (Extensions › Apps, an app's page, the composer's "@" list): one live query,
// `apps:list` (local_backend/apps.rs), shared by every view that asks. Apps are MCP servers kept in Filo's database
// (crates/filo-local `apps`), local or remote, that the coding tools get in every turn; nothing here talks to a
// server of Filo's.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Global, ImageSource, Img, IntoElement, ParentElement as _,
    Resource, Styled as _, Task, div, img, px,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend;
use crate::theme::{ActiveTheme as _, Typography as _};
use crate::ui::Tone;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppTool {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub read_only: bool,
    pub destructive: bool,
    pub open_world: bool,
    pub marked: bool,
}

impl AppTool {
    /// Its title, else its name made readable ("list_issues" → "List issues").
    pub fn label(&self) -> String {
        if let Some(title) = self.title.as_ref().filter(|title| !title.trim().is_empty()) {
            return title.clone();
        }
        let spaced = self.name.replace(['_', '-'], " ");
        let mut chars = spaced.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OAuthRow {
    pub client_id: Option<String>,
    pub scopes: Option<String>,
    pub registered_client_id: Option<String>,
    pub signed_in_at: Option<f64>,
}

/// An app (crates/filo-local `apps::App`, with what's under way for it).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppRow {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub preset: Option<String>,
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
    pub auth: String,
    pub key_name: String,
    pub has_secret: bool,
    pub oauth: OAuthRow,
    pub enabled: bool,
    pub claude: bool,
    pub codex: bool,
    pub muse: bool,
    pub opencode: bool,
    pub pi: bool,
    /// `all` (every agent, new ones too) or `some` (only `agents`).
    pub agent_scope: String,
    pub agents: Vec<String>,
    /// Every tool starts allowed (an app connected now); off, only those that read do (one connected before).
    pub all_tools: bool,
    pub tools: Vec<AppTool>,
    /// The tools the agents may use now.
    pub allowed_tools: Vec<String>,
    /// Its preset's tools Filo checked only read (on though the server doesn't mark them).
    pub read_tools: Vec<String>,
    pub status: String,
    pub status_detail: String,
    /// `checking` or `signingIn`.
    pub busy: Option<String>,
    /// Its MCP server's name in the CLIs (`filo-<slug>`).
    pub server_id: String,
}

pub const STDIO: &str = "stdio";
pub const AUTH_NONE: &str = "none";
pub const AUTH_OAUTH: &str = "oauth";
pub const AUTH_KEY: &str = "api_key";
pub const READY: &str = "ready";
pub const NEEDS_SIGN_IN: &str = "needs_sign_in";
pub const FAILED: &str = "failed";

impl AppRow {
    pub fn is_local(&self) -> bool {
        self.transport == STDIO
    }

    pub fn tool_allowed(&self, tool: &str) -> bool {
        self.allowed_tools.iter().any(|name| name == tool)
    }

    /// It has what it signs in with: nothing needed, or its key or sign-in kept.
    pub fn credentials_ready(&self) -> bool {
        self.auth == AUTH_NONE || self.has_secret
    }

    /// Whether `agent` (an Agents-tab agent's id) may use it.
    pub fn allows_agent(&self, agent: &str) -> bool {
        self.agent_scope != "some" || self.agents.iter().any(|allowed| allowed == agent)
    }

    pub fn signing_in(&self) -> bool {
        self.busy.as_deref() == Some("signingIn")
    }

    /// Where it runs, in a few words: "mcp.linear.app" or "npx -y @playwright/mcp@latest".
    pub fn place(&self) -> String {
        if self.is_local() {
            std::iter::once(self.command.as_str())
                .chain(self.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            url::Url::parse(&self.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_else(|| self.url.clone())
        }
    }

    /// How it's doing, in a few words, and the tone to draw that in.
    pub fn status_line(&self) -> (String, Tone) {
        match self.busy.as_deref() {
            Some("signingIn") => return ("Finish signing in in your browser…".into(), Tone::Accent),
            Some(_) => return ("Checking…".into(), Tone::Neutral),
            None => {}
        }
        if !self.enabled {
            return ("Off for every agent".into(), Tone::Neutral);
        }
        match self.status.as_str() {
            READY if self.tools.is_empty() => ("Connected".into(), Tone::Success),
            READY if self.allowed_tools.is_empty() => {
                ("Connected · turn on the tools agents may use".into(), Tone::Accent)
            }
            READY => {
                (format!("Connected · {} of {} tools on", self.allowed_tools.len(), self.tools.len()), Tone::Success)
            }
            NEEDS_SIGN_IN if self.auth == AUTH_KEY => ("API key needed".into(), Tone::Warning),
            NEEDS_SIGN_IN => ("Sign-in needed".into(), Tone::Warning),
            FAILED => ("Couldn’t connect".into(), Tone::Danger),
            _ => ("Not checked yet".into(), Tone::Neutral),
        }
    }

    /// Whether it needs the user (a sign-in, a key, or it couldn't connect).
    pub fn needs_attention(&self) -> bool {
        self.enabled && self.busy.is_none() && matches!(self.status.as_str(), NEEDS_SIGN_IN | FAILED)
    }
}

/// A ready-made app as the page reads it, from the app's own list when the live one hasn't come yet.
pub fn preset_row(id: &str) -> Option<PresetRow> {
    serde_json::to_value(filo_local::apps::preset(id)?).ok().and_then(|value| serde_json::from_value(value).ok())
}

/// A ready-made app the page offers (crates/filo-local `apps::PRESETS`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PresetRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub url: String,
    pub command: String,
    pub args: Vec<String>,
    pub auth: String,
    pub key_name: String,
    pub key_prefix: String,
    pub key_url: String,
    pub note: String,
    pub needs: String,
}

/// A coding tool, one of the CLIs that run an app's tools.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineRow {
    pub engine: String,
    pub name: String,
    pub installed: bool,
}

/// An agent of the Agents tab an app may be open to (Filo first: its own chats and the group's Filo are it).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub picture: String,
    pub background: Option<String>,
    pub is_filo: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppsData {
    pub apps: Vec<AppRow>,
    pub presets: Vec<PresetRow>,
    pub engines: Vec<EngineRow>,
    pub agents: Vec<AgentRow>,
    /// The redirect a client the user makes at a provider registers.
    pub fixed_redirect: String,
}

pub struct AppsStore {
    /// None until the first answer.
    pub data: Option<Result<AppsData, String>>,
    _sub: Task<()>,
}

impl AppsStore {
    pub fn apps(&self) -> &[AppRow] {
        match &self.data {
            Some(Ok(data)) => &data.apps,
            _ => &[],
        }
    }

    pub fn app(&self, id: &str) -> Option<&AppRow> {
        self.apps().iter().find(|app| app.id == id)
    }

    pub fn loaded(&self) -> Option<&AppsData> {
        self.data.as_ref().and_then(|data| data.as_ref().ok())
    }
}

struct StoreGlobal(Entity<AppsStore>);

impl Global for StoreGlobal {}

/// The shared store, made the first time a view asks; it outlives the views, so a sign-in keeps going while the user
/// is elsewhere.
pub fn store(cx: &mut App) -> Entity<AppsStore> {
    if let Some(existing) = cx.try_global::<StoreGlobal>() {
        return existing.0.clone();
    }
    let entity = cx.new(|cx: &mut Context<AppsStore>| AppsStore {
        data: None,
        _sub: backend::subscribe(
            cx,
            "apps:list",
            json!({}),
            |this: &mut AppsStore, result: Result<AppsData, String>, cx| {
                this.data = Some(result);
                cx.notify();
            },
        ),
    });
    cx.set_global(StoreGlobal(entity.clone()));
    entity
}

/// The store if a view made it, without making one (a read while drawing).
pub fn store_if_loaded(cx: &App) -> Option<Entity<AppsStore>> {
    cx.try_global::<StoreGlobal>().map(|global| global.0.clone())
}

/// Asks for remote apps not checked for a while to be checked again (the Apps page opening).
pub fn refresh(cx: &mut App) {
    let future = backend::backend(cx).call_raw("apps:refresh", json!({}));
    cx.background_spawn(async move {
        let _: Result<Value, String> = future.await;
    })
    .detach();
}

fn embedded(path: String, size: f32) -> Img {
    img(ImageSource::Resource(Resource::Embedded(path.into()))).size(px(size)).flex_none()
}

/// A ready-made app's logo at `size`, bundled with the app (assets/icons/mcp-<id>.svg, assets/images/mcp-<id>.png;
/// where each comes from: assets/icons/MCP-ICONS.md). Colourful marks are drawn as they are; marks that are black by
/// brand (Notion, Vercel, GitHub, Sentry) in the theme's text colour, so they show on light and dark alike; Context7's,
/// Upsy's and Executor's in their light or dark form. None for an app that isn't one of them.
pub fn preset_icon(preset: &str, size: f32, cx: &App) -> Option<AnyElement> {
    let colors = *cx.colors();
    Some(match preset {
        "clickup" | "linear" | "stripe" | "huggingface" | "cloudflare-docs" | "playwright" => {
            embedded(format!("icons/mcp-{preset}.svg"), size).into_any_element()
        }
        "notion" | "vercel" | "github" | "sentry" => {
            crate::ui::icon_sized(&format!("mcp-{preset}"), size, colors.text).into_any_element()
        }
        "context7" => {
            let dark = gpui_component::theme::Theme::global(cx).is_dark();
            embedded(format!("icons/mcp-context7{}.svg", if dark { "-dark" } else { "" }), size * 0.92)
                .into_any_element()
        }
        // A dark tile in light mode, a light one in dark, as on upsy.ai.
        "upsy" => {
            let dark = gpui_component::theme::Theme::global(cx).is_dark();
            embedded(format!("icons/mcp-upsy{}.svg", if dark { "-dark" } else { "" }), size * 0.92).into_any_element()
        }
        // A black disc in light mode, inverted in dark, as on executor.sh.
        "executor" => {
            let dark = gpui_component::theme::Theme::global(cx).is_dark();
            embedded(format!("images/mcp-executor{}.png", if dark { "-dark" } else { "" }), size).into_any_element()
        }
        // Its picture has room around the mark: a little larger to match the others.
        "deepwiki" => embedded("images/mcp-deepwiki.png".into(), size * 1.15).into_any_element(),
        _ => return None,
    })
}

/// An app's mark at `size`: its logo for a ready-made one, else what it is, a server on the web or a command on this
/// Mac (no made-up logo).
pub fn app_icon(preset: Option<&str>, local: bool, size: f32, cx: &App) -> AnyElement {
    if let Some(icon) = preset.and_then(|preset| preset_icon(preset, size, cx)) {
        return icon;
    }
    crate::ui::icon_sized(if local { "square-terminal" } else { "globe" }, size * 0.86, cx.colors().accent)
        .into_any_element()
}

/// The mark of the user's app `id` (the composer's "@" list), from the store when it's loaded.
pub fn icon_for_app(id: &str, size: f32, cx: &App) -> AnyElement {
    let app =
        store_if_loaded(cx).and_then(|store| store.read(cx).app(id).map(|app| (app.preset.clone(), app.is_local())));
    match app {
        Some((preset, local)) => app_icon(preset.as_deref(), local, size, cx),
        None => app_icon(None, false, size, cx),
    }
}

/// A command and its arguments as one line to edit, quoted where a word needs it.
pub fn command_line(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|word| {
            if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./@:=+,%~".contains(c)) {
                word.to_string()
            } else {
                format!("'{}'", word.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A footnote in the secondary colour (the page's small print).
pub fn small_print(text: impl Into<gpui::SharedString>, cx: &App) -> gpui::Div {
    div().footnote().text_color(cx.colors().text_secondary).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ready_made_app_has_its_logo_bundled() {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        for preset in filo_local::apps::PRESETS {
            let files: Vec<String> = match preset.id {
                "deepwiki" => vec!["images/mcp-deepwiki.png".into()],
                "context7" => vec!["icons/mcp-context7.svg".into(), "icons/mcp-context7-dark.svg".into()],
                "upsy" => vec!["icons/mcp-upsy.svg".into(), "icons/mcp-upsy-dark.svg".into()],
                "executor" => vec!["images/mcp-executor.png".into(), "images/mcp-executor-dark.png".into()],
                id => vec![format!("icons/mcp-{id}.svg")],
            };
            for file in files {
                assert!(assets.join(&file).is_file(), "{}: no {file}", preset.id);
            }
        }
    }

    #[test]
    fn command_lines_quote_only_what_needs_it() {
        assert_eq!(
            command_line("npx", &["-y".into(), "@playwright/mcp@latest".into()]),
            "npx -y @playwright/mcp@latest"
        );
        assert_eq!(
            command_line("npx", &["/Users/me/My Docs".into(), "it's".into()]),
            "npx '/Users/me/My Docs' 'it'\\''s'"
        );
    }

    #[test]
    fn rows_read_the_backends_apps() {
        let row: AppRow = serde_json::from_value(json!({
            "id": "a1", "name": "Linear", "transport": "http", "url": "https://mcp.linear.app/mcp", "auth": "oauth", "enabled": true,
            "status": "ready", "tools": [{ "name": "list_issues", "readOnly": true }, { "name": "save_issue" }], "allowedTools": ["list_issues"],
            "oauth": { "registeredClientId": "c1" }, "busy": null, "serverId": "filo-linear", "allTools": true,
        }))
        .unwrap();
        assert!(row.all_tools);
        assert_eq!(row.place(), "mcp.linear.app");
        assert_eq!(row.status_line().0, "Connected · 1 of 2 tools on");
        assert!(row.tool_allowed("list_issues") && !row.tool_allowed("save_issue"));
        assert_eq!(row.tools[1].label(), "Save issue");
    }
}

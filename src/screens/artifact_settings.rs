// The Artifacts tab's settings (its sliders button, shown only while Claude Code is signed in to claude.ai): "Share to
// claude.ai", off by default. Artifacts stay on this Mac either way; on, Claude Code may publish one to the user's
// claude.ai account when they ask, in a conversation or with an artifact's Share button, and the link stays on the
// artifact (crates/filo-local `artifact_share`, local_backend/artifacts.rs `artifactShare:*`).

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, px,
};
use gpui_component::Disableable as _;
use gpui_component::switch::Switch;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend;
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, ListRow};

/// `artifactShare:settings`: the switch, and the claude.ai account Claude Code is signed in to.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ShareSettings {
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub on: bool,
}

impl ShareSettings {
    /// Sharing works now: on, and signed in to claude.ai.
    pub fn sharing(&self) -> bool {
        self.on && self.account.is_some()
    }
}

pub struct ArtifactSettingsScreen {
    settings: Option<ShareSettings>,
    switching: Option<bool>,
    _sub: Task<()>,
}

impl ArtifactSettingsScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = backend::subscribe(
            cx,
            "artifactShare:settings",
            json!({}),
            |this: &mut Self, result: Result<ShareSettings, String>, cx| {
                this.settings = result.ok();
                cx.notify();
            },
        );
        ArtifactSettingsScreen { settings: None, switching: None, _sub: sub }
    }

    fn set_on(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.switching = Some(on);
        cx.notify();
        backend::call_in(
            window,
            cx,
            "artifactShare:setOn",
            json!({ "on": on }),
            |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.switching = None;
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t change it", error);
                }
                cx.notify();
            },
        );
    }
}

impl Render for ArtifactSettingsScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let account = self.settings.as_ref().and_then(|settings| settings.account.clone());
        let on = self.switching.unwrap_or_else(|| self.settings.as_ref().is_some_and(|settings| settings.on));
        let body = match &account {
            Some(account) => {
                let row = ListRow::new("artifacts-share-claude", "Share to claude.ai")
                    .subtitle("When you ask, Claude Code publishes an artifact to your claude.ai account and Filo keeps the link on it. Artifacts still live here; Codex never shares.")
                    .trailing(
                        Switch::new("artifacts-share-claude-switch")
                            .checked(on)
                            .disabled(self.settings.is_none() || self.switching.is_some())
                            .on_click(cx.listener(|this, checked: &bool, window, cx| this.set_on(*checked, window, cx))),
                    )
                    .into_any_element();
                let footer = format!("Shared pages go to {account}, private until you share them on claude.ai.");
                ui::section(Some("Sharing"), Some(footer.into()), vec![row], cx).into_any_element()
            }
            None => div()
                .callout()
                .text_color(colors.text_secondary)
                .child("Sign Claude Code in to claude.ai to share artifacts there. Until then, they stay on this Mac.")
                .into_any_element(),
        };
        ui::sub_page(
            ui::sub_page_header("artifact-settings", "Artifacts", vec![], cx),
            div()
                .id("artifact-settings-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(ui::page_column().pt(px(space::LG)).pb(px(space::XL)).child(body)),
            cx,
        )
    }
}

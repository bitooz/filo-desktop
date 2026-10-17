// Your name, from Settings: what Filo calls you. Enter or Save keeps it. The field is an agent's name field
// (agents.rs `name_input`, `name_field`, `NAME_MAX`).

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{InputEvent, InputState};
use serde_json::json;

use crate::backend::{self, backend};
use crate::model::UserProfile;
use crate::screens::agents::{NAME_MAX, name_field, name_input};
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, Button};

pub struct NameScreen {
    /// `None` until `users:current` first answers.
    initial: Option<Result<String, String>>,
    input: Entity<InputState>,
    saving: bool,
    error: Option<String>,
    _sub: Task<()>,
    _input_sub: Subscription,
}

impl NameScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| name_input(window, cx).placeholder("Your name"));
        let _input_sub =
            cx.subscribe_in(&input, window, |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    ui::cap_length(input, NAME_MAX, window, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.save(window, cx),
                _ => {}
            });
        // Like `backend::subscribe`, but with the window, to fill and focus the field.
        let mut rx = backend(cx).subscribe_raw("users:current", json!({}));
        let _sub = cx.spawn_in(window, async move |this, cx| {
            while let Some(item) = rx.recv().await {
                let parsed = item.and_then(|value| {
                    serde_json::from_value::<Option<UserProfile>>(value)
                        .map_err(|error| format!("Unexpected reply from users:current: {error}"))
                });
                if this.update_in(cx, |this, window, cx| this.profile_loaded(parsed, window, cx)).is_err() {
                    break;
                }
            }
        });
        NameScreen { initial: None, input, saving: false, error: None, _sub, _input_sub }
    }

    fn profile_loaded(
        &mut self,
        result: Result<Option<UserProfile>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = !matches!(self.initial, Some(Ok(_)));
        let result = result.map(|profile| profile.and_then(|profile| profile.user_name).unwrap_or_default());
        if first && let Ok(name) = &result {
            let name = name.clone();
            self.input.update(cx, |state, cx| {
                state.set_value(name, window, cx);
                state.focus(window, cx);
            });
        }
        self.initial = Some(result);
        cx.notify();
    }

    fn trimmed(&self, cx: &gpui::App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn can_save(&self, cx: &gpui::App) -> bool {
        let trimmed = self.trimmed(cx);
        let count = trimmed.chars().count();
        let initial = match &self.initial {
            Some(Ok(initial)) => initial.as_str(),
            _ => return false,
        };
        count > 0 && count <= NAME_MAX && trimmed != initial && !self.saving
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        self.saving = true;
        self.error = None;
        cx.notify();
        let user_name = self.trimmed(cx);
        backend::call_in(
            window,
            cx,
            "users:updateUserName",
            json!({ "userName": user_name }),
            |this: &mut Self, result: Result<serde_json::Value, String>, window, cx| {
                match result {
                    Ok(_) => crate::nav::back(window, cx),
                    Err(_) => {
                        this.saving = false;
                        this.error = Some("Couldn’t save your name. Try again.".into());
                    }
                }
                cx.notify();
            },
        );
    }
}

impl Render for NameScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let can_save = self.can_save(cx);
        let save = Button::primary("name-save", "Save")
            .small()
            .disabled(!can_save && !self.saving)
            .loading(self.saving)
            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        let body = match &self.initial {
            None => ui::skeleton_page(&[1], None, cx).into_any_element(),
            Some(Err(error)) => {
                div().p(px(space::XL)).child(ui::banner(error.clone(), ui::Tone::Danger, cx)).into_any_element()
            }
            Some(Ok(_)) => div()
                .id("name-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(
                    ui::page_column()
                        .pt(px(space::XXL))
                        .pb(px(48.))
                        .flex()
                        .flex_col()
                        .gap(px(space::SM))
                        .child(div().callout().semibold().text_color(colors.text_secondary).child("Name"))
                        .child(name_field(&self.input, window, cx))
                        .when_some(self.error.clone(), |this, error| {
                            this.child(div().footnote().text_color(colors.danger).child(error))
                        }),
                )
                .into_any_element(),
        };
        ui::sub_page(
            ui::sub_page_header("name", "Your name", vec![save.into_any_element()], cx),
            div().flex_1().min_h_0().child(body),
            cx,
        )
    }
}

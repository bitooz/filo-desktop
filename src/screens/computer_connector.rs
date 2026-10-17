// "Your computer" in Extensions, Apps: whether Filo and its workers may use the apps on this Mac (computer:appsOn, as
// `filo computer use` sets it), the permissions macOS needs for that, and how to stop Filo while it works. Each chat
// turn's `use_computer` server starts the Swift helper (computer-use/, filo_local `computer`); macOS counts its
// permissions as Filo's. While they run the page says which apps they're in, with Stop, which ends it and pauses Filo's
// use of the apps for ten minutes (`computer:stop`).

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::Disableable as _;
use gpui_component::switch::Switch;
use serde_json::{Value, json};

use super::connectors::icon_tile_sized;
use crate::backend;
use crate::computer_use;
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, Button, ListRow};

pub const NAME: &str = "Your computer";

/// How often the page asks whether Filo is using the apps (a turn's helper, in another process).
const STATUS_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    using: Option<Vec<String>>,
    stopped_until: Option<f64>,
}

pub struct ComputerConnectorScreen {
    on: Option<bool>,
    switching: Option<bool>,
    accessibility: bool,
    screen_recording: bool,
    /// The apps Filo is using now (none named when unsaid).
    using: Option<Vec<String>>,
    /// Filo was stopped: its calls are refused until then (ms).
    stopped_until: Option<f64>,
    _sub: Task<()>,
    _status: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ComputerConnectorScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = backend::subscribe(
            cx,
            "computer:appsOn",
            json!({}),
            |this: &mut Self, result: Result<bool, String>, cx| {
                this.on = result.ok();
                cx.notify();
            },
        );
        // Coming back from System Settings: read the permissions again.
        let subscriptions = vec![cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.read_permissions();
                cx.notify();
            }
        })];
        let status = cx.spawn(async move |this, cx| {
            loop {
                if this.update(cx, |this, cx| this.read_status(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(STATUS_EVERY).await;
            }
        });
        let mut this = ComputerConnectorScreen {
            on: None,
            switching: None,
            accessibility: false,
            screen_recording: false,
            using: None,
            stopped_until: None,
            _sub: sub,
            _status: status,
            _subscriptions: subscriptions,
        };
        this.read_permissions();
        this
    }

    fn read_permissions(&mut self) {
        self.accessibility = computer_use::accessibility_allowed();
        self.screen_recording = computer_use::screen_recording_allowed();
    }

    fn read_status(&mut self, cx: &mut Context<Self>) {
        backend::call(cx, "computer:status", json!({}), |this: &mut Self, result: Result<Status, String>, cx| {
            let Ok(status) = result else { return };
            if this.using != status.using || this.stopped_until != status.stopped_until {
                this.using = status.using;
                this.stopped_until = status.stopped_until;
                cx.notify();
            }
        });
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.using = None;
        crate::companion::run(crate::companion::Command::StopApps, cx);
        self.read_status(cx);
        cx.notify();
    }

    fn set_on(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.switching = Some(on);
        cx.notify();
        // Turning it on is when macOS should ask: its prompt lists Filo in Accessibility.
        if on && !self.accessibility {
            computer_use::ask_accessibility();
        }
        backend::call_in(
            window,
            cx,
            "computer:setAppsOn",
            json!({ "on": on }),
            move |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.switching = None;
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t change it", error);
                }
                cx.notify();
            },
        );
    }

    fn permission_row(
        &self,
        id: &'static str,
        title: &'static str,
        detail: &'static str,
        allowed: bool,
        ask: fn(),
        pane: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let row = ListRow::new(id, title).subtitle(detail);
        if allowed {
            return row
                .trailing(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .pr(px(space::XS))
                        .footnote()
                        .text_color(colors.success)
                        .child("Allowed"),
                )
                .into_any_element();
        }
        row.trailing(Button::secondary(SharedString::from(format!("{id}-allow")), "Allow…").small().on_click(
            cx.listener(move |this, _, _, cx| {
                ask();
                computer_use::open_settings(pane);
                this.read_permissions();
                cx.notify();
            }),
        ))
        .into_any_element()
    }
}

impl Render for ComputerConnectorScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        let colors = *cx.colors();
        let on = self.switching.or(self.on).unwrap_or(false);
        let using = self.using.clone().filter(|_| on);
        let stopped_until = self.stopped_until.filter(|_| on && using.is_none());
        let stop_keys =
            crate::hotkeys::label(crate::companion::settings(cx).shortcut(crate::companion::STOP_APPS).as_deref());

        let hero = div()
            .flex()
            .items_center()
            .gap(px(space::LG))
            .child(icon_tile_sized(ui::icon_sized("laptop", 30., colors.accent), 56., cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .child(div().title2().truncate().child(NAME))
                    .child(
                        div()
                            .subhead()
                            .text_color(colors.text_secondary)
                            .child("Filo clicks and types in the apps on this Mac."),
                    ),
            );

        let switch_row = ListRow::new("computer-on", "Let Filo use your apps")
            .subtitle("It works without asking first, even in Supervised mode.")
            .trailing(
                Switch::new("computer-on-switch")
                    .checked(on)
                    .disabled(self.on.is_none() || self.switching.is_some())
                    .on_click(cx.listener(|this, checked: &bool, window, cx| {
                        this.set_on(*checked, window, cx);
                    })),
            )
            .into_any_element();

        let permissions = vec![
            self.permission_row(
                "computer-accessibility",
                "Accessibility",
                "Required to read and click in windows.",
                self.accessibility,
                computer_use::ask_accessibility,
                "Privacy_Accessibility",
                cx,
            ),
            self.permission_row(
                "computer-screen-recording",
                "Screen Recording",
                "Optional, for screenshots.",
                self.screen_recording,
                computer_use::ask_screen_recording,
                "Privacy_ScreenCapture",
                cx,
            ),
        ];
        let permissions_footer: Option<SharedString> = (on && !self.accessibility)
            .then(|| "Filo can’t use your apps until Accessibility is allowed: turn Filo on in System Settings, Privacy & Security, Accessibility, then come back here.".into());

        let stop_rows = vec![
            ListRow::new("computer-stop-shortcut", "Stop shortcut")
                .subtitle("Stops Filo for ten minutes")
                .trailing(super::shortcuts::keycap(stop_keys, false, cx).flex_none())
                .into_any_element(),
        ];

        // The scroll fills the pane itself: stretched in a flex column it gave the text no definite width, so lines ran
        // past their cards and the window (connectors.rs `pane`).
        div().id("computer-connector-scroll").size_full().overflow_y_scroll().child(
            ui::page_column()
                .pt(px(space::SM))
                .pb(px(48.))
                .flex()
                .flex_col()
                .gap(px(space::XL))
                .child(hero)
                .when_some(using, |this, apps| {
                    let label = format!(
                        "Filo is using {} now.",
                        crate::companion::apps_label(&apps).unwrap_or_else(|| "your apps".into())
                    );
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::MD))
                            .child(div().flex_1().min_w_0().callout().child(label))
                            .child(
                                Button::danger("computer-stop", "Stop")
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                            ),
                    )
                })
                .when_some(stopped_until, |this, until| {
                    let at = crate::format::clock(until);
                    this.child(div().callout().text_color(colors.text_secondary).child(format!(
                        "Stopped: Filo won’t use your apps until {at}, or until you turn this off and on again."
                    )))
                })
                .child(ui::section(None, None, vec![switch_row], cx))
                .child(ui::section(Some("Permissions"), permissions_footer, permissions, cx))
                .child(ui::section(Some("While Filo works"), None, stop_rows, cx)),
        )
    }
}

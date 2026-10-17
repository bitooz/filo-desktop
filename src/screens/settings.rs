// Settings, the sidebar's bottom entry (also ⌘,): the user's name; Memories and Scheduled; Pages, Feed and Notes
// switched on or off (pages.rs, the welcome's same section); the app icon,
// desktop pet, appearance, shortcuts, and notifications; and Help: the welcome tour and Report a problem
// (screens/report.rs). There's no account; the name row shows a letter avatar. Notifications says whether macOS lets
// Filo show banners (with sound) and opens System Settings to switch them on. Builds that update (updater.rs) have an
// Updates switch: new builds download on their own, or wait for the sidebar's Update button.

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use crate::backend;
use crate::model::UserProfile;
use crate::nav::{self, Route};
use crate::notifications::{self, Permission};
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, ListRow};

pub struct SettingsScreen {
    profile: Option<Result<Option<UserProfile>, String>>,
    reminders: Option<usize>,
    active_routines: Option<usize>,
    notifications: Option<Permission>,
    /// Updates download on their own (updater.rs).
    auto_update: bool,
    _subs: Vec<Task<()>>,
    _checking: Option<Task<()>>,
    _activation: Subscription,
}

impl SettingsScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _subs = vec![
            backend::subscribe(cx, "users:current", json!({}), |this: &mut Self, result, cx| {
                this.profile = Some(result);
                cx.notify();
            }),
            backend::subscribe(
                cx,
                "reminders:myReminders",
                json!({}),
                |this: &mut Self, result: Result<Vec<Value>, String>, cx| {
                    this.reminders = result.ok().map(|reminders| reminders.len());
                    cx.notify();
                },
            ),
            backend::subscribe(
                cx,
                "routines:myRoutines",
                json!({}),
                |this: &mut Self, result: Result<Vec<Value>, String>, cx| {
                    this.active_routines = result.ok().map(|routines| {
                        routines
                            .iter()
                            .filter(|routine| routine.get("status").and_then(Value::as_str) == Some("active"))
                            .count()
                    });
                    cx.notify();
                },
            ),
        ];
        // Back from System Settings: what changed there shows.
        let _activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.check_notifications(cx);
            }
        });
        let mut this = SettingsScreen {
            profile: None,
            reminders: None,
            active_routines: None,
            notifications: None,
            auto_update: crate::updater::auto_download(),
            _subs,
            _checking: None,
            _activation,
        };
        this.check_notifications(cx);
        this
    }

    fn check_notifications(&mut self, cx: &mut Context<Self>) {
        let permission = notifications::permission();
        self._checking = Some(cx.spawn(async move |this, cx| {
            if let Ok(permission) = permission.await {
                this.update(cx, |this, cx| {
                    this.notifications = Some(permission);
                    cx.notify();
                })
                .ok();
            }
        }));
    }
}

impl Render for SettingsScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let theme = cx.theme();
        let mode = match theme.prefs.theme_mode.as_str() {
            "light" => "Light",
            "dark" => "Dark",
            _ => "Auto",
        };
        let mut appearance = vec![theme.family().label.clone(), mode.to_string()];
        if theme.high_contrast() {
            appearance.push("High contrast".into());
        }
        let appearance = appearance.join(" · ");

        let profile = match &self.profile {
            Some(Ok(profile)) => profile.as_ref(),
            _ => None,
        };
        let display_name =
            profile.and_then(|profile| profile.user_name.clone()).unwrap_or_else(|| "Your account".into());
        // No account: everything is kept on this Mac (backend.rs).
        let env_note = Some(SharedString::from("On this Mac, no account"));
        let error = match &self.profile {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        };
        let scheduled = self.reminders.unwrap_or(0) + self.active_routines.unwrap_or(0);
        let profile_row = ListRow::new("settings-name", display_name.clone())
            .leading(ui::letter_avatar(&display_name, 52., colors.accent_soft, colors.on_accent_soft))
            .subtitle_opt(env_note)
            .chevron()
            .on_click(|_, window, cx| nav::push(Route::Name, window, cx));

        let personal = vec![
            ListRow::new("settings-memories", "Memories")
                .icon("brain")
                .chevron()
                .on_click(|_, window, cx| nav::push(Route::Memories, window, cx))
                .into_any_element(),
            ListRow::new("settings-scheduled", "Scheduled")
                .icon("calendar-clock")
                .subtitle_opt((scheduled > 0).then(|| format!("{scheduled} upcoming")))
                .chevron()
                .on_click(|_, window, cx| nav::push(Route::Scheduled, window, cx))
                .into_any_element(),
        ];

        let app_icon_row = ListRow::new("settings-app-icon", "App icon")
            .icon("smile")
            .subtitle(crate::app_icon::chosen().label())
            .chevron()
            .on_click(|_, window, cx| nav::push(Route::AppIcon, window, cx));

        let appearance_row = ListRow::new("settings-appearance", "Appearance")
            .icon("palette")
            .subtitle(appearance)
            .chevron()
            .on_click(|_, window, cx| nav::push(Route::Appearance, window, cx));

        let companion = crate::companion::settings(cx);
        let pet_name =
            crate::companion::companion(cx).and_then(|filo| filo.read(cx).pet_look().map(crate::companion::pet_name));
        let pet_subtitle = match (pet_name, companion.pet) {
            (Some(name), true) => name,
            (Some(name), false) => format!("{name} · Hidden"),
            (None, true) => "On".into(),
            (None, false) => "Hidden".into(),
        };
        let pet_row = ListRow::new("settings-desktop-pet", "Desktop pet")
            .icon("paw-print")
            .subtitle(pet_subtitle)
            .chevron()
            .on_click(|_, window, cx| nav::push(Route::DesktopPet, window, cx));
        let shortcuts_row = ListRow::new("settings-shortcuts", "Shortcuts & menu bar")
            .icon("keyboard")
            .subtitle(format!(
                "{} opens Filo",
                crate::hotkeys::label(companion.shortcut(crate::companion::OPEN).as_deref())
            ))
            .chevron()
            .on_click(|_, window, cx| nav::push(Route::Shortcuts, window, cx));

        let welcome_row = ListRow::new("settings-welcome", "Welcome tour")
            .icon("lightbulb")
            .subtitle("Take the first-run tour again")
            .chevron()
            .on_click(|_, window, cx| crate::screens::welcome::show(window, cx));

        let report_row = ListRow::new("settings-report", "Report a problem")
            .icon("flag")
            .subtitle("Checks this Mac, then sends what it finds to Filo")
            .chevron()
            .on_click(|_, window, cx| nav::push(Route::Report(crate::report::Origin::Settings), window, cx));

        let notifications_row = self.notifications.map(|permission| {
            let subtitle = match permission {
                Permission::Allowed { sound: true } => "On, with sound",
                Permission::Allowed { sound: false } => "On, without sound · turn sound on in System Settings",
                Permission::Denied => "Off · turn them on in System Settings",
                Permission::NotAsked => "Not set up · click to allow",
                Permission::NotAnApp => "Only in Filo.app (scripts/app.sh)",
                Permission::NotSigned => "Not available in this build · macOS needs an Apple-signed Filo",
            };
            ListRow::new("settings-notifications", "Notifications")
                .icon("bell")
                .subtitle(subtitle)
                .chevron()
                .on_click(cx.listener(move |this, _, _, cx| {
                    match permission {
                        Permission::NotAsked => notifications::request(),
                        Permission::NotAnApp | Permission::NotSigned => return,
                        _ => notifications::open_settings(),
                    }
                    this.check_notifications(cx);
                }))
                .into_any_element()
        });
        let updates_row = crate::updater::updater(cx).map(|_| {
            let switch = gpui_component::switch::Switch::new("settings-updates-switch")
                .checked(self.auto_update)
                .on_click(cx.listener(|this, checked: &bool, window, cx| {
                    match crate::updater::set_auto_download(*checked, cx) {
                        Ok(()) => {
                            this.auto_update = *checked;
                            cx.notify();
                        }
                        Err(error) => ui::toast_error(window, cx, "Couldn’t change how Filo updates", error),
                    }
                }));
            ListRow::new("settings-updates", "Download updates automatically")
                .icon("refresh-cw")
                .subtitle(if self.auto_update {
                    "Checks every hour; a new version goes in when you restart or quit Filo"
                } else {
                    "Off · the Update button in the sidebar downloads one"
                })
                .trailing(crate::smoke::tracked("settings-updates-switch", switch))
                .into_any_element()
        });
        let app = [
            app_icon_row.into_any_element(),
            pet_row.into_any_element(),
            appearance_row.into_any_element(),
            shortcuts_row.into_any_element(),
        ]
        .into_iter()
        .chain(notifications_row)
        .chain(updates_row)
        .collect();
        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .when_some(crate::smoke::page_scroll("settings"), |this, handle| this.track_scroll(&handle))
            .child(crate::smoke::tracked(
                "settings-end",
                ui::page_column()
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .child(ui::page_header("Settings", vec![]))
                    .children(error.map(|error| ui::banner(error, ui::Tone::Danger, cx)))
                    .child(ui::section(Some("Profile"), None, vec![profile_row.into_any_element()], cx))
                    .child(ui::section(Some("Personal"), None, personal, cx))
                    .child(crate::pages::section("settings-pages", cx))
                    .child(ui::section(Some("App"), None, app, cx))
                    .child(ui::section(
                        Some("Help"),
                        None,
                        vec![welcome_row.into_any_element(), report_row.into_any_element()],
                        cx,
                    ))
                    .child(
                        div()
                            .text_center()
                            .footnote()
                            .text_color(colors.text_tertiary)
                            .child(format!("Filo for Mac {}", crate::updater::version_label())),
                    )
                    .children(crate::smoke::page_end("settings")),
            ))
    }
}

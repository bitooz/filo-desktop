// "Scheduled" (the app's /reminders): routines on top, then pending reminders with complete, snooze and cancel. What's
// no agent's is the main agent's; once another agent owns something, each owner gets a section under its name and
// picture: the main agent's first, then the others' in the Agents tab's order. Below them, the check-ins the agents
// planned for themselves (`agenda:myWakes`), which can be cancelled. Everything here fires on this Mac, also while Filo is
// closed (local_backend/scheduled.rs, launch_agent.rs).

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, ClickEvent, Context, ImageSource, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_component::WindowExt as _;
use serde::Deserialize;
use serde_json::json;

use super::agents::{AgentRow, agent_picture};
use super::routine_display::{Routine, format_routine_schedule};
use crate::avatars;
use crate::backend;
use crate::format;
use crate::nav::{self, Route};
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, Button, IconButton, ListRow};

const DONE_DISPLAY_MS: u64 = 420;
const COMPLETE_FAILED: &str = "Filo couldn’t complete this reminder. Please try again.";
const SNOOZE_FAILED: &str = "Filo couldn’t snooze this reminder. Please try again.";
const CANCEL_FAILED: &str = "Filo couldn’t cancel this reminder. Please try again.";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reminder {
    #[serde(rename = "_id")]
    id: String,
    title: String,
    note: Option<String>,
    due_at: f64,
    /// `alarm`; none is a plain notification.
    delivery: Option<String>,
    /// The agent that owns it; none for Filo's.
    agent_id: Option<String>,
    agent_name: Option<String>,
}

/// A routine and the agent that owns it (none for Filo's).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedRoutine {
    /// Its owner's id is the routine's own `agent_id`.
    #[serde(flatten)]
    routine: Routine,
    agent_name: Option<String>,
}

/// A check-in Filo or an agent planned for itself in a conversation.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wake {
    #[serde(rename = "_id")]
    id: String,
    title: String,
    #[serde(default)]
    note: String,
    run_at: f64,
    agent_id: Option<String>,
    agent_name: Option<String>,
}

impl Reminder {
    /// Reminders come as notifications on this Mac (there's no phone or call here); an alarm says so.
    fn delivery_label(&self) -> &'static str {
        if self.delivery.as_deref() == Some("alarm") { "alarm on this Mac" } else { "notification on this Mac" }
    }
}

enum Action {
    Complete,
    Snooze(f64),
    Cancel,
}

pub struct ScheduledScreen {
    reminders: Option<Result<Vec<Reminder>, String>>,
    routines: Option<Result<Vec<OwnedRoutine>, String>>,
    wakes: Vec<Wake>,
    /// The agents, for the owners' sections: the main agent owns what's no agent's.
    agents: Vec<AgentRow>,
    busy_id: Option<String>,
    done_id: Option<String>,
    now: f64,
    _reminders_sub: Task<()>,
    _routines_sub: Task<()>,
    _wakes_sub: Task<()>,
    _agents_sub: Task<()>,
    _clock: Task<()>,
}

impl ScheduledScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _reminders_sub =
            backend::subscribe(cx, "reminders:myReminders", json!({}), |this: &mut Self, result, cx| {
                this.reminders = Some(result);
                cx.notify();
            });
        let _routines_sub = backend::subscribe(cx, "routines:myRoutines", json!({}), |this: &mut Self, result, cx| {
            this.routines = Some(result);
            cx.notify();
        });
        let _wakes_sub = backend::subscribe(
            cx,
            "agenda:myWakes",
            json!({}),
            |this: &mut Self, result: Result<Vec<Wake>, String>, cx| {
                if let Ok(wakes) = result {
                    this.wakes = wakes;
                    cx.notify();
                }
            },
        );
        let _agents_sub = backend::subscribe(
            cx,
            "agents:list",
            json!({}),
            |this: &mut Self, result: Result<Vec<AgentRow>, String>, cx| {
                if let Ok(rows) = result {
                    this.agents = rows;
                    cx.notify();
                }
            },
        );
        let _clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                if this
                    .update(cx, |this, cx| {
                        this.now = format::now_ms();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        ScheduledScreen {
            reminders: None,
            routines: None,
            wakes: Vec::new(),
            agents: Vec::new(),
            busy_id: None,
            done_id: None,
            now: format::now_ms(),
            _reminders_sub,
            _routines_sub,
            _wakes_sub,
            _agents_sub,
            _clock,
        }
    }

    fn perform(&mut self, id: String, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let (path, args, failure) = match action {
            Action::Complete => ("reminders:completeMine", json!({ "id": id }), COMPLETE_FAILED),
            Action::Snooze(minutes) => ("reminders:snoozeMine", json!({ "id": id, "minutes": minutes }), SNOOZE_FAILED),
            Action::Cancel => ("reminders:cancelMine", json!({ "id": id }), CANCEL_FAILED),
        };
        self.busy_id = Some(id);
        cx.notify();
        backend::call_in(window, cx, path, args, move |this: &mut Self, result: Result<bool, String>, window, cx| {
            this.busy_id = None;
            this.done_id = None;
            if result != Ok(true) {
                if let Err(error) = &result {
                    eprintln!("{path}: {error}");
                }
                ui::toast_error(window, cx, "Couldn’t update reminder", failure);
            }
            cx.notify();
        });
    }

    fn complete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy_id.is_some() || self.done_id.is_some() {
            return;
        }
        self.done_id = Some(id.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(DONE_DISPLAY_MS)).await;
            this.update_in(cx, |this, window, cx| this.perform(id, Action::Complete, window, cx)).ok();
        })
        .detach();
    }

    fn choose_snooze(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let colors = *cx.colors();
            let (view, id) = (view.clone(), id.clone());
            dialog
                .title(div().headline().child("Snooze reminder"))
                .child(div().callout().text_color(colors.text_secondary).child("When should Filo remind you again?"))
                .footer(move |_, _, _, _| {
                    let option = |label: &'static str, minutes: f64| {
                        let (view, id) = (view.clone(), id.clone());
                        Button::primary(SharedString::from(format!("snooze-{minutes}")), label).small().on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                view.update(cx, |this, cx| {
                                    this.perform(id.clone(), Action::Snooze(minutes), window, cx)
                                })
                                .ok();
                            },
                        )
                    };
                    vec![
                        Button::secondary("snooze-cancel", "Cancel")
                            .small()
                            .on_click(|_, window, cx| window.close_dialog(cx))
                            .into_any_element(),
                        option("10 minutes", 10.).into_any_element(),
                        option("1 hour", 60.).into_any_element(),
                    ]
                })
        });
    }

    fn confirm_cancel(&mut self, reminder: &Reminder, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let id = reminder.id.clone();
        ui::confirm(
            window,
            cx,
            "Cancel reminder?",
            reminder.title.clone(),
            "Cancel reminder",
            "Keep",
            true,
            move |window, cx| {
                this.update(cx, |this, cx| this.perform(id.clone(), Action::Cancel, window, cx)).ok();
            },
        );
    }

    fn cancel_wake(&mut self, wake: &Wake, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let id = wake.id.clone();
        ui::confirm(
            window,
            cx,
            "Cancel check-in?",
            wake.title.clone(),
            "Cancel check-in",
            "Keep",
            true,
            move |window, cx| {
                let id = id.clone();
                this.update(cx, |_, cx| {
                    backend::call_in(
                        window,
                        cx,
                        "agenda:cancelWakeMine",
                        json!({ "id": id }),
                        |_: &mut Self, result: Result<bool, String>, window, cx| {
                            if result != Ok(true) {
                                ui::toast_error(
                                    window,
                                    cx,
                                    "Couldn’t cancel the check-in",
                                    "Filo couldn’t cancel this check-in. Please try again.",
                                );
                            }
                        },
                    );
                })
                .ok();
            },
        );
    }

    /// The check-ins planned, the soonest first: whose, when and the note.
    fn render_wakes(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let rows = self
            .wakes
            .iter()
            .map(|wake| {
                let who = match self.owner_of(&wake.agent_id) {
                    Some(id) => self
                        .agents
                        .iter()
                        .find(|row| row.id == id)
                        .map(|row| row.name.clone())
                        .or(wake.agent_name.clone())
                        .unwrap_or_else(|| "Agent".into()),
                    None => "Agent".into(),
                };
                let note = if wake.note.is_empty() { String::new() } else { format!(" · {}", wake.note) };
                let cancel = wake.clone();
                ListRow::new(SharedString::from(format!("wake-{}", wake.id)), wake.title.clone())
                    .icon("bell")
                    .subtitle(format!("{who} · {}{note}", format::reminder_due(wake.run_at, self.now)))
                    .trailing(
                        IconButton::new(SharedString::from(format!("wake-cancel-{}", wake.id)), "x")
                            .small()
                            .color(colors.text_secondary)
                            .tooltip("Cancel")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.cancel_wake(&cancel, window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        ui::section(
            Some("Check-ins"),
            Some("Times Filo and your agents planned to come back to a conversation.".into()),
            rows,
            cx,
        )
        .mt(px(space::XXXL))
        .into_any_element()
    }

    fn render_routines(&self, routines: &[&Routine], cx: &mut Context<Self>) -> AnyElement {
        let rows = routines
            .iter()
            .map(|routine| {
                let schedule = format_routine_schedule(routine);
                let subtitle = if routine.status == "paused" { format!("Paused · {schedule}") } else { schedule };
                let id = routine.id.clone();
                ListRow::new(SharedString::from(format!("routine-{}", routine.id)), routine.title.clone())
                    .icon("repeat")
                    .subtitle(subtitle)
                    .chevron()
                    .on_click(move |_, window, cx| nav::push(Route::Routine(id.clone()), window, cx))
                    .into_any_element()
            })
            .collect();
        ui::section(Some("Routines"), None, rows, cx).mt(px(space::LG)).into_any_element()
    }

    /// Routines, then reminders under their own label when there are both.
    fn render_items(&self, routines: &[&Routine], reminders: &[&Reminder], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let colors = *cx.colors();
        let mut items = Vec::new();
        if !routines.is_empty() {
            items.push(self.render_routines(routines, cx));
        }
        if !reminders.is_empty() {
            let mut list = div().mt(px(space::LG)).flex().flex_col();
            if !routines.is_empty() {
                list = list.child(
                    div()
                        .px(px(space::SM))
                        .pb(px(space::XS))
                        .subhead()
                        .semibold()
                        .text_color(colors.text_secondary)
                        .child("Reminders"),
                );
            }
            list = list.children(reminders.iter().map(|reminder| self.render_reminder(reminder, cx)));
            items.push(list.into_any_element());
        }
        items
    }

    /// The main agent's id, once the agents are in.
    fn main_id(&self) -> Option<String> {
        self.agents.iter().find(|row| row.main).map(|row| row.id.clone())
    }

    /// Whose something is: its agent's, else the main agent's (none until the agents are in).
    fn owner_of(&self, agent_id: &Option<String>) -> Option<String> {
        agent_id.clone().or_else(|| self.main_id())
    }

    /// Who owns something, with name, picture and the colour behind it: the main agent first, then the others in the
    /// Agents tab's order.
    fn owners(
        &self,
        routines: &[OwnedRoutine],
        reminders: &[Reminder],
    ) -> Vec<(Option<String>, String, ImageSource, Option<String>)> {
        let owned = routines
            .iter()
            .map(|owned| (self.owner_of(&owned.routine.agent_id), &owned.agent_name))
            .chain(reminders.iter().map(|reminder| (self.owner_of(&reminder.agent_id), &reminder.agent_name)));
        let mut owners: Vec<(Option<String>, &Option<String>)> = Vec::new();
        for (id, name) in owned {
            if !owners.iter().any(|(seen, _)| *seen == id) {
                owners.push((id, name));
            }
        }
        let main = self.main_id();
        owners.sort_by_key(|(id, _)| {
            (*id != main, self.agents.iter().position(|row| Some(&row.id) == id.as_ref()).unwrap_or(usize::MAX))
        });
        // One missing from the Agents tab's list keeps the name it came with.
        owners
            .into_iter()
            .map(|(id, name)| match self.agents.iter().find(|row| Some(&row.id) == id.as_ref()) {
                Some(row) => (id, row.name.clone(), row.image(), row.background.clone()),
                None => (
                    id,
                    name.clone().unwrap_or_else(|| "Agent".into()),
                    avatars::animal_image(avatars::DEFAULT_AVATAR),
                    None,
                ),
            })
            .collect()
    }

    fn render_owner(
        &self,
        name: String,
        picture: ImageSource,
        background: Option<String>,
        first: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .mt(px(if first { space::LG } else { space::XXXL }))
            .px(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(agent_picture(picture, background.as_deref(), 28., cx))
            .child(div().min_w_0().headline().truncate().child(name))
            .into_any_element()
    }

    fn render_reminder(&self, reminder: &Reminder, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let busy = self.busy_id.as_deref() == Some(reminder.id.as_str());
        let done = self.done_id.as_deref() == Some(reminder.id.as_str());
        let overdue = reminder.due_at < self.now;
        let key = reminder.id.clone();

        let ring = div()
            .id(SharedString::from(format!("ring-{key}")))
            .mt(px(1.))
            .size(px(26.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(13.))
            .border(px(1.5))
            .border_color(colors.text_secondary)
            .when(done, |this| this.bg(colors.accent).border_color(colors.accent))
            .map(|this| {
                if done {
                    this.child(ui::icon_sized("check", 15., colors.on_accent))
                } else if busy {
                    this.child(ui::spinner(16., cx))
                } else {
                    this.child(ui::icon_sized("check", 14., colors.text_secondary.opacity(0.45)))
                }
            })
            .when(!busy && !done, |this| {
                let id = reminder.id.clone();
                this.cursor_pointer()
                    .hover(|style| style.bg(colors.selected_fill))
                    .active(|style| style.opacity(0.62))
                    .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Mark done").build(window, cx))
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| this.complete(id.clone(), window, cx)),
                    )
            });

        let copy = div()
            .relative()
            .flex_1()
            .min_w_0()
            .pr(px(space::XS))
            .flex()
            .flex_col()
            .when(done, |this| {
                this.opacity(0.55).child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(10.))
                        .h(px(1.5))
                        .rounded(px(1.))
                        .bg(colors.text_secondary)
                        .with_animation(
                            SharedString::from(format!("strike-{key}")),
                            Animation::new(Duration::from_millis(300)),
                            |line, delta| line.w(relative(delta)),
                        ),
                )
            })
            .child(div().body().bold().child(reminder.title.clone()))
            .when_some(reminder.note.clone().filter(|note| !note.is_empty()), |this, note| {
                this.child(
                    div().mt(px(space::XS)).footnote().text_color(colors.text_secondary).line_clamp(2).child(note),
                )
            })
            .child(
                div()
                    .mt(px(space::XS))
                    .caption()
                    .text_color(if overdue { colors.danger } else { colors.text_secondary })
                    .child(format!(
                        "{} · {}",
                        format::reminder_due(reminder.due_at, self.now),
                        reminder.delivery_label()
                    )),
            );

        let snooze_id = reminder.id.clone();
        let cancel_reminder = reminder.clone();
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .map(|mut this| {
                this.style().align_self = Some(gpui::AlignSelf::Center);
                this
            })
            .child(
                IconButton::new(SharedString::from(format!("snooze-{key}")), "clock-3")
                    .small()
                    .color(colors.text_secondary)
                    .tooltip("Snooze")
                    .disabled(busy || done)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.choose_snooze(snooze_id.clone(), window, cx)
                    })),
            )
            .child(
                IconButton::new(SharedString::from(format!("cancel-{key}")), "x")
                    .small()
                    .color(colors.text_secondary)
                    .tooltip("Cancel")
                    .disabled(busy || done)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.confirm_cancel(&cancel_reminder, window, cx)
                    })),
            );

        div()
            .flex()
            .items_start()
            .gap(px(space::MD))
            .py(px(space::MD + space::XXS))
            .border_b_1()
            .border_color(colors.border)
            .child(ring)
            .child(copy)
            .child(actions)
            .into_any_element()
    }
}

impl Render for ScheduledScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = ui::sub_page_header("scheduled", "Scheduled", vec![], cx);

        let error = match (&self.reminders, &self.routines) {
            (Some(Err(error)), _) | (_, Some(Err(error))) => Some(error.clone()),
            _ => None,
        };
        let body: AnyElement = if let Some(error) = error {
            ui::page_column().pt(px(space::LG)).child(ui::banner(error, ui::Tone::Danger, cx)).into_any_element()
        } else {
            match (&self.reminders, &self.routines) {
                (Some(Ok(reminders)), Some(Ok(routines))) => {
                    if reminders.is_empty() && routines.is_empty() && self.wakes.is_empty() {
                        ui::empty_state(
                            "calendar-clock",
                            "Nothing scheduled yet.",
                            Some(
                                "Ask Filo to remind you of something, or to do a job on a schedule, like a morning inbox brief."
                                    .into(),
                            ),
                            None,
                            cx,
                        )
                        .into_any_element()
                    } else {
                        let mut column = ui::page_column().pb(px(48.)).flex().flex_col();
                        let owners = self.owners(routines, reminders);
                        let main = self.main_id();
                        if owners.iter().all(|(id, _, _, _)| *id == main) {
                            let routines: Vec<&Routine> = routines.iter().map(|owned| &owned.routine).collect();
                            let reminders: Vec<&Reminder> = reminders.iter().collect();
                            column = column.children(self.render_items(&routines, &reminders, cx));
                        } else {
                            for (index, (owner, name, picture, background)) in owners.into_iter().enumerate() {
                                let routines: Vec<&Routine> = routines
                                    .iter()
                                    .filter(|owned| self.owner_of(&owned.routine.agent_id) == owner)
                                    .map(|owned| &owned.routine)
                                    .collect();
                                let reminders: Vec<&Reminder> = reminders
                                    .iter()
                                    .filter(|reminder| self.owner_of(&reminder.agent_id) == owner)
                                    .collect();
                                column = column.child(self.render_owner(name, picture, background, index == 0, cx));
                                column = column.children(self.render_items(&routines, &reminders, cx));
                            }
                        }
                        if !self.wakes.is_empty() {
                            column = column.child(self.render_wakes(cx));
                        }
                        div().id("scheduled-scroll").size_full().overflow_y_scroll().child(column).into_any_element()
                    }
                }
                _ => ui::skeleton_page(&[4, 3], Some(28.), cx).into_any_element(),
            }
        };

        ui::sub_page(header, body, cx)
    }
}

// A routine's page: Active
// switch, days, time, instructions, Run now, past runs and Delete. A run is a turn
// on this Mac: an agent's opens its conversation; Filo's (whose chats have no page)
// opens in place, showing what it sent.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use serde::Deserialize;
use serde_json::{Value, json};

use super::routine_display::{Routine, WEEKDAY_FULL_NAMES, WEEKDAY_LETTERS, format_routine_time, routine_run_label};
use crate::backend;
use crate::format;
use crate::nav;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton, ListRow};

// Monday first, like the schedule summary.
const DAY_ORDER: [u32; 7] = [1, 2, 3, 4, 5, 6, 0];
const TITLE_MAX: usize = 120;
const INSTRUCTIONS_MAX: usize = 3_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    #[serde(rename = "_id")]
    id: String,
    state: String,
    created_at: f64,
    /// The conversation it ran in.
    chat_id: Option<String>,
    /// What it sent the user.
    summary: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Detail {
    routine: Routine,
    runs: Vec<Run>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Save,
    Run,
    Delete,
}

pub struct RoutineScreen {
    id: String,
    detail: Option<Result<Option<Detail>, String>>,
    title_input: Entity<InputState>,
    instructions_input: Entity<InputState>,
    /// The fields start from the routine the first time it loads.
    seeded: bool,
    busy: Option<Busy>,
    /// Minute of day being picked while the time editor is open.
    time_draft: Option<u32>,
    /// The run shown open (Filo's routines).
    open_run: Option<String>,
    _sub: Task<()>,
    _inputs: Vec<Subscription>,
}

impl RoutineScreen {
    pub fn new(id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Name").validate(|text, _| text.chars().count() <= TITLE_MAX)
        });
        let instructions_input = cx.new(|cx| {
            InputState::new(window, cx).multi_line(true).auto_grow(5, 16).placeholder("What should Filo do?")
        });
        let _inputs = vec![
            cx.subscribe_in(&title_input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&instructions_input, window, |_, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    crate::ui::cap_length(input, INSTRUCTIONS_MAX, window, cx);
                    cx.notify();
                }
            }),
        ];
        let _sub = backend::subscribe(cx, "routines:myRoutine", json!({ "id": id }), |this: &mut Self, result, cx| {
            this.detail = Some(result);
            cx.notify();
        });
        RoutineScreen {
            id,
            detail: None,
            title_input,
            instructions_input,
            seeded: false,
            busy: None,
            time_draft: None,
            open_run: None,
            _sub,
            _inputs,
        }
    }

    fn routine(&self) -> Option<&Routine> {
        match &self.detail {
            Some(Ok(Some(detail))) => Some(&detail.routine),
            _ => None,
        }
    }

    /// Runs `routines:<path>`; `false`/`null` means the routine is gone.
    fn perform(
        &mut self,
        busy: Option<Busy>,
        path: &str,
        args: Value,
        failure: &'static str,
        on_success: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if busy.is_some() {
            self.busy = busy;
            cx.notify();
        }
        let is_run = busy == Some(Busy::Run);
        backend::call_in(window, cx, path, args, move |this: &mut Self, result: Result<Value, String>, window, cx| {
            if busy.is_some() {
                this.busy = None;
            }
            cx.notify();
            let message = match result {
                Ok(Value::Bool(false) | Value::Null) => "Routine is no longer available".to_string(),
                Ok(_) => return on_success(this, window, cx),
                Err(error) => error,
            };
            eprintln!("{failure} {message}");
            // Run now shows why it was skipped ("The previous run was still working.").
            let shown = if is_run { message } else { failure.to_string() };
            ui::toast_error(window, cx, "Couldn’t update routine", shown);
        });
    }

    fn update(&mut self, patch: Value, failure: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let mut args = json!({ "id": self.id });
        if let (Some(args), Value::Object(patch)) = (args.as_object_mut(), patch) {
            args.extend(patch);
        }
        self.perform(None, "routines:updateMine", args, failure, |_, _, _| {}, window, cx);
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        let status = if active { "active" } else { "paused" };
        self.update(json!({ "status": status }), "Filo couldn’t change this routine. Please try again.", window, cx);
    }

    fn toggle_day(&mut self, day: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(routine) = self.routine() else { return };
        let has = routine.has_day(day);
        if has && routine.days.len() == 1 {
            return;
        }
        let mut days: Vec<f64> = routine.days.iter().copied().filter(|value| *value as u32 != day).collect();
        if !has {
            days.push(day as f64);
        }
        self.update(json!({ "days": days }), "Filo couldn’t change the days. Please try again.", window, cx);
    }

    fn set_time(&mut self, minute_of_day: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.time_draft = None;
        cx.notify();
        let Some(routine) = self.routine() else { return };
        if routine.minute_of_day as u32 == minute_of_day {
            return;
        }
        self.update(
            json!({ "minuteOfDay": minute_of_day as f64 }),
            "Filo couldn’t change the time. Please try again.",
            window,
            cx,
        );
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title_input.read(cx).value().to_string();
        let instructions = self.instructions_input.read(cx).value().to_string();
        let args = json!({ "id": self.id, "title": title, "instructions": instructions });
        self.perform(
            Some(Busy::Save),
            "routines:updateMine",
            args,
            "Filo couldn’t save your changes. Please try again.",
            |_, _, _| {},
            window,
            cx,
        );
    }

    fn run_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let args = json!({ "id": self.id });
        self.perform(
            Some(Busy::Run),
            "routines:runNowMine",
            args,
            "Filo couldn’t start this routine. Please try again.",
            |_, _, _| {},
            window,
            cx,
        );
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(routine) = self.routine() else { return };
        let title = format!("Delete “{}”?", routine.title);
        let view = cx.entity().downgrade();
        ui::confirm(window, cx, title, "Past results stay available.", "Delete", "Cancel", true, move |window, cx| {
            view.update(cx, |this, cx| {
                let args = json!({ "id": this.id });
                this.perform(
                    Some(Busy::Delete),
                    "routines:deleteMine",
                    args,
                    "Filo couldn’t delete this routine. Please try again.",
                    |_, window, cx| nav::back(window, cx),
                    window,
                    cx,
                );
            })
            .ok();
        });
    }

    fn render_time(&self, routine: &Routine, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let open = self.time_draft.is_some();
        let row = ListRow::new("routine-time", "Time")
            .trailing(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::XS))
                    .text_color(colors.text_secondary)
                    .body()
                    .child(format_routine_time(routine.minute_of_day))
                    .child(ui::icon_sized(if open { "chevron-up" } else { "chevron-down" }, 18., colors.text_tertiary)),
            )
            .on_click({
                let current = routine.minute_of_day as u32;
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.time_draft = if this.time_draft.is_some() { None } else { Some(current) };
                    cx.notify();
                })
            });
        let Some(draft) = self.time_draft else { return row.into_any_element() };

        let (hour, minute) = (draft / 60, draft % 60);
        let pm = hour >= 12;
        let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
        let step =
            |id: &'static str, label: String, up: fn(u32) -> u32, down: fn(u32) -> u32, cx: &mut Context<Self>| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .child(IconButton::new(SharedString::from(format!("{id}-up")), "chevron-up").small().on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.time_draft = this.time_draft.map(up);
                            cx.notify();
                        }),
                    ))
                    .child(div().w(px(44.)).text_center().title2().child(label))
                    .child(IconButton::new(SharedString::from(format!("{id}-down")), "chevron-down").small().on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.time_draft = this.time_draft.map(down);
                            cx.notify();
                        }),
                    ))
            };
        // Hours step within the same half of the day; minutes in fives, wrapping within the hour.
        let hour_up = |m: u32| (m / 720) * 720 + (m % 720 + 60) % 720;
        let hour_down = |m: u32| (m / 720) * 720 + (m % 720 + 660) % 720;
        let minute_up = |m: u32| (m / 60) * 60 + (m % 60 / 5 * 5 + 5) % 60;
        let minute_down = |m: u32| {
            let minute = m % 60;
            let next = if !minute.is_multiple_of(5) { minute - minute % 5 } else { (minute + 55) % 60 };
            (m / 60) * 60 + next
        };
        let editor = div()
            .px(px(space::LG))
            .py(px(space::MD))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .child(step("routine-hour", hour12.to_string(), hour_up, hour_down, cx))
            .child(div().title2().child(":"))
            .child(step("routine-minute", format!("{minute:02}"), minute_up, minute_down, cx))
            .child(div().w(px(150.)).ml(px(space::SM)).child(ui::segmented(
                "routine-ampm",
                vec![(false, "AM".into(), None), (true, "PM".into(), None)],
                &pm,
                {
                    let view = cx.entity().downgrade();
                    move |pm, _, cx| {
                        view.update(cx, |this, cx| {
                            this.time_draft = this.time_draft.map(|m| m % 720 + if pm { 720 } else { 0 });
                            cx.notify();
                        })
                        .ok();
                    }
                },
                cx,
            )))
            .child(div().flex_1())
            .child(Button::ghost("routine-time-cancel", "Cancel").small().on_click(cx.listener(
                |this, _: &ClickEvent, _, cx| {
                    this.time_draft = None;
                    cx.notify();
                },
            )))
            .child(
                Button::primary("routine-time-set", "Set time")
                    .small()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_time(draft, window, cx))),
            );
        div()
            .flex()
            .flex_col()
            .child(row)
            .child(div().mx(px(space::LG)).h(px(1.)).bg(colors.border.opacity(0.6)))
            .child(editor)
            .into_any_element()
    }

    /// A past run: when, how it went and the start of what it sent. An agent's opens its conversation; Filo's
    /// opens in place.
    fn render_run(&self, routine: &Routine, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let label = routine_run_label(&run.state);
        let preview = run
            .summary
            .as_deref()
            .or(run.error.as_deref())
            .map(|text| text.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim().to_string());
        let subtitle = match preview.filter(|text| !text.is_empty()) {
            Some(text) => format!("{label} · {text}"),
            None => label,
        };
        let row =
            ListRow::new(SharedString::from(format!("run-{}", run.id)), format::weekday_date_time(run.created_at))
                .subtitle(subtitle);
        if let (Some(_), Some(chat)) = (&routine.agent_id, run.chat_id.clone()) {
            return row
                .chevron()
                .on_click(move |_, window, cx| nav::open_chat(chat.clone(), window, cx))
                .into_any_element();
        }
        let open = self.open_run.as_deref() == Some(run.id.as_str());
        let text = run.summary.clone().or_else(|| run.error.clone());
        let id = run.id.clone();
        let row = row
            .trailing(ui::icon_sized(if open { "chevron-up" } else { "chevron-down" }, 18., colors.text_tertiary))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open_run = if this.open_run.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                cx.notify();
            }));
        match text.filter(|_| open) {
            Some(text) => div()
                .flex()
                .flex_col()
                .child(row)
                .child(
                    div()
                        .px(px(space::LG))
                        .pb(px(space::MD))
                        .callout()
                        .text_color(if run.error.is_some() && run.summary.is_none() {
                            colors.danger
                        } else {
                            colors.text
                        })
                        .child(text),
                )
                .into_any_element(),
            None => row.into_any_element(),
        }
    }

    fn render_detail(&self, detail: &Detail, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let routine = &detail.routine;
        let active = routine.active();
        let next_run = match routine.next_run_at {
            Some(next) if active => {
                let due = format::reminder_due(next, format::now_ms());
                let mut chars = due.chars();
                let lowered =
                    chars.next().map(|first| first.to_lowercase().chain(chars).collect::<String>()).unwrap_or_default();
                format!("Next run {lowered}")
            }
            _ => "Paused".into(),
        };

        let active_row = ListRow::new("routine-active", "Active")
            .subtitle(next_run)
            .trailing(
                Switch::new("routine-active-switch")
                    .checked(active)
                    .on_click(cx.listener(|this, checked: &bool, window, cx| this.set_active(*checked, window, cx))),
            )
            .into_any_element();

        let days = div()
            .px(px(space::LG))
            .py(px(space::MD))
            .flex()
            .gap(px(space::XS))
            .children(DAY_ORDER.iter().map(|&day| {
                let selected = routine.has_day(day);
                div()
                    .id(SharedString::from(format!("routine-day-{day}")))
                    .flex_1()
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::FULL))
                    .footnote()
                    .semibold()
                    .cursor_pointer()
                    .map(|this| {
                        if selected {
                            this.bg(colors.accent_soft)
                                .text_color(colors.on_accent_soft)
                                .border_1()
                                .border_color(colors.accent)
                        } else {
                            this.bg(colors.surface_strong).text_color(colors.text_secondary)
                        }
                    })
                    .hover(|style| style.opacity(0.85))
                    .tooltip(move |window, cx| {
                        let label =
                            format!("{}, {}", WEEKDAY_FULL_NAMES[day as usize], if selected { "on" } else { "off" });
                        gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.toggle_day(day, window, cx)))
                    .child(WEEKDAY_LETTERS[day as usize])
            }))
            .into_any_element();

        let schedule = ui::section(
            Some("Schedule"),
            routine.last_skip_reason.as_ref().map(|reason| SharedString::from(format!("Skipped last time: {reason}"))),
            vec![active_row, days, self.render_time(routine, cx)],
            cx,
        );

        let title = self.title_input.read(cx).value();
        let instructions = self.instructions_input.read(cx).value();
        let dirty = title.trim() != routine.title || instructions.trim() != routine.instructions;
        let blank = title.trim().is_empty() || instructions.trim().is_empty();
        let fields = div()
            .p(px(space::LG))
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .child(Input::new(&self.title_input))
            .child(div().min_h(px(120.)).child(Input::new(&self.instructions_input)))
            .when(dirty, |this| {
                this.child(
                    Button::primary("routine-save", "Save changes")
                        .full_width()
                        .disabled(blank)
                        .loading(self.busy == Some(Busy::Save))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx))),
                )
            })
            .into_any_element();
        let instructions_section = ui::section(
            Some("Instructions"),
            Some("Filo follows these each time the routine runs. It only sends, deletes or shares things when they say so.".into()),
            vec![fields],
            cx,
        );

        let run_now = Button::secondary("routine-run", "Run now")
            .icon("play")
            .full_width()
            .loading(self.busy == Some(Busy::Run))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.run_now(window, cx)));

        let runs = ui::section(
            Some("Runs"),
            detail.runs.is_empty().then(|| SharedString::from("Results show up here after the first run.")),
            detail.runs.iter().map(|run| self.render_run(routine, run, cx)).collect(),
            cx,
        );

        let delete = Button::danger("routine-delete", "Delete routine")
            .icon("trash-2")
            .full_width()
            .loading(self.busy == Some(Busy::Delete))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.confirm_delete(window, cx)));

        let _ = window;
        div()
            .id("routine-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(
                ui::page_column()
                    .pt(px(space::LG))
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .child(schedule)
                    .child(instructions_section)
                    .child(run_now)
                    .child(runs)
                    .child(delete),
            )
            .into_any_element()
    }
}

impl Render for RoutineScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Esc cancels the time being picked, as its Cancel does.
        if self.time_draft.is_some() {
            crate::escape::layer(window, cx, |this, _, cx| {
                this.time_draft = None;
                cx.notify();
            });
        }
        // Fill the fields once, when the routine first arrives.
        if !self.seeded
            && let Some(Ok(Some(detail))) = &self.detail
        {
            let (title, instructions) = (detail.routine.title.clone(), detail.routine.instructions.clone());
            self.seeded = true;
            self.title_input.update(cx, |state, cx| state.set_value(title, window, cx));
            self.instructions_input.update(cx, |state, cx| state.set_value(instructions, window, cx));
        }
        let header_title = self.routine().map(|routine| routine.title.clone()).unwrap_or_else(|| "Routine".into());
        let header = ui::sub_page_header("routine", header_title, vec![], cx);
        let body: AnyElement = match self.detail.clone() {
            None => ui::skeleton_page(&[2, 4], None, cx).into_any_element(),
            Some(Err(error)) => {
                ui::page_column().pt(px(space::LG)).child(ui::banner(error, ui::Tone::Danger, cx)).into_any_element()
            }
            Some(Ok(None)) => ui::empty_state(
                "repeat",
                "This routine isn’t available.",
                Some("It may have been deleted.".into()),
                None,
                cx,
            )
            .into_any_element(),
            Some(Ok(Some(detail))) => self.render_detail(&detail, window, cx),
        };
        ui::sub_page(header, body, cx)
    }
}

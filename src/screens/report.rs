// Report a problem (report.rs, docs/problem-reports.md): opened from a chat's failed reply, a coding task that failed,
// or Settings › Help. The checks run as soon as it opens, each row filling in as it finishes; a red one with a known fix
// has a button for it, and Check again runs them all once more. Under them, an optional "What happened?", one line
// saying what Send sends (Show all has the whole body), and Send, which says "Sent ✓" once Filo's backend has it, or
// why not (offline, too many reports) with Send again.

use filo_local::engine::Engine;
use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::backend::backend;
use crate::report::{self, Check, Details, Origin, SendError, Status};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, ListRow};

enum Sending {
    Idle,
    Sending,
    /// The backend has it, with its id when it said one.
    Sent(Option<String>),
    Failed(SendError),
}

pub struct ReportScreen {
    origin: Origin,
    engine: Engine,
    /// One per `report::CHECKS`, None while it runs.
    checks: Vec<Option<Check>>,
    details: Option<Details>,
    note: Entity<InputState>,
    /// The same id on every try, so a send that got through before an error isn't counted twice.
    report_id: String,
    sending: Sending,
    show_all: bool,
    _runs: Vec<Task<()>>,
    _send: Option<Task<()>>,
    _note: Subscription,
}

impl ReportScreen {
    pub fn new(origin: Origin, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let note = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 8)
                .placeholder("What were you doing, and what did you expect? (optional)")
        });
        let _note = cx.subscribe(&note, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let engine = report::engine_for(&origin);
        let mut this = ReportScreen {
            origin,
            engine,
            checks: Vec::new(),
            details: None,
            note,
            report_id: uuid::Uuid::new_v4().to_string(),
            sending: Sending::Idle,
            show_all: false,
            _runs: Vec::new(),
            _send: None,
            _note,
        };
        this.run_checks(cx);
        this
    }

    /// Runs every check (and reads the details again), each on its own thread so the quick ones show at once.
    fn run_checks(&mut self, cx: &mut Context<Self>) {
        self.checks = vec![None; report::CHECKS.len()];
        self.details = None;
        if matches!(self.sending, Sending::Failed(_)) {
            self.sending = Sending::Idle;
        }
        let engine = self.engine;
        let mut runs: Vec<Task<()>> = report::CHECKS
            .iter()
            .enumerate()
            .map(|(index, (id, _))| {
                let id = *id;
                cx.spawn(async move |this, cx| {
                    let check = cx.background_spawn(async move { report::run(id, engine) }).await;
                    this.update(cx, |this, cx| {
                        if let Some(slot) = this.checks.get_mut(index) {
                            *slot = Some(check);
                        }
                        cx.notify();
                    })
                    .ok();
                })
            })
            .collect();
        let origin = self.origin.clone();
        runs.push(cx.spawn(async move |this, cx| {
            let details = cx.background_spawn(async move { report::gather(&origin) }).await;
            this.update(cx, |this, cx| {
                this.details = Some(details);
                cx.notify();
            })
            .ok();
        }));
        self._runs = runs;
        cx.notify();
    }

    fn checking(&self) -> bool {
        self.details.is_none() || self.checks.iter().any(Option::is_none)
    }

    fn done_checks(&self) -> Vec<Check> {
        self.checks.iter().flatten().cloned().collect()
    }

    fn body(&self, cx: &Context<Self>) -> serde_json::Value {
        let details = self.details.clone().unwrap_or_default();
        report::payload(
            &self.report_id,
            &self.origin,
            self.engine,
            &self.done_checks(),
            &details,
            &self.note.read(cx).value(),
        )
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.checking() || matches!(self.sending, Sending::Sending | Sending::Sent(_)) {
            return;
        }
        let body = self.body(cx);
        let backend = backend(cx);
        let http = backend.http();
        let endpoint = report::endpoint();
        let sent = backend.run(async move { report::send(http, &endpoint, body).await });
        self.sending = Sending::Sending;
        self._send = Some(cx.spawn(async move |this, cx| {
            let result = sent.await;
            this.update(cx, |this, cx| {
                this.sending = match result {
                    Ok(id) => Sending::Sent(id),
                    Err(error) => Sending::Failed(error),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// For `FILO_OPEN=report-drive` (smoke.rs): whether checks run, each check, the preview line and how sending went
    /// (with the note typed so far).
    #[cfg(feature = "dev")]
    pub fn drive_state(&self, cx: &gpui::App) -> (bool, Vec<String>, String, String) {
        let checks = self
            .checks
            .iter()
            .flatten()
            .map(|check| {
                format!(
                    "{:?} {}: {}{}",
                    check.status,
                    check.label,
                    check.detail,
                    check.fix.as_ref().map(|fix| format!(" [{}]", fix.label())).unwrap_or_default()
                )
            })
            .collect();
        let preview =
            self.details.as_ref().map(|details| report::preview(&self.done_checks(), details)).unwrap_or_default();
        let sending = match &self.sending {
            Sending::Idle => format!("idle, note {:?}", self.note.read(cx).value()),
            Sending::Sending => "sending".to_string(),
            Sending::Sent(id) => format!("Sent ✓ {id:?}"),
            Sending::Failed(error) => format!("failed: {}", error.message()),
        };
        (self.checking(), checks, preview, sending)
    }

    fn check_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let (id, label) = report::CHECKS[index];
        let Some(check) = self.checks.get(index).cloned().flatten() else {
            return ListRow::new(SharedString::from(format!("report-check-{id}")), label)
                .leading(div().size(px(20.)).flex().items_center().justify_center().child(ui::spinner(16., cx)))
                .subtitle(if id == "test-reply" { "Asking it for a one-word reply…" } else { "Checking…" })
                .into_any_element();
        };
        let (icon, color) = match check.status {
            Status::Ok => ("circle-check", colors.success),
            Status::Warn => ("triangle-alert", colors.warning),
            Status::Fail => ("circle-x", colors.danger),
            Status::Skip => ("circle-minus", colors.text_tertiary),
        };
        let mut row = ListRow::new(SharedString::from(format!("report-check-{id}")), check.label.clone())
            .leading(div().size(px(20.)).flex().items_center().justify_center().child(ui::icon_sized(icon, 18., color)))
            .subtitle(check.detail.clone());
        if let Some(fix) = check.fix.clone().filter(|_| check.status == Status::Fail) {
            row = row.trailing(
                Button::secondary(SharedString::from(format!("report-fix-{id}")), fix.label()).small().on_click(
                    move |_, window, cx| {
                        if let Err(error) = fix.run() {
                            ui::toast_error(window, cx, "Couldn’t open it", error);
                        }
                    },
                ),
            );
        }
        row.into_any_element()
    }
}

impl Render for ReportScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let checking = self.checking();
        let check_again = Button::ghost("report-check-again", "Check again")
            .small()
            .icon("refresh-cw")
            .disabled(checking)
            .on_click(cx.listener(|this, _, _, cx| this.run_checks(cx)));
        let rows: Vec<AnyElement> = (0..report::CHECKS.len()).map(|index| self.check_row(index, cx)).collect();
        let fixable = self.checks.iter().flatten().any(|check| check.status == Status::Fail && check.fix.is_some());
        let checks_footer = fixable.then(|| "After a fix, click Check again.".into());

        let what = match &self.origin {
            Origin::Settings => None,
            Origin::Turn { .. } => Some("This report is about the reply that failed."),
            Origin::Task { .. } => Some("This report is about the coding task that failed."),
        };
        let preview = match (&self.details, checking) {
            (Some(details), false) => report::preview(&self.done_checks(), details),
            _ => "Checking this Mac…".to_string(),
        };
        let body =
            (self.show_all && !checking).then(|| serde_json::to_string_pretty(&self.body(cx)).unwrap_or_default());

        let (sent, status_line): (bool, Option<AnyElement>) =
            match &self.sending {
                Sending::Sent(id) => (
                    true,
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::XS))
                            .callout()
                            .medium()
                            .text_color(colors.success)
                            .child("Sent ✓")
                            .children(id.clone().map(|id| {
                                div().footnote().text_color(colors.text_tertiary).child(format!("Report {id}"))
                            }))
                            .into_any_element(),
                    ),
                ),
                Sending::Failed(error) => {
                    (false, Some(div().footnote().text_color(colors.danger).child(error.message()).into_any_element()))
                }
                _ => (false, None),
            };
        let send_label = match self.sending {
            Sending::Failed(_) => "Send again",
            _ => "Send",
        };
        let send = Button::primary("report-send", send_label)
            .icon("send")
            .loading(matches!(self.sending, Sending::Sending))
            .disabled(checking || sent)
            .on_click(cx.listener(|this, _, _, cx| this.send(cx)));
        let failed_send = matches!(self.sending, Sending::Failed(_));
        let copy = failed_send.then(|| {
            Button::ghost("report-copy", "Copy report").small().icon("copy").on_click(cx.listener(
                |this, _, window, cx| {
                    let text = serde_json::to_string_pretty(&this.body(cx)).unwrap_or_default();
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    ui::toast(window, cx, "Copied the report");
                },
            ))
        });

        let show_all = Button::ghost("report-show-all", if self.show_all { "Hide" } else { "Show all" })
            .small()
            .disabled(checking)
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_all = !this.show_all;
                cx.notify();
            }));

        let content = ui::page_column()
            .pt(px(space::LG))
            .pb(px(48.))
            .flex()
            .flex_col()
            .gap(px(space::XL))
            .children(what.map(|what| div().px(px(space::SM)).footnote().text_color(colors.text_secondary).child(what)))
            .child(ui::section(Some("Checks"), checks_footer, rows, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::SM))
                    .child(
                        div()
                            .px(px(space::SM))
                            .footnote()
                            .semibold()
                            .text_color(colors.text_secondary)
                            .child("What happened?"),
                    )
                    .child(crate::smoke::tracked_fill(
                        "report-note",
                        Input::new(&self.note).rounded(px(radius::MD)).bg(colors.surface),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::SM))
                    .child(
                        div()
                            .px(px(space::SM))
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().flex_1().min_w_0().footnote().text_color(colors.text_tertiary).child(preview))
                            .child(show_all),
                    )
                    .children(body.map(|body| {
                        div()
                            .id("report-body")
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .p(px(space::MD))
                            .rounded(px(radius::MD))
                            .bg(colors.surface)
                            .child(crate::screens::coding::mono(body, cx).text_color(colors.text_secondary))
                    }))
                    .child(
                        div()
                            .px(px(space::SM))
                            .pt(px(space::XS))
                            .flex()
                            .items_center()
                            .gap(px(space::MD))
                            .child(crate::smoke::tracked("report-send", send))
                            .children(copy)
                            .children(status_line),
                    ),
            );
        ui::sub_page(
            ui::sub_page_header("report", "Report a problem", vec![check_again.into_any_element()], cx),
            div().id("report-scroll").flex_1().min_h_0().overflow_y_scroll().child(content),
            cx,
        )
    }
}

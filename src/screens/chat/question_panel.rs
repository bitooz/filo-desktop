// The coding questions waiting on the user, in one compact panel just above the composer (the approved mock
// "Questions above the composer", variant F). One question at a time: the tool and task on top with a tab per
// question (✓ once answered) and Review; numbered answers with what each means and the recommended one; "Something
// else", which is typed in the composer; key hints, Back and Next or Skip at the bottom. Copy question and Open the
// task are in its ⋯ menu.
//
// Keys, while it has the keyboard: ↑↓ move the highlight (the first press puts it on the current answer, else the
// recommended one: a panel opens with none, so nothing looks picked or hovered), ↵ picks the highlighted answer and goes
// to the next question, ←→ switch questions, 1–9 pick, and typing a letter carries on in the composer ("Or type your
// own answer…", where ↵ answers). ↓ from the last answer goes down to the composer and ↑ on the composer's first line
// comes back to it; the highlight shows only while the panel has the keyboard, so nothing looks picked while the user
// types. The pointer moves the highlight too, and takes it off the row it leaves. A click picks and moves on. A
// lone question goes as soon as it's picked; with more, Review ends with Send answers. Several tasks asking at once
// queue: the panel shows one task's questions, then the next.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, ClickEvent, ClipboardItem, Context, Corner, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::Icon;
use gpui_component::Sizable as _;
use gpui_component::button::{Button as MenuButton, ButtonRounded, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::text::TextView;
use serde_json::{Value, json};

use super::coding_requests::{self, Answerer, ChatRequest, Choice, SKIPPED};
use crate::backend;
use crate::nav::{self, Route};
use crate::screens::coding::tool_tile;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// How long a question takes to ease in.
const EASE_IN: Duration = Duration::from_millis(170);

pub enum PanelEvent {
    /// Over to the composer for an answer in the user's words, with the letter they started typing.
    Compose(Option<String>),
    /// Answers went (the chat's line says what was answered), or the questions shown changed.
    Changed,
}

impl EventEmitter<PanelEvent> for QuestionPanel {}

pub struct QuestionPanel {
    /// The chat's requests, `ordered`.
    requests: Vec<ChatRequest>,
    answerer: Answerer,
    /// Picked here and not sent yet, by request id: the tool's own wording of the answer.
    picks: HashMap<String, String>,
    /// Sent and taken, by request id, until the backend's copy says so.
    sent: HashMap<String, String>,
    /// Answers on their way.
    sending: usize,
    error: Option<String>,
    /// The task whose questions show (the first in the queue), and which of them: its count is Review.
    shown: Option<String>,
    index: usize,
    /// The highlighted row (none until a key or the pointer puts it on one), and the question it was placed for.
    highlight: Option<usize>,
    highlight_for: Option<(String, usize)>,
    /// The keys moved it last (not the pointer): the next question shows it at once, on its current answer.
    keyed: bool,
    /// Counts each question shown, so each eases in.
    step: usize,
    /// How tall each question of the set (and Review, after them) draws, measured from hidden copies: the body keeps
    /// the tallest's height, so switching never changes the panel's size.
    heights: Rc<RefCell<HashMap<usize, f32>>>,
    focus: FocusHandle,
    #[cfg(feature = "dev")]
    fixture_attempts: u32,
}

impl QuestionPanel {
    pub fn new(answerer: Answerer, cx: &mut Context<Self>) -> Self {
        QuestionPanel {
            requests: Vec::new(),
            answerer,
            picks: HashMap::new(),
            sent: HashMap::new(),
            sending: 0,
            error: None,
            shown: None,
            index: 0,
            highlight: None,
            highlight_for: None,
            keyed: false,
            step: 0,
            heights: Rc::default(),
            focus: cx.focus_handle(),
            #[cfg(feature = "dev")]
            fixture_attempts: 0,
        }
    }

    #[cfg(feature = "dev")]
    pub fn set_answerer(&mut self, answerer: Answerer) {
        self.answerer = answerer;
    }

    /// The backend's newest copy of the chat's requests.
    pub fn set_requests(&mut self, requests: Vec<ChatRequest>, cx: &mut Context<Self>) {
        if requests == self.requests {
            return;
        }
        // What the backend now has decided (or no longer lists) needs no local copy.
        let open = |id: &str| requests.iter().any(|request| request.request_id == id && request.state == "pending");
        self.sent.retain(|id, _| open(id));
        self.picks.retain(|id, _| open(id));
        self.requests = requests;
        self.sync();
        cx.notify();
    }

    /// Waiting on the user, in the chat's order: pending, and not answered from here.
    fn waiting(&self) -> impl Iterator<Item = &ChatRequest> {
        self.requests
            .iter()
            .filter(|request| request.state == "pending" && !self.sent.contains_key(&request.request_id))
    }

    /// The questions shown: the first waiting task's, in the order asked.
    fn group(&self) -> Vec<ChatRequest> {
        let Some(task) = self.waiting().next().map(|request| request.task_id.clone()) else { return Vec::new() };
        self.waiting().filter(|request| request.task_id == task).cloned().collect()
    }

    /// Other tasks waiting their turn.
    fn queued(&self) -> usize {
        let mut tasks: Vec<&str> = self.waiting().map(|request| request.task_id.as_str()).collect();
        tasks.dedup();
        tasks.sort_unstable();
        tasks.dedup();
        tasks.len().saturating_sub(1)
    }

    /// Follows the queue: a new task's questions start at the first; the place stays in range as they change.
    fn sync(&mut self) {
        let group = self.group();
        let task = group.first().map(|request| request.task_id.clone());
        if task != self.shown {
            self.shown = task;
            self.index = 0;
            self.error = None;
            self.step += 1;
            self.heights.borrow_mut().clear();
            self.keyed = false;
        }
        let last = if group.len() > 1 { group.len() } else { 0 };
        if self.index > last {
            self.index = last;
            self.step += 1;
        }
        if let Some(task) = &self.shown {
            let at = (task.clone(), self.index);
            if self.highlight_for.as_ref() != Some(&at) {
                self.highlight = self.keyed.then(|| self.default_highlight(&group));
                self.highlight_for = Some(at);
            }
        }
    }

    /// Where the keys put the highlight first: on the answer picked, else on the recommended one (Review: on Send answers).
    fn default_highlight(&self, group: &[ChatRequest]) -> usize {
        let Some(request) = group.get(self.index) else { return group.len() };
        let choices = coding_requests::choices(request);
        match self.picks.get(&request.request_id) {
            Some(answer) => choices.iter().position(|choice| choice.answer == *answer).unwrap_or(choices.len()),
            None => choices.iter().position(|choice| choice.recommended).unwrap_or(0),
        }
    }

    /// How many of task `task_id`'s questions wait (the fixture's working panel follows it).
    #[cfg(feature = "dev")]
    pub fn waiting_for(&self, task_id: &str) -> usize {
        self.waiting().filter(|request| request.task_id == task_id).count()
    }

    pub fn is_open(&self) -> bool {
        self.waiting().next().is_some()
    }

    /// On a question (not Review): what's typed in the composer answers it.
    pub fn on_question(&self) -> bool {
        let group = self.group();
        !group.is_empty() && self.index < group.len()
    }

    /// The answers sent from here the backend doesn't say yet, for the chat's line.
    pub fn sent(&self) -> Vec<(String, String)> {
        let mut sent: Vec<(String, String)> =
            self.sent.iter().map(|(id, answer)| (id.clone(), answer.clone())).collect();
        sent.sort();
        sent
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn has_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Back up from the composer: the highlight on the bottom row (Something else, or Review's Send answers).
    pub fn from_below(&mut self, cx: &mut Context<Self>) {
        let group = self.group();
        self.highlight = Some(match group.get(self.index) {
            Some(request) => coding_requests::choices(request).len(),
            None => group.len(),
        });
        self.keyed = true;
        cx.notify();
    }

    /// What it shows, for the fixture's log: the task, the question (or Review), the highlighted row, and the picks.
    #[cfg(feature = "dev")]
    pub fn describe(&self) -> String {
        let group = self.group();
        let Some(first) = group.first() else { return "closed".into() };
        let at = match group.get(self.index) {
            Some(request) => format!("{} ({})", request.label(self.index + 1), request.request_id),
            None => "Review".into(),
        };
        let mut picks: Vec<String> = group
            .iter()
            .filter_map(|request| {
                self.picks.get(&request.request_id).map(|answer| format!("{}={answer}", request.request_id))
            })
            .collect();
        picks.sort();
        let place =
            if self.index < group.len() { format!("{} of {} · ", self.index + 1, group.len()) } else { String::new() };
        format!(
            "task {} · {place}{at} · highlight {} · picks {picks:?} · queued {}{}{}",
            first.task_id,
            self.highlight.map_or("none".to_string(), |row| (row + 1).to_string()),
            self.queued(),
            if self.sending > 0 { " · sending" } else { "" },
            self.error.as_ref().map(|error| format!(" · error {error}")).unwrap_or_default()
        )
    }

    fn go(&mut self, index: usize, cx: &mut Context<Self>) {
        let group = self.group();
        let last = if group.len() > 1 { group.len() } else { 0 };
        let index = index.min(last);
        if index != self.index {
            self.index = index;
            self.step += 1;
            self.sync();
            cx.notify();
        }
    }

    /// Answer `answer` to the question shown, then the next one unanswered (or Review); a lone question goes now.
    fn answer(&mut self, answer: String, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group();
        let Some(request) = group.get(self.index) else { return };
        if self.sending > 0 || answer.trim().is_empty() {
            return;
        }
        self.picks.insert(request.request_id.clone(), answer);
        if group.len() == 1 {
            self.send(window, cx);
            return;
        }
        let next =
            group.iter().position(|request| !self.picks.contains_key(&request.request_id)).unwrap_or(group.len());
        self.index = next;
        self.step += 1;
        self.sync();
        cx.notify();
    }

    fn pick(&mut self, choice: usize, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group();
        let Some(choice) =
            group.get(self.index).and_then(|request| coding_requests::choices(request).into_iter().nth(choice))
        else {
            return;
        };
        self.answer(choice.answer, window, cx);
    }

    /// Typed in the composer: the answer to the question shown. False when it isn't on one (the text is a message).
    pub fn take_typed(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.on_question() || self.sending > 0 {
            return false;
        }
        self.answer(text, window, cx);
        true
    }

    /// Sends the shown task's answers: each picked one, and for a question left unanswered, that it was skipped (a
    /// skipped permission request waits for an answer, and shows again).
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending > 0 {
            return;
        }
        let answers: Vec<(String, String)> = self
            .group()
            .iter()
            .filter_map(|request| match self.picks.get(&request.request_id) {
                Some(answer) => Some((request.request_id.clone(), answer.clone())),
                None if request.kind != "approval" => Some((request.request_id.clone(), SKIPPED.to_string())),
                None => None,
            })
            .collect();
        if answers.is_empty() {
            return;
        }
        self.sending = answers.len();
        self.error = None;
        cx.notify();
        #[cfg(feature = "dev")]
        let fail = matches!(self.answerer, Answerer::Fixture) && {
            self.fixture_attempts += 1;
            self.fixture_attempts == 1
        };
        for (id, answer) in answers {
            match self.answerer {
                Answerer::Backend => {
                    let args = json!({ "requestId": id, "answer": answer });
                    backend::call_in(
                        window,
                        cx,
                        "coding:answerRequest",
                        args,
                        move |this: &mut Self, result: Result<Value, String>, _, cx| {
                            this.sent_one(id, answer, result.map(|_| ()), cx);
                        },
                    );
                }
                #[cfg(feature = "dev")]
                Answerer::Fixture => {
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(700)).await;
                        this.update(cx, |this, cx| {
                            let result = if fail {
                                Err("Couldn't reach Filo. Check your connection.".to_string())
                            } else {
                                Ok(())
                            };
                            this.sent_one(id, answer, result, cx);
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
    }

    fn sent_one(&mut self, id: String, answer: String, result: Result<(), String>, cx: &mut Context<Self>) {
        self.sending = self.sending.saturating_sub(1);
        match result {
            Ok(()) => {
                self.picks.remove(&id);
                self.sent.insert(id, answer);
            }
            // Answered meanwhile (by the agent, or elsewhere): the backend's copy takes it off.
            Err(error) => self.error = Some(error),
        }
        if self.sending == 0 {
            self.sync();
            cx.emit(PanelEvent::Changed);
        }
        cx.notify();
    }

    /// The pointer on a row highlights it; leaving it takes the highlight off, unless the keys moved it elsewhere.
    fn hover_row(&mut self, row: usize, hovered: bool, cx: &mut Context<Self>) {
        let highlight = if hovered {
            Some(row)
        } else if self.highlight == Some(row) {
            None
        } else {
            return;
        };
        if highlight != self.highlight {
            self.highlight = highlight;
            self.keyed = false;
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return;
        }
        let group = self.group();
        if group.is_empty() {
            return;
        }
        let review = self.index >= group.len();
        let choices = group.get(self.index).map(coding_requests::choices).unwrap_or_default();
        // A question's answers and Something else; Review's questions and Send answers.
        let rows = if review { group.len() + 1 } else { choices.len() + 1 };
        let many = group.len() > 1;
        let key = keystroke.key.as_str();
        if matches!(key, "up" | "down" | "left" | "right" | "enter") {
            self.keyed = true;
        }
        match (key, self.highlight) {
            // No highlight yet: ↑, ↓ or ↵ puts it where it starts, so ↵ never picks a row that doesn't show.
            ("up" | "down" | "enter", None) => self.highlight = Some(self.default_highlight(&group)),
            ("up", Some(row)) => self.highlight = Some(row.saturating_sub(1)),
            // Down past the last answer: on to the composer, where Something else is typed.
            ("down", Some(row)) if !review && row + 1 >= rows => cx.emit(PanelEvent::Compose(None)),
            ("down", Some(row)) => self.highlight = Some((row + 1).min(rows - 1)),
            ("left", _) if many => self.go(self.index.saturating_sub(1), cx),
            ("right", _) if many => self.go(self.index + 1, cx),
            ("enter", Some(row)) if review => {
                if row >= group.len() {
                    self.send(window, cx);
                } else {
                    self.go(row, cx);
                }
            }
            ("enter", Some(row)) => {
                if row < choices.len() {
                    self.pick(row, window, cx);
                } else {
                    cx.emit(PanelEvent::Compose(None));
                }
            }
            (key, _) if !review && !modifiers.shift && key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => {
                let number = (key.as_bytes()[0] - b'0') as usize;
                if number >= 1 && number <= choices.len() {
                    self.pick(number - 1, window, cx);
                }
            }
            _ => {
                // A letter (or anything typed) starts the user's own answer in the composer.
                let typed = keystroke
                    .key_char
                    .clone()
                    .filter(|text| !text.is_empty() && text != " " && !text.chars().any(char::is_control));
                match typed {
                    Some(text) if !review => cx.emit(PanelEvent::Compose(Some(text))),
                    _ => return,
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ---- Drawing ----

    /// Tool and task, the queue, the tabs and ⋯.
    fn header(&self, group: &[ChatRequest], window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let colors = *cx.colors();
        let first = &group[0];
        let many = group.len() > 1;
        let tab = |id: String, label: String, on: bool, done: bool| {
            div()
                .id(SharedString::from(id))
                .flex_none()
                .max_w(px(150.))
                .px(px(space::SM + 1.))
                .py(px(space::XXS))
                .flex()
                .items_center()
                .gap(px(space::XS - 1.))
                .rounded(px(radius::FULL))
                .caption()
                .text_color(if on { colors.text } else { colors.text_secondary })
                .when(on, |this| this.bg(colors.surface_strong))
                .when(done, |this| this.child(ui::icon_sized("check", 11., colors.accent)))
                .child(div().min_w_0().truncate().child(label))
        };
        let mut tabs: Vec<gpui::AnyElement> = Vec::new();
        for (index, request) in group.iter().enumerate() {
            let done = many && self.picks.contains_key(&request.request_id);
            let element = tab(format!("question-tab-{index}"), request.label(index + 1), index == self.index, done);
            let element = if many {
                element.cursor_pointer().hover(|style| style.text_color(colors.text)).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.focus.focus(window);
                        this.go(index, cx);
                    },
                ))
            } else {
                element
            };
            tabs.push(crate::smoke::tracked(format!("question-tab-{index}"), element).into_any_element());
        }
        if many {
            let review = group.len();
            let element = tab("question-tab-review".into(), "Review".into(), self.index == review, false)
                .cursor_pointer()
                .hover(|style| style.text_color(colors.text))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.focus.focus(window);
                    this.go(review, cx);
                }));
            tabs.push(crate::smoke::tracked("question-tab-review", element).into_any_element());
        }
        let queued = self.queued();
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(space::SM - 2.))
            .child(tool_tile(first.tool_id(), 14., cx))
            .child(
                div().flex_1().min_w_0().caption().text_color(colors.text_secondary).truncate().child(first.context()),
            )
            .when(queued > 0, |this| {
                this.child(div().flex_none().caption().text_color(colors.text_tertiary).child(if queued == 1 {
                    "then 1 more".to_string()
                } else {
                    format!("then {queued} more")
                }))
            })
            .child(div().flex_none().flex().items_center().gap(px(space::XXS)).children(tabs))
            .child(self.menu(group, window, cx))
    }

    /// ⋯: Copy question, Open the task.
    fn menu(&self, group: &[ChatRequest], _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let copy = match group.get(self.index) {
            Some(request) => request.copy_text(),
            None => group.iter().map(ChatRequest::copy_text).collect::<Vec<_>>().join("\n\n"),
        };
        let task_id = group[0].task_id.clone();
        let label = if self.index < group.len() { "Copy question" } else { "Copy questions" };
        crate::smoke::tracked(
            "question-menu",
            MenuButton::new("question-menu")
                .ghost()
                .small()
                .compact()
                .rounded(ButtonRounded::Size(px(radius::FULL)))
                .w(px(24.))
                .h(px(22.))
                .icon(Icon::empty().path("icons/ellipsis.svg").text_color(colors.text_secondary))
                .dropdown_menu_with_anchor(Corner::BottomRight, move |menu, _, _| {
                    let (copy, task_id) = (copy.clone(), task_id.clone());
                    menu.item(PopupMenuItem::new(label).icon(Icon::empty().path("icons/copy.svg")).on_click(
                        move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            ui::toast(
                                window,
                                cx,
                                if label == "Copy question" { "Question copied" } else { "Questions copied" },
                            );
                        },
                    ))
                    .item(
                        PopupMenuItem::new("Open the task")
                            .icon(Icon::empty().path("icons/arrow-up-right.svg"))
                            .on_click(move |_, window, cx| nav::push(Route::CodingTask(task_id.clone()), window, cx)),
                    )
                }),
        )
    }

    /// A key hint: the key in a small box, then what it does.
    fn hint(key: &'static str, what: &'static str, cx: &App) -> gpui::Div {
        let colors = *cx.colors();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::XS))
            .child(
                div()
                    .min_w(px(16.))
                    .px(px(space::XS))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(colors.border)
                    .flex()
                    .justify_center()
                    .micro()
                    .text_color(colors.text)
                    .child(key),
            )
            .child(div().caption().text_color(colors.text_secondary).child(what))
    }

    /// A numbered (or ✎) answer row.
    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        id: String,
        mark: gpui::AnyElement,
        label: String,
        note: String,
        recommended: bool,
        picked: bool,
        row: usize,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = *cx.colors();
        // The keyboard's highlight only while the panel has it; with the composer's, the pointer's hover.
        let highlighted = focused && self.highlight == Some(row);
        div()
            .id(SharedString::from(id))
            .w_full()
            .px(px(space::SM + 1.))
            .py(px(space::XS + 2.))
            .flex()
            .items_start()
            .gap(px(space::SM + 2.))
            .rounded(px(radius::SM - 1.))
            .border_1()
            .border_color(if picked { colors.accent.opacity(0.7) } else { gpui::transparent_black() })
            .when(highlighted || picked, |this| this.bg(colors.surface_strong))
            .when(!focused, |this| this.hover(|style| style.bg(colors.surface_strong)))
            .when(self.sending == 0, |this| this.cursor_pointer())
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_row(row, *hovered, cx)))
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    // The label and its "Recommended" as one run of text, which wraps like the note under it.
                    .child(div().w_full().subhead().text_color(colors.text).child(label_text(
                        label,
                        recommended,
                        colors.accent,
                    )))
                    .when(!note.is_empty(), |this| {
                        this.child(div().w_full().caption().text_color(colors.text_secondary).child(note))
                    }),
            )
            .when(picked, |this| {
                this.child(div().flex_none().pt(px(2.)).child(ui::icon_sized("check", 14., colors.accent)))
            })
    }

    fn key_box(content: gpui::AnyElement, picked: bool, cx: &App) -> gpui::AnyElement {
        let colors = *cx.colors();
        div()
            .flex_none()
            .mt(px(1.))
            .size(px(20.))
            .rounded(px(6.))
            .border_1()
            .border_color(if picked { colors.accent } else { colors.border })
            .flex()
            .items_center()
            .justify_center()
            .caption()
            .text_color(if picked { colors.accent } else { colors.text_secondary })
            .child(content)
            .into_any_element()
    }

    /// Question `at` of the set; `live` is the one shown, the others are hidden copies to measure (`heights`), with
    /// views of their own and nothing the scripted checks find.
    fn question(
        &self,
        group: &[ChatRequest],
        at: usize,
        live: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let colors = *cx.colors();
        let request = &group[at];
        let track = |id: String, element: gpui::Stateful<gpui::Div>| {
            if live { crate::smoke::tracked_fill(id, element) } else { element.into_any_element() }
        };
        let copy = if live { "" } else { "measure-" };
        let focused = self.focus.is_focused(window);
        let choices: Vec<Choice> = coding_requests::choices(request);
        let picked = self.picks.get(&request.request_id).cloned();
        let own = picked.clone().filter(|answer| !choices.iter().any(|choice| choice.answer == *answer));
        let question_id = SharedString::from(format!("{copy}question-text-{}", request.request_id));
        let question = TextView::markdown(question_id.clone(), request.question.clone(), window, cx)
            .style(ui::markdown_style())
            .selectable(live);
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (index, choice) in choices.iter().enumerate() {
            let is_picked = picked.as_deref() == Some(choice.answer.as_str());
            let id = format!("question-option-{}", index + 1);
            let mark = Self::key_box(div().child((index + 1).to_string()).into_any_element(), is_picked, cx);
            let element = self
                .row(
                    id.clone(),
                    mark,
                    choice.label.clone(),
                    choice.note.clone(),
                    choice.recommended,
                    is_picked,
                    index,
                    focused,
                    cx,
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if event.standard_click() {
                        this.focus.focus(window);
                        this.pick(index, window, cx);
                    }
                }));
            rows.push(track(id, element));
        }
        let other = choices.len();
        let mark = Self::key_box(
            ui::icon_sized("pencil", 11., if own.is_some() { colors.accent } else { colors.text_secondary })
                .into_any_element(),
            own.is_some(),
            cx,
        );
        let (label, note) = match &own {
            Some(answer) => (answer.clone(), "Your answer".to_string()),
            None => ("Something else".to_string(), "Just start typing".to_string()),
        };
        let element = self
            .row("question-own".into(), mark, label, note, false, own.is_some(), other, focused, cx)
            .on_click(cx.listener(|_, event: &ClickEvent, _, cx| {
                if event.standard_click() {
                    cx.emit(PanelEvent::Compose(None));
                }
            }));
        rows.push(track("question-own".into(), element));
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .mt(px(space::SM + 1.))
                    .mb(px(space::SM - 2.))
                    .px(px(space::XXS))
                    .callout()
                    .text_color(colors.text)
                    .child(question),
            )
            .when_some(request.shown_detail().map(str::to_string), |this, detail| {
                let detail = TextView::markdown(
                    SharedString::from(format!("{copy}question-detail-{}", request.request_id)),
                    coding_requests::plain_markdown(&detail),
                    window,
                    cx,
                )
                .style(coding_requests::detail_style())
                .selectable(live);
                this.child(
                    div()
                        .w_full()
                        .mb(px(space::SM - 2.))
                        .max_h(px(76.))
                        .overflow_hidden()
                        .px(px(space::SM + 2.))
                        .py(px(space::XS + 2.))
                        .rounded(px(radius::SM))
                        .bg(colors.chat_background)
                        .footnote()
                        .text_color(colors.text_secondary)
                        .child(detail),
                )
            })
            .children(rows)
    }

    fn review(&self, group: &[ChatRequest], live: bool, focused: bool, cx: &mut Context<Self>) -> gpui::Div {
        let colors = *cx.colors();
        let rows = group.iter().enumerate().map(|(index, request)| {
            let answer =
                self.picks.get(&request.request_id).map(|answer| coding_requests::shown_answer(request, answer));
            let id = format!("question-review-{}", index + 1);
            let element = div()
                .id(SharedString::from(id.clone()))
                .w_full()
                .px(px(space::SM + 1.))
                .py(px(space::XS + 3.))
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(space::MD))
                .rounded(px(radius::SM - 1.))
                .when(focused && self.highlight == Some(index), |this| this.bg(colors.surface_strong))
                .when(!focused, |this| this.hover(|style| style.bg(colors.surface_strong)))
                .cursor_pointer()
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_row(index, *hovered, cx)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.focus.focus(window);
                    this.go(index, cx);
                }))
                .child(div().flex_none().footnote().text_color(colors.text_secondary).child(request.label(index + 1)))
                .child(match answer {
                    Some(answer) => div().min_w_0().footnote().text_color(colors.text).truncate().child(answer),
                    None => div()
                        .flex_none()
                        .footnote()
                        .text_color(colors.text_tertiary)
                        .child(if request.kind == "approval" { "Not answered yet" } else { "Skipped" }),
                });
            if live { crate::smoke::tracked_fill(id, element) } else { element.into_any_element() }
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .mt(px(space::SM + 1.))
                    .mb(px(space::SM - 2.))
                    .px(px(space::XXS))
                    .callout()
                    .text_color(colors.text)
                    .child("Review your answers"),
            )
            .children(rows)
    }

    fn footer(&self, group: &[ChatRequest], focused: bool, cx: &mut Context<Self>) -> gpui::Div {
        let colors = *cx.colors();
        let many = group.len() > 1;
        let review = self.index >= group.len();
        let hints = if review {
            vec![Self::hint("↑↓", "choose", cx), Self::hint("↵", "change or send", cx), Self::hint("←", "back", cx)]
        } else {
            let mut hints = vec![Self::hint("↑↓", "choose", cx), Self::hint("↵", "select", cx)];
            if many {
                hints.push(Self::hint("←→", "questions", cx));
            }
            hints
        };
        let right: Option<gpui::AnyElement> = if self.sending > 0 {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::XS + 2.))
                    .child(ui::spinner(12., cx))
                    .child(div().caption().text_color(colors.text_secondary).child("Sending…"))
                    .into_any_element(),
            )
        } else if review {
            let ring = focused && self.highlight.is_some_and(|row| row >= group.len());
            Some(
                crate::smoke::tracked(
                    "question-send",
                    div()
                        .id("question-send")
                        .rounded(px(radius::FULL))
                        .when(ring, |this| this.border_2().border_color(colors.accent.opacity(0.55)))
                        .child(ui::Button::primary("question-send-button", "Send answers").small().on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.focus.focus(window);
                                this.send(window, cx);
                            }),
                        )),
                )
                .into_any_element(),
            )
        } else if many {
            let answered = self.picks.contains_key(&group[self.index].request_id);
            let back = self.index > 0;
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .child(crate::smoke::tracked(
                        "question-back",
                        div()
                            .id("question-back")
                            .caption()
                            .text_color(colors.text_secondary)
                            .when(!back, |this| this.opacity(0.35))
                            .when(back, |this| {
                                this.cursor_pointer().hover(|style| style.text_color(colors.text)).on_click(
                                    cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.focus.focus(window);
                                        this.go(this.index.saturating_sub(1), cx);
                                    }),
                                )
                            })
                            .child("Back"),
                    ))
                    .child(crate::smoke::tracked(
                        "question-next",
                        div()
                            .id("question-next")
                            .px(px(space::MD - 1.))
                            .py(px(space::XXS + 1.))
                            .rounded(px(radius::FULL))
                            .bg(colors.surface_strong)
                            .caption()
                            .medium()
                            .text_color(colors.text)
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.85))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.focus.focus(window);
                                this.go(this.index + 1, cx);
                            }))
                            .child(if answered { "Next" } else { "Skip" }),
                    ))
                    .into_any_element(),
            )
        } else {
            None
        };
        // As tall as Send answers, so Review's footer is no taller than a question's.
        div()
            .w_full()
            .h(px(28.))
            .mt(px(space::SM))
            .px(px(space::XXS))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(space::MD))
            .child(div().min_w_0().flex().items_center().gap(px(space::MD - 2.)).overflow_hidden().children(hints))
            .children(right)
    }
}

/// An answer's label, with "Recommended" after it in the accent colour when it is.
fn label_text(label: String, recommended: bool, accent: gpui::Hsla) -> gpui::StyledText {
    if !recommended {
        return gpui::StyledText::new(label);
    }
    let start = label.len() + 3;
    let text = format!("{label}   Recommended");
    let end = text.len();
    gpui::StyledText::new(text)
        .with_highlights([(start..end, gpui::HighlightStyle { color: Some(accent), ..Default::default() })])
}

impl Render for QuestionPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync();
        let group = self.group();
        if group.is_empty() {
            return div().id("question-panel").into_any_element();
        }
        let colors = *cx.colors();
        let focused = self.focus.is_focused(window);
        let body = if self.index < group.len() {
            self.question(&group, self.index, true, window, cx)
        } else {
            self.review(&group, true, focused, cx)
        };
        // Every question of the set (and Review) drawn hidden behind it, to keep the body as tall as the tallest.
        let pages = if group.len() > 1 { group.len() + 1 } else { 1 };
        self.heights.borrow_mut().retain(|page, _| *page < pages);
        let tallest = self.heights.borrow().values().copied().fold(0., f32::max);
        let view = cx.entity().downgrade();
        let mut copies: Vec<gpui::AnyElement> = Vec::new();
        for page in 0..pages {
            let copy = if page < group.len() {
                self.question(&group, page, false, window, cx)
            } else {
                self.review(&group, false, focused, cx)
            };
            let (heights, view) = (self.heights.clone(), view.clone());
            let measure = gpui::canvas(
                move |bounds, _, cx| {
                    let height = f32::from(bounds.size.height);
                    let before = heights.borrow_mut().insert(page, height);
                    if before.is_none_or(|before| (before - height).abs() > 0.5) {
                        view.update(cx, |_, cx| cx.notify()).ok();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full();
            copies.push(
                div().absolute().top_0().left_0().right_0().invisible().child(copy).child(measure).into_any_element(),
            );
        }
        let error = self.error.clone().map(|error| {
            div()
                .w_full()
                .mt(px(space::XS + 2.))
                .px(px(space::XXS))
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(ui::icon_sized("triangle-alert", 13., colors.danger))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .caption()
                        .text_color(colors.danger)
                        .child(format!("Couldn't send. {error}")),
                )
                .child(crate::smoke::tracked(
                    "question-retry",
                    ui::Button::secondary("question-retry", "Try again")
                        .small()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.send(window, cx))),
                ))
        });
        let step = self.step;
        let task = group[0].task_id.clone();
        let panel = div()
            .id("question-panel")
            .key_context("QuestionPanel")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .w_full()
            .px(px(space::MD - 1.))
            .pt(px(space::SM + 2.))
            .pb(px(space::SM + 1.))
            .flex()
            .flex_col()
            .rounded(px(radius::LG))
            // Floats over the bottom of the chat: opaque, so the messages under it don't show through.
            .block_mouse_except_scroll()
            .bg(colors.surface)
            .border_1()
            .border_color(if focused { colors.border_strong } else { colors.border })
            .shadow_lg()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.is_focused(window) {
                        this.focus.focus(window);
                        cx.notify();
                    }
                }),
            )
            .child(self.header(&group, window, cx))
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex()
                    .flex_col()
                    .min_h(px(tallest))
                    .children(copies)
                    .child(body)
                    .with_animation(
                        SharedString::from(format!("question-step-{step}")),
                        Animation::new(EASE_IN).with_easing(gpui::ease_out_quint()),
                        |body, t| body.opacity(t).top(px((1. - t) * 5.)),
                    ),
            )
            .children(error)
            .child(self.footer(&group, focused, cx));
        // The panel itself eases in when a task's questions arrive.
        crate::smoke::tracked_fill(
            "question-panel",
            panel.with_animation(
                SharedString::from(format!("question-panel-{task}")),
                Animation::new(Duration::from_millis(200)).with_easing(gpui::ease_out_quint()),
                |panel, t| panel.opacity(t).top(px((1. - t) * 8.)),
            ),
        )
        .into_any_element()
    }
}

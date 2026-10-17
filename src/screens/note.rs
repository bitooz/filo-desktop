// Note editor. The note itself is a web page in the app's native web view (web/note.html, web/note-editor.ts;
// note_page.rs): edit mode is a rich-text editor, so formatting shows as formatting and the note is still saved as
// Markdown; read mode renders the Markdown with clickable task boxes. This view owns the header, the banners and
// autosave: a save runs after 1.5 s idle
// and no later than 10 s after the first unsaved change, and unchanged or empty
// notes aren't saved. A save goes straight to Filo's database (`notes:*`), one write at
// a time: the first save creates the note and later ones update it. Leaving the page
// (back, a tab switch) saves once more, and deletes a new note left empty.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend;
use crate::nav;
use crate::screens::note_page::{NotePage, PageMessage};
use crate::screens::note_text::{MAX_BODY_LENGTH, MAX_TITLE_LENGTH, note_preview, toggle_task_line, utf16_len};
use crate::theme::{self, ActiveTheme as _, PAGE_PADDING, Typography as _, space};
use crate::ui::{self, Button, IconButton};

const IDLE_DELAY: Duration = Duration::from_millis(1_500);
const MAX_DELAY: Duration = Duration::from_millis(10_000);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoteData {
    title: String,
    body: String,
    tags: Option<Vec<String>>,
    pinned: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveStatus {
    Idle,
    /// Typed, and the autosave timer is running.
    Pending,
    /// On its way to the database.
    Saving,
    Saved,
    /// The database turned it down: Retry, or the next save, tries again.
    Error,
}

impl SaveStatus {
    /// The header status, shown in read mode.
    fn label(self) -> &'static str {
        match self {
            SaveStatus::Idle => "",
            SaveStatus::Pending | SaveStatus::Saving => "Saving…",
            SaveStatus::Saved => "Saved",
            SaveStatus::Error => "Not saved",
        }
    }

    /// While editing, the Save button carries the autosave status instead.
    fn save_label(self) -> &'static str {
        match self {
            SaveStatus::Idle | SaveStatus::Error => "Save",
            SaveStatus::Pending | SaveStatus::Saving => "Saving…",
            SaveStatus::Saved => "Saved",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct NoteValues {
    title: String,
    body: String,
    tags: Option<Vec<String>>,
    pinned: bool,
}

impl NoteValues {
    fn same(&self, other: Option<&NoteValues>) -> bool {
        other.is_some_and(|other| {
            self.title == other.title
                && self.body == other.body
                && self.pinned == other.pinned
                && self.tags.clone().unwrap_or_default().join("\n") == other.tags.clone().unwrap_or_default().join("\n")
        })
    }

    fn is_empty(&self) -> bool {
        self.title.is_empty() && self.body.is_empty()
    }

    /// `notes:createMine` / `notes:updateMine` arguments: an update's tags left out keep the note's.
    fn args(&self, id: Option<&str>) -> Value {
        let mut args = json!({ "title": self.title, "body": self.body, "pinned": self.pinned });
        if let Some(tags) = &self.tags {
            args["tags"] = json!(tags);
        }
        if let Some(id) = id {
            args["id"] = json!(id);
        }
        args
    }
}

/// One write to Filo's database.
#[derive(Clone, Debug, PartialEq)]
enum Write {
    Create(NoteValues),
    Update(String, NoteValues),
    /// Pinning on its own doesn't move the note to the top.
    Pin(String, bool),
    Delete(String),
}

impl Write {
    /// The `notes:*` mutation and its arguments.
    fn call(&self) -> (&'static str, Value) {
        match self {
            Write::Create(values) => ("notes:createMine", values.args(None)),
            Write::Update(id, values) => ("notes:updateMine", values.args(Some(id))),
            Write::Pin(id, pinned) => ("notes:setPinnedMine", json!({ "id": id, "pinned": pinned })),
            Write::Delete(id) => ("notes:deleteMine", json!({ "id": id })),
        }
    }
}

/// The note's writes, one at a time: a new note is created once, and what's saved meanwhile goes after it as an update.
#[derive(Debug, Default)]
struct Writes {
    /// The note's id, once it's in the database.
    id: Option<String>,
    /// Values to write next.
    values: Option<NoteValues>,
    /// A pin to write next.
    pin: Option<bool>,
    /// The note is to be deleted.
    delete: bool,
    /// The write on its way.
    sending: Option<Write>,
    /// The last create or update failed: the next save writes the values even if they didn't change.
    failed: bool,
    error: Option<String>,
}

impl Writes {
    fn save(&mut self, values: NoteValues) {
        self.values = Some(values);
        self.failed = false;
    }

    /// The next write, unless one is on its way or there's nothing to write.
    fn next(&mut self) -> Option<Write> {
        if self.sending.is_some() {
            return None;
        }
        let write = if self.delete {
            self.values = None;
            self.pin = None;
            // A note that never got into the database has nothing to delete.
            Write::Delete(self.id.take()?)
        } else if let Some(mut values) = self.values.take() {
            if let Some(pinned) = self.pin.take() {
                values.pinned = pinned;
            }
            match &self.id {
                Some(id) => Write::Update(id.clone(), values),
                None => Write::Create(values),
            }
        } else {
            // A note not in the database yet gets its pin with its first save.
            let pinned = self.pin.take()?;
            Write::Pin(self.id.clone()?, pinned)
        };
        self.sending = Some(write.clone());
        Some(write)
    }

    /// The database's answer to the write on its way; a pin it turned down, to show as it was.
    fn done(&mut self, result: Result<Value, String>) -> Option<bool> {
        let write = self.sending.take()?;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                eprintln!("note: {} failed: {error}", write.call().0);
                return match write {
                    Write::Pin(_, pinned) => Some(pinned),
                    Write::Create(_) | Write::Update(..) => {
                        self.failed = true;
                        self.error = Some(error);
                        None
                    }
                    // Tried again on the next save.
                    Write::Delete(id) => {
                        self.id = Some(id);
                        self.error = Some(error);
                        None
                    }
                };
            }
        };
        self.error = None;
        match (write, value) {
            (Write::Create(_), Value::String(id)) => self.id = Some(id),
            (Write::Create(_), other) => {
                self.failed = true;
                self.error = Some(format!("Unexpected reply from notes:createMine: {other}"));
            }
            // Deleted somewhere else meanwhile (by an agent, say): what was written here comes back as a new note
            // rather than being lost.
            (Write::Update(_, values), Value::Bool(false)) => {
                self.id = None;
                self.values.get_or_insert(values);
            }
            _ => {}
        }
        None
    }

    /// A create or update is waiting or on its way.
    fn saving(&self) -> bool {
        self.values.is_some() || matches!(self.sending, Some(Write::Create(_) | Write::Update(..)))
    }
}

/// Autosave state. It lives outside the view so the last save (and the clean-up of
/// an empty new note) still happens after the page is closed.
struct Autosave {
    values: NoteValues,
    /// What was last saved (or loaded), so unchanged values aren't saved again.
    last_sent: Option<NoteValues>,
    writes: Writes,
    /// Typed and not saved yet: the idle timer is running.
    pending: bool,
    /// This page saved the note at least once.
    kept: bool,
    first_change_at: Option<Instant>,
    /// Bumped to cancel the pending idle timer.
    timer: u64,
    view: WeakEntity<NoteScreen>,
}

type Core = Rc<RefCell<Autosave>>;

fn notify(core: &Core, cx: &mut App) {
    let view = core.borrow().view.clone();
    // Callers are often inside the view's own update (input events); updating it
    // again there aborts, so re-render once the current update is done.
    cx.defer(move |cx| {
        view.update(cx, |_, cx| cx.notify()).ok();
    });
}

/// Marks the note changed and (re)arms the idle timer, capped at 10 s after the first change.
fn changed(core: &Core, cx: &mut App) {
    let (wait, timer) = {
        let mut state = core.borrow_mut();
        let now = Instant::now();
        let first = *state.first_change_at.get_or_insert(now);
        state.pending = true;
        state.timer += 1;
        (IDLE_DELAY.min((first + MAX_DELAY).saturating_duration_since(now)), state.timer)
    };
    notify(core, cx);
    let core = core.clone();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(wait).await;
        cx.update(|cx| {
            if core.borrow().timer == timer {
                save(&core, cx);
            }
        })
        .ok();
    })
    .detach();
}

/// Saves the note now; unchanged or empty notes aren't saved, unless the last save failed.
fn save(core: &Core, cx: &mut App) {
    {
        let mut state = core.borrow_mut();
        state.timer += 1;
        state.first_change_at = None;
        state.pending = false;
        if !state.values.is_empty() && (state.writes.failed || !state.values.same(state.last_sent.as_ref())) {
            let values = state.values.clone();
            state.last_sent = Some(values.clone());
            state.kept = true;
            state.writes.save(values);
        }
    }
    write(core, cx);
    notify(core, cx);
}

/// Sends the next write to the database once the one on its way is done.
fn write(core: &Core, cx: &mut App) {
    let Some(next) = core.borrow_mut().writes.next() else { return };
    let (path, args) = next.call();
    let future = backend::backend(cx).call_raw(path, args);
    let core = core.clone();
    cx.spawn(async move |cx| {
        let result = future.await;
        cx.update(|cx| {
            let turned_down = core.borrow_mut().writes.done(result);
            if let Some(pinned) = turned_down {
                set_pinned(&core, !pinned);
            }
            write(&core, cx);
            notify(&core, cx);
        })
        .ok();
    })
    .detach();
}

/// What the header says: typing, else where the last save is.
fn status(core: &Core) -> SaveStatus {
    let state = core.borrow();
    if state.pending {
        SaveStatus::Pending
    } else if state.writes.error.is_some() {
        SaveStatus::Error
    } else if state.writes.saving() {
        SaveStatus::Saving
    } else if state.kept {
        SaveStatus::Saved
    } else {
        SaveStatus::Idle
    }
}

/// Deletes the note: the write on its way finishes first, and nothing is saved after.
fn delete(core: &Core, cx: &mut App) {
    core.borrow_mut().writes.delete = true;
    write(core, cx);
}

/// Stops saving, for a note that is being deleted.
fn discard(core: &Core) {
    let mut state = core.borrow_mut();
    state.timer += 1;
    state.pending = false;
    state.last_sent = Some(state.values.clone());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Body,
}

enum Loaded {
    Loading,
    Unavailable,
    Failed(String),
}

pub struct NoteScreen {
    existing_id: Option<String>,
    loaded: Loaded,
    hydrated: bool,
    editing: bool,
    too_long: bool,
    core: Core,
    /// The note's page, once the note is loaded.
    page: Option<NotePage>,
    _note: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl NoteScreen {
    /// `id` is the note's id; None for a new note.
    pub fn new(id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let core = Rc::new(RefCell::new(Autosave {
            values: NoteValues::default(),
            last_sent: None,
            writes: Writes { id: id.clone(), ..Writes::default() },
            pending: false,
            kept: false,
            first_change_at: None,
            timer: 0,
            view: cx.weak_entity(),
        }));
        let subscriptions = vec![
            // The window losing focus saves, like leaving the page.
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if !window.is_window_active() {
                    save(&this.core, cx);
                }
            }),
        ];
        // Leaving the page (back button, sidebar) saves once more; a new note left empty
        // is deleted.
        cx.on_release(|this: &mut Self, cx| {
            save(&this.core, cx);
            let emptied = {
                let state = this.core.borrow();
                this.existing_id.is_none() && state.kept && state.values.is_empty()
            };
            if emptied {
                delete(&this.core, cx);
            }
        })
        .detach();
        let note = id.as_ref().map(|id| {
            backend::subscribe(
                cx,
                "notes:getMine",
                json!({ "id": id }),
                |this: &mut Self, result: Result<Option<NoteData>, String>, cx| this.loaded(result, cx),
            )
        });
        NoteScreen {
            hydrated: id.is_none(),
            editing: id.is_none(),
            existing_id: id,
            loaded: Loaded::Loading,
            too_long: false,
            core,
            page: None,
            _note: note,
            _subscriptions: subscriptions,
        }
    }

    fn loaded(&mut self, result: Result<Option<NoteData>, String>, cx: &mut Context<Self>) {
        match result {
            Ok(Some(note)) => {
                if !self.hydrated {
                    self.hydrated = true;
                    let values = NoteValues {
                        title: note.title,
                        body: note.body,
                        tags: note.tags,
                        pinned: note.pinned == Some(true),
                    };
                    let mut state = self.core.borrow_mut();
                    // The stored note is what the first save compares against.
                    state.last_sent.get_or_insert_with(|| values.clone());
                    state.values = values;
                }
            }
            Ok(None) => self.loaded = Loaded::Unavailable,
            Err(error) => self.loaded = Loaded::Failed(error),
        }
        cx.notify();
    }

    /// `FiloNote.show(…)` arguments for the current mode and note.
    fn show_args(&self, focus: Option<Field>) -> Value {
        let values = &self.core.borrow().values;
        let mut args = json!({
            "mode": if self.editing { "edit" } else { "read" },
            "title": values.title,
            "markdown": values.body,
        });
        if let Some(focus) = focus {
            args["focus"] = json!(if focus == Field::Title { "title" } else { "body" });
        }
        args
    }

    #[cfg(feature = "dev")]
    fn run_script(&mut self, script: String, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.page {
            page.run_script(script, cx);
        }
    }

    fn show_page(&mut self, focus: Option<Field>, cx: &mut Context<Self>) {
        let args = self.show_args(focus);
        if let Some(page) = &mut self.page {
            page.show(&args, cx);
        }
    }

    /// Builds the page once the note is loaded; a new note opens with the title focused.
    fn ensure_web(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page.is_some() || !self.hydrated || matches!(self.loaded, Loaded::Failed(_)) {
            return;
        }
        let focus = self.existing_id.is_none().then_some(Field::Title);
        let show = self.show_args(focus);
        match NotePage::build(
            &show,
            MAX_TITLE_LENGTH,
            window,
            cx,
            |this: &mut Self| this.page.as_mut(),
            |this, message, window, cx| this.page_message(message, window, cx),
        ) {
            Ok(page) => self.page = Some(page),
            Err(error) => self.loaded = Loaded::Failed(format!("The note couldn't open: {error}")),
        }
    }

    fn page_message(&mut self, message: PageMessage, window: &mut Window, cx: &mut Context<Self>) {
        match message {
            PageMessage::Title { title } => {
                if title == self.core.borrow().values.title || utf16_len(&title) > MAX_TITLE_LENGTH {
                    return;
                }
                self.core.borrow_mut().values.title = title;
                changed(&self.core, cx);
            }
            PageMessage::Body { markdown } => {
                // An overlong note keeps its last saved body, so nothing typed so far is lost.
                let too_long = utf16_len(&markdown) > MAX_BODY_LENGTH;
                if too_long != self.too_long {
                    self.too_long = too_long;
                    cx.notify();
                }
                if too_long || markdown == self.core.borrow().values.body {
                    return;
                }
                self.core.borrow_mut().values.body = markdown;
                changed(&self.core, cx);
            }
            PageMessage::Blur => save(&self.core, cx),
            PageMessage::Edit => self.start_editing(Field::Body, cx),
            PageMessage::Task { line } => self.toggle_task(line, cx),
            PageMessage::Link { url } => {
                if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                    let _ = open::that(url);
                }
            }
            PageMessage::Nav { dir } => match dir.as_str() {
                "back" => nav::back(window, cx),
                "forward" => nav::forward(window, cx),
                _ => {}
            },
            PageMessage::Description { .. }
            | PageMessage::Save
            | PageMessage::Ready
            | PageMessage::Painted
            | PageMessage::Other => {}
        }
    }

    /// Types into the title and body the way keystrokes do (smoke test).
    #[cfg(feature = "dev")]
    pub fn type_text(&mut self, title: &str, body: &str, _window: &mut Window, cx: &mut Context<Self>) {
        self.run_script(format!("FiloNote.typeForTest({}, {})", json!(title), json!(body)), cx);
    }

    /// The note's id once it's in the database (smoke test).
    #[cfg(feature = "dev")]
    pub fn saved_id(&self) -> Option<String> {
        self.core.borrow().writes.id.clone()
    }

    /// Hidden under a pushed page (the shell calls this, like the artifact page's).
    pub fn set_page_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.page {
            page.set_page_visible(visible, cx);
        }
    }

    fn start_editing(&mut self, field: Field, cx: &mut Context<Self>) {
        if self.editing || !self.hydrated {
            return;
        }
        self.editing = true;
        self.show_page(Some(field), cx);
        cx.notify();
    }

    fn save_and_read(&mut self, cx: &mut Context<Self>) {
        // An overlong note stays in the editor so nothing typed is dropped.
        if self.too_long {
            return;
        }
        save(&self.core, cx);
        self.editing = false;
        self.show_page(None, cx);
        cx.notify();
    }

    fn toggle_task(&mut self, line: usize, cx: &mut Context<Self>) {
        let body = self.core.borrow().values.body.clone();
        let updated = toggle_task_line(&body, line);
        if updated == body {
            return;
        }
        self.core.borrow_mut().values.body = updated;
        changed(&self.core, cx);
        self.show_page(None, cx);
    }

    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let next = !self.core.borrow().values.pinned;
        set_pinned(&self.core, next);
        cx.notify();
        // Written on its own so pinning doesn't mark the note changed and bump its date.
        self.core.borrow_mut().writes.pin = Some(next);
        write(&self.core, cx);
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = {
            let values = &self.core.borrow().values;
            if values.title.is_empty() {
                note_preview(&values.body).encode_utf16().take(60).collect::<Vec<_>>()
            } else {
                values.title.encode_utf16().collect()
            }
        };
        let message = String::from_utf16_lossy(&message);
        let view = cx.weak_entity();
        ui::confirm(window, cx, "Delete note?", message, "Delete", "Keep", true, move |window, cx| {
            view.update(cx, |this, cx| this.delete(window, cx)).ok();
        });
        if let Some(page) = &mut self.page {
            page.hide_for_dialog(window, cx, |this: &mut Self| this.page.as_mut());
        }
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        discard(&self.core);
        delete(&self.core, cx);
        nav::back(window, cx);
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let status = status(&self.core);
        let (pinned, can_delete) = {
            let state = self.core.borrow();
            (state.values.pinned, self.hydrated && (self.existing_id.is_some() || state.kept))
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .px(px(PAGE_PADDING))
            .pt(px(space::SM))
            .pb(px(space::SM))
            .child(
                IconButton::new("note-back", "arrow-left")
                    .small()
                    .tooltip("Back to notes")
                    .on_click(|_, window, cx| nav::back(window, cx)),
            )
            // The bar drags the window, like every page's.
            .child(
                crate::chrome::drag_region(div().id("note-drag"))
                    .flex_1()
                    .min_w_0()
                    .py(px(space::SM))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .footnote()
                            .text_color(colors.text_secondary)
                            .child(if self.editing { "" } else { status.label() }),
                    ),
            )
            .child(
                IconButton::new("note-pin", if pinned { "pin-filled" } else { "pin" })
                    .tooltip(if pinned { "Unpin note" } else { "Pin note" })
                    .color(if pinned { colors.text } else { colors.text_secondary })
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_pin(cx))),
            )
            .when(can_delete, |this| {
                this.child(
                    IconButton::new("note-delete", "trash-2")
                        .tooltip("Delete note")
                        .color(colors.text_secondary)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.confirm_delete(window, cx))),
                )
            })
            .when(self.hydrated, |this| {
                let button = if self.editing {
                    Button::secondary("note-save", status.save_label())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_and_read(cx)))
                } else {
                    Button::secondary("note-edit", "Edit")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_editing(Field::Body, cx)))
                };
                // Wide enough for "Saving…", so the header icons don't shift as the label changes.
                this.child(div().min_w(px(if self.editing { 96. } else { 0. })).flex().justify_end().child(button))
            })
    }

    fn render_banners(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = status(&self.core) == SaveStatus::Error;
        if !self.too_long && !error {
            return None;
        }
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .px(px(PAGE_PADDING))
                .pb(px(space::SM))
                .when(self.too_long, |this| {
                    this.child(ui::banner(
                        "This note is too long to save. Shorten it a little to keep your changes.",
                        ui::Tone::Danger,
                        cx,
                    ))
                })
                .when(error, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().flex_1().child(ui::banner("Couldn’t save this note.", ui::Tone::Danger, cx)))
                            .child(
                                Button::danger("note-retry", "Retry")
                                    .small()
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| save(&this.core, cx))),
                            ),
                    )
                })
                .into_any_element(),
        )
    }
}

/// A note's outline while it loads: title, rule and lines of text, laid out like
/// web/note.html so the note lands on top of it.
fn note_skeleton(cx: &App) -> impl IntoElement {
    use gpui::relative;
    use gpui_component::skeleton::Skeleton;
    let colors = *cx.colors();
    let scale = theme::text_scale();
    let (line, title_line) = ((23. * scale).round(), (29. * scale).round());
    // Text-height bars a line apart; a gap stands for a blank line, a short wide bar for a heading.
    let lines: [(f32, f32); 10] = [
        (0.62, 1.),
        (0., 1.),
        (0.34, 1.6),
        (0.94, 1.),
        (0.88, 1.),
        (0.97, 1.),
        (0.55, 1.),
        (0., 1.),
        (0.9, 1.),
        (0.72, 1.),
    ];
    div().size_full().child(
        div()
            .w_full()
            .max_w(px(760.))
            .mx_auto()
            .pt(px(28.))
            .px(px(24.))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(title_line))
                    .flex()
                    .items_center()
                    .child(Skeleton::new().w(px(300.)).h(px(title_line * 0.7)).rounded(px(8.))),
            )
            .child(div().h(px(1.)).bg(colors.border).mt(px(18.)).mb(px(20.)))
            .children(lines.into_iter().map(|(width, size)| {
                let height = line * size;
                div().h(px(height)).flex().items_center().when(width > 0., |this| {
                    this.child(Skeleton::new().w(relative(width)).h(px(height * 0.62)).rounded(px(6.)))
                })
            })),
    )
}

fn set_pinned(core: &Core, pinned: bool) {
    let mut state = core.borrow_mut();
    state.values.pinned = pinned;
    // Records a pin change that was written on its own, so autosave doesn't send it again.
    if let Some(last) = state.last_sent.as_mut() {
        last.pinned = pinned;
    }
}

impl Render for NoteScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        self.ensure_web(window, cx);
        // The page follows the app's theme and text size.
        if let Some(page) = &mut self.page {
            page.follow_theme(cx);
        }
        let content: AnyElement = match (&self.page, &self.loaded) {
            // The page is transparent until it draws: the skeleton shows through it till then.
            (Some(page), _) => div()
                .flex_1()
                .min_h_0()
                .w_full()
                .relative()
                .when(!page.painted(), |this| this.child(div().absolute().inset_0().child(note_skeleton(cx))))
                .child(page.web().clone())
                .into_any_element(),
            (None, Loaded::Loading) => div().flex_1().min_h_0().child(note_skeleton(cx)).into_any_element(),
            (None, Loaded::Unavailable | Loaded::Failed(_)) => {
                let message = match &self.loaded {
                    Loaded::Failed(error) => error.clone(),
                    _ => "This note was deleted or couldn't be loaded.".to_string(),
                };
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(space::SM))
                    .px(px(space::XXXL))
                    .text_center()
                    .child(div().title3().bold().child("Note unavailable"))
                    .child(div().subhead().text_color(colors.text_secondary).child(message))
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .child(self.render_header(cx))
            .children(self.render_banners(cx))
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::local_backend::Local;

    /// The page's writes against a backend of their own.
    struct Fixture {
        root: PathBuf,
        local: Arc<Local>,
        writes: Writes,
    }

    impl Fixture {
        fn new(id: Option<String>) -> Self {
            let root = std::env::temp_dir().join(format!("filo-note-page-{}", uuid::Uuid::new_v4()));
            Fixture { local: Local::open(root.clone()), root, writes: Writes { id, ..Writes::default() } }
        }

        /// Answers `write` (from `next`) as the database does.
        fn answer(&mut self, write: &Write) -> Option<bool> {
            let (path, args) = write.call();
            self.writes.done(self.local.call(path, &args))
        }

        /// Sends what's waiting, one write at a time, as the page does.
        fn send(&mut self) {
            while let Some(write) = self.writes.next() {
                self.answer(&write);
            }
        }

        fn create(&self, title: &str) -> String {
            let id = self.local.call("notes:createMine", &json!({ "title": title, "body": "" })).unwrap();
            id.as_str().unwrap().to_string()
        }

        fn notes(&self) -> Vec<Value> {
            self.local.call("notes:mine", &json!({})).unwrap().as_array().unwrap().clone()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    fn values(title: &str, body: &str) -> NoteValues {
        NoteValues { title: title.into(), body: body.into(), tags: None, pinned: false }
    }

    #[test]
    fn a_new_note_is_created_once_then_edited_and_deleted() {
        let mut page = Fixture::new(None);
        page.writes.save(values("Groceries", "milk"));
        let create = page.writes.next().unwrap();
        assert!(matches!(create, Write::Create(_)));
        // Saved again while the create is on its way: it waits, then goes as an update.
        page.writes.save(values("Groceries", "milk, eggs"));
        assert_eq!(page.writes.next(), None);
        assert!(page.writes.saving());
        page.answer(&create);
        let id = page.writes.id.clone().unwrap();
        let update = page.writes.next().unwrap();
        assert_eq!(update, Write::Update(id.clone(), values("Groceries", "milk, eggs")));
        page.answer(&update);
        page.writes.save(values("Groceries", "milk, eggs, bread"));
        page.send();
        assert!(!page.writes.saving());
        let notes = page.notes();
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0]["_id"].as_str(), notes[0]["body"].as_str()),
            (Some(id.as_str()), Some("milk, eggs, bread"))
        );

        page.writes.delete = true;
        page.send();
        assert!(page.notes().is_empty());
        // Nothing is saved after it, and the note isn't made again.
        page.writes.save(values("Groceries", "milk"));
        page.send();
        assert!(page.notes().is_empty());
        assert_eq!(page.writes.error, None);
    }

    #[test]
    fn an_existing_note_is_edited_and_deleted_by_its_id() {
        let mut page = Fixture::new(None);
        let id = page.create("Ideas");
        page.writes.id = Some(id.clone());
        page.writes.save(NoteValues { tags: Some(vec!["Later".into()]), ..values("Ideas", "A pet that naps") });
        page.send();
        let note = page.local.call("notes:getMine", &json!({ "id": id })).unwrap();
        assert_eq!((note["body"].as_str(), note["tags"].clone()), (Some("A pet that naps"), json!(["later"])));
        assert_eq!(page.notes().len(), 1);
        page.writes.delete = true;
        page.send();
        assert_eq!(page.local.call("notes:getMine", &json!({ "id": id })).unwrap(), Value::Null);
    }

    #[test]
    fn a_new_note_deleted_while_its_create_is_on_its_way_is_deleted_after_it() {
        let mut page = Fixture::new(None);
        page.writes.save(values("Scratch", "x"));
        let create = page.writes.next().unwrap();
        page.writes.delete = true;
        assert_eq!(page.writes.next(), None);
        page.answer(&create);
        page.send();
        assert!(page.notes().is_empty());
    }

    #[test]
    fn a_note_deleted_elsewhere_comes_back_as_a_new_note() {
        let mut page = Fixture::new(None);
        let id = page.create("Plan");
        page.writes.id = Some(id.clone());
        page.local.call("notes:deleteMine", &json!({ "id": id })).unwrap();
        page.writes.save(values("Plan", "still wanted"));
        page.send();
        let notes = page.notes();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0]["body"].as_str(), Some("still wanted"));
        assert_ne!(notes[0]["_id"].as_str(), Some(id.as_str()));
        assert_eq!(page.writes.id.as_deref(), notes[0]["_id"].as_str());
    }

    #[test]
    fn a_pin_is_written_on_its_own_and_keeps_the_notes_date() {
        let mut page = Fixture::new(None);
        let id = page.create("Pinned");
        page.writes.id = Some(id.clone());
        let before = page.notes()[0]["updatedAt"].clone();
        page.writes.pin = Some(true);
        let pin = page.writes.next().unwrap();
        assert_eq!(pin, Write::Pin(id.clone(), true));
        assert!(!page.writes.saving());
        page.answer(&pin);
        let note = &page.notes()[0];
        assert_eq!((note["pinned"].as_bool(), note["updatedAt"].clone()), (Some(true), before));

        // Pinned while a new note's create is on its way: the pin follows it.
        let mut page = Fixture::new(None);
        page.writes.save(values("New", "x"));
        let create = page.writes.next().unwrap();
        page.writes.pin = Some(true);
        page.answer(&create);
        page.send();
        assert_eq!(page.notes()[0]["pinned"].as_bool(), Some(true));
    }

    #[test]
    fn a_failed_save_shows_and_is_tried_again() {
        let mut page = Fixture::new(None);
        page.writes.save(values("Groceries", "milk"));
        page.writes.next().unwrap();
        assert_eq!(page.writes.done(Err("The disk is full.".into())), None);
        assert!(page.writes.failed);
        assert_eq!(page.writes.error.as_deref(), Some("The disk is full."));
        assert!(page.notes().is_empty());
        // Retry (or the next save) writes the same values again.
        page.writes.save(values("Groceries", "milk"));
        page.send();
        assert_eq!(page.writes.error, None);
        assert_eq!(page.notes().len(), 1);

        // A pin the database turned down is handed back, to show as it was.
        page.writes.pin = Some(true);
        page.writes.next().unwrap();
        assert_eq!(page.writes.done(Err("The disk is full.".into())), Some(true));
        assert_eq!(page.writes.error, None);
    }
}

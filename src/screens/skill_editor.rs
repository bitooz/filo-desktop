// A skill's page under "Skills › <name>" (skills.rs), new or to change: the Notes page's editor (note_page.rs) with the
// skill's name as the title, a line under it for when to use it, and the instructions as the note's body, which grows
// as it's typed. What's typed is kept on this Mac as a draft (~/.config/filo/offline/<env>/<user>/skill-drafts.json)
// half a second after typing stops, when the page loses focus and when it's left, so closing it (or Filo) and opening
// the skill again, or New skill for a new one, brings it back. Save sends it (`skills:save` creates the skill or
// changes the one with that name, its folder's other files left as they are) and drops the draft; Discard drops it
// without saving. A saved skill's page names the other files in its folder, and Show in Finder opens it. A saved
// skill opens to read, its Markdown drawn as a note's read mode draws it; Edit (or a double-click) shows its source to
// change, never put through a rich-text editor, so a save keeps the file as it was but for what was typed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    AnyElement, App, ClickEvent, Context, EventEmitter, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::connectors::{breadcrumb, pane, pane_bar};
use super::note_page::{NotePage, PageMessage};
use super::note_text::{toggle_task_line, utf16_len};
use super::skills::{MAX_BODY, MAX_DESCRIPTION, MAX_NAME, Picked, Skill, display_name};
use crate::backend;
use crate::config;
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, Button, IconButton};

/// How long after the last keystroke the draft is kept.
const DRAFT_DELAY: Duration = Duration::from_millis(500);

/// A skill's three fields, as typed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Values {
    pub name: String,
    pub description: String,
    pub body: String,
}

impl Values {
    fn of(skill: &Skill) -> Self {
        Values { name: skill.name.clone(), description: skill.description.clone(), body: skill.body.clone() }
    }

    /// Whether these differ from `skill` (a new skill: whether anything is typed). Space at the ends doesn't count.
    fn differ(&self, skill: Option<&Skill>) -> bool {
        match skill {
            Some(skill) => self.description.trim() != skill.description.trim() || self.body.trim() != skill.body.trim(),
            None => [&self.name, &self.description, &self.body].iter().any(|value| !value.trim().is_empty()),
        }
    }

    fn ready(&self) -> bool {
        [&self.name, &self.description, &self.body].iter().all(|value| !value.trim().is_empty())
    }
}

// ---- drafts ----

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Stored {
    /// New skill's draft.
    #[serde(default)]
    new: Option<Values>,
    /// Changes to saved skills, by name.
    #[serde(default)]
    skills: BTreeMap<String, Values>,
}

struct SkillDrafts {
    file: PathBuf,
    stored: Stored,
}

impl gpui::Global for SkillDrafts {}

/// The drafts, loaded on first use.
fn drafts(cx: &mut App) -> &mut Stored {
    let backend = backend::backend(cx);
    let file = config::offline_dir(backend.user_id()).join("skill-drafts.json");
    if cx.try_global::<SkillDrafts>().is_none_or(|drafts| drafts.file != file) {
        let stored =
            std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        cx.set_global(SkillDrafts { file, stored });
    }
    &mut cx.global_mut::<SkillDrafts>().stored
}

/// The draft of the skill `name`, or of the new skill (`None`).
pub fn draft(cx: &mut App, name: Option<&str>) -> Option<Values> {
    let stored = drafts(cx);
    match name {
        None => stored.new.clone(),
        Some(name) => stored.skills.get(name).cloned(),
    }
}

/// The names of the skills with a draft.
pub fn drafted(cx: &mut App) -> Vec<String> {
    drafts(cx).skills.keys().cloned().collect()
}

/// Reads the drafts from the file again on next use, as after a restart (smoke test).
#[cfg(feature = "dev")]
pub fn forget_loaded(cx: &mut App) {
    if cx.has_global::<SkillDrafts>() {
        cx.remove_global::<SkillDrafts>();
    }
}

/// Keeps `values` as the draft of `name` (`None`: the new skill), or drops it; the file is written when that changed.
pub fn put(cx: &mut App, name: Option<&str>, values: Option<Values>) {
    let stored = drafts(cx);
    let changed = match name {
        None if stored.new != values => {
            stored.new = values;
            true
        }
        None => false,
        Some(name) => match values {
            Some(values) if stored.skills.get(name) != Some(&values) => {
                stored.skills.insert(name.to_string(), values);
                true
            }
            Some(_) => false,
            None => stored.skills.remove(name).is_some(),
        },
    };
    if changed {
        let drafts = cx.global::<SkillDrafts>();
        match serde_json::to_vec_pretty(&drafts.stored) {
            Ok(bytes) => {
                if let Err(error) = config::write_atomic(&drafts.file, &bytes) {
                    eprintln!("skill drafts: couldn't save {}: {error}", drafts.file.display());
                }
            }
            Err(error) => eprintln!("skill drafts: {error}"),
        }
    }
}

// ---- the page ----

pub enum EditorEvent {
    /// Back to the list (the breadcrumb, or the back key or mouse button over the page).
    Back,
    Removed,
    /// A skill folder was imported from the new skill's page: its page opens.
    Imported(String),
}

impl EventEmitter<EditorEvent> for SkillEditor {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Saving,
    Removing,
}

pub struct SkillEditor {
    /// The skill as it's saved; none for a new one until it's saved.
    skill: Option<Skill>,
    values: Values,
    page: Option<NotePage>,
    /// Showing the fields to change rather than the skill to read (a new skill always is).
    editing: bool,
    /// The instructions are past the limit: the last ones that fit are kept.
    too_long: bool,
    busy: Option<Busy>,
    /// Saved from this page, and nothing typed since.
    saved: bool,
    error: Option<String>,
    /// Typed and not kept as a draft yet.
    pending: bool,
    /// Bumped to cancel the pending draft timer.
    timer: u64,
    _subscriptions: Vec<Subscription>,
}

impl SkillEditor {
    /// The page for `skill`, or a new skill (none) filled with `values` (an import) or else its draft.
    pub fn new(skill: Option<Skill>, values: Option<Values>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = skill.as_ref().map(|skill| skill.name.clone());
        let values = values
            .or_else(|| draft(cx, name.as_deref()))
            .or_else(|| skill.as_ref().map(Values::of))
            .unwrap_or_default();
        let subscriptions = vec![
            // The window losing focus keeps the draft, like leaving the page.
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if !window.is_window_active() {
                    this.keep_draft(cx);
                }
            }),
        ];
        // Leaving the page (back, another page or tab) keeps what's typed.
        cx.on_release(|this: &mut Self, cx| this.keep_draft(cx)).detach();
        let editing = skill.is_none();
        let mut editor = SkillEditor {
            skill,
            values,
            page: None,
            editing,
            too_long: false,
            busy: None,
            saved: false,
            error: None,
            pending: false,
            timer: 0,
            _subscriptions: subscriptions,
        };
        // An import is a draft from the start.
        editor.keep_draft(cx);
        editor
    }

    /// The fields as typed (smoke test).
    #[cfg(feature = "dev")]
    pub fn values(&self) -> &Values {
        &self.values
    }

    /// The page has drawn (smoke test).
    #[cfg(feature = "dev")]
    pub fn painted(&self) -> bool {
        self.page.as_ref().is_some_and(NotePage::painted)
    }

    /// Showing the skill's source to change (smoke test).
    #[cfg(feature = "dev")]
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// The skill's name, once it's saved.
    pub fn name(&self) -> Option<&str> {
        self.skill.as_ref().map(|skill| skill.name.as_str())
    }

    /// What the page shows, as `FiloNote.stateForTest()` gives it (smoke test).
    #[cfg(feature = "dev")]
    pub fn page_state(&self, cx: &App) -> Option<futures::channel::oneshot::Receiver<String>> {
        Some(self.page.as_ref()?.evaluate("FiloNote.stateForTest()", cx))
    }

    /// The saved copy changed (the skills list): what's typed stays, and Save shows if it now differs.
    pub fn skill_changed(&mut self, skill: Option<&Skill>, cx: &mut Context<Self>) {
        if let (Some(mine), Some(skill)) = (&self.skill, skill)
            && (mine.description != skill.description
                || mine.body != skill.body
                || mine.source != skill.source
                || mine.files != skill.files)
        {
            self.skill = Some(skill.clone());
            cx.notify();
        }
    }

    pub fn set_page_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.page {
            page.set_page_visible(visible, cx);
        }
    }

    /// Fills a new skill's fields from an imported SKILL.md.
    pub fn import(&mut self, values: Values, cx: &mut Context<Self>) {
        self.values = values;
        self.saved = false;
        self.keep_draft(cx);
        self.show_page(Some("body"), cx);
        cx.notify();
    }

    fn show_args(&self, focus: Option<&str>) -> Value {
        let mut args = json!({
            "mode": if self.editing { "edit" } else { "read" },
            "title": self.values.name,
            "markdown": self.values.body,
            "fields": {
                "titlePlaceholder": "Name, like weekly-report",
                "titleReadOnly": self.skill.is_some(),
                "description": self.values.description,
                "descriptionPlaceholder": "When to use it, in a line",
                "descriptionMax": MAX_DESCRIPTION,
                "bodyPlaceholder": "What to do, as steps someone else could follow",
            },
        });
        if let Some(focus) = focus {
            args["focus"] = json!(focus);
        }
        args
    }

    fn show_page(&mut self, focus: Option<&str>, cx: &mut Context<Self>) {
        let args = self.show_args(focus);
        if let Some(page) = &mut self.page {
            page.show(&args, cx);
        }
    }

    fn ensure_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page.is_some() || self.error.as_deref().is_some_and(|error| error.starts_with("The editor")) {
            return;
        }
        // A new skill starts at its name.
        let show = self.show_args(self.skill.is_none().then_some("title"));
        match NotePage::build(
            &show,
            MAX_NAME,
            window,
            cx,
            |this: &mut Self| this.page.as_mut(),
            |this, message, window, cx| this.page_message(message, window, cx),
        ) {
            Ok(page) => self.page = Some(page),
            Err(error) => self.error = Some(format!("The editor couldn't open: {error}")),
        }
    }

    fn page_message(&mut self, message: PageMessage, window: &mut Window, cx: &mut Context<Self>) {
        match message {
            PageMessage::Title { title } => {
                if self.skill.is_none() && title != self.values.name && utf16_len(&title) <= MAX_NAME {
                    self.values.name = title;
                    self.changed(cx);
                }
            }
            PageMessage::Description { description } => {
                if description != self.values.description && utf16_len(&description) <= MAX_DESCRIPTION {
                    self.values.description = description;
                    self.changed(cx);
                }
            }
            PageMessage::Body { markdown } => {
                // Overlong instructions keep the last ones that fit, so nothing kept so far is lost.
                let too_long = utf16_len(&markdown) > MAX_BODY;
                if too_long != self.too_long {
                    self.too_long = too_long;
                    cx.notify();
                }
                if !too_long && markdown != self.values.body {
                    self.values.body = markdown;
                    self.changed(cx);
                }
            }
            PageMessage::Blur => self.keep_draft(cx),
            PageMessage::Edit => self.start_editing(cx),
            PageMessage::Task { line } => {
                // Only that line's box changes, as in a note.
                let body = toggle_task_line(&self.values.body, line);
                if body != self.values.body {
                    self.values.body = body;
                    self.changed(cx);
                    self.show_page(None, cx);
                }
            }
            PageMessage::Save => {
                if self.can_save() {
                    self.save(window, cx);
                }
            }
            PageMessage::Link { url } => {
                if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:") {
                    let _ = open::that(url);
                }
            }
            PageMessage::Nav { dir } if dir == "back" => cx.emit(EditorEvent::Back),
            _ => {}
        }
    }

    fn start_editing(&mut self, cx: &mut Context<Self>) {
        if self.editing {
            return;
        }
        self.editing = true;
        self.show_page(Some("body"), cx);
        cx.notify();
    }

    /// Back to reading, what's typed kept as the draft. Overlong instructions stay in the editor, so nothing typed is
    /// dropped.
    fn stop_editing(&mut self, cx: &mut Context<Self>) {
        if !self.editing || self.skill.is_none() || self.too_long {
            return;
        }
        self.keep_draft(cx);
        self.editing = false;
        self.show_page(None, cx);
        cx.notify();
    }

    /// Something was typed: the draft is kept once typing stops for a moment.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.saved = false;
        self.pending = true;
        self.timer += 1;
        let timer = self.timer;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_DELAY).await;
            this.update(cx, |this, cx| {
                if this.timer == timer {
                    this.keep_draft(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps what's typed as the draft now, or drops the draft when it's the same as the skill (or empty).
    pub fn keep_draft(&mut self, cx: &mut App) {
        self.pending = false;
        self.timer += 1;
        let differ = self.values.differ(self.skill.as_ref());
        put(cx, self.name().map(str::to_string).as_deref(), differ.then(|| self.values.clone()));
    }

    fn can_save(&self) -> bool {
        self.busy.is_none() && !self.too_long && self.values.ready() && self.values.differ(self.skill.as_ref())
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_draft(cx);
        self.busy = Some(Busy::Saving);
        self.error = None;
        cx.notify();
        let values = self.values.clone();
        let args = json!({ "name": values.name.trim(), "description": values.description, "body": values.body });
        backend::call_in(
            window,
            cx,
            "skills:save",
            args,
            move |this: &mut Self, result: Result<Value, String>, _, cx| {
                this.busy = None;
                match result {
                    Ok(saved) => {
                        let name = saved.as_str().unwrap_or_default().to_string();
                        let was_new = this.skill.is_none();
                        put(cx, this.name().map(str::to_string).as_deref(), None);
                        // The backend's copy: it trims the fields and may tidy the name. The folder keeps its other files.
                        let (folder, files) = this
                            .skill
                            .as_ref()
                            .map(|skill| (skill.folder.clone(), skill.files.clone()))
                            .unwrap_or_default();
                        this.skill = Some(Skill {
                            name: name.clone(),
                            description: values.description.split_whitespace().collect::<Vec<_>>().join(" "),
                            body: values.body.trim().to_string(),
                            source: "owner".into(),
                            folder,
                            files,
                        });
                        this.saved = true;
                        // Typed while it was saving: kept as the saved skill's draft.
                        if this.values != values {
                            this.values.name = name;
                            this.keep_draft(cx);
                            this.saved = false;
                        } else {
                            // Now a saved skill (its name fixed, as the backend has it), shown to read.
                            if was_new {
                                this.values.name = name;
                            }
                            this.editing = false;
                            this.show_page(None, cx);
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            },
        );
    }

    /// Drops the draft: a skill goes back to what's saved, a new one to empty fields.
    fn discard(&mut self, cx: &mut Context<Self>) {
        self.values = self.skill.as_ref().map(Values::of).unwrap_or_default();
        self.too_long = false;
        self.keep_draft(cx);
        self.editing = self.skill.is_none();
        self.show_page(self.skill.is_none().then_some("title"), cx);
        cx.notify();
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.name().map(str::to_string) else { return };
        let view = cx.entity().downgrade();
        let title = format!("Remove {}?", display_name(&name));
        ui::confirm(
            window,
            cx,
            title,
            "Filo and its workers stop using it.",
            "Remove",
            "Cancel",
            true,
            move |window, cx| {
                let name = name.clone();
                view.update(cx, |this, cx| {
                    this.busy = Some(Busy::Removing);
                    cx.notify();
                    backend::call_in(
                        window,
                        cx,
                        "skills:remove",
                        json!({ "name": name }),
                        move |this: &mut Self, result: Result<Value, String>, _, cx| {
                            this.busy = None;
                            match result {
                                Ok(_) => {
                                    this.values = Values::default();
                                    put(cx, Some(&name), None);
                                    cx.emit(EditorEvent::Removed);
                                }
                                Err(error) => this.error = Some(error),
                            }
                            cx.notify();
                        },
                    );
                })
                .ok();
            },
        );
        if let Some(page) = &mut self.page {
            page.hide_for_dialog(window, cx, |this: &mut Self| this.page.as_mut());
        }
    }

    /// The breadcrumb's title.
    fn title(&self) -> SharedString {
        match &self.skill {
            Some(skill) => display_name(&skill.name).into(),
            None => "New skill".into(),
        }
    }

    /// The buttons at the right of the title bar, beside the breadcrumb: nothing else sits over the writing, as on the
    /// Notes page. Save carries where the skill is, as the note's Save button does.
    fn render_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let differ = self.values.differ(self.skill.as_ref());
        let mut buttons = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::SM))
            // Clicks here aren't drags of the window, which the bar under them is.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if self.skill.is_none() {
            buttons = buttons.child(Button::ghost("skill-import", "Import").small().icon("file").on_click(
                cx.listener(|_, _: &ClickEvent, window, cx| {
                    let task = super::skills::pick_skill(window, cx);
                    cx.spawn_in(window, async move |this, cx| {
                        if let Some(result) = task.await {
                            this.update_in(cx, |this, window, cx| match result {
                                Ok(Picked::File(values)) => this.import(values, cx),
                                Ok(Picked::Folder(path)) => {
                                    super::skills::import_folder(path, window, cx, |_: &mut Self, name, cx| {
                                        cx.emit(EditorEvent::Imported(name))
                                    })
                                }
                                Err(error) => {
                                    this.error = Some(error);
                                    cx.notify();
                                }
                            })
                            .ok();
                        }
                    })
                    .detach();
                }),
            ));
        }
        if let Some(folder) = self.skill.as_ref().and_then(|skill| skill.folder.clone()) {
            buttons = buttons.child(IconButton::new("skill-folder", "folder").tooltip("Show in Finder").on_click(
                cx.listener(move |_, _: &ClickEvent, window, cx| {
                    if let Err(error) = open::that(&folder) {
                        ui::toast_error(window, cx, "Couldn’t show this skill in Finder", error.to_string());
                    }
                }),
            ));
        }
        if self.skill.is_some() {
            let edit = if self.editing {
                Button::ghost("skill-edit", "Done")
                    .small()
                    .disabled(self.too_long)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_editing(cx)))
            } else {
                Button::ghost("skill-edit", "Edit")
                    .small()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_editing(cx)))
            };
            buttons = buttons.child(crate::smoke::tracked("skill-edit", edit));
        }
        if differ && self.busy.is_none() {
            buttons = buttons.child(
                Button::ghost("skill-discard", "Discard")
                    .small()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.discard(cx))),
            );
        }
        if self.skill.is_some() {
            buttons = buttons.child(
                Button::ghost("skill-remove", "Remove")
                    .small()
                    .loading(self.busy == Some(Busy::Removing))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.remove(window, cx))),
            );
        }
        let label = match self.busy {
            Some(Busy::Saving) => "Saving…",
            _ if self.saved => "Saved",
            _ => "Save",
        };
        let save = Button::primary("skill-save", label)
            .small()
            .disabled(!self.can_save())
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)));
        buttons.child(crate::smoke::tracked("skill-save", save)).into_any_element()
    }

    /// What else the skill's folder holds, in a line: its scripts, templates and references.
    fn files_line(&self) -> Option<String> {
        const SHOWN: usize = 4;
        let files = &self.skill.as_ref()?.files;
        if files.is_empty() {
            return None;
        }
        let mut named = files.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
        if files.len() > SHOWN {
            named.push_str(", …");
        }
        let count = if files.len() == 1 { "1 file".to_string() } else { format!("{} files", files.len()) };
        Some(format!("Also in its folder ({count}): {named}"))
    }

    fn render_banners(&self, cx: &App) -> Option<AnyElement> {
        let files = self.files_line();
        if !self.too_long && self.error.is_none() && files.is_none() {
            return None;
        }
        Some(
            ui::page_column()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .pt(px(space::SM))
                .when(self.too_long, |this| {
                    this.child(ui::banner(
                        format!("The instructions are too long to save (over {MAX_BODY} characters). Shorten them a little to keep your changes."),
                        ui::Tone::Danger,
                        cx,
                    ))
                })
                .when_some(self.error.clone(), |this, error| this.child(ui::banner(error, ui::Tone::Danger, cx)))
                .when_some(files, |this, files| this.child(div().footnote().text_color(cx.colors().text_secondary).truncate().child(files)))
                .into_any_element(),
        )
    }
}

impl Render for SkillEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_page(window, cx);
        // The page follows the app's theme and text size.
        if let Some(page) = &mut self.page {
            page.follow_theme(cx);
        }
        let body = div().size_full().flex().flex_col().children(self.render_banners(cx)).when_some(
            self.page.as_ref(),
            |this, page| {
                this.child(crate::smoke::tracked_grow(
                    "skill-page",
                    div().flex_1().min_h_0().w_full().child(page.web().clone()),
                ))
            },
        );
        let crumbs = breadcrumb(
            "skills-crumb",
            "Skills",
            self.title(),
            cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(EditorEvent::Back)),
            cx,
        );
        let bar = pane_bar("skill-bar", Some(crate::smoke::tracked("skill-crumbs", crumbs)))
            .gap(px(space::MD))
            .child(div().flex_1())
            .child(self.render_actions(cx));
        pane(bar, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(description: &str, body: &str) -> Skill {
        Skill {
            name: "weekly-report".into(),
            description: description.into(),
            body: body.into(),
            source: "owner".into(),
            folder: None,
            files: Vec::new(),
        }
    }

    #[test]
    fn a_draft_is_what_differs_from_the_saved_skill() {
        let saved = skill("When asked for a report", "1. Group by project.");
        let same = Values {
            name: "weekly-report".into(),
            description: " When asked for a report".into(),
            body: "1. Group by project.\n".into(),
        };
        assert!(!same.differ(Some(&saved)));
        let changed = Values { body: "1. Group by project.\n2. Add dates.".into(), ..same.clone() };
        assert!(changed.differ(Some(&saved)));
        assert!(!Values::default().differ(None));
        assert!(Values { description: "x".into(), ..Values::default() }.differ(None));
    }

    #[test]
    fn saving_needs_every_field() {
        let mut values =
            Values { name: "release-notes".into(), description: "When writing release notes".into(), body: " ".into() };
        assert!(!values.ready());
        values.body = "Group by screen.".into();
        assert!(values.ready());
    }

    #[test]
    fn drafts_read_back_from_the_file() {
        let mut stored = Stored {
            new: Some(Values { name: "a".into(), description: "b".into(), body: "c".into() }),
            ..Stored::default()
        };
        stored.skills.insert(
            "weekly-report".into(),
            Values { name: "weekly-report".into(), description: "d".into(), body: "e".into() },
        );
        let read: Stored = serde_json::from_slice(&serde_json::to_vec(&stored).unwrap()).unwrap();
        assert_eq!(read, stored);
        assert_eq!(serde_json::from_str::<Stored>("{}").unwrap(), Stored::default());
    }
}

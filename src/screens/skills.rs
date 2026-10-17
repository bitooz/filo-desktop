// Skills, a page of the Extensions tab: how the user likes things done, written as instructions in the Agent Skills
// format. Filo and its workers use all of them, and Filo gives the coding tools the ones a piece of work needs. Each
// is a folder on this Mac (filo_local skills.rs), its SKILL.md and whatever files it holds, read here as they are. The
// page lists them as cards (yours and the ones Filo learned); a card opens the skill under "Skills › <name>" to read,
// change or remove (skill_editor.rs, the Notes page's editor), and New skill or Import adds one: a skill folder comes in
// whole, a lone SKILL.md opens as a new skill to check. A card with changes not saved yet says Draft.

use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use serde::Deserialize;
use serde_json::json;

use super::connectors::{self, card, grid, page_title, pane, pane_bar};
use super::skill_editor::{self, EditorEvent, SkillEditor, Values};
use crate::backend;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};

/// The backend's limits, so a field stops where it would refuse.
pub use filo_local::skills::{MAX_BODY, MAX_DESCRIPTION, MAX_NAME};
/// With more skills than this, the page gets a search field.
const SEARCH_FROM: usize = 6;

#[derive(Clone, Debug, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// "owner" or "filo" (learned by Filo).
    #[serde(default)]
    pub source: String,
    /// Its folder on this Mac.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// The folder's other files ("scripts/check.py").
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Yours,
    Learned,
}

/// A skill's name as a heading: "weekly-report" reads "Weekly report".
pub fn display_name(name: &str) -> String {
    let spaced = name.replace('-', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

enum View {
    List,
    /// A skill's page, or a new one's.
    Editor(Entity<SkillEditor>),
}

/// What Import picked: a skill folder, to copy in whole (`skills:import`), or a lone SKILL.md's fields for a new skill.
pub enum Picked {
    Folder(PathBuf),
    File(Values),
}

/// Picks a skill's folder or a SKILL.md (nothing when the pick is cancelled).
pub fn pick_skill<V: 'static>(window: &mut Window, cx: &mut Context<V>) -> Task<Option<Result<Picked, String>>> {
    let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: true,
        directories: true,
        multiple: false,
        prompt: Some("Import".into()),
    });
    window.spawn(cx, async move |_| {
        let path = paths.await.ok()?.ok()??.into_iter().next()?;
        if path.is_dir() {
            return Some(if path.join("SKILL.md").is_file() {
                Ok(Picked::Folder(path))
            } else {
                Err("That folder has no SKILL.md.".into())
            });
        }
        Some(match std::fs::read_to_string(&path) {
            Ok(text) => {
                let file = filo_local::skills::parse(&text);
                let name = file
                    .name
                    .or_else(|| {
                        path.parent().and_then(|dir| dir.file_name()).map(|dir| dir.to_string_lossy().to_string())
                    })
                    .unwrap_or_default();
                Ok(Picked::File(Values { name, description: file.description.unwrap_or_default(), body: file.body }))
            }
            Err(error) => Err(format!("Couldn’t read that file: {error}")),
        })
    })
}

pub struct SkillsScreen {
    skills: Option<Result<Vec<Skill>, String>>,
    view: View,
    filter: Filter,
    search: Entity<InputState>,
    query: String,
    /// A skill to open once the list is in (`FILO_ABILITIES=skill:<name>`, or "new").
    #[cfg(feature = "dev")]
    open_when_loaded: Option<String>,
    /// A skill just imported, opened once the list has it.
    imported: Option<String>,
    _sub: Task<()>,
    _search_sub: Subscription,
    _editor_sub: Option<Subscription>,
}

impl SkillsScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _sub = backend::subscribe(
            cx,
            "skills:list",
            json!({}),
            |this: &mut Self, result: Result<Vec<Skill>, String>, cx| {
                if let (View::Editor(editor), Ok(skills)) = (&this.view, &result) {
                    editor.update(cx, |editor, cx| {
                        let skill = editor.name().and_then(|name| skills.iter().find(|skill| skill.name == name));
                        editor.skill_changed(skill, cx);
                    });
                }
                this.skills = Some(result);
                cx.notify();
            },
        );
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search skills"));
        let _search_sub = cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = state.read(cx).value().to_string();
                cx.notify();
            }
        });
        SkillsScreen {
            skills: None,
            view: View::List,
            filter: Filter::All,
            search,
            query: String::new(),
            #[cfg(feature = "dev")]
            open_when_loaded: None,
            imported: None,
            _sub,
            _search_sub,
            _editor_sub: None,
        }
    }

    #[cfg(feature = "dev")]
    pub fn open_when_loaded(&mut self, name: String) {
        self.open_when_loaded = Some(name);
    }

    /// The skill `name` as the server last sent it (smoke test).
    #[cfg(feature = "dev")]
    pub fn skill(&self, name: &str) -> Option<Skill> {
        self.skills.as_ref()?.as_ref().ok()?.iter().find(|skill| skill.name == name).cloned()
    }

    /// The open skill's page (smoke test).
    #[cfg(feature = "dev")]
    pub fn editor(&self) -> Option<Entity<SkillEditor>> {
        match &self.view {
            View::Editor(editor) => Some(editor.clone()),
            View::List => None,
        }
    }

    /// Hidden under a pushed page or another tab: the skill's page is a native web view, drawn above everything.
    pub fn set_page_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if let View::Editor(editor) = &self.view {
            editor.update(cx, |editor, cx| editor.set_page_visible(visible, cx));
        }
    }

    pub fn show_list(&mut self, cx: &mut Context<Self>) {
        // The page keeps its draft as it goes, so the cards can say which have one.
        if let View::Editor(editor) = &self.view {
            editor.update(cx, |editor, cx| editor.keep_draft(cx));
        }
        self.view = View::List;
        self._editor_sub = None;
        cx.notify();
    }

    /// Opens `skill`'s page, or a new skill's filled with `values` (an import) or its draft.
    fn open_editor(
        &mut self,
        skill: Option<Skill>,
        values: Option<Values>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| SkillEditor::new(skill, values, window, cx));
        self._editor_sub = Some(cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| match event {
            EditorEvent::Back | EditorEvent::Removed => this.show_list(cx),
            EditorEvent::Imported(name) => {
                this.imported = Some(name.clone());
                this.show_list(cx);
            }
        }));
        self.view = View::Editor(editor);
        cx.notify();
    }

    fn open(&mut self, skill: &Skill, window: &mut Window, cx: &mut Context<Self>) {
        self.open_editor(Some(skill.clone()), None, window, cx);
    }

    fn new_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_editor(None, None, window, cx);
    }

    /// Picks a skill folder, copied in whole and then opened, or a SKILL.md, opened as a new skill filled from it for
    /// the user to check.
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = pick_skill(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(result) = task.await else { return };
            this.update_in(cx, |this, window, cx| match result {
                Ok(Picked::File(values)) => this.open_editor(None, Some(values), window, cx),
                Ok(Picked::Folder(path)) => import_folder(path, window, cx, |this: &mut Self, name, cx| {
                    this.imported = Some(name);
                    cx.notify();
                }),
                Err(error) => ui::toast_error(window, cx, "Couldn’t import the skill", error),
            })
            .ok();
        })
        .detach();
    }

    fn filter_pill(&self, filter: Filter, label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let selected = self.filter == filter;
        div()
            .id(SharedString::from(format!("skills-filter-{label}")))
            .px(px(space::MD))
            .min_h(px(30.))
            .flex()
            .items_center()
            .rounded(px(radius::FULL))
            .cursor_pointer()
            .footnote()
            .map(|this| {
                if selected {
                    this.bg(colors.selected_fill).semibold().text_color(colors.text)
                } else {
                    this.medium().text_color(colors.text_secondary).hover(|style| style.text_color(colors.text))
                }
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.filter = filter;
                cx.notify();
            }))
            .child(label)
            .into_any_element()
    }

    fn skill_icon(size: f32, cx: &Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        ui::icon_sized("skills", size, colors.accent).into_any_element()
    }

    fn render_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let drafted = skill_editor::drafted(cx);
        let actions = vec![
            Button::secondary("skills-import", "Import")
                .small()
                .icon("file")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.import(window, cx)))
                .into_any_element(),
            crate::smoke::tracked(
                "skills-new",
                Button::primary("skills-new", "New skill")
                    .small()
                    .icon("plus")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_skill(window, cx))),
            ),
        ];
        let mut column = ui::page_column().pb(px(48.)).flex().flex_col().child(page_title(
            "Skills",
            "How you like things done, written as instructions. Filo and its workers use all of them, and give Claude Code or Codex the ones a piece of work needs.",
            actions,
            cx,
        ));
        match self.skills.clone() {
            None => column = column.child(div().pt(px(space::XXL)).child(ui::skeleton_rows(4, Some(40.), cx))),
            Some(Err(error)) => column = column.child(div().pt(px(space::XXL)).child(ui::banner(error, ui::Tone::Danger, cx))),
            Some(Ok(skills)) if skills.is_empty() => {
                column = column.child(
                    div()
                        .pt(px(space::XXL))
                        .footnote()
                        .text_color(colors.text_tertiary)
                        .child("No skills yet. Add one, or teach Filo how you like something done and ask it to save that as a skill."),
                )
            }
            Some(Ok(skills)) => {
                let learned = skills.iter().filter(|skill| skill.source == "filo").count();
                let mut tools = div().pt(px(space::XL)).pb(px(space::SM)).flex().items_center().gap(px(space::XS));
                // The filters only when there are both kinds.
                if learned > 0 && learned < skills.len() {
                    tools = tools.child(self.filter_pill(Filter::All, "All", cx)).child(self.filter_pill(Filter::Yours, "Yours", cx)).child(self.filter_pill(Filter::Learned, "Learned by Filo", cx));
                }
                tools = tools.child(div().flex_1());
                if skills.len() > SEARCH_FROM {
                    tools = tools.child(div().w(px(220.)).child(Input::new(&self.search).cleanable(true).prefix(ui::icon_sized("search", 14., colors.text_secondary))));
                }
                column = column.child(tools);
                let needle = self.query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
                let shown: Vec<Skill> = skills
                    .into_iter()
                    .filter(|skill| match self.filter {
                        Filter::All => true,
                        Filter::Yours => skill.source != "filo",
                        Filter::Learned => skill.source == "filo",
                    })
                    .filter(|skill| needle.is_empty() || display_name(&skill.name).to_lowercase().contains(&needle) || skill.description.to_lowercase().contains(&needle))
                    .collect();
                if shown.is_empty() {
                    column = column.child(div().py(px(space::XL)).subhead().text_color(colors.text_secondary).text_center().child(format!("No skills match “{}”.", self.query.trim())));
                } else {
                    let all = self.filter == Filter::All;
                    let cards = shown
                        .into_iter()
                        .map(|skill| {
                            let tag = if drafted.contains(&skill.name) {
                                Some(ui::chip("Draft", ui::Tone::Warning, true, cx).into_any_element())
                            } else {
                                (all && skill.source == "filo").then(|| ui::chip("Learned", ui::Tone::Neutral, true, cx).into_any_element())
                            };
                            card(format!("skill-{}", skill.name), Self::skill_icon(20., cx), display_name(&skill.name), skill.description.clone().into(), tag, cx)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.open(&skill, window, cx)))
                                .into_any_element()
                        })
                        .collect();
                    column = column.child(grid(cards, connectors::columns_for_pane()));
                }
            }
        }
        pane(pane_bar("skills-bar", None), div().id("skills-scroll").size_full().overflow_y_scroll().child(column))
    }
}

/// Copies the skill folder at `path` in (`skills:import`), then `done` with the name it got; an error is a toast.
pub fn import_folder<V: 'static>(
    path: PathBuf,
    window: &mut Window,
    cx: &mut Context<V>,
    done: impl FnOnce(&mut V, String, &mut Context<V>) + 'static,
) {
    backend::call_in(
        window,
        cx,
        "skills:import",
        json!({ "path": path }),
        move |this: &mut V, result: Result<String, String>, window, cx| match result {
            Ok(name) => done(this, name, cx),
            Err(error) => ui::toast_error(window, cx, "Couldn’t import the skill", error),
        },
    );
}

impl Render for SkillsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let (Some(name), Some(Ok(skills))) = (self.imported.clone(), self.skills.clone())
            && let Some(skill) = skills.iter().find(|skill| skill.name == name)
        {
            self.imported = None;
            self.open(skill, window, cx);
        }
        #[cfg(feature = "dev")]
        if let (Some(name), Some(Ok(skills))) = (self.open_when_loaded.clone(), self.skills.clone()) {
            self.open_when_loaded = None;
            match skills.iter().find(|skill| skill.name == name) {
                Some(skill) => self.open(skill, window, cx),
                None => self.new_skill(window, cx),
            }
        }
        match &self.view {
            View::List => self.render_list(cx),
            View::Editor(editor) => editor.clone().into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn names_read_as_headings() {
        assert_eq!(display_name("weekly-report"), "Weekly report");
        assert_eq!(display_name("png-to-svg"), "Png to svg");
    }
}

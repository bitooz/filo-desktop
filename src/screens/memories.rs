// "What your agents remember": memories:list 30 at a time, newest first, with a Forget button on each, and writing
// them yourself:
// the header's + adds one (`memories:create`), a row's pencil edits it in place
// (`memories:update`); Enter saves, Escape or Cancel leaves it as it was.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend;
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, Button, IconButton};

const PAGE_SIZE: f64 = 30.;
/// As long as a memory can be (crates/filo-local memories.rs `MAX_CONTENT`).
const CONTENT_MAX: usize = 500;

#[derive(Clone, Debug, Deserialize)]
struct Provenance {
    #[serde(default)]
    explanation: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Memory {
    #[serde(rename = "_id")]
    id: String,
    content: String,
    provenance: Provenance,
    confirmation: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    page: Vec<Memory>,
    is_done: bool,
    continue_cursor: String,
}

/// One loaded page, each kept live.
struct PageSub {
    result: Option<Result<Page, String>>,
    _sub: Task<()>,
}

/// The memory being written: a new one (`id` none) or one being edited.
struct Editor {
    id: Option<String>,
    /// What it said when editing started (Save is off until it changes).
    initial: String,
    input: Entity<InputState>,
    saving: bool,
    _sub: Subscription,
}

pub struct MemoriesScreen {
    pages: Vec<PageSub>,
    editor: Option<Editor>,
}

impl MemoriesScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = MemoriesScreen { pages: Vec::new(), editor: None };
        this.load_page(Value::Null, cx);
        this
    }

    fn load_page(&mut self, cursor: Value, cx: &mut Context<Self>) {
        let index = self.pages.len();
        let args = json!({ "paginationOpts": { "numItems": PAGE_SIZE, "cursor": cursor } });
        let _sub =
            backend::subscribe(cx, "memories:list", args, move |this: &mut Self, result: Result<Page, String>, cx| {
                if let Some(page) = this.pages.get_mut(index) {
                    page.result = Some(result);
                }
                cx.notify();
            });
        self.pages.push(PageSub { result: None, _sub });
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(Some(Ok(last))) = self.pages.last().map(|page| page.result.as_ref()) else { return };
        if last.is_done {
            return;
        }
        let cursor = Value::String(last.continue_cursor.clone());
        self.load_page(cursor, cx);
        cx.notify();
    }

    fn forget(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        backend::call_in(
            window,
            cx,
            "memories:remove",
            json!({ "id": id }),
            |_: &mut Self, result: Result<bool, String>, window, cx| {
                if let Err(error) = result {
                    ui::toast_error(window, cx, "Couldn’t forget this memory", error);
                }
            },
        );
    }

    /// Opens the editor on a new memory (`id` none) or on one to change, with its text.
    fn edit(&mut self, id: Option<String>, content: String, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = "Something your agents should remember about you";
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder(placeholder)
                .validate(|text, _| text.chars().count() <= CONTENT_MAX);
            input.set_value(content.clone(), window, cx);
            input
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let _sub = cx.subscribe_in(&input, window, |this: &mut Self, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => this.save(window, cx),
            _ => {}
        });
        self.editor = Some(Editor { id, initial: content, input, saving: false, _sub });
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        cx.notify();
    }

    fn can_save(&self, cx: &gpui::App) -> bool {
        self.editor.as_ref().is_some_and(|editor| {
            let text = editor.input.read(cx).value();
            let text = text.trim();
            !editor.saving && !text.is_empty() && text != editor.initial.trim()
        })
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        let Some(editor) = self.editor.as_mut() else { return };
        editor.saving = true;
        let content = editor.input.read(cx).value().trim().to_string();
        let (path, args) = match &editor.id {
            Some(id) => ("memories:update", json!({ "id": id, "content": content })),
            None => ("memories:create", json!({ "content": content })),
        };
        cx.notify();
        backend::call_in(window, cx, path, args, |this: &mut Self, result: Result<Value, String>, window, cx| {
            match result {
                Ok(Value::Bool(false)) => {
                    this.editor = None;
                    ui::toast_error(window, cx, "Couldn’t save this memory", "It was forgotten in the meantime.");
                }
                Ok(_) => this.editor = None,
                Err(error) => {
                    if let Some(editor) = this.editor.as_mut() {
                        editor.saving = false;
                    }
                    ui::toast_error(window, cx, "Couldn’t save this memory", error);
                }
            }
            cx.notify();
        });
    }

    /// The editor in a row's place, or at the top for a new memory.
    fn render_editor(&self, editor: &Editor, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let count = editor.input.read(cx).value().chars().count();
        let label = if editor.id.is_some() { "Save" } else { "Remember" };
        div()
            .min_h(px(82.))
            .py(px(space::MD))
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .border_b_1()
            .border_color(colors.border)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                Input::new(&editor.input)
                    .suffix(div().footnote().text_color(colors.text_secondary).child(format!("{count}/{CONTENT_MAX}"))),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(space::SM))
                    .child(
                        Button::ghost("memory-cancel", "Cancel")
                            .small()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel(cx))),
                    )
                    .child(
                        Button::primary("memory-save", label)
                            .small()
                            .disabled(!self.can_save(cx) && !editor.saving)
                            .loading(editor.saving)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_memory(&self, memory: &Memory, cx: &mut Context<Self>) -> AnyElement {
        if let Some(editor) = self.editor.as_ref().filter(|editor| editor.id.as_deref() == Some(memory.id.as_str())) {
            return self.render_editor(editor, cx);
        }
        let colors = *cx.colors();
        let why = if memory.provenance.explanation.is_empty() {
            "Saved from a conversation"
        } else {
            &memory.provenance.explanation
        };
        let confirmed = if memory.confirmation == "inferred" { "Not yet confirmed" } else { "Confirmed" };
        let id = memory.id.clone();
        let (edit_id, content) = (memory.id.clone(), memory.content.clone());
        div()
            .min_h(px(82.))
            .py(px(space::MD))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .border_b_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().callout().semibold().child(memory.content.clone()))
                    .child(
                        div()
                            .mt(px(space::XS))
                            .caption()
                            .text_color(colors.text_secondary)
                            .child(format!("{why} · {confirmed}")),
                    ),
            )
            .child(
                IconButton::new(SharedString::from(format!("edit-{}", memory.id)), "square-pen")
                    .tooltip("Edit")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.edit(Some(edit_id.clone()), content.clone(), window, cx)
                    })),
            )
            .child(
                IconButton::new(SharedString::from(format!("forget-{}", memory.id)), "trash-2")
                    .color(colors.danger)
                    .tooltip("Forget")
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.forget(id.clone(), window, cx))),
            )
            .into_any_element()
    }
}

impl Render for MemoriesScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let add = IconButton::new("memories-add", "plus")
            .small()
            .tooltip("Add a memory")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.edit(None, String::new(), window, cx)));
        let header = ui::sub_page_header("memories", "What your agents remember", vec![add.into_any_element()], cx);

        let error = self.pages.iter().find_map(|page| match &page.result {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        });
        let first_loading = self.pages.first().is_none_or(|page| page.result.is_none());
        let body: AnyElement = if let Some(error) = error {
            ui::page_column().pt(px(space::LG)).child(ui::banner(error, ui::Tone::Danger, cx)).into_any_element()
        } else if first_loading {
            ui::skeleton_page(&[6], None, cx).into_any_element()
        } else {
            let memories: Vec<&Memory> = self
                .pages
                .iter()
                .filter_map(|page| page.result.as_ref()?.as_ref().ok())
                .flat_map(|page| page.page.iter())
                .collect();
            let loading_more = self.pages.last().is_some_and(|page| page.result.is_none());
            let can_load_more =
                self.pages.last().is_some_and(|page| matches!(&page.result, Some(Ok(page)) if !page.is_done));
            let rows: Vec<AnyElement> = memories.iter().map(|memory| self.render_memory(memory, cx)).collect();
            let adding =
                self.editor.as_ref().filter(|editor| editor.id.is_none()).map(|editor| self.render_editor(editor, cx));
            let is_adding = adding.is_some();
            div()
                .id("memories-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(
                    ui::page_column()
                        .pt(px(space::LG))
                        .pb(px(32.))
                        .flex()
                        .flex_col()
                        .child(div().mb(px(space::XL)).callout().text_color(colors.text_secondary).child(
                            "Each memory keeps an explanation of where it came from. You can ask your agents to correct one in conversation, or add, edit and remove them here.",
                        ))
                        .children(adding)
                        .when(rows.is_empty() && !loading_more && !is_adding, |this| {
                            this.child(
                                div()
                                    .mt(px(space::XXL))
                                    .text_center()
                                    .callout()
                                    .text_color(colors.text_secondary)
                                    .child("Your agents haven’t saved anything about you yet."),
                            )
                        })
                        .children(rows)
                        .when(can_load_more || loading_more, |this| {
                            this.child(
                                div().mt(px(space::XL)).flex().justify_center().child(
                                    Button::secondary("memories-more", "Show older memories")
                                        .disabled(loading_more)
                                        .loading(loading_more)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.load_more(cx))),
                                ),
                            )
                        }),
                )
                .into_any_element()
        };
        ui::sub_page(header, body, cx)
    }
}

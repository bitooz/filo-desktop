// Notes tab: every note in Filo's
// database, pinned first, with search, an optimistic pin toggle and a New button.
// Right-click a note to share it to an agent (chat/note_share.rs): its chat opens
// with the note above its composer.

use std::collections::HashMap;

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use serde::Deserialize;
use serde_json::json;

use crate::backend;
use crate::format;
use crate::nav::{self, Route};
use crate::screens::chat::note_share::SharedNote;
use crate::screens::note_text::note_preview;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, IconButton};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    #[serde(rename = "_id")]
    id: String,
    title: String,
    body: String,
    #[serde(default)]
    tags: Vec<String>,
    pinned: Option<bool>,
    updated_at: f64,
}

/// A note with what its row shows, worked out once per update.
struct Entry {
    note: Note,
    heading: SharedString,
    preview: Option<SharedString>,
    date: SharedString,
    search_text: String,
}

impl Entry {
    fn new(note: Note) -> Self {
        let preview = note_preview(&note.body);
        let heading = if !note.title.is_empty() {
            note.title.clone()
        } else if !preview.is_empty() {
            preview.clone()
        } else {
            "Untitled".to_string()
        };
        let show_preview = !preview.is_empty() && (note.title.is_empty() || preview != note.title);
        let mut search_parts = vec![note.title.as_str(), note.body.as_str()];
        search_parts.extend(note.tags.iter().map(String::as_str));
        Entry {
            heading: heading.into(),
            preview: show_preview.then(|| preview.into()),
            date: format::short_date_time(note.updated_at).into(),
            search_text: search_parts.join("\n").to_lowercase(),
            note,
        }
    }
}

pub struct NotesScreen {
    /// The notes, the most recently changed first, as rows.
    notes: Option<Result<Vec<Entry>, String>>,
    /// Pin changes shown before the database has them.
    pin_overrides: HashMap<String, bool>,
    search_active: bool,
    search: Entity<InputState>,
    query: String,
    _sub: Task<()>,
    _search_sub: Subscription,
}

impl NotesScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search notes"));
        let _search_sub = cx.subscribe_in(&search, window, |this: &mut Self, state, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.query = state.read(cx).value().to_string();
                cx.notify();
            }
        });
        let _sub = backend::subscribe(
            cx,
            "notes:mine",
            json!({}),
            |this: &mut Self, result: Result<Vec<Note>, String>, cx| {
                if let Ok(notes) = &result {
                    // An override goes once the database agrees with it or the note is gone.
                    let saved: HashMap<&str, bool> =
                        notes.iter().map(|note| (note.id.as_str(), note.pinned == Some(true))).collect();
                    this.pin_overrides.retain(|id, pinned| saved.get(id.as_str()).is_some_and(|saved| saved != pinned));
                }
                this.notes = Some(result.map(|notes| notes.into_iter().map(Entry::new).collect()));
                cx.notify();
            },
        );
        NotesScreen {
            notes: None,
            pin_overrides: HashMap::new(),
            search_active: false,
            search,
            query: String::new(),
            _sub,
            _search_sub,
        }
    }

    fn expand_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_active = true;
        self.search.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn collapse_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.search.update(cx, |state, cx| state.set_value("", window, cx));
        self.search_active = false;
        cx.notify();
    }

    fn toggle_pin(&mut self, id: String, pinned: bool, cx: &mut Context<Self>) {
        self.pin_overrides.insert(id.clone(), pinned);
        cx.notify();
        backend::call(
            cx,
            "notes:setPinnedMine",
            json!({ "id": id, "pinned": pinned }),
            move |this: &mut Self, result: Result<bool, String>, cx| {
                if result != Ok(true) && this.pin_overrides.get(&id) == Some(&pinned) {
                    this.pin_overrides.remove(&id);
                    cx.notify();
                }
            },
        );
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        if self.search_active {
            return div()
                .flex()
                .items_center()
                .gap(px(space::MD))
                .min_h(px(44.))
                .pt(px(space::XL))
                .pb(px(space::SM))
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.search)
                            .prefix(ui::icon_sized("search", 16., colors.text_secondary))
                            .cleanable(true),
                    ),
                )
                .child(
                    div()
                        .id("notes-search-cancel")
                        .cursor_pointer()
                        .subhead()
                        .bold()
                        .hover(|style| style.opacity(0.7))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.collapse_search(window, cx))),
                )
                .into_any_element();
        }
        ui::page_header(
            "Notes",
            vec![
                IconButton::new("notes-search", "search")
                    .tooltip("Search notes")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.expand_search(window, cx)))
                    .into_any_element(),
                IconButton::new("notes-new", "plus")
                    .tooltip("New note")
                    .on_click(|_, window, cx| nav::push(Route::Note(None), window, cx))
                    .into_any_element(),
            ],
        )
        .into_any_element()
    }

    fn render_row(&self, entry: &Entry, pinned: bool, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::ContextMenuExt as _;
        let colors = *cx.colors();
        let id = entry.note.id.clone();
        let open_id = id.clone();
        let shared = SharedNote { key: id.clone(), title: entry.note.title.clone(), body: entry.note.body.clone() };
        // Where the row is, for the note-share check to click (smoke.rs `run_note_share_drive`).
        let row = div()
            .id(SharedString::from(format!("note-{id}")))
            .flex()
            .items_start()
            // No rules between notes: a rounded highlight on hover, the text where it was.
            .mx(px(-space::MD))
            .px(px(space::MD))
            .min_h(px(64.))
            .py(px(space::LG + space::XXS))
            .rounded(px(radius::MD))
            .cursor_pointer()
            .hover(|style| style.bg(colors.text.opacity(0.04)))
            .on_click(move |event: &ClickEvent, window, cx| {
                // A right-click opens the share menu, not the note.
                if event.standard_click() {
                    nav::push(Route::Note(Some(open_id.clone())), window, cx);
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .pr(px(space::SM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(space::SM))
                            .child(div().min_w_0().body().bold().truncate().child(entry.heading.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .caption()
                                    .text_color(colors.text_secondary)
                                    .opacity(0.8)
                                    .child(entry.date.clone()),
                            ),
                    )
                    .when_some(entry.preview.clone(), |this, preview| {
                        this.child(div().subhead().text_color(colors.text_secondary).line_clamp(2).child(preview))
                    })
                    .when(!entry.note.tags.is_empty(), |this| {
                        this.child(div().flex().flex_wrap().gap(px(space::XS)).children(entry.note.tags.iter().map(
                            |tag| {
                                div()
                                    .px(px(space::SM))
                                    .py(px(space::XXS))
                                    .rounded(px(radius::FULL))
                                    .bg(colors.surface_strong)
                                    .border_1()
                                    .border_color(colors.border)
                                    .micro()
                                    .font_weight(FontWeight::NORMAL)
                                    .text_color(colors.text_secondary)
                                    .child(format!("#{tag}"))
                            },
                        )))
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("note-pin-{id}")))
                    .flex_none()
                    .size(px(28.))
                    .mt(px(-2.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::FULL))
                    .opacity(if pinned { 1. } else { 0.8 })
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.selected_fill))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(if pinned { "Unpin note" } else { "Pin note" })
                            .build(window, cx)
                    })
                    .child(ui::icon_sized(
                        if pinned { "pin-filled" } else { "pin" },
                        17.,
                        if pinned { colors.text } else { colors.text_secondary },
                    ))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if event.standard_click() {
                            this.toggle_pin(id.clone(), !pinned, cx);
                        }
                    })),
            )
            .context_menu(move |menu, window, cx| share_menu(menu, shared.clone(), window, cx));
        // Its own id around the menu: gpui-component names every context menu "context-menu", so rows side by side
        // share one menu state and each drew the open menu, its shadow stacked into a grey halo.
        let row = div().id(SharedString::from(format!("note-menu-{}", entry.note.id))).child(row);
        crate::smoke::tracked(format!("note-row-{}", entry.note.id), row)
    }
}

/// A note's right-click menu: share it to an agent.
fn share_menu(
    menu: gpui_component::menu::PopupMenu,
    note: SharedNote,
    window: &mut Window,
    cx: &mut Context<gpui_component::menu::PopupMenu>,
) -> gpui_component::menu::PopupMenu {
    use gpui_component::menu::PopupMenuItem;
    let agents = nav::share_targets(cx);
    if agents.is_empty() {
        return menu;
    }
    crate::smoke::note_share_menu_item("Share to agent");
    menu.submenu("Share to agent", window, cx, move |menu, _, _| {
        agents.iter().fold(menu, |menu, agent| {
            crate::smoke::note_share_menu_item(&agent.name);
            let (destination, note, target) = (agent.id.clone(), note.clone(), agent.clone());
            // Its picture on its colour beside the name, like the Agents tab.
            let row = PopupMenuItem::element(move |_, cx| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(crate::screens::agents::picture_on(target.picture.clone(), target.background, 20., cx))
                    .child(target.name.clone())
            });
            menu.item(row.on_click(move |_, window, cx| nav::share_note(destination.clone(), note.clone(), window, cx)))
        })
    })
}

impl Render for NotesScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        // Esc closes the search, as its Cancel does.
        if self.search_active {
            crate::escape::layer(window, cx, |this, window, cx| this.collapse_search(window, cx));
        }
        let header = self.render_header(cx);
        let content: AnyElement = match &self.notes {
            // Note rows while they load: a title with its date, and a line of preview.
            None => div()
                .flex()
                .flex_col()
                .children(
                    [(0.42, 0.8), (0.55, 0.62), (0.34, 0.9), (0.5, 0.7), (0.38, 0.55), (0.6, 0.84)].into_iter().map(
                        |(title, preview)| {
                            div()
                                .min_h(px(64.))
                                .py(px(space::LG + space::XXS))
                                .flex()
                                .flex_col()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(ui::skeleton(gpui::relative(title), 13., 6.))
                                        .child(ui::skeleton(px(54.), 9., 4.5).secondary()),
                                )
                                .child(ui::skeleton(gpui::relative(preview), 10., 5.).secondary())
                        },
                    ),
                )
                .into_any_element(),
            Some(Err(error)) => {
                div().pt(px(space::LG)).child(ui::banner(error.clone(), ui::Tone::Danger, cx)).into_any_element()
            }
            Some(Ok(entries)) => {
                let needle = self
                    .query
                    .split(char::is_whitespace)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                let mut pinned_rows = Vec::new();
                let mut other_rows = Vec::new();
                for entry in entries {
                    if !needle.is_empty() && !entry.search_text.contains(&needle) {
                        continue;
                    }
                    let pinned =
                        self.pin_overrides.get(&entry.note.id).copied().unwrap_or(entry.note.pinned == Some(true));
                    if pinned { pinned_rows.push((entry, true)) } else { other_rows.push((entry, false)) }
                }
                if pinned_rows.is_empty() && other_rows.is_empty() {
                    if entries.is_empty() {
                        div()
                            .flex_1()
                            .min_h(px(360.))
                            .child(ui::empty_state(
                                "notebook-pen",
                                "A place to put things down.",
                                Some("Jot anything you want to keep — ideas, lists, thoughts. Your notes will appear here.".into()),
                                None,
                                cx,
                            ))
                            .into_any_element()
                    } else {
                        div()
                            .mt(px(space::XXL))
                            .text_center()
                            .subhead()
                            .text_color(colors.text_secondary)
                            .child(format!("No notes match “{}”.", self.query.trim()))
                            .into_any_element()
                    }
                } else {
                    let rows: Vec<AnyElement> = pinned_rows
                        .into_iter()
                        .chain(other_rows)
                        .map(|(entry, pinned)| self.render_row(entry, pinned, cx))
                        .collect();
                    div().flex().flex_col().children(rows).into_any_element()
                }
            }
        };
        div()
            .id("notes-scroll")
            .size_full()
            .overflow_y_scroll()
            .when_some(crate::smoke::page_scroll("notes"), |this, handle| this.track_scroll(&handle))
            .child(crate::smoke::tracked(
                "notes-end",
                ui::page_column()
                    .min_h_full()
                    .flex()
                    .flex_col()
                    .pb(px(48.))
                    .child(div().mb(px(space::SM)).child(header))
                    .child(content)
                    .children(crate::smoke::page_end("notes")),
            ))
    }
}

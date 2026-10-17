// ⌘P's search box: a panel over the dimmed window that finds an agent, the group, or words in their messages. Empty,
// it lists the agents and the group by their latest message; typed into, the names that hold the letters typed in
// order (those letters bold), then the messages that hold every word typed (local_backend/search.rs), newest first.
// ↑↓ move, ↵ opens (a message opens its chat scrolled to it, flashing it), esc or a click outside closes. The shell
// (app.rs) shows it and does what it asks (`SearchEvent`).

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight, HighlightStyle,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Enter, Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use serde::Deserialize;
use serde_json::json;

use crate::backend;
use crate::screens::agents::{AgentsScreen, GROUP};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

const WIDTH: f32 = 380.;
const SEARCH_HEIGHT: f32 = 46.;
const ROW_HEIGHT: f32 = 50.;
const PICTURE: f32 = 32.;
/// A message's chat, under it.
const SMALL_PICTURE: f32 = 14.;
/// The most the list shows before it scrolls.
const LIST_MAX: f32 = 420.;
/// How long typing rests before the messages are searched.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(90);

/// What the panel asks the shell for.
pub enum SearchEvent {
    Close,
    /// An agent's chat or the group's (`GROUP`).
    OpenChat(String),
    /// A chat at one of its messages, `depth` messages from the newest.
    OpenMessage {
        chat: String,
        message: String,
        depth: usize,
    },
}

/// A message `agents:searchMessages` found.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundMessage {
    chat: String,
    message_id: String,
    line: String,
    at: f64,
    depth: usize,
}

/// A row: a chat (with the letters of its name that matched), or a message in one.
enum Item {
    Chat { id: String, name: String, preview: Option<String>, matched: Vec<usize> },
    Message { found: FoundMessage, name: String },
}

pub struct SearchPanel {
    input: Entity<InputState>,
    agents: WeakEntity<AgentsScreen>,
    query: String,
    selected: usize,
    /// The messages found, and the search they were found for.
    found: Vec<FoundMessage>,
    found_for: String,
    _searching: Option<Task<()>>,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for SearchPanel {}

impl SearchPanel {
    pub fn new(agents: &Entity<AgentsScreen>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let typed = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.typed(cx);
            }
        });
        // The agents' rows change as messages come in.
        let rows = cx.observe(agents, |_, _, cx| cx.notify());
        SearchPanel {
            input,
            agents: agents.downgrade(),
            query: String::new(),
            selected: 0,
            found: Vec::new(),
            found_for: String::new(),
            _searching: None,
            scroll: ScrollHandle::new(),
            _subs: vec![typed, rows],
        }
    }

    fn typed(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        if query == self.query {
            return;
        }
        self.query = query.clone();
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        if query.trim().is_empty() {
            self._searching = None;
            self.found.clear();
            self.found_for.clear();
        } else {
            self._searching = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SETTLE).await;
                this.update(cx, |this, cx| this.search(query, cx)).ok();
            }));
        }
        cx.notify();
    }

    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        backend::call(
            cx,
            "agents:searchMessages",
            json!({ "query": query }),
            move |this: &mut Self, found: Result<Vec<FoundMessage>, String>, cx| {
                // Only what was found for the words in the box now.
                if this.query != query {
                    return;
                }
                this.found = found.unwrap_or_default();
                this.found_for = query;
                cx.notify();
            },
        );
    }

    /// The rows, top to bottom.
    fn items(&self, cx: &App) -> Vec<Item> {
        let Some(agents) = self.agents.upgrade() else { return Vec::new() };
        let agents = agents.read(cx);
        let (rows, group) = agents.search_sources();
        // Each chat with its latest message's time.
        let mut chats: Vec<(String, String, Option<String>, f64)> = Vec::new();
        if let Some(group) = group {
            let preview = group.last.as_ref().map(|last| format!("{}: {}", last.speaker, last.preview));
            chats.push((GROUP.to_string(), group.title(), preview, group.last.as_ref().map_or(0., |last| last.at)));
        }
        for row in rows {
            let preview = row
                .last
                .as_ref()
                .map(|last| if last.from_user { format!("You: {}", last.preview) } else { last.preview.clone() });
            chats.push((row.id.clone(), row.name.clone(), preview, row.last.as_ref().map_or(0., |last| last.at)));
        }
        let query = self.query.trim();
        let mut scored: Vec<(i64, f64, Item)> = chats
            .into_iter()
            .filter_map(|(id, name, preview, at)| {
                let (score, matched) = if query.is_empty() { (0, Vec::new()) } else { name_match(&name, query)? };
                Some((score, at, Item::Chat { id, name, preview, matched }))
            })
            .collect();
        // The best matches first, then the latest chats (a stable sort keeps the list's own order after that).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
        let mut items: Vec<Item> = scored.into_iter().map(|(_, _, item)| item).collect();
        if !query.is_empty() && self.found_for == self.query {
            let name_of = |chat: &str| match chat {
                GROUP => group.map(|group| group.title()),
                id => rows.iter().find(|row| row.id == id).map(|row| row.name.clone()),
            };
            items.extend(
                self.found
                    .iter()
                    .filter_map(|found| Some(Item::Message { name: name_of(&found.chat)?, found: found.clone() })),
            );
        }
        items
    }

    fn move_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.items(cx).len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + step).clamp(0, count as isize - 1) as usize;
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items(cx).into_iter().nth(index) else { return };
        cx.emit(match item {
            Item::Chat { id, .. } => SearchEvent::OpenChat(id),
            Item::Message { found, .. } => {
                SearchEvent::OpenMessage { chat: found.chat, message: found.message_id, depth: found.depth }
            }
        });
    }

    /// The selected row, for the drive's log.
    #[cfg(feature = "dev")]
    pub fn drive_state(&self, cx: &App) -> (String, usize, Vec<String>) {
        let items = self.items(cx).into_iter().map(|item| match item {
            Item::Chat { name, .. } => name,
            Item::Message { name, found } => format!("{name} › {}", found.line),
        });
        (self.query.clone(), self.selected, items.collect())
    }

    /// Row `index`; `divided`: the first message under the names, with a rule over it. A chat: its picture, its name
    /// (the letters matched bold) and its latest message. A message: what was said (the words bold), and under it the
    /// chat's small picture, its name and when.
    fn render_row(&self, index: usize, item: &Item, divided: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let selected = index == self.selected;
        let (picture, main, under) = match item {
            Item::Chat { id, name, preview, matched } => {
                let preview = preview.clone().map(|preview| {
                    div().footnote().text_color(colors.text_secondary).truncate().child(preview).into_any_element()
                });
                (Some(self.picture(id, PICTURE, cx)), bold_at(name, matched, colors.text), preview)
            }
            Item::Message { found, name } => {
                let words: Vec<String> = self.query.split_whitespace().map(str::to_lowercase).collect();
                let under = div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child(self.picture(&found.chat, SMALL_PICTURE, cx))
                    .child(div().min_w_0().truncate().child(name.clone()))
                    .child(div().flex_none().child("·"))
                    .child(div().flex_none().child(crate::format::list_time(found.at, crate::format::now_ms())));
                (
                    None,
                    bold_at(&found.line, &places_of(&found.line, &words), colors.text),
                    Some(under.into_any_element()),
                )
            }
        };
        div()
            .id(("search-row", index))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .px(px(space::XS))
            .when(divided, |this| this.border_t_1().border_color(colors.border).mt(px(space::XS)).pt(px(space::XS)))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .gap(px(space::MD))
                    .px(px(space::SM))
                    .rounded(px(radius::SM))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(colors.selected_fill))
                    .when(!selected, |this| this.hover(|style| style.bg(colors.selected_fill.opacity(0.5))))
                    .children(picture)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(div().callout().text_color(colors.text).truncate().child(main))
                            .children(under),
                    )
                    .when(selected, |this| {
                        this.child(div().flex_none().callout().text_color(colors.text_secondary).child("↵"))
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(index, cx)))
            .into_any_element()
    }

    /// A chat's picture: the agent's, or the group's.
    fn picture(&self, chat: &str, size: f32, cx: &App) -> AnyElement {
        let Some(agents) = self.agents.upgrade() else { return div().size(px(size)).into_any_element() };
        let agents = agents.read(cx);
        let (rows, group) = agents.search_sources();
        match chat {
            GROUP => match group {
                Some(group) => group.tile(size, cx).into_any_element(),
                None => crate::screens::agents::round_picture(crate::screens::agents::group_picture(), size, cx)
                    .into_any_element(),
            },
            id => match rows.iter().find(|row| row.id == id) {
                Some(row) => row.tile(size, cx).into_any_element(),
                None => div().size(px(size)).flex_none().into_any_element(),
            },
        }
    }
}

/// How well `query`'s letters (spaces aside) are found in order in `name`, and where: None when they aren't all there.
/// Letters together and at the start of a word count most, so "re" finds "Researcher" before "Grace".
fn name_match(name: &str, query: &str) -> Option<(i64, Vec<usize>)> {
    let wanted: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    let chars: Vec<char> = name.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let word_start = |at: usize| at == 0 || !chars[at - 1].is_alphanumeric();
    // The whole of it together: at the start of the name, of a word, or anywhere.
    if let Some(at) = lower.windows(wanted.len().max(1)).position(|window| window == wanted.as_slice()) {
        let at = (0..lower.len())
            .filter(|&start| lower[start..].starts_with(&wanted))
            .find(|&start| word_start(start))
            .unwrap_or(at);
        let score = 1000 - at as i64
            + if at == 0 {
                300
            } else if word_start(at) {
                200
            } else {
                0
            };
        return Some((score, (at..at + wanted.len()).collect()));
    }
    // Else each letter in order, the next one found as early as it can be.
    let mut matched = Vec::new();
    let mut from = 0;
    for letter in &wanted {
        let at = (from..lower.len()).find(|&at| lower[at] == *letter)?;
        matched.push(at);
        from = at + 1;
    }
    let spread = matched.last().copied().unwrap_or(0) as i64 - matched.first().copied().unwrap_or(0) as i64;
    let starts = matched.iter().filter(|&&at| word_start(at)).count() as i64;
    Some((500 - spread + 20 * starts, matched))
}

/// `text` with the characters at `matched` (character places) in bold, in `color`.
fn bold_at(text: &str, matched: &[usize], color: gpui::Hsla) -> StyledText {
    let style = HighlightStyle { font_weight: Some(FontWeight::BOLD), color: Some(color), ..Default::default() };
    let highlights = text
        .char_indices()
        .enumerate()
        .filter(|(index, _)| matched.contains(index))
        .map(|(_, (at, c))| (at..at + c.len_utf8(), style))
        .collect::<Vec<_>>();
    StyledText::new(text.to_string()).with_highlights(highlights)
}

/// The character places of `text` where one of `words` (lowercased) appears.
fn places_of(text: &str, words: &[String]) -> Vec<usize> {
    let lower: Vec<char> = text.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
    let mut bold = vec![false; lower.len()];
    for word in words {
        let word: Vec<char> = word.chars().collect();
        if word.is_empty() || word.len() > lower.len() {
            continue;
        }
        for start in 0..=lower.len() - word.len() {
            if lower[start..start + word.len()] == word[..] {
                bold[start..start + word.len()].iter_mut().for_each(|on| *on = true);
            }
        }
    }
    (0..lower.len()).filter(|&index| bold[index]).collect()
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        // Esc (and another menu opening, or the window going to the back) closes it.
        crate::escape::popup(window, cx, |_, _, cx| cx.emit(SearchEvent::Close));
        let items = self.items(cx);
        if self.selected >= items.len() {
            self.selected = items.len().saturating_sub(1);
        }
        let viewport = window.viewport_size();
        let (window_width, window_height) = (f32::from(viewport.width), f32::from(viewport.height));
        let top = (window_height * 0.16).max(space::HUGE);
        let width = WIDTH.min(window_width - 2. * space::LG);
        let list_max = LIST_MAX.min(window_height - top - SEARCH_HEIGHT - space::HUGE).max(ROW_HEIGHT);
        let divider = items.iter().position(|item| matches!(item, Item::Message { .. })).filter(|first| *first > 0);
        let rows: Vec<AnyElement> = items
            .iter()
            .enumerate()
            .map(|(index, item)| self.render_row(index, item, divider == Some(index), cx))
            .collect();
        let search = div()
            .h(px(SEARCH_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .px(px(space::MD))
            .child(ui::icon_sized("search", 16., colors.text_secondary))
            .child(div().flex_1().min_w_0().body().child(Input::new(&self.input).appearance(false).px_0()))
            .child(
                div()
                    .flex_none()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(radius::XS))
                    .border_1()
                    .border_color(colors.border)
                    .caption()
                    .text_color(colors.text_secondary)
                    .child("esc"),
            );
        let has_rows = !rows.is_empty();
        let panel = div()
            .id("search-panel")
            .occlude()
            .w(px(width))
            .flex()
            .flex_col()
            .bg(colors.raised)
            .border_1()
            .border_color(colors.border)
            .rounded(px(radius::MD))
            .shadow_lg()
            .overflow_hidden()
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_by(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_by(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Enter, _, cx| {
                this.open(this.selected, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|_, _: &Escape, _, cx| {
                cx.emit(SearchEvent::Close);
                cx.stop_propagation();
            }))
            .child(search)
            .when(has_rows, |this| {
                this.child(
                    div()
                        .id("search-rows")
                        .max_h(px(list_max))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .border_t_1()
                        .border_color(colors.border)
                        .py(px(space::XS))
                        .flex()
                        .flex_col()
                        .children(rows),
                )
            });
        div()
            .absolute()
            .inset_0()
            // Over a web view (an artifact's page, the note editor) too: it gets a hole here (gpui-component's one_menu.rs).
            .child(gpui_component::one_menu::surface())
            .child(
                div()
                    .id("search-backdrop")
                    .absolute()
                    .inset_0()
                    .bg(colors.scrim)
                    .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Close))),
            )
            .child(
                div()
                    .absolute()
                    .top(px(top))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(crate::smoke::tracked("search-panel", panel)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_their_letters_in_order_best_first() {
        assert_eq!(name_match("Researcher", "re").map(|(_, at)| at), Some(vec![0, 1]));
        assert_eq!(name_match("Grace", "re").map(|(_, at)| at), Some(vec![1, 4]));
        assert!(name_match("Researcher", "re").unwrap().0 > name_match("Grace", "re").unwrap().0);
        assert_eq!(name_match("Grace", "ge").map(|(_, at)| at), Some(vec![0, 4]));
        assert_eq!(name_match("Coding Coach", "cc").map(|(_, at)| at), Some(vec![0, 7]));
        assert_eq!(name_match("Coach", "xyz"), None);
        // Together and at a word's start beats together inside one, which beats apart.
        let score = |name: &str, query: &str| name_match(name, query).unwrap().0;
        assert!(score("Tutor", "tu") > score("Mr Tutor", "tu"));
        assert!(score("Mr Tutor", "tu") > score("Statue", "tu"));
        assert!(score("Statue", "tu") > score("Trust", "tu"));
        // The second "tu" of "Status Tutor" starts a word: it's the one bold.
        assert_eq!(name_match("Status Tutor", "tu").map(|(_, at)| at), Some(vec![7, 8]));
    }

    #[test]
    fn words_are_bold_wherever_they_appear() {
        assert_eq!(places_of("Ris, more ris", &["ris".into()]), vec![0, 1, 2, 10, 11, 12]);
        assert_eq!(places_of("Ünï zebra", &["ünï".into(), "zeb".into()]), vec![0, 1, 2, 4, 5, 6]);
        assert!(places_of("nothing", &["zebra".into()]).is_empty());
    }
}

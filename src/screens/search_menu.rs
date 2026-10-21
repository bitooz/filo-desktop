// A menu that is searched: a field above a list that scrolls, in a popover by the button that opens it. For lists too
// long for a plain menu, as the models are once OpenCode's hundreds are among them: the Coding page's model pickers, an
// agent's page and the chat composer. Typing narrows the list to what holds every word typed; Up and Down move a
// highlight over it (the pointer moves it too) while the field keeps the keyboard, and Return picks the highlighted line.

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Corner, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Enter, Input, InputState, MoveDown, MoveUp};
use gpui_component::popover::Popover;
use gpui_component::{Icon, IconName, Selectable, Sizable as _, StyledExt as _};

use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui;

/// A line's height, and the most lines shown before the list scrolls.
const ROW_HEIGHT: f32 = 26.;
const ROWS_SHOWN: f32 = 12.;
/// Room kept above and below the panel, and for its search field, when the window is short.
const WINDOW_MARGIN: f32 = 16.;
const SEARCH_HEIGHT: f32 = 40.;

/// One line of the menu.
#[derive(Clone)]
pub struct Choice {
    pub label: SharedString,
    /// An icon before the name (a provider's logo).
    pub icon: Option<&'static str>,
    /// A quiet word after the name ("Default").
    pub note: Option<SharedString>,
    pub checked: bool,
    /// Lines of another group than the line above get a rule over them, while nothing is searched.
    pub group: usize,
    /// What a search looks in besides the name (an id, a tool's name).
    pub also: String,
    /// It can't be picked now (a model whose sign-in doesn't work): greyed out, its note saying why, and a click or
    /// Return on it does nothing.
    pub off: bool,
}

impl Choice {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Choice {
            label: label.into(),
            icon: None,
            note: None,
            checked: false,
            group: 0,
            also: String::new(),
            off: false,
        }
    }

    fn holds(&self, words: &[String]) -> bool {
        let text = format!("{} {}", self.label, self.also).to_lowercase();
        words.iter().all(|word| text.contains(word.as_str()))
    }
}

/// The indexes of the choices that hold every word of `query`, in their order.
pub fn matching(choices: &[Choice], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    choices.iter().enumerate().filter(|(_, choice)| choice.holds(&words)).map(|(index, _)| index).collect()
}

/// What the menu keeps while it's open: its search field, which has the keyboard once it opens, and the highlighted
/// line. It goes when the menu closes, so each opening starts with an empty search.
struct Search {
    input: Entity<InputState>,
    focused: bool,
    /// The highlighted line, of those shown, and the search it was set for: a new search puts it back on the first
    /// match, or on the ticked line when the search is emptied (as when the menu opens).
    highlight: usize,
    searched: Option<String>,
    scroll: UniformListScrollHandle,
}

impl Search {
    /// The highlight `step` lines on, stopping at the list's ends, scrolled into view.
    fn move_by(&mut self, step: isize, count: usize) {
        if count == 0 {
            return;
        }
        self.highlight = (self.highlight as isize + step).clamp(0, count as isize - 1) as usize;
        let strategy = if step < 0 { ScrollStrategy::Top } else { ScrollStrategy::Bottom };
        self.scroll.scroll_to_item(self.highlight, strategy);
    }
}

/// `trigger` opening the menu of `choices` (made only while it's open), anchored at `anchor`, `width` points wide;
/// `pick` gets the index of the choice picked, and the menu closes.
pub fn search_menu<T: Selectable + IntoElement + 'static>(
    id: impl Into<SharedString>,
    trigger: T,
    anchor: Corner,
    width: f32,
    placeholder: &'static str,
    choices: impl Fn() -> Rc<Vec<Choice>> + 'static,
    pick: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Popover {
    let id: SharedString = id.into();
    let pick = Rc::new(pick);
    Popover::new(ElementId::from(id.clone())).appearance(false).anchor(anchor).trigger(trigger).content(
        move |_, window, cx| {
            let popover = cx.entity();
            let search = window.use_keyed_state(SharedString::from(format!("{id}-search")), cx, |window, cx| Search {
                input: cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)),
                focused: false,
                highlight: 0,
                searched: None,
                scroll: UniformListScrollHandle::new(),
            });
            let input = search.read(cx).input.clone();
            if !search.read(cx).focused {
                search.update(cx, |search, _| search.focused = true);
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            let query = input.read(cx).value().to_string();
            let choices = choices();
            let found = Rc::new(matching(&choices, &query));
            let searching = !query.trim().is_empty();
            let (highlight, scroll) = search.update(cx, |search, _| {
                if search.searched.as_deref() != Some(query.as_str()) {
                    let ticked = (!searching).then(|| found.iter().position(|&index| choices[index].checked)).flatten();
                    search.highlight = ticked.unwrap_or(0);
                    search.scroll.scroll_to_item(search.highlight, ScrollStrategy::Center);
                    search.searched = Some(query.clone());
                }
                search.highlight = search.highlight.min(found.len().saturating_sub(1));
                (search.highlight, search.scroll.clone())
            });
            let theme = gpui_component::ActiveTheme::theme(&**cx);
            let (hover_bg, hover_fg, border, radius, text) = (
                theme.accent,
                theme.accent_foreground,
                theme.border,
                theme.radius.min(px(8.)),
                theme.popover_foreground,
            );
            let colors = *cx.colors();
            // Picks the choice, then closes the menu; one that's off stays as it is.
            let choose = {
                let (pick, popover, choices) = (pick.clone(), popover.clone(), choices.clone());
                Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
                    if choices[index].off {
                        return;
                    }
                    pick(index, window, cx);
                    popover.update(cx, |popover, cx| popover.dismiss(window, cx));
                })
            };
            let list = if found.is_empty() {
                div()
                    .px(px(space::SM))
                    .py(px(space::SM))
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child("Nothing matches.")
                    .into_any_element()
            } else {
                let room = f32::from(window.viewport_size().height) - 2. * WINDOW_MARGIN - SEARCH_HEIGHT;
                let height =
                    (found.len() as f32 * ROW_HEIGHT).min(ROWS_SHOWN * ROW_HEIGHT).min(room.max(4. * ROW_HEIGHT));
                let (choices, found, choose, row_id, state) =
                    (choices.clone(), found.clone(), choose.clone(), id.clone(), search.clone());
                let list =
                    gpui::uniform_list(SharedString::from(format!("{id}-list")), found.len(), move |range, _, cx| {
                        let colors = *cx.colors();
                        range
                            .map(|row| {
                                let index = found[row];
                                let choice = &choices[index];
                                let ruled = !searching && row > 0 && choices[found[row - 1]].group != choice.group;
                                let (choose, state) = (choose.clone(), state.clone());
                                let line =
                                    div()
                                        .id(row)
                                        .h(px(ROW_HEIGHT))
                                        .px(px(space::SM))
                                        .flex()
                                        .items_center()
                                        .gap(px(space::SM))
                                        .rounded(radius)
                                        .text_sm()
                                        .when(ruled, |this| this.border_t_1().border_color(border).rounded_none())
                                        .when(row == highlight, |this| this.bg(hover_bg).text_color(hover_fg))
                                        .when(choice.off, |this| this.opacity(0.5))
                                        .child(div().flex_none().w(px(12.)).when(choice.checked, |this| {
                                            this.child(Icon::new(IconName::Check).xsmall())
                                        }))
                                        .children(
                                            choice.icon.map(|icon| {
                                                ui::icon_sized(icon, 13., colors.text_secondary).flex_none()
                                            }),
                                        )
                                        .child(div().min_w_0().truncate().child(choice.label.clone()))
                                        .children(choice.note.clone().map(|note| {
                                            div().flex_none().caption().text_color(colors.text_secondary).child(note)
                                        }))
                                        // The pointer moves the highlight, so it and the keys show one line.
                                        .on_hover(move |hovered, _, cx| {
                                            if *hovered {
                                                state.update(cx, |search, cx| {
                                                    if search.highlight != row {
                                                        search.highlight = row;
                                                        cx.notify();
                                                    }
                                                });
                                            }
                                        })
                                        .on_click(move |_, window, cx| choose(index, window, cx));
                                crate::smoke::tracked(format!("{row_id}-row-{row}"), line)
                            })
                            .collect()
                    })
                    .track_scroll(scroll)
                    .h(px(height));
                crate::smoke::tracked(format!("{id}-list"), list)
            };
            let count = found.len();
            let (up, down) = (search.clone(), search.clone());
            div()
                .w(px(width))
                .p(px(space::XS))
                .flex()
                .flex_col()
                .gap(px(space::XS))
                .popover_style(cx)
                .text_color(text)
                // The scroll wheel over it scrolls its list, not closing it (gpui-component's one_menu.rs).
                .relative()
                .child(gpui_component::one_menu::surface())
                // Up and Down in the search field move the highlight rather than its caret; Return picks it.
                .capture_action(move |_: &MoveUp, _, cx| {
                    up.update(cx, |search, cx| {
                        search.move_by(-1, count);
                        cx.notify();
                    });
                    cx.stop_propagation();
                })
                .capture_action(move |_: &MoveDown, _, cx| {
                    down.update(cx, |search, cx| {
                        search.move_by(1, count);
                        cx.notify();
                    });
                    cx.stop_propagation();
                })
                .on_action(move |_: &Enter, window, cx| {
                    if let Some(&index) = found.get(search.read(cx).highlight) {
                        choose(index, window, cx);
                    }
                })
                .child(Input::new(&input).small().cleanable(true).prefix(ui::icon_sized(
                    "search",
                    14.,
                    colors.text_secondary,
                )))
                .child(list)
        },
    )
}

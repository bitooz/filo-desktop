// A menu that is searched: a field above a list that scrolls, in a popover by the button that opens it. For lists too
// long for a plain menu, as the models are once OpenCode's hundreds are among them: the Coding page's model pickers, an
// agent's page and the chat composer. Typing narrows the list to what holds every word typed; Return picks the first.

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Corner, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Enter, Input, InputState};
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
}

impl Choice {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Choice { label: label.into(), icon: None, note: None, checked: false, group: 0, also: String::new() }
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

/// What the menu keeps while it's open: its search field, which has the keyboard once it opens. It goes when the menu
/// closes, so each opening starts with an empty search.
struct Search {
    input: Entity<InputState>,
    focused: bool,
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
            let theme = gpui_component::ActiveTheme::theme(&**cx);
            let (hover_bg, hover_fg, border, radius, text) = (
                theme.accent,
                theme.accent_foreground,
                theme.border,
                theme.radius.min(px(8.)),
                theme.popover_foreground,
            );
            let colors = *cx.colors();
            // Picks the choice, then closes the menu.
            let choose = {
                let (pick, popover) = (pick.clone(), popover.clone());
                Rc::new(move |index: usize, window: &mut Window, cx: &mut App| {
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
                let (choices, found, choose, row_id) = (choices.clone(), found.clone(), choose.clone(), id.clone());
                let list =
                    gpui::uniform_list(SharedString::from(format!("{id}-list")), found.len(), move |range, _, cx| {
                        let colors = *cx.colors();
                        range
                            .map(|row| {
                                let index = found[row];
                                let choice = &choices[index];
                                let ruled = !searching && row > 0 && choices[found[row - 1]].group != choice.group;
                                let choose = choose.clone();
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
                                        .hover(|this| this.bg(hover_bg).text_color(hover_fg))
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
                                        .on_click(move |_, window, cx| choose(index, window, cx));
                                crate::smoke::tracked(format!("{row_id}-row-{row}"), line)
                            })
                            .collect()
                    })
                    .h(px(height));
                crate::smoke::tracked(format!("{id}-list"), list)
            };
            let first = found.first().copied();
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
                // Return in the search field picks the first that matches.
                .on_action(move |_: &Enter, window, cx| {
                    if let Some(index) = first {
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

// A reply's link to the message it answers, when someone else (the user, or in the group another member) wrote after
// it, the same in agents' chats and the group: a thin rounded elbow in the border colour rising from the reply to a
// one-line quote of that message, in grey, with its author's name first when that isn't obvious. On the left for an agent's reply, mirrored on the right for the
// user's. A click jumps to the message (conversation.rs `jump_to`).

use gpui::{
    App, Div, InteractiveElement as _, ParentElement as _, SharedString, Stateful, Styled as _, div,
    prelude::FluentBuilder as _, px, relative,
};

use crate::theme::{ActiveTheme as _, Typography as _, space};

/// What a reply's link shows: the message it answers, its author's name when it isn't obvious, and its words.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReplyLink {
    pub target: String,
    pub author: Option<String>,
    pub quote: String,
}

/// The elbow: as wide and as tall as it is, its line, and how far in from the bubble's side it rises.
const ELBOW: (f32, f32) = (16., 9.);
const LINE: f32 = 1.5;
const INSET: f32 = 12.;

/// The link over a reply's bubble, on the user's side (`mine`) or the agent's. Its bottom meets the bubble's top.
pub(super) fn render(id: impl Into<SharedString>, link: &ReplyLink, mine: bool, cx: &App) -> Stateful<Div> {
    let colors = cx.colors();
    let (width, height) = ELBOW;
    let elbow =
        div().flex_none().w(px(width)).h(px(height)).border_t(px(LINE)).border_color(colors.border).map(|elbow| {
            if mine {
                elbow.border_r(px(LINE)).rounded_tr(px(height - LINE))
            } else {
                elbow.border_l(px(LINE)).rounded_tl(px(height - LINE))
            }
        });
    let words = div()
        .min_w_0()
        .flex()
        .gap(px(space::XS))
        .children(link.author.clone().map(|author| div().flex_none().semibold().child(author)))
        // Clamped rather than kept on one line (`truncate`): gpui then measures it again at the width it gets, so the
        // ellipsis lands where the quote is cut.
        .child(div().min_w_0().line_clamp(1).text_ellipsis().child(link.quote.clone()));
    // The elbow's top runs into the middle of the quote's line.
    div()
        .id(id.into())
        .max_w(relative(0.86))
        .min_w_0()
        .flex()
        .items_end()
        .gap(px(space::XS))
        .footnote()
        .text_color(colors.text_tertiary)
        .hover(|style| style.text_color(colors.text_secondary))
        .cursor_pointer()
        .map(|link| if mine { link.flex_row_reverse().pr(px(INSET)) } else { link.pl(px(INSET)) })
        .child(elbow)
        .child(words)
}

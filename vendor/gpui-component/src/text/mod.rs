pub mod commits;
mod format;
mod inline;
mod node;
pub mod safety;
pub mod select;
mod style;
mod text_view;
mod utils;
pub mod wrap;

use gpui::App;
pub use style::*;
pub use text_view::*;

pub(crate) fn init(cx: &mut App) {
    text_view::init(cx);
}

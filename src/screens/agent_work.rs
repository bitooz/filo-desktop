// What the chat's rows and the pet's bubbles share: a coding round's state as a pill (the proof under a result), and
// text cut to a few lines.

use gpui::{AnyElement, App, Div, IntoElement, ParentElement as _, Styled as _, div, prelude::FluentBuilder as _, px};

use crate::theme::{Typography as _, radius};
use crate::ui::{self, Tone};

/// A task's state (screens/coding.rs `task_state`) as a tone, the same wherever it shows: the pills, and the dots and
/// words on the Coding page's rows. Stopped is neutral: the user ended it, nothing went wrong.
pub fn state_tone(state: &str) -> Tone {
    match state {
        "queued" | "active" | "monitoring" => Tone::Accent,
        "waiting" => Tone::Warning,
        "completed" => Tone::Success,
        "failed" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

/// The state as a pill: a turning ring while working, an eye while monitoring (a coding task waiting on its background
/// work), a check when done, the word otherwise.
pub fn state_pill(state: &str, label: impl Into<gpui::SharedString>, small: bool, cx: &App) -> Div {
    let (bg, fg) = state_tone(state).soft(cx);
    let leading = state_mark(state, if small { 11. } else { 13. }, fg);
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(if small { 19. } else { 22. }))
        .px(px(if small { 7. } else { 9. }))
        .rounded(px(radius::FULL))
        .bg(bg)
        .text_color(fg)
        .map(|this| if small { this.micro().semibold() } else { this.caption().semibold() })
        .children(leading)
        .child(label.into())
}

/// A state's mark before its words: a turning ring while it works or waits to start, an eye while it monitors
/// background work, a check once done; none otherwise.
pub fn state_mark(state: &str, size: f32, color: gpui::Hsla) -> Option<AnyElement> {
    match state {
        "queued" | "active" => Some(ui::working_ring(size, color).into_any_element()),
        "monitoring" => Some(ui::icon_sized("eye", size, color).into_any_element()),
        "completed" => Some(ui::icon_sized("check", size, color).into_any_element()),
        _ => None,
    }
}

/// At most `lines` lines of text whose line height is `line_height` (unscaled). GPUI's
/// `line_clamp` measures clamped text wider than its box and cuts it at the right edge,
/// so this caps the height instead.
pub fn max_lines(element: Div, lines: f32, line_height: f32) -> Div {
    let line = (line_height * crate::theme::text_scale()).round();
    element.max_h(px(lines * line)).overflow_hidden()
}

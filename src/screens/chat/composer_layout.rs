// The composer's text row. While the message fits on one line it's one slim row: +, the text, then the pickers and
// Send. Once it doesn't (it wraps, or has a new line) the text takes the whole width and + and the pickers move to a
// row of their own under it, and back up when it fits again. Which one shows comes from the text measured against the
// one-row width, whichever layout is showing, so it can't flip back and forth at the wrap point (the stacked text is
// wider, and would fit again). The row's height and the text's place glide to where they're going (`GLIDE`, ease-out).
// The text grows to about 40% of the window, 12 lines at most, then scrolls inside (the input keeps the caret in
// sight, at the bottom when typing or putting a quote in at the end) with the buttons where they are, a fade at the top
// while lines are out of sight above, and a thin scrollbar only while it scrolls. While it scrolls, the wheel over it
// moves only its text, never the chat behind, at its top and bottom ends too (gpui-component `on_scroll_wheel`).

use std::cell::Cell;
use std::time::Instant;

use gpui::{LineFragment, linear_color_stop, linear_gradient, rems};

use super::*;

/// Room around the row's text and buttons, and between them.
const PAD: f32 = space::XS;
const GAP: f32 = space::XS;
/// The buttons' height (and the + button's width): one line of text sits on their centre line.
const BUTTON: f32 = 28.;
/// The row with one line of text.
const ONE_ROW: f32 = PAD + BUTTON + PAD;
/// The input's own room around its text.
pub(super) const INPUT_PX: f32 = space::XS;
pub(super) const INPUT_PY: f32 = 4.;
/// Room the input keeps at the right of a line, past which it wraps: the caret's, so a line runs nearly to the edge.
pub(super) const WRAP_MARGIN: f32 = 2.;
/// How long the row takes to get to a new height or layout.
const GLIDE: Duration = Duration::from_millis(180);
/// The most lines the text shows before it scrolls, and the share of the window the composer may take.
const MAX_LINES: usize = 12;
const MIN_LINES: usize = 3;
const MAX_SHARE: f32 = 0.4;
/// Longer than this on one line can't fit the one-row width (the chat is at most 760 points wide): not measured.
const MAX_MEASURED: usize = 400;
/// The pickers and Send together, until they've been measured.
const CONTROLS_GUESS: f32 = 120.;

/// A value gliding to where it's going.
#[derive(Clone, Copy, Default)]
struct Glide {
    from: f32,
    to: f32,
    since: Option<Instant>,
}

impl Glide {
    fn at(&self, now: Instant) -> f32 {
        let Some(since) = self.since else { return self.to };
        let t = now.saturating_duration_since(since).as_secs_f32() / GLIDE.as_secs_f32();
        if t >= 1. {
            return self.to;
        }
        // Ease-out (cubic): quick to start, settling gently.
        self.from + (self.to - self.from) * (1. - (1. - t).powi(3))
    }

    fn moving(&self, now: Instant) -> bool {
        self.since.is_some_and(|since| now.saturating_duration_since(since) < GLIDE)
    }

    /// Heads for `to` from wherever it is now (at once with `jump`).
    fn head(&mut self, to: f32, now: Instant, jump: bool) {
        if jump {
            *self = Glide { from: to, to, since: None };
        } else if (to - self.to).abs() > 0.5 {
            *self = Glide { from: self.at(now), to, since: Some(now) };
        }
    }
}

#[derive(Default)]
pub(super) struct ComposerLayout {
    /// Measured as drawn: the row's width, and the pickers' and Send's together.
    row_width: Rc<Cell<f32>>,
    controls_width: Rc<Cell<f32>>,
    stacked: bool,
    /// The text last measured, the one-row room it was measured against, and whether it fit.
    measured: Option<(SharedString, f32, bool)>,
    /// The input's font, as last measured with.
    font: Option<gpui::Font>,
    height: Glide,
    /// How far the text is from its place, sideways: it slides as + moves out from beside it, or back.
    shift: Glide,
    /// Drawn at a known width once: before that, nothing glides (a draft shows as it is).
    settled: bool,
    /// The height the row was last drawn at.
    drawn: f32,
    /// Shrinking after a send: the feed makes its room for the one row at once (`feed_lag`).
    sending: bool,
}

impl ComposerLayout {
    /// Whether the text has the whole width, with + and the pickers under it.
    #[cfg(feature = "dev")]
    pub(super) fn stacked(&self) -> bool {
        self.stacked
    }

    /// A message is being sent: the row shrinks to one line, and the feed's room for it goes now, not as it glides,
    /// so the message lands where it stays. How much shorter the row will be.
    pub(super) fn sent(&mut self) -> f32 {
        self.sending = true;
        (self.drawn - ONE_ROW).max(0.)
    }

    /// How much taller the row is drawn than the feed should make room for (only while it shrinks after a send).
    pub(super) fn feed_lag(&self) -> f32 {
        if self.sending { (self.drawn - self.height.to).max(0.) } else { 0. }
    }

    /// The row's width as last drawn.
    #[cfg(feature = "dev")]
    pub(super) fn row_width(&self) -> f32 {
        self.row_width.get()
    }

    /// Where the input should wrap the text when stacked: the row's width, less the text box's inset at either side,
    /// the input's own padding and the caret's room.
    #[cfg(feature = "dev")]
    pub(super) fn stacked_wrap_width(&self) -> f32 {
        self.row_width.get() - (PAD + INPUT_PX) * 2. - INPUT_PX * 2. - WRAP_MARGIN
    }

    /// Whether `text` wraps at `width` in the input's font and size (as last measured with).
    #[cfg(feature = "dev")]
    pub(super) fn wraps(&self, text: &str, width: f32, window: &Window) -> bool {
        let Some(font) = self.font.clone() else { return false };
        let font_size = rems(0.875).to_pixels(window.rem_size());
        window
            .text_system()
            .line_wrapper(font, font_size)
            .wrap_line(&[LineFragment::text(text)], px(width))
            .next()
            .is_some()
    }

    /// How tall the row is drawn now, on its way or there.
    #[cfg(feature = "dev")]
    pub(super) fn height_now(&self) -> f32 {
        self.height.at(Instant::now())
    }
}

/// Whether `text` needs the stacked layout: it has a new line, or `wraps` at `room` (the text's width in the one row).
pub(super) fn needs_stacking(text: &str, room: f32, wraps: impl FnOnce(&str, f32) -> bool) -> bool {
    if text.is_empty() {
        return false;
    }
    if text.contains('\n') {
        return true;
    }
    if room <= 0. {
        return false;
    }
    text.chars().count() > MAX_MEASURED || wraps(text, room)
}

impl Conversation {
    /// The text row (see the top of this file): `plus` at its bottom left, `controls` (pickers and Send) at its bottom
    /// right, the text over them or between them.
    pub(super) fn render_text_row(
        &mut self,
        plus: AnyElement,
        controls: Vec<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let now = Instant::now();
        let rem = window.rem_size();
        // The input's own line height and text size (gpui-component `Input`: 1.25rem, `text_sm`).
        let line_height = f32::from(rems(1.25).to_pixels(rem));
        let window_height = f32::from(window.viewport_size().height);
        let text_room =
            window_height * MAX_SHARE - (PAD + GAP + BUTTON + PAD) - self.strips_height.get() - INPUT_PY * 2.;
        let max_lines = ((text_room / line_height).floor().max(0.) as usize).clamp(MIN_LINES, MAX_LINES);
        let (text, lines, scrolled) = self.input.update(cx, |input, cx| {
            input.set_max_rows(max_lines, cx);
            (input.value(), input.shown_rows().max(1), f32::from(input.scroll_offset().y) < -0.5)
        });

        let layout = &mut self.composer_layout;
        let row_width = layout.row_width.get();
        let controls_width = match layout.controls_width.get() {
            0. => CONTROLS_GUESS,
            width => width,
        };
        let (one_row_left, one_row_right) = (PAD + BUTTON + GAP, GAP + controls_width + PAD);
        // Where the input would wrap the text in the one row (`WRAP_MARGIN` short of its right edge), less a point so it
        // never wraps there first.
        let room = row_width - one_row_left - one_row_right - INPUT_PX * 2. - WRAP_MARGIN - 1.;
        let fits = match &layout.measured {
            Some((measured, at, fits)) if *measured == text && (at - room).abs() < 0.5 => *fits,
            _ => {
                // Wrapped the way the input wraps it (gpui-component `TextWrapper`), in its font and size (`text_sm`).
                let fits = !needs_stacking(&text, room, |text, room| {
                    let font_size = rems(0.875).to_pixels(rem);
                    let font = window.text_style().font();
                    layout.font = Some(font.clone());
                    let mut wrapper = window.text_system().line_wrapper(font, font_size);
                    wrapper.wrap_line(&[LineFragment::text(text)], px(room)).next().is_some()
                });
                layout.measured = Some((text.clone(), room, fits));
                fits
            }
        };
        let stacked = !fits;
        let jump = !layout.settled;
        layout.settled |= row_width > 0.;
        let left_of = |stacked: bool| if stacked { PAD + INPUT_PX } else { one_row_left };
        if stacked != layout.stacked {
            // From where the text is now, wherever its slide had got to.
            let from = left_of(layout.stacked) + layout.shift.at(now) - left_of(stacked);
            layout.shift = if jump { Glide::default() } else { Glide { from, to: 0., since: Some(now) } };
            layout.stacked = stacked;
        }
        let text_height = lines as f32 * line_height + INPUT_PY * 2.;
        let height = if stacked { PAD + text_height + GAP + BUTTON + PAD } else { PAD + text_height.max(BUTTON) + PAD };
        layout.height.head(height, now, jump);
        if layout.height.moving(now) || layout.shift.moving(now) {
            window.request_animation_frame();
        }
        let (height, shift) = (layout.height.at(now), layout.shift.at(now));
        layout.drawn = height;
        layout.sending &= layout.height.moving(now);
        let (left, right) = if stacked { (PAD + INPUT_PX, PAD + INPUT_PX) } else { (one_row_left, one_row_right) };

        let view = cx.entity().downgrade();
        let measure = |cell: Rc<Cell<f32>>| {
            let view = view.clone();
            gpui::canvas(
                move |bounds, _, cx| {
                    let width = f32::from(bounds.size.width);
                    if (width - cell.get()).abs() > 0.5 {
                        cell.set(width);
                        view.update(cx, |_, cx| cx.notify()).ok();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        div()
            .relative()
            .flex_none()
            .h(px(height))
            .overflow_hidden()
            .child(self.measure_row(cx))
            .child(measure(self.composer_layout.row_width.clone()))
            // Same width all the way as it slides, so it doesn't wrap again on the way.
            .child(
                div()
                    .absolute()
                    .top(px(PAD))
                    .left(px(left + shift))
                    .right(px(right - shift))
                    .h(px(text_height))
                    .callout()
                    .child(crate::smoke::tracked_fill(
                        "composer-input",
                        Input::new(&self.input).appearance(false).px(px(INPUT_PX)).py(px(INPUT_PY)),
                    ))
                    // Lines out of sight above fade out under the row's top edge.
                    .when(scrolled, |this| {
                        this.child(div().absolute().top_0().left_0().right_0().h(px(line_height + INPUT_PY)).bg(
                            linear_gradient(
                                180.,
                                linear_color_stop(colors.surface, 0.),
                                linear_color_stop(colors.surface.opacity(0.), 1.),
                            ),
                        ))
                    }),
            )
            .child(div().absolute().left(px(PAD)).bottom(px(PAD)).child(plus))
            .child(
                div()
                    .absolute()
                    .right(px(PAD))
                    .bottom(px(PAD))
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .children(controls)
                    .child(measure(self.composer_layout.controls_width.clone())),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_on_a_new_line_or_past_the_one_row_width() {
        let wraps = |text: &str, room: f32| text.chars().count() as f32 * 8. > room;
        assert!(!needs_stacking("", 100., wraps));
        assert!(!needs_stacking("short", 100., wraps));
        assert!(needs_stacking("two\nlines", 100., wraps));
        assert!(needs_stacking("this one is too long", 100., wraps));
        // Unmeasured yet: one row, unless it has lines of its own.
        assert!(!needs_stacking("this one is too long", 0., wraps));
        assert!(needs_stacking(&"x".repeat(MAX_MEASURED + 1), 10_000., wraps));
    }

    #[test]
    fn the_same_text_and_width_always_get_the_same_layout() {
        // Decided at the one-row width whichever layout shows, so the answer can't depend on the last one.
        let wraps = |text: &str, room: f32| text.chars().count() as f32 * 8. > room;
        assert!(!needs_stacking(&"x".repeat(12), 100., wraps));
        assert!(needs_stacking(&"x".repeat(13), 100., wraps));
    }

    #[test]
    fn glides_ease_out_to_the_end() {
        let start = Instant::now();
        let mut glide = Glide::default();
        glide.head(36., start, true);
        assert_eq!(glide.at(start), 36.);
        assert!(!glide.moving(start));
        glide.head(66., start, false);
        assert_eq!(glide.at(start), 36.);
        let half = glide.at(start + GLIDE / 2);
        // Past halfway by half the time: ease-out.
        assert!(half > 51. && half < 66., "{half}");
        assert_eq!(glide.at(start + GLIDE), 66.);
        assert!(!glide.moving(start + GLIDE));
        // Heading somewhere else on the way starts from where it got to.
        glide.head(36., start + GLIDE / 2, false);
        assert_eq!(glide.at(start + GLIDE / 2), half);
    }
}

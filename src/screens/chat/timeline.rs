// The conversation's timeline: a column of short dashes at the chat's right edge, one per message the user sent (one
// per few in a long chat), evenly spaced, with no line (design/filo-desktop-chat-timeline.html, option B). Where the
// reader is shows as a fractional place among those messages: the dash there is long and bright and its neighbours
// taper back to the resting dash, so the bright spot glides as the chat scrolls. Hovering a dash shows when that
// message was sent and how it starts; pressing and dragging along the column slides through the chat, with the same
// card riding the bright dash; a click without a drag scrolls to that message; Esc while dragging puts the chat back.
// The grab strip is wider than the dashes, and rows leave room for it (`gutter`), so it never covers a bubble.
// Only while the chat scrolls and the user has sent two messages or more.
//
// GPUI's list only knows the heights of rows it has laid out, and none while it follows the bottom, so the place is
// read in rows, not pixels: the fractional row at the top of the view, between the sent messages around it, pulled
// toward the last one over the final screen so the newest message can be current.
//
// Dragging up near the oldest messages loaded loads the page before them, which the list takes in above the message
// on screen without moving it (feed_rows::sync_list_anchored). The dashes then stand for more messages, so the one
// under the pointer is an older one: the drag goes on from where the chat is (`Scrub::from_here`, `from_here`), the
// strip above the pointer now holding the older messages and below it the newer ones, rather than to the dash's new
// message, which was a jump into the page just loaded and on to load the next.

use gpui::{
    Bounds, DispatchPhase, Hitbox, HitboxBehavior, Hsla, ListOffset, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, canvas,
};

use super::*;

/// The grab strip, flush with the chat's right edge.
const RAIL_WIDTH: f32 = 36.;
/// From the chat's edge to the dashes' right ends.
const INSET: f32 = 8.;
/// Between dashes, closer (down to `MIN_PITCH`) when they wouldn't fit; past that, a dash stands for several messages.
const PITCH: f32 = 10.;
const MIN_PITCH: f32 = 5.;
/// The strip reaches this far past the first and the last dash.
const PAD: f32 = 14.;
const DASH: f32 = 8.;
const DASH_CURRENT: f32 = 20.;
const DASH_HOVERED: f32 = 16.;
/// How many dashes either side of the place still lengthen.
const TAPER: f32 = 2.5;
/// A press that moves less than this is a click.
const CLICK_SLOP: f32 = 3.;
/// Dragged within this many sent messages of the oldest loaded, the page before them is asked for.
const LOAD_AHEAD: f32 = 6.;
/// Bubbles keep this far from the chat's right edge while the strip can show: the strip, and a gap.
pub(super) const CLEAR: f32 = RAIL_WIDTH + space::SM;

/// A press on the strip, until it comes up.
pub(super) struct Scrub {
    /// Where the chat was, for Esc: the list's top (and the row there, as rows come in above it) and the follow flags.
    from: ListOffset,
    from_row: Option<String>,
    from_flags: (bool, bool, bool),
    start_y: Pixels,
    /// Past the click slop: the chat follows the pointer.
    moved: bool,
    /// Where the pointer has put the chat, in sent messages; shown instead of the place read off the list.
    pos: Option<f32>,
    /// The pointer's last height in the window while pressed.
    last_y: Option<Pixels>,
    /// The rows the sent messages were counted in (how many, and the first), to tell when a page came in.
    rows_seen: (usize, Option<String>),
    /// Since a page came in under the drag: the dash and the place it started again from (`from_here`).
    from_here: Option<(f32, f32)>,
    /// A page came in since the last move: the next one starts from where the chat is.
    rows_changed: bool,
    /// Esc, heard before anything focused does.
    _escape: Subscription,
}

#[derive(Default)]
pub(super) struct Timeline {
    /// The dash under the pointer.
    hover: Option<usize>,
    scrub: Option<Scrub>,
    /// What the last frame drew, for the scroll fixture: the strip, its dashes and the card's message.
    #[cfg(feature = "dev")]
    drawn: Option<Drawn>,
}

#[cfg(feature = "dev")]
#[derive(Clone, Debug)]
pub(super) struct Drawn {
    pub strip: Bounds<Pixels>,
    pub dashes: usize,
    pub pitch: f32,
    /// The bright place, in dashes.
    pub place: f32,
    pub hover: Option<usize>,
    pub pressed: bool,
    pub card: Option<String>,
}

#[cfg(feature = "dev")]
impl Timeline {
    pub(super) fn drawn(&self) -> Option<Drawn> {
        self.drawn.clone()
    }

    /// The y of dash `dash`'s middle in the window, as last drawn.
    pub(super) fn dash_y(drawn: &Drawn, dash: f32) -> Pixels {
        drawn.strip.top() + px(PAD + dash * drawn.pitch)
    }
}

/// How the dashes sit in a strip of a given height.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// Sent messages per dash.
    per: usize,
    dashes: usize,
    pitch: f32,
}

impl Layout {
    fn new(messages: usize, height: f32) -> Layout {
        let room = (height - 2. * PAD).max(MIN_PITCH);
        let fits = (room / MIN_PITCH).floor() as usize + 1;
        let per = messages.div_ceil(fits.max(2)).max(1);
        let dashes = messages.div_ceil(per).max(1);
        let pitch = (room / (dashes.saturating_sub(1).max(1)) as f32).clamp(MIN_PITCH, PITCH);
        Layout { per, dashes, pitch }
    }

    /// The column's height with the strip's ends.
    fn height(&self) -> f32 {
        self.pitch * self.dashes.saturating_sub(1) as f32 + 2. * PAD
    }

    /// The dash at `y` down the strip, fractional, not clamped.
    fn dash_at(&self, y: f32) -> f32 {
        (y - PAD) / self.pitch
    }
}

/// The place among the sent messages (`sent`: their rows) for a view whose top is at fractional row `row`, and
/// `toward_end` (0 to 1) through the chat's final screen.
fn place(sent: &[usize], row: f32, toward_end: f32) -> f32 {
    let Some(last) = sent.len().checked_sub(1) else { return 0. };
    let at_top = match sent.iter().rposition(|&index| index as f32 <= row) {
        None => 0.,
        Some(k) if k == last => last as f32,
        Some(k) => k as f32 + (row - sent[k] as f32) / (sent[k + 1] - sent[k]) as f32,
    };
    at_top + (last as f32 - at_top) * toward_end.clamp(0., 1.)
}

/// The fractional row a view's top goes to for place `pos` (the inverse of `place` before the final screen).
fn row_for(sent: &[usize], pos: f32) -> f32 {
    let Some(last) = sent.len().checked_sub(1) else { return 0. };
    let pos = pos.clamp(0., last as f32);
    let k = (pos.floor() as usize).min(last);
    if k == last {
        return sent[last] as f32;
    }
    sent[k] as f32 + (pos - k as f32) * (sent[k + 1] - sent[k]) as f32
}

/// The place for dash `dash` in a drag that a page came in under, the pointer then at dash `from` and the chat at place
/// `place` (both since the page): up the strip to the oldest at the first dash, down it to the newest (`newest`) at
/// the last (`last`). Past the first dash (the pointer was over or above it), the older messages go on up from there
/// at `per` sent messages a dash.
fn from_here(dash: f32, (from, place): (f32, f32), last: f32, newest: f32, per: f32) -> f32 {
    let pos = if dash >= from {
        if last > from { place + (newest - place) * ((dash - from) / (last - from)).min(1.) } else { place }
    } else if from > 0. {
        place * (dash.max(0.) / from)
    } else {
        place + (dash - from) * per
    };
    pos.clamp(0., newest)
}

/// How far a dash `distance` from the place lengthens and brightens: 1 on it, 0 from `TAPER` away.
fn nearness(distance: f32) -> f32 {
    (1. - distance.abs() / TAPER).max(0.)
}

fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    Hsla {
        h: from.h + (to.h - from.h) * t,
        s: from.s + (to.s - from.s) * t,
        l: from.l + (to.l - from.l) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

/// Room the rows leave at the right, beyond their own padding, so bubbles stay `CLEAR` from the edge in a chat
/// `width` wide (none once the centred column leaves that much anyway).
pub(super) fn gutter(width: f32) -> f32 {
    let needed = CLEAR - crate::theme::PAGE_PADDING;
    (crate::theme::MAX_CONTENT_WIDTH + 2. * needed - width).clamp(0., needed)
}

impl Conversation {
    /// Rows holding a message the user sent.
    fn sent_rows(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, ViewRow::Message { message, .. } if message.is_user()))
            .map(|(index, _)| index)
            .collect()
    }

    /// How many sent messages there are and the first one's row: what a drag's dashes were counted from.
    fn sent_rows_seen(&self, sent: &[usize]) -> (usize, Option<String>) {
        (sent.len(), sent.first().and_then(|&index| self.row_sigs.get(index)).map(|row| row.key.clone()))
    }

    /// Where the reader is, among the sent messages, as the list was last laid out.
    fn timeline_place(&self, sent: &[usize]) -> f32 {
        let list = &self.list_state;
        let top = list.logical_scroll_top();
        let into = list
            .bounds_for_item(top.item_ix)
            .map_or(0., |bounds| (top.offset_in_item / bounds.size.height).clamp(0., 1.));
        let below = f32::from(list.max_offset_for_scrollbar().height + list.scroll_px_offset_for_scrollbar().y);
        let screen = (f32::from(list.viewport_bounds().size.height) - self.composer_height).max(1.);
        place(sent, top.item_ix as f32 + into, 1. - below / screen)
    }

    fn timeline_press(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        let escape = cx.intercept_keystrokes(move |event, _, cx| {
            if event.keystroke.key == "escape" && view.update(cx, |this, cx| this.timeline_cancel(cx)).is_ok() {
                cx.stop_propagation();
            }
        });
        let from = self.list_state.logical_scroll_top();
        let rows_seen = self.sent_rows_seen(&self.sent_rows());
        self.timeline.scrub = Some(Scrub {
            from,
            from_row: self.row_sigs.get(from.item_ix).map(|row| row.key.clone()),
            from_flags: (self.at_bottom, self.at_end, self.held_at_end),
            start_y: y,
            moved: false,
            pos: None,
            last_y: Some(y),
            rows_seen,
            from_here: None,
            rows_changed: false,
            _escape: escape,
        });
        cx.notify();
    }

    /// A pointer move anywhere in the window: the hovered dash, or the chat following a press.
    fn timeline_move(
        &mut self,
        event: &MouseMoveEvent,
        strip: Bounds<Pixels>,
        hovered: bool,
        layout: Layout,
        sent: &[usize],
        cx: &mut Context<Self>,
    ) {
        let y = f32::from(event.position.y - strip.top());
        let rows_now = self.timeline.scrub.as_ref().map(|_| self.sent_rows_seen(&self.sent_rows()));
        if let Some(scrub) = &mut self.timeline.scrub {
            // A move with no button is a release that never arrived.
            let released = event.pressed_button != Some(MouseButton::Left);
            if !released && !scrub.moved && f32::from((event.position.y - scrub.start_y).abs()) < CLICK_SLOP {
                return;
            }
            scrub.moved |= !released;
            if released {
                self.timeline_release(event.position.y, strip, layout, sent, cx);
                return;
            }
            // A page came in since the strip was drawn: its dashes (and `sent`) are the old rows', so the chat stays
            // where the list kept it until the next move, which goes on from there.
            if let Some(rows_now) = rows_now
                && rows_now != scrub.rows_seen
            {
                scrub.rows_seen = rows_now;
                scrub.pos = None;
                scrub.rows_changed = true;
                scrub.last_y = Some(event.position.y);
                return;
            }
            let changed = std::mem::take(&mut scrub.rows_changed);
            let at = scrub.last_y.replace(event.position.y).map_or(y, |last| f32::from(last - strip.top()));
            // Since a page came in: on from the dash the pointer was at, and the place the chat is at.
            if changed {
                let from_here = Some((layout.dash_at(at), self.timeline_place(sent)));
                if let Some(scrub) = &mut self.timeline.scrub {
                    scrub.from_here = from_here;
                }
            }
            self.timeline_scrub(layout.dash_at(y), layout, sent, cx);
            return;
        }
        let hover = hovered.then(|| layout.dash_at(y).round().clamp(0., (layout.dashes - 1) as f32) as usize);
        if hover != self.timeline.hover {
            self.timeline.hover = hover;
            cx.notify();
        }
    }

    /// The chat at dash `dash` (fractional): its top at that place, the start or end past either end of the column.
    /// Since a page came in under the drag, it goes from where that left it instead (`Scrub::from_here`).
    fn timeline_scrub(&mut self, dash: f32, layout: Layout, sent: &[usize], cx: &mut Context<Self>) {
        let last = (layout.dashes - 1) as f32;
        let newest = (sent.len() - 1) as f32;
        let pos = match self.timeline.scrub.as_ref().and_then(|scrub| scrub.from_here) {
            Some(from) => from_here(dash, from, last, newest, layout.per as f32),
            None => (dash.clamp(0., last) * layout.per as f32).min(newest),
        };
        if let Some(scrub) = &mut self.timeline.scrub {
            scrub.pos = Some(pos);
        }
        if dash > last + 0.5 {
            self.jump_to_latest(cx);
            return;
        }
        // Past the top of the column: the very top of the chat, once at its oldest sent message.
        let row = if dash < -0.5 && pos <= 0. { 0. } else { row_for(sent, pos) };
        let item_ix = row.floor() as usize;
        let into = self
            .list_state
            .bounds_for_item(item_ix)
            .map_or(px(0.), |bounds| bounds.size.height * (row - item_ix as f32));
        self.list_state.scroll_to(ListOffset { item_ix, offset_in_item: into });
        self.at_bottom = false;
        self.held_at_end = false;
        // The reader moved the chat: a message held in place as rows changed above it stays where the drag put it,
        // as after the wheel (`scrolled`); the hold put it back the next frame.
        self.held = None;
        // The page before the oldest loaded is on its way before the drag gets there.
        if item_ix <= 2 || pos < LOAD_AHEAD {
            self.request_older(cx);
        }
        cx.notify();
    }

    fn timeline_release(
        &mut self,
        y: Pixels,
        strip: Bounds<Pixels>,
        layout: Layout,
        sent: &[usize],
        cx: &mut Context<Self>,
    ) {
        let Some(scrub) = self.timeline.scrub.take() else { return };
        if !scrub.moved {
            // A click: to that dash's message.
            let dash =
                layout.dash_at(f32::from(y - strip.top())).round().clamp(0., (layout.dashes - 1) as f32) as usize;
            if let Some(&row) = sent.get((dash * layout.per).min(sent.len() - 1)) {
                self.list_state.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
                self.at_bottom = false;
                self.held_at_end = false;
                self.held = None;
            }
        }
        cx.notify();
    }

    /// Esc while pressing: the chat back where the press found it, its row found again if pages came in above it.
    fn timeline_cancel(&mut self, cx: &mut Context<Self>) {
        let Some(scrub) = self.timeline.scrub.take() else { return };
        let item_ix = scrub
            .from_row
            .and_then(|key| self.row_sigs.iter().position(|row| row.key == key))
            .unwrap_or(scrub.from.item_ix);
        self.list_state.scroll_to(ListOffset { item_ix, ..scrub.from });
        (self.at_bottom, self.at_end, self.held_at_end) = scrub.from_flags;
        self.held = None;
        cx.notify();
    }

    pub(super) fn render_timeline(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sent = self.sent_rows();
        let height = f32::from(self.list_state.viewport_bounds().size.height) - 2. * space::LG - self.composer_height;
        let layout = Layout::new(sent.len(), height);
        if sent.len() < 2 || self.list_state.max_offset_for_scrollbar().height <= px(1.) || layout.height() > height {
            self.timeline = Timeline::default();
            return None;
        }
        let colors = *cx.colors();
        let rows_now = self.sent_rows_seen(&sent);
        if let Some(scrub) = &mut self.timeline.scrub
            && scrub.rows_seen != rows_now
        {
            // A page came in under the drag: the place shown is read off the list (which kept the message on screen)
            // until the pointer moves on from it.
            scrub.rows_seen = rows_now;
            scrub.pos = None;
            scrub.rows_changed = true;
        }
        let scrub = self.timeline.scrub.as_ref();
        let scrubbing = scrub.is_some_and(|scrub| scrub.moved);
        let pos = scrub.and_then(|scrub| scrub.pos).unwrap_or_else(|| self.timeline_place(&sent)) / layout.per as f32;
        let hover = if scrubbing { None } else { self.timeline.hover };
        let idle = if hover.is_some() || scrubbing { 0.55 } else { 0.38 };
        let dashes = (0..layout.dashes).map(|dash| {
            let near = nearness(dash as f32 - pos);
            let mut width = DASH + (DASH_CURRENT - DASH) * near;
            let mut color = mix(colors.text_tertiary, colors.text, near).opacity(idle + (1. - idle) * near);
            if hover == Some(dash) {
                width = width.max(DASH_HOVERED);
                if near < 0.8 {
                    color = colors.text_secondary;
                }
            }
            div()
                .absolute()
                .right(px(INSET))
                .top(px(PAD + dash as f32 * layout.pitch - 1.))
                .h(px(2.))
                .w(px(width))
                .rounded(px(1.))
                .bg(color)
        });
        // The card: when the dash's message was sent and how it starts, left of the strip, on the raised fill so it
        // reads over the messages.
        let shown = if scrubbing { Some((pos.round() as usize, pos)) } else { hover.map(|dash| (dash, dash as f32)) };
        let card = shown.and_then(|(dash, at)| {
            let index = sent.get((dash * layout.per).min(sent.len() - 1))?;
            let ViewRow::Message { message, .. } = self.rows.get(*index)? else { return None };
            let opening = if message.voice_recording.is_some() {
                "Voice message".to_string()
            } else {
                let text = feed::strip_markdown_for_quote(&Conversation::shown_text(message));
                let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
                // Only what can show: a truncated line still measures all it's given.
                match line.char_indices().nth(80) {
                    Some((cut, _)) => format!("{}…", &line[..cut]),
                    None if line.is_empty() && !message.attachments.is_empty() => "Attachment".to_string(),
                    None => line.to_string(),
                }
            };
            Some(
                div()
                    .absolute()
                    .right(px(RAIL_WIDTH + space::XS))
                    .top(px(PAD + at * layout.pitch))
                    .h(px(0.))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .max_w(px(280.))
                            .px(px(space::MD))
                            .py(px(space::XS + 2.))
                            .rounded(px(radius::MD))
                            .bg(colors.raised)
                            .border_1()
                            .border_color(colors.border)
                            .shadow_md()
                            .child(
                                div()
                                    .flex()
                                    .footnote()
                                    .whitespace_nowrap()
                                    .text_color(colors.text_secondary)
                                    .child(format!("{} · ", format::day_label(message.creation_time)))
                                    .child(
                                        div()
                                            .semibold()
                                            .text_color(colors.text)
                                            .child(format::clock(message.creation_time)),
                                    ),
                            )
                            .when(!opening.is_empty(), |this| {
                                this.child(div().caption().text_color(colors.text_tertiary).truncate().child(opening))
                            }),
                    ),
            )
        });
        #[cfg(feature = "dev")]
        let drawn = Drawn {
            strip: Bounds::default(),
            dashes: layout.dashes,
            pitch: layout.pitch,
            place: pos,
            hover,
            pressed: scrub.is_some(),
            card: card.as_ref().and(shown).and_then(|(dash, _)| {
                match self.rows.get(sent[(dash * layout.per).min(sent.len() - 1)]) {
                    Some(ViewRow::Message { message, .. }) => Some(message.id.clone()),
                    _ => None,
                }
            }),
        };
        // Every frame, not just while pressing: a press can come up before the next frame.
        let view = cx.entity().downgrade();
        let sent = Rc::new(sent);
        #[cfg(feature = "dev")]
        let probe = view.clone();
        let events = canvas(
            move |bounds, window, _cx| {
                // What the scroll fixture checks (`dev` builds).
                #[cfg(feature = "dev")]
                probe.update(_cx, |this, _| this.timeline.drawn = Some(Drawn { strip: bounds, ..drawn })).ok();
                window.insert_hitbox(bounds, HitboxBehavior::Normal)
            },
            move |strip, hitbox: Hitbox, window, _| {
                {
                    let (view, hitbox) = (view.clone(), hitbox.clone());
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble
                            && event.button == MouseButton::Left
                            && hitbox.is_hovered(window)
                        {
                            view.update(cx, |this, cx| this.timeline_press(event.position.y, cx)).ok();
                            cx.stop_propagation();
                        }
                    });
                }
                {
                    let (view, sent) = (view.clone(), sent.clone());
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble {
                            let hovered = hitbox.is_hovered(window);
                            view.update(cx, |this, cx| this.timeline_move(event, strip, hovered, layout, &sent, cx))
                                .ok();
                        }
                    });
                }
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        view.update(cx, |this, cx| this.timeline_release(event.position.y, strip, layout, &sent, cx))
                            .ok();
                    }
                });
            },
        )
        .absolute()
        .size_full();
        Some(
            div()
                .absolute()
                .top(px(space::LG))
                .bottom(px(self.composer_height + space::LG))
                .right_0()
                .w(px(RAIL_WIDTH))
                .flex()
                .flex_col()
                .justify_center()
                .child(div().relative().w_full().h(px(layout.height())).children(dashes).child(events).children(card))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn place_reads_between_sent_messages() {
        let sent = [2, 6, 10];
        assert_eq!(place(&sent, 0., 0.), 0.);
        assert_eq!(place(&sent, 2., 0.), 0.);
        assert_eq!(place(&sent, 4., 0.), 0.5);
        assert_eq!(place(&sent, 8.5, 0.), 1.625);
        assert_eq!(place(&sent, 12., 0.), 2.);
        // Over the final screen it moves on to the newest.
        assert_eq!(place(&sent, 4., 1.), 2.);
        assert_eq!(place(&sent, 4., 0.5), 1.25);
    }

    #[test]
    fn row_for_inverts_place() {
        let sent = [1, 3, 9, 10, 15];
        for step in 0..=40 {
            let pos = step as f32 / 10.;
            assert!((place(&sent, row_for(&sent, pos), 0.) - pos).abs() < 1e-4, "{pos}");
        }
        assert_eq!(row_for(&sent, -1.), 1.);
        assert_eq!(row_for(&sent, 9.), 15.);
    }

    #[test]
    fn a_drag_goes_on_from_where_a_page_left_it() {
        // 30 dashes, the chat at place 50 of 120 with the pointer at dash 10 when the page came in.
        let at = |dash: f32| from_here(dash, (10., 50.), 29., 120., 4.);
        assert_eq!(at(10.), 50.);
        // Up to the oldest at the first dash, in order; down to the newest at the last.
        assert_eq!(at(5.), 25.);
        assert_eq!(at(0.), 0.);
        assert_eq!(at(-3.), 0.);
        assert!((at(19.5) - 85.).abs() < 1e-4);
        assert_eq!(at(29.), 120.);
        assert_eq!(at(40.), 120.);
        // Small moves either side stay close (no jump), and up is always older.
        assert!((at(10.2) - 50.).abs() < 1. && (at(9.8) - 50.).abs() < 2.);
        assert!((0..=290).all(|step| at(step as f32 / 10.) <= at((step + 1) as f32 / 10.)));
        // With the pointer over or above the first dash, the older ones go on up from it.
        let above = |dash: f32| from_here(dash, (-2., 50.), 29., 120., 4.);
        assert_eq!(above(-2.), 50.);
        assert_eq!(above(-4.), 42.);
        assert_eq!(above(-20.), 0.);
        assert!(above(0.) > 50.);
    }

    #[test]
    fn layout_keeps_long_chats_in_the_strip() {
        let short = Layout::new(12, 500.);
        assert_eq!((short.per, short.dashes, short.pitch), (1, 12, PITCH));
        // Closer together before they share dashes.
        let tight = Layout::new(60, 400.);
        assert_eq!((tight.per, tight.dashes), (1, 60));
        assert!(tight.pitch < PITCH && tight.pitch >= MIN_PITCH);
        let long = Layout::new(400, 400.);
        assert!(long.per > 1);
        assert!(long.height() <= 400. + 0.01, "{long:?}");
        assert!(Layout::new(2, 400.).height() <= 400.);
    }

    #[test]
    fn dashes_taper_from_the_place() {
        assert_eq!(nearness(0.), 1.);
        assert!(nearness(1.) > nearness(2.) && nearness(2.) > 0.);
        assert_eq!(nearness(3.), 0.);
        assert_eq!(nearness(-1.), nearness(1.));
    }

    #[test]
    fn gutter_clears_bubbles_only_when_needed() {
        assert_eq!(gutter(1200.), 0.);
        assert_eq!(gutter(808.), 0.);
        assert_eq!(gutter(796.), 12.);
        assert_eq!(gutter(600.), 24.);
        // The bubbles' right edge stays CLEAR from the chat's edge.
        for width in [500., 760., 784., 790., 800., 808., 900.] {
            let list = width - gutter(width);
            let edge =
                gutter(width) + (list - list.min(crate::theme::MAX_CONTENT_WIDTH)) / 2. + crate::theme::PAGE_PADDING;
            assert!(edge >= CLEAR - 0.01, "{width}: {edge}");
        }
    }
}

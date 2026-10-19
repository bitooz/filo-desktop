// Rows that slide to their new places when their order changes, as the Agents list's do when a chat moves up.
//
// When the rows' order changes, each row that moved is drawn first where it showed in the frame before and eases from
// there to where the layout puts it now, both measured once the column is laid out: the rows' heights may change in
// the frame their order does (a draft's line, a status line) and a row starts from where it really was. A change of
// height alone (a line more under a row, a row coming or going) moves the rows at once, as any layout does. A row
// coming from or going to a place out of view (scrolled away, or far down a long list) slides from or to just past the
// list's edge, rather than crossing it in a frame or two, and is put in its place once there. Moving rows are drawn over
// the still ones, and those moving up over those moving down, each on the list's own colour, so one row passing another
// never shows the two's words over each other; and the pointer is theirs where they're drawn, so a click opens the row
// on top. Only the drawing moves: the layout is laid out once a frame as without the slide.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, ElementId, GlobalElementId, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Style, Window, fill, point, px,
};

/// How long a row takes to slide to its new place.
const SLIDE: Duration = Duration::from_millis(250);

/// A row is a frame on its way in the first frame drawn with its new place, so it moves at once (and keeps moving when
/// its place changes again on the way, rather than standing still for a frame).
const FIRST_STEP: Duration = Duration::from_millis(16);

/// A column of `rows`, each with a key of its own; on `cover` (the list's background, which the caller sets on it).
pub fn sliding_rows(
    id: impl Into<ElementId>,
    cover: Hsla,
    rows: impl IntoIterator<Item = (SharedString, AnyElement)>,
) -> SlidingRows {
    SlidingRows { id: id.into(), cover, rows: rows.into_iter().collect() }
}

pub struct SlidingRows {
    id: ElementId,
    cover: Hsla,
    rows: Vec<(SharedString, AnyElement)>,
}

/// Kept from frame to frame while the column shows; it goes when it doesn't, so a list shown again doesn't slide.
#[derive(Default)]
struct Placed {
    /// The rows' keys in the order drawn last frame.
    order: Vec<SharedString>,
    /// Where each row was drawn last frame (points from the column's top).
    drawn: HashMap<SharedString, f32>,
    /// The rows sliding: where each started (within the list's view) and when.
    moving: HashMap<SharedString, (f32, Instant)>,
}

/// The rows in the order they're drawn, each with where it's drawn and how far that is from its place.
pub struct Drawn(Vec<(usize, Bounds<Pixels>, f32)>);

/// Whether the rows in both `old` and `new` come in another order (a row coming or going doesn't count).
fn reordered(old: &[SharedString], new: &[SharedString]) -> bool {
    let kept = |order: &[SharedString], other: &[SharedString]| -> Vec<SharedString> {
        order.iter().filter(|key| other.contains(key)).cloned().collect()
    };
    kept(old, new) != kept(new, old)
}

/// `at` for a row `height` tall, with the list showing from `top` to `bottom`: where it is, or just past the edge it's
/// beyond (where a slide from or to a place out of view starts or ends).
fn within(at: f32, height: f32, top: f32, bottom: f32) -> f32 {
    at.clamp(top - height, bottom.max(top - height))
}

/// Where a row sliding from `start` to `end` is `elapsed` after it started, easing out (none once there).
fn eased(start: f32, end: f32, elapsed: Duration) -> Option<f32> {
    let t = elapsed.as_secs_f32() / slide_time().as_secs_f32();
    (t < 1.).then(|| end + (start - end) * (1. - t).powi(3))
}

fn slide_time() -> Duration {
    #[cfg(feature = "dev")]
    if let Some(slow) = check::slow() {
        return slow;
    }
    SLIDE
}

impl IntoElement for SlidingRows {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for SlidingRows {
    type RequestLayoutState = Vec<LayoutId>;
    type PrepaintState = Drawn;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Vec<LayoutId>) {
        let rows: Vec<LayoutId> = self.rows.iter_mut().map(|(_, row)| row.request_layout(window, cx)).collect();
        (window.request_layout(Style::default(), rows.iter().copied(), cx), rows)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Vec<LayoutId>,
        window: &mut Window,
        cx: &mut App,
    ) -> Drawn {
        let shown = window.content_mask().bounds;
        #[cfg(feature = "dev")]
        check::shown(&self.id, bounds, shown);
        let (top, bottom) = (f32::from(shown.top() - bounds.top()), f32::from(shown.bottom() - bounds.top()));
        let now = Instant::now();
        let mut drawn = window.with_element_state::<Placed, _>(id.unwrap(), |placed, window| {
            let placed = placed.unwrap_or_default();
            let order: Vec<SharedString> = self.rows.iter().map(|(key, _)| key.clone()).collect();
            let reordered = reordered(&placed.order, &order);
            let mut next = Placed { order, ..Placed::default() };
            let mut drawn = Vec::with_capacity(self.rows.len());
            for (index, ((key, _), layout)) in self.rows.iter().zip(layout.iter()).enumerate() {
                let row = window.layout_bounds(*layout);
                let (at, height) = (f32::from(row.top() - bounds.top()), f32::from(row.size.height));
                // Toward its place, or the list's edge when that's out of view (then put in its place once there).
                let end = within(at, height, top, bottom);
                let mut moving = placed.moving.get(key).copied();
                // The order changed and this row's place with it: it slides there from where it was drawn.
                if reordered
                    && let Some(was) = placed.drawn.get(key)
                    && (was - at).abs() >= 0.5
                {
                    let start = within(*was, height, top, bottom);
                    moving = ((start - end).abs() >= 0.5).then_some((start, now - FIRST_STEP));
                }
                let y = moving.and_then(|(start, started)| eased(start, end, now - started));
                if y.is_some() {
                    next.moving.extend(moving.map(|moving| (key.clone(), moving)));
                }
                let by = y.map_or(0., |y| y - at);
                next.drawn.insert(key.clone(), at + by);
                drawn.push((index, Bounds { origin: row.origin + point(px(0.), px(by)), size: row.size }, by));
            }
            if !next.moving.is_empty() {
                window.request_animation_frame();
            }
            (drawn, next)
        });
        // The still rows first, then those moving down, then those moving up, each over the ones before.
        drawn.sort_by_key(|(_, _, by)| (*by != 0., *by > 0.));
        for (index, row, by) in &drawn {
            // A moving row takes the pointer where it's drawn (the list under it still scrolls): a click there is its,
            // not the covered row's.
            if *by != 0. {
                window.insert_hitbox(*row, HitboxBehavior::BlockMouseExceptScroll);
            }
            window.with_element_offset(point(px(0.), px(*by)), |window| self.rows[*index].1.prepaint(window, cx));
        }
        Drawn(drawn)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Vec<LayoutId>,
        drawn: &mut Drawn,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (index, row, by) in &drawn.0 {
            if *by != 0. {
                window.paint_quad(fill(*row, self.cover));
            }
            self.rows[*index].1.paint(window, cx);
        }
    }
}

/// For the scripted checks (`FILO_OPEN=list-reorder-timing`).
#[cfg(feature = "dev")]
pub mod check {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use gpui::{Bounds, ElementId, Pixels};

    static SLOW: AtomicU64 = AtomicU64::new(0);
    static SHOWN: Mutex<Option<HashMap<String, (f32, f32, f32)>>> = Mutex::new(None);

    /// Slides take `slide` (none: as long as they do), so a check can click rows on their way.
    pub fn slow_down(slide: Option<Duration>) {
        SLOW.store(slide.map_or(0, |slide| slide.as_millis() as u64), Ordering::Relaxed);
    }

    pub(super) fn slow() -> Option<Duration> {
        Some(SLOW.load(Ordering::Relaxed)).filter(|ms| *ms > 0).map(Duration::from_millis)
    }

    pub(super) fn shown(id: &ElementId, column: Bounds<Pixels>, shown: Bounds<Pixels>) {
        let part = (f32::from(column.top()), f32::from(shown.top()), f32::from(shown.bottom()));
        SHOWN.lock().unwrap().get_or_insert_default().insert(id.to_string(), part);
    }

    /// Where the column `id` was last drawn, in window points: its top, and the top and bottom of the part of it in
    /// view.
    pub fn shown_part(id: &str) -> Option<(f32, f32, f32)> {
        SHOWN.lock().unwrap().as_ref()?.get(id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_slides_from_or_to_the_edge_of_the_view_when_its_place_is_out_of_it() {
        // In view (the list shows 0 to 600), where it is.
        assert_eq!(within(300., 56., 0., 600.), 300.);
        assert_eq!(within(-20., 56., 0., 600.), -20.);
        // Far below or above, just past the edge.
        assert_eq!(within(1400., 56., 0., 600.), 600.);
        assert_eq!(within(-900., 56., 0., 600.), -56.);
    }

    #[test]
    fn a_row_eases_to_its_place_and_stops_there() {
        assert_eq!(eased(400., 200., Duration::ZERO), Some(400.));
        let early = eased(400., 200., Duration::from_millis(16)).unwrap();
        let later = eased(400., 200., Duration::from_millis(125)).unwrap();
        // Moving at once, but not most of the way in its first frame; slowing as it gets there.
        assert!(early < 400. && early > 350., "{early}");
        assert!(later < early && later > 200., "{later}");
        assert_eq!(eased(400., 200., SLIDE), None);
    }

    #[test]
    fn only_a_new_order_slides() {
        let keys = |list: &[&str]| list.iter().map(|key| SharedString::from(key.to_string())).collect::<Vec<_>>();
        assert!(reordered(&keys(&["a", "b", "c"]), &keys(&["c", "a", "b"])));
        assert!(!reordered(&keys(&["a", "b", "c"]), &keys(&["a", "b", "c"])));
        // A row coming or going moves the others at once.
        assert!(!reordered(&keys(&["a", "b", "c"]), &keys(&["g", "a", "b", "c"])));
        assert!(!reordered(&keys(&["a", "b", "c"]), &keys(&["a", "c"])));
        assert!(reordered(&keys(&["a", "b", "c"]), &keys(&["g", "b", "a"])));
    }
}

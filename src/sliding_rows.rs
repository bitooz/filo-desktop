// Rows that slide to their new places when their order changes, as the Agents list's do when a chat moves up.
//
// Each row is drawn first where it showed in the frame before and eases from there to where the layout puts it now,
// both measured once the column is laid out: the rows' heights may change in the frame their order does (a draft's
// line, a status line) and a row starts from where it really was. A row coming from far off (scrolled away) starts at
// the list's edge rather than crossing it in a frame or two. Rows moving up are drawn over the others, on the list's
// own colour, so one row passing another never shows the two's words over each other. Only the drawing moves: the
// layout is laid out once a frame as without the slide.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels,
    SharedString, Style, Window, fill, point, px,
};

/// How long a row takes to slide to its new place.
const SLIDE: Duration = Duration::from_millis(250);

/// A row is a frame on its way in the first frame drawn with its new place, so it moves at once (and keeps moving when
/// its place changes again on the way, rather than standing still for a frame).
const FIRST_STEP: Duration = Duration::from_millis(16);

/// A column of `rows`, each with a key of its own; on `cover` (the colour behind the list).
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
    /// Each row's place in the layout last frame, and where it was drawn (points from the column's top).
    rows: HashMap<SharedString, (f32, f32)>,
    /// The rows sliding: how far from their place each started, and when.
    moving: HashMap<SharedString, (f32, Instant)>,
}

/// The rows in the order they're drawn, each with where it's drawn and how far that is from its place.
pub struct Drawn(Vec<(usize, Bounds<Pixels>, f32)>);

/// Where a row drawn at `drawn` starts sliding, `height` tall, with the list showing from `top` to `bottom`: from where
/// it was, or from just past the edge it's beyond.
fn start(drawn: f32, height: f32, top: f32, bottom: f32) -> f32 {
    drawn.clamp(top - height, bottom.max(top - height))
}

/// How far a row that started `from` its place is from it `elapsed` later, easing out to nothing (none once there).
fn offset(from: f32, elapsed: Duration) -> Option<f32> {
    let t = elapsed.as_secs_f32() / SLIDE.as_secs_f32();
    (t < 1.).then(|| from * (1. - t).powi(3))
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
        let (top, bottom) = (f32::from(shown.top() - bounds.top()), f32::from(shown.bottom() - bounds.top()));
        let now = Instant::now();
        let mut drawn = window.with_element_state::<Placed, _>(id.unwrap(), |placed, window| {
            let placed = placed.unwrap_or_default();
            let mut next = Placed::default();
            let mut drawn = Vec::with_capacity(self.rows.len());
            for (index, ((key, _), layout)) in self.rows.iter().zip(layout.iter()).enumerate() {
                let row = window.layout_bounds(*layout);
                let at = f32::from(row.top() - bounds.top());
                let mut moving = placed.moving.get(key).copied();
                // Its place changed: it slides there from where it was drawn.
                if let Some((was, shown_at)) = placed.rows.get(key)
                    && (was - at).abs() >= 0.5
                {
                    let from = start(*shown_at, f32::from(row.size.height), top, bottom) - at;
                    moving = (from.abs() >= 0.5).then_some((from, now - FIRST_STEP));
                }
                let by = moving.and_then(|(from, started)| offset(from, now - started)).unwrap_or(0.);
                if by != 0. {
                    next.moving.extend(moving.map(|moving| (key.clone(), moving)));
                }
                next.rows.insert(key.clone(), (at, at + by));
                drawn.push((index, Bounds { origin: row.origin + point(px(0.), px(by)), size: row.size }, by));
            }
            if !next.moving.is_empty() {
                window.request_animation_frame();
            }
            (drawn, next)
        });
        // Those moving up go last, over the rest (and take the pointer there first).
        drawn.sort_by_key(|(_, _, by)| *by > 0.);
        for (index, _, by) in &drawn {
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
            if *by > 0. {
                window.paint_quad(fill(*row, self.cover));
            }
            self.rows[*index].1.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_slides_from_where_it_was_or_from_the_edge_it_is_past() {
        // On screen (the list shows 0 to 600), it starts where it was.
        assert_eq!(start(300., 56., 0., 600.), 300.);
        assert_eq!(start(-20., 56., 0., 600.), -20.);
        // Far below or above, just past the edge.
        assert_eq!(start(1400., 56., 0., 600.), 600.);
        assert_eq!(start(-900., 56., 0., 600.), -56.);
    }

    #[test]
    fn a_row_eases_to_its_place_and_stops_there() {
        assert_eq!(offset(-200., Duration::ZERO), Some(-200.));
        let early = offset(-200., Duration::from_millis(16)).unwrap();
        let later = offset(-200., Duration::from_millis(125)).unwrap();
        // Moving at once, but not most of the way in its first frame; slowing as it gets there.
        assert!(early > -200. && early < -150., "{early}");
        assert!(later > early && later < 0., "{later}");
        assert_eq!(offset(-200., SLIDE), None);
    }
}

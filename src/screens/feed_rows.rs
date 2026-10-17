// The Feed's list bookkeeping (screens/feed.rs): telling GPUI's `list` which rows changed, without losing the reader's
// place.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use gpui::{ListOffset, ListState, Pixels, px};

/// A row as the list knows it: what it shows (`key`, stable while it shows the same thing) and a hash of everything
/// that sets its height (`sig`). A row whose `sig` changes is measured again; one whose key stays keeps its height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowSig {
    pub key: String,
    pub sig: u64,
}

pub fn sig(parts: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

/// Brings `list` from the `old` rows to the `new` ones: rows that left go, new ones come in where they belong, rows
/// whose height may have changed are measured again, and every other row keeps its measured height. The row at the top
/// of the screen stays there, as far scrolled into it as before, even when it was measured again or rows came or went
/// above it (GPUI moves it to its top otherwise, or onto a neighbour).
pub fn sync_list(list: &ListState, old: &[RowSig], new: &[RowSig]) {
    if old == new {
        return;
    }
    let top = list.logical_scroll_top();
    let anchor = old.get(top.item_ix).map(|row| row.key.clone());
    for (range, count) in splices(old, new).into_iter().rev() {
        list.splice(range, count);
    }
    if let Some(index) = anchor.and_then(|key| new.iter().position(|row| row.key == key)) {
        list.scroll_to(ListOffset { item_ix: index, offset_in_item: top.offset_in_item });
    }
}

/// A row held `below` points under the top of the view while the rows above it, just come, haven't been measured: the
/// list only measures them as it lays out, so the place is set again on each frame (`hold` in a row's prepaint).
#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub key: String,
    pub below: Pixels,
}

/// `sync_list`, anchored on rows `can_anchor` allows (not one that stays on top while history comes in under it,
/// like a "loading older messages" row). When the row at the top goes, or can't anchor, the first row in view that
/// stays keeps its place on screen instead: older messages arriving above the one being read don't move it. That row
/// then sits below the top with new rows above it, and the returned `Pin` says where to hold it.
pub fn sync_list_anchored(
    list: &ListState,
    old: &[RowSig],
    new: &[RowSig],
    can_anchor: impl Fn(&str) -> bool,
) -> Option<Pin> {
    if old == new {
        return None;
    }
    let top = list.logical_scroll_top();
    let kept: HashSet<&str> = new.iter().map(|row| row.key.as_str()).collect();
    let stays = |row: &RowSig| kept.contains(row.key.as_str()) && can_anchor(&row.key);
    // (key, offset into it: below zero, that far under the top of the view).
    let anchor = match old.get(top.item_ix) {
        Some(row) if stays(row) => Some((row.key.clone(), top.offset_in_item)),
        Some(row) => below_top(list, old, top.item_ix, &stays)
            .or_else(|| kept.contains(row.key.as_str()).then(|| (row.key.clone(), top.offset_in_item))),
        None => None,
    };
    for (range, count) in splices(old, new).into_iter().rev() {
        list.splice(range, count);
    }
    let (key, offset) = anchor?;
    let index = new.iter().position(|row| row.key == key)?;
    list.scroll_to(ListOffset { item_ix: index, offset_in_item: offset });
    (offset < px(0.)).then(|| Pin { key, below: -offset })
}

/// The first row after `top` that's on screen and `stays`, with how far into it the view's top is (negative: under it).
fn below_top(
    list: &ListState,
    old: &[RowSig],
    top: usize,
    stays: &impl Fn(&RowSig) -> bool,
) -> Option<(String, Pixels)> {
    let viewport = list.viewport_bounds();
    for (index, row) in old.iter().enumerate().skip(top + 1) {
        let bounds = list.bounds_for_item(index)?;
        if bounds.top() >= viewport.bottom() {
            return None;
        }
        if stays(row) {
            return Some((row.key.clone(), viewport.top() - bounds.top()));
        }
    }
    None
}

/// Holds `pin`'s row (at `index`) where it was, for this frame: the list lays out with the row a point lower than
/// wanted, and `hold`, prepainted with the row, asks it to scroll so the row sits `below` the top. The list answers in
/// the same frame, measuring the rows above it as it does, so nothing shows in between.
pub fn place_pin(list: &ListState, index: usize, pin: &Pin) {
    list.scroll_to(ListOffset { item_ix: index, offset_in_item: -(pin.below + px(1.)) });
}

/// The other half of `place_pin`, drawn at the top of the pinned row: the part of the view from the row's top down
/// to `viewport_height - below`, which the list scrolls to show at its bottom (gpui's autoscroll).
pub fn hold(pin: &Pin, viewport_height: Pixels) -> impl gpui::IntoElement + use<> {
    use gpui::Styled as _;
    let span = (viewport_height - pin.below).max(px(1.));
    gpui::canvas(
        move |bounds, window, _| {
            window.request_autoscroll(gpui::Bounds::new(bounds.origin, gpui::size(bounds.size.width.max(px(1.)), span)))
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h(px(0.))
}

/// The splices, in order, that turn `old` into `new`: (old rows replaced, how many new rows replace them). Rows kept
/// (the longest common run of keys) aren't touched unless their `sig` changed.
pub fn splices(old: &[RowSig], new: &[RowSig]) -> Vec<(std::ops::Range<usize>, usize)> {
    let (n, m) = (old.len(), new.len());
    // Longest common subsequence of keys (lists are a few dozen rows).
    let mut lcs = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i].key == new[j].key { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut out: Vec<(std::ops::Range<usize>, usize)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut gap_old, mut gap_new) = (0, 0);
    let flush = |out: &mut Vec<_>, i: usize, gap_old: &mut usize, gap_new: &mut usize| {
        if *gap_old > 0 || *gap_new > 0 {
            out.push((i - *gap_old..i, *gap_new));
        }
        *gap_old = 0;
        *gap_new = 0;
    };
    while i < n || j < m {
        if i < n && j < m && old[i].key == new[j].key {
            flush(&mut out, i, &mut gap_old, &mut gap_new);
            if old[i].sig != new[j].sig {
                out.push((i..i + 1, 1));
            }
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            gap_new += 1;
            j += 1;
        } else {
            gap_old += 1;
            i += 1;
        }
    }
    flush(&mut out, i, &mut gap_old, &mut gap_new);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{ListAlignment, px};

    fn rows(keys: &[&str]) -> Vec<RowSig> {
        keys.iter().map(|key| RowSig { key: key.to_string(), sig: 0 }).collect()
    }

    fn apply(list_len: usize, old: &[RowSig], new: &[RowSig]) -> usize {
        let mut len = list_len;
        for (range, count) in splices(old, new).into_iter().rev() {
            len = len - range.len() + count;
        }
        len
    }

    #[test]
    fn only_the_rows_that_changed_are_spliced() {
        let old = rows(&["header", "a", "b", "c", "end"]);
        // A new post on top and the oldest gone, at once.
        let new = rows(&["header", "z", "a", "b", "end"]);
        assert_eq!(splices(&old, &new), vec![(1..1, 1), (3..4, 0)]);
        assert_eq!(apply(old.len(), &old, &new), new.len());
        let mut grown = new.clone();
        grown[2].sig = 7;
        assert_eq!(splices(&new, &grown), vec![(2..3, 1)]);
        assert!(splices(&new, &new).is_empty());
    }

    #[test]
    fn the_row_being_read_keeps_its_place_when_posts_come_and_go_above_it() {
        let old = rows(&["header", "a", "b", "c", "d", "end"]);
        let list = ListState::new(old.len(), ListAlignment::Top, px(100.));
        // Reading "c", 120 points into it.
        list.scroll_to(ListOffset { item_ix: 3, offset_in_item: px(120.) });
        let new = rows(&["header", "y", "z", "b", "c", "end"]);
        sync_list(&list, &old, &new);
        let top = list.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (4, px(120.)));
        assert_eq!(list.item_count(), new.len());
    }

    #[test]
    fn opening_more_on_the_row_being_read_keeps_the_place_in_it() {
        let old = rows(&["header", "a", "b", "end"]);
        let list = ListState::new(old.len(), ListAlignment::Top, px(100.));
        list.scroll_to(ListOffset { item_ix: 2, offset_in_item: px(300.) });
        let mut new = old.clone();
        new[2].sig = 1; // "b" opened its summary.
        sync_list(&list, &old, &new);
        let top = list.logical_scroll_top();
        // GPUI alone would put it back at the top of "b" (offset 0).
        assert_eq!((top.item_ix, top.offset_in_item), (2, px(300.)));
    }

    #[test]
    fn anchored_sync_keeps_the_row_being_read_without_a_pin() {
        let old = rows(&["older", "a", "b", "c", "end"]);
        let list = ListState::new(old.len(), ListAlignment::Top, px(100.));
        list.scroll_to(ListOffset { item_ix: 2, offset_in_item: px(30.) });
        // An older page comes in under the loading row.
        let new = rows(&["older", "x", "y", "a", "b", "c", "end"]);
        let pin = sync_list_anchored(&list, &old, &new, |key| key != "older");
        let top = list.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (4, px(30.)));
        assert_eq!(pin, None, "a row read from its top needs no holding");
        assert_eq!(list.item_count(), new.len());
    }

    #[test]
    fn when_the_row_being_read_goes_the_list_stays_where_it_was() {
        let old = rows(&["header", "a", "b", "c", "end"]);
        let list = ListState::new(old.len(), ListAlignment::Top, px(100.));
        list.scroll_to(ListOffset { item_ix: 2, offset_in_item: px(40.) });
        let new = rows(&["header", "a", "c", "end"]);
        sync_list(&list, &old, &new);
        assert_eq!(list.logical_scroll_top().item_ix, 2);
        assert_eq!(list.item_count(), 4);
    }
}

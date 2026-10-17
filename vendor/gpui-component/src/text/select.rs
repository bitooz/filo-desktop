//! Filo: what a text view's selection covers, kept apart from drawing so the app can test it.
//!
//! 0.5.1 took the selection as the rectangle between where the drag began and ended, so dragging from the middle of
//! a line to the start of a later one selected the first line from the left edge and the last one to the right of
//! where the pointer stopped. A selection runs in reading order instead: from one end to the other along the lines.

use std::ops::Range;

use gpui::{Half, Pixels, Point};

/// Whether a character is between the selection's ends `a` and `b` (window points, either first) in reading order.
/// `pos` is the top left of the character's box, `width` its width and `line_height` its line's height.
pub fn char_selected(pos: Point<Pixels>, width: Pixels, line_height: Pixels, a: Point<Pixels>, b: Point<Pixels>) -> bool {
    let center = pos.x + width.half();
    let on_line = |p: Point<Pixels>| pos.y <= p.y && p.y < pos.y + line_height;
    // At or after `p`: on a line below it, or on its line from it on.
    let after = |p: Point<Pixels>| pos.y > p.y || (on_line(p) && center >= p.x);
    // At or before `p`: on a line above it, or on its line up to it.
    let before = |p: Point<Pixels>| pos.y + line_height <= p.y || (on_line(p) && center <= p.x);
    (after(a) && before(b)) || (after(b) && before(a))
}

/// The word around byte `ix` of `text`, as a double-click selects it: letters, digits and `_` (with an apostrophe
/// inside a word, "can't"), a run of spaces, or a single other character. Past the end, the last character's.
pub fn word_at(text: &str, ix: usize) -> Range<usize> {
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        Word,
        Space,
        Other,
    }
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Kind::Word
        } else if c.is_whitespace() {
            Kind::Space
        } else {
            Kind::Other
        }
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        return 0..0;
    }
    let mut at = chars.iter().position(|(start, c)| ix < start + c.len_utf8()).unwrap_or(chars.len() - 1);
    // A click past a line's end lands on its newline: take the character before it.
    if chars[at].1 == '\n' && at > 0 && ix >= chars[at].0 {
        at -= 1;
    }
    let end_of = |i: usize| chars[i].0 + chars[i].1.len_utf8();
    let here = kind(chars[at].1);
    if here == Kind::Other {
        return chars[at].0..end_of(at);
    }
    // An apostrophe joins two word characters.
    let joins = |i: usize| {
        here == Kind::Word
            && matches!(chars[i].1, '\'' | '\u{2019}')
            && i > 0
            && i + 1 < chars.len()
            && kind(chars[i - 1].1) == Kind::Word
            && kind(chars[i + 1].1) == Kind::Word
    };
    let same = |i: usize| (kind(chars[i].1) == here && chars[i].1 != '\n') || joins(i);
    let (mut first, mut last) = (at, at);
    while first > 0 && same(first - 1) {
        first -= 1;
    }
    while last + 1 < chars.len() && same(last + 1) {
        last += 1;
    }
    chars[first].0..end_of(last)
}

//! Filo: where `inline code` and links sit on wrapped lines, kept apart from drawing so the app can test it.
//!
//! gpui 0.2 wraps a paragraph itself and may break before any character that isn't a letter, a digit, a space or one of
//! `-_.'$%@#^~,=:` (`LineWrapper::is_word_char`), so a `)` after a commit hash that just fits went to the next line on
//! its own. And when a line wrapped right where a link ended, gpui drew a stray stub of its underline at the next line's
//! start, half a glyph box to the left of the text, past the bubble's padding. `Inline` keeps such punctuation with the
//! code or link before it (`kept_with`) and draws links' underlines itself, a piece per line (`pieces`).

use std::ops::Range;

use gpui::{point, Pixels, Point};

/// Closing punctuation gpui 0.2's wrapper may move to a line of its own.
const CLOSERS: [char; 7] = [')', ']', '}', '"', '?', '!', ';'];

/// The byte offsets of the closing punctuation right after one of `spans` (`inline code` and links): `0959ce6)` or
/// `` `x`)." `` keeps its `)` with the hash, so the two move to the next line together. Punctuation that is itself code
/// is left alone. Sorted, each once.
pub fn kept_with(text: &str, spans: &[Range<usize>], code: &[Range<usize>]) -> Vec<usize> {
    let mut kept = Vec::new();
    for span in spans.iter().filter(|span| !span.is_empty()) {
        let mut ix = span.end;
        while let Some(c) = text.get(ix..).and_then(|rest| rest.chars().next()) {
            if !CLOSERS.contains(&c) || code.iter().any(|code| code.contains(&ix)) {
                break;
            }
            kept.push(ix);
            ix += c.len_utf8();
        }
    }
    kept.sort_unstable();
    kept.dedup();
    kept
}

/// Each piece of `range` of `text` on one line: its left and right edges and its line's top, from `position`, where
/// each byte offset is drawn (`TextLayout::position_for_index`, which puts the offset a line wraps at at the end of the
/// line before). A piece after a wrap starts at `left`, the text's left edge; spaces a piece ends with at a wrap are left
/// out. Empty pieces are dropped.
pub fn pieces(
    text: &str,
    range: Range<usize>,
    position: impl Fn(usize) -> Option<Point<Pixels>>,
    left: Pixels,
) -> Vec<(Pixels, Pixels, Pixels)> {
    let mut pieces = Vec::new();
    let Some(mut start) = position(range.start) else { return pieces };
    let Some(slice) = text.get(range.clone()) else { return pieces };
    let mut last = start;
    // The end of the piece's last character that isn't a space.
    let mut inked: Option<Pixels> = None;
    let mut ix = range.start;
    for c in slice.chars() {
        ix += c.len_utf8();
        let Some(next) = position(ix) else { break };
        if next.y != last.y {
            // Wrapped before `c`: this line's piece ends with its last character; the next starts at the left edge.
            if let Some(right) = inked.take() {
                pieces.push((start.x, right, start.y));
            }
            start = point(left, next.y);
        }
        if !c.is_whitespace() {
            inked = Some(next.x);
        }
        last = next;
    }
    // The last piece runs to the range's end, spaces and all.
    pieces.push((start.x, last.x, start.y));
    pieces.retain(|(left, right, _)| right > left);
    pieces
}

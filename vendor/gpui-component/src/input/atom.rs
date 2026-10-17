//! Filo: atoms, pieces of an input's text drawn as a chip and edited as one unit (an attached file in the chat
//! composer: its picture, name and size). The text stays in the input as it is; the element paints a rounded chip
//! behind it, an image over its first `image_len` bytes (spaces left for it) and its text from `muted_from` in the
//! muted colour. Edits that touch an atom take all of it, the cursor never stops inside one, and edits elsewhere move
//! it along.

use std::ops::Range;
use std::sync::Arc;

use gpui::{Bounds, Hsla, Pixels, RenderImage, Window, point, px, size};

#[derive(Clone)]
pub struct InputAtom {
    /// The caller's id for it.
    pub id: u64,
    /// Its bytes in the input's text.
    pub range: Range<usize>,
    /// Painted over its first `image_len` bytes, once there is one.
    pub image: Option<Arc<RenderImage>>,
    pub image_len: usize,
    /// From this byte of the atom on, its text is muted (a file's size after its name).
    pub muted_from: usize,
    pub background: Hsla,
    pub border: Hsla,
    pub muted: Hsla,
}

/// Paints `atom`'s chip under its text: a rounded chip behind each of its `pieces` (one per line it covers, each a
/// line high) and its picture, round, in the `image` room its text leaves for it. Also used for the chips in a sent
/// message's text, so they look as they did in the input.
pub fn paint_chip(
    atom: &InputAtom,
    pieces: &[Bounds<Pixels>],
    image: Option<Bounds<Pixels>>,
    line_height: Pixels,
    window: &mut Window,
) {
    let inset = (line_height * 0.1).round();
    for piece in pieces {
        let chip = Bounds::new(
            point(piece.origin.x, piece.origin.y + inset),
            size(piece.size.width, piece.size.height - inset * 2.),
        );
        window.paint_quad(gpui::quad(
            chip,
            chip.size.height / 2.,
            atom.background,
            px(1.),
            atom.border,
            gpui::BorderStyle::Solid,
        ));
    }
    if let (Some(picture), Some(room)) = (atom.image.clone(), image) {
        let side = (room.size.height - inset * 2. - px(4.)).max(px(4.));
        let at = point(
            room.origin.x + (room.size.width - side).max(px(0.)) / 2.,
            room.origin.y + (room.size.height - side) / 2.,
        );
        let _ = window.paint_image(Bounds::new(at, size(side, side)), gpui::Corners::all(side / 2.), picture, 0, false);
    }
}

/// `range` grown to take in every atom it cuts into or lands inside: an edit takes an atom whole or not at all. An
/// empty range at an atom's edge stays as it is (typing next to a chip).
pub fn expand_to_atoms(atoms: &[InputAtom], range: Range<usize>) -> Range<usize> {
    let mut range = range;
    for atom in atoms {
        let cuts = if range.is_empty() {
            range.start > atom.range.start && range.start < atom.range.end
        } else {
            range.start < atom.range.end && range.end > atom.range.start
        };
        if cuts {
            range.start = range.start.min(atom.range.start);
            range.end = range.end.max(atom.range.end);
        }
    }
    range
}

/// The atoms after `range` was replaced by `new_len` bytes (`range` already expanded): those inside it are gone,
/// those after it move.
pub fn atoms_after_edit(atoms: &mut Vec<InputAtom>, range: &Range<usize>, new_len: usize) {
    atoms.retain(|atom| !(range.start < atom.range.end && range.end > atom.range.start));
    for atom in atoms.iter_mut() {
        if atom.range.start >= range.end {
            atom.range.start = atom.range.start - range.end + range.start + new_len;
            atom.range.end = atom.range.end - range.end + range.start + new_len;
        }
    }
}

/// `offset` moved out of any atom it is inside: to the edge `toward` is nearer to (the start when moving back).
pub fn snap_out_of_atoms(atoms: &[InputAtom], offset: usize, toward: usize) -> usize {
    for atom in atoms {
        if offset > atom.range.start && offset < atom.range.end {
            return if toward <= offset { atom.range.start } else { atom.range.end };
        }
    }
    offset
}

// Tested from the app (apps/desktop/src/screens/chat/attachments.rs): this copy's own tests need a workspace.

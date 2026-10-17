use std::{
    cell::RefCell,
    collections::HashMap,
    ops::Range,
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui::{
    point, px, quad, App, BorderStyle, Bounds, CursorStyle, Edges, Element, ElementId, Font, FontWeight, Hsla, TextRun,
    GlobalElementId, Half, HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, StyledText,
    TextLayout, UnderlineStyle, Window,
};

use crate::{global_state::GlobalState, input::Selection, text::{node::LinkMark, InlineCodeStyle}, ActiveTheme};

/// A inline element used to render a inline text and support selectable.
///
/// All text in TextView (including the CodeBlock) used this for text rendering.
pub(super) struct Inline {
    id: ElementId,
    text: SharedString,
    links: Rc<Vec<(Range<usize>, LinkMark)>>,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// Filo: `inline code`, in the mono font on a chip (see `paint_code_chips`).
    code_ranges: Vec<Range<usize>>,
    /// Filo: the chip's colours and the code's font, when the text view's style sets them.
    code_style: Option<InlineCodeStyle>,
    /// Filo: set during layout when `code_style.compact` draws code smaller than the text.
    compact: Option<CompactCode>,
    /// Filo: underlines (links'), drawn by `paint_underlines` rather than by gpui, set during layout.
    underlines: Vec<(Range<usize>, UnderlineStyle)>,
    /// Filo: punctuation kept with the code or link before it (`wrap::kept_with`), set during layout.
    kept: Vec<Kept>,
    styled_text: StyledText,

    state: Arc<Mutex<InlineState>>,
}

/// The inline text state, used RefCell to keep the selection state.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct InlineState {
    hovered_index: Option<usize>,
    /// Filo: the link the left button went down on, and where: it opens if the button comes up on it close by.
    pressed_link: Option<(SharedString, Point<Pixels>)>,
    /// The text that actually rendering, matched with selection.
    pub(super) text: SharedString,
    pub(super) selection: Option<Selection>,
}

impl InlineState {
    /// Save actually rendered text for selected text to use.
    pub(crate) fn set_text(&mut self, text: SharedString) {
        self.text = text;
    }
}

/// Filo: how compact `inline code` was laid out (`InlineCodeStyle::compact`). gpui 0.2 shapes a paragraph at one font
/// size, so each code character is held in the layout by one stand-in letter of the text's own font, `slot` wide (the
/// first and last by a wider letter, `pad` more, for the chip's sides), drawn transparent. The code is painted over them
/// in the mono font at `size`, chosen so its characters are exactly `slot` wide. Byte offsets are unchanged, so
/// selection, copying and links work on the real text.
struct CompactCode {
    ranges: Vec<Range<usize>>,
    size: Pixels,
    slot: Pixels,
    pad: Pixels,
    mono: Font,
}

impl CompactCode {
    /// How much smaller than the text the code is drawn, roughly.
    const SCALE: f32 = 0.8;

    /// Picks the stand-in letters for `font` at `font_size`; `None` when the fonts can't be measured. Widths are taken
    /// from shaped runs, not font metrics: CoreText tracks SF Pro, so its letters sit closer than their advances say.
    fn measure(font: &Font, mono: Font, font_size: Pixels, window: &Window) -> Option<(Self, char, char)> {
        const RUN: usize = 10;
        let text_system = window.text_system();
        let width = |font: &Font, text: String| {
            let run = TextRun { len: text.len(), font: font.clone(), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
            text_system.shape_line(text.into(), font_size, &[run], None).width
        };
        let mono_advance = width(&mono, "0".repeat(RUN)) / RUN as f32;
        let closest = |candidates: &str, target: Pixels, measure: &dyn Fn(char) -> Pixels| {
            candidates
                .chars()
                .map(|c| (c, measure(c)))
                .min_by(|a, b| (a.1 - target).abs().partial_cmp(&(b.1 - target).abs()).unwrap_or(std::cmp::Ordering::Equal))
        };
        // Letters, so the wrapper keeps a code span together as one word.
        let (letter, slot) = closest("nuhvxoaecs", mono_advance * Self::SCALE, &|c| width(font, c.to_string().repeat(RUN)) / RUN as f32)?;
        // A wide letter at each end, as it sits next to the stand-ins, for the chip's sides.
        let wide_in_run = |c: char| (width(font, format!("{c}{}{c}", letter.to_string().repeat(RUN))) - slot * RUN as f32).half();
        let (wide, wide_width) = closest("mwMW", slot + font_size * 0.3, &wide_in_run)?;
        if mono_advance <= px(0.) || slot <= px(0.) || wide_width <= slot {
            return None;
        }
        let size = font_size * (slot / mono_advance);
        Some((Self { ranges: Vec::new(), size, slot, pad: wide_width - slot, mono }, letter, wide))
    }

    /// Each piece of a compact range on one line: its byte range and its left edge. Measured from each character's end,
    /// as gpui puts the index where a line wraps at the end of the line before.
    fn pieces(&self, text_layout: &TextLayout) -> Vec<(Range<usize>, Point<Pixels>)> {
        let mut pieces: Vec<(Range<usize>, Point<Pixels>)> = Vec::new();
        for range in &self.ranges {
            // ASCII only (see `Inline::request_layout`): one byte per character.
            for ix in range.clone() {
                let Some(end) = text_layout.position_for_index(ix + 1) else { break };
                let wide = ix == range.start || ix + 1 == range.end;
                let left = point(end.x - self.slot - if wide { self.pad } else { px(0.) }, end.y);
                match pieces.last_mut() {
                    Some((piece, start)) if piece.end == ix && piece.start >= range.start && start.y == end.y => piece.end = ix + 1,
                    _ => pieces.push((ix..ix + 1, left)),
                }
            }
        }
        pieces
    }

    /// The piece's width in the layout, and where its code starts within it.
    fn piece_extent(&self, piece: &Range<usize>, range: &Range<usize>) -> (Pixels, Pixels) {
        let count = piece.len() as f32;
        let (first, last) = (piece.start == range.start, piece.end == range.end);
        let wide = if range.len() == 1 { 1. } else { f32::from(first) + f32::from(last) };
        let lead = if range.len() == 1 { self.pad.half() } else if first { self.pad } else { px(0.) };
        (self.slot * count + self.pad * wide, lead)
    }
}

/// Filo: a closing mark kept with the code or link before it (`wrap::kept_with`). gpui 0.2 can't be told where not to
/// wrap, so the layout holds the mark's place with a letter about as wide (`stand_in`), which it never breaks before,
/// drawn transparent, and the mark is painted over it (`Inline::paint_kept`). Its byte offset is unchanged, so
/// selection and copying get the real text.
struct Kept {
    ix: usize,
    mark: char,
    font: Font,
    color: Hsla,
    /// The stand-in letter's width.
    width: Pixels,
}

/// Filo: the letter that holds `mark`'s place in a layout in `font` at `font_size` (see `Kept`), and its width: of the
/// letters gpui never breaks before, the one closest in width. Widths are taken from shaped runs, like `CompactCode`'s.
fn stand_in(mark: char, font: &Font, font_size: Pixels, window: &Window) -> Option<(char, Pixels)> {
    // Letters that kern little against punctuation, and no `f`, which some fonts join to the letter after it.
    const LETTERS: &str = "ijlInhumbdpqoecsazxgkJHNMEBDKSZOCGQU";
    thread_local! {
        static CHOSEN: RefCell<HashMap<(char, Font, u32), Option<(char, Pixels)>>> = RefCell::new(HashMap::new());
    }
    let key = (mark, font.clone(), f32::from(font_size).to_bits());
    if let Some(chosen) = CHOSEN.with(|chosen| chosen.borrow().get(&key).copied()) {
        return chosen;
    }
    const RUN: usize = 10;
    let text_system = window.text_system();
    let width = |c: char| {
        let text = c.to_string().repeat(RUN);
        let run = TextRun { len: text.len(), font: font.clone(), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
        text_system.shape_line(text.into(), font_size, &[run], None).width / RUN as f32
    };
    let target = width(mark);
    let chosen = LETTERS
        .chars()
        .map(|c| (c, width(c)))
        .filter(|(_, width)| *width > px(0.))
        .min_by(|a, b| (a.1 - target).abs().partial_cmp(&(b.1 - target).abs()).unwrap_or(std::cmp::Ordering::Equal))
        .filter(|_| target > px(0.));
    CHOSEN.with(|chosen_by_key| chosen_by_key.borrow_mut().insert(key, chosen));
    chosen
}

impl Inline {
    pub(super) fn new(
        id: impl Into<ElementId>,
        state: Arc<Mutex<InlineState>>,
        links: Vec<(Range<usize>, LinkMark)>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
    ) -> Self {
        let text = state.lock().unwrap().text.clone();
        Self {
            id: id.into(),
            links: Rc::new(links),
            highlights,
            code_ranges: Vec::new(),
            code_style: None,
            compact: None,
            underlines: Vec::new(),
            kept: Vec::new(),
            text: text.clone(),
            styled_text: StyledText::new(text),
            state,
        }
    }

    /// Filo: the byte ranges of `inline code` in this text.
    pub(super) fn code_ranges(mut self, code_ranges: Vec<Range<usize>>) -> Self {
        self.code_ranges = code_ranges;
        self
    }

    /// Filo: how this text's `inline code` looks (`TextViewStyle::inline_code`).
    pub(super) fn code_style(mut self, code_style: Option<InlineCodeStyle>) -> Self {
        self.code_style = code_style;
        self
    }

    /// Filo: a rounded chip behind each piece of `inline code`, one per line it wraps onto, a little narrower than the
    /// line is tall and a hair wider than the glyphs, like the phone's (surfaceStrong outlined with `border`; the app
    /// maps those to `secondary_active` and `border`).
    fn paint_code_chips(&self, text_layout: &TextLayout, bounds: &Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if self.code_ranges.is_empty() {
            return;
        }
        if let Some(compact) = &self.compact {
            self.paint_compact_code(compact, text_layout, false, window, cx);
        }
        let line_height = text_layout.line_height();
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let height = (font_size * 1.35).min(line_height);
        let inset = (line_height - height).half();
        let (pad, radius) = (px(1.5), px(4.));
        let (background, border) = match &self.code_style {
            Some(style) => (style.background, style.border),
            None => (cx.theme().secondary_active, Some(cx.theme().border)),
        };
        let chip = |left: Pixels, right: Pixels, top: Pixels, window: &mut Window| {
            if right <= left {
                return;
            }
            window.paint_quad(quad(
                Bounds::from_corners(point(left - pad, top + inset), point(right + pad, top + inset + height)),
                radius,
                background,
                Edges::all(px(if border.is_some() { 1. } else { 0. })),
                border.unwrap_or(background),
                BorderStyle::default(),
            ));
        };
        for range in &self.code_ranges {
            if self.compact.as_ref().is_some_and(|compact| compact.ranges.contains(range)) {
                continue;
            }
            // A chip per line the code is on (`wrap::pieces`).
            for (left, right, top) in super::wrap::pieces(&self.text, range.clone(), |ix| text_layout.position_for_index(ix), bounds.left()) {
                chip(left, right, top, window);
            }
        }
    }

    /// Filo: links' underlines, a piece under each line a link is on, where gpui 0.2 draws them (a little over half the
    /// descent below the baseline). gpui's own left a stub at the next line's start, past the text's left edge, when a
    /// line wrapped right where a link ended.
    fn paint_underlines(&self, text_layout: &TextLayout, bounds: &Bounds<Pixels>, window: &mut Window) {
        let line_height = text_layout.line_height();
        for (range, style) in &self.underlines {
            let Some(line) = text_layout.line_layout_for_index(range.start) else { continue };
            let below = (line_height - line.ascent() - line.descent()).half() + line.ascent() + line.descent() * 0.618;
            for (left, right, top) in super::wrap::pieces(&self.text, range.clone(), |ix| text_layout.position_for_index(ix), bounds.left()) {
                window.paint_underline(point(left, top + below), right - left, style);
            }
        }
    }

    /// Filo: the closing marks kept with the code or link before them, over their stand-ins (see `Kept`), on the
    /// text's baseline.
    fn paint_kept(&self, text_layout: &TextLayout, window: &mut Window, cx: &mut App) {
        if self.kept.is_empty() {
            return;
        }
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = text_layout.line_height();
        let text_system = window.text_system().clone();
        for kept in &self.kept {
            // From the stand-in's end: gpui puts the offset a line wraps at at the end of the line before.
            let Some(end) = text_layout.position_for_index(kept.ix + 1) else { continue };
            let Some(layout) = text_layout.line_layout_for_index(kept.ix) else { continue };
            let width = match text_layout.position_for_index(kept.ix) {
                Some(start) if start.y == end.y => end.x - start.x,
                _ => kept.width,
            };
            let run = TextRun { len: kept.mark.len_utf8(), font: kept.font.clone(), color: kept.color, background_color: None, underline: None, strikethrough: None };
            let line = text_system.shape_line(kept.mark.to_string().into(), font_size, &[run], None);
            // Centred in the stand-in's room, its baseline on the text's.
            let top = end.y + (layout.ascent() - layout.descent()).half() - (line.ascent - line.descent).half();
            let _ = line.paint(point(end.x - width + (width - line.width).half(), top), line_height, window, cx);
        }
    }

    /// Filo: compact code's chips (`glyphs: false`, under the text) or its code (`glyphs: true`, over the stand-ins).
    fn paint_compact_code(&self, compact: &CompactCode, text_layout: &TextLayout, glyphs: bool, window: &mut Window, cx: &mut App) {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = text_layout.line_height();
        let text_system = window.text_system().clone();
        let body = text_system.resolve_font(&text_style.font());
        // Where the text's baseline sits in its line box, as gpui centres it (font metrics count descent below zero).
        let (ascent, descent) = (text_system.ascent(body, font_size), -text_system.descent(body, font_size));
        let color = self.code_style.as_ref().and_then(|style| style.color).unwrap_or(text_style.color);
        let (background, border) = match &self.code_style {
            Some(style) => (style.background, style.border),
            None => (cx.theme().secondary_active, Some(cx.theme().border)),
        };
        for (piece, start) in compact.pieces(text_layout) {
            let Some(range) = compact.ranges.iter().find(|range| range.start <= piece.start && piece.end <= range.end) else { continue };
            let code: SharedString = self.text[piece.clone()].to_string().into();
            let run = TextRun {
                len: code.len(),
                font: compact.mono.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = text_system.shape_line(code, compact.size, &[run], None);
            // Baselines shared: the small line's box moved so its baseline lands on the text's.
            let top = start.y + (ascent - descent).half() - (line.ascent - line.descent).half();
            let (width, lead) = compact.piece_extent(&piece, range);
            if glyphs {
                let _ = line.paint(point(start.x + lead, top), line_height, window, cx);
            } else {
                let height = (compact.size * 1.6).min(line_height);
                let chip_top = top + (line_height - height).half();
                window.paint_quad(quad(
                    Bounds::new(point(start.x + px(1.), chip_top), gpui::size(width - px(2.), height)),
                    px(5.),
                    background,
                    Edges::all(px(if border.is_some() { 1. } else { 0. })),
                    border.unwrap_or(background),
                    BorderStyle::default(),
                ));
            }
        }
    }

    /// Get link at given mouse position.
    fn link_for_position(
        layout: &TextLayout,
        links: &Vec<(Range<usize>, LinkMark)>,
        position: Point<Pixels>,
    ) -> Option<LinkMark> {
        let offset = layout.index_for_position(position).ok()?;
        for (range, link) in links.iter() {
            if range.contains(&offset) {
                return Some(link.clone());
            }
        }

        None
    }

    /// Filo: which of `links` is at `position`, by its place in the list.
    fn link_index_at(layout: &TextLayout, links: &[(Range<usize>, LinkMark)], position: Point<Pixels>) -> Option<usize> {
        let offset = layout.index_for_position(position).ok()?;
        links.iter().position(|(range, _)| range.contains(&offset))
    }

    /// Paint selected bounds for debug.
    #[allow(unused)]
    fn paint_selected_bounds(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.paint_quad(gpui::PaintQuad {
            bounds,
            background: cx.theme().blue.alpha(0.01).into(),
            corner_radii: gpui::Corners::default(),
            border_color: gpui::transparent_black(),
            border_style: BorderStyle::default(),
            border_widths: gpui::Edges::all(px(0.)),
        });
    }

    fn layout_selections(
        &self,
        text_layout: &TextLayout,
        cx: &mut App,
    ) -> (bool, bool, Option<Selection>) {
        let Some(text_view_state) = GlobalState::global(cx).text_view_state() else {
            return (false, false, None);
        };

        let text_view_state = text_view_state.read(cx);
        let is_selectable = text_view_state.is_selectable();
        // Filo: the selection runs along the lines from one end to the other (text::select), not over the rectangle
        // between them.
        let Some((a, b)) = text_view_state.selection_points() else {
            return (is_selectable, false, None);
        };
        let line_height = text_layout.line_height();
        let mut selection: Option<Selection> = None;
        let mut offset = 0;
        for c in self.text.chars() {
            let next_offset = offset + c.len_utf8();
            let Some(pos) = text_layout.position_for_index(offset) else {
                offset = next_offset;
                continue;
            };

            let mut char_width = line_height.half();
            if let Some(next_pos) = text_layout.position_for_index(next_offset) {
                if next_pos.y == pos.y {
                    char_width = next_pos.x - pos.x;
                }
            }

            if super::select::char_selected(pos, char_width, line_height, a, b) {
                if selection.is_none() {
                    selection = Some((offset..offset).into());
                }
                selection.as_mut().unwrap().end = next_offset;
            }

            offset = next_offset;
        }

        (true, true, selection)
    }

    /// Paint the selection background.
    fn paint_selection(
        selection: &Selection,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut start = selection.start;
        let mut end = selection.end;
        if end < start {
            std::mem::swap(&mut start, &mut end);
        }
        let Some(start_position) = text_layout.position_for_index(start) else {
            return;
        };
        let Some(end_position) = text_layout.position_for_index(end) else {
            return;
        };

        let line_height = text_layout.line_height();
        if start_position.y == end_position.y {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        } else {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(bounds.right(), start_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));

            if end_position.y > start_position.y + line_height {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        point(bounds.left(), start_position.y + line_height),
                        point(bounds.right(), end_position.y),
                    ),
                    px(0.),
                    cx.theme().selection,
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }

            window.paint_quad(quad(
                Bounds::from_corners(
                    point(bounds.left(), end_position.y),
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }
}

impl IntoElement for Inline {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Inline {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_element_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();

        // Filo: runs break at highlight and code edges alike; code takes the mono family.
        let mut edges: Vec<usize> = vec![0, self.text.len()];
        for (range, _) in &self.highlights {
            edges.extend([range.start, range.end]);
        }
        for range in &self.code_ranges {
            edges.extend([range.start, range.end]);
        }
        // Filo: closing marks right after code or a link stay on its line (`Kept`); each is a run of its own.
        let spans: Vec<Range<usize>> = self.code_ranges.iter().cloned().chain(self.links.iter().map(|(range, _)| range.clone())).collect();
        let kept = super::wrap::kept_with(&self.text, &spans, &self.code_ranges);
        for ix in &kept {
            edges.extend([*ix, ix + 1]);
        }
        edges.retain(|edge| *edge <= self.text.len());
        edges.sort_unstable();
        edges.dedup();
        let mono = self.code_style.as_ref().and_then(|style| style.font_family.clone()).unwrap_or_else(|| cx.theme().mono_font_family.clone());
        // Filo: compact code holds its room with stand-in letters (see `CompactCode`); only plain ASCII code, so each
        // stand-in has the byte length of the character it holds.
        self.compact = None;
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        // Filo: the text gpui lays out, with stand-ins of the same byte length where compact code and kept marks go.
        let mut bytes = self.text.as_bytes().to_vec();
        if self.code_style.as_ref().is_some_and(|style| style.compact) && !self.code_ranges.is_empty() {
            let mono_font = Font { family: mono.clone(), weight: FontWeight::NORMAL, ..text_style.font() };
            if let Some((mut compact, letter, wide)) = CompactCode::measure(&text_style.font(), mono_font, font_size, window) {
                for range in &self.code_ranges {
                    let code = &self.text[range.clone()];
                    if code.is_empty() || !code.is_ascii() || code.contains('\n') {
                        continue;
                    }
                    for ix in range.clone() {
                        let ends = ix == range.start || ix + 1 == range.end;
                        bytes[ix] = if ends { wide as u8 } else { letter as u8 };
                    }
                    compact.ranges.push(range.clone());
                }
                if !compact.ranges.is_empty() {
                    self.compact = Some(compact);
                }
            }
        }
        self.underlines.clear();
        self.kept.clear();
        let mut runs = Vec::new();
        for pair in edges.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let mut style = text_style.clone();
            if let Some((_, highlight)) = self.highlights.iter().find(|(range, _)| range.start <= start && start < range.end) {
                style = style.highlight(*highlight);
            }
            if self.compact.as_ref().is_some_and(|compact| compact.ranges.iter().any(|range| range.start <= start && start < range.end)) {
                // The stand-ins: the text's own font and weight, so their widths are the measured ones, unseen.
                style = text_style.clone();
                style.color = gpui::transparent_black();
            } else if self.code_ranges.iter().any(|range| range.start <= start && start < range.end) {
                style.font_family = mono.clone();
            }
            // Filo: underlines are drawn by `paint_underlines`, a piece per line.
            if let Some(underline) = style.underline.take() {
                let underline = UnderlineStyle { color: Some(underline.color.unwrap_or(style.color)), ..underline };
                match self.underlines.last_mut() {
                    Some((range, last)) if range.end == start && *last == underline => range.end = end,
                    _ => self.underlines.push((start..end, underline)),
                }
            }
            if kept.binary_search(&start).is_ok() {
                let mark = self.text[start..].chars().next().unwrap_or(' ');
                let font = style.font();
                if let Some((letter, width)) = stand_in(mark, &font, font_size, window) {
                    // Only ASCII marks are kept, so the letter takes the mark's one byte.
                    bytes[start] = letter as u8;
                    if let Some(strikethrough) = style.strikethrough.as_mut() {
                        strikethrough.color.get_or_insert(style.color);
                    }
                    self.kept.push(Kept { ix: start, mark, font, color: style.color, width });
                    style.color = gpui::transparent_black();
                }
            }
            runs.push(style.to_run(end - start));
        }

        let layout_text = if bytes == self.text.as_bytes() {
            self.text.clone()
        } else {
            String::from_utf8(bytes).map(SharedString::from).unwrap_or_else(|_| self.text.clone())
        };
        self.styled_text = StyledText::new(layout_text).with_runs(runs);
        let (layout_id, _) =
            self.styled_text
                .request_layout(global_element_id, inspector_id, window, cx);

        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled_text
            .prepaint(id, inspector_id, bounds, &mut (), window, cx);

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let current_view = window.current_view();
        let hitbox = prepaint;
        let mut state = self.state.lock().unwrap();

        let text_layout = self.styled_text.layout().clone();
        self.paint_code_chips(&text_layout, &bounds, window, cx);
        self.styled_text
            .paint(global_id, None, bounds, &mut (), &mut (), window, cx);
        if let Some(compact) = &self.compact {
            self.paint_compact_code(compact, &text_layout, true, window, cx);
        }
        self.paint_kept(&text_layout, window, cx);
        self.paint_underlines(&text_layout, &bounds, window);

        // layout selections
        let (is_selectable, is_selection, selection) =
            self.layout_selections(&text_layout, cx);

        state.selection = selection;

        if is_selection || is_selectable {
            window.set_cursor_style(CursorStyle::IBeam, &hitbox);
        }

        // link cursor pointer
        let mouse_position = window.mouse_position();
        if let Some(_) = Self::link_for_position(&text_layout, &self.links, mouse_position) {
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
        }

        if let Some(selection) = &state.selection {
            Self::paint_selection(selection, &text_layout, &bounds, window, cx);
        }

        // Filo: repaint when the pointer moves onto or off a link (for its cursor), not on every character: 0.5.1
        // re-rendered the whole view for each character the pointer crossed.
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let text_layout = text_layout.clone();
            let links = self.links.clone();
            let state = self.state.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                let updated = if hitbox.is_hovered(window) { Self::link_index_at(&text_layout, &links, event.position) } else { None };
                let mut state = state.lock().unwrap();
                if state.hovered_index != updated {
                    state.hovered_index = updated;
                    cx.notify(current_view);
                }
            }
        });

        // Filo: a link opens when the left button goes down and comes up on it, a few points apart at most, whether or
        // not the view has a selection. 0.5.1 only listened while nothing was selected, so a click whose pointer moved
        // a pixel (which starts a selection) or a click after selecting anything in the message did nothing.
        let text_view = GlobalState::global(cx).text_view_state().cloned();
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let text_layout = text_layout.clone();
            let links = self.links.clone();
            let state = self.state.clone();
            let text = self.text.clone();
            let text_view = text_view.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if !phase.bubble() || event.button != MouseButton::Left {
                    return;
                }
                let hovered = hitbox.is_hovered(window);
                // The second press of a double-click selects instead.
                state.lock().unwrap().pressed_link = (hovered && event.click_count == 1)
                    .then(|| Self::link_for_position(&text_layout, &links, event.position))
                    .flatten()
                    .map(|link| (link.url, event.position));
                // Filo: a double-click selects the word under the pointer, a triple-click the paragraph.
                if !hovered || event.click_count < 2 || text.is_empty() {
                    return;
                }
                let Some(text_view) = &text_view else { return };
                if !text_view.read(cx).is_selectable() {
                    return;
                }
                let ix = text_layout.index_for_position(event.position).unwrap_or_else(|ix| ix).min(text.len());
                let range = if event.click_count == 2 { super::select::word_at(&text, ix) } else { 0..text.len() };
                let line_height = text_layout.line_height();
                let (Some(start), Some(end)) = (text_layout.position_for_index(range.start), text_layout.position_for_index(range.end)) else {
                    return;
                };
                let middle = |p: Point<Pixels>| point(p.x, p.y + line_height.half());
                text_view.update(cx, |text_view, _| text_view.select_between(middle(start), middle(end)));
                cx.notify(current_view);
            }
        });
        // Filo: a right-click on a link opens a menu with "Copy link" (`text_view::open_link_menu`). Only for links
        // the view would open in the browser or mail app: an @name or a commit hash keeps today's right-click.
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let text_layout = text_layout.clone();
            let links = self.links.clone();
            let text_view = text_view.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                // Where the link shows, or through an open menu's backdrop (one_menu.rs).
                let here = hitbox.is_hovered(window) || crate::one_menu::passed_through(event.position, window, cx);
                if !phase.bubble() || event.button != MouseButton::Right || !here {
                    return;
                }
                let Some(text_view) = &text_view else { return };
                let Some(link) = Self::link_for_position(&text_layout, &links, event.position) else { return };
                if !super::safety::is_safe_link(&link.url) {
                    return;
                }
                cx.stop_propagation();
                super::text_view::open_link_menu(text_view, link.url.trim().to_string().into(), event.position, window, cx);
            }
        });
        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let links = self.links.clone();
            let state = self.state.clone();
            move |event: &MouseUpEvent, phase, window, cx| {
                if !phase.bubble() || event.button != MouseButton::Left {
                    return;
                }
                let Some((url, down)) = state.lock().unwrap().pressed_link.take() else { return };
                let moved = (event.position.x - down.x).abs().max((event.position.y - down.y).abs());
                let same = Self::link_for_position(&text_layout, &links, event.position).is_some_and(|link| link.url == url);
                if !hitbox.is_hovered(window) || !same || moved > px(4.) {
                    return;
                }
                cx.stop_propagation();
                if let Some(text_view) = &text_view {
                    text_view.update(cx, |text_view, _| text_view.clear_selection());
                    cx.notify(current_view);
                }
                // Filo: only http(s) and mailto, or what the app's link handler takes (text::safety).
                super::safety::open_link(&url, window, cx);
            }
        });
    }
}

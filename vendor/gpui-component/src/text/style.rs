use std::sync::Arc;

use gpui::{FontWeight, Hsla, Pixels, Rems, SharedString, StyleRefinement, px, rems};

use crate::highlighter::HighlightTheme;

/// TextViewStyle used to customize the style for [`TextView`].
#[derive(Clone)]
pub struct TextViewStyle {
    /// Gap of each paragraphs, default is 1 rem.
    pub paragraph_gap: Rems,
    /// Base font size for headings, default is 14px.
    pub heading_base_font_size: Pixels,
    /// Function to calculate heading font size based on heading level (1-6).
    ///
    /// The first parameter is the heading level (1-6), the second parameter is the base font size.
    /// The second parameter is the base font size.
    pub heading_font_size: Option<Arc<dyn Fn(u8, Pixels) -> Pixels + Send + Sync + 'static>>,
    /// Highlight theme for code blocks. Default: [`HighlightTheme::default_light()`]
    pub highlight_theme: Arc<HighlightTheme>,
    /// The style refinement for code blocks.
    pub code_block: StyleRefinement,
    pub is_dark: bool,
    /// Filo: the text comes from somewhere we don't trust (a web page, a model): see `text::safety`.
    pub untrusted: bool,
    /// Filo: pictures at an address on the web are fetched and shown. When off, they're links instead (nothing is
    /// fetched) and raw HTML with a picture in it is shown as text; pictures on this Mac still show
    /// (`text::safety::remote_pictures_as_links`). On by default.
    pub remote_pictures: bool,
    /// Filo: the weight of **bold** text. Default: bold (700).
    pub bold_weight: Option<FontWeight>,
    /// Filo: how `inline code` looks. Default: the theme's mono font on a `secondary_active` chip outlined with `border`.
    pub inline_code: Option<InlineCodeStyle>,
    /// Filo: refines each heading (1-6) after its default size and weight, e.g. room above it or its line height. gpui
    /// replaces the heading's text style with the refinement's as a whole, so a refinement that sets any text property
    /// (line height, weight) must set the size too.
    pub heading: Option<Arc<dyn Fn(u8) -> StyleRefinement + Send + Sync + 'static>>,
    /// Filo: the gap between list items. Default: 0.25 rem.
    pub list_gap: Option<Rems>,
    /// Filo: when set, a list item's marker (a round bullet, or its number) sits in a column this wide, so wrapped lines
    /// line up under the text. Default: the marker and a space, inline.
    pub list_indent: Option<Rems>,
    /// Filo: keep `inline code` inside bold, italic or struck-through text (`**failed with `x`**`) as code. Off by
    /// default: upstream drops it and draws the code as plain styled text.
    pub code_in_styled_text: bool,
    /// Filo: commit hashes in the text (`8d40428f`, bare or as inline code) are links to `commit:<hash>`, which the
    /// app's link handler opens (`text::commits`, `text::safety::set_link_handler`). Off by default.
    pub commit_links: bool,
    /// Filo: links to `mention:` addresses (the group chat's @names, which the app's link handler opens) are drawn in
    /// this colour, semibold and not underlined, like a name rather than a link. Drawn as links when `None`, the default.
    pub mention_links: Option<Hsla>,
}

/// Filo: the chip behind `inline code` and its font.
#[derive(Clone, PartialEq)]
pub struct InlineCodeStyle {
    pub background: Hsla,
    /// No outline when `None`.
    pub border: Option<Hsla>,
    /// The theme's mono font when `None`.
    pub font_family: Option<SharedString>,
    /// The code's colour. The text's when `None`.
    pub color: Option<Hsla>,
    /// Draw the code smaller than the text around it, at about 0.8 of its size (see `Inline::compact_code`). gpui 0.2
    /// lays a paragraph out at one font size, so the code's room is held by stand-in letters of the text's font and the
    /// small code is painted over them.
    pub compact: bool,
}

impl PartialEq for TextViewStyle {
    fn eq(&self, other: &Self) -> bool {
        self.paragraph_gap == other.paragraph_gap
            && self.heading_base_font_size == other.heading_base_font_size
            && self.highlight_theme == other.highlight_theme
            && self.untrusted == other.untrusted
            && self.remote_pictures == other.remote_pictures
            && self.bold_weight == other.bold_weight
            && self.inline_code == other.inline_code
            && self.list_gap == other.list_gap
            && self.list_indent == other.list_indent
            && self.code_in_styled_text == other.code_in_styled_text
            && self.commit_links == other.commit_links
            && self.mention_links == other.mention_links
    }
}

impl Default for TextViewStyle {
    fn default() -> Self {
        Self {
            paragraph_gap: rems(1.),
            heading_base_font_size: px(14.),
            heading_font_size: None,
            highlight_theme: HighlightTheme::default_light().clone(),
            code_block: StyleRefinement::default(),
            is_dark: false,
            untrusted: false,
            remote_pictures: true,
            bold_weight: None,
            inline_code: None,
            heading: None,
            list_gap: None,
            list_indent: None,
            code_in_styled_text: false,
            commit_links: false,
            mention_links: None,
        }
    }
}

impl TextViewStyle {
    /// Set paragraph gap, default is 1 rem.
    pub fn paragraph_gap(mut self, gap: Rems) -> Self {
        self.paragraph_gap = gap;
        self
    }

    pub fn heading_font_size<F>(mut self, f: F) -> Self
    where
        F: Fn(u8, Pixels) -> Pixels + Send + Sync + 'static,
    {
        self.heading_font_size = Some(Arc::new(f));
        self
    }

    /// Filo: render the text as untrusted (`text::safety::sanitize_untrusted`).
    pub fn untrusted(mut self, untrusted: bool) -> Self {
        self.untrusted = untrusted;
        self
    }

    /// Filo: fetch pictures at web addresses (`remote_pictures`).
    pub fn remote_pictures(mut self, fetch: bool) -> Self {
        self.remote_pictures = fetch;
        self
    }

    /// Filo: the weight of **bold** text.
    pub fn bold_weight(mut self, weight: FontWeight) -> Self {
        self.bold_weight = Some(weight);
        self
    }

    /// Filo: how `inline code` looks.
    pub fn inline_code(mut self, style: InlineCodeStyle) -> Self {
        self.inline_code = Some(style);
        self
    }

    /// Filo: refines each heading after its default size and weight.
    pub fn heading<F>(mut self, f: F) -> Self
    where
        F: Fn(u8) -> StyleRefinement + Send + Sync + 'static,
    {
        self.heading = Some(Arc::new(f));
        self
    }

    /// Filo: the gap between list items.
    pub fn list_gap(mut self, gap: Rems) -> Self {
        self.list_gap = Some(gap);
        self
    }

    /// Filo: list markers in a column of this width.
    pub fn list_indent(mut self, indent: Rems) -> Self {
        self.list_indent = Some(indent);
        self
    }

    /// Filo: keep `inline code` inside bold, italic or struck-through text as code.
    pub fn code_in_styled_text(mut self, keep: bool) -> Self {
        self.code_in_styled_text = keep;
        self
    }

    /// Filo: commit hashes in the text become `commit:<hash>` links.
    pub fn commit_links(mut self, link: bool) -> Self {
        self.commit_links = link;
        self
    }

    /// Filo: `mention:` links drawn as names in `color` (see `mention_links`).
    pub fn mention_links(mut self, color: Hsla) -> Self {
        self.mention_links = Some(color);
        self
    }

    /// Set style for code blocks.
    pub fn code_block(mut self, style: StyleRefinement) -> Self {
        self.code_block = style;
        self
    }
}

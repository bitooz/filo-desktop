use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, Mutex},
};

use gpui::{
    AnyElement, App, DefiniteLength, Div, Element, ElementId, FontStyle, FontWeight, Half,
    HighlightStyle, InteractiveElement as _, IntoElement, ListState, ObjectFit,
    ParentElement, Pixels, ScrollHandle, SharedString, SharedUri, StatefulInteractiveElement, Styled,
    StyledImage as _, TextRun, Window, div, img, prelude::FluentBuilder as _, px, relative, rems,
};
use markdown::mdast;
use ropey::Rope;

use crate::{
    ActiveTheme as _, Icon, IconName, StyledExt, h_flex,
    scroll::ScrollableElement as _,
    highlighter::{HighlightTheme, SyntaxHighlighter},
    text::{
        CodeBlockActionsFn,
        inline::{Inline, InlineState},
    },
    tooltip::Tooltip,
    v_flex,
};

use super::{TextViewStyle, utils::list_item_prefix};

#[allow(unused)]
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LinkMark {
    pub url: SharedString,
    /// Optional identifier for footnotes.
    pub identifier: Option<SharedString>,
    pub title: Option<SharedString>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TextMark {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub code: bool,
    pub link: Option<LinkMark>,
}

impl TextMark {
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }

    pub fn link(mut self, link: impl Into<LinkMark>) -> Self {
        self.link = Some(link.into());
        self
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl From<Span> for ElementId {
    fn from(value: Span) -> Self {
        ElementId::Name(format!("md-{}:{}", value.start, value.end).into())
    }
}

#[allow(unused)]
#[derive(Debug, Default, Clone)]
pub struct ImageNode {
    pub url: SharedUri,
    pub link: Option<LinkMark>,
    pub title: Option<SharedString>,
    pub alt: Option<SharedString>,
    pub width: Option<DefiniteLength>,
    pub height: Option<DefiniteLength>,
}

impl ImageNode {
    pub fn title(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| self.alt.clone().unwrap_or_default())
            .to_string()
    }
}

impl PartialEq for ImageNode {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
            && self.link == other.link
            && self.title == other.title
            && self.alt == other.alt
            && self.width == other.width
            && self.height == other.height
    }
}

#[derive(Default, Clone, Debug)]
pub(crate) struct InlineNode {
    /// The text content.
    pub(crate) text: SharedString,
    pub(crate) image: Option<ImageNode>,
    /// The text styles, each tuple contains the range of the text and the style.
    pub(crate) marks: Vec<(Range<usize>, TextMark)>,

    state: Arc<Mutex<InlineState>>,
}

impl PartialEq for InlineNode {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.image == other.image && self.marks == other.marks
    }
}

impl InlineNode {
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            image: None,
            marks: vec![],
            state: Arc::new(Mutex::new(InlineState::default())),
        }
    }

    pub(crate) fn image(image: ImageNode) -> Self {
        let mut this = Self::new("");
        this.image = Some(image);
        this
    }

    pub(crate) fn marks(mut self, marks: Vec<(Range<usize>, TextMark)>) -> Self {
        self.marks = marks;
        self
    }
}

/// The paragraph element, contains multiple text nodes.
///
/// Unlike other Element, this is cloneable, because it is used in the Node AST.
/// We are keep the selection state inside this AST Nodes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Paragraph {
    pub(super) span: Option<Span>,
    pub(super) children: Vec<InlineNode>,
    /// The link references in this paragraph, used for reference links.
    ///
    /// The key is the identifier, the value is the url.
    pub(super) link_refs: HashMap<SharedString, SharedString>,

    pub(crate) state: Arc<Mutex<InlineState>>,
}

impl PartialEq for Paragraph {
    fn eq(&self, other: &Self) -> bool {
        self.span == other.span
            && self.children == other.children
            && self.link_refs == other.link_refs
    }
}

impl Paragraph {
    pub(crate) fn new(text: String) -> Self {
        Self {
            span: None,
            children: vec![InlineNode::new(&text)],
            link_refs: HashMap::new(),
            state: Arc::new(Mutex::new(InlineState::default())),
        }
    }

    pub(super) fn selected_text(&self) -> String {
        let mut text = String::new();

        for c in self.children.iter() {
            let state = c.state.lock().unwrap();
            if let Some(selection) = &state.selection {
                let part_text = state.text.clone();
                text.push_str(&part_text[selection.start..selection.end]);
            }
        }

        let state = self.state.lock().unwrap();
        if let Some(selection) = &state.selection {
            let all_text = state.text.clone();
            text.push_str(&all_text[selection.start..selection.end]);
        }

        text
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Table {
    pub children: Vec<TableRow>,
    pub column_aligns: Vec<ColumnumnAlign>,
}

impl Table {
    pub(crate) fn column_align(&self, index: usize) -> ColumnumnAlign {
        self.column_aligns.get(index).copied().unwrap_or_default()
    }
}

/// Filo: how narrow and how wide a table column wants to be (`Node::render_table`).
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub(crate) struct TableColumn {
    /// Its longest word, so words aren't broken unless they're longer than the cap.
    pub min: Pixels,
    /// Its longest cell on one line.
    pub max: Pixels,
    /// Where it starts before the table's spare room is shared out: a short column (labels, numbers) starts on one line
    /// and only wraps when the table can't fit otherwise; a long one starts at its narrowest and takes most of the room.
    pub basis: Pixels,
}

/// Filo: each column's width range from its cells' (longest word, whole text) widths. `floor` keeps empty or tiny
/// columns clickable, `cap` stops one long word (a URL, a path) from making its column wide, and `slack` covers bold and
/// code, which are measured in the body font.
pub(crate) fn table_columns(
    rows: &[Vec<(Pixels, Pixels)>],
    floor: Pixels,
    cap: Pixels,
    slack: f32,
) -> Vec<TableColumn> {
    let count = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    (0..count)
        .map(|ix| {
            let (word, whole) = rows
                .iter()
                .filter_map(|row| row.get(ix))
                .fold((px(0.), px(0.)), |(word, whole), &(w, t)| {
                    (if w > word { w } else { word }, if t > whole { t } else { whole })
                });
            let mut min = word * slack + px(2.);
            if min < floor {
                min = floor;
            }
            if min > cap {
                min = cap;
            }
            let mut max = whole * slack + px(2.);
            if max < min {
                max = min;
            }
            let basis = if max <= cap { max } else { min };
            TableColumn { min, max, basis }
        })
        .collect()
}

/// Filo: the widest whitespace-separated word in `text`, given where each byte index sits on its shaped line.
pub(crate) fn longest_word(text: &str, x_for_index: impl Fn(usize) -> Pixels) -> Pixels {
    let mut longest = px(0.);
    let mut start = None;
    for (ix, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if c.is_whitespace() {
            if let Some(start) = start.take() {
                let width = x_for_index(ix) - x_for_index(start);
                if width > longest {
                    longest = width;
                }
            }
        } else if start.is_none() {
            start = Some(ix);
        }
    }
    longest
}

#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub(crate) enum ColumnumnAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl From<mdast::AlignKind> for ColumnumnAlign {
    fn from(value: mdast::AlignKind) -> Self {
        match value {
            mdast::AlignKind::None => ColumnumnAlign::Left,
            mdast::AlignKind::Left => ColumnumnAlign::Left,
            mdast::AlignKind::Center => ColumnumnAlign::Center,
            mdast::AlignKind::Right => ColumnumnAlign::Right,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TableRow {
    pub children: Vec<TableCell>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TableCell {
    pub children: Paragraph,
    pub width: Option<DefiniteLength>,
}

impl Paragraph {
    pub(crate) fn take(&mut self) -> Paragraph {
        std::mem::replace(
            self,
            Paragraph {
                span: None,
                children: vec![],
                link_refs: Default::default(),
                state: Arc::new(Mutex::new(InlineState::default())),
            },
        )
    }

    pub(crate) fn is_image(&self) -> bool {
        false
    }

    pub(crate) fn set_span(&mut self, span: Span) {
        self.span = Some(span);
    }

    pub(crate) fn push_str(&mut self, text: &str) {
        self.children.push(
            InlineNode::new(text.to_string()).marks(vec![(0..text.len(), TextMark::default())]),
        );
    }

    pub(crate) fn push(&mut self, text: InlineNode) {
        self.children.push(text);
    }

    pub(crate) fn push_image(&mut self, image: ImageNode) {
        self.children.push(InlineNode::image(image));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.children.is_empty()
            || self
                .children
                .iter()
                .all(|node| node.text.is_empty() && node.image.is_none())
    }

    /// Filo: the text of its inline nodes, as one string (table column sizing).
    pub(crate) fn plain_text(&self) -> String {
        self.children.iter().map(|node| node.text.as_str()).collect()
    }

    /// Return length of children text.
    pub(crate) fn text_len(&self) -> usize {
        self.children
            .iter()
            .map(|node| node.text.len())
            .sum::<usize>()
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.children.extend(other.children);
    }
}

#[derive(Debug, Clone)]
pub struct CodeBlock {
    lang: Option<SharedString>,
    styles: Vec<(Range<usize>, HighlightStyle)>,
    state: Arc<Mutex<InlineState>>,
}

impl PartialEq for CodeBlock {
    fn eq(&self, other: &Self) -> bool {
        self.lang == other.lang && self.styles == other.styles
    }
}

impl CodeBlock {
    /// Get the language of the code block.
    pub fn lang(&self) -> Option<SharedString> {
        self.lang.clone()
    }

    /// Get the code content of the code block.
    pub fn code(&self) -> SharedString {
        self.state.lock().unwrap().text.clone()
    }

    pub(crate) fn new(
        code: SharedString,
        lang: Option<SharedString>,
        _: &TextViewStyle,
        highlight_theme: &HighlightTheme,
    ) -> Self {
        let mut styles = vec![];
        if let Some(lang) = &lang {
            let mut highlighter = SyntaxHighlighter::new(&lang);
            highlighter.update(None, &Rope::from_str(code.as_str()));
            styles = highlighter.styles(&(0..code.len()), highlight_theme);
        };

        let state = Arc::new(Mutex::new(InlineState::default()));
        state.lock().unwrap().set_text(code);

        Self {
            lang,
            styles,
            state,
        }
    }

    pub(super) fn selected_text(&self) -> String {
        let mut text = String::new();
        let state = self.state.lock().unwrap();
        if let Some(selection) = &state.selection {
            let part_text = state.text.clone();
            text.push_str(&part_text[selection.start..selection.end]);
        }
        text
    }

    fn render(
        &self,
        options: &NodeRenderOptions,
        node_cx: &NodeContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let style = &node_cx.style;

        div()
            .when(!options.is_last, |this| this.pb(style.paragraph_gap))
            .child(
                div()
                    .id("codeblock")
                    .p_3()
                    .rounded(cx.theme().radius)
                    // Filo: `secondary_active` is surfaceStrong, outlined with `border`, so the box shows on a bubble
                    // (`muted` is the bubble's own colour).
                    .bg(cx.theme().secondary_active)
                    .border_1()
                    .border_color(cx.theme().border)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .relative()
                    .refine_style(&style.code_block)
                    .child(Inline::new(
                        "code",
                        self.state.clone(),
                        vec![],
                        self.styles.clone(),
                    ))
                    .when_some(node_cx.code_block_actions.clone(), |this, actions| {
                        this.child(
                            div()
                                .absolute()
                                .top_2()
                                .right_2()
                                .bg(cx.theme().muted)
                                .rounded(cx.theme().radius)
                                .child(actions(&self, window, cx)),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// A context for rendering nodes, contains link references.
#[derive(Default, Clone)]
pub(crate) struct NodeContext {
    pub(crate) link_refs: HashMap<SharedString, LinkMark>,
    pub(crate) style: TextViewStyle,
    pub(crate) code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

impl NodeContext {
    pub(super) fn add_ref(&mut self, identifier: SharedString, link: LinkMark) {
        self.link_refs.insert(identifier, link);
    }
}

impl PartialEq for NodeContext {
    fn eq(&self, other: &Self) -> bool {
        self.link_refs == other.link_refs && self.style == other.style
        // Note: code_block_buttons is intentionally not compared (closures can't be compared)
    }
}

/// The AST Node of the rich text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Root {
        children: Vec<Node>,
    },
    Paragraph(Paragraph),
    Heading {
        level: u8,
        children: Paragraph,
    },
    Blockquote {
        children: Vec<Node>,
    },
    List {
        /// Only contains ListItem, others will be ignored
        children: Vec<Node>,
        ordered: bool,
    },
    ListItem {
        children: Vec<Node>,
        spread: bool,
        /// Whether the list item is checked, if None, it's not a checkbox
        checked: Option<bool>,
    },
    CodeBlock(CodeBlock),
    Table(Table),
    Break {
        html: bool,
    },
    Divider,
    /// Use for to_markdown get raw definition
    Definition {
        identifier: SharedString,
        url: SharedString,
        title: Option<SharedString>,
    },
    Unknown,
}

impl Node {
    pub(super) fn is_list_item(&self) -> bool {
        matches!(self, Self::ListItem { .. })
    }

    pub(super) fn is_break(&self) -> bool {
        matches!(self, Self::Break { .. })
    }

    /// Combine all children, omitting the empt parent nodes.
    pub(super) fn compact(self) -> Node {
        match self {
            Self::Root { mut children } if children.len() == 1 => children.remove(0).compact(),
            _ => self,
        }
    }

    pub(super) fn selected_text(&self) -> String {
        let mut text = String::new();
        match self {
            Node::Root { children } => {
                let mut block_text = String::new();
                for c in children.iter() {
                    block_text.push_str(&c.selected_text());
                }
                if !block_text.is_empty() {
                    text.push_str(&block_text);
                    text.push('\n');
                }
            }
            Node::Paragraph(paragraph) => {
                let mut block_text = String::new();
                block_text.push_str(&paragraph.selected_text());
                if !block_text.is_empty() {
                    text.push_str(&block_text);
                    text.push('\n');
                }
            }
            Node::Heading { children, .. } => {
                let mut block_text = String::new();
                block_text.push_str(&children.selected_text());
                if !block_text.is_empty() {
                    text.push_str(&block_text);
                    text.push('\n');
                }
            }
            Node::List { children, .. } => {
                for c in children.iter() {
                    text.push_str(&c.selected_text());
                }
            }
            Node::ListItem { children, .. } => {
                for c in children.iter() {
                    text.push_str(&c.selected_text());
                }
            }
            Node::Blockquote { children } => {
                let mut block_text = String::new();
                for c in children.iter() {
                    block_text.push_str(&c.selected_text());
                }

                if !block_text.is_empty() {
                    text.push_str(&block_text);
                    text.push('\n');
                }
            }
            Node::Table(table) => {
                let mut block_text = String::new();
                for row in table.children.iter() {
                    let mut row_texts = vec![];
                    for cell in row.children.iter() {
                        row_texts.push(cell.children.selected_text());
                    }
                    if !row_texts.is_empty() {
                        block_text.push_str(&row_texts.join(" "));
                        block_text.push('\n');
                    }
                }

                if !block_text.is_empty() {
                    text.push_str(&block_text);
                    text.push('\n');
                }
            }
            Node::CodeBlock(code_block) => {
                let block_text = code_block.selected_text();
                if !block_text.is_empty() {
                    text.push_str(&block_text);
                    text.push('\n');
                }
            }
            Node::Definition { .. } | Node::Break { .. } | Node::Divider | Node::Unknown => {}
        }

        text
    }
}

impl Paragraph {
    fn render(
        &self,
        node_cx: &NodeContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let span = self.span;
        let children = &self.children;

        let mut child_nodes: Vec<AnyElement> = vec![];

        let mut text = String::new();
        let mut highlights: Vec<(Range<usize>, HighlightStyle)> = vec![];
        let mut links: Vec<(Range<usize>, LinkMark)> = vec![];
        // Filo: inline code is drawn by `Inline` (mono font and a chip), not as a run background.
        let mut code_ranges: Vec<Range<usize>> = vec![];
        let mut offset = 0;

        let mut ix = 0;
        for inline_node in children {
            let text_len = inline_node.text.len();
            text.push_str(&inline_node.text);

            if let Some(image) = &inline_node.image {
                if text.len() > 0 {
                    inline_node
                        .state
                        .lock()
                        .unwrap()
                        .set_text(text.clone().into());
                    child_nodes.push(
                        Inline::new(
                            ix,
                            inline_node.state.clone(),
                            links.clone(),
                            highlights.clone(),
                        )
                        .code_ranges(code_ranges.clone())
                        .code_style(node_cx.style.inline_code.clone())
                        .into_any_element(),
                    );
                }
                child_nodes.push(
                    img(image.url.clone())
                        .id(ix)
                        .object_fit(ObjectFit::Contain)
                        .max_w(relative(1.))
                        .when_some(image.width, |this, width| this.w(width))
                        .when_some(image.link.clone(), |this, link| {
                            let title = image.title();
                            this.cursor_pointer()
                                .tooltip(move |window, cx| {
                                    Tooltip::new(title.clone()).build(window, cx)
                                })
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    // Filo: only http(s) and mailto, or what the app's link handler takes (text::safety).
                                    super::safety::open_link(&link.url, window, cx);
                                })
                        })
                        .into_any_element(),
                );

                text.clear();
                links.clear();
                highlights.clear();
                code_ranges.clear();
                offset = 0;
            } else {
                let mut node_highlights = vec![];
                for (range, style) in &inline_node.marks {
                    let inner_range = (offset + range.start)..(offset + range.end);

                    let mut highlight = HighlightStyle::default();
                    if style.bold {
                        // Filo: the style can ask for a lighter bold.
                        highlight.font_weight = Some(node_cx.style.bold_weight.unwrap_or(FontWeight::BOLD));
                    }
                    if style.italic {
                        highlight.font_style = Some(FontStyle::Italic);
                    }
                    if style.strikethrough {
                        highlight.strikethrough = Some(gpui::StrikethroughStyle {
                            thickness: gpui::px(1.),
                            ..Default::default()
                        });
                    }
                    if style.code {
                        code_ranges.push(inner_range.clone());
                    }

                    if let Some(mut link_mark) = style.link.clone() {
                        // Filo: an @name in the group chat looks like a name (`TextViewStyle::mention_links`).
                        match node_cx.style.mention_links.filter(|_| link_mark.url.starts_with("mention:")) {
                            Some(color) => {
                                highlight.color = Some(color);
                                // Bold around it stays bold.
                                highlight.font_weight.get_or_insert(FontWeight::SEMIBOLD);
                            }
                            None => {
                                highlight.color = Some(cx.theme().link);
                                highlight.underline = Some(gpui::UnderlineStyle {
                                    thickness: gpui::px(1.),
                                    ..Default::default()
                                });
                            }
                        }

                        // convert link references, replace link
                        if let Some(identifier) = link_mark.identifier.as_ref() {
                            if let Some(mark) = node_cx.link_refs.get(identifier) {
                                link_mark = mark.clone();
                            }
                        }

                        links.push((inner_range.clone(), link_mark));
                    }

                    node_highlights.push((inner_range, highlight));
                }

                highlights = gpui::combine_highlights(highlights, node_highlights).collect();
                offset += text_len;
            }
            ix += 1;
        }

        // Add the last text node
        if text.len() > 0 {
            self.state.lock().unwrap().set_text(text.into());
            child_nodes.push(
                Inline::new(ix, self.state.clone(), links, highlights)
                    .code_ranges(code_ranges)
                    .code_style(node_cx.style.inline_code.clone())
                    .into_any_element(),
            );
        }

        div().id(span.unwrap_or_default()).children(child_nodes)
    }
}

#[derive(Default, Clone, Copy)]
struct NodeRenderOptions {
    in_list: bool,
    todo: bool,
    ordered: bool,
    depth: usize,
    is_last: bool,
}

impl NodeRenderOptions {
    fn is_last(mut self, is_last: bool) -> Self {
        self.is_last = is_last;
        self
    }
}

impl Paragraph {
    fn to_markdown(&self) -> String {
        let mut text = self
            .children
            .iter()
            .map(|text_node| {
                let mut text = text_node.text.to_string();
                for (range, style) in &text_node.marks {
                    if style.bold {
                        text = format!("**{}**", &text_node.text[range.clone()]);
                    }
                    if style.italic {
                        text = format!("*{}*", &text_node.text[range.clone()]);
                    }
                    if style.strikethrough {
                        text = format!("~~{}~~", &text_node.text[range.clone()]);
                    }
                    if style.code {
                        text = format!("`{}`", &text_node.text[range.clone()]);
                    }
                    if let Some(link) = &style.link {
                        text = format!("[{}]({})", &text_node.text[range.clone()], link.url);
                    }
                }

                if let Some(image) = &text_node.image {
                    let alt = image.alt.clone().unwrap_or_default();
                    let title = image
                        .title
                        .clone()
                        .map_or(String::new(), |t| format!(" \"{}\"", t));
                    text.push_str(&format!("![{}]({}{})", alt, image.url, title))
                }

                text
            })
            .collect::<Vec<_>>()
            .join("");

        text.push_str("\n\n");
        text
    }
}

impl Node {
    /// Converts the node to markdown format.
    ///
    /// This is used to generate markdown for test.
    #[allow(dead_code)]
    pub(crate) fn to_markdown(&self) -> String {
        match self {
            Node::Root { children } => children
                .iter()
                .map(|child| child.to_markdown())
                .collect::<Vec<_>>()
                .join("\n\n"),
            Node::Paragraph(paragraph) => paragraph.to_markdown(),
            Node::Heading { level, children } => {
                let hashes = "#".repeat(*level as usize);
                format!("{} {}", hashes, children.to_markdown())
            }
            Node::Blockquote { children } => {
                let content = children
                    .iter()
                    .map(|child| child.to_markdown())
                    .collect::<Vec<_>>()
                    .join("\n\n");

                content
                    .lines()
                    .map(|line| format!("> {}", line))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            Node::List { children, ordered } => children
                .iter()
                .enumerate()
                .map(|(i, child)| {
                    let prefix = if *ordered {
                        format!("{}. ", i + 1)
                    } else {
                        "- ".to_string()
                    };
                    format!("{}{}", prefix, child.to_markdown())
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Node::ListItem {
                children, checked, ..
            } => {
                let checkbox = if let Some(checked) = checked {
                    if *checked { "[x] " } else { "[ ] " }
                } else {
                    ""
                };
                format!(
                    "{}{}",
                    checkbox,
                    children
                        .iter()
                        .map(|child| child.to_markdown())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            }
            Node::CodeBlock(code_block) => {
                format!(
                    "```{}\n{}\n```",
                    code_block.lang.clone().unwrap_or_default(),
                    code_block.code()
                )
            }
            Node::Table(table) => {
                let header = table
                    .children
                    .first()
                    .map(|row| {
                        row.children
                            .iter()
                            .map(|cell| cell.children.to_markdown())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .unwrap_or_default();
                let alignments = table
                    .column_aligns
                    .iter()
                    .map(|align| {
                        match align {
                            ColumnumnAlign::Left => ":--",
                            ColumnumnAlign::Center => ":-:",
                            ColumnumnAlign::Right => "--:",
                        }
                        .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                let rows = table
                    .children
                    .iter()
                    .skip(1)
                    .map(|row| {
                        row.children
                            .iter()
                            .map(|cell| cell.children.to_markdown())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}\n{}\n{}", header, alignments, rows)
            }
            Node::Break { html } => {
                if *html {
                    "<br>".to_string()
                } else {
                    "\n".to_string()
                }
            }
            Node::Divider => "---".to_string(),
            Node::Definition {
                identifier,
                url,
                title,
            } => {
                if let Some(title) = title {
                    format!("[{}]: {} \"{}\"", identifier, url, title)
                } else {
                    format!("[{}]: {}", identifier, url)
                }
            }
            Node::Unknown => "".to_string(),
        }
        .trim()
        .to_string()
    }
}

impl Node {
    fn render_list_item(
        item: &Node,
        ix: usize,
        options: NodeRenderOptions,
        node_cx: &NodeContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        match item {
            Node::ListItem {
                children,
                spread,
                checked,
            } => v_flex()
                .id("li")
                .when(*spread, |this| this.child(div()))
                .children({
                    let mut items: Vec<Div> = Vec::with_capacity(children.len());

                    for (child_ix, child) in children.iter().enumerate() {
                        match child {
                            Node::Paragraph(_) => {
                                let last_not_list = child_ix > 0
                                    && !matches!(children[child_ix - 1], Node::List { .. });

                                let text = child.render_block(
                                    NodeRenderOptions {
                                        depth: options.depth + 1,
                                        todo: checked.is_some(),
                                        is_last: true,
                                        ..options
                                    },
                                    node_cx,
                                    window,
                                    cx,
                                );

                                // merge content into last item.
                                if last_not_list {
                                    if let Some(item_item) = items.last_mut() {
                                        item_item.extend(vec![
                                            div().w_full().overflow_hidden().child(text).into_any_element(),
                                        ]);
                                        continue;
                                    }
                                }

                                items.push(
                                    h_flex()
                                        .flex_1()
                                        .relative()
                                        .items_start()
                                        .content_start()
                                        .when(!options.todo && checked.is_none(), |this| {
                                            match node_cx.style.list_indent {
                                                // Filo: the marker in its own column, a round bullet (an open one
                                                // when nested) or the number, so wrapped lines start under the text.
                                                Some(indent) => this.child(if options.ordered {
                                                    // Numbers end at the same place, a little before the text.
                                                    div()
                                                        .flex_none()
                                                        .w(indent)
                                                        .pr(rems(indent.0 * 0.25))
                                                        .text_right()
                                                        .child(list_item_prefix(ix, true, options.depth).trim_end().to_string())
                                                } else {
                                                    div()
                                                        .flex_none()
                                                        .w(indent)
                                                        .pl(rems(indent.0 * 0.35))
                                                        .child(if options.depth == 0 { "•" } else { "◦" })
                                                }),
                                                None => this.child(list_item_prefix(
                                                    ix,
                                                    options.ordered,
                                                    options.depth,
                                                )),
                                            }
                                        })
                                        .when_some(*checked, |this, checked| {
                                            // Todo list checkbox
                                            this.child(
                                                div()
                                                    .flex()
                                                    .mt(rems(0.4))
                                                    .mr_1p5()
                                                    .size(rems(0.875))
                                                    .items_center()
                                                    .justify_center()
                                                    .rounded(cx.theme().radius.half())
                                                    .border_1()
                                                    .border_color(cx.theme().primary)
                                                    .text_color(cx.theme().primary_foreground)
                                                    .when(checked, |this| {
                                                        this.bg(cx.theme().primary).child(
                                                            Icon::new(IconName::Check)
                                                                .size_2()
                                                                .text_xs(),
                                                        )
                                                    }),
                                            )
                                        })
                                        // Filo: the text takes the rest of the row. Sized by its own unwrapped
                                        // width instead, a short one-line item measured wrong and overlapped others.
                                        .child(div().flex_1().min_w_0().overflow_hidden().child(text)),
                                );
                            }
                            Node::List { .. } => {
                                items.push(div().ml(rems(1.)).child(child.render_block(
                                    NodeRenderOptions {
                                        depth: options.depth + 1,
                                        todo: checked.is_some(),
                                        is_last: true,
                                        ..options
                                    },
                                    node_cx,
                                    window,
                                    cx,
                                )));
                            }
                            _ => {}
                        }
                    }
                    items
                })
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// Filo: columns sized the way browsers size an auto-layout table. Each column is at least as wide as its longest
    /// word (capped, so a long URL or path wraps inside its cell instead of widening the table) and grows towards its
    /// longest cell in proportion to how much more room that would take. Cells wrap, so a row is as tall as its tallest
    /// cell. When even the narrowest columns don't fit, the table scrolls sideways instead of clipping text.
    /// (0.5.1 gave every cell `relative(characters)` of the width, a multiple of the table's own, and truncated it to
    /// one line, so long cells were cut off.)
    fn render_table(
        item: &Node,
        node_cx: &NodeContext,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let Node::Table(table) = item else {
            return div().into_any_element();
        };

        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let font = text_style.font();
        let text_system = window.text_system().clone();
        let measure = |text: &str| -> (Pixels, Pixels) {
            // A `<br>` comes through as a newline; shaped lines are single lines.
            let text: SharedString = text.replace('\n', " ").into();
            if text.trim().is_empty() {
                return (px(0.), px(0.));
            }
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: text_style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = text_system.shape_line(text.clone(), font_size, &[run], None);
            (longest_word(&text, |ix| line.x_for_index(ix)), line.width)
        };
        let measured: Vec<Vec<(Pixels, Pixels)>> = table
            .children
            .iter()
            .map(|row| {
                row.children
                    .iter()
                    .map(|cell| measure(&cell.children.plain_text()))
                    .collect()
            })
            .collect();
        // Bold, italics and code chips are measured in the body font: a little slack keeps their words whole.
        const SLACK: f32 = 1.06;
        let columns = table_columns(&measured, font_size * 2., font_size * 10., SLACK);

        let padding_x = rems(0.5).to_pixels(window.rem_size());
        let border = px(1.);
        let outer = |width: Pixels| width + padding_x * 2. + border;
        let min_width = columns
            .iter()
            .map(|column| outer(column.min))
            .fold(px(0.), |sum, width| sum + width);
        // Each table keeps its own sideways scroll: its first cell's place in the source tells it apart from others.
        let id = table
            .children
            .iter()
            .flat_map(|row| row.children.iter())
            .find_map(|cell| cell.children.span)
            .map(|span| ElementId::NamedInteger("table".into(), span.start as u64))
            .unwrap_or_else(|| "table".into());
        let row_count = table.children.len();

        let scroll_handle = window
            .use_keyed_state(id.clone(), cx, |_, _| ScrollHandle::default())
            .read(cx)
            .clone();
        let table_scroll = scroll_handle.clone();
        let mut scroller = div()
            .id(id)
            .w_full()
            .overflow_x_scroll()
            .track_scroll(&scroll_handle)
            // A sideways swipe over a table that scrolls is the table's: a page that only scrolls vertically turns a
            // sideways swipe into a vertical one, so it would scroll as well. (This runs after the table has scrolled.)
            .on_scroll_wheel(move |event, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if delta.x.abs() > delta.y.abs() && table_scroll.max_offset().width > px(0.) {
                    cx.stop_propagation();
                }
            });
        // A vertical swipe over the table scrolls the page, not the table.
        scroller.style().restrict_scroll_to_axis = Some(true);

        div().pb(rems(1.)).w_full().child(
            div()
                .relative()
                .w_full()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(scroller.child(
                    div()
                        .w_full()
                        .min_w(min_width)
                        .flex()
                        .flex_col()
                        .children(table.children.iter().enumerate().map(|(row_ix, row)| {
                            let cell_count = row.children.len();
                            div()
                                .id(("row", row_ix))
                                .w_full()
                                .flex()
                                .flex_row()
                                .when(row_ix + 1 < row_count, |this| this.border_b_1())
                                .border_color(cx.theme().border)
                                .children(row.children.iter().enumerate().map(|(ix, cell)| {
                                    let column = columns.get(ix).copied().unwrap_or_default();
                                    let align = table.column_align(ix);
                                    // Short columns shrink (towards their longest word) before anything scrolls; spare room goes
                                    // where it lets long cells take fewer lines.
                                    let mut cell_div = div()
                                        .id(("cell", ix))
                                        .flex_basis(outer(column.basis))
                                        .min_w(outer(column.min));
                                    cell_div.style().flex_grow = Some(f32::from(column.max - column.basis) + 1.);
                                    // The text is a block child, not a flex item: as a flex item gpui measured it on one
                                    // line and then wrapped it narrower, so rows overlapped. A centred or right column moves
                                    // the text's box, no wider than its one line, rather than aligning the text in it: gpui
                                    // 0.2 places selection, clicks and link underlines as if text were left-aligned.
                                    let content = cell.children.render(node_cx, window, cx);
                                    let width = measured
                                        .get(row_ix)
                                        .and_then(|row| row.get(ix))
                                        .map(|(_, whole)| *whole * SLACK + px(2.))
                                        .unwrap_or_default();
                                    let content = match align {
                                        ColumnumnAlign::Center if width > px(2.) => {
                                            div().max_w(width).mx_auto().child(content).into_any_element()
                                        }
                                        ColumnumnAlign::Right if width > px(2.) => {
                                            div().max_w(width).ml_auto().child(content).into_any_element()
                                        }
                                        _ => content.into_any_element(),
                                    };
                                    cell_div
                                        .px(padding_x)
                                        .py_1()
                                        .when(ix + 1 < cell_count, |this| {
                                            this.border_r_1().border_color(cx.theme().border)
                                        })
                                        .child(content)
                                }))
                        })),
                ))
                // Shows only when the table is wider than the page.
                .horizontal_scrollbar(&scroll_handle),
        )
        .into_any_element()
    }

    pub(super) fn render_root(
        &self,
        list_state: Option<ListState>,
        node_cx: &NodeContext,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let options = NodeRenderOptions {
            is_last: true,
            ..Default::default()
        };

        let Some(list_state) = list_state else {
            return self
                .render_block(options, node_cx, window, cx)
                .into_any_element();
        };

        let children = match self {
            Node::Root { children } => children,
            _ => return div().into_any_element(),
        };

        let children = children.clone();
        let node_cx = node_cx.clone();

        if list_state.item_count() != children.len() {
            list_state.reset(children.len());
        }

        gpui::list(list_state, move |ix, window, cx| {
            let is_last = ix + 1 == children.len();
            children[ix]
                .render_block(options.is_last(is_last), &node_cx, window, cx)
                .into_any_element()
        })
        .size_full()
        .into_any()
    }

    fn render_block(
        &self,
        options: NodeRenderOptions,
        node_cx: &NodeContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let mb = if options.in_list || options.is_last {
            rems(0.)
        } else {
            node_cx.style.paragraph_gap
        };

        match self {
            // Filo: only the last block is last. Every block was passed `is_last`, so none got a gap below it.
            Node::Root { children } => div()
                .id("div")
                .children({
                    let count = children.len();
                    children.into_iter().enumerate().map(move |(index, node)| {
                        node.render_block(options.is_last(index + 1 == count), node_cx, window, cx)
                    })
                })
                .into_any_element(),
            Node::Paragraph(paragraph) => div()
                .id("p")
                .pb(mb)
                .child(paragraph.render(node_cx, window, cx))
                .into_any_element(),
            Node::Heading { level, children } => {
                let (text_size, font_weight) = match level {
                    1 => (rems(2.), FontWeight::BOLD),
                    2 => (rems(1.5), FontWeight::SEMIBOLD),
                    3 => (rems(1.25), FontWeight::SEMIBOLD),
                    4 => (rems(1.125), FontWeight::SEMIBOLD),
                    5 => (rems(1.), FontWeight::SEMIBOLD),
                    6 => (rems(1.), FontWeight::MEDIUM),
                    _ => (rems(1.), FontWeight::NORMAL),
                };

                let mut text_size = text_size.to_pixels(node_cx.style.heading_base_font_size);
                if let Some(f) = node_cx.style.heading_font_size.as_ref() {
                    text_size = (f)(*level, node_cx.style.heading_base_font_size);
                }

                h_flex()
                    .id(("h", *level as usize))
                    .pb(rems(0.3))
                    .whitespace_normal()
                    .text_size(text_size)
                    .font_weight(font_weight)
                    // Filo: the app's own heading spacing and line height, when it sets them.
                    .when_some(node_cx.style.heading.as_ref(), |this, f| this.refine_style(&f(*level)))
                    .child(children.render(node_cx, window, cx))
                    .into_any_element()
            }
            Node::Blockquote { children } => div()
                .w_full()
                .pb(mb)
                .child(
                    div()
                        .id("blockquote")
                        .w_full()
                        .text_color(cx.theme().muted_foreground)
                        .border_l_3()
                        .border_color(cx.theme().secondary_active)
                        .px_4()
                        .children({
                            let children_len = children.len();
                            children.into_iter().enumerate().map(move |(index, c)| {
                                let is_last = index == children_len - 1;
                                c.render_block(options.is_last(is_last), node_cx, window, cx)
                            })
                        }),
                )
                .into_any_element(),
            Node::List { children, ordered } => v_flex()
                .id(if *ordered { "ol" } else { "ul" })
                // Filo: a little room between items (the style can set it).
                .gap(node_cx.style.list_gap.unwrap_or(rems(0.25)))
                .pb(mb)
                .children({
                    let mut items = Vec::with_capacity(children.len());
                    let mut ix = 0;
                    for item in children.into_iter() {
                        let is_item = item.is_list_item();

                        items.push(Self::render_list_item(
                            item,
                            ix,
                            NodeRenderOptions {
                                ordered: *ordered,
                                ..options
                            },
                            node_cx,
                            window,
                            cx,
                        ));

                        if is_item {
                            ix += 1;
                        }
                    }
                    items
                })
                .into_any_element(),
            Node::CodeBlock(code_block) => code_block.render(&options, node_cx, window, cx),
            Node::Table { .. } => Self::render_table(self, node_cx, window, cx).into_any_element(),
            Node::Divider => div()
                .pb(mb)
                .child(div().id("divider").bg(cx.theme().border).h(px(2.)))
                .into_any_element(),
            Node::Break { .. } => div().id("break").into_any_element(),
            Node::Unknown | Node::Definition { .. } => div().into_any_element(),
            _ => {
                if cfg!(debug_assertions) {
                    tracing::warn!("unknown implementation: {:?}", self);
                }

                div().into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod table_tests {
    use super::{TableColumn, longest_word, table_columns};
    use gpui::px;

    #[test]
    fn longest_word_measures_between_spaces() {
        // 10 px a byte.
        let x = |ix: usize| px(ix as f32 * 10.);
        assert_eq!(longest_word("Docs (2 accounts)", x), px(90.));
        assert_eq!(longest_word("  one\ttwo  ", x), px(30.));
        assert_eq!(longest_word("", x), px(0.));
    }

    #[test]
    fn short_columns_keep_words_and_long_ones_grow() {
        // (longest word, whole cell) per cell: a label column and a long description column.
        let rows = vec![
            vec![(px(30.), px(30.)), (px(60.), px(60.))],
            vec![(px(80.), px(150.)), (px(90.), px(900.))],
            vec![(px(60.), px(60.)), (px(70.), px(400.))],
        ];
        let columns = table_columns(&rows, px(20.), px(200.), 1.);
        assert_eq!(
            columns,
            vec![
                TableColumn { min: px(82.), max: px(152.), basis: px(152.) },
                TableColumn { min: px(92.), max: px(902.), basis: px(92.) },
            ]
        );
    }

    #[test]
    fn long_words_are_capped_and_empty_columns_floored() {
        let rows = vec![vec![(px(600.), px(600.)), (px(0.), px(0.))], vec![(px(10.), px(10.))]];
        let columns = table_columns(&rows, px(26.), px(182.), 1.06);
        assert_eq!(columns[0].min, px(182.));
        assert!(columns[0].max >= px(600.));
        assert_eq!(columns[0].basis, px(182.));
        assert_eq!(columns[1], TableColumn { min: px(26.), max: px(26.), basis: px(26.) });
    }
}

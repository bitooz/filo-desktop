//! Filo: links and untrusted markdown.
//!
//! Every link a text view opens goes through [`open_link`], which only opens http(s) and mailto addresses: a model's
//! reply or a web page's text could otherwise hand macOS a `file:///Applications/…` path or a custom scheme to launch.
//! A text view whose style is [`TextViewStyle::untrusted`](super::TextViewStyle) also has its parsed tree cleaned
//! before it's rendered ([`sanitize_untrusted`]): raw HTML becomes its text, pictures become links (nothing is
//! fetched), and links and definitions with other addresses keep only their words.

use std::rc::Rc;

use gpui::{App, Global, Window};
use markdown::mdast::{self, Node};

/// Whether `url` may be opened: http, https or mailto, with nothing that could change how macOS reads it.
pub fn is_safe_link(url: &str) -> bool {
    let url = url.trim_matches(|c: char| c.is_ascii_whitespace());
    if url.is_empty() || url.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("mailto:") {
        return !rest.is_empty();
    } else {
        return false;
    };
    // A host is needed: "http:///etc" or "https://" open nothing useful.
    !rest.is_empty() && !rest.starts_with('/')
}

/// The app's own handler for the links a text view opens, set with [`set_link_handler`]: it sees every link first
/// and returns true for one it took (a `commit:` link, `text::commits`), or false to leave it to [`open_link`].
struct LinkHandler(Rc<dyn Fn(&str, &mut Window, &mut App) -> bool>);

impl Global for LinkHandler {}

/// Sets the app's handler for the links its text views open (see [`LinkHandler`]).
pub fn set_link_handler(cx: &mut App, handler: impl Fn(&str, &mut Window, &mut App) -> bool + 'static) {
    cx.set_global(LinkHandler(Rc::new(handler)));
}

/// Hands `url` to the app's link handler, then, if it didn't take it, opens it in the browser or mail app if
/// [`is_safe_link`] allows it; anything else is ignored.
pub fn open_link(url: &str, window: &mut Window, cx: &mut App) -> bool {
    if let Some(handler) = cx.try_global::<LinkHandler>().map(|handler| handler.0.clone()) {
        if handler(url, window, cx) {
            return true;
        }
    }
    if !is_safe_link(url) {
        if cfg!(debug_assertions) {
            tracing::warn!("not opening link with an unsupported address: {url:?}");
        }
        return false;
    }
    cx.open_url(url.trim());
    true
}

/// Cleans a parsed markdown tree for untrusted text (see the module docs).
pub(crate) fn sanitize_untrusted(root: &mut Node) {
    if let Some(children) = root.children_mut() {
        sanitize_children(children, true);
    }
}

/// Filo: pictures at an address on the web become links to it, so the app requests nothing until the reader clicks
/// (and then in their browser), and raw HTML with a picture in it becomes its text. Text a model or a tool wrote could
/// otherwise have the app itself fetch an address of its choosing, with whatever it put in it, around a sandbox's
/// network rules or a mode's review. Pictures on this Mac (a path, `file://`) still show.
pub(crate) fn remote_pictures_as_links(root: &mut Node) {
    if let Some(children) = root.children_mut() {
        pictures_in(children, true);
    }
}

/// Whether a picture's address is on the web: one with a scheme (other than `file:`) or `//host`.
fn on_the_web(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("//") || (url.contains("://") && !url.starts_with("file://"))
}

/// `remote_pictures_as_links` on `children`; `flow` as in `sanitize_children`.
fn pictures_in(children: &mut Vec<Node>, flow: bool) {
    let wrap = |node: Node| -> Node {
        if flow {
            Node::Paragraph(mdast::Paragraph { children: vec![node], position: None })
        } else {
            node
        }
    };
    let mut out = Vec::with_capacity(children.len());
    for child in children.drain(..) {
        match child {
            Node::Image(image) if on_the_web(&image.url) => {
                let label = text(if image.alt.trim().is_empty() { "Picture".to_string() } else { image.alt }, None);
                out.push(wrap(if is_safe_link(&image.url) {
                    Node::Link(mdast::Link { children: vec![label], position: image.position, url: image.url, title: image.title })
                } else {
                    label
                }));
            }
            // A reference's address is only known from its definition: its words.
            Node::ImageReference(image) => {
                if !image.alt.trim().is_empty() {
                    out.push(wrap(text(image.alt, image.position)));
                }
            }
            Node::Html(html) if html.value.to_ascii_lowercase().contains("<img") => out.push(wrap(text(html.value, html.position))),
            mut other => {
                let blocks = matches!(other, Node::Root(_) | Node::Blockquote(_) | Node::ListItem(_) | Node::FootnoteDefinition(_));
                if let Some(children) = other.children_mut() {
                    pictures_in(children, blocks);
                }
                out.push(other);
            }
        }
    }
    *children = out;
}

fn text(value: String, position: Option<markdown::unist::Position>) -> Node {
    Node::Text(mdast::Text { value, position })
}

/// `flow`: the children are blocks (a root, quote, list item or footnote), so loose inline text is wrapped in a
/// paragraph to keep rendering it.
fn sanitize_children(children: &mut Vec<Node>, flow: bool) {
    let wrap = |node: Node| -> Node {
        if flow {
            Node::Paragraph(mdast::Paragraph { children: vec![node], position: None })
        } else {
            node
        }
    };
    let mut out = Vec::with_capacity(children.len());
    for child in children.drain(..) {
        match child {
            Node::Html(html) => out.push(wrap(text(html.value, html.position))),
            Node::Image(image) => {
                let label = if image.alt.trim().is_empty() { "Picture".to_string() } else { image.alt };
                let label = text(label, None);
                out.push(wrap(if is_safe_link(&image.url) {
                    Node::Link(mdast::Link { children: vec![label], position: image.position, url: image.url, title: image.title })
                } else {
                    label
                }));
            }
            Node::ImageReference(image) => {
                if !image.alt.trim().is_empty() {
                    out.push(wrap(text(image.alt, image.position)));
                }
            }
            Node::Link(mut link) => {
                sanitize_children(&mut link.children, false);
                if is_safe_link(&link.url) {
                    out.push(Node::Link(link));
                } else {
                    out.extend(link.children.into_iter().map(&wrap));
                }
            }
            // Reference links never resolve to an address here; keep their words.
            Node::LinkReference(mut link) => {
                sanitize_children(&mut link.children, false);
                out.extend(link.children.into_iter().map(&wrap));
            }
            Node::Definition(mut definition) => {
                if !is_safe_link(&definition.url) {
                    definition.url = String::new();
                }
                out.push(Node::Definition(definition));
            }
            mut other => {
                let blocks = matches!(
                    other,
                    Node::Root(_) | Node::Blockquote(_) | Node::ListItem(_) | Node::FootnoteDefinition(_)
                );
                if let Some(children) = other.children_mut() {
                    sanitize_children(children, blocks);
                }
                out.push(other);
            }
        }
    }
    *children = out;
}

/// Every address `raw` would render as untrusted markdown: link and picture targets, as `link:<url>`,
/// `image:<url>` and `definition:<url>`, from the tree the text view draws. For tests and audits.
pub fn untrusted_markdown_targets(raw: &str) -> Vec<String> {
    markdown_targets(raw, super::TextViewStyle::default().untrusted(true))
}

/// [`untrusted_markdown_targets`] with any style (`commit_links`, for one).
pub fn markdown_targets(raw: &str, style: super::TextViewStyle) -> Vec<String> {
    use super::node::{self, NodeContext};
    let mut cx = NodeContext { style: style.clone(), ..NodeContext::default() };
    let theme = crate::highlighter::HighlightTheme::default_light();
    let Ok(root) = super::format::markdown::parse(raw, &style, &mut cx, &theme) else { return vec!["error".into()] };
    let mut found = Vec::new();
    fn paragraph(paragraph: &node::Paragraph, found: &mut Vec<String>) {
        for inline in &paragraph.children {
            if let Some(image) = &inline.image {
                found.push(format!("image:{}", image.url));
                if let Some(link) = &image.link {
                    found.push(format!("link:{}", link.url));
                }
            }
            for (_, mark) in &inline.marks {
                if let Some(link) = &mark.link {
                    found.push(format!("link:{}", link.url));
                }
            }
        }
        for url in paragraph.link_refs.values() {
            found.push(format!("link:{url}"));
        }
    }
    fn walk(node: &node::Node, found: &mut Vec<String>) {
        match node {
            node::Node::Root { children }
            | node::Node::Blockquote { children }
            | node::Node::List { children, .. }
            | node::Node::ListItem { children, .. } => children.iter().for_each(|child| walk(child, found)),
            node::Node::Paragraph(p) | node::Node::Heading { children: p, .. } => paragraph(p, found),
            node::Node::Table(table) => {
                for row in &table.children {
                    for cell in &row.children {
                        paragraph(&cell.children, found);
                    }
                }
            }
            node::Node::Definition { url, .. } => found.push(format!("definition:{url}")),
            node::Node::CodeBlock(_) | node::Node::Break { .. } | node::Node::Divider | node::Node::Unknown => {}
        }
    }
    walk(&root, &mut found);
    for link in cx.link_refs.values() {
        found.push(format!("definition:{}", link.url));
    }
    found.retain(|target| !target.ends_with(':'));
    found
}

/// The text `raw` renders as untrusted markdown (paragraphs, headings, lists, tables; not code blocks), for tests:
/// what a reader sees, with raw HTML shown as text.
pub fn untrusted_markdown_text(raw: &str) -> String {
    use super::node::{self, NodeContext};
    let style = super::TextViewStyle::default().untrusted(true);
    let mut cx = NodeContext { style: style.clone(), ..NodeContext::default() };
    let theme = crate::highlighter::HighlightTheme::default_light();
    let Ok(root) = super::format::markdown::parse(raw, &style, &mut cx, &theme) else { return String::new() };
    fn paragraph(paragraph: &node::Paragraph, out: &mut String) {
        for inline in &paragraph.children {
            out.push_str(&inline.text);
        }
        out.push('\n');
    }
    fn walk(node: &node::Node, out: &mut String) {
        match node {
            node::Node::Root { children }
            | node::Node::Blockquote { children }
            | node::Node::List { children, .. }
            | node::Node::ListItem { children, .. } => children.iter().for_each(|child| walk(child, out)),
            node::Node::Paragraph(p) | node::Node::Heading { children: p, .. } => paragraph(p, out),
            node::Node::Table(table) => {
                for row in &table.children {
                    for cell in &row.children {
                        paragraph(&cell.children, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    walk(&root, &mut out);
    out
}

//! Filo: commit hashes as links.
//!
//! With [`TextViewStyle::commit_links`](super::TextViewStyle), a commit hash in the text (`8d40428f`, bare or as a
//! piece of inline code that is only the hash) is drawn as a link to `commit:<hash>`. The text view can't know which
//! repo it's from, so the app's link handler opens it (`safety::set_link_handler`). Hashes already inside a link,
//! code blocks and raw HTML are left alone.

use std::ops::Range;

use markdown::mdast::{self, Node};

/// The scheme of a commit link's address: `commit:8d40428f`.
pub const SCHEME: &str = "commit:";

/// A commit link's address.
pub fn commit_url(hash: &str) -> String {
    format!("{SCHEME}{hash}")
}

/// The hash a commit link points at, if `url` is one.
pub fn commit_of(url: &str) -> Option<&str> {
    url.strip_prefix(SCHEME).filter(|hash| is_hash(hash))
}

/// Whether `word` reads as a commit hash: 7 to 40 lowercase hex digits with at least one digit and one letter, so
/// numbers ("1234567") and words spelt in hex letters ("defaced") aren't taken for one.
pub fn is_hash(word: &str) -> bool {
    (7..=40).contains(&word.len())
        && word.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && word.bytes().any(|b| b.is_ascii_digit())
        && word.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Where the commit hashes in `text` are: whole words that read as one ([`is_hash`]), not part of a path, address,
/// version, id or file name around them (`a/8d40428f`, `v1-8d40428f`, `#8d40428f`, `8d40428f.txt`).
pub fn hash_ranges(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut found = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !word(bytes[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && word(bytes[end]) {
            end += 1;
        }
        let before = start.checked_sub(1).map(|ix| bytes[ix]);
        let after = bytes.get(end).copied();
        let joined_before = before.is_some_and(|b| b"-/.@#:=+&\\~$%".contains(&b));
        let joined_after = after.is_some_and(|b| b"-/@\\".contains(&b))
            || (after == Some(b'.') && bytes.get(end + 1).is_some_and(|b| word(*b)));
        if !joined_before && !joined_after && is_hash(&text[start..end]) {
            found.push(start..end);
        }
        start = end;
    }
    found
}

/// Turns the commit hashes in a parsed tree's text into links (see the module docs).
pub(crate) fn link_commits(node: &mut Node) {
    if matches!(
        node,
        Node::Link(_) | Node::LinkReference(_) | Node::Code(_) | Node::Html(_) | Node::Definition(_) | Node::Image(_) | Node::ImageReference(_)
    ) {
        return;
    }
    let Some(children) = node.children_mut() else { return };
    let mut out = Vec::with_capacity(children.len());
    for mut child in children.drain(..) {
        match child {
            Node::Text(text) => split_text(text, &mut out),
            Node::InlineCode(code) if is_hash(code.value.trim()) => {
                let url = commit_url(code.value.trim());
                out.push(Node::Link(mdast::Link { children: vec![Node::InlineCode(code)], position: None, url, title: None }));
            }
            _ => {
                link_commits(&mut child);
                out.push(child);
            }
        }
    }
    *children = out;
}

/// `text` as its words and a link for each hash in it.
fn split_text(text: mdast::Text, out: &mut Vec<Node>) {
    let ranges = hash_ranges(&text.value);
    if ranges.is_empty() {
        out.push(Node::Text(text));
        return;
    }
    let words = |value: &str| Node::Text(mdast::Text { value: value.to_string(), position: None });
    let mut at = 0;
    for range in ranges {
        if range.start > at {
            out.push(words(&text.value[at..range.start]));
        }
        let hash = &text.value[range.clone()];
        out.push(Node::Link(mdast::Link { children: vec![words(hash)], position: None, url: commit_url(hash), title: None }));
        at = range.end;
    }
    if at < text.value.len() {
        out.push(words(&text.value[at..]));
    }
}

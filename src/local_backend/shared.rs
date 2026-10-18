// What a conversation shared, for the Shared section of an agent's profile (screens/chat/shared.rs): pictures and
// videos, files, voice (voice messages' recordings and audio files) and links, read from the chat's messages, so nothing extra is stored.
// Newest first, a page at a time. A page's cursor is the last message it read (`<at>:<id>`), and the next page starts
// just before it, so messages added meanwhile don't shift what's older.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use filo_local::{Message, Role};
use regex::Regex;
use serde_json::{Value, json};

use super::{Local, arg};

/// Items a page holds, about: a message's items stay together on one page.
const PAGE: usize = 60;

// A bare URL ends at whitespace, quotes, angle brackets or a closing bracket; trailing punctuation isn't part of it.
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`)\]]+"#).unwrap());
static MARKDOWN_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]\n]{1,200})\]\((https?://[^)\s]+)\)").unwrap());
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '*', '_', '~'];

impl Local {
    /// `chatShared:list`: one conversation's, Filo's chat or an agent's.
    pub(super) fn shared(&self, args: &Value) -> Value {
        let Some(id) = arg(args, "sessionId") else { return done(Vec::new()) };
        let Some(store) = self.store_of(id) else { return done(Vec::new()) };
        page(&self.root, store.messages(id).unwrap_or_default(), arg(args, "cursor"))
    }
}

fn done(items: Vec<Value>) -> Value {
    json!({ "items": items, "cursor": "", "done": true })
}

/// The page of `messages`' items older than `cursor` (none: the newest). `root` is where the recordings are.
fn page(root: &Path, mut messages: Vec<Message>, cursor: Option<&str>) -> Value {
    let before = cursor
        .and_then(|cursor| cursor.split_once(':'))
        .and_then(|(at, id)| Some((at.parse::<u64>().ok()?, id.to_string())));
    messages.retain(|message| message.role != Role::Error && !message.hidden);
    messages.sort_by(|a, b| (b.at, &b.id).cmp(&(a.at, &a.id)));
    let older =
        messages.iter().filter(|message| before.as_ref().is_none_or(|(at, id)| (message.at, &message.id) < (*at, id)));
    let mut items = Vec::new();
    let mut last = None;
    for message in older {
        if items.len() >= PAGE {
            let last: &Message = last.unwrap();
            return json!({ "items": items, "cursor": format!("{}:{}", last.at, last.id), "done": false });
        }
        items.extend(shared_in(root, message));
        last = Some(message);
    }
    done(items)
}

/// The links in a message's text, each once, with its markdown title when it has one.
fn links_in(text: &str) -> Vec<(String, Option<String>)> {
    let titles: Vec<(&str, &str)> = MARKDOWN_LINK
        .captures_iter(text)
        .map(|link| (link.get(2).unwrap().as_str(), link.get(1).unwrap().as_str().trim()))
        .collect();
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for found in URL.find_iter(text) {
        let url = found.as_str().trim_end_matches(TRAILING);
        if !seen.insert(url) {
            continue;
        }
        let title = titles
            .iter()
            .rev()
            .find(|(link, _)| *link == url)
            .map(|(_, title)| title.to_string())
            .filter(|title| title != url);
        links.push((url.to_string(), title));
    }
    links
}

fn kind_of(content_type: &str) -> &'static str {
    if content_type.starts_with("image/") || content_type.starts_with("video/") {
        "media"
    } else if content_type.starts_with("audio/") {
        "voice"
    } else {
        "file"
    }
}

/// One message's shared items (`SharedItem`), in the order the message shows them. A file is its copy on this Mac, as
/// a `file://` address; one no longer there is left out.
fn shared_in(root: &Path, message: &Message) -> Vec<Value> {
    let item = |kind: &str, url: String| json!({ "kind": kind, "messageId": message.id, "at": message.at as f64, "fromUser": message.role == Role::User, "url": url });
    let mut items = Vec::new();
    // A voice message's recording (local_backend/voice.rs).
    if let Some((voice, file)) =
        message.voice.as_ref().and_then(|voice| Some((voice, filo_local::attachments::file(root, &voice.storage_id)?)))
    {
        let mut row = item("voice", format!("file://{}", file.display()));
        row["name"] = json!("Voice message");
        row["contentType"] = json!("audio/mp4");
        row["size"] = json!(std::fs::metadata(&file).map_or(0., |meta| meta.len() as f64));
        row["durationMs"] = json!(voice.duration_ms as f64);
        items.push(row);
    }
    for file in message.attachments.iter().filter(|file| file.file.is_file()) {
        let mut row = item(kind_of(&file.content_type), format!("file://{}", file.file.display()));
        row["name"] = json!(file.name);
        row["contentType"] = json!(file.content_type);
        row["size"] = json!(file.size as f64);
        items.push(row);
    }
    for (url, title) in links_in(&message.text) {
        let mut row = item("link", url);
        if let Some(title) = title {
            row["title"] = json!(title);
        }
        items.push(row);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links_once_with_their_titles() {
        let links = links_in(
            "See [the docs](https://docs.example/a) and https://docs.example/a, or <https://x.example/b>. Also (https://y.example/c).",
        );
        assert_eq!(
            links,
            vec![
                ("https://docs.example/a".to_string(), Some("the docs".to_string())),
                ("https://x.example/b".to_string(), None),
                ("https://y.example/c".to_string(), None),
            ]
        );
        assert_eq!(links_in("[https://z.example](https://z.example)"), vec![("https://z.example".to_string(), None)]);
    }

    #[test]
    fn lists_what_a_chat_shared_newest_first_in_pages() {
        let local = Local::open(std::env::temp_dir().join(format!("filo-local-shared-{}", uuid::Uuid::new_v4())));
        let chat = local.agent_chats.create_chat(super::super::default_engine()).unwrap();
        let file = |content_type: &str, name: &str| {
            let storage_id = local.save_attachment(content_type, b"bytes").unwrap();
            filo_local::store::Attachment {
                storage_id: storage_id.clone(),
                name: name.into(),
                content_type: content_type.into(),
                size: 5,
                file: filo_local::attachments::file(&local.root, &storage_id).unwrap(),
                local_path: None,
                bookmark: None,
            }
        };
        let files = vec![
            file("image/png", "a.png"),
            file("video/mp4", "b.mp4"),
            file("application/pdf", "c.pdf"),
            file("audio/mpeg", "d.mp3"),
        ];
        local.agent_chats.add_user_message_with(&chat.id, "Look at https://example.com/x", "turn-1", files).unwrap();
        let mut reply =
            Message::new(Role::Assistant, "Here: [Rust](https://www.rust-lang.org/).", Some("turn-1".into()));
        reply.at += 10;
        local.agent_chats.append(&chat.id, &reply).unwrap();

        let page = local.call("chatShared:list", &json!({ "sessionId": chat.id, "cursor": null })).unwrap();
        assert_eq!(page["done"], true);
        let items = page["items"].as_array().unwrap();
        let kinds: Vec<&str> = items.iter().map(|item| item["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["link", "media", "media", "file", "voice", "link"]);
        assert_eq!((items[0]["title"].as_str(), items[0]["fromUser"].as_bool()), (Some("Rust"), Some(false)));
        assert_eq!((items[1]["name"].as_str(), items[1]["fromUser"].as_bool()), (Some("a.png"), Some(true)));
        assert!(items[1]["url"].as_str().unwrap().starts_with("file:///"));
        // An unknown chat has none.
        let none = local.call("chatShared:list", &json!({ "sessionId": "nope" })).unwrap();
        assert_eq!((none["items"].as_array().unwrap().len(), &none["done"]), (0, &json!(true)));

        // More than a page: the rest follow from the cursor, none twice and none missed.
        for n in 0..100 {
            let mut message = Message::new(Role::Assistant, format!("https://example.com/{n}"), None);
            message.at += 100 + n;
            local.agent_chats.append(&chat.id, &message).unwrap();
        }
        let (mut urls, mut cursor) = (Vec::new(), Value::Null);
        loop {
            let page = local.call("chatShared:list", &json!({ "sessionId": chat.id, "cursor": cursor })).unwrap();
            urls.extend(page["items"].as_array().unwrap().iter().map(|item| item["url"].as_str().unwrap().to_string()));
            if page["done"] == true {
                break;
            }
            cursor = page["cursor"].clone();
        }
        assert_eq!(urls.len(), 106);
        assert_eq!(urls[0], "https://example.com/99");
        assert_eq!(urls.iter().collect::<HashSet<_>>().len(), 106);
    }
}

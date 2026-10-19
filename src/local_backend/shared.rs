// What a conversation shared, for the Shared section of an agent's profile and of Group settings
// (screens/chat/shared.rs): pictures and videos, files, voice (voice messages' recordings and audio files) and links,
// read from the chat's messages (the group's), so nothing extra is stored. Newest first, a page at a time. A page's
// cursor is the last message it read (`<at>:<id>`), and the next page starts just before it, so messages added
// meanwhile don't shift what's older.
//
// The user can delete the files Filo keeps copies of from there, and voice messages' recordings
// (`chatShared:deleteFiles`, attachments.rs `delete_copy`, `delete_recording`), to free the room they take: the messages
// then show them as no longer kept. A file shown where it
// is on this Mac (an agent's, attachments.rs `in_place`) isn't Filo's to delete. A chat's copies also go with the chat
// (`delete_chat_files`).
//
// Show in Chat asks how far back an item's message is (`chatShared:depth`), counted as the chat loads its pages, so the
// chat loads back that far before it scrolls to it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use filo_local::group;
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
    /// `chatShared:list`: one conversation's, an agent's or the group's.
    pub(super) fn shared(&self, args: &Value) -> Value {
        let Some(id) = arg(args, "sessionId") else { return done(Vec::new()) };
        let (messages, senders) = self.shared_messages_of(id);
        page(&self.root, messages, &senders, arg(args, "cursor"))
    }

    /// `chatShared:depth { sessionId, messageId }`: how many messages from the newest that one is (1: the newest), as
    /// `voiceSessions:detail`'s `limit` counts them (the group's: as its detail does), or null when the chat doesn't
    /// show it.
    pub(super) fn shared_depth(&self, args: &Value) -> Result<Value, String> {
        let (Some(id), Some(message)) = (arg(args, "sessionId"), arg(args, "messageId")) else {
            return Ok(Value::Null);
        };
        if self.group_session().as_deref() == Some(id) {
            let depth: Vec<i64> = self.db()?.with(|conn| {
                let mut statement = conn.prepare(
                    "SELECT (SELECT COUNT(*) FROM group_messages WHERE seq >= m.seq) FROM group_messages m WHERE m.id = ?1",
                )?;
                statement.query_map([message], |row| row.get(0))?.collect()
            })?;
            return Ok(json!(depth.first()));
        }
        let Some(store) = self.store_of(id) else { return Ok(Value::Null) };
        let all = store.shared_messages(id).unwrap_or_default();
        let shown = filo_local::store::shown_of(&all);
        Ok(json!(shown.iter().rposition(|shown| shown.id == message).map(|index| shown.len() - index)))
    }

    /// The messages of conversation `id` whose files the Shared list shows: an agent's chat's, with its files where
    /// they are now (`find_moved_files`), or the group's, with each member's post's speaker by its id.
    fn shared_messages_of(&self, id: &str) -> (Vec<Message>, HashMap<String, String>) {
        if self.group_session().as_deref() == Some(id) {
            return self.group_as_messages();
        }
        let Some(store) = self.store_of(id) else { return Default::default() };
        let mut messages = store.messages(id).unwrap_or_default();
        store.find_moved_files(id, &mut messages);
        (messages, HashMap::new())
    }

    /// The group's messages as a chat's, for the Shared list, and the name of each member post's speaker by its id.
    fn group_as_messages(&self) -> (Vec<Message>, HashMap<String, String>) {
        let Ok(db) = self.db() else { return Default::default() };
        // All of them (a limit past i64 wraps in the query).
        let Ok((mut posts, _)) = group::messages(db, u32::MAX as usize) else { return Default::default() };
        group::find_moved_files(db, &mut posts);
        let ids: Vec<&str> = posts.iter().map(|post| post.id.as_str()).collect();
        let mut voices = group::voices(db, &ids).unwrap_or_default();
        let members = self.group_members();
        let mut senders = HashMap::new();
        let messages = posts
            .into_iter()
            .map(|post| {
                let mut message = Message::new(post.role, post.text, None);
                message.at = post.at.max(0) as u64;
                message.voice = voices.remove(&post.id);
                message.attachments = post.attachments;
                if let Some(speaker) = post.speaker {
                    senders.insert(post.id.clone(), group::speaker_name(&speaker, &members));
                }
                message.id = post.id;
                message
            })
            .collect();
        (messages, senders)
    }

    /// `chatShared:deleteFiles { sessionId, files: [{ messageId, url }] }`: deletes Filo's copies of those files of the
    /// conversation's messages (the Shared list's picks), each only when it's one (attachments.rs `delete_copy`), never a
    /// file shown where it is, and their voice messages' recordings (`delete_recording`). How many it deleted and the
    /// room that freed.
    pub(super) fn delete_shared_files(&self, args: &Value) -> Result<Value, String> {
        let id = super::required(args, "sessionId")?;
        let picked: HashSet<(&str, &str)> = args
            .get("files")
            .and_then(Value::as_array)
            .ok_or("Pick the files to delete.")?
            .iter()
            .filter_map(|file| Some((arg(file, "messageId")?, arg(file, "url")?)))
            .collect();
        let (mut deleted, mut freed) = (0, 0u64);
        for message in self.shared_messages_of(id).0 {
            // Its voice message's recording, Filo's too.
            if let Some((voice, file)) = message
                .voice
                .as_ref()
                .and_then(|voice| Some((voice, filo_local::attachments::file(&self.root, &voice.storage_id)?)))
                && picked.contains(&(message.id.as_str(), file_url(&file).as_str()))
            {
                let size = std::fs::metadata(&file).map_or(0, |meta| meta.len());
                match filo_local::attachments::delete_recording(&self.root, &voice.storage_id) {
                    Ok(true) => (deleted, freed) = (deleted + 1, freed + size),
                    Ok(false) => {}
                    Err(error) => return Err(format!("Couldn't delete the voice message: {error}")),
                }
            }
            for file in &message.attachments {
                if !picked.contains(&(message.id.as_str(), file_url(&file.file).as_str())) {
                    continue;
                }
                let size = std::fs::symlink_metadata(&file.file).map_or(0, |meta| meta.len());
                match filo_local::attachments::delete_copy(file) {
                    Ok(true) => (deleted, freed) = (deleted + 1, freed + size),
                    Ok(false) => {}
                    Err(error) => return Err(format!("Couldn't delete {}: {error}", file.name)),
                }
            }
        }
        if deleted > 0 {
            self.changed();
        }
        Ok(json!({ "deleted": deleted, "freed": freed as f64 }))
    }

    /// Deletes Filo's copies of the files in chat `id` (attachments.rs `delete_copy`) and its voice messages'
    /// recordings, as the chat goes: those no other chat or the group has too (a member's post shows in its own chat
    /// as well, the user's group files in the chats of the members who answer).
    pub(super) fn delete_chat_files(&self, id: &str) {
        let Some(store) = self.store_of(id) else { return };
        let messages = store.messages(id).unwrap_or_default();
        let copies: Vec<&filo_local::store::Attachment> = messages
            .iter()
            .flat_map(|message| &message.attachments)
            .filter(|file| filo_local::attachments::own_copy(file))
            .collect();
        let recordings: Vec<&str> =
            messages.iter().filter_map(|message| Some(message.voice.as_ref()?.storage_id.as_str())).collect();
        if copies.is_empty() && recordings.is_empty() {
            return;
        }
        let (mut files, mut voices): (HashSet<PathBuf>, HashSet<String>) =
            self.db().ok().and_then(|db| group::files_kept(db).ok()).unwrap_or_default();
        for other in store.list_chats().into_iter().filter(|other| other.chat.id != id) {
            for message in store.messages(&other.chat.id).unwrap_or_default() {
                files.extend(message.attachments.into_iter().map(|file| file.file));
                voices.extend(message.voice.map(|voice| voice.storage_id));
            }
        }
        for copy in copies.into_iter().filter(|copy| !files.contains(&copy.file)) {
            let _ = filo_local::attachments::delete_copy(copy);
        }
        for recording in recordings.into_iter().filter(|recording| !voices.contains(*recording)) {
            if let Some(file) = filo_local::attachments::file(&self.root, recording) {
                let _ = std::fs::remove_file(file);
            }
        }
    }
}

/// A file as the Shared list names it: its `file://` address.
fn file_url(file: &Path) -> String {
    format!("file://{}", file.display())
}

fn done(items: Vec<Value>) -> Value {
    json!({ "items": items, "cursor": "", "done": true })
}

/// The page of `messages`' items older than `cursor` (none: the newest). `root` is where the recordings are; `senders`
/// who sent a group post, by its id.
fn page(root: &Path, mut messages: Vec<Message>, senders: &HashMap<String, String>, cursor: Option<&str>) -> Value {
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
        items.extend(shared_in(root, message, senders.get(&message.id).map(String::as_str)));
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

fn kind_of(content_type: &str, name: &str) -> &'static str {
    if content_type.starts_with("image/") || content_type.starts_with("video/") {
        "media"
    } else if crate::media::is_audio(content_type, name) {
        "voice"
    } else {
        "file"
    }
}

/// One message's shared items (`SharedItem`), in the order the message shows them. A file is its copy on this Mac (or
/// itself, shown where it is), as a `file://` address, with its size and whether it's Filo's copy, which the user can
/// delete (`deletable`); one no longer there is left out. A group post's carry its speaker's name (`sender`).
fn shared_in(root: &Path, message: &Message, sender: Option<&str>) -> Vec<Value> {
    let item = |kind: &str, url: String| json!({ "kind": kind, "messageId": message.id, "at": message.at as f64, "fromUser": message.role == Role::User, "sender": sender, "url": url });
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
        row["deletable"] = json!(true);
        items.push(row);
    }
    for file in &message.attachments {
        let Some(meta) = std::fs::metadata(&file.file).ok().filter(|meta| meta.is_file()) else { continue };
        let mut row = item(kind_of(&file.content_type, &file.name), file_url(&file.file));
        row["name"] = json!(file.name);
        row["contentType"] = json!(file.content_type);
        row["size"] = json!(meta.len() as f64);
        row["deletable"] = json!(filo_local::attachments::own_copy(file));
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
    fn a_sound_is_a_voice_item_by_its_type_or_its_name() {
        assert_eq!(kind_of("audio/mpeg", "take"), "voice");
        // A FLAC an older Filo sent untyped.
        assert_eq!(kind_of("application/octet-stream", "interview.flac"), "voice");
        assert_eq!(kind_of("application/octet-stream", "notes.txt"), "file");
        assert_eq!(kind_of("video/mp4", "clip.mp4"), "media");
    }

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
                inode: None,
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

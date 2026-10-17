// Chats with replies the user hasn't seen (voiceSessions:unseen, local_backend.rs `unseen`): the Agents tab's and
// list's dots, the Dock badge, and a macOS
// banner per chat for a reply nobody has seen for a few seconds (`pushedUntil`, local_backend.rs `unseen`) while
// the user is in another app; in Filo, only the card in its window (in_app_notice.rs).

use std::collections::HashMap;

use gpui::{Context, Entity, EventEmitter, Global, Task};
use serde::Deserialize;
use serde_json::json;

use crate::backend;
use crate::notifications;
use crate::presence;

/// Counts stop here; "99+" past it (MAX_UNSEEN_COUNT).
const MAX_COUNT: u32 = 99;

/// A chat's unseen replies as `voiceSessions:unseen` gives them: here (the dots, the badge, banners) and in the
/// companion (the pet's bubbles, the menu bar's rows).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnseenChat {
    pub session_id: String,
    pub title: String,
    pub count: f64,
    /// When its newest unseen reply came.
    #[serde(default)]
    pub latest_at: f64,
    /// The newest message time the user has seen.
    #[serde(default)]
    pub seen_until: f64,
    /// The newest reply to notify about, once it has waited a few seconds unseen.
    pub pushed_until: Option<f64>,
    pub preview: String,
    /// Set on an agent's chat, which counts under the Agents tab.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Set on the group chat of Filo and the agents, which counts under the Agents tab too.
    #[serde(default)]
    pub agent_group: bool,
}

impl UnseenChat {
    /// An agent's chat or the group: it counts under the Agents tab.
    pub fn in_agents(&self) -> bool {
        self.agent_id.is_some() || self.agent_group
    }

    /// What a notice of it says (a banner, the pet's bubble): its title (`fallback` with none), "3 new messages" when
    /// there are several, and the newest's preview.
    pub fn notice(&self, fallback: &str) -> (String, Option<String>, String) {
        let title = if self.title.trim().is_empty() { fallback.to_string() } else { self.title.clone() };
        let subtitle = (self.count > 1.).then(|| format!("{} new messages", format_count(self.count as u32)));
        let body = if self.preview.is_empty() { "New message".to_string() } else { self.preview.clone() };
        (title, subtitle, body)
    }
}

/// `voiceSessions:unseen`'s answer.
#[derive(Debug, Deserialize)]
pub struct Summary {
    #[serde(default)]
    pub total: f64,
    pub chats: Vec<UnseenChat>,
}

/// Which replies are news, for one place that tells the user (the banners here, the pet): each chat's newest
/// `pushedUntil` it told, so a reply is told once. Nothing at the first answer (what was waiting at launch), and none
/// from before the user last looked at the chat.
#[derive(Default)]
pub struct Told {
    told: HashMap<String, f64>,
    loaded: bool,
}

impl Told {
    /// The chats with a reply to tell now, from `chats` (the whole unseen list: chats gone from it were read and are
    /// forgotten, returned as `read`).
    pub fn news<'a>(&mut self, chats: &'a [UnseenChat]) -> (Vec<&'a UnseenChat>, Vec<String>) {
        let read: Vec<String> =
            self.told.keys().filter(|id| !chats.iter().any(|chat| &chat.session_id == *id)).cloned().collect();
        self.told.retain(|id, _| chats.iter().any(|chat| &chat.session_id == id));
        let mut news = Vec::new();
        for chat in chats {
            let Some(pushed) = chat.pushed_until else { continue };
            let told = self.told.get(&chat.session_id).copied().unwrap_or(0.);
            // A push from before the user's last look was for replies they've since seen: once a chat is read it's
            // forgotten here, so a new reply would otherwise bring the old push back.
            if pushed <= told || pushed <= chat.seen_until {
                continue;
            }
            self.told.insert(chat.session_id.clone(), pushed);
            if self.loaded {
                news.push(chat);
            }
        }
        self.loaded = true;
        (news, read)
    }
}

pub fn format_count(count: u32) -> String {
    if count > MAX_COUNT { format!("{MAX_COUNT}+") } else { count.to_string() }
}

/// A reply nobody has seen for a few seconds (`pushedUntil`): the shell shows it in the window too.
#[derive(Clone, Debug)]
pub struct ReplyNotice {
    pub session_id: String,
    pub title: String,
    /// A macOS banner went out for it too (Filo was in the background), which plays the sound.
    pub banner: bool,
}

impl EventEmitter<ReplyNotice> for Unseen {}

/// The unseen chats, for any view that shows counts.
pub struct UnseenGlobal(pub Entity<Unseen>);

impl Global for UnseenGlobal {}

pub struct Unseen {
    total: u32,
    chats: HashMap<String, UnseenChat>,
    /// The replies a banner (or the window's card) told.
    told: Told,
    _sub: Task<()>,
}

impl Unseen {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sub = backend::subscribe(
            cx,
            "voiceSessions:unseen",
            json!({}),
            |this: &mut Self, result: Result<Summary, String>, cx| {
                if let Ok(summary) = result {
                    this.update(summary, cx);
                }
            },
        );
        Unseen { total: 0, chats: HashMap::new(), told: Told::default(), _sub: sub }
    }

    /// Unseen replies from the user's agents (the Agents tab's dot).
    pub fn agents_total(&self) -> u32 {
        self.chats.values().filter(|chat| chat.in_agents()).map(|chat| chat.count as u32).sum()
    }

    fn update(&mut self, summary: Summary, cx: &mut Context<Self>) {
        let (news, read) = self.told.news(&summary.chats);
        // Read chats take their banners down.
        notifications::remove_chats(&read);
        for chat in news {
            let (title, subtitle, body) = chat.notice("Filo");
            // A banner only while the user is in another app: in Filo, the card in its window shows it (app.rs).
            // The pet says it instead, when it's out (pet.rs).
            let banner = !crate::companion::app_active() && !crate::pet::speaks_replies(cx);
            if banner {
                notifications::show_chat(&chat.session_id, &title, subtitle.as_deref(), &body);
            }
            cx.emit(ReplyNotice { session_id: chat.session_id.clone(), title, banner });
        }
        self.total = summary.total as u32;
        self.chats = summary.chats.into_iter().map(|chat| (chat.session_id.clone(), chat)).collect();
        presence::set_dock_badge((self.total > 0).then(|| format_count(self.total)).as_deref());
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, pushed: Option<f64>, seen_until: f64) -> UnseenChat {
        let chat = json!({ "sessionId": id, "title": "", "count": 2., "seenUntil": seen_until, "pushedUntil": pushed, "preview": "" });
        serde_json::from_value(chat).unwrap()
    }

    #[test]
    fn a_reply_is_told_once_and_never_what_was_waiting_at_launch() {
        let mut told = Told::default();
        assert!(told.news(&[chat("a", Some(10.), 0.)]).0.is_empty(), "waiting at launch");
        let ids = |news: Vec<&UnseenChat>| news.into_iter().map(|chat| chat.session_id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(told.news(&[chat("a", Some(20.), 0.), chat("b", Some(5.), 0.)]).0), ["a", "b"]);
        assert!(told.news(&[chat("a", Some(20.), 0.), chat("b", Some(5.), 0.)]).0.is_empty(), "told already");
        // "b" was read: it's forgotten, and said so; a push from before the user's last look isn't news.
        let later = [chat("a", Some(30.), 40.)];
        let (news, read) = told.news(&later);
        assert!(news.is_empty());
        assert_eq!(read, ["b"]);
        assert_eq!(
            chat("a", None, 0.).notice("Milo"),
            ("Milo".to_string(), Some("2 new messages".to_string()), "New message".to_string())
        );
    }
}

// Usage limits that lift: a turn that failed on its plan's usage limit, saying when it resets ("You've hit your session
// limit · resets 2:50pm"), is tried again on its own shortly after (crates/filo-local `turn_state::retry_at`), once.
// Meanwhile its chat, or the group, says when (`retryAt` on its turn). One its chat settled since (a turn there went
// through) isn't, nor one whose time passed long ago while the app was closed: those are the user's Retry. A member's
// turn in the group runs again through the group, so it posts there.
//
// Only the app does this, while it's open. The turns it tried are kept in local/limit-retries.json, so a restart
// doesn't try them twice.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use filo_local::store::valid_id;
use filo_local::turn_state::{self, End};
use serde_json::json;

use super::{Local, now_ms};

/// The turns tried again on their own, with when (ms).
const RETRIED: &str = "limit-retries.json";
/// How often it looks for limits that lifted.
const LOOK_EVERY: Duration = Duration::from_secs(30);
/// A turn whose retry time passed longer ago than this (the app was closed then) is left to the user's Retry.
const RETRY_WITHIN: i64 = 6 * 3_600_000;
/// Tried turns are forgotten after this long.
const FORGET_AFTER: f64 = 30. * 24. * 3_600_000.;

/// A chat's turns that failed on a limit naming its reset (turn, error, when it failed), as of the chat's messages
/// and its last turn that went through: read again only when those change.
type Known = HashMap<String, ((u64, Option<u64>), Vec<(String, String, i64)>)>;

/// Looks for limits that lifted while the app is open.
pub fn start(local: Arc<Local>) {
    let spawned = std::thread::Builder::new().name("filo-limit-retry".into()).spawn(move || {
        let mut known = Known::new();
        loop {
            local.retry_lifted_limits(&mut known, now_ms());
            std::thread::sleep(LOOK_EVERY);
        }
    });
    if let Err(error) = spawned {
        eprintln!("limits: couldn't start looking for limits that lifted: {error}");
    }
}

impl Local {
    /// The turns tried again on their own, by id.
    pub(super) fn limit_retried(&self) -> HashMap<String, f64> {
        self.read_json(RETRIED)
    }

    /// When the failed turn `turn_id` (its `error`, at `failed_at`) is tried again on its own: none when the error names
    /// no reset, it was tried already, or its time passed too long ago.
    pub(super) fn limit_retry_at(
        &self,
        retried: &HashMap<String, f64>,
        turn_id: &str,
        error: &str,
        failed_at: i64,
        now: i64,
    ) -> Option<i64> {
        if retried.contains_key(turn_id) {
            return None;
        }
        turn_state::retry_at(error, failed_at, &chrono::Local).filter(|at| now < at + RETRY_WITHIN)
    }

    /// Tries again each failed turn whose limit lifted: the group's through the group, the rest in their chats.
    fn retry_lifted_limits(self: &Arc<Self>, known: &mut Known, now: i64) {
        let retried = self.limit_retried();
        let due = |turn: &str, error: &str, failed_at: i64| {
            self.limit_retry_at(&retried, turn, error, failed_at, now).is_some_and(|at| at <= now)
        };
        for turn in self.shown_group_failures() {
            if turn.error.as_deref().is_some_and(|error| due(&turn.id, error, turn.created_at))
                && self.mark_limit_retried(&turn.id, now)
            {
                eprintln!("limits: trying the group's turn {} again", turn.id);
                if let Err(error) = self.retry_group_turn(&turn.id) {
                    eprintln!("limits: couldn't: {error}");
                }
            }
        }
        let store = &self.agent_chats;
        let Ok(entries) = std::fs::read_dir(store.root().join("chats")) else { return };
        for chat in entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).filter(|id| valid_id(id))
        {
            let Some(last_ok_at) = store.chat(&chat).ok().flatten().map(|chat| chat.last_ok_at) else { continue };
            let version = (store.messages_version(&chat), last_ok_at);
            let key = format!("{}/{chat}", store.root().display());
            if known.get(&key).is_none_or(|(seen, _)| *seen != version) {
                let messages = store.messages(&chat).unwrap_or_default();
                let failed = turn_state::ends(&messages, last_ok_at)
                    .into_iter()
                    .filter_map(|(turn, end)| match end {
                        End::Failed(error) => Some((turn.to_string(), error.text.clone(), error.at as i64)),
                        _ => None,
                    })
                    .filter(|(_, error, at)| turn_state::resets_at(error, *at, &chrono::Local).is_some())
                    .collect();
                known.insert(key.clone(), (version, failed));
            }
            let busy = self.running.lock().unwrap().values().any(|running| running.session_id == chat);
            let Some((_, failed)) = known.get(&key) else { continue };
            for (turn, error, at) in failed {
                // A member's turn in the group goes again through the group (above), when the group still shows it.
                let in_group =
                    self.db().ok().is_some_and(|db| filo_local::group::turn(db, turn).ok().flatten().is_some());
                if busy || in_group || !due(turn, error, *at) || !self.mark_limit_retried(turn, now) {
                    continue;
                }
                eprintln!("limits: trying turn {turn} of chat {chat} again");
                if let Err(error) = self.dispatch_turn(&json!({ "turnId": turn, "sessionId": chat })) {
                    eprintln!("limits: couldn't: {error}");
                }
                // One turn of a chat at a time: the next look finds the rest, unless this one settles them.
                break;
            }
        }
    }

    /// Keeps that turn `turn_id` was tried again on its own: false when it couldn't be kept, so it isn't tried (it might
    /// be tried again and again).
    fn mark_limit_retried(&self, turn_id: &str, now: i64) -> bool {
        self.update_json(RETRIED, |retried: &mut HashMap<String, f64>| {
            retried.retain(|_, at| now as f64 - *at < FORGET_AFTER);
            retried.insert(turn_id.to_string(), now as f64);
            Ok(())
        })
        .inspect_err(|error| eprintln!("limits: {error}"))
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use filo_local::{Message, Role};

    use super::*;
    use crate::local_backend::DEFAULT_AGENT_ID;

    /// A reply that failed on a usage limit says when it's tried again, and is, once, after the limit lifts; one whose
    /// limit lifts later waits. A later turn that goes through settles both: neither is tried, and nothing says the
    /// reply failed.
    #[test]
    fn a_turn_that_hit_a_usage_limit_is_tried_again_once_it_lifts() {
        let local = Local::open(std::env::temp_dir().join(format!("filo-local-limits-{}", uuid::Uuid::new_v4())));
        local.no_turns.store(true, Ordering::SeqCst);
        let chat = local.open_agent_chat(DEFAULT_AGENT_ID).unwrap().as_str().unwrap().to_string();
        let now = now_ms();
        let limit = "Codex: You've hit your usage limit. Try again in 30 minutes.\n\nThat's your Codex plan's limit, not Filo's.";
        for (turn, ago) in [("t-1", 40), ("t-2", 10)] {
            local.agent_chats.add_user_message(&chat, "hi", turn).unwrap();
            let mut error = Message::new(Role::Error, limit, Some(turn.into()));
            error.at = (now - ago * 60_000) as u64;
            local.agent_chats.append(&chat, &error).unwrap();
        }
        let turns = || local.call("voiceSessions:chatTurns", &json!({ "sessionId": chat })).unwrap();
        let failed = || {
            local
                .call("agents:list", &json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["_id"] == DEFAULT_AGENT_ID)
                .unwrap()["replyFailed"]
                .clone()
        };
        let (first, second) = (now - 40 * 60_000 + 32 * 60_000, now - 10 * 60_000 + 32 * 60_000);
        assert_eq!((turns()[0]["retryAt"].as_i64(), turns()[1]["retryAt"].as_i64()), (Some(first), Some(second)));
        assert_eq!(failed(), true);

        // The first is due: tried once, and no longer said to be; the second waits.
        let mut known = Known::new();
        local.retry_lifted_limits(&mut known, now);
        assert_eq!(local.limit_retried().keys().collect::<Vec<_>>(), ["t-1"]);
        assert_eq!(
            (turns()[0]["state"].as_str(), turns()[0]["retryAt"].as_i64(), turns()[1]["retryAt"].as_i64()),
            (Some("failed"), None, Some(second))
        );
        local.retry_lifted_limits(&mut known, now + 60_000);
        assert_eq!(local.limit_retried().len(), 1);

        // A later turn replied: both settle, quietly, with nothing to try.
        local.agent_chats.add_user_message(&chat, "still there?", "t-3").unwrap();
        local.agent_chats.append(&chat, &Message::new(Role::Assistant, "Yes.", Some("t-3".into()))).unwrap();
        let states: Vec<_> = turns()
            .as_array()
            .unwrap()
            .iter()
            .map(|turn| (turn["state"].as_str().unwrap().to_string(), turn["retryAt"].as_i64()))
            .collect();
        assert_eq!(
            states,
            [("settled".to_string(), None), ("settled".to_string(), None), ("completed".to_string(), None)]
        );
        assert_eq!(turns()[1]["error"], limit);
        assert_eq!(failed(), false);
        local.retry_lifted_limits(&mut known, second + 60_000);
        assert_eq!(local.limit_retried().len(), 1);
        std::fs::remove_dir_all(&local.root).ok();
    }
}

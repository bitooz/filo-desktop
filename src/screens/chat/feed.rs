// A chat's timeline: its messages, cards and failed turns in order, with day marks, plus the text helpers the chat
// shows (a quote's plain text).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use super::coding_requests::ChatRequest;
use super::coding_sessions::CodingSession;
use super::types::{ChatTurn, Message, Task};
use crate::format;

/// Two messages share a bubble group within five minutes (MESSAGE_GROUP_WINDOW_MS).
const MESSAGE_GROUP_WINDOW_MS: f64 = 5. * 60. * 1000.;

#[derive(Clone, Debug, PartialEq)]
pub enum FeedItem {
    Message {
        message: Message,
    },
    /// An artifact a reply made, as its card.
    Task(Task),
    /// Coding sessions started together (coding_sessions.rs), placed by `coding_sessions::place`.
    Coding(Vec<CodingSession>),
    /// A coding question handed to the user, where it was asked (`coding_requests::place`).
    CodingRequest(ChatRequest),
    /// A failed turn whose triggering message is hidden or absent from the loaded feed.
    FailedTurn(ChatTurn),
}

impl FeedItem {
    pub(super) fn timestamp(&self) -> f64 {
        match self {
            FeedItem::Message { message, .. } => message.creation_time,
            FeedItem::Task(task) => task.updated_at,
            FeedItem::Coding(sessions) => sessions.first().map_or(0., |session| session.created_at),
            FeedItem::CodingRequest(request) => request.at(),
            FeedItem::FailedTurn(turn) => turn.created_at,
        }
    }
}

/// Unattached failures belong to their triggering event, before newer messages and replacement coding sessions.
/// Failures on loaded user messages render under those bubbles instead; older unloaded failures wait for history.
pub(super) fn place_failed_turns(
    items: Vec<FeedItem>,
    turns: &[ChatTurn],
    messages: &[Message],
    pending_request: Option<&str>,
    loaded_from: Option<f64>,
) -> Vec<FeedItem> {
    let loaded: HashSet<&str> = messages.iter().map(|message| message.id.as_str()).collect();
    let mut failures: Vec<ChatTurn> = turns
        .iter()
        .filter(|turn| {
            turn.state == "failed"
                && turn.user_message_id.as_deref().is_none_or(|id| !loaded.contains(id))
                && pending_request != Some(turn.request_id.as_str())
                && loaded_from.is_none_or(|from| turn.created_at >= from)
        })
        .cloned()
        .collect();
    failures.sort_by(|left, right| {
        left.created_at.total_cmp(&right.created_at).then_with(|| left.turn_id.cmp(&right.turn_id))
    });
    let mut failures = failures.into_iter().peekable();
    let mut placed = Vec::with_capacity(items.len() + failures.len());
    for item in items {
        // Ties follow the triggering event, just as a failure under a loaded bubble does.
        while let Some(turn) = failures.next_if(|turn| turn.created_at < item.timestamp()) {
            placed.push(FeedItem::FailedTurn(turn));
        }
        placed.push(item);
    }
    placed.extend(failures.map(FeedItem::FailedTurn));
    placed
}

/// `buildConversationFeed`: the messages in order, and the artifacts replies made (`tasks`, as cards) between them by
/// time. A reply that says only what its artifact's card says shows as the card.
pub fn build_feed(messages: &[Message], tasks: &[Task]) -> Vec<FeedItem> {
    let mut ordered: Vec<Message> = messages.to_vec();
    // Rows written together share a _creationTime; user rows sort first on ties.
    ordered.sort_by(|left, right| {
        left.creation_time
            .partial_cmp(&right.creation_time)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.is_user().cmp(&left.is_user()))
    });
    let result_message_ids: HashSet<String> = tasks
        .iter()
        .filter_map(|task| {
            ordered
                .iter()
                .rev()
                .find(|message| {
                    !message.is_user()
                        && message.content.trim() == task.summary.trim()
                        && message.creation_time >= task.created_at
                        && message.creation_time <= task.updated_at
                })
                .map(|message| message.id.clone())
        })
        .collect();
    // The cards in one line by time, each placed before the first message after it.
    let mut between: Vec<FeedItem> = tasks.iter().cloned().map(FeedItem::Task).collect();
    between
        .sort_by(|left, right| left.timestamp().partial_cmp(&right.timestamp()).unwrap_or(std::cmp::Ordering::Equal));

    let mut feed = Vec::new();
    let mut between = between.into_iter().peekable();
    for message in &ordered {
        while let Some(item) = between.next_if(|item| item.timestamp() <= message.creation_time) {
            feed.push(item);
        }
        if result_message_ids.contains(&message.id) {
            continue;
        }
        feed.push(FeedItem::Message { message: message.clone() });
    }
    feed.extend(between);
    feed
}

pub fn share_group(first: &Message, second: &Message) -> bool {
    // A line pointing at the agents' group stands apart from the bubbles around it.
    first.group_link.is_none()
        && second.group_link.is_none()
        && first.role == second.role
        && first.speaker == second.speaker
        && format::day_key(first.creation_time) == format::day_key(second.creation_time)
        && second.creation_time - first.creation_time <= MESSAGE_GROUP_WINDOW_MS
}

/// A row of the rendered feed, oldest first.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Gap(String),
    /// A time marker: where the conversation starts, picks up after a pause, or moves to another day.
    Mark(String),
    Item(FeedItem),
}

/// A conversation quiet this long gets a time marker when it picks up again.
const PAUSE_MS: f64 = 60. * 60. * 1000.;

/// Places time markers between the items: "Today · 3:07 PM" where the conversation starts and on each new local day
/// (after a gap), "5:40 PM" where it picks up after an hour or more. Items placed out of time order (a task row under
/// the message that started it) don't count as the conversation moving on.
pub fn day_rows(items: Vec<FeedItem>) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut latest: Option<f64> = None;
    for item in items {
        let at = item.timestamp();
        if at > 0. {
            match latest {
                None => rows.push(Row::Mark(format::day_time(at))),
                Some(previous) if format::day_key(at) != format::day_key(previous) && at > previous => {
                    rows.push(Row::Gap(format::day_key(at).to_string()));
                    rows.push(Row::Mark(format::day_time(at)));
                }
                Some(previous) if at - previous >= PAUSE_MS => rows.push(Row::Mark(format::clock(at))),
                Some(_) => {}
            }
            latest = Some(latest.map_or(at, |previous| previous.max(at)));
        }
        rows.push(Row::Item(item));
    }
    rows
}

/// A reply this long after the message it answers shows its time under it.
const LATE_REPLY_MS: f64 = 2. * 60. * 1000.;

/// The replies that show their time: the last bubble of a group that came more than two minutes after the user's
/// message it answers. Not where a time marker since that message already says when.
pub fn late_replies(rows: &[Row]) -> HashSet<String> {
    let mut late = HashSet::new();
    let mut asked_at: Option<f64> = None;
    let messages: Vec<Option<&Message>> = rows
        .iter()
        .map(|row| match row {
            Row::Item(FeedItem::Message { message, .. }) => Some(message),
            _ => None,
        })
        .collect();
    for (index, row) in rows.iter().enumerate() {
        match (row, messages[index]) {
            (Row::Mark(_), _) => asked_at = None,
            (_, Some(message)) if message.is_user() => asked_at = Some(message.creation_time),
            (_, Some(message)) => {
                let ends_group =
                    !messages.get(index + 1).copied().flatten().is_some_and(|next| share_group(message, next));
                if ends_group && asked_at.is_some_and(|at| message.creation_time - at > LATE_REPLY_MS) {
                    late.insert(message.id.clone());
                }
            }
            _ => {}
        }
    }
    late
}

/// Which message carries each "Stopped" label: the last assistant message after a
/// canceled user message, or the user message itself (value = true when assistant).
pub fn stopped_labels(messages: &[Message], turns: &[ChatTurn]) -> HashMap<String, bool> {
    let canceled: HashSet<&str> = turns
        .iter()
        .filter(|turn| turn.state == "canceled")
        .filter_map(|turn| turn.user_message_id.as_deref())
        .collect();
    let mut labels = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        if !message.is_user() || !canceled.contains(message.id.as_str()) {
            continue;
        }
        let mut carrier = (message.id.clone(), false);
        for next in &messages[index + 1..] {
            if next.is_user() {
                break;
            }
            carrier = (next.id.clone(), true);
        }
        labels.insert(carrier.0, carrier.1);
    }
    labels
}

/// A message's text without its Markdown, for a quote.
pub fn strip_markdown_for_quote(text: &str) -> String {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            (r"\u{200B}", ""),
            (r"!\[([^\]]*)\]\([^)]*\)", "$1"),
            (r"\[([^\]]*)\]\([^)]*\)", "$1"),
            (r"```\w*", ""),
            (r"`{1,2}([^`]*?)`{1,2}", "$1"),
            (r"(?:\*\*\*|___)(.+?)(?:\*\*\*|___)", "$1"),
            (r"(?:\*\*|__)(.+?)(?:\*\*|__)", "$1"),
            (r"(^|[\s(])[*_]([^*_]+?)[*_]([\s).,!?;:'\]-]|$)", "$1$2$3"),
            (r"~~(.+?)~~", "$1"),
            (r"(?m)^[#>]+[ \t]*", ""),
            (r"(?m)^\s*(?:[-*+]|\d+[.)])\s+", ""),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).expect("regex"), replacement))
        .collect()
    });
    let mut out = text.to_string();
    for (pattern, replacement) in rules {
        out = pattern.replace_all(&out, *replacement).to_string();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `markdownToPreviewText`: the plain text of each block, one line per block.
pub fn markdown_preview(markdown: &str) -> String {
    let blocks: Vec<String> = markdown.lines().map(strip_markdown_for_quote).filter(|line| !line.is_empty()).collect();
    blocks.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(id: &str, at: f64, user: Option<&str>) -> ChatTurn {
        serde_json::from_value(serde_json::json!({
            "turnId": id, "requestId": id, "createdAt": at, "userMessageId": user, "state": "failed", "error": id,
        }))
        .unwrap()
    }

    fn message_at(id: &str, at: f64) -> Message {
        serde_json::from_value(serde_json::json!({ "_id": id, "_creationTime": at, "role": "user", "content": id }))
            .unwrap()
    }

    fn item_ids(items: &[FeedItem]) -> Vec<&str> {
        items
            .iter()
            .map(|item| match item {
                FeedItem::Message { message, .. } => message.id.as_str(),
                FeedItem::FailedTurn(turn) => turn.turn_id.as_str(),
                FeedItem::Coding(sessions) => sessions[0].task_id.as_str(),
                _ => panic!("unexpected item"),
            })
            .collect()
    }

    #[test]
    fn unattached_failures_stay_with_their_events_before_new_messages_and_replacement_work() {
        let messages = vec![message_at("first", 10.), message_at("continued", 40.), message_at("confirmed", 60.)];
        // Returned out of order: each failure keeps its own Retry turn id and error, even after more work starts.
        let turns = vec![failure("latest-error", 70., None), failure("session-limit", 20., Some("hidden-trigger"))];
        let items = place_failed_turns(build_feed(&messages, &[]), &turns, &messages, None, None);
        let sessions: Vec<CodingSession> = serde_json::from_value(serde_json::json!([
            { "taskId": "claude-task", "tool": "claude-code", "title": "Old work", "status": "failed", "now": "Failed", "pending": 0., "createdAt": 15., "updatedAt": 20. },
            { "taskId": "codex-task", "tool": "codex", "title": "Replacement", "status": "working", "now": "Working", "pending": 0., "createdAt": 30., "updatedAt": 30. },
        ])).unwrap();
        let items = super::super::coding_sessions::place(items, &sessions, None);
        assert_eq!(
            item_ids(&items),
            ["first", "claude-task", "session-limit", "codex-task", "continued", "confirmed", "latest-error"]
        );
        let failures: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                FeedItem::FailedTurn(turn) => Some((turn.turn_id.as_str(), turn.error.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(failures, [("session-limit", Some("session-limit")), ("latest-error", Some("latest-error"))]);
    }

    #[test]
    fn unattached_failures_wait_for_their_history_and_then_attach_to_loaded_bubbles() {
        let turn = failure("older-error", 10., Some("older-user"));
        let messages = vec![message_at("newer", 30.)];
        let items = || build_feed(&messages, &[]);
        assert_eq!(item_ids(&place_failed_turns(items(), &[turn.clone()], &messages, None, Some(30.))), ["newer"]);
        assert_eq!(
            item_ids(&place_failed_turns(items(), &[turn.clone()], &messages, None, None)),
            ["older-error", "newer"]
        );
        let messages = vec![message_at("older-user", 10.), message_at("newer", 30.)];
        let items = place_failed_turns(build_feed(&messages, &[]), &[turn], &messages, None, None);
        assert_eq!(item_ids(&items), ["older-user", "newer"]);
    }

    #[test]
    fn unattached_failures_exclude_pending_sends_and_resolved_turns() {
        let messages = vec![message_at("sent", 10.), message_at("newer", 40.)];
        let mut resolved = failure("resolved", 20., None);
        resolved.state = "completed".into();
        let mut retrying = failure("retrying", 25., None);
        retrying.state = "pending".into();
        let turns = [failure("pending-send", 15., None), resolved, retrying, failure("unresolved", 30., None)];
        let items = place_failed_turns(build_feed(&messages, &[]), &turns, &messages, Some("pending-send"), None);
        assert_eq!(item_ids(&items), ["sent", "unresolved", "newer"]);
    }

    #[test]
    fn unattached_failures_follow_same_time_events_and_keep_their_day_marker() {
        let messages = vec![message_at("trigger", 1_000.), message_at("next-day", 86_401_000.)];
        let items =
            place_failed_turns(build_feed(&messages, &[]), &[failure("error", 1_000., None)], &messages, None, None);
        assert_eq!(item_ids(&items), ["trigger", "error", "next-day"]);
        let rows = day_rows(items);
        assert!(matches!(&rows[2], Row::Item(FeedItem::FailedTurn(turn)) if turn.turn_id == "error"));
        assert!(matches!(&rows[3], Row::Gap(_)));
    }

    #[test]
    fn an_artifacts_card_sits_by_its_time_in_place_of_the_reply_saying_the_same() {
        let message = |id: &str, at: f64, role: &str, content: &str| -> Message {
            serde_json::from_value(
                serde_json::json!({ "_id": id, "_creationTime": at, "role": role, "content": content }),
            )
            .unwrap()
        };
        let messages = vec![
            message("ask", 10., "user", "Make me a packing list"),
            message("summary", 22., "assistant", "Your packing list"),
            message("later", 40., "assistant", "Anything else?"),
        ];
        let card: Task = serde_json::from_value(serde_json::json!({
            "_id": "artifact-1", "state": "completed", "title": "Packing list", "summary": "Your packing list", "createdAt": 20., "updatedAt": 25.,
        }))
        .unwrap();
        let ids: Vec<String> = build_feed(&messages, &[card])
            .into_iter()
            .map(|item| match item {
                FeedItem::Message { message } => message.id,
                FeedItem::Task(task) => task.id,
                _ => panic!("unexpected item"),
            })
            .collect();
        assert_eq!(ids, ["ask", "artifact-1", "later"]);
    }

    #[test]
    fn marks_the_start_new_days_and_pauses() {
        use chrono::TimeZone as _;
        let at = |day: u32, hour: u32, minute: u32| {
            chrono::Local.with_ymd_and_hms(2025, 3, day, hour, minute, 0).unwrap().timestamp_millis() as f64
        };
        let message = |id: &str, at: f64| -> FeedItem {
            let message: Message = serde_json::from_value(
                serde_json::json!({ "_id": id, "_creationTime": at, "role": "user", "content": id }),
            )
            .unwrap();
            FeedItem::Message { message }
        };
        let rows = day_rows(vec![
            message("a", at(4, 10, 0)),
            message("b", at(4, 10, 50)),
            message("c", at(4, 11, 55)),
            // Placed after c but older: not a pause.
            message("d", at(4, 10, 52)),
            message("e", at(5, 9, 0)),
        ]);
        let shown: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Gap(_) => "gap".into(),
                Row::Mark(label) => label.clone(),
                Row::Item(FeedItem::Message { message, .. }) => message.id.clone(),
                Row::Item(_) => "item".into(),
            })
            .collect();
        assert_eq!(
            shown,
            ["March 4, 2025 · 10:00 AM", "a", "b", "11:55 AM", "c", "d", "gap", "March 5, 2025 · 9:00 AM", "e"]
        );
    }

    #[test]
    fn late_replies_show_their_time() {
        use chrono::TimeZone as _;
        let at = |hour: u32, minute: u32, second: u32| {
            chrono::Local.with_ymd_and_hms(2025, 3, 4, hour, minute, second).unwrap().timestamp_millis() as f64
        };
        let message = |id: &str, role: &str, at: f64| -> FeedItem {
            let message: Message = serde_json::from_value(
                serde_json::json!({ "_id": id, "_creationTime": at, "role": role, "content": id }),
            )
            .unwrap();
            FeedItem::Message { message }
        };
        let rows = day_rows(vec![
            // Answered in seconds: no time, and none under what the user sent.
            message("ask", "user", at(10, 0, 0)),
            message("quick", "assistant", at(10, 0, 20)),
            // Answered in three bubbles, the last after three minutes: the time shows once, under the last.
            message("ask2", "user", at(10, 10, 0)),
            message("slow1", "assistant", at(10, 11, 0)),
            message("slow2", "assistant", at(10, 12, 0)),
            message("slow3", "assistant", at(10, 13, 0)),
            // Two minutes exactly is not late.
            message("ask3", "user", at(10, 20, 0)),
            message("on-time", "assistant", at(10, 22, 0)),
            // After an hour the marker above already says when.
            message("ask4", "user", at(10, 30, 0)),
            message("next-hour", "assistant", at(11, 40, 0)),
        ]);
        let mut late: Vec<String> = late_replies(&rows).into_iter().collect();
        late.sort();
        assert_eq!(late, ["slow3"]);
    }

    #[test]
    fn strips_markdown_for_quote() {
        assert_eq!(strip_markdown_for_quote("## Hi **there** [link](https://x.y)"), "Hi there link");
        assert_eq!(strip_markdown_for_quote("- one\n- `two`"), "one two");
    }
}

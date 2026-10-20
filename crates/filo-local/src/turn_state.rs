// How a chat's turns ended, read from its messages (store.rs). A turn whose last message is an error failed: the chat
// shows the error with Retry, and its row in the lists says "Reply failed". Once a turn in the chat goes through after
// it (a reply in a turn that didn't fail, or one ending quietly as it may, `Chat::last_ok_at`), the error no longer
// stands: the turn is settled, its error a quiet line under what asked for it, and nothing says the reply failed. That
// covers the failed turn itself retried, when it then had nothing to say. A turn that may end quietly (`may_end_quietly`)
// and ended without a message (`without_message`) didn't fail at all, also one saved as failed before it could.
//
// A failure on a plan's usage limit says when the limit resets ("You've hit your session limit · resets 2:50pm"):
// `retry_at` reads when to try it again, shortly after. The app does, once (local_backend/limit_retry.rs).

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, NaiveDate, NaiveTime, TimeZone};

use crate::schedule::at_local;
use crate::store::{Message, Role};

/// How long after a usage limit resets its failed turn is tried again: the reset time is shown to the minute.
const AFTER_RESET: i64 = 2 * 60_000;
/// A reset time without a day that passed this little before the failure is still that day's: the limit hadn't quite
/// lifted yet. Longer ago, it's the next day's.
const JUST_PASSED: i64 = 10 * 60_000;
const DAY: i64 = 24 * 3_600_000;

/// How a turn ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End<'a> {
    /// It replied, ended quietly as it may, or hasn't ended.
    Done,
    /// Its last message is this error, and nothing in the chat went through since.
    Failed(&'a Message),
    /// It failed with this error, and a turn in the chat went through after.
    Settled(&'a Message),
}

/// How a turn that ended without a message fails (turn.rs), read back by `ended_without_message`.
const NO_MESSAGE: &str = " finished without sending a message with Filo's send_message tool.";
const NO_REPLY: &str = " finished without replying.";

/// The error of a turn on `engine` (its label) that ended without sending a message, with the text it wrote instead.
pub fn without_message(engine: &str, final_text: Option<&str>) -> String {
    match final_text {
        Some(text) => format!("{engine}{NO_MESSAGE} What it wrote instead:\n\n{text}"),
        None => format!("{engine}{NO_REPLY}"),
    }
}

/// Whether `error` is a turn's that ended without a message (`without_message`).
fn ended_without_message(error: &str) -> bool {
    let first = error.trim().split("\n\n").next().unwrap_or_default();
    first.contains(NO_MESSAGE) || first.ends_with(NO_REPLY)
}

/// Whether a turn whose messages are `turn` may end without telling the user anything: one Filo's app started on its
/// own (`Message::from_app`: a coding task's news, a routine's run, a check-in).
pub fn may_end_quietly<'a>(mut turn: impl Iterator<Item = &'a Message>) -> bool {
    turn.any(Message::from_app)
}

/// Each turn's end, in the order of the messages that started them. `last_ok_at` is when a turn in the chat last
/// ended without an error (`Chat::last_ok_at`).
pub fn ends(messages: &[Message], last_ok_at: Option<u64>) -> Vec<(&str, End<'_>)> {
    let mut order: Vec<&str> = Vec::new();
    let mut started: HashSet<&str> = HashSet::new();
    let mut last: HashMap<&str, &Message> = HashMap::new();
    for message in messages {
        let Some(turn) = message.turn.as_deref() else { continue };
        if message.role == Role::User && started.insert(turn) {
            order.push(turn);
        }
        last.insert(turn, message);
    }
    // Its last message an error, unless it was one the turn may end with (saved before turn.rs let it).
    let quiet = |turn: &str, error: &Message| {
        ended_without_message(&error.text)
            && may_end_quietly(
                messages.iter().filter(|message| message.role == Role::User && message.turn.as_deref() == Some(turn)),
            )
    };
    let failed_turn = |turn: &str| last.get(turn).is_some_and(|last| last.role == Role::Error && !quiet(turn, last));
    let failed = |message: &Message| message.turn.as_deref().is_some_and(failed_turn);
    // The latest reply in a turn that didn't fail, or the latest turn that went through without one.
    let replied = messages
        .iter()
        .filter(|message| message.role == Role::Assistant && !failed(message))
        .map(|message| message.at)
        .max();
    let went_through = replied.max(last_ok_at);
    order
        .into_iter()
        .map(|turn| {
            let end = match last.get(turn) {
                _ if !failed_turn(turn) => End::Done,
                Some(error) if error.role == Role::Error && went_through.is_some_and(|at| at > error.at) => {
                    End::Settled(error)
                }
                Some(error) if error.role == Role::Error => End::Failed(error),
                _ => End::Done,
            };
            (turn, end)
        })
        .collect()
}

/// Whether a turn in the chat failed and nothing went through since: its row says "Reply failed".
pub fn any_failed(messages: &[Message], last_ok_at: Option<u64>) -> bool {
    ends(messages, last_ok_at).iter().any(|(_, end)| matches!(end, End::Failed(_)))
}

/// When to try a turn that failed with `error` at `failed_at` again on its own: shortly after the usage limit it names
/// resets (`resets_at`). None for any other error.
pub fn retry_at<Tz: TimeZone>(error: &str, failed_at: i64, tz: &Tz) -> Option<i64> {
    resets_at(error, failed_at, tz).map(|at| at + AFTER_RESET)
}

/// When the usage limit `error` names lifts: Claude Code's "resets 2:50pm (Asia/Tashkent)" or "resets Oct 16, 9am",
/// its older "usage limit reached|1751234567", and Codex's "try again in 1 hour 31 minutes" or "try again at 3:12 PM".
/// A time with no day is the first one after the failure (`failed_at`) in `tz`, this Mac's time zone, which the zone
/// the CLI names is taken to be. None for any other error, or a limit that says no time.
pub fn resets_at<Tz: TimeZone>(error: &str, failed_at: i64, tz: &Tz) -> Option<i64> {
    // The error's own words: what follows the first paragraph is the chat's advice (engine.rs `explain`).
    let text = error.trim().split("\n\n").next().unwrap_or_default().to_lowercase();
    if !text.contains("limit") {
        return None;
    }
    if let Some((_, rest)) = text.split_once("limit reached|") {
        let seconds: String = rest.chars().take_while(char::is_ascii_digit).collect();
        return seconds.parse::<i64>().ok().map(|seconds| seconds * 1000);
    }
    if let Some((_, rest)) = text.split_once("try again in ") {
        return span(rest).map(|ms| failed_at + ms);
    }
    let rest = ["resets at ", "reset at ", "resets ", "try again at "]
        .iter()
        .find_map(|lead| text.split_once(lead).map(|(_, rest)| rest))?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut next = 0;
    let date = match words.first().and_then(|word| month(word.trim_end_matches([',', '.']))) {
        Some(month) => {
            let day: u32 =
                words.get(1)?.trim_end_matches(',').trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()?;
            let year = words
                .get(2)
                .and_then(|word| word.trim_end_matches(',').parse::<i32>().ok())
                .filter(|year| *year > 2000);
            next = if year.is_some() { 3 } else { 2 };
            Some((month, day, year))
        }
        None => None,
    };
    if words.get(next) == Some(&"at") {
        next += 1;
    }
    let time = clock(&words.get(next..)?.join(" "))?;
    let failed = tz.timestamp_millis_opt(failed_at).earliest()?;
    match date {
        Some((month, day, year)) => {
            let on =
                |year: i32| NaiveDate::from_ymd_opt(year, month, day).map(|date| at_local(tz, date.and_time(time)));
            let at = on(year.unwrap_or(failed.year()))?;
            // "Jan 2" said on Dec 30 is next year's.
            if year.is_none() && at < failed_at - DAY { on(failed.year() + 1) } else { Some(at) }
        }
        None => {
            let today = failed.date_naive();
            let at = at_local(tz, today.and_time(time));
            if at < failed_at - JUST_PASSED { Some(at_local(tz, today.succ_opt()?.and_time(time))) } else { Some(at) }
        }
    }
}

/// "oct" or "october": its number.
fn month(word: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    (word.len() >= 3)
        .then(|| MONTHS.iter().position(|month| word.starts_with(month)))
        .flatten()
        .map(|index| index as u32 + 1)
}

/// "2:50pm", "9am", "3:12 pm", "15:12" at the start of `text`.
fn clock(text: &str) -> Option<NaiveTime> {
    let hour_digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    let mut hour: u32 = hour_digits.parse().ok()?;
    let mut rest = &text[hour_digits.len()..];
    let mut minute = 0;
    if let Some(after) = rest.strip_prefix(':') {
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        minute = digits.parse().ok()?;
        rest = &after[digits.len()..];
    }
    let rest = rest.trim_start();
    if rest.starts_with("pm") || rest.starts_with("p.m") {
        hour = if hour == 12 { 12 } else { hour + 12 };
    } else if (rest.starts_with("am") || rest.starts_with("a.m")) && hour == 12 {
        hour = 0;
    }
    NaiveTime::from_hms_opt(hour, minute, 0)
}

/// "1 hour 31 minutes", "2 days", "45m" at the start of `text`, in milliseconds.
fn span(text: &str) -> Option<i64> {
    let mut total = 0;
    let mut words = text.split_whitespace().peekable();
    while let Some(word) = words.next() {
        let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
        let Ok(count) = digits.parse::<i64>() else { break };
        let attached = &word[digits.len()..];
        let unit = if attached.is_empty() { words.next().unwrap_or_default() } else { attached };
        let unit = unit.trim_end_matches([',', '.']);
        let size = match unit {
            "d" | "day" | "days" => DAY,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000,
            "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
            "s" | "sec" | "secs" | "second" | "seconds" => 1_000,
            _ => break,
        };
        total += count * size;
        if words.peek() == Some(&"and") {
            words.next();
        }
    }
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, NaiveDateTime};

    fn message(role: Role, turn: &str, at: u64) -> Message {
        let mut message = Message::new(role, format!("{role:?} {turn}"), Some(turn.into()));
        message.at = at;
        message
    }

    fn states(messages: &[Message], last_ok_at: Option<u64>) -> Vec<(String, &'static str)> {
        let name = |end: &End| match end {
            End::Done => "done",
            End::Failed(_) => "failed",
            End::Settled(_) => "settled",
        };
        ends(messages, last_ok_at).iter().map(|(turn, end)| (turn.to_string(), name(end))).collect()
    }

    /// As on Oct 13: two turns failed on the session limit, the first retried and replied; the second, retried, went
    /// through with nothing to say. Neither stands as failed after; before that reply, both did.
    #[test]
    fn a_failure_settles_once_a_later_turn_goes_through() {
        let mut messages = vec![
            message(Role::User, "a", 10),
            message(Role::Error, "a", 20),
            message(Role::User, "b", 30),
            message(Role::Error, "b", 40),
        ];
        assert_eq!(states(&messages, None), [("a".into(), "failed"), ("b".into(), "failed")]);
        assert!(any_failed(&messages, None));
        messages.push(message(Role::Assistant, "a", 50));
        assert_eq!(states(&messages, None), [("a".into(), "done"), ("b".into(), "settled")]);
        assert!(!any_failed(&messages, None));
        // A reply from before the failure doesn't settle it; a turn that went through quietly after it does.
        let messages = vec![
            message(Role::User, "a", 10),
            message(Role::Assistant, "a", 15),
            message(Role::User, "b", 30),
            message(Role::Error, "b", 40),
        ];
        assert_eq!(states(&messages, Some(35)), [("a".into(), "done"), ("b".into(), "failed")]);
        assert_eq!(states(&messages, Some(45)), [("a".into(), "done"), ("b".into(), "settled")]);
        // A reply in a turn that failed after it isn't one that went through.
        let messages = vec![
            message(Role::User, "a", 10),
            message(Role::Error, "a", 20),
            message(Role::User, "b", 30),
            message(Role::Assistant, "b", 35),
            message(Role::Error, "b", 40),
        ];
        assert_eq!(states(&messages, None), [("a".into(), "failed"), ("b".into(), "failed")]);
    }

    /// As in Animation Developer's chat on Oct 21: a check-in saved before it had its mark (known by its words) ended
    /// without a message three times, saved as errors then; it didn't fail. Nor did a marked one, or a coding task's
    /// news. The user's own message ending that way still failed, as did a check-in on anything else.
    #[test]
    fn a_turn_that_may_end_quietly_didnt_fail_when_it_did() {
        let quiet = without_message("Claude Code", Some("Nothing needed doing at this check-in."));
        let error = |turn: &str, text: &str, at: u64| Message { text: text.into(), ..message(Role::Error, turn, at) };
        let check_in = |turn: &str, at: u64| Message {
            text: format!("{}Destroy the Vast GPU when the round is done", crate::store::CHECK_IN),
            ..message(Role::User, turn, at)
        };
        let messages = vec![
            check_in("old", 10),
            error("old", &quiet, 20),
            error("old", &quiet, 30),
            error("old", &without_message("Claude Code", None), 40),
            Message { check_in: true, ..message(Role::User, "marked", 50) },
            error("marked", &quiet, 60),
            Message { hidden: true, ..message(Role::User, "news", 70) },
            error("news", &without_message("Codex", None), 80),
            message(Role::User, "user", 90),
            error("user", &quiet, 100),
            check_in("limit", 110),
            error("limit", "You've hit your session limit · resets 2:50pm", 120),
        ];
        assert_eq!(
            states(&messages, None),
            [
                ("old".into(), "done"),
                ("marked".into(), "done"),
                ("news".into(), "done"),
                ("user".into(), "failed"),
                ("limit".into(), "failed")
            ]
        );
        assert!(
            ended_without_message(&quiet)
                && quiet.ends_with("What it wrote instead:\n\nNothing needed doing at this check-in.")
        );
    }

    fn at(tz: &FixedOffset, text: &str) -> i64 {
        at_local(tz, NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap())
    }

    #[test]
    fn a_usage_limit_says_when_it_resets() {
        let tz = FixedOffset::east_opt(5 * 3600).unwrap();
        let failed = at(&tz, "2026-10-13 14:28");
        let resets = |error: &str| resets_at(error, failed, &tz);
        let advice =
            "\n\nThat's your Claude Code plan's limit, not Filo's: this chat picks up where it left off once it lifts.";
        assert_eq!(
            resets(&format!("Claude Code: You've hit your session limit · resets 2:50pm (Asia/Tashkent){advice}")),
            Some(at(&tz, "2026-10-13 14:50"))
        );
        assert_eq!(
            retry_at("Claude Code: You've hit your session limit · resets 2:50pm (Asia/Tashkent)", failed, &tz),
            Some(at(&tz, "2026-10-13 14:52"))
        );
        // Past already: tomorrow's, unless it only just passed.
        assert_eq!(
            resets("Claude Code: You've hit your session limit · resets 3:50am (Asia/Tashkent)"),
            Some(at(&tz, "2026-10-14 03:50"))
        );
        assert_eq!(
            resets("Claude Code: You've hit your session limit · resets 2:20pm"),
            Some(at(&tz, "2026-10-13 14:20"))
        );
        assert_eq!(resets("5-hour limit reached ∙ resets 3pm"), Some(at(&tz, "2026-10-13 15:00")));
        assert_eq!(
            resets("Claude Code: You've hit your weekly limit · resets Oct 16, 9am (Asia/Tashkent)"),
            Some(at(&tz, "2026-10-16 09:00"))
        );
        assert_eq!(resets("Opus weekly limit reached ∙ resets Jan 2 at 12am"), Some(at(&tz, "2027-01-02 00:00")));
        assert_eq!(resets("Claude AI usage limit reached|1791900000"), Some(1_791_900_000_000));
        assert_eq!(
            resets("Codex: You've hit your usage limit. Upgrade to Pro or try again in 1 hour 31 minutes."),
            Some(failed + 91 * 60_000)
        );
        assert_eq!(
            resets("Codex: You've hit your usage limit. Try again at 3:12 PM."),
            Some(at(&tz, "2026-10-13 15:12"))
        );
        // Not a limit, or no time.
        assert_eq!(resets("Claude Code: connection reset at 2:50pm"), None);
        assert_eq!(resets("Claude Code: You've hit your session limit"), None);
        assert_eq!(resets("Claude Code isn't installed on this Mac"), None);
    }
}

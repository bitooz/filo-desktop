// A fresh session seeded with the conversation's own words. When a chat's session can't be resumed (it went cold, its
// engine changed, or the CLI lost it), its turn starts a new one whose prompt opens with the chat's newest messages
// word for word (`chat_seed`, turn.rs). Nothing is summarized: the seed is a view of the record, which stays whole on
// disk for the engine to read (the chat's messages file).
//
// How the seed is made:
// - The newest whole messages, oldest first, up to `BUDGET` tokens; at least `AT_LEAST` of them however long, up to
//   `CAP`. Each is headed with who wrote it, when (the user's local time) and its id, for looking it up. For a model
//   with a small window, a tenth of it and half as much again (`sizes`: 20k and 30k on 200k), so the fresh session
//   starts well below where its CLI would compact (compaction.rs `full_from`).
// - Exact repeats of what's shown above (a routine's prompt every run, the boilerplate of the app's reports and of the
//   group's relays) become "[same as message <id>]", a whole message, or "[same as in message <id>]", a block of one.
// - Filo's app's reports and the group's relays past the newest `WHOLE_REPORTS` messages shrink to a line, the id
//   saying where the rest is. Errors, failures and what the user said are never shortened.
// - A header says how many older messages aren't shown, their dates and the file they're in.
// Tokens are counted as four characters each.

use std::collections::HashMap;
use std::path::Path;

use chrono::TimeZone;

use crate::attachments;
use crate::store::{Message, Role};

/// The newest messages that fit in this many tokens are shown.
const BUDGET: usize = 40_000;
/// At least this many are shown, past `BUDGET`…
const AT_LEAST: usize = 8;
/// …up to this many tokens.
const CAP: usize = 60_000;
/// The app's reports among the newest this many messages are shown whole.
const WHOLE_REPORTS: usize = 10;
/// A block of a message this long that's shown above already is pointed to, not shown again.
const REPEAT_BLOCK: usize = 200;
/// A whole message this long that's shown above already is pointed to.
const REPEAT_MESSAGE: usize = 80;
/// A shortened message keeps this much of its line.
const SHORT_LINE: usize = 300;

/// The seed's budget and cap for a model whose context window is `window` (unknown: `BUDGET` and `CAP`).
fn sizes(window: Option<u64>) -> (usize, usize) {
    let budget = window.map_or(BUDGET, |window| (window / 10) as usize).min(BUDGET);
    (budget, (budget + budget / 2).min(CAP))
}

/// About how many tokens `text` takes.
pub fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// A message as the seed shows it.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    pub id: String,
    /// Milliseconds since the epoch.
    pub at: i64,
    pub who: String,
    pub text: String,
    /// The line it shrinks to once it's older than the newest `WHOLE_REPORTS` (an app's report, a relay); none: always
    /// whole.
    pub short: Option<String>,
}

/// The newest entries that fit, as the seed shows them.
#[derive(Clone, Debug, PartialEq)]
struct Window {
    pub text: String,
    /// How many entries are shown, and how many older ones aren't.
    pub shown: usize,
    pub older: usize,
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end())
    }
}

fn time<Tz: TimeZone>(tz: &Tz, at: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    tz.timestamp_millis_opt(at)
        .earliest()
        .map(|time| time.format("%a %-d %b %Y, %H:%M").to_string())
        .unwrap_or_default()
}

fn day<Tz: TimeZone>(tz: &Tz, at: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    tz.timestamp_millis_opt(at).earliest().map(|time| time.format("%-d %b %Y").to_string()).unwrap_or_default()
}

/// "3 Oct 2026 to 12 Oct 2026", or one day.
fn days<Tz: TimeZone>(tz: &Tz, entries: &[Entry]) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (Some(first), Some(last)) = (entries.first(), entries.last()) else { return String::new() };
    let (from, to) = (day(tz, first.at), day(tz, last.at));
    if from == to { format!("on {from}") } else { format!("from {from} to {to}") }
}

/// `entries[from..]` as the seed shows them, oldest first: each headed, repeats pointed to, old reports shortened.
fn render<Tz: TimeZone>(entries: &[Entry], from: usize, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let total = entries.len();
    let mut whole: HashMap<&str, &str> = HashMap::new();
    let mut blocks: HashMap<&str, &str> = HashMap::new();
    let mut out: Vec<String> = Vec::new();
    for (index, entry) in entries.iter().enumerate().skip(from) {
        let text = entry.text.trim();
        let body = match &entry.short {
            Some(line) if total - index > WHOLE_REPORTS => {
                format!(
                    "{} [shortened: message {} in the history file has it whole]",
                    cut(line.trim(), SHORT_LINE),
                    entry.id
                )
            }
            _ => {
                let body = match whole.get(text) {
                    Some(id) if text.chars().count() >= REPEAT_MESSAGE => format!("[same as message {id}]"),
                    _ => text
                        .split("\n\n")
                        .map(|block| match blocks.get(block.trim()) {
                            Some(id) if block.trim().chars().count() >= REPEAT_BLOCK => {
                                format!("[same as in message {id}]")
                            }
                            _ => block.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                };
                whole.entry(text).or_insert(&entry.id);
                for block in text.split("\n\n").map(str::trim).filter(|block| block.chars().count() >= REPEAT_BLOCK) {
                    blocks.entry(block).or_insert(&entry.id);
                }
                body
            }
        };
        out.push(format!("[{} · {} · message {}]\n{body}", entry.who, time(tz, entry.at), entry.id));
    }
    out.join("\n\n")
}

/// The newest entries that fit: as many as `budget` takes, at least `AT_LEAST` within `cap`, and the newest one cut to
/// `cap` when even it is longer (`sizes`).
fn window<Tz: TimeZone>(entries: &[Entry], (budget, cap): (usize, usize), tz: &Tz) -> Window
where
    Tz::Offset: std::fmt::Display,
{
    let total = entries.len();
    let cost = |from: usize| tokens(&render(entries, from, tz));
    // An older message only adds to what the newer ones cost (a repeat it starts saves less than it shows), so the
    // first that fits is found by halving.
    let first_within = |budget: usize, low: usize| {
        let (mut low, mut high) = (low, total);
        while low < high {
            let middle = (low + high) / 2;
            if cost(middle) <= budget { high = middle } else { low = middle + 1 }
        }
        low
    };
    let mut from = first_within(budget, 0);
    if total - from < AT_LEAST {
        from = first_within(cap, total.saturating_sub(AT_LEAST)).min(from);
    }
    if from == total && total > 0 {
        let newest = &entries[total - 1];
        let room = (cap - 1_000) * 4;
        let text =
            format!("{} [cut here: message {} in the history file has it whole]", cut(&newest.text, room), newest.id);
        let cut = Entry { text, short: None, ..newest.clone() };
        return Window { text: render(std::slice::from_ref(&cut), 0, tz), shown: 1, older: total - 1 };
    }
    Window { text: render(entries, from, tz), shown: total - from, older: from }
}

/// What the header says of the window and what's older, and where all of it is.
fn header<Tz: TimeZone>(entries: &[Entry], window: &Window, record: Option<&Path>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let plural = |count: usize| if count == 1 { "message" } else { "messages" };
    let shown = &entries[window.older..];
    let mut parts = vec![format!(
        "Here are its newest {} {} word for word, oldest first ({}), each headed with who it's from, when (the user's local time) and its id. \
A message that repeats one above says which; an old report from Filo's app or relay from the group is cut to its first line.",
        window.shown,
        plural(window.shown),
        days(tz, shown)
    )];
    if window.older > 0 {
        parts.push(format!(
            "The {} {} before them ({}) aren't shown.",
            window.older,
            plural(window.older),
            days(tz, &entries[..window.older])
        ));
        if let Some(file) = record {
            parts.push(format!(
                "All of it is kept word for word in {} (one JSON message per line, with its id): when you need something from before, or the whole of a shortened one, search it by id or by words, and read the ones around what you find.",
                file.display()
            ));
        }
        return parts.join(" ");
    }
    // The whole of it is here: the file has nothing more, but for what's shortened.
    parts.push("That's all of it, from its first message: there's nothing older to look up.".to_string());
    let total = entries.len();
    let shortened =
        entries.iter().enumerate().any(|(index, entry)| entry.short.is_some() && total - index > WHOLE_REPORTS)
            || window.text.contains("[cut here: ");
    if let (Some(file), true) = (record, shortened) {
        parts.push(format!(
            "The whole of a shortened one is in {} (one JSON message per line, with its id).",
            file.display()
        ));
    }
    parts.join(" ")
}

/// Who a chat's message is from, as the seed heads it.
fn chat_who(message: &Message) -> String {
    match message.role {
        Role::User if message.group_call.is_some() => {
            match message.group_call.as_ref().and_then(|call| call.from.as_ref()) {
                Some(member) => format!("{} in the group chat, relayed by Filo's app", member.name),
                None => "The user in the group chat, relayed by Filo's app".into(),
            }
        }
        Role::User if message.routine.is_some() => format!(
            "Filo's app: the routine \"{}\" running",
            message.routine.as_ref().map(|routine| routine.title.as_str()).unwrap_or_default()
        ),
        Role::User if message.hidden => "Filo's app (to you, not from the user)".into(),
        Role::User => "User".into(),
        Role::Error => "Filo's app: this error showed in the chat".into(),
        _ if message.in_group.is_some() => "You (posted in the group chat)".into(),
        _ => "You".into(),
    }
}

/// The line an old message shrinks to: the app's report's first line, a relay's ask. None for what's never shortened:
/// what the user or the engine said, errors, and reports of a failure or of the user's own answer.
fn chat_short(message: &Message) -> Option<String> {
    if message.role != Role::User {
        return None;
    }
    if let Some(call) = &message.group_call {
        return Some(format!(
            "Asked: {}",
            call.ask.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default()
        ));
    }
    if !message.hidden {
        return None;
    }
    let first = message.text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    let keep = ["fail", "error", "answered by the user"].iter().any(|word| first.to_lowercase().contains(word));
    (!keep).then(|| first_sentence(first).to_string())
}

/// A line's first sentence (to a ". " before a capital, past its first few words), or all of it: an app report's
/// "[Coding task round 2 done] Task "Blue" in repo, run by Claude Code (task <id>)." without what every report
/// says after it.
fn first_sentence(line: &str) -> &str {
    line.match_indices(". ")
        .find(|(at, _)| *at >= 20 && line[at + 2..].starts_with(char::is_uppercase))
        .map_or(line, |(at, _)| &line[..=at])
}

fn chat_entry(message: &Message) -> Entry {
    // An agent's own files are named with where they are, so it can find one it sent to change it.
    let files = attachments::names(&message.attachments, message.role == Role::Assistant);
    let text =
        [message.text.trim(), &files].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n");
    Entry { id: message.id.clone(), at: message.at as i64, who: chat_who(message), text, short: chat_short(message) }
}

/// What a new session is told of the chat before the turn's message `turn_id`: its newest messages word for word, and
/// where the rest is (`record`, the chat's messages file), sized for the turn's model's context `window` (`sizes`).
/// None when there's nothing before it.
pub fn chat_seed<Tz: TimeZone>(
    messages: &[Message],
    turn_id: &str,
    record: Option<&Path>,
    window: Option<u64>,
    tz: &Tz,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let entries: Vec<Entry> =
        messages.iter().filter(|message| message.turn.as_deref() != Some(turn_id)).map(chat_entry).collect();
    if entries.is_empty() {
        return None;
    }
    let window = self::window(&entries, sizes(window), tz);
    Some(format!(
        "You're picking this conversation up in a new session, so you don't remember it. {}\n\nThey were answered already: don't act on them again, and carry on from the new message after them.\n\n{}",
        header(&entries, &window, record, tz),
        window.text
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GroupCall, LinkedMember, RoutineRun};

    use crate::test_support::tz;

    /// 2026-10-12 09:00 UTC, and a minute per step after.
    const AT: i64 = 1_791_795_600_000;

    fn message(id: &str, role: Role, text: &str, turn: &str, minute: i64) -> Message {
        let mut message = Message::new(role, text, Some(turn.into()));
        message.id = id.into();
        message.at = (AT + minute * 60_000) as u64;
        message
    }

    #[test]
    fn a_seed_shows_the_newest_messages_whole_with_who_when_and_their_id() {
        let mut hidden = message(
            "m3",
            Role::User,
            "[Coding task round 1 done] Task \"Blue\" in repo, run by Claude Code (task t1).\nDetails…",
            "t2",
            3,
        );
        hidden.hidden = true;
        let messages = vec![
            message("m1", Role::User, "Make the button blue.", "t1", 0),
            message("m2", Role::Assistant, "Done: it's blue.", "t1", 1),
            message("m-err", Role::Error, "Claude Code: You've hit your session limit", "t1", 2),
            hidden,
            message("m4", Role::User, "And bigger?", "t3", 4),
        ];
        let seed = chat_seed(&messages, "t3", Some(Path::new("/chats/c1/messages.jsonl")), None, &tz()).unwrap();
        assert!(seed.starts_with("You're picking this conversation up in a new session"), "{seed}");
        assert!(seed.contains("Here are its newest 4 messages word for word, oldest first (on 12 Oct 2026)"), "{seed}");
        // Nothing older, nothing shortened: said so, and no file to search.
        assert!(seed.contains("That's all of it, from its first message: there's nothing older to look up."), "{seed}");
        assert!(!seed.contains("before them") && !seed.contains("/chats/c1/messages.jsonl"), "{seed}");
        assert!(seed.contains("[User · Mon 12 Oct 2026, 14:00 · message m1]\nMake the button blue.\n\n[You · Mon 12 Oct 2026, 14:01 · message m2]\nDone: it's blue."), "{seed}");
        // Errors are in it now, and the app's report is whole while it's recent.
        assert!(seed.contains("[Filo's app: this error showed in the chat · Mon 12 Oct 2026, 14:02 · message m-err]\nClaude Code: You've hit your session limit"), "{seed}");
        assert!(seed.contains("[Filo's app (to you, not from the user) · Mon 12 Oct 2026, 14:03 · message m3]\n[Coding task round 1 done] Task \"Blue\" in repo, run by Claude Code (task t1).\nDetails…"), "{seed}");
        // The turn's own message isn't in it: the prompt says it after.
        assert!(!seed.contains("And bigger?"));
        assert_eq!(chat_seed(&messages[4..], "t3", None, None, &tz()), None);
    }

    #[test]
    fn repeats_point_to_the_message_shown_above() {
        let boilerplate = "This is Filo's app telling you, not a message from the user. ".repeat(5);
        let routine = "⏰ Routine \"Feed\" (Every 6 h). Look for new posts on the topic and publish what's new; stay quiet when there's nothing.".repeat(2);
        let mut runs = Vec::new();
        for (index, turn) in ["t1", "t2", "t3"].iter().enumerate() {
            let mut run = message(&format!("r{index}"), Role::User, &routine, turn, index as i64 * 10);
            run.routine = Some(RoutineRun { title: "Feed".into(), when: "every 6 hours".into() });
            runs.push(run);
            runs.push(message(
                &format!("a{index}"),
                Role::Assistant,
                &format!("Round {index} done.\n\n{boilerplate}"),
                turn,
                index as i64 * 10 + 1,
            ));
        }
        runs.push(message("now", Role::User, "hi", "t9", 60));
        let seed = chat_seed(&runs, "t9", None, None, &tz()).unwrap();
        assert_eq!(seed.matches(routine.as_str()).count(), 1, "{seed}");
        assert!(
            seed.contains(
                "[Filo's app: the routine \"Feed\" running · Mon 12 Oct 2026, 14:10 · message r1]\n[same as message r0]"
            ),
            "{seed}"
        );
        assert_eq!(seed.matches(boilerplate.trim()).count(), 1, "{seed}");
        assert!(seed.contains("message a2]\nRound 2 done.\n\n[same as in message a0]"), "{seed}");
        // Short repeats ("ok") stay as they are.
        let oks = vec![
            message("o1", Role::User, "ok", "t1", 0),
            message("o2", Role::User, "ok", "t2", 1),
            message("n", Role::User, "?", "t3", 2),
        ];
        assert!(chat_seed(&oks, "t3", None, None, &tz()).unwrap().ends_with("message o2]\nok"));
    }

    #[test]
    fn old_reports_and_relays_shrink_to_a_line_but_failures_and_answers_stay_whole() {
        let mut messages = Vec::new();
        let report = |id: &str, first: &str, minute: i64| {
            let mut report = message(
                id,
                Role::User,
                &format!("{first}\n\nThe brief was: …\n\nThe round's report: a long report."),
                id,
                minute,
            );
            report.hidden = true;
            report
        };
        messages.push(report("done", "[Coding task round 1 done] Task \"Blue\" in repo, run by Claude Code (task t1). This is Filo's app telling you.", 0));
        messages.push(report(
            "failed",
            "[Coding task round 2 failed] Task \"Blue\" in repo, run by Claude Code (task t1).",
            1,
        ));
        messages.push(report(
            "answered",
            "[Coding task question answered by the user] Task \"Blue\" in repo, run by Codex (task t1).",
            2,
        ));
        let mut relay = message(
            "relay",
            Role::User,
            "[From the group chat of Filo and the agents …]\n\nMembers: …\n\nChef asks: dinner?",
            "relay",
            3,
        );
        relay.group_call = Some(GroupCall {
            speaker: crate::apps::FILO_AGENT.into(),
            from: Some(LinkedMember { speaker: "chef".into(), name: "Chef".into() }),
            message_id: None,
            ask: "Dinner at 8?\nWith the usual crowd.".into(),
            context: String::new(),
            answers_user: false,
            after_question: false,
            checking: false,
        });
        messages.push(relay);
        for minute in 0..WHOLE_REPORTS as i64 {
            messages.push(message(&format!("u{minute}"), Role::User, "next", &format!("n{minute}"), 10 + minute));
        }
        messages.push(message("now", Role::User, "go", "now", 30));
        let seed = chat_seed(&messages, "now", Some(Path::new("/m.jsonl")), None, &tz()).unwrap();
        // Nothing older, but shortened ones: the file has them whole.
        assert!(
            seed.contains("there's nothing older to look up. The whole of a shortened one is in /m.jsonl"),
            "{seed}"
        );
        assert!(seed.contains("message done]\n[Coding task round 1 done] Task \"Blue\" in repo, run by Claude Code (task t1). [shortened: message done in the history file has it whole]"), "{seed}");
        assert!(seed.contains("[Chef in the group chat, relayed by Filo's app · Mon 12 Oct 2026, 14:03 · message relay]\nAsked: Dinner at 8? [shortened"), "{seed}");
        assert!(seed.contains("message failed]\n[Coding task round 2 failed] Task \"Blue\" in repo, run by Claude Code (task t1).\n\nThe brief was"), "{seed}");
        assert!(seed.contains("message answered]\n[Coding task question answered by the user] Task \"Blue\" in repo, run by Codex (task t1).\n\nThe brief was"), "{seed}");
    }

    #[test]
    fn the_window_takes_the_budget_at_least_eight_messages_and_never_past_the_cap() {
        let tokens_each = |count: usize, each: usize| -> Vec<Message> {
            let mut messages: Vec<Message> = (0..count)
                .map(|index| {
                    message(
                        &format!("m{index}"),
                        Role::User,
                        &format!("{index} {}", "x".repeat(each * 4)),
                        &format!("t{index}"),
                        index as i64,
                    )
                })
                .collect();
            messages.push(message("now", Role::User, "go", "now", 10_000));
            messages
        };
        let count = |seed: &str| seed.matches("· message m").count();
        // 1k-token messages: 40k of them, about 39 (each has its header too); the older ones counted, with their days.
        let seed = chat_seed(&tokens_each(100, 1_000), "now", Some(Path::new("/m.jsonl")), None, &tz()).unwrap();
        let shown = count(&seed);
        assert!((35..40).contains(&shown), "{shown}");
        assert!(tokens(&seed) <= BUDGET + 600, "{}", tokens(&seed));
        assert!(seed.contains(&format!("The {} messages before them (on 12 Oct 2026) aren't shown. All of it is kept word for word in /m.jsonl", 100 - shown)), "{seed}");
        assert!(
            seed.contains(&format!("message m{}]", 100 - shown))
                && !seed.contains(&format!("message m{}]", 99 - shown))
        );
        // 6k-token ones: 8 of them anyway (48k)…
        assert_eq!(count(&chat_seed(&tokens_each(20, 6_000), "now", None, None, &tz()).unwrap()), AT_LEAST);
        // …but 9k ones only as many as 60k takes.
        assert_eq!(count(&chat_seed(&tokens_each(20, 9_000), "now", None, None, &tz()).unwrap()), 6);
        // One message longer than the cap: the newest, cut.
        let seed = chat_seed(&tokens_each(3, 70_000), "now", None, None, &tz()).unwrap();
        assert_eq!(count(&seed), 1);
        assert!(
            seed.contains("[cut here: message m2 in the history file has it whole]") && tokens(&seed) <= CAP,
            "{}",
            tokens(&seed)
        );
        // For a 200k window, a tenth of it: about 19 of the 1k ones, 3k ones 8 within 30k, a long one cut to 30k.
        assert_eq!(sizes(Some(200_000)), (20_000, 30_000));
        assert_eq!(
            (sizes(Some(1_000_000)), sizes(Some(475_000)), sizes(None)),
            ((BUDGET, CAP), (BUDGET, CAP), (BUDGET, CAP))
        );
        let seed = chat_seed(&tokens_each(100, 1_000), "now", None, Some(200_000), &tz()).unwrap();
        assert!((17..20).contains(&count(&seed)) && tokens(&seed) <= 20_600, "{} {}", count(&seed), tokens(&seed));
        assert_eq!(count(&chat_seed(&tokens_each(20, 3_000), "now", None, Some(200_000), &tz()).unwrap()), AT_LEAST);
        let seed = chat_seed(&tokens_each(3, 70_000), "now", None, Some(200_000), &tz()).unwrap();
        assert!(seed.contains("[cut here: ") && tokens(&seed) <= 30_000, "{}", tokens(&seed));
    }
}

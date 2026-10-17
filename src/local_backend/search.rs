// Search on this Mac: `agents:searchMessages`, the words of the agents' conversations and the group's for ⌘P's search
// box (src/screens/search.rs). Each message holding every word typed, newest first, as a line around the first word
// found, with how far from the newest message it is, so its chat can load back that far to show it.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{Local, arg};

/// The functions answered here (checked by a test, like `MOVED`).
#[cfg(test)]
pub const SEARCH: &[&str] = &["agents:searchMessages"];

/// Most messages a search gives back.
const MAX_FOUND: usize = 30;
/// A found message's line: how much of it before the first word found, and how long.
const LEAD_CHARS: usize = 28;
const LINE_CHARS: usize = 140;

/// A message that holds the words: which chat (an agent's id, or `GROUP`), its id, its words, when.
struct Found {
    chat: String,
    message: String,
    text: String,
    at: f64,
    /// 1 for the chat's newest message shown, 2 for the one before…
    depth: usize,
}

/// The group's place in what's found, as the Agents tab names it (screens/agents.rs `GROUP`).
const GROUP: &str = "group";

impl Local {
    /// Answers the search's functions; `None` for any other path.
    pub(super) fn search_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        match path {
            "agents:searchMessages" => Some(self.search_messages(arg(args, "query").unwrap_or_default())),
            _ => None,
        }
    }

    fn search_messages(&self, query: &str) -> Result<Value, String> {
        let words = words_of(query);
        if words.is_empty() {
            return Ok(json!([]));
        }
        let mut found = Vec::new();
        for agent in self.agents() {
            let Some(session) = agent.session_id.as_deref() else { continue };
            // What the chat shows (as `voiceSessions:detail`), less a routine's instructions and the lines pointing at
            // posts in the group (the group's own message is found there).
            let shown = filo_local::store::shown(self.agent_chats.messages(session).unwrap_or_default());
            let count = shown.len();
            for (index, message) in shown.iter().enumerate() {
                if message.in_group.is_some() || super::routine_run(message).is_some() {
                    continue;
                }
                let text = filo_local::store::preview(&message.summary(), usize::MAX);
                if holds(&text, &words) {
                    found.push(Found {
                        chat: agent.id.clone(),
                        message: message.id.clone(),
                        text,
                        at: message.at as f64,
                        depth: count - index,
                    });
                }
            }
        }
        if let Some(db) = self.group_session().and_then(|_| self.db().ok()) {
            let rows: Vec<(String, String, String, i64)> = db.with(|conn| {
                let mut statement = conn.prepare("SELECT id, role, text, at FROM group_messages ORDER BY seq DESC")?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
                rows.collect()
            })?;
            for (index, (id, role, text, at)) in rows.into_iter().enumerate() {
                let text = filo_local::store::preview(&text, usize::MAX);
                if role != "error" && holds(&text, &words) {
                    found.push(Found { chat: GROUP.into(), message: id, text, at: at as f64, depth: index + 1 });
                }
            }
        }
        found.sort_by(|a, b| b.at.total_cmp(&a.at));
        found.truncate(MAX_FOUND);
        Ok(Value::Array(
            found
                .into_iter()
                .map(|found| {
                    json!({
                        "chat": found.chat,
                        "messageId": found.message,
                        "line": line_around(&found.text, &words),
                        "at": found.at,
                        "depth": found.depth,
                    })
                })
                .collect(),
        ))
    }
}

/// The words of a search, lowercased.
fn words_of(query: &str) -> Vec<Vec<char>> {
    query.split_whitespace().map(lower).collect()
}

/// `text` lowercased a character for a character, so places in it are places in the text.
fn lower(text: &str) -> Vec<char> {
    text.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

fn find(haystack: &[char], word: &[char]) -> Option<usize> {
    if word.is_empty() || word.len() > haystack.len() {
        return None;
    }
    haystack.windows(word.len()).position(|window| window == word)
}

/// Whether `text` holds every word.
fn holds(text: &str, words: &[Vec<char>]) -> bool {
    let text = lower(text);
    words.iter().all(|word| find(&text, word).is_some())
}

/// One line of `text` with the first word found in it near its start: from the start of a word a little before it,
/// "…" where something's left out.
fn line_around(text: &str, words: &[Vec<char>]) -> String {
    let chars: Vec<char> = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    let lowered: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let first = words.iter().filter_map(|word| find(&lowered, word)).min().unwrap_or(0);
    let mut start = first.saturating_sub(LEAD_CHARS);
    if start > 0 {
        // From the start of a word, not inside one.
        match chars[start..first].iter().position(|c| *c == ' ') {
            Some(space) => start += space + 1,
            None => start = first,
        }
    }
    let end = (start + LINE_CHARS).min(chars.len());
    let mut line: String = chars[start..end].iter().collect();
    if start > 0 {
        line = format!("…{line}");
    }
    if end < chars.len() {
        line = format!("{}…", line.trim_end());
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use filo_local::{Message, Role};

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("search")
    }

    #[test]
    fn a_line_starts_near_the_first_word_found() {
        let words = words_of("ZEBRA");
        assert_eq!(line_around("A zebra here", &words), "A zebra here");
        let long = "one two three four five six seven eight nine ten the zebra crossed";
        assert_eq!(line_around(long, &words), "…seven eight nine ten the zebra crossed");
        assert!(holds("Ünïcode Zebra", &words_of("ünï zeb")));
        assert!(!holds("zebra", &words_of("zebra lion")));
    }

    #[test]
    fn the_agents_and_the_groups_messages_are_found_newest_first() {
        let local = temp();
        // Fresh data has the Filo agent.
        let filo = super::super::DEFAULT_AGENT_ID;
        let session = local.call("agents:openChat", &json!({ "id": filo })).unwrap().as_str().unwrap().to_string();
        let asked = local.agent_chats.add_user_message(&session, "What goes in a **risotto**?", "turn-1").unwrap();
        local
            .agent_chats
            .append(
                &session,
                &Message::new(Role::Assistant, "Arborio rice, stock and parmesan.", Some("turn-1".into())),
            )
            .unwrap();
        local.call("agents:openGroup", &json!({})).unwrap();
        let posted = filo_local::group::post(
            local.db().unwrap(),
            filo_local::apps::FILO_AGENT,
            "Risotto night on Friday?",
            filo_local::group::Asks::default(),
            None,
            super::super::now_ms() + 1000,
        )
        .unwrap();
        let found = local.call("agents:searchMessages", &json!({ "query": "risotto" })).unwrap();
        let found = found.as_array().unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            (&found[0]["chat"], &found[0]["messageId"], &found[0]["depth"]),
            (&json!("group"), &json!(posted.id), &json!(1))
        );
        assert_eq!(found[1]["chat"], json!(filo));
        assert_eq!(found[1]["messageId"], json!(asked.id));
        assert_eq!(found[1]["line"], json!("What goes in a risotto?"));
        assert_eq!(found[1]["depth"], json!(2));
        assert_eq!(local.call("agents:searchMessages", &json!({ "query": "risotto parmesan" })).unwrap(), json!([]));
        assert_eq!(local.call("agents:searchMessages", &json!({ "query": "  " })).unwrap(), json!([]));
        std::fs::remove_dir_all(&local.root).ok();
    }
}

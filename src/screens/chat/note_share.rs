// Notes shared into an agent's conversation from the Notes tab (a note's right-click menu, `screens/notes.rs`): the note waits above the
// composer as a chip with an ×, and its whole text goes to the model with the next message (`sharedNotes` on
// `voiceSessions:sendMessage` and the voice-message mutations; local_backend.rs `message_context`), not into the
// bubble. Sharing sends nothing by itself, and what's typed in the composer stays.

use serde_json::{Value, json};

/// Notes one message carries, and their text together (the backend's limits: a note's longest body).
pub const MAX_NOTES: usize = 3;
pub const MAX_CHARS: usize = 8_000;

#[derive(Clone, Debug, PartialEq)]
pub struct SharedNote {
    /// The note's id.
    pub key: String,
    pub title: String,
    pub body: String,
}

impl SharedNote {
    /// What the chip says: the title, else the first line, else "Untitled note".
    pub fn label(&self) -> String {
        let title = self.title.trim();
        if !title.is_empty() {
            return title.to_string();
        }
        self.body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "Untitled note".into())
    }

    /// Its length for the limit, in UTF-16 units (never fewer than the characters the backend counts).
    fn chars(&self) -> usize {
        self.body.trim().encode_utf16().count()
    }

    fn json(&self) -> Value {
        json!({ "id": self.key, "title": self.title, "body": self.body })
    }
}

/// An agent the share menu offers, drawn like the Agents tab: its picture on its colour (None for the theme's).
#[derive(Clone)]
pub struct AgentTarget {
    pub id: String,
    pub name: String,
    pub picture: gpui::ImageSource,
    pub background: Option<gpui::Hsla>,
}

impl std::fmt::Debug for AgentTarget {
    // The picture has no Debug.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTarget").field("id", &self.id).field("name", &self.name).finish()
    }
}

/// The notes as the send mutations take them.
pub fn args(notes: &[SharedNote]) -> Value {
    Value::Array(notes.iter().map(SharedNote::json).collect())
}

/// `notes` with `note` added (or brought up to date, when it's there already), or why it can't go with one message.
pub fn add(notes: &[SharedNote], note: SharedNote) -> Result<Vec<SharedNote>, &'static str> {
    let mut next: Vec<SharedNote> = notes.iter().filter(|shared| shared.key != note.key).cloned().collect();
    next.push(note);
    if next.len() > MAX_NOTES {
        return Err("Up to 3 notes go with one message.");
    }
    if next.iter().map(SharedNote::chars).sum::<usize>() > MAX_CHARS {
        return Err("These notes are too long to go with one message together. Remove one first.");
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: &str, title: &str, body: &str) -> SharedNote {
        SharedNote { key: key.into(), title: title.into(), body: body.into() }
    }

    #[test]
    fn a_note_shared_again_is_updated_not_doubled() {
        let notes = add(&[], note("a", "Old", "one")).unwrap();
        let notes = add(&notes, note("a", "New", "two")).unwrap();
        assert_eq!(notes, [note("a", "New", "two")]);
    }

    #[test]
    fn at_most_three_notes_and_eight_thousand_characters() {
        let three =
            ["a", "b", "c"].into_iter().fold(Vec::new(), |notes, key| add(&notes, note(key, key, "x")).unwrap());
        assert!(add(&three, note("d", "d", "x")).is_err());
        let long = add(&[], note("a", "Long", &"x".repeat(6_000))).unwrap();
        assert!(add(&long, note("b", "Also long", &"y".repeat(2_001))).is_err());
        assert!(add(&long, note("b", "Short", &"y".repeat(2_000))).is_ok());
    }

    #[test]
    fn a_note_goes_with_its_id() {
        assert_eq!(args(&[note("j57", "List", "Milk")]), json!([{ "title": "List", "body": "Milk", "id": "j57" }]));
    }

    #[test]
    fn the_chip_says_the_title_or_the_first_line() {
        assert_eq!(note("a", "Plan", "x").label(), "Plan");
        assert_eq!(note("a", " ", "\n  First line\nSecond").label(), "First line");
        assert_eq!(note("a", "", "").label(), "Untitled note");
    }
}

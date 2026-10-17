// Where a voice message held with the open shortcut from another app goes (app.rs `talk`): the chat picked in
// Settings › Keyboard shortcuts (companion `VoiceTo`: the main agent, the open chat, or an agent), unless the message
// starts by naming an agent or the group ("Milo, …", "Group: …", "Hey Milo …"), which sends that one message there.
// The name is in the words, so the chat is picked once the recording is transcribed; while the keys are held the pet
// says where it goes as things stand. A picked agent that's gone, or an open chat when none is open, is the main agent.

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::companion::VoiceTo;
use crate::screens::agents::{AgentRow, GroupRow, group_title};

/// Who a voice message goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recipient {
    Agent(String),
    Group,
    /// The chat that was open in Filo's window, by its id.
    Chat(String),
}

/// What a voice message can go to: the agents and the group.
pub struct Directory {
    agents: Vec<AgentRow>,
    group: Option<GroupRow>,
}

impl Directory {
    pub async fn load(backend: &Backend) -> Result<Directory, String> {
        let agents = backend.call_raw("agents:list", json!({})).await?;
        let agents: Vec<AgentRow> = serde_json::from_value(agents).map_err(|error| error.to_string())?;
        // No group yet: the agents alone.
        let group = backend
            .call_raw("agents:group", json!({}))
            .await
            .ok()
            .and_then(|group| serde_json::from_value(group).ok())
            .flatten();
        Ok(Directory { agents, group })
    }

    fn main_agent(&self) -> Option<&AgentRow> {
        self.agents.iter().find(|agent| agent.main).or(self.agents.first())
    }

    /// Who `choice` sends to, with `open_chat` the chat open in Filo's window when the keys went down.
    pub fn chosen(&self, choice: &VoiceTo, open_chat: Option<&str>) -> Option<Recipient> {
        match choice {
            VoiceTo::Agent(id) if self.agents.iter().any(|agent| &agent.id == id) => Some(Recipient::Agent(id.clone())),
            VoiceTo::OpenChat if open_chat.is_some() => open_chat.map(|chat| Recipient::Chat(chat.to_string())),
            _ => self.main_agent().map(|agent| Recipient::Agent(agent.id.clone())),
        }
    }

    /// The agent or the group `transcript` opens by naming, if any (`addresses`). The longest name wins, so "Milo
    /// Two, …" isn't Milo's.
    pub fn addressed(&self, transcript: &str) -> Option<Recipient> {
        let mut names: Vec<(&str, Recipient)> =
            self.agents.iter().map(|agent| (agent.name.as_str(), Recipient::Agent(agent.id.clone()))).collect();
        // The group while it's on (`agents:group` has none while it's off).
        if let Some(group) = &self.group {
            names.push(("group", Recipient::Group));
            if let Some(name) = group.name.as_deref() {
                names.push((name, Recipient::Group));
            }
        }
        names.sort_by_key(|(name, _)| std::cmp::Reverse(name.chars().count()));
        names.into_iter().find(|(name, _)| addresses(transcript, name)).map(|(_, recipient)| recipient)
    }

    /// The recipient's name, as the pet shows it ("→ Milo").
    pub fn label(&self, recipient: &Recipient) -> String {
        let group = || group_title(self.group.as_ref().and_then(|group| group.name.as_deref()));
        match recipient {
            Recipient::Agent(id) => self
                .agents
                .iter()
                .find(|agent| &agent.id == id)
                .map_or_else(|| "your agent".into(), |agent| agent.name.clone()),
            Recipient::Group => group(),
            Recipient::Chat(chat) => {
                if self.group.as_ref().is_some_and(|row| &row.session_id == chat) {
                    group()
                } else {
                    self.agents
                        .iter()
                        .find(|agent| agent.session_id.as_deref() == Some(chat.as_str()))
                        .map_or_else(|| "the open chat".into(), |agent| agent.name.clone())
                }
            }
        }
    }

    /// The recipient's chat, started if it hasn't been yet.
    pub async fn session(&self, backend: &Backend, recipient: &Recipient) -> Result<String, String> {
        let started = match recipient {
            Recipient::Chat(chat) => return Ok(chat.clone()),
            Recipient::Agent(id) => {
                match self.agents.iter().find(|agent| &agent.id == id).and_then(|agent| agent.session_id.clone()) {
                    Some(session) => return Ok(session),
                    None => backend.call_raw("agents:openChat", json!({ "id": id })).await?,
                }
            }
            Recipient::Group => match &self.group {
                Some(group) => return Ok(group.session_id.clone()),
                None => return Err("The group is off.".into()),
            },
        };
        started
            .as_str()
            .map(str::to_string)
            .or_else(|| started.get("sessionId").and_then(Value::as_str).map(str::to_string))
            .ok_or_else(|| "The chat could not be opened.".into())
    }
}

/// Words a message may open with before the name it's for ("Hey Milo").
const GREETINGS: [&str; 5] = ["hey", "hi", "hello", "ok", "okay"];

/// Whether `text` opens by speaking to `name` (any case): the name set off by punctuation ("Milo, …", "Group: …", or
/// all there is), or after a greeting ("Hey Milo …", "Hi, group. …"). A name running straight on into the sentence
/// doesn't count, so "Group the photos by date" stays with the chosen chat.
pub fn addresses(text: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let text = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    if let Some(rest) = strip_word(text, name) {
        let rest = rest.trim_start();
        return rest.is_empty() || rest.starts_with([',', ':', ';', '.', '!', '?', '—', '–', '-']);
    }
    GREETINGS.iter().any(|greeting| {
        strip_word(text, greeting).is_some_and(|rest| strip_word(rest.trim_start_matches([',', ' ']), name).is_some())
    })
}

/// `text` after `word` when it starts with it (any case) as a whole word.
fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let head = text.get(..word.len())?;
    let rest = &text[word.len()..];
    (head.to_lowercase() == word.to_lowercase() && !rest.starts_with(char::is_alphanumeric)).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, name: &str, main: bool, session: Option<&str>) -> AgentRow {
        serde_json::from_value(json!({
            "_id": id, "name": name, "picture": "fox", "instructions": "", "sessionId": session, "unseen": 0, "main": main,
        }))
        .unwrap()
    }

    fn directory() -> Directory {
        let group = serde_json::from_value(json!({ "sessionId": "s-group", "unseen": 0, "members": [] })).unwrap();
        Directory {
            agents: vec![
                agent("filo", "Filo", true, Some("s-filo")),
                agent("milo", "Milo", false, None),
                agent("milo2", "Milo Two", false, Some("s-milo2")),
            ],
            group: Some(group),
        }
    }

    #[test]
    fn a_name_set_off_or_greeted_addresses_the_message() {
        assert!(addresses("Milo, what's on today?", "Milo"));
        assert!(addresses("milo: check the build", "Milo"));
        assert!(addresses("  Milo.", "Milo"));
        assert!(addresses("Milo", "Milo"));
        assert!(addresses("Hey Milo what's on today", "Milo"));
        assert!(addresses("Hi, group. Plan the trip", "group"));
        assert!(addresses("Group, plan the trip", "Group"));
        // Running on into the sentence, or only starting the same way, it doesn't.
        assert!(!addresses("Group the photos by date", "group"));
        assert!(!addresses("Milos are nice", "Milo"));
        assert!(!addresses("Tell Milo to check", "Milo"));
        assert!(!addresses("Hey there, Milo", "Milo"));
        assert!(!addresses("Anything", ""));
    }

    #[test]
    fn the_message_goes_to_whoever_it_names_first() {
        let directory = directory();
        assert_eq!(directory.addressed("Milo, hi"), Some(Recipient::Agent("milo".into())));
        assert_eq!(directory.addressed("Milo Two, hi"), Some(Recipient::Agent("milo2".into())));
        assert_eq!(directory.addressed("Group: plan the trip"), Some(Recipient::Group));
        assert_eq!(directory.addressed("What's on today?"), None);
        // The group off: nothing goes there.
        let off = Directory { group: None, ..directory };
        assert_eq!(off.addressed("Group: plan the trip"), None);
    }

    #[test]
    fn the_chosen_recipient_falls_back_to_the_main_agent() {
        let directory = directory();
        let main = Some(Recipient::Agent("filo".into()));
        assert_eq!(directory.chosen(&VoiceTo::Main, Some("s-milo2")), main);
        assert_eq!(directory.chosen(&VoiceTo::Agent("milo".into()), None), Some(Recipient::Agent("milo".into())));
        // A deleted agent, or no chat open.
        assert_eq!(directory.chosen(&VoiceTo::Agent("gone".into()), None), main);
        assert_eq!(directory.chosen(&VoiceTo::OpenChat, None), main);
        assert_eq!(directory.chosen(&VoiceTo::OpenChat, Some("s-milo2")), Some(Recipient::Chat("s-milo2".into())));
    }

    #[test]
    fn the_recipient_is_named_for_the_pet() {
        let directory = directory();
        assert_eq!(directory.label(&Recipient::Agent("filo".into())), "Filo");
        assert_eq!(directory.label(&Recipient::Group), "Group");
        assert_eq!(directory.label(&Recipient::Chat("s-group".into())), "Group");
        assert_eq!(directory.label(&Recipient::Chat("s-milo2".into())), "Milo Two");
    }
}

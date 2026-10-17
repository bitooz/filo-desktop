// Messages the user sends a chat while a turn answers there, taken into that turn ("steering") rather than left waiting
// for it to end. Every engine takes a message mid-turn (coding_session.rs's notes: a user message on Claude Code's stdin,
// `turn/steer` on Codex's app-server and Muse Code's, a prompt delivered as `steer` on OpenCode's server) and acts on it
// at its next step. Muse Code and OpenCode may refuse a steer: their turn is then interrupted, and a new turn in the same
// session starts with the message, carrying on after what the first one did and posted.
//
// Each message still starts a turn of its own (turn.rs), which waits for the running one (`take_turn`). The running
// turn takes a message whose turn waits behind it (turn.rs `Steering`): it claims that turn here, hands the message to
// its CLI, and marks it seen once it's the turn's (coding_session.rs `Notes`: the CLI has it and acted after, or a turn
// it started with it is under way). From then on the turn's replies answer that message (store.rs `agent_message`).
// When the turn ends (`turn_over`), what its CLI saw is answered, and the message's own turn ends without running once
// it holds the chat's turn; a claim the CLI never saw, and every claim of a turn that failed, goes back, and the
// message's turn runs after it, as it would have. So does a claim whose turn was killed before it could say how it
// ended. Claims don't outlive the turns they're about: the message's turn drops its own as it stops waiting, and the
// turn that took it drops those whose turn isn't waiting anymore.
//
// `chats/<id>/steered.json`: { "<the message's turn>": { "into": "<the running turn>", "seen": bool, "ended": bool } },
// changed under `chats/<id>/steered.lock` by the turns' processes (workers, the app) and read by the MCP server's
// send_message.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::store::Store;

const FILE: &str = "steered.json";
const LOCK: &str = "steered.lock";

/// A message's turn taken into a running one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Taken {
    /// The running turn it went into.
    pub into: String,
    /// Its CLI has the message: the turn's replies answer it from now on.
    #[serde(default)]
    pub seen: bool,
    /// The turn it went into has ended, and answered it.
    #[serde(default)]
    pub ended: bool,
}

fn read(dir: &Path) -> HashMap<String, Taken> {
    crate::files::read_json(&dir.join(FILE)).ok().flatten().unwrap_or_default()
}

/// Changes the chat's record under its lock; what `change` returns. Nothing changes when the chat's folder isn't there.
fn change<T>(store: &Store, chat_id: &str, change: impl FnOnce(&mut HashMap<String, Taken>) -> T) -> Option<T> {
    let dir = store.chat_dir(chat_id).ok().filter(|dir| dir.is_dir())?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK)).ok()?;
    crate::lock::flock(&lock, libc::LOCK_EX).ok()?;
    let mut all = read(&dir);
    let before = all.clone();
    let answer = change(&mut all);
    if all != before {
        let bytes = serde_json::to_vec_pretty(&all).ok()?;
        crate::files::write_atomic(&dir.join(FILE), &bytes).ok()?;
    }
    Some(answer)
}

/// The chat's taken turns.
pub fn all(store: &Store, chat_id: &str) -> HashMap<String, Taken> {
    store.chat_dir(chat_id).map(|dir| read(&dir)).unwrap_or_default()
}

/// Where `turn`'s message went, if a running turn took it.
pub fn taken(store: &Store, chat_id: &str, turn: &str) -> Option<Taken> {
    all(store, chat_id).remove(turn)
}

/// `turn` stops waiting: where its message went, if a turn took it, and the claim is dropped.
pub fn take_back(store: &Store, chat_id: &str, turn: &str) -> Option<Taken> {
    change(store, chat_id, |all| all.remove(turn)).flatten()
}

/// Turn `into` takes `turn`'s message: false when another turn has it already.
pub fn claim(store: &Store, chat_id: &str, turn: &str, into: &str) -> bool {
    change(store, chat_id, |all| match all.get(turn) {
        Some(taken) => taken.into == into,
        None => {
            all.insert(turn.to_string(), Taken { into: into.to_string(), seen: false, ended: false });
            true
        }
    })
    .unwrap_or(false)
}

/// The CLI has `turn`'s message.
pub fn seen(store: &Store, chat_id: &str, turn: &str) {
    change(store, chat_id, |all| {
        if let Some(taken) = all.get_mut(turn) {
            taken.seen = true;
        }
    });
}

/// Turn `into` is over, `answered` unless it failed: the messages it took that its CLI had are answered, and their
/// turns end without running; the rest go back to their own turns (all of them when it failed). Claims whose turns
/// don't wait anymore (`waits`: their worker was killed) go, as do those of a turn that ended before.
pub fn turn_over(store: &Store, chat_id: &str, into: &str, answered: bool, waits: impl Fn(&str) -> bool) {
    change(store, chat_id, |all| {
        all.retain(|turn, taken| waits(turn) && taken.seen && if taken.into == into { answered } else { taken.ended });
        for taken in all.values_mut().filter(|taken| taken.into == into) {
            taken.ended = true;
        }
    });
}

/// The turns whose messages went into `into` and its CLI has, in no order.
pub fn seen_into(all: &HashMap<String, Taken>, into: &str) -> Vec<String> {
    all.iter().filter(|(_, taken)| taken.seen && taken.into == into).map(|(turn, _)| turn.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use crate::store::tests::temp_store;

    #[test]
    fn a_claim_is_one_turns_until_its_turn_ends() {
        let store = temp_store("steer");
        let chat = store.create_chat(Engine::Claude).unwrap();
        assert_eq!(taken(&store, &chat.id, "t2"), None);
        assert!(claim(&store, &chat.id, "t2", "t1"));
        // Taken already: not by another turn, though the same one may claim it again (a retry with a fresh session).
        assert!(!claim(&store, &chat.id, "t2", "t9"));
        assert!(claim(&store, &chat.id, "t2", "t1"));
        assert!(claim(&store, &chat.id, "t3", "t1"));
        assert!(claim(&store, &chat.id, "t4", "t1"));
        seen(&store, &chat.id, "t2");
        seen(&store, &chat.id, "t4");
        assert_eq!(taken(&store, &chat.id, "t2"), Some(Taken { into: "t1".into(), seen: true, ended: false }));
        let mut steered = seen_into(&all(&store, &chat.id), "t1");
        steered.sort();
        assert_eq!(steered, ["t2", "t4"]);
        // The turn ends: what its CLI had is answered, what it never had goes back, and so does a claim whose turn
        // isn't waiting anymore (t4's worker was killed).
        turn_over(&store, &chat.id, "t1", true, |turn| turn != "t4");
        assert_eq!(
            all(&store, &chat.id).into_iter().collect::<Vec<_>>(),
            [("t2".to_string(), Taken { into: "t1".into(), seen: true, ended: true })]
        );
        // t2's turn stops waiting: its claim goes with it.
        assert!(take_back(&store, &chat.id, "t2").is_some_and(|taken| taken.ended));
        assert!(all(&store, &chat.id).is_empty());
        // A turn that failed gives every message back, the ones its CLI had too.
        assert!(claim(&store, &chat.id, "t6", "t5"));
        seen(&store, &chat.id, "t6");
        turn_over(&store, &chat.id, "t5", false, |_| true);
        assert!(all(&store, &chat.id).is_empty());
        // Not a chat: nothing to claim.
        assert!(!claim(&store, "../x", "t2", "t1"));
        assert!(!claim(&store, "no-such-chat", "t2", "t1"));
        std::fs::remove_dir_all(store.root()).ok();
    }
}

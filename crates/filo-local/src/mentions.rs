// What a message can name of Filo's own besides skills and agents: "@computer" (the apps on the user's Mac,
// `use_computer`, computer.rs) and "@browser" (Filo's browser, the `browser_*` tools, browser_mcp.rs). The composer's
// "@" list offers them next to the user's apps (src/screens/chat/mention_suggestions.rs), and a turn whose message names
// one gets a line after its prompt on which tools to use for it (turn.rs), as `skills::with_mentioned` attaches skills.

/// One of them: its id, its name in the "@" list, what's typed after "@" and what the turn is told of it.
pub struct Named {
    pub id: &'static str,
    pub name: &'static str,
    pub handle: &'static str,
    hint: &'static str,
}

/// "Your computer", offered while its switch is on. The instructions say whether `use_computer` is there now
/// (computer.rs `capability`).
pub const COMPUTER: Named = Named {
    id: "computer",
    name: "Your computer",
    handle: "computer",
    hint: "The user's message names @computer, their Mac: do what it asks in the apps on their Mac with `use_computer`, as your instructions say of the computer.",
};

/// Filo's browser, which every turn of every engine has (mcp.rs lists its tools to all of them).
pub const BROWSER: Named = Named {
    id: "browser",
    name: "Filo's browser",
    handle: "browser",
    hint: "The user's message names @browser, Filo's browser: do what it asks there, with Filo's browser tools (`browser_open` opens a page in a tab of your own, then `browser_read`, `browser_click`, `browser_type`, `browser_select`, `browser_upload`, `browser_drag` and the other `browser_*` tools), rather than your own web tools. If they aren't loaded yet, search your tools for \"browser\" and load them first.",
};

pub const ALL: [&Named; 2] = [&COMPUTER, &BROWSER];

/// Whether `text` names `named`: "@handle", in any case, at the start or after a space, and not running on into a
/// longer word ("@browsers"), as the composer draws it.
fn names(text: &str, named: &Named) -> bool {
    let word = format!("@{}", named.handle);
    let lower = text.to_lowercase();
    lower.match_indices(&word).any(|(at, _)| {
        let after_space = lower[..at].chars().next_back().is_none_or(char::is_whitespace);
        let runs_on = lower[at + word.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_alphanumeric() || matches!(next, '_' | '-' | '/'));
        after_space && !runs_on
    })
}

/// The turn's prompt with a line after it for each of them its message names.
pub fn with_named(prompt: &str, message: &str) -> String {
    let hints: Vec<&str> = ALL.iter().filter(|named| names(message, named)).map(|named| named.hint).collect();
    if hints.is_empty() {
        return prompt.to_string();
    }
    format!("{prompt}\n\n---\n{}", hints.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_one_gets_its_hint_after_the_prompt() {
        assert_eq!(with_named("Hi", "check my orders"), "Hi");
        let browser = with_named("Hi", "@browser check my orders on amazon");
        assert!(
            browser.starts_with("Hi\n\n---\nThe user's message names @browser")
                && browser.contains("`browser_open`")
                && browser.contains("search your tools for \"browser\""),
            "{browser}"
        );
        assert!(!browser.contains("@computer"));
        let both = with_named("Hi", "Look it up in @Browser, then open Notes with @computer.");
        assert!(both.contains("names @computer") && both.contains("names @browser"), "{both}");
        // Not inside a word, nor running on into a longer one.
        assert_eq!(with_named("Hi", "me@browser.dev @browsers @browser-tab"), "Hi");
    }
}

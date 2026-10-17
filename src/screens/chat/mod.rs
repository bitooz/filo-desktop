// What conversations are made of (the Agents tab shows them): the conversation view, its composer and cards, the
// working panel, drafts, and the keys they use. The Chats tab that used to hold Filo's own chats is gone.

pub(crate) mod attachments;
mod coding_requests;
mod coding_sessions;
pub mod conversation;
pub mod drafts;
mod feed;
#[cfg(feature = "dev")]
pub mod fixture;
pub(crate) mod image_editor;
mod mention_suggestions;
pub mod note_share;
mod question_panel;
pub mod reply_status;
pub mod shared;
mod types;
pub mod work_share;
pub mod working_panel;

/// A glance's card head, as a Feed post carries it too (feed_glance.rs).
pub(crate) use types::GlanceRef;

use gpui::{App, KeyBinding};

use self::conversation::SendMessage;
pub use self::mention_suggestions::Mention;

/// Enter sends; Shift+Enter adds a line (later bindings win at the same depth).
/// ⌘[ ⌘] go Back and Forward from the composer (it has no indenting); ⌘← ⌘→ keep
/// moving the caret to the line's start and end.
pub fn bind_keys(cx: &mut App) {
    use crate::chrome::{GoBack, GoForward};
    cx.bind_keys([
        KeyBinding::new("enter", SendMessage, Some("ChatComposer > Input")),
        KeyBinding::new("shift-enter", gpui_component::input::Enter { secondary: false }, Some("ChatComposer > Input")),
        KeyBinding::new("cmd-[", GoBack, Some("ChatComposer > Input")),
        KeyBinding::new("cmd-]", GoForward, Some("ChatComposer > Input")),
        // While the @mention list is open (conversation.rs), these keys work it instead of the field.
        KeyBinding::new("up", conversation::MentionPrev, Some("mentioning > Input")),
        KeyBinding::new("down", conversation::MentionNext, Some("mentioning > Input")),
        KeyBinding::new("enter", conversation::MentionPick, Some("mentioning > Input")),
        KeyBinding::new("tab", conversation::MentionPick, Some("mentioning > Input")),
        KeyBinding::new("escape", conversation::MentionDismiss, Some("mentioning > Input")),
        // On a question in the panel above the composer (question_panel.rs), ↑ on the composer's first line goes back
        // to its answers.
        KeyBinding::new("up", conversation::BackToQuestions, Some("answering > Input")),
        // The image editor (image_editor.rs): Enter sends the picture with its prompt, Escape closes it from the
        // prompt too, and ⌘Z undoes a stroke unless the prompt has the focus.
        KeyBinding::new("enter", image_editor::SendPicture, Some("ImageEditor > Input")),
        KeyBinding::new("shift-enter", gpui_component::input::Enter { secondary: false }, Some("ImageEditor > Input")),
        KeyBinding::new("escape", image_editor::CloseEditor, Some("ImageEditor")),
        KeyBinding::new("escape", image_editor::CloseEditor, Some("ImageEditor > Input")),
        KeyBinding::new("cmd-z", image_editor::UndoStroke, Some("ImageEditor")),
    ]);
}

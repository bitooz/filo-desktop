// Chats left with something typed and not sent. The Agents list puts them on top, under those needing input, latest
// message first (agents.rs `arrange`), with a "Draft" line in the warning colours: not the accent of an unseen reply, nor
// the link colour of work running. A chat joins when it's left with a draft (not while it's being typed in, so its row
// doesn't jump under the pointer), stays while the user is back in it, and leaves once its draft is sent or cleared. The
// drafts live in the chats' open conversations (mod.rs and agents.rs keep those with unsent work), so nothing here is
// saved or sent anywhere.

use gpui::{AnyElement, App, IntoElement as _, ParentElement as _, Styled as _, div, px};

use crate::theme::{ActiveTheme as _, Typography as _, space};

/// Chats with a draft, most recently left first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drafts(Vec<(String, String)>);

impl Drafts {
    /// `key` was left: with a draft (`Some(preview)`) it goes first, else it's dropped.
    pub fn left(&mut self, key: &str, preview: Option<String>) {
        self.0.retain(|(id, _)| id != key);
        if let Some(preview) = preview {
            self.0.insert(0, (key.to_string(), preview));
        }
    }

    /// Each kept chat's draft as it is now: those sent or cleared drop out, the rest keep their place. Whether it
    /// changed.
    pub fn refresh(&mut self, preview: impl Fn(&str) -> Option<String>) -> bool {
        let next: Vec<(String, String)> =
            self.0.iter().filter_map(|(key, _)| preview(key).map(|text| (key.clone(), text))).collect();
        let changed = next != self.0;
        self.0 = next;
        changed
    }

    pub fn preview(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(id, _)| id == key).map(|(_, text)| text.as_str())
    }

    /// Each chat's key and its draft's line, in order (the scripted checks).
    #[cfg(feature = "dev")]
    pub fn entries(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(key, _)| key.as_str())
    }
}

/// The line under a drafted row's name: a pencil, "Draft", and what's typed.
pub fn line(preview: &str, cx: &App) -> AnyElement {
    let colors = cx.colors();
    div()
        .flex()
        .items_center()
        .gap(px(space::XS))
        .min_w_0()
        .child(crate::ui::icon_sized("pencil", 12., colors.on_warning_soft))
        .child(div().flex_none().caption().semibold().text_color(colors.on_warning_soft).child("Draft"))
        // Clamped rather than kept on one line (`truncate`), so it ends in "…" where it's cut (agents.rs `preview_line`).
        .child(
            div()
                .flex_1()
                .min_w_0()
                .caption()
                .text_color(colors.text_secondary)
                .line_clamp(1)
                .text_ellipsis()
                .child(preview.to_string()),
        )
        .into_any_element()
}

/// The line under an Agents list row's name with its draft: in the last message's place, or under the row's status
/// (`status`: `preview` says it's working or failed) when it has one.
pub fn with_line(draft: Option<&str>, status: bool, preview: Option<AnyElement>, cx: &App) -> Option<AnyElement> {
    let Some(draft) = draft else { return preview };
    match preview.filter(|_| status) {
        Some(status) => {
            Some(div().flex().flex_col().gap(px(2.)).child(status).child(line(draft, cx)).into_any_element())
        }
        None => Some(line(draft, cx)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chats_left_with_a_draft_go_first_until_it_is_sent_or_cleared() {
        let mut drafts = Drafts::default();
        let keys = |drafts: &Drafts| drafts.keys().map(str::to_string).collect::<Vec<_>>();
        // Left with nothing typed: no draft.
        drafts.left("b", None);
        assert!(keys(&drafts).is_empty());
        // Left with a draft: the one left last goes first.
        drafts.left("c", Some("Buy milk".into()));
        drafts.left("d", Some("Call mum".into()));
        assert_eq!(keys(&drafts), ["d", "c"]);
        assert_eq!(drafts.preview("c"), Some("Buy milk"));
        // Back in "c" and out again with more typed: its preview follows, and it's first again.
        drafts.left("c", Some("Buy milk and eggs".into()));
        assert_eq!(keys(&drafts), ["c", "d"]);
        assert_eq!(drafts.preview("c"), Some("Buy milk and eggs"));
        // "d" sent: it drops out, and nothing changes the next time round.
        let typed = |key: &str| (key == "c").then(|| "Buy milk and eggs".to_string());
        assert!(drafts.refresh(typed));
        assert!(!drafts.refresh(typed));
        assert_eq!(keys(&drafts), ["c"]);
        // Left again with the text cleared: it's gone too.
        drafts.left("c", None);
        assert!(keys(&drafts).is_empty());
    }
}

// What the user has seen of this chat (voiceSessions:markSeen): Filo's message counts as seen once it's on screen while the
// user is looking (`attending`, from presence.rs through the shell). While they
// aren't, the feed holds its place instead of following new replies, so coming
// back shows where they left off, a "New messages" divider above what arrived,
// and a pill that jumps down and counts what's still unseen below.

use gpui::{Bounds, ListOffset, Pixels, point};

use super::*;

/// Filo's messages from before this time count as seen.
const SEEN_TRACKING_SINCE: f64 = 1_790_985_600_000.;
/// Seen marks wait this long, so scrolling past several replies sends one.
const MARK_SEEN_DELAY: Duration = Duration::from_millis(500);
/// How much of a message must show for it to count as seen.
const SEEN_MARGIN: f32 = 24.;

impl Conversation {
    /// Whether the user is looking at this chat (the shell checks every second).
    pub fn set_attending(&mut self, attending: bool, cx: &mut Context<Self>) {
        if self.attending == attending {
            return;
        }
        self.attending = attending;
        if attending {
            self.attended = true;
            if !self.stale {
                self.came_back(cx);
            } else if self.held_at_end {
                // Shown again after it was parked, left at its newest: at the newest it knows (pictures that loaded
                // meanwhile moved it), held again when the backend's answer comes, which says what came meanwhile
                // (`detail_arrived`).
                self.list_state.scroll_to(ListOffset { item_ix: self.list_state.item_count(), offset_in_item: px(0.) });
                self.at_bottom = true;
            }
        } else {
            self.hold_position();
        }
        cx.notify();
    }

    /// Back after being away: mark where the new messages start. With nothing new, the last divider was read, so it
    /// goes, and the feed follows new replies again. (`at_end` alone isn't enough: rows that arrived below a held
    /// place, a reply under way or a card, leave the end out of view.)
    pub(super) fn came_back(&mut self, cx: &mut Context<Self>) {
        if self.unseen_count() > 0 {
            self.divider_at = Some(self.seen_until);
            self.rebuild(cx);
        } else {
            if self.divider_at.take().is_some() {
                self.rebuild(cx);
            }
            if self.at_end || self.held_at_end {
                self.list_state.scroll_to(ListOffset { item_ix: self.list_state.item_count(), offset_in_item: px(0.) });
                self.at_bottom = true;
            }
        }
        self.held_at_end = false;
    }

    /// Whether this is `session_id`'s chat and the user is looking at it.
    pub fn showing(&self, session_id: &str) -> bool {
        self.attending && self.session_id.as_deref() == Some(session_id)
    }

    /// Takes the backend's cursor; the first page of messages places the divider.
    pub(super) fn sync_seen(&mut self) {
        let Some(detail) = self.detail() else { return };
        let server = detail.session.seen_until.unwrap_or(0.).max(SEEN_TRACKING_SINCE);
        self.seen_until = self.seen_until.max(server);
        if !self.seen_ready {
            self.seen_ready = true;
            if self.unseen_count() > 0 {
                self.divider_at = Some(self.seen_until);
            }
        }
    }

    /// Filo's messages after the cursor; not what an agent posted in the group, which is unseen there.
    pub(super) fn unseen_count(&self) -> usize {
        self.detail().map_or(0, |detail| {
            detail
                .messages
                .iter()
                .filter(|message| counts_unseen(message) && message.creation_time > self.seen_until)
                .count()
        })
    }

    /// The first unseen message, which the divider sits above.
    pub(super) fn divider_before(&self, message: &Message) -> bool {
        self.divider_at.is_some_and(|at| counts_unseen(message) && message.creation_time > at)
    }

    /// Whether the feed sticks to the newest message (gpui's bottom-aligned list with no scroll set).
    fn following(&self) -> bool {
        self.list_state.logical_scroll_top().item_ix >= self.list_state.item_count()
    }

    /// Stops following new messages: keeps the scroll offset where it is, so they arrive below. Not in a chat the
    /// user hasn't looked at yet (one just opened, before the shell's check says they're looking): it follows its
    /// first pages and live rows to the newest instead of stopping wherever the first layout left it.
    pub(super) fn hold_position(&mut self) {
        if !self.attended || !self.following() {
            return;
        }
        let max = self.list_state.max_offset_for_scrollbar().height;
        // Everything fits: new messages show anyway.
        if max <= px(1.) {
            return;
        }
        self.list_state.set_offset_from_scrollbar(point(px(0.), -(max - px(1.))));
        self.at_bottom = false;
        self.held_at_end = true;
    }

    /// Opening a chat with unseen messages starts at the divider (the bottom when they fit).
    pub(super) fn position_at_divider(&mut self) {
        if self.positioned || self.rows.is_empty() {
            return;
        }
        self.positioned = true;
        if let Some(index) = self.rows.iter().position(|row| matches!(row, ViewRow::Unseen)) {
            self.list_state.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.) });
            self.at_bottom = false;
        }
    }

    pub(super) fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        // Past the last row is gpui's bottom; rows added later keep it there.
        self.list_state.scroll_to(ListOffset { item_ix: self.list_state.item_count(), offset_in_item: px(0.) });
        self.at_bottom = true;
        self.at_end = true;
        cx.notify();
    }

    /// The part of the feed the composer doesn't cover.
    fn visible_area(&self) -> Bounds<Pixels> {
        let mut area = self.list_state.viewport_bounds();
        area.size.height = (area.size.height - px(self.composer_height)).max(px(0.));
        area
    }

    /// After each layout: whether the end of the feed shows, and the newest message on screen
    /// moves the cursor up while the user is looking.
    pub(super) fn after_layout(&mut self, cx: &mut Context<Self>) {
        let area = self.visible_area();
        let following = self.following();
        let at_end = following
            || self
                .rows
                .len()
                .checked_sub(1)
                .and_then(|last| self.list_state.bounds_for_item(last))
                .is_some_and(|spacer| spacer.bottom() <= self.list_state.viewport_bounds().bottom() + px(2.));
        if at_end != self.at_end {
            self.at_end = at_end;
            cx.notify();
        }
        if !self.attending || self.unseen_count() == 0 {
            return;
        }
        let top = self.list_state.logical_scroll_top().item_ix;
        let mut newest: Option<(String, f64)> = None;
        for (index, row) in self.rows.iter().enumerate().rev() {
            if !following && index < top {
                break;
            }
            let Some(mark) = seen_mark(row, self.detail().map_or(&[][..], |detail| &detail.messages)) else { continue };
            if following {
                newest = Some(mark);
                break;
            }
            let Some(bounds) = self.list_state.bounds_for_item(index) else { continue };
            if bounds.top() < area.bottom() - px(SEEN_MARGIN) && bounds.bottom() > area.top() + px(SEEN_MARGIN) {
                newest = Some(mark);
                break;
            }
        }
        let Some((message_id, time)) = newest else { return };
        if time <= self.seen_until {
            return;
        }
        self.seen_until = time;
        self.mark_seen(message_id, cx);
        cx.notify();
    }

    fn mark_seen(&mut self, message_id: String, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_id.clone() else { return };
        self._mark_seen = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(MARK_SEEN_DELAY).await;
            cx.update(|cx| {
                backend::fire(
                    cx,
                    "voiceSessions:markSeen",
                    json!({ "sessionId": session_id, "messageId": message_id }),
                );
            })
            .ok();
        }));
    }

    /// The pill over the composer when the newest messages are out of view: jumps down,
    /// and says how many of Filo's are unseen.
    pub(super) fn render_jump_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.at_end || self.rows.is_empty() {
            return None;
        }
        let colors = *cx.colors();
        let unseen = self.unseen_count();
        let (fill, ink) = if unseen > 0 { (colors.accent, colors.on_accent) } else { (colors.raised, colors.text) };
        let pill = div()
            .id("jump-to-latest")
            .h(px(32.))
            .min_w(px(32.))
            .px(px(if unseen > 0 { space::MD } else { 0. }))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(space::XS))
            .rounded(px(radius::FULL))
            .bg(fill)
            .border_1()
            .border_color(if unseen > 0 { fill } else { colors.border })
            .shadow_md()
            .cursor_pointer()
            .hover(|style| style.opacity(0.9))
            // Floats over the messages: a click is the pill's, never the picture's under it.
            .occlude()
            .child(ui::icon_sized("arrow-down", 16., ink))
            .when(unseen > 0, |this| {
                this.child(
                    div()
                        .footnote()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ink)
                        .child(format!("{} new", crate::unseen::format_count(unseen as u32))),
                )
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump_to_latest(cx)));
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(self.composer_height + space::SM))
                .flex()
                .justify_center()
                .child(pill)
                .into_any_element(),
        )
    }
}

/// The message a row shows and its time, for the seen cursor: a message, or a line pointing at the group ("Called by
/// Chef in the group: …"), which is a message of its own (in `messages`) drawn as a row of its own. Without the second,
/// a chat whose only news was such a line kept its unread dot after being read.
pub(super) fn seen_mark(row: &ViewRow, messages: &[Message]) -> Option<(String, f64)> {
    match row {
        ViewRow::Message { message, .. } => Some((message.id.clone(), message.creation_time)),
        ViewRow::GroupLink { id, .. } | ViewRow::GroupCall { id, .. } => {
            messages.iter().find(|message| &message.id == id).map(|message| (message.id.clone(), message.creation_time))
        }
        _ => None,
    }
}

/// A reply that can be new here: not the user's, nor an agent's post in the group (local_backend.rs `unseen`).
fn counts_unseen(message: &Message) -> bool {
    !message.is_user() && message.posted_in_group().is_none()
}

/// The "New messages" line above the first unseen message.
pub(super) fn render_divider(cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let rule = || div().flex_1().h(px(1.)).bg(colors.accent.opacity(0.35));
    div()
        .py(px(space::SM))
        .flex()
        .items_center()
        .gap(px(space::SM))
        .child(rule())
        .child(div().caption().font_weight(gpui::FontWeight::SEMIBOLD).text_color(colors.accent).child("New messages"))
        .child(rule())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_pointing_at_the_group_counts_as_seen_like_a_message() {
        let messages: Vec<Message> = vec![
            serde_json::from_value(json!({ "_id": "m1", "_creationTime": 1., "role": "assistant", "content": "Hi" })).unwrap(),
            serde_json::from_value(json!({
                "_id": "l1", "_creationTime": 2., "role": "assistant", "content": "Called by Chef in the group: Dinner?",
                "groupLink": { "kind": "calledBy", "who": [{ "speaker": "chef", "name": "Chef" }], "messageId": "g1" },
            }))
            .unwrap(),
        ];
        let link = messages[1].group_link.clone().unwrap();
        let row = ViewRow::GroupLink { id: "l1".into(), link: link.clone(), quote: "Dinner?".into() };
        assert_eq!(seen_mark(&row, &messages), Some(("l1".to_string(), 2.)));
        // A line whose message isn't loaded, and rows that aren't messages, move nothing.
        assert_eq!(seen_mark(&ViewRow::GroupLink { id: "gone".into(), link, quote: String::new() }, &messages), None);
        assert_eq!(seen_mark(&ViewRow::Unseen, &messages), None);
    }

    #[test]
    fn what_an_agent_posted_in_the_group_is_never_new_in_its_chat() {
        let message = |link: &str| -> Message {
            serde_json::from_value(json!({
                "_id": "m", "_creationTime": 1., "role": "assistant", "content": "Pasta.",
                "groupLink": { "kind": link, "who": [], "messageId": "g1" },
            }))
            .unwrap()
        };
        assert!(!counts_unseen(&message("posted")));
        assert!(counts_unseen(&message("calledBy")));
    }
}

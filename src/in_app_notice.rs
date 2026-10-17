// A reply nobody has seen, inside Filo's own window: a small card at the top right
// with the chat's title (one per chat; a newer reply replaces it) that opens the
// chat when clicked anywhere, a sound, and a Dock bounce when Filo is in the
// background. It needs no permission, so it
// works where macOS refuses Filo's banners (notifications.rs). In Filo it's the
// only one (no banner); from another app a banner goes out too, which plays the
// sound when macOS shows it.
//
// Also the note before macOS asks to show Filo's banners (`ask_for_banners`).

#![allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, IntoElement, ParentElement as _, Styled as _, Window, div, px};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};

use crate::notifications::{Need, Permission};
use crate::theme::{Typography as _, space};
use crate::unseen::ReplyNotice;

/// The sound a reply makes (one of macOS's alert sounds).
const SOUND: &str = "Glass";
/// `NSInformationalRequest`: the Dock icon bounces once.
const INFORMATIONAL_REQUEST: isize = 10;

/// Keys the cards, so each chat has at most one.
struct ChatNotice;
/// Keys the note before macOS asks for banners.
struct BannersNote;

/// How long the note shows before macOS asks.
const NOTE_FIRST: Duration = Duration::from_secs(2);
/// Asked in this run of the app: once is enough (macOS keeps an answer; an unanswered ask waits in Notification Center).
static ASKED_BANNERS: AtomicBool = AtomicBool::new(false);

const CARD_WIDTH: f32 = 420.;
/// Room on the right for the card's close button, which shows on hover.
const CLOSE_ROOM: f32 = 40.;
/// The title's width: the card less its padding. A pixel width, so a long title is cut cleanly.
const TITLE_WIDTH: f32 = CARD_WIDTH - space::LG - CLOSE_ROOM;

/// Shows the card: the chat's title. A click anywhere on it (a pointer and a hover background say so) opens the chat (an
/// agent's chat opens under the Agents tab) and takes the card down; its close button only takes it down.
pub fn show(notice: &ReplyNotice, window: &mut Window, cx: &mut App) {
    window.push_notification(card(notice), cx);
    if !(notice.banner && crate::notifications::banners_on()) {
        play_sound();
    }
    if !window.is_window_active() {
        bounce_dock();
    }
}

/// The card itself, without its sound (`show`; the overlay drive shows it over an artifact's page).
pub fn card(notice: &ReplyNotice) -> Notification {
    let session_id = notice.session_id.clone();
    let title = notice.title.clone();
    Notification::new()
        .id1::<ChatNotice>(gpui::SharedString::from(notice.session_id.clone()))
        .content(move |_, _, _| {
            div().w(px(TITLE_WIDTH)).subhead().semibold().truncate().child(title.clone()).into_any_element()
        })
        .on_click(move |_, window, cx| crate::nav::open_chat(session_id.clone(), window, cx))
        .w(px(CARD_WIDTH))
        .py(px(space::MD))
        .pl(px(space::LG))
        .pr(px(CLOSE_ROOM))
}

/// The first time Filo has something to show as a banner: if macOS hasn't asked yet, a note says why, then macOS asks.
pub fn ask_for_banners(need: Need, window: &mut Window, cx: &mut App) {
    if !crate::recorder::in_app_bundle() || ASKED_BANNERS.swap(true, Ordering::Relaxed) {
        return;
    }
    let permission = crate::notifications::permission();
    window
        .spawn(cx, async move |cx| {
            if permission.await != Ok(Permission::NotAsked) {
                return;
            }
            let why = match need {
                Need::Reply => "Filo can let you know when a reply comes in while you're in another app.",
                Need::Scheduled => {
                    "Filo can let you know when reminders, routines and check-ins come due, even while it's closed."
                }
            };
            let shown = cx.update(|window, cx| {
                let away = !window.is_window_active();
                window.push_notification(
                    Notification::new()
                        .id::<BannersNote>()
                        .title("Notifications")
                        .message(format!("{why} macOS will ask you to allow them."))
                        // Still there to read when the user comes back to the window.
                        .autohide(!away),
                    cx,
                );
            });
            if shown.is_err() {
                return;
            }
            cx.background_executor().timer(NOTE_FIRST).await;
            crate::notifications::request();
        })
        .detach();
}

fn play_sound() {
    // SAFETY: AppKit messages on the main thread; the sound is a shared, cached object.
    unsafe {
        let name = std::ffi::CString::new(SOUND).unwrap_or_default();
        let name: *mut Object = msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
        let sound: *mut Object = msg_send![class!(NSSound), soundNamed: name];
        if !sound.is_null() {
            let _: bool = msg_send![sound, play];
        }
    }
}

fn bounce_dock() {
    // SAFETY: an AppKit message on the main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: isize = msg_send![app, requestUserAttention: INFORMATIONAL_REQUEST];
    }
}

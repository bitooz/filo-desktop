// fn+⌃ held in Filo's window: what's said goes into the text field that has the cursor, as if typed: at the caret (over
// what's selected), spaced from the words beside it, the rest of the text left as it was. Nothing is sent. Where it goes
// is picked as the keys go down (`target`):
//
//   - a web view with the keyboard (the note editor, an artifact page): its focused field, by script (`type_into_web`)
//   - the open chat's message box: the chat records in its own recording row (screens/chat/voice.rs `dictate`)
//   - another of Filo's text fields (an agent's name or instructions, search, Settings): GPUI's input for it
//     (`Window::with_focused_input`, vendor/gpui/FILO.md), while this records with a pill at the window's foot
//   - none of those: the open chat's message box, as if it had the cursor
//
// A field that's lost the cursor by the time the words are back hands them to the open chat's box as well. The recording
// and its words are a voice message's (`AwayRecording`, live transcription, else the file through the relay), and the
// recording goes once they're in.

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render, Styled as _, Task,
    Window, div, prelude::FluentBuilder as _, px,
};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};

use crate::backend::backend;
use crate::screens::chat::conversation::{AwayRecording, Conversation};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};

/// Where fn+⌃'s words go.
pub enum Target {
    /// The open chat's message box, which records in its own row.
    Chat(Entity<Conversation>),
    Field(Field),
}

/// A text field outside the chat's message box.
#[derive(Clone)]
pub enum Field {
    /// One of Filo's own (GPUI's), the one typing goes to.
    Filo,
    /// A web view's: whichever of its fields has focus.
    Web(Retained<AnyObject>),
}

/// Where the words go if fn+⌃ goes down now, with `chat` the conversation on screen.
pub fn target(chat: Option<Entity<Conversation>>, window: &mut Window, cx: &mut App) -> Option<Target> {
    if let Some(web) = web_view_with_keys(window) {
        return Some(Target::Field(Field::Web(web)));
    }
    if window.with_focused_input(cx, |_, _, _| ()).is_some() {
        let composer = chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).composer().read(cx).focus_handle(cx).contains_focused(window, cx));
        return Some(if composer { Target::Chat(chat?) } else { Target::Field(Field::Filo) });
    }
    chat.map(Target::Chat)
}

/// `words` to go in between `before` and `after` (the characters beside the caret, none at the field's start or end):
/// trimmed, and spaced from words they'd otherwise run into. `INSERT_SCRIPT` does the same in a web view.
pub fn spaced(words: &str, before: Option<char>, after: Option<char>) -> String {
    let words = words.trim();
    let lead = before.is_some_and(|c| !c.is_whitespace() && !"([{\"'“‘".contains(c));
    let trail = after.is_some_and(|c| c.is_alphanumeric());
    format!("{}{words}{}", if lead { " " } else { "" }, if trail { " " } else { "" })
}

/// Types `words` into the GPUI field that has the keyboard, over its selection; false when none has it.
pub fn type_into_filo(words: &str, window: &mut Window, cx: &mut App) -> bool {
    window
        .with_focused_input(cx, |input, window, cx| {
            let Some(selection) = input.selected_text_range(false, window, cx) else { return false };
            let range = selection.range;
            let mut adjusted = None;
            // A few UTF-16 units each side: the character next to the caret, whole.
            let before = (range.start > 0)
                .then(|| input.text_for_range(range.start.saturating_sub(4)..range.start, &mut adjusted, window, cx))
                .flatten()
                .and_then(|text| text.chars().next_back());
            let after = input
                .text_for_range(range.end..range.end + 4, &mut adjusted, window, cx)
                .and_then(|text| text.chars().next());
            input.replace_text_in_range(None, &spaced(words, before, after), window, cx);
            true
        })
        .unwrap_or(false)
}

/// The web view inside the window's view that has the keyboard, if one does.
fn web_view_with_keys(window: &Window) -> Option<Retained<AnyObject>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    let view = handle.ns_view.as_ptr() as *mut AnyObject;
    let web_view = AnyClass::get(c"WKWebView")?;
    let ns_view = AnyClass::get(c"NSView")?;
    // SAFETY: AppKit messages on the main thread; `view` is GPUI's view, alive with its window, and the responder is
    // retained before it's kept.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return None;
        }
        let mut responder: *mut AnyObject = msg_send![ns_window, firstResponder];
        if responder.is_null() || responder == view {
            return None;
        }
        let is_view: Bool = msg_send![responder, isKindOfClass: ns_view];
        let inside: Bool = if is_view.as_bool() { msg_send![responder, isDescendantOf: view] } else { Bool::NO };
        if !inside.as_bool() {
            return None;
        }
        while !responder.is_null() && responder != view {
            let is_web: Bool = msg_send![responder, isKindOfClass: web_view];
            if is_web.as_bool() {
                // A page hidden (a glance scrolled away) hands the keys back on the next frame (vendor/gpui/FILO.md).
                let hidden: Bool = msg_send![responder, isHiddenOrHasHiddenAncestor];
                return if hidden.as_bool() { None } else { Retained::retain(responder) };
            }
            responder = msg_send![responder, superview];
        }
        None
    }
}

/// Inserts `words` (its argument) at the focused field's caret, as typed (`execCommand`, so the page's editor hears it
/// and can undo it), spaced as `spaced` spaces them; false when no field has focus.
const INSERT_SCRIPT: &str = r#"(function (words) {
  var el = document.activeElement;
  while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
  var field = el && !el.disabled && !el.readOnly && (el.tagName === 'TEXTAREA' ||
    (el.tagName === 'INPUT' && /^(text|search|email|url|tel|password)?$/i.test(el.getAttribute('type') || '')));
  if (!(field || (el && el.isContentEditable))) return false;
  var before = '', after = '';
  if (field) {
    before = el.value.slice(0, el.selectionStart).slice(-1);
    after = el.value.slice(el.selectionEnd, el.selectionEnd + 1);
  } else {
    var selection = getSelection();
    if (selection.rangeCount) {
      var range = selection.getRangeAt(0), around = range.cloneRange();
      around.selectNodeContents(el); around.setEnd(range.startContainer, range.startOffset);
      before = around.toString().slice(-1);
      around.selectNodeContents(el); around.setStart(range.endContainer, range.endOffset);
      after = around.toString().slice(0, 1);
    }
  }
  var text = words.trim();
  if (before && !/[\s(\[{"'“‘]/.test(before)) text = ' ' + text;
  if (after && /[\p{L}\p{N}]/u.test(after)) text += ' ';
  if (!document.execCommand('insertText', false, text)) {
    if (!field) return false;
    el.setRangeText(text, el.selectionStart, el.selectionEnd, 'end');
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
  }
  return true;
})"#;

/// Types `words` into `web_view`'s focused field (`INSERT_SCRIPT`); false when none has focus.
pub async fn type_into_web(web_view: &Retained<AnyObject>, words: &str) -> bool {
    evaluate_bool(web_view, &format!("{INSERT_SCRIPT}({})", serde_json::Value::from(words))).await
}

/// Runs `script` in `web_view`: whether it came to `true`.
pub async fn evaluate_bool(web_view: &Retained<AnyObject>, script: &str) -> bool {
    let (sender, answer) = futures::channel::oneshot::channel::<bool>();
    let sender = std::cell::Cell::new(Some(sender));
    let done = block2::RcBlock::new(move |result: *mut AnyObject, _error: *mut AnyObject| {
        let Some(sender) = sender.take() else { return };
        // SAFETY: WebKit hands the script's value (an NSNumber for a boolean) on the main thread.
        let typed = !result.is_null()
            && AnyClass::get(c"NSNumber").is_some_and(|number| unsafe {
                let is_number: Bool = msg_send![result, isKindOfClass: number];
                is_number.as_bool() && {
                    let value: Bool = msg_send![result, boolValue];
                    value.as_bool()
                }
            });
        sender.send(typed).ok();
    });
    let script = objc2_foundation::NSString::from_str(script);
    // SAFETY: `web_view` is a WKWebView, retained; the block is copied by WebKit and called once, on the main thread.
    unsafe {
        let _: () = msg_send![&**web_view, evaluateJavaScript: &*script, completionHandler: &*done];
    }
    answer.await.unwrap_or(false)
}

/// The words go into `field`, or the open chat's box (`fallback`) when it no longer has a field with the cursor.
pub async fn type_words(
    field: Field,
    words: String,
    fallback: Option<Entity<Conversation>>,
    cx: &mut gpui::AsyncWindowContext,
) -> bool {
    let typed = match &field {
        Field::Web(web_view) => type_into_web(web_view, &words).await,
        Field::Filo => cx.update(|window, cx| type_into_filo(&words, window, cx)).unwrap_or(false),
    };
    if typed {
        return true;
    }
    let Some(chat) = fallback else { return false };
    cx.update(|window, cx| chat.update(cx, |chat, cx| chat.type_dictated(&words, window, cx))).is_ok()
}

/// A recording for a field outside the chat's message box, while fn+⌃ is held, and what's shown of it.
pub struct Dictation {
    state: State,
    field: Option<Field>,
    /// When fn+⌃ went down.
    asked: Instant,
    _tick: Option<Task<()>>,
}

enum State {
    Idle,
    /// The pill is up and the microphone opens once it's drawn.
    Opening,
    Recording {
        recording: AwayRecording,
        started: Instant,
    },
    /// Let go: waiting for the words.
    Writing,
    /// Why nothing went in, for a few seconds.
    Note(String),
}

/// How long a note stays.
const NOTE_FOR: Duration = Duration::from_secs(4);
/// A recording's longest length (screens/chat/voice.rs MAX_VOICE_MS).
const MAX_MS: f64 = 10. * 60_000.;

impl Dictation {
    pub fn new() -> Dictation {
        Dictation { state: State::Idle, field: None, asked: Instant::now(), _tick: None }
    }

    /// Recording, or about to.
    pub fn holding(&self) -> bool {
        matches!(self.state, State::Opening | State::Recording { .. })
    }

    /// fn+⌃ went down with `field` to write in: records once the pill is drawn (the microphone takes 0.1–0.3 s to
    /// open, and the window doesn't draw meanwhile).
    pub fn hold(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        if self.holding() {
            return;
        }
        self.field = Some(field);
        self.asked = Instant::now();
        self.state = State::Opening;
        cx.notify();
        cx.on_next_frame(window, |_, window, cx| cx.on_next_frame(window, |this, window, cx| this.open(window, cx)));
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, State::Opening) {
            return;
        }
        crate::media::stop(cx);
        match AwayRecording::start(&backend(cx), self.asked) {
            Ok(recording) => self.state = State::Recording { recording, started: Instant::now() },
            Err(problem) => return self.note(problem, window, cx),
        }
        // The clock and the words heard; at the limit, or with the input gone, it's let go.
        self._tick = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let going = this
                    .update_in(cx, |this, window, cx| {
                        let State::Recording { recording, started } = &this.state else { return false };
                        let ended = recording.elapsed_ms() >= MAX_MS
                            || (!recording.is_recording() && started.elapsed() > Duration::from_secs(1))
                            || recording.heard().is_some_and(|heard| heard.borrow().stopped.is_some());
                        if ended {
                            this.release(None, window, cx);
                        }
                        cx.notify();
                        !ended
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Esc or another key while holding: nothing goes in.
    pub fn cancel(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.holding() {
            return false;
        }
        if let State::Recording { recording, .. } = std::mem::replace(&mut self.state, State::Idle) {
            recording.cancel();
        }
        self.state = State::Idle;
        self._tick = None;
        cx.notify();
        true
    }

    /// fn+⌃ let go (or the window left mid-hold): the words go into the field once they're back, or the open chat's
    /// box (`fallback`) if the field lost the cursor. A press too short to say anything is dropped.
    pub fn release(&mut self, fallback: Option<Entity<Conversation>>, window: &mut Window, cx: &mut Context<Self>) {
        let state = std::mem::replace(&mut self.state, State::Idle);
        let recording = match state {
            State::Recording { recording, .. } => recording,
            // Why it couldn't record stays its while.
            note @ State::Note(_) => return self.state = note,
            _ => {
                self._tick = None;
                return cx.notify();
            }
        };
        self._tick = None;
        cx.notify();
        let Some(field) = self.field.take() else { return recording.cancel() };
        let voice = match recording.finish() {
            Ok(voice) => voice,
            // Too short to be words: a brush of the keys.
            Err(problem) if problem.contains("too short") => return,
            Err(problem) => return self.note(problem, window, cx),
        };
        self.state = State::Writing;
        let backend = backend(cx);
        cx.spawn_in(window, async move |this, cx| {
            let words = voice.words(&backend).await;
            let problem = match words {
                Ok(words) => {
                    let typed = type_words(field, words, fallback, cx).await;
                    if typed {
                        voice.log_timing();
                    }
                    (!typed).then(|| "No text field had the cursor for what you said.".to_string())
                }
                Err(problem) => Some(problem),
            };
            voice.discard();
            this.update_in(cx, |this, window, cx| {
                if !matches!(this.state, State::Writing) {
                    return;
                }
                match problem {
                    Some(problem) => this.note(problem, window, cx),
                    None => this.state = State::Idle,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn note(&mut self, note: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state = State::Note(note);
        self._tick = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(NOTE_FOR).await;
            this.update(cx, |this, cx| {
                if matches!(this.state, State::Note(_)) {
                    this.state = State::Idle;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for Dictation {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let pill = |content: gpui::AnyElement| {
            div().absolute().bottom(px(28.)).left_0().right_0().flex().justify_center().child(
                div()
                    .max_w(px(560.))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .px(px(14.))
                    .py(px(8.))
                    .rounded(px(radius::FULL))
                    .bg(colors.raised)
                    .border_1()
                    .border_color(colors.border)
                    .shadow_md()
                    .child(content),
            )
        };
        let (elapsed, words) = match &self.state {
            State::Idle => return div().into_any_element(),
            State::Note(note) => {
                return pill(div().footnote().text_color(colors.text_secondary).child(note.clone()).into_any_element())
                    .into_any_element();
            }
            State::Writing => {
                return pill(
                    div().footnote().text_color(colors.text_secondary).child("Writing it in…").into_any_element(),
                )
                .into_any_element();
            }
            State::Opening => (0., String::new()),
            State::Recording { recording, .. } => (
                recording.elapsed_ms(),
                recording.heard().map(|heard| heard.borrow().words.trim().to_string()).unwrap_or_default(),
            ),
        };
        // The dot blinks each half second, like a recording light; the newest words stay in view.
        let lit = (elapsed / 500.) as i64 % 2 == 0;
        pill(
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .min_w_0()
                .child(div().flex_none().size(px(8.)).rounded(px(radius::FULL)).bg(colors.danger).opacity(if lit {
                    1.
                } else {
                    0.35
                }))
                .child(
                    div()
                        .flex_none()
                        .callout()
                        .medium()
                        .child(crate::format::minutes_seconds((elapsed / 1000.).floor().max(0.) as i64)),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .footnote()
                        .text_color(colors.text_secondary)
                        .when(words.is_empty(), |this| this.child("Release to write it in · Esc to cancel"))
                        .when(!words.is_empty(), |this| {
                            this.flex().justify_end().child(div().flex_none().child(words))
                        }),
                )
                .into_any_element(),
        )
        .into_any_element()
    }
}

/// Opens the shell's dictation view.
pub fn view(cx: &mut App) -> Entity<Dictation> {
    cx.new(|_| Dictation::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_spaced_from_their_neighbours() {
        assert_eq!(spaced(" Check the build. ", None, None), "Check the build.");
        assert_eq!(spaced("check the build", Some('o'), None), " check the build");
        assert_eq!(spaced("check the build", Some(' '), None), "check the build");
        assert_eq!(spaced("the build", Some(' '), Some('a')), "the build ");
        assert_eq!(spaced("the build", Some('e'), Some(',')), " the build");
        assert_eq!(spaced("the build", Some('('), Some(')')), "the build");
        assert_eq!(spaced("the build", Some('\n'), Some('\n')), "the build");
    }
}

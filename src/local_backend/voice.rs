// Voice messages on this Mac: the functions the chat's mic calls (screens/chat/voice.rs), for Filo's chats, the
// agents' and the group.
//
//   voiceRecordings:sendLive      the message, with the words live transcription heard (live_transcription.rs), sent
//                                 at once; its recording comes just after, with the ticket's token (`upload_voice`)
//   voiceRecordings:createUpload  a ticket for a recording that has no words yet
//   the upload route              `upload_voice` (backend.rs): the recording kept under attachments/, as the ticket says
//   voiceRecordings:send          the uploaded recording turned into text through Filo's relay, then sent like sendLive
//
// The message's text is what was said, so its turn reads it like a typed one and a chat is titled by it; the recording
// stays on this Mac like a sent file and plays on the message's bubble (`voiceRecording`). Tickets live as long as the
// app: a recording not uploaded before a restart is sent again from the start.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filo_local::store::{Voice, valid_id};
use filo_local::{Role, attachments, group};
use serde_json::{Value, json};

use super::{Local, arg, required};
use crate::live_transcription::{self, Relay};

/// A recording's shortest and longest length (the longest with a second's slack), and its largest file.
const MIN_VOICE_MS: f64 = 300.;
const MAX_VOICE_MS: f64 = 10. * 60_000. + 1_000.;
const MAX_VOICE_BYTES: usize = 10 * 1024 * 1024;
const VOICE_CONTENT_TYPE: &str = "audio/mp4";
pub const NOTHING_HEARD: &str = "No words were heard in this recording.";
const EXPIRED: &str = "This recording's upload has expired. Record it again.";

/// Uploads promised and not yet done, by token.
#[derive(Default)]
pub struct State {
    tickets: Mutex<HashMap<String, Ticket>>,
    /// Where recordings are transcribed; Filo's own relay unless a test says otherwise.
    relay: Mutex<Option<Relay>>,
}

#[derive(Clone)]
struct Ticket {
    session_id: String,
    /// `createUpload`'s id, which `send` names and its turn takes as its id.
    upload_id: String,
    storage_id: String,
    duration_ms: u64,
    /// The recording is saved.
    uploaded: bool,
}

/// The message a voice send made (or found, sent before).
struct Sent {
    message_id: String,
    turn_id: String,
    voice: Voice,
}

fn duration(args: &Value) -> Result<u64, String> {
    let ms = args.get("durationMs").and_then(Value::as_f64).ok_or("`durationMs` is required")?;
    if ms < MIN_VOICE_MS {
        return Err("Recording is too short.".into());
    }
    if ms > MAX_VOICE_MS {
        return Err("Recording is longer than 10 minutes.".into());
    }
    Ok(ms.round() as u64)
}

/// Runs `future` to its end from any thread, the backend's blocking ones included, on a runtime of its own.
fn block_on<T: Send>(future: impl Future<Output = T> + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a runtime for the transcription")
                    .block_on(future)
            })
            .join()
            .expect("the transcription thread")
    })
}

impl Local {
    pub(super) fn voice_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        Some(match path {
            "voiceRecordings:sendLive" => self.send_live(args),
            "voiceRecordings:createUpload" => self.create_upload(args),
            "voiceRecordings:send" => self.send_uploaded(args),
            _ => return None,
        })
    }

    /// The relay recordings are transcribed through (`voiceRecordings:send`).
    fn relay(&self) -> Relay {
        self.voice.relay.lock().unwrap().clone().unwrap_or_else(|| Relay::new(&live_transcription::origin()))
    }

    #[cfg(test)]
    pub(super) fn set_relay(&self, relay: Relay) {
        *self.voice.relay.lock().unwrap() = Some(relay);
    }

    fn ticket(&self, ticket: Ticket) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        self.voice.tickets.lock().unwrap().insert(token.clone(), ticket);
        token
    }

    /// `voiceRecordings:sendLive`: `{ sessionId, requestId, durationMs, transcript, … }` to `{ messageId, uploadId,
    /// token, turnId, sessionId }`. Sent again with the same `requestId`, it's the message the first one made.
    fn send_live(self: &Arc<Self>, args: &Value) -> Result<Value, String> {
        let session_id = required(args, "sessionId")?.to_string();
        let duration_ms = duration(args)?;
        let text = arg(args, "transcript").unwrap_or_default().trim();
        if text.is_empty() {
            return Err(NOTHING_HEARD.into());
        }
        let request_id = arg(args, "requestId")
            .filter(|id| valid_id(id))
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let voice = Voice { storage_id: attachments::reserve(VOICE_CONTENT_TYPE), duration_ms };
        let sent = self.send_voice(&session_id, &request_id, text, voice, args)?;
        let uploaded = attachments::file(&self.root, &sent.voice.storage_id).is_some();
        let token = self.ticket(Ticket {
            session_id: session_id.clone(),
            upload_id: request_id.clone(),
            storage_id: sent.voice.storage_id,
            duration_ms,
            uploaded,
        });
        Ok(
            json!({ "messageId": sent.message_id, "uploadId": request_id, "token": token, "turnId": sent.turn_id, "sessionId": session_id }),
        )
    }

    /// `voiceRecordings:createUpload`: `{ sessionId, durationMs }` to `{ uploadId, token }`.
    fn create_upload(&self, args: &Value) -> Result<Value, String> {
        let session_id = required(args, "sessionId")?.to_string();
        if self.store_of(&session_id).is_none() && self.group_session().as_deref() != Some(session_id.as_str()) {
            return Err("This conversation no longer exists.".into());
        }
        let duration_ms = duration(args)?;
        let upload_id = uuid::Uuid::new_v4().to_string();
        let storage_id = attachments::reserve(VOICE_CONTENT_TYPE);
        let token =
            self.ticket(Ticket { session_id, upload_id: upload_id.clone(), storage_id, duration_ms, uploaded: false });
        Ok(json!({ "uploadId": upload_id, "token": token }))
    }

    /// The upload route: the recording for ticket `token`, kept with the message it's for.
    pub fn upload_voice(&self, token: &str, bytes: &[u8]) -> Result<(), String> {
        let ticket = self.voice.tickets.lock().unwrap().get(token).cloned().ok_or(EXPIRED)?;
        if bytes.is_empty() || bytes.len() > MAX_VOICE_BYTES {
            return Err("Recording must be under 10 MB.".into());
        }
        attachments::save_as(&self.root, &ticket.storage_id, bytes)
            .map_err(|error| format!("Couldn't keep the recording: {error}"))?;
        if let Some(ticket) = self.voice.tickets.lock().unwrap().get_mut(token) {
            ticket.uploaded = true;
        }
        // The message's bubble plays the saved recording now.
        self.changed();
        Ok(())
    }

    /// `voiceRecordings:send`: `{ uploadId, … }` to `{ turnId, sessionId, messageId }`, once the uploaded recording is
    /// transcribed. Refused with what the relay said (an allowance used up, offline), the ticket stays for Send again.
    fn send_uploaded(self: &Arc<Self>, args: &Value) -> Result<Value, String> {
        let upload_id = required(args, "uploadId")?;
        let (token, ticket) = {
            let tickets = self.voice.tickets.lock().unwrap();
            let found = tickets
                .iter()
                .find(|(_, ticket)| ticket.upload_id == upload_id)
                .map(|(token, ticket)| (token.clone(), ticket.clone()));
            found.ok_or(EXPIRED)?
        };
        let voice = Voice { storage_id: ticket.storage_id.clone(), duration_ms: ticket.duration_ms };
        // Sent already (a retry after the reply was lost): the message it made.
        if let Some(sent) = self.voice_sent(&ticket.session_id, upload_id) {
            return Ok(json!({ "turnId": sent.turn_id, "sessionId": ticket.session_id, "messageId": sent.message_id }));
        }
        let file = attachments::file(&self.root, &ticket.storage_id)
            .filter(|_| ticket.uploaded)
            .ok_or("The recording didn't reach Filo. Send it again.")?;
        let relay = self.relay();
        let text = block_on(async move { live_transcription::transcribe_file(&relay, &file).await })
            .map_err(|problem| problem.message())?;
        if text.trim().is_empty() {
            return Err(NOTHING_HEARD.into());
        }
        let sent = self.send_voice(&ticket.session_id, upload_id, &text, voice, args)?;
        self.voice.tickets.lock().unwrap().remove(&token);
        Ok(json!({ "turnId": sent.turn_id, "sessionId": ticket.session_id, "messageId": sent.message_id }))
    }

    /// The voice message turn `turn_id` started in `session_id`, if it's there.
    fn voice_sent(&self, session_id: &str, turn_id: &str) -> Option<Sent> {
        if self.group_session().as_deref() == Some(session_id) {
            let db = self.db().ok()?;
            let message =
                group::thread_messages(db, turn_id).ok()?.into_iter().find(|message| message.role == Role::User)?;
            let voice = group::voices(db, &[message.id.as_str()]).ok()?.remove(&message.id)?;
            return Some(Sent { message_id: message.id, turn_id: turn_id.to_string(), voice });
        }
        let store = self.store_of(session_id)?;
        let message = store
            .messages(session_id)
            .ok()?
            .into_iter()
            .find(|message| message.role == Role::User && message.turn.as_deref() == Some(turn_id))?;
        Some(Sent { message_id: message.id, turn_id: turn_id.to_string(), voice: message.voice? })
    }

    /// Sends what was said as the user's message in `session_id` (an agent's chat, Filo's or the group), with its
    /// recording, the files and reply in `args`, and starts its turn `turn_id`; the message already there when it was
    /// sent before.
    fn send_voice(
        self: &Arc<Self>,
        session_id: &str,
        turn_id: &str,
        text: &str,
        voice: Voice,
        args: &Value,
    ) -> Result<Sent, String> {
        if let Some(sent) = self.voice_sent(session_id, turn_id) {
            return Ok(sent);
        }
        if let Some(group_session) = self.group_session().filter(|group| group == session_id) {
            self.group_send_as(&group_session, turn_id, text, args)?;
            // The user's message starts the thread named for its turn.
            let db = self.db()?;
            let message = group::thread_messages(db, turn_id)?
                .into_iter()
                .find(|message| message.role == Role::User)
                .ok_or("The voice message wasn't saved.")?;
            let message_id = message.id;
            group::set_voice(db, &message_id, &voice)?;
            self.changed();
            return Ok(Sent { message_id, turn_id: turn_id.to_string(), voice });
        }
        let files = self.sent_files(args)?;
        let message = self.user_turn(session_id, turn_id, text, files, Some(voice.clone()), args)?;
        self.changed();
        Ok(Sent { message_id: message.id, turn_id: turn_id.to_string(), voice })
    }

    /// A voice message's recording as the chat shows it (`voiceRecording`): it plays from this Mac once it's saved.
    pub(super) fn voice_json(&self, voice: &Voice) -> Value {
        let saved = attachments::file(&self.root, &voice.storage_id);
        json!({
            "storageId": saved.as_ref().map(|_| voice.storage_id.clone()),
            "durationMs": voice.duration_ms as f64,
            "url": saved.map(|path| format!("file://{}", path.display())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filo_local::Engine;
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;

    fn temp() -> crate::local_backend::test_support::TempLocal {
        let local = crate::local_backend::test_support::temp("voice");
        // Nothing reaches the real relay (this port is closed) or runs the real CLI.
        local.set_relay(Relay::new("http://127.0.0.1:9"));
        local.no_turns.store(true, std::sync::atomic::Ordering::SeqCst);
        local
    }

    /// A relay on this Mac that answers each recording with `answer` (a `final`'s text, or an error's JSON).
    fn relay(answer: &'static str) -> Relay {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                listener.set_nonblocking(true).unwrap();
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                while let Ok((stream, _)) = listener.accept().await {
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    socket.next().await;
                    if answer.starts_with('{') {
                        socket.send(Message::text(answer)).await.ok();
                        continue;
                    }
                    socket.send(Message::text(r#"{"type":"ready"}"#)).await.ok();
                    while let Some(Ok(message)) = socket.next().await {
                        if let Message::Text(text) = message
                            && text.as_str().contains("commit")
                        {
                            socket
                                .send(Message::text(json!({ "type": "final", "text": answer }).to_string()))
                                .await
                                .ok();
                        }
                    }
                }
            });
        });
        Relay::new(&origin)
    }

    /// An m4a of a second's tone, as the recorder makes them.
    fn recording(dir: &std::path::Path) -> Vec<u8> {
        std::fs::create_dir_all(dir).unwrap();
        let pcm: Vec<u8> =
            (0..16_000).flat_map(|n: i32| (((n as f32 / 10.).sin() * 8_000.) as i16).to_le_bytes()).collect();
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + pcm.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data");
        wav.extend((pcm.len() as u32).to_le_bytes());
        wav.extend(pcm);
        std::fs::write(dir.join("tone.wav"), wav).unwrap();
        let made = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(dir.join("tone.wav"))
            .arg(dir.join("tone.m4a"))
            .status()
            .unwrap();
        assert!(made.success());
        std::fs::read(dir.join("tone.m4a")).unwrap()
    }

    #[test]
    fn a_live_transcribed_message_is_sent_and_its_recording_kept() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let args = json!({ "sessionId": session, "requestId": "req-voice-1", "durationMs": 2400.0, "transcript": " Check the build, please. " });
        let sent = local.call("voiceRecordings:sendLive", &args).unwrap();
        assert_eq!((sent["uploadId"].as_str(), sent["turnId"].as_str()), (Some("req-voice-1"), Some("req-voice-1")));
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let message = &detail["messages"][0];
        assert_eq!(
            (message["_id"].as_str(), message["content"].as_str()),
            (sent["messageId"].as_str(), Some("Check the build, please."))
        );
        // Until the recording is saved, the bubble plays the app's own copy.
        assert_eq!(message["voiceRecording"], json!({ "storageId": null, "durationMs": 2400.0, "url": null }));
        assert_eq!(detail["session"]["title"].as_str(), Some("Check the build, please."));

        local.upload_voice(sent["token"].as_str().unwrap(), b"m4a bytes").unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let voice = &detail["messages"][0]["voiceRecording"];
        let url = voice["url"].as_str().unwrap();
        assert!(url.starts_with("file://") && url.ends_with(".m4a"), "{url}");
        assert_eq!(std::fs::read(url.trim_start_matches("file://")).unwrap(), b"m4a bytes");
        assert!(voice["storageId"].is_string());
        // And it's among what the chat shared (an agent's profile, Shared › Voice).
        let shared = local.call("chatShared:list", &json!({ "sessionId": session })).unwrap();
        let item = &shared["items"][0];
        assert_eq!(
            (item["kind"].as_str(), item["durationMs"].as_f64(), item["url"].as_str()),
            (Some("voice"), Some(2400.), Some(url))
        );

        // Sent again (the reply was lost): the same message, and no second one.
        let again = local.call("voiceRecordings:sendLive", &args).unwrap();
        assert_eq!(again["messageId"], sent["messageId"]);
        assert_eq!(
            local.call("voiceSessions:detail", &json!({ "id": session })).unwrap()["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let refused = |args: Value| local.call("voiceRecordings:sendLive", &args).unwrap_err();
        assert_eq!(refused(json!({ "sessionId": session, "durationMs": 2400.0, "transcript": "  " })), NOTHING_HEARD);
        assert!(
            refused(json!({ "sessionId": session, "durationMs": 100.0, "transcript": "hi" })).contains("too short")
        );
        assert!(
            refused(json!({ "sessionId": "gone", "durationMs": 2400.0, "transcript": "hi" }))
                .contains("no longer exists")
        );
        // A note shared into the chat goes with a voice message too, for the engine and not the bubble.
        let noted = json!({ "sessionId": session, "requestId": "req-voice-2", "durationMs": 2400.0, "transcript": "Add milk", "sharedNotes": [{ "title": "Groceries", "body": "- eggs" }] });
        local.call("voiceRecordings:sendLive", &noted).unwrap();
        let messages = local.agent_chats.messages(&session).unwrap();
        assert_eq!(messages.last().unwrap().text, "Add milk");
        let context =
            std::fs::read_to_string(local.agent_chats.root().join("chats").join(&session).join("context.json"))
                .unwrap();
        assert!(
            context.contains("A note the user shared from their Notes:\\nTitle: Groceries\\n\\n- eggs"),
            "{context}"
        );
        assert_eq!(local.upload_voice("no-such-ticket", b"x").unwrap_err(), EXPIRED);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_recording_without_words_is_transcribed_through_the_relay() {
        let local = temp();
        let session = local.agent_chats.create_chat(Engine::Claude).unwrap().id;
        let bytes = recording(&local.root.join("made"));
        let ticket =
            local.call("voiceRecordings:createUpload", &json!({ "sessionId": session, "durationMs": 1000.0 })).unwrap();
        let (upload_id, token) = (ticket["uploadId"].as_str().unwrap(), ticket["token"].as_str().unwrap());
        // Not uploaded yet.
        assert!(
            local.call("voiceRecordings:send", &json!({ "uploadId": upload_id })).unwrap_err().contains("didn't reach")
        );
        local.upload_voice(token, &bytes).unwrap();

        // Offline, then refused: the ticket stays for Send again, and nothing is sent.
        assert!(local.call("voiceRecordings:send", &json!({ "uploadId": upload_id })).unwrap_err().contains("offline"));
        local.set_relay(relay(
            r#"{"type":"error","code":"usage_limit","scope":"ip","message":"…","resetAt":1800000000000}"#,
        ));
        let refused = local.call("voiceRecordings:send", &json!({ "uploadId": upload_id })).unwrap_err();
        assert!(refused.starts_with("You've used up today's voice messages. They're back "), "{refused}");
        assert!(
            local.call("voiceSessions:detail", &json!({ "id": session })).unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        local.set_relay(relay("Milo, what's on today?"));
        let sent = local.call("voiceRecordings:send", &json!({ "uploadId": upload_id })).unwrap();
        assert_eq!(sent["turnId"].as_str(), Some(upload_id));
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let message = &detail["messages"][0];
        assert_eq!(message["content"].as_str(), Some("Milo, what's on today?"));
        assert_eq!(
            std::fs::read(message["voiceRecording"]["url"].as_str().unwrap().trim_start_matches("file://")).unwrap(),
            bytes
        );
        // The ticket is used up.
        assert_eq!(local.call("voiceRecordings:send", &json!({ "uploadId": upload_id })).unwrap_err(), EXPIRED);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn the_group_takes_voice_messages() {
        let local = temp();
        let group = local.call("agents:openGroup", &json!({})).unwrap();
        let session =
            group.as_str().map(str::to_string).or_else(|| group["sessionId"].as_str().map(str::to_string)).unwrap();
        let args = json!({ "sessionId": session, "requestId": "req-group-1", "durationMs": 1800.0, "transcript": "Plan the trip." });
        let sent = local.call("voiceRecordings:sendLive", &args).unwrap();
        local.upload_voice(sent["token"].as_str().unwrap(), b"group m4a").unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let message =
            detail["messages"].as_array().unwrap().iter().find(|message| message["role"] == "user").unwrap().clone();
        assert_eq!(
            (message["_id"].as_str(), message["content"].as_str()),
            (sent["messageId"].as_str(), Some("Plan the trip."))
        );
        assert_eq!(message["voiceRecording"]["durationMs"].as_f64(), Some(1800.));
        assert_eq!(
            std::fs::read(message["voiceRecording"]["url"].as_str().unwrap().trim_start_matches("file://")).unwrap(),
            b"group m4a"
        );
        // Its turn is the group's, under the voice message.
        let turns = local.call("voiceSessions:chatTurns", &json!({ "sessionId": session })).unwrap();
        assert_eq!(turns[0]["userMessageId"], message["_id"]);
        // Again: the same message.
        assert_eq!(local.call("voiceRecordings:sendLive", &args).unwrap()["messageId"], sent["messageId"]);
        // One with a picture shows it, as an agent's chat would.
        let picture = local.save_attachment("image/png", b"png").unwrap();
        let with_file = json!({ "sessionId": session, "requestId": "req-group-2", "durationMs": 1800.0, "transcript": "Look", "attachments": [{ "storageId": picture, "name": "shot.png", "contentType": "image/png" }] });
        let sent = local.call("voiceRecordings:sendLive", &with_file).unwrap();
        let detail = local.call("voiceSessions:detail", &json!({ "id": session })).unwrap();
        let message = detail["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["_id"] == sent["messageId"])
            .unwrap()
            .clone();
        assert_eq!(message["attachments"][0]["name"].as_str(), Some("shot.png"));
        std::fs::remove_dir_all(&local.root).ok();
    }
}

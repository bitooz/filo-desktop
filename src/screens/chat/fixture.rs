// `FILO_OPEN=coding-cards` (Filo's chat) or `FILO_OPEN=coding-cards:agent` (an agent's chat, Desktop App Developer's):
// the chat with made-up messages, coding question cards and work, and the working panel beside it, in a window of its
// own with no backend behind it (answers go to a script that fails the first send, then takes the
// retry). It mirrors a reported layout: two disk-space questions answered long ago, Filo's later reply below them,
// then open questions. This chat's tasks are its two Claude Code tasks; its own reply is under way, which shows on its
// row in the agents' list (a stand-in for the Agents tab's), never in the badge; and other chats work too (another
// agent, a coding task of another chat's), which the badge leaves out. Rows the panel opens show a stand-in page with
// Back, the way the app pushes a page over the chat and keeps it.
//
// `FILO_FIXTURE_TASKS=<n>` adds that many more working coding tasks (a popover long enough to scroll),
// `FILO_FIXTURE_REPLIES=1` has two answers say what they answer, one with a reply link and one without, and
// `FILO_FIXTURE_BROWSER=<name>` (with `:agent`) names the agent <name> and has it work in Filo's browser, its pill at
// the header's end (agents.rs `browser_pill`). `FILO_FIXTURE_AUDIO=<file>,<file>` ends the chat with those sound files
// sent, the first by the user and the rest by Filo, each in its audio row (audio_row.rs; a test window plays them
// muted), and the plan's `audio` step logs the sound playing or paused.
//
// `FILO_FIXTURE_DRIVE=1` clicks and types through it with in-app AppKit events (smoke.rs `native_input`), logging each
// step and saving window screenshots to `FILO_FIXTURE_SHOTS` (and frames for a recording to `FILO_FIXTURE_FRAMES`),
// then quits.

use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, ClickEvent, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px, size,
};
use serde_json::json;

use super::coding_requests;
use super::coding_sessions::CodingSession;
use super::conversation::Conversation;
use super::reply_status;
use super::types::{ChatTurn, Detail};
use super::working_panel::{self, OnOpen, PanelRow, Target};
use crate::companion::{Opens, Working};
use crate::smoke::native_input;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open.starts_with("coding-cards"))
}

pub fn open(cx: &mut App) {
    let agent = std::env::var("FILO_OPEN").is_ok_and(|open| open == "coding-cards:agent");
    let (width, height) = crate::test_run::window_size((1280., 860.));
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Filo fixture".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            // Above other windows without taking the focus: the scripted check runs while the Mac is in use.
            kind: gpui::WindowKind::PopUp,
            focus: false,
            ..Default::default()
        },
        |window, cx| {
            let fixture = cx.new(|cx| ChatFixture::new(agent, window, cx));
            if std::env::var_os("FILO_FIXTURE_DRIVE").is_some() {
                drive(fixture.clone(), window, cx);
            }
            cx.new(|cx| gpui_component::Root::new(fixture, window, cx))
        },
    )
    .expect("open window");
}

pub struct ChatFixture {
    agent: bool,
    conversation: Entity<Conversation>,
    others: Vec<Working>,
    /// What a panel row opened (a stand-in page over the chat), until Back.
    page: Option<String>,
}

const MINUTE: f64 = 60_000.;

impl ChatFixture {
    fn new(agent: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let now = crate::format::now_ms();
        let name = if agent { "Desktop App Developer" } else { "Filo" };
        let conversation = cx.new(|cx| Conversation::new(None, name.into(), window, cx));
        let message = |id: &str, minutes: f64, role: &str, content: &str| json!({ "_id": id, "_creationTime": now - minutes * MINUTE, "role": role, "content": content });
        let mut messages = vec![
            message(
                "u1",
                62.,
                "user",
                "My Mac is almost out of space. Can Claude Code clean up what's safe to delete?",
            ),
            message(
                "a1",
                60.,
                "assistant",
                "On it. I've asked Claude Code to look for big caches in **notes-app** and to check with you before deleting anything.",
            ),
            message(
                "a2",
                40.,
                "assistant",
                "Claude Code freed **4.1 GB** by deleting the Rust incremental cache. The simulator caches stay, as you asked.",
            ),
            message("u2", 12., "user", "Can Filo keep running when I quit the app?"),
            message(
                "a3",
                10.,
                "assistant",
                "Yes. In **Settings › Pet, shortcuts & menu bar**, turn on *Keep Filo in the menu bar*. Closing the window then hides Filo, and ⌘Q asks before it stops coding tasks that are still working.",
            ),
            message("u3", 2., "user", "Can you check how the Working popover looks now?"),
        ];
        // Claude Code's round that freed the space reported here: its Open proof sits at the foot of this message; the
        // later reply reports another task's round, and opens that one.
        messages[2]["codingResult"] = json!({ "taskId": "task-disk", "round": 1 });
        messages[4]["codingResult"] = json!({ "taskId": "task-sidebar", "round": 2 });
        // `FILO_FIXTURE_REPLIES=1`: the answers say what they answer (reply_link.rs). The second to u1 is past the
        // question cards and its own first message, so no link; the one to u2 is past another message of the user's, and
        // the one to that message past the answer to u2, so both have one.
        if std::env::var_os("FILO_FIXTURE_REPLIES").is_some() {
            messages[1]["replyToMessageId"] = json!("u1");
            messages[2]["replyToMessageId"] = json!("u1");
            messages[4]["replyToMessageId"] = json!("u2");
            messages.insert(4, message("u2b", 11., "user", "And does it start again after a restart?"));
            let mut again = message("a3b", 9.5, "assistant", "It does: Filo opens at login and picks up where it was.");
            again["replyToMessageId"] = json!("u2b");
            messages.insert(6, again);
        }
        messages.extend(media_messages(now));
        messages.extend(audio_messages(now));
        let detail: Detail = serde_json::from_value(json!({
            "session": { "_id": "fixture-chat", "title": "Disk space", "seenUntil": now },
            "messages": messages,
            "hasMore": false,
        }))
        .expect("fixture detail");
        // The reply to the last message, under way: the chat's own, so not one of its tasks.
        let turns: Vec<ChatTurn> = serde_json::from_value(json!([
            { "turnId": "turn-u3", "requestId": "request-u3", "createdAt": now, "userMessageId": "u3", "state": "pending" },
        ]))
        .expect("fixture turns");
        let sessions: Vec<CodingSession> = serde_json::from_value(json!([
            { "taskId": "task-disk", "tool": "claude-code", "title": "Free up disk space", "status": "running", "now": "Waiting for your answer",
              "pending": 2.0, "createdAt": now - 59. * MINUTE, "updatedAt": now - MINUTE },
            // Started 14 hours ago; its second round has run 6 minutes. Its title is longer than a row.
            { "taskId": "task-sidebar", "tool": "claude-code", "title": "Restore docked Working sidebar and keep each task on one line", "status": "running",
              "now": "Running cargo test --bin filo -- working_panel coding_sessions", "pending": 0.0, "createdAt": now - 14. * 60. * MINUTE,
              "updatedAt": now - MINUTE, "roundStartedAt": now - 6. * MINUTE, "repoName": "notes-app", "branch": "main",
              "model": "claude-opus-5-5", "computer": { "name": "Sam's MacBook", "lastSeenAt": now - MINUTE } },
        ]))
        .expect("fixture sessions");
        // `FILO_FIXTURE_TASKS=<n>`: that many more coding tasks working, so the popover's list scrolls.
        let extra: usize = std::env::var("FILO_FIXTURE_TASKS").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
        let sessions: Vec<CodingSession> = sessions
            .into_iter()
            .chain((0..extra).map(|n| {
                serde_json::from_value(json!({ "taskId": format!("task-more-{n}"), "tool": "claude-code", "title": format!("Another task {}", n + 1),
                    "status": "running", "now": "Running cargo build", "pending": 0.0, "createdAt": now - 30. * MINUTE, "updatedAt": now - MINUTE }))
                .expect("fixture session")
            }))
            .collect();
        conversation.update(cx, |conversation, cx| {
            // Its two Claude Code tasks.
            conversation.coding_sessions = sessions;
            let mut requests = coding_requests::fixture_requests(now);
            // With the answers' links (`FILO_FIXTURE_REPLIES`) or sounds (`FILO_FIXTURE_AUDIO`), no open questions:
            // their panel would cover them.
            if std::env::var_os("FILO_FIXTURE_REPLIES").is_some() || std::env::var_os("FILO_FIXTURE_AUDIO").is_some() {
                requests.retain(|request| request.state == "answered");
            }
            conversation.load_fixture(detail, requests, cx);
            conversation.set_fixture_turns(turns, cx);
        });
        // The working chats, as the companion has them (`voiceSessions:working`, `coding:workingChats`): this chat, for
        // its reply under way, and others: another of Filo's chats, another agent's (Context Engineer, as in the owner's
        // screenshot) and a chat only a coding task works for. None of it is this chat's tasks, so none of it shows in
        // the badge (`work` logs them beside what does); the agents' list says which agents' chats work.
        let this_chat = if agent { Opens::Agent("agent-dev".into()) } else { Opens::Chat };
        let others = vec![
            Working {
                session_id: "fixture-chat".into(),
                title: name.into(),
                since: Some(now - 2. * MINUTE),
                opens: this_chat,
            },
            Working {
                session_id: "fixture-context".into(),
                title: "Context Engineer".into(),
                since: Some(now - 9. * MINUTE),
                opens: Opens::Agent("agent-context".into()),
            },
            Working {
                session_id: "fixture-other-coding".into(),
                title: "Fix the iOS widget".into(),
                since: Some(now - 20. * MINUTE),
                opens: Opens::Chat,
            },
            Working {
                session_id: "fixture-lisbon".into(),
                title: "Plan the Lisbon trip".into(),
                since: Some(now - 6. * MINUTE),
                opens: Opens::Chat,
            },
        ];
        ChatFixture { agent, conversation, others, page: None }
    }

    fn opener(&self, cx: &mut Context<Self>) -> OnOpen {
        let me = cx.entity().downgrade();
        let conversation = self.conversation.clone();
        std::rc::Rc::new(move |target: &Target, _window: &mut Window, cx: &mut App| {
            let page = match target {
                Target::Question(task) => {
                    if conversation.update(cx, |conversation, cx| conversation.reveal_question(task, cx)) {
                        eprintln!("fixture: panel row scrolled to the open question of {task}");
                        return;
                    }
                    format!("Coding task {task}")
                }
                Target::CodingTask(id) => format!("Coding task {id}"),
            };
            eprintln!("fixture: panel row opened {page}");
            me.update(cx, |this, cx| {
                this.page = Some(page);
                cx.notify();
            })
            .ok();
        })
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.page = None;
        cx.notify();
    }

    /// This chat's tasks: all the badge and the panel show.
    fn rows(&self, cx: &App) -> Vec<PanelRow> {
        working_panel::here_for(self.conversation.read(cx))
    }

    /// The agents' list as the Agents tab works it out (chat/reply_status.rs): (id, name, picture, working since).
    fn agent_rows(&self, cx: &App) -> Vec<(&'static str, &'static str, &'static str, Option<Option<f64>>)> {
        let reply = reply_status::reply_since(self.conversation.read(cx));
        [
            ("agent-dev", "Desktop App Developer", "fox-laptop"),
            ("agent-context", "Context Engineer", "cat-idle"),
            ("agent-coach", "Gym Coach", "bear-waving"),
        ]
        .into_iter()
        .map(|(id, name, picture)| {
            (
                id,
                name,
                picture,
                reply_status::list_working(id, &self.others, if id == "agent-dev" { reply } else { None }),
            )
        })
        .collect()
    }
}

/// `FILO_FIXTURE_MEDIA=<folder>`: pictures sent and received at the end of the chat, below the questions (one, two,
/// three), for checking how they sit on the chat's background: `<folder>/dark.png`, `transparent.png`, `tiny.png`,
/// `wide.png` and `light.png`, drawn from there as a chat's files are.
fn media_messages(now: f64) -> Vec<serde_json::Value> {
    let Ok(base) = std::env::var("FILO_FIXTURE_MEDIA") else { return Vec::new() };
    let base = base.strip_prefix("file://").unwrap_or(&base).to_string();
    let picture = |name: &str| json!({ "storageId": name, "name": format!("{name}.png"), "contentType": "image/png", "size": 1.0, "localFile": format!("{base}/{name}.png") });
    let message = |id: &str, minutes: f64, role: &str, names: &[&str]| json!({ "_id": id, "_creationTime": now - minutes * MINUTE, "role": role, "content": "", "attachments": names.iter().map(|name| picture(name)).collect::<Vec<_>>() });
    vec![
        message("m1", 1.6, "user", &["dark"]),
        message("m2", 1.2, "assistant", &["transparent"]),
        message("m3", 0.8, "user", &["tiny", "dark"]),
        message("m4", 0.4, "assistant", &["wide", "transparent", "light"]),
    ]
}

/// `FILO_FIXTURE_AUDIO=<file>,<file>`: the first sent by the user, the rest by Filo, played from where they are.
fn audio_messages(now: f64) -> Vec<serde_json::Value> {
    let Ok(files) = std::env::var("FILO_FIXTURE_AUDIO") else { return Vec::new() };
    files
        .split(',')
        .filter(|file| !file.is_empty())
        .enumerate()
        .map(|(index, file)| {
            let path = std::path::Path::new(file);
            let name = path.file_name().map_or_else(|| file.to_string(), |name| name.to_string_lossy().into_owned());
            let size = std::fs::metadata(path).map_or(0., |meta| meta.len() as f64);
            let content_type = crate::screens::chat::attachments::content_type_for(path);
            let attachment = json!({ "storageId": format!("sound-{index}"), "name": name, "contentType": content_type, "size": size, "localFile": file });
            let role = if index == 0 { "user" } else { "assistant" };
            json!({ "_id": format!("s{index}"), "_creationTime": now - (0.3 - index as f64 * 0.01) * MINUTE, "role": role, "content": "", "attachments": [attachment] })
        })
        .collect()
}

impl ChatFixture {
    /// The coding session's open questions follow the questions panel, as `coding:sessionsForChat` would: answered,
    /// its "Needs you" clears from the working panel, the indicator and the timeline row.
    fn sync_pending(&mut self, cx: &mut Context<Self>) {
        let conversation = self.conversation.read(cx);
        let open = conversation.question_panel().read(cx).waiting_for("task-disk") as f64;
        let mut sessions = conversation.coding_sessions.clone();
        if sessions.first().is_some_and(|session| session.pending != open) {
            sessions[0].pending = open;
            if open == 0. {
                sessions[0].now = "Deleting old-experiments' node_modules".into();
            }
            self.conversation.update(cx, |conversation, cx| conversation.set_fixture_sessions(sessions, cx));
        }
    }
}

impl Render for ChatFixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Esc closes what's on top, as in the app (escape.rs).
        crate::escape::frame(window, cx);
        self.sync_pending(cx);
        let colors = *cx.colors();
        let content = f32::from(window.viewport_size().width);
        ui::set_content_width(content);
        // As in the app: the agents' list beside an agent's chat takes its room.
        let list_width = if self.agent { 280. } else { 0. };
        let room = content - list_width;
        let here = self.rows(cx);
        let layout = working_panel::layout(room, !here.is_empty(), cx);
        let opener = self.opener(cx);
        let details_open = layout.indicator && cx.default_global::<working_panel::WorkPlacement>().details_open;
        let browser_agent = std::env::var("FILO_FIXTURE_BROWSER").ok().filter(|_| self.agent);
        let title = browser_agent
            .clone()
            .unwrap_or_else(|| if self.agent { "Desktop App Developer" } else { "Disk space" }.to_string());
        // The name and the badge share the bar as in the app (agents.rs `render_chat`): never overlapping.
        let lead = if self.agent { space::MD + 28. + space::SM } else { 80. };
        let name_width = working_panel::text_width(
            &title,
            crate::theme::TypeVariant::Headline.size(),
            gpui::FontWeight::SEMIBOLD,
            cx,
        );
        let header_width = room - layout.docked.unwrap_or(0.);
        let browser = browser_agent.as_ref().map(|name| {
            let room = working_panel::side_badge_room(header_width, lead, name_width, space::SM);
            crate::screens::agents::browser_pill(
                name,
                crate::browser::Control::Agent,
                room,
                std::rc::Rc::new(|_| {}),
                cx,
            )
        });
        let trailing = browser.as_ref().map_or(0., |(_, width)| *width) + space::SM;
        let pill = if layout.indicator {
            working_panel::indicator_width(here.len(), &working_panel::status_text(&here).unwrap_or_default(), cx)
        } else {
            0.
        };
        let fit = working_panel::fit_header(header_width, lead, name_width, trailing, pill);
        let indicator = layout.indicator.then(|| {
            let text = working_panel::status_text(&here).unwrap_or_default();
            let needs_you = here.iter().any(|row| row.state == "waiting");
            let toggle: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = std::rc::Rc::new(move |_, cx| {
                working_panel::activate(
                    room,
                    |cx| {
                        let state = cx.default_global::<working_panel::WorkPlacement>();
                        state.details_open = !state.details_open;
                    },
                    cx,
                )
            });
            working_panel::indicator(
                "fixture-work-badge",
                working_panel::faces(&here, cx),
                text,
                fit.pill,
                needs_you,
                details_open,
                toggle,
                window,
                cx,
            )
        });
        // The popover as each screen draws it: an agent's chat's (`details`), or Filo's chat's.
        let details = details_open.then(|| {
            if self.agent {
                return working_panel::details(&here, opener.clone(), cx);
            }
            working_panel::popover("work-sheet", 44., &here, opener.clone(), cx)
        });
        let right = layout.docked.map(|width| working_panel::render(&here, width, opener.clone(), cx));
        working_panel::escape_layers(&layout, details_open, window, cx);
        let header = div()
            .relative()
            .flex_none()
            .h(px(44.))
            .pl(px(if self.agent { space::MD } else { 80. }))
            .pr(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .when(self.agent, |this| {
                this.child(crate::screens::agents::round_picture(
                    crate::screens::agents::picture_source("fox-laptop"),
                    28.,
                    cx,
                ))
            })
            .child(working_panel::header_name(&title, fit.name, cx))
            .child(div().flex_1())
            .children(browser.map(|(pill, _)| pill))
            .children(indicator.map(|indicator| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(fit.left))
                    .w(px(fit.pill))
                    .flex()
                    .items_center()
                    .child(indicator)
            }));
        let body = match &self.page {
            Some(page) => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::MD))
                .child(div().title3().child(page.clone()))
                .child(
                    div()
                        .footnote()
                        .text_color(colors.text_secondary)
                        .child("Stand-in for the page the app pushes over the chat."),
                )
                .child(crate::smoke::tracked(
                    "fixture-back",
                    ui::Button::secondary("fixture-back", "Back")
                        .icon("arrow-left")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.back(cx))),
                ))
                .into_any_element(),
            None => div().flex_1().min_h_0().child(self.conversation.clone()).into_any_element(),
        };
        // The agents' list, as a stand-in (an agent's chat sits right of it in the Agents tab): a working agent's row says
        // "Working 2m" in place of its last message, as the Agents tab's does.
        let agent_rows = self.agent_rows(cx);
        let agent_list = self.agent.then(|| {
            div()
                .w(px(list_width))
                .flex_none()
                .h_full()
                .pt(px(52.))
                .px(px(space::MD))
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .bg(colors.background)
                .border_r_1()
                .border_color(colors.border.opacity(0.6))
                .children(agent_rows.into_iter().map(|(id, name, picture, working)| {
                    let second = match working {
                        Some(since) => reply_status::list_line(&format!("agent-{id}"), since, cx),
                        None => div()
                            .footnote()
                            .text_color(colors.text_secondary)
                            .truncate()
                            .child("Sounds good, talk tomorrow.")
                            .into_any_element(),
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .p(px(space::SM))
                        .rounded(px(radius::MD))
                        .when(id == "agent-dev", |this| this.bg(colors.selected_fill))
                        .child(crate::screens::agents::round_picture(
                            crate::screens::agents::picture_source(picture),
                            28.,
                            cx,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().subhead().semibold().child(name))
                                .child(second),
                        )
                }))
        });
        div()
            .size_full()
            .flex()
            .bg(colors.chat_background)
            .text_color(colors.text)
            .children(agent_list)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(body)
                    .children(details),
            )
            .children(right)
    }
}

// ---- The scripted check ----

/// The Mac's key code for a character on a US keyboard (0, the A key, for others).
fn key_code(character: char) -> u16 {
    const KEYS: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.";
    match character.to_ascii_lowercase() {
        ' ' => 49,
        lower => KEYS.chars().position(|key| key == lower).map_or(0, |at| at as u16),
    }
}

/// `FILO_FIXTURE_PLAN`: comma-separated steps. `shot:<name>`, `click:<id>`, `rclick:<id>` (right-click), `menu:<n>` (the
/// open menu's n-th item, by Down and Return), `tab:<focus key>` (Tab until it has the keyboard: an indicator
/// `fixture-work-badge`), `key:<name>` (enter, space, up, down, left, right, escape, backspace, a digit or a letter), `type:<text>`,
/// `panel` (what the questions panel shows and who has the keyboard), `layout:<message id>` (what moved), `wait:<ms>`, `log` (placement,
/// its file, the timeline), `work` (the indicator's words and the work cards), `quiet` (this chat's work ends, so only
/// other chats work), `finish:<chat id>` (that other chat's work ends), `replied` (this chat's reply is done), `has:<id>`
/// (whether it's on screen), `tip` (whose title details show), `title:<task id>=<text>` / `doing:<task id>=<text>`
/// (a coding task's title or what it's doing now), `draft` (a draft survives opening a panel row and coming back),
/// `wheel:<id>=<dy>` (scroll the wheel over it and log whether the chat moved).
fn drive(fixture: Entity<ChatFixture>, window: &mut Window, cx: &mut App) {
    let width = f64::from(f32::from(window.bounds().size.width));
    let shots = std::env::var("FILO_FIXTURE_SHOTS").unwrap_or_else(|_| "/tmp/filo-cards".into());
    let _ = std::fs::create_dir_all(&shots);
    if let Ok(frames) = std::env::var("FILO_FIXTURE_FRAMES") {
        let _ = std::fs::create_dir_all(&frames);
        cx.spawn(async move |cx| {
            let executor = cx.background_executor().clone();
            executor.timer(Duration::from_millis(1500)).await;
            let mut index = 0;
            while let Some(number) = native_input::window_number(width) {
                crate::window_shot::save(number, format!("{frames}/{index:05}.png"));
                index += 1;
                executor.timer(Duration::from_millis(120)).await;
            }
        })
        .detach();
    }
    let plan: Vec<String> = std::env::var("FILO_FIXTURE_PLAN")
        .unwrap_or_else(|_| "shot:initial,log".into())
        .split(',')
        .map(|step| step.trim().to_string())
        .filter(|step| !step.is_empty())
        .collect();
    let window_handle = window.window_handle();
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shot = |name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("fixture: shot {path}");
            }
        };
        let center = |id: &str| match crate::smoke::on_screen(id) {
            Some(bounds) => {
                let center = bounds.center();
                Some((f64::from(f32::from(center.x)), f64::from(f32::from(center.y))))
            }
            None => {
                eprintln!("fixture: FAIL {id} isn't on screen");
                None
            }
        };
        let click = |id: &str| {
            if let Some((x, y)) = center(id) {
                eprintln!("fixture: click {id}");
                native_input::click(width, x, y);
            }
        };
        let key = |characters: &str, code: u16, flags: u64| native_input::key(width, characters, code, flags);
        // Each character with its own key (GPUI names a key by its code), shifted when it's a capital or "@".
        let type_char = |character: char| {
            let (key, shifted) = match character {
                '@' => ('2', true),
                character => (character, character.is_ascii_uppercase()),
            };
            let flags = if shifted { native_input::SHIFT } else { 0 };
            native_input::key(width, &character.to_string(), key_code(key), flags);
        };
        let type_text = |text: &str| text.chars().for_each(type_char);
        let rows = |cx: &mut gpui::AsyncApp| fixture.read_with(cx, |fixture, cx| fixture.conversation.read(cx).row_labels()).unwrap_or_default();
        // What the questions panel shows, whether it or the composer has the keyboard, and the composer's text.
        let panel = |cx: &mut gpui::AsyncApp| {
            cx.update_window(window_handle, |_, window, cx| {
                let conversation = fixture.read(cx).conversation.read(cx);
                let panel = conversation.question_panel();
                let composer = conversation.composer();
                let keyboard = if panel.read(cx).has_focus(window) {
                    "panel"
                } else if gpui::Focusable::focus_handle(composer.read(cx), cx).is_focused(window) {
                    "composer"
                } else {
                    "elsewhere"
                };
                format!("{} · keyboard on {keyboard} · composer {:?}", panel.read(cx).describe(), composer.read(cx).value())
            })
            .unwrap_or_default()
        };
        let placement = |cx: &mut gpui::AsyncApp| cx.update(working_panel::placement).ok();
        let settings = || std::fs::read_to_string(crate::config::config_dir().join("desktop-working.json")).unwrap_or_else(|_| "(no file)".into());
        let focused_place = |cx: &mut gpui::AsyncApp, choice: &str| {
            cx.update_window(window_handle, |_, window, cx| working_panel::is_focused(choice, window, cx)).unwrap_or(false)
        };

        wait(2500).await;
        eprintln!("fixture: started with placement {:?}, file {:?}", placement(cx), settings().trim());
        for step in plan {
            let (verb, arg) = step.split_once(':').map(|(verb, arg)| (verb.to_string(), arg.to_string())).unwrap_or((step.clone(), String::new()));
            match verb.as_str() {
                "shot" => shot(&arg),
                "wait" => wait(arg.parse().unwrap_or(500)).await,
                "click" => {
                    click(&arg);
                    wait(600).await;
                }
                // `hover:<id>` points at its middle; `hover:<id>@<x>x<y>` at that many points from its top left.
                "hover" => {
                    let (target, at) = arg.split_once('@').map(|(target, at)| (target.to_string(), Some(at.to_string()))).unwrap_or((arg.clone(), None));
                    let point = match (crate::smoke::on_screen(&target), at.and_then(|at| at.split_once('x').and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?))))) {
                        (Some(bounds), Some((x, y))) => Some((f64::from(f32::from(bounds.origin.x) + x), f64::from(f32::from(bounds.origin.y) + y))),
                        _ => center(&target),
                    };
                    if let Some((x, y)) = point {
                        eprintln!("fixture: hover {arg}");
                        native_input::hover(width, x, y);
                    }
                    wait(600).await;
                }
                "rclick" => {
                    if let Some((x, y)) = center(&arg) {
                        eprintln!("fixture: right-click {arg}");
                        native_input::right_click(width, x, y);
                    }
                    wait(600).await;
                }
                "menu" => {
                    let n: usize = arg.parse().unwrap_or(1);
                    for _ in 0..n {
                        key("\u{f701}", 125, 0);
                        wait(120).await;
                    }
                    key("\r", 36, 0);
                    wait(600).await;
                    eprintln!("fixture: menu item {n} chosen: placement {:?}, file {:?}", placement(cx), settings().trim());
                }
                "tab" => {
                    let mut found = false;
                    for _ in 0..40 {
                        key("\t", 48, 0);
                        wait(110).await;
                        found = focused_place(cx, &arg);
                        if found {
                            break;
                        }
                    }
                    eprintln!("fixture: Tab reached {arg}: {found}");
                    wait(400).await;
                }
                // `key:<name>`: enter, space, up, down, left, right, escape, backspace, or a digit or letter.
                "key" => {
                    match arg.as_str() {
                        "enter" => key("\r", 36, 0),
                        "up" => key("\u{f700}", 126, 0),
                        "down" => key("\u{f701}", 125, 0),
                        "left" => key("\u{f702}", 123, 0),
                        "right" => key("\u{f703}", 124, 0),
                        "escape" => key("\u{1b}", 53, 0),
                        "backspace" => key("\u{7f}", 51, 0),
                        "space" => key(" ", 49, 0),
                        other => {
                            other.chars().next().map(type_char);
                            true
                        }
                    };
                    wait(450).await;
                    eprintln!("fixture: key {arg}: {}", panel(cx));
                }
                "type" => {
                    type_text(&arg);
                    wait(300).await;
                    eprintln!("fixture: typed {arg:?}: {}", panel(cx));
                }
                "panel" => eprintln!("fixture: panel {}", panel(cx)),
                "audio" => eprintln!("fixture: audio {:?}", cx.update(|cx| crate::media::now_playing(cx)).ok().flatten()),
                // `layout:<message id>`: the composer's height, the chat's scroll, that message's bubble and the panel's
                // box, to show what moves (nothing should, as the panel comes, changes and goes).
                "layout" => {
                    let probe = fixture.read_with(cx, |fixture, cx| fixture.conversation.read(cx).layout_probe(&arg)).ok();
                    let panel_box = crate::smoke::on_screen("question-panel").map(|bounds| (f32::from(bounds.origin.y), f32::from(bounds.size.height)));
                    let composer_top = crate::smoke::on_screen("composer-action").map(|bounds| f32::from(bounds.origin.y));
                    eprintln!("fixture: layout composer height, scroll, {arg} top: {probe:?}; composer button top {composer_top:?}; panel top and height {panel_box:?}");
                }
                "log" => {
                    eprintln!("fixture: placement {:?}, file {:?}", placement(cx), settings().trim());
                    eprintln!("fixture: rows {:?}", rows(cx));
                }
                "work" => {
                    let (text, keys) = fixture
                        .read_with(cx, |fixture, cx| {
                            let here = fixture.rows(cx);
                            let keys: Vec<String> = here.iter().map(|row| format!("{} ({}: {})", row.key, row.name, row.title)).collect();
                            let elsewhere: Vec<&str> = fixture.others.iter().map(|chat| chat.title.as_str()).collect();
                            eprintln!("fixture: working chats, none of them this chat's tasks, not shown here: {elsewhere:?}");
                            let replying = reply_status::reply_since(fixture.conversation.read(cx)).is_some();
                            eprintln!("fixture: this chat's own reply under way: {replying}");
                            if fixture.agent {
                                let list: Vec<String> = fixture.agent_rows(cx).into_iter().map(|(_, name, _, working)| format!("{name}: {}", if working.is_some() { "working" } else { "idle" })).collect();
                                eprintln!("fixture: agents' list {list:?}");
                            }
                            (working_panel::status_text(&here), keys)
                        })
                        .unwrap_or_default();
                    eprintln!("fixture: indicator {text:?}, cards {keys:?}");
                }
                "quiet" => {
                    fixture
                        .update(cx, |fixture, cx| {
                            fixture.conversation.update(cx, |conversation, cx| conversation.set_fixture_sessions(Vec::new(), cx));
                            cx.notify();
                        })
                        .ok();
                    wait(500).await;
                    eprintln!("fixture: this chat's work ended");
                }
                "replied" => {
                    fixture
                        .update(cx, |fixture, cx| {
                            fixture.conversation.update(cx, |conversation, cx| conversation.set_fixture_turns(Vec::new(), cx));
                            fixture.others.retain(|chat| chat.session_id != "fixture-chat");
                            cx.notify();
                        })
                        .ok();
                    wait(500).await;
                    eprintln!("fixture: this chat's reply is done");
                }
                // `ask`: Claude Code asks one more question, of a task of its own (the panel comes back).
                "ask" => {
                    fixture
                        .update(cx, |fixture, cx| {
                            let now = crate::format::now_ms();
                            let mut request = coding_requests::fixture_requests(now).into_iter().find(|request| request.request_id == "disk-3b").expect("fixture question");
                            request.request_id = "later-1".into();
                            request.task_id = "task-later".into();
                            request.asked_at = Some(now);
                            request.created_at = Some(now);
                            fixture.conversation.update(cx, |conversation, cx| conversation.add_fixture_request(request, cx));
                        })
                        .ok();
                    wait(60).await;
                    eprintln!("fixture: asked one more: {}", panel(cx));
                }
                "has" => eprintln!("fixture: {arg} on screen: {}", crate::smoke::on_screen(&arg).is_some()),
                "wheel" => {
                    let (target, dy) = arg.split_once('=').map(|(target, dy)| (target.to_string(), dy.parse().unwrap_or(300.))).unwrap_or((arg.clone(), 300.));
                    let position = |cx: &mut gpui::AsyncApp| fixture.read_with(cx, |fixture, cx| fixture.conversation.read(cx).scroll_position()).ok();
                    let before = position(cx);
                    if let Some((x, y)) = center(&target) {
                        native_input::scroll_in(width, x, y, dy);
                    }
                    wait(600).await;
                    let after = position(cx);
                    eprintln!("fixture: wheel {dy} over {target}: chat {before:?} -> {after:?} ({})", if before == after { "stayed" } else { "moved" });
                }
                "tip" => eprintln!("fixture: tip shown for {:?}", cx.update(|cx| working_panel::tip(cx)).ok().flatten()),
                // `title:<task id>=<text>` / `doing:<task id>=<text>`: a coding task's title, or what it's doing now.
                "title" | "doing" => {
                    let (task, text) = arg.split_once('=').unwrap_or((arg.as_str(), ""));
                    fixture
                        .update(cx, |fixture, cx| {
                            fixture.conversation.update(cx, |conversation, cx| {
                                let mut sessions = conversation.coding_sessions.clone();
                                for session in sessions.iter_mut().filter(|session| session.task_id == task) {
                                    if verb == "title" { session.title = text.to_string() } else { session.now = text.to_string() }
                                }
                                conversation.set_fixture_sessions(sessions, cx);
                            });
                            cx.notify();
                        })
                        .ok();
                    wait(400).await;
                    eprintln!("fixture: {task}'s {verb} is now {text:?}");
                }
                "finish" => {
                    fixture
                        .update(cx, |fixture, cx| {
                            fixture.others.retain(|chat| chat.session_id != arg);
                            cx.notify();
                        })
                        .ok();
                    wait(500).await;
                    eprintln!("fixture: {arg}'s work ended");
                }
                "draft" => {
                    let composer = fixture.read_with(cx, |fixture, cx| fixture.conversation.read(cx).composer()).ok();
                    if let Some(composer) = &composer {
                        cx.update_window(window_handle, |_, window, cx| {
                            composer.update(cx, |input, cx| input.set_value("Draft: check the backups before deleting more", window, cx));
                        })
                        .ok();
                    }
                    wait(300).await;
                    click("working-row-coding-task-sidebar");
                    wait(600).await;
                    shot(&format!("{}-row-opened-page", if fixture.read_with(cx, |f, _| f.agent).unwrap_or(false) { "agent" } else { "chat" }));
                    click("fixture-back");
                    wait(600).await;
                    let draft = composer.and_then(|composer| composer.read_with(cx, |input, _| input.value().to_string()).ok()).unwrap_or_default();
                    eprintln!("fixture: back in the chat, draft kept: {draft:?}");
                }
                other => eprintln!("fixture: unknown step {other}"),
            }
        }
        eprintln!("fixture: rows at the end {:?}", rows(cx));
        eprintln!("fixture: ended with placement {:?}, file {:?}", placement(cx), settings().trim());
        wait(500).await;
        cx.update(|cx| cx.quit()).ok();
    })
    .detach();
}

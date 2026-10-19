// `FILO_OPEN=shared-show-drive`: the Shared list's Show in Chat on a copy of the user's data, for files far up a long
// chat and the group's.

use std::time::{Duration, Instant};

use gpui::{Context, Window};
use serde_json::{Value, json};

use super::{PAGE_SCROLLS, id_of, native_input, on_screen, shot, window_width};
use crate::app::AppShell;
use crate::backend::backend;
use crate::nav::Tab;

/// Every item a conversation's Shared list holds, newest first: its kind and message.
async fn shared_items(backend: &crate::backend::Backend, session: &str) -> Vec<(String, String)> {
    let (mut items, mut cursor) = (Vec::new(), Value::Null);
    loop {
        let page = backend.call_raw("chatShared:list", json!({ "sessionId": session, "cursor": cursor })).await;
        let Ok(page) = page else { break };
        items.extend(
            page["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| Some((item["kind"].as_str()?.to_string(), item["messageId"].as_str()?.to_string()))),
        );
        if page["done"] != false {
            break;
        }
        cursor = page["cursor"].clone();
    }
    items
}

/// With `FILO_SHOW_CHATS=<session id>,…,group`, opens each chat's profile (the group's settings), and from its Shared
/// list shows the oldest file, one in the middle and the newest in its chat (the menu's Show in Chat, `drive_show_in_chat`),
/// checking the chat scrolled to its message and flashed it; on the first, the newest tile's menu is used, by a right
/// click and the keyboard. Logs each one's depth and how long it took; pictures with `FILO_DRIVE_SHOTS=<folder>`. Run it
/// on a copy of the user's data in a `FILO_CONFIG_DIR` of its own.
pub(super) fn run(window: &mut Window, cx: &mut Context<AppShell>) {
    let chats: Vec<String> = std::env::var("FILO_SHOW_CHATS")
        .unwrap_or_default()
        .split(',')
        .map(|chat| chat.trim().to_string())
        .filter(|chat| !chat.is_empty())
        .collect();
    let backend = backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let quit = |cx: &mut gpui::AsyncWindowContext| {
            if std::env::var_os("FILO_DRIVE_STAY").is_none() {
                cx.update(|_, cx| cx.quit()).ok();
            }
        };
        if !crate::test_run::own_config() || chats.is_empty() {
            eprintln!("shared-show-drive: FAIL needs FILO_SHOW_CHATS and a FILO_CONFIG_DIR of its own");
            return quit(cx);
        }
        wait(4000).await;
        let agents = backend.call_raw("agents:list", json!({})).await.unwrap_or_default();
        let group = backend.call_raw("agents:openGroup", json!({})).await.ok();
        let mut failed = 0;
        for (index, chat) in chats.iter().enumerate() {
            // The agent whose chat it is (None: the group), and its session.
            let (agent, session) = if chat == "group" {
                let Some(session) = group.as_ref().and_then(Value::as_str) else {
                    eprintln!("shared-show-drive: FAIL no group");
                    failed += 1;
                    continue;
                };
                (None, session.to_string())
            } else {
                let agent = agents
                    .as_array()
                    .and_then(|rows| rows.iter().find(|row| row["sessionId"].as_str() == Some(chat.as_str())))
                    .and_then(id_of);
                let Some(agent) = agent else {
                    eprintln!("shared-show-drive: FAIL {chat} isn't an agent's chat");
                    failed += 1;
                    continue;
                };
                (Some(agent), chat.clone())
            };
            let files: Vec<String> = shared_items(&backend, &session)
                .await
                .into_iter()
                .filter(|(kind, _)| kind != "link")
                .map(|(_, message)| message)
                .collect();
            let Some(newest) = files.first().cloned() else {
                eprintln!("shared-show-drive: {chat} shared no files");
                continue;
            };
            let mut picked = vec![
                ("newest", newest),
                ("middle", files[files.len() / 2].clone()),
                ("oldest", files[files.len() - 1].clone()),
            ];
            picked.dedup_by(|a, b| a.1 == b.1);
            let page = if agent.is_some() { "agent-profile" } else { "group-settings" };
            for (n, (which, message)) in picked.into_iter().enumerate() {
                // Its profile (Group settings), scrolled to the Shared list.
                this.update_in(cx, |shell, window, cx| {
                    shell.open_tab(Tab::Agents, window, cx);
                    if let Some(agents) = shell.agents_screen() {
                        agents.update(cx, |agents, cx| match &agent {
                            Some(agent) => agents.edit_agent(agent.clone(), window, cx),
                            None => agents.edit_group(window, cx),
                        });
                    }
                })
                .ok();
                wait(2500).await;
                if let Some(handle) = PAGE_SCROLLS.with(|scrolls| scrolls.borrow().get(page).cloned()) {
                    handle.set_offset(gpui::point(gpui::px(0.), -handle.max_offset().height));
                }
                this.update_in(cx, |_, window, _| window.refresh()).ok();
                wait(1500).await;
                let depth = backend
                    .call_raw("chatShared:depth", json!({ "sessionId": session, "messageId": message }))
                    .await
                    .ok()
                    .and_then(|depth| depth.as_u64());
                let started = Instant::now();
                // The first chat's newest file by its tile's menu (its first item), the rest from the panel.
                let by_menu = index == 0 && n == 0;
                if by_menu {
                    // The first row of tiles clear of the page's header, as a person would scroll it.
                    let row = on_screen("shared-media-row-0").or_else(|| on_screen("shared-voice-0"));
                    let handle = PAGE_SCROLLS.with(|scrolls| scrolls.borrow().get(page).cloned());
                    if let (Some(row), Some(handle)) = (row, handle) {
                        let offset = handle.offset();
                        handle.set_offset(gpui::point(offset.x, offset.y + gpui::px(200.) - row.origin.y));
                        this.update_in(cx, |_, window, _| window.refresh()).ok();
                        wait(1000).await;
                    }
                    shot(&format!("{index}-shared"));
                    let tile = on_screen("shared-media-row-0").or_else(|| on_screen("shared-voice-0"));
                    let (Some(tile), Some(width)) = (tile, window_width(&this, cx)) else {
                        eprintln!("shared-show-drive: FAIL {chat}'s newest file isn't on screen");
                        failed += 1;
                        continue;
                    };
                    let x = f32::from(tile.origin.x) + 30.;
                    native_input::right_click(width, f64::from(x), f64::from(f32::from(tile.center().y)));
                    wait(800).await;
                    shot(&format!("{index}-menu"));
                    native_input::key(width, "\u{f701}", 125, 0);
                    wait(200).await;
                    native_input::key(width, "\r", 36, 0);
                } else {
                    this.update(cx, |shell, cx| {
                        let shared = shell.agents_screen().and_then(|agents| agents.read(cx).drive_shared());
                        if let Some(shared) = shared {
                            shared.update(cx, |shared, cx| shared.drive_show_in_chat(message.clone(), cx));
                        }
                    })
                    .ok();
                }
                // Until its chat flashes it.
                let flashed = loop {
                    let highlighted = this
                        .update(cx, |shell, cx| {
                            let agents = shell.agents_screen()?;
                            let conversation = agents.read(cx).current_conversation()?;
                            conversation.read(cx).drive_highlighted()
                        })
                        .ok()
                        .flatten();
                    if highlighted.as_deref() == Some(message.as_str()) {
                        break true;
                    }
                    if started.elapsed() > Duration::from_secs(15) {
                        break false;
                    }
                    wait(50).await;
                };
                let took = started.elapsed();
                wait(500).await;
                let top = this
                    .update(cx, |shell, cx| {
                        let conversation = shell.agents_screen()?.read(cx).current_conversation()?;
                        conversation.read(cx).top_message()
                    })
                    .ok()
                    .flatten();
                let at_top = top.as_ref().is_some_and(|(id, _)| *id == message);
                // The newest is at the end of the chat, where the chat opens.
                let ok = flashed && (at_top || n == 0);
                failed += usize::from(!ok);
                shot(&format!("{index}-{n}-shown"));
                eprintln!(
                    "shared-show-drive: {chat} {} file (depth {}{}): {} in {} ms, {} {}",
                    which,
                    depth.map_or("?".into(), |depth| depth.to_string()),
                    if by_menu { ", from its menu" } else { "" },
                    if flashed { "flashed" } else { "not flashed" },
                    took.as_millis(),
                    if at_top { "at the top of the view" } else { "not at the top of the view" },
                    if ok { "PASS" } else { "FAIL" }
                );
            }
        }
        eprintln!("shared-show-drive: done, {failed} failed");
        quit(cx);
    })
    .detach();
}

// `FILO_OPEN=shared-delete-drive`: Filo's copies of a chat's files deleted from its Shared list, like a person would.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, Window};
use serde_json::{Value, json};

use super::{PAGE_SCROLLS, native_input, on_screen, shot, window_width};
use crate::app::AppShell;
use crate::backend::backend;
use crate::local_backend::DEFAULT_AGENT_ID;
use crate::nav::Tab;

/// A picture of one colour, as an agent's design would be, written at `path`.
fn picture(path: &Path, rgb: [u8; 3]) {
    let image = image::RgbImage::from_pixel(360, 240, image::Rgb(rgb));
    if let Err(error) = image.save_with_format(path, image::ImageFormat::Png) {
        eprintln!("shared-delete-drive: couldn't write {}: {error}", path.display());
    }
}

/// Fills the main agent's chat as an agent iterating on a design would (`attachments::sent`): v1 of a logo, the user's
/// sketch, then v2 drawn over the same file with its brief (shown where it is), and a picture posted in the group.
/// The copies of v1 and the sketch, the files the drive deletes.
fn seed(local: &Path, chat: &str) -> Result<(PathBuf, PathBuf), String> {
    use filo_local::{Message, Role};
    let store = filo_local::Store::at(local.join("agents"));
    let work = local.join("agents/workspace/designs");
    std::fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    let (logo, brief) = (work.join("logo.png"), work.join("brief.md"));
    let sent = |path: &Path| filo_local::attachments::sent(local, path).map_err(|error| error.to_string());
    let say = |text: &str, role: Role, files: Vec<filo_local::store::Attachment>| {
        let mut message = Message::new(role, text, None);
        message.attachments = files;
        store.append(chat, &message).map_err(|error| error.to_string())
    };
    picture(&logo, [38, 166, 154]);
    let v1 = sent(&logo)?;
    say("Here's v1 of the logo.", Role::Assistant, vec![v1.clone()])?;
    let sketch_id = filo_local::attachments::save(local, "image/png", &{
        let path = work.join("sketch.png");
        picture(&path, [92, 107, 192]);
        std::fs::read(&path).map_err(|error| error.to_string())?
    })
    .map_err(|error| error.to_string())?;
    let sketch = filo_local::store::Attachment {
        file: filo_local::attachments::file(local, &sketch_id).ok_or("the sketch wasn't saved")?,
        storage_id: sketch_id,
        name: "sketch.png".into(),
        content_type: "image/png".into(),
        size: 0,
        local_path: None,
        bookmark: None,
        inode: None,
    };
    say("Warmer, more like this.", Role::User, vec![sketch.clone()])?;
    // A voice message, its recording saved as the app saves one.
    let storage_id = filo_local::attachments::reserve("audio/mp4");
    filo_local::attachments::save_as(local, &storage_id, &[0; 2048]).map_err(|error| error.to_string())?;
    let mut voice = Message::new(Role::User, "And make the corners rounder.", None);
    voice.voice = Some(filo_local::store::Voice { storage_id, duration_ms: 3400 });
    store.append(chat, &voice).map_err(|error| error.to_string())?;
    picture(&logo, [239, 108, 0]);
    std::fs::write(&brief, "# Logo brief\n\nWarm, round, friendly.\n").map_err(|error| error.to_string())?;
    say("And v2, warmer, with the brief.", Role::Assistant, vec![sent(&logo)?, sent(&brief)?])?;
    // A post in the group, for its Shared list.
    let db = filo_local::db::Db::open(&local.join(filo_local::db::FILE)).map_err(|error| error.to_string())?;
    let plate = work.join("menu-card.png");
    picture(&plate, [216, 27, 96]);
    filo_local::group::post_files(
        &db,
        DEFAULT_AGENT_ID,
        "The menu card.",
        vec![sent(&plate)?],
        filo_local::group::Asks::default(),
        None,
        crate::format::now_ms() as i64,
    )?;
    Ok((v1.file, sketch.file))
}

/// Seeds the main agent's chat (`seed`), opens its profile and scrolls to Shared. The menu's Select, then plain clicks
/// pick and let go until none is picked, after which one opens a file; Esc and Clear end picking too. ⌘-click picks v1
/// and the sketch, the Delete key and the dialog's Return, then checks only those copies went (the logo and brief where the agent has them
/// stay); then the same on the Voice tab for the voice message. It shows the chat (v1, the sketch and the voice message
/// no longer kept, v2 and the brief there) and Group settings' Shared.
/// Pictures with `FILO_DRIVE_SHOTS=<folder>`. Run it with a `FILO_CONFIG_DIR` of its own.
pub(super) fn run(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let quit = |cx: &mut gpui::AsyncWindowContext| {
            if std::env::var_os("FILO_DRIVE_STAY").is_none() {
                cx.update(|_, cx| cx.quit()).ok();
            }
        };
        wait(4000).await;
        let local = crate::config::config_dir().join("local");
        let chat = backend.call_raw("agents:openChat", json!({ "id": DEFAULT_AGENT_ID })).await;
        let _ = backend.call_raw("agents:openGroup", json!({})).await;
        let Some(chat) = chat.ok().and_then(|chat| chat.as_str().map(str::to_string)) else {
            eprintln!("shared-delete-drive: FAIL no chat for the main agent");
            return quit(cx);
        };
        let (v1, sketch) = match seed(&local, &chat) {
            Ok(files) => files,
            Err(error) => {
                eprintln!("shared-delete-drive: FAIL couldn't fill the chat: {error}");
                return quit(cx);
            }
        };
        backend.poke();
        this.update_in(cx, |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_agent(DEFAULT_AGENT_ID.to_string(), window, cx));
            }
        })
        .ok();
        wait(3000).await;
        let to_bottom = |page: &str| {
            if let Some(handle) = PAGE_SCROLLS.with(|scrolls| scrolls.borrow().get(page).cloned()) {
                handle.set_offset(gpui::point(gpui::px(0.), -handle.max_offset().height));
            }
        };
        to_bottom("agent-profile");
        this.update_in(cx, |_, window, _| window.refresh()).ok();
        wait(1000).await;
        shot("1-shared");
        // Newest first: v2, the sketch, v1, three to a row.
        if on_screen("shared-media-row-0").is_none() {
            eprintln!("shared-delete-drive: FAIL the media grid isn't on screen");
            return quit(cx);
        }
        let Some(width) = window_width(&this, cx) else { return quit(cx) };
        // Where the tile is now: the bar over the list moves it down while files are picked.
        let tile = |column: f32| {
            let row = on_screen("shared-media-row-0").unwrap_or_default();
            let x = f32::from(row.origin.x) + f32::from(row.size.width) * (column + 0.5) / 3.;
            (f64::from(x), f64::from(f32::from(row.center().y)))
        };
        const COMMAND: u64 = native_input::COMMAND;
        const ESCAPE: (&str, u16) = ("\u{1b}", 53);
        let picked_bar = || on_screen("shared-delete").is_some();
        // v1's menu, for it alone.
        let (x, y) = tile(2.);
        native_input::right_click(width, x, y);
        wait(800).await;
        shot("2-menu");
        native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
        wait(600).await;
        // The menu's Select: plain clicks pick and let go, until the last one goes, Esc or Clear; then one opens a file.
        // How many the bar says are picked, in the latest frame: drawn with the tabs just over it (`on_screen` would
        // still see a bar gone in the frame before).
        let picks = || {
            let bounds = super::DRIVE_BOUNDS.lock().unwrap();
            let Some((_, tabs)) = bounds.get("shared-tabs") else { return 0 };
            (1..4)
                .find(|count| {
                    bounds
                        .get(&format!("shared-picks-{count}"))
                        .is_some_and(|(_, at)| at.max(tabs).duration_since(*at.min(tabs)) < Duration::from_millis(3))
                })
                .unwrap_or(0)
        };
        let viewer = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |_, window, cx| crate::media_viewer::smoke_state(window, cx) != "closed")
                .unwrap_or(false)
        };
        let select = async |column: f32| {
            let (x, y) = tile(column);
            native_input::right_click(width, x, y);
            wait(800).await;
            // Show in Chat, Show in Finder, Edit, Select.
            for _ in 0..4 {
                native_input::key(width, "\u{f701}", 125, 0);
                wait(150).await;
            }
            native_input::key(width, "\r", 36, 0);
            wait(600).await;
        };
        let plain = async |column: f32| {
            let (x, y) = tile(column);
            native_input::click(width, x, y);
            wait(500).await;
        };
        select(0.).await;
        let mut counts = vec![picks()];
        for column in [1., 2.] {
            plain(column).await;
            counts.push(picks());
        }
        shot("2a-select-mode");
        let opened_while_picking = viewer(&this, cx);
        for column in [1., 0., 2.] {
            plain(column).await;
            counts.push(picks());
        }
        plain(1.).await;
        let opens_after_last = viewer(&this, cx);
        shot("2a-opens-again");
        native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
        wait(800).await;
        let ok = counts == [1, 2, 3, 2, 1, 0] && !opened_while_picking && opens_after_last;
        eprintln!(
            "shared-delete-drive: Select, then plain clicks picked {counts:?}, {} while picking, {} once none was {}",
            if opened_while_picking { "opened a file" } else { "opened nothing" },
            if opens_after_last { "opened the file" } else { "didn't open the file" },
            if ok { "PASS" } else { "FAIL" }
        );
        // Esc and Clear end it too.
        for how in ["Esc", "Clear"] {
            select(2.).await;
            plain(1.).await;
            let picked = picks();
            if how == "Esc" {
                native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
            } else if let Some(clear) = on_screen("shared-clear") {
                native_input::click(
                    width,
                    f64::from(f32::from(clear.center().x)),
                    f64::from(f32::from(clear.center().y)),
                );
            }
            wait(600).await;
            let after = picks();
            plain(0.).await;
            let opens = viewer(&this, cx);
            native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
            wait(800).await;
            let ok = picked == 2 && after == 0 && opens;
            eprintln!(
                "shared-delete-drive: {how} ends picking ({picked} picked, then {after}), a plain click then {} {}",
                if opens { "opens a file" } else { "doesn't open the file" },
                if ok { "PASS" } else { "FAIL" }
            );
        }
        // ⌘-click v2, Shift-click v1: the run of all three is picked; Esc lets them go.
        let (x0, y0) = tile(0.);
        native_input::click_with(width, x0, y0, COMMAND);
        wait(400).await;
        let (x2, y2) = tile(2.);
        native_input::click_with(width, x2, y2, native_input::SHIFT);
        wait(600).await;
        shot("2b-shift-run");
        native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
        wait(600).await;
        // ⌘-click the sketch and v1: both picked; v1's menu now deletes both.
        for column in [1., 2.] {
            let (x, y) = tile(column);
            native_input::click_with(width, x, y, COMMAND);
            wait(400).await;
        }
        let (x, y) = tile(2.);
        native_input::right_click(width, x, y);
        wait(800).await;
        shot("3-picked-menu");
        native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
        wait(600).await;
        shot("4-picked");
        let both_picked = picked_bar();
        // The Delete key, then the dialog's Return.
        native_input::key(width, "\u{7f}", 51, 0);
        wait(1000).await;
        shot("5-confirm");
        native_input::key(width, "\r", 36, 0);
        wait(2000).await;
        shot("5a-deleted");
        let designs = local.join("agents/workspace/designs");
        let kept = [designs.join("logo.png"), designs.join("brief.md")].iter().all(|path| path.is_file());
        let ok = !v1.exists() && !sketch.exists() && kept;
        eprintln!(
            "shared-delete-drive: v1's copy {}, the sketch's {}, the agent's own logo and brief {} {}",
            if v1.exists() { "kept" } else { "deleted" },
            if sketch.exists() { "kept" } else { "deleted" },
            if kept { "still there" } else { "gone" },
            if ok { "PASS" } else { "FAIL" }
        );
        eprintln!(
            "shared-delete-drive: ⌘-click picked two, the bar showed {} {}",
            if both_picked { "with Delete" } else { "nothing" },
            if both_picked { "PASS" } else { "FAIL" }
        );
        // ⌘-click v2 (the agent's latest), then Esc lets it go.
        let (x, y) = tile(0.);
        native_input::click_with(width, x, y, COMMAND);
        wait(600).await;
        let picked_then = picked_bar();
        native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
        wait(600).await;
        let cleared = picked_then && !picked_bar();
        eprintln!("shared-delete-drive: Esc lets go of the picked {}", if cleared { "PASS" } else { "FAIL" });
        // The voice message, from the Voice tab: ⌘-click, Delete key, Return.
        if let Some(tabs) = on_screen("shared-tabs") {
            let x = f32::from(tabs.origin.x) + f32::from(tabs.size.width) * 7. / 8.;
            native_input::click(width, f64::from(x), f64::from(f32::from(tabs.center().y)));
        }
        wait(800).await;
        if let Some(voice) = on_screen("shared-voice-0") {
            let center = voice.center();
            native_input::right_click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
            wait(800).await;
            shot("5b-voice-menu");
            native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
            wait(600).await;
            native_input::click_with(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)), COMMAND);
        }
        wait(600).await;
        native_input::key(width, "\u{7f}", 51, 0);
        wait(1000).await;
        native_input::key(width, "\r", 36, 0);
        wait(2000).await;
        shot("5c-voice-deleted");
        let detail = backend.call_raw("voiceSessions:detail", json!({ "id": chat })).await.unwrap_or_default();
        let voice_gone = detail["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message.get("voiceRecording"))
            .any(|voice| voice["gone"] == true);
        eprintln!(
            "shared-delete-drive: the voice message {} {}",
            if voice_gone { "shows as no longer kept" } else { "still has its recording" },
            if voice_gone { "PASS" } else { "FAIL" }
        );
        let labels: Vec<String> = detail["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|message| message["attachments"].as_array().cloned().unwrap_or_default())
            .map(|file: Value| {
                let state = match (file["gone"] == true, file["copy"] == true) {
                    (true, true) => "no longer kept",
                    (true, false) => "no longer there",
                    (false, true) => "Filo's copy",
                    (false, false) => "where it is",
                };
                format!("{} ({state})", file["name"].as_str().unwrap_or("?"))
            })
            .collect();
        eprintln!("shared-delete-drive: the chat's files: {}", labels.join(", "));
        // v2's menu, from the keyboard: its first item, Show in Chat, opens the chat at its message.
        if let Some(tabs) = on_screen("shared-tabs") {
            let x = f32::from(tabs.origin.x) + f32::from(tabs.size.width) / 8.;
            native_input::click(width, f64::from(x), f64::from(f32::from(tabs.center().y)));
        }
        wait(800).await;
        // Opens v2's menu and runs its `item`th item from the keyboard (↓ that many times, Return).
        let pick_from_menu = async |item: usize| {
            let (x, y) = tile(0.);
            native_input::right_click(width, x, y);
            wait(800).await;
            for _ in 0..item {
                native_input::key(width, "\u{f701}", 125, 0);
                wait(200).await;
            }
            native_input::key(width, "\r", 36, 0);
            wait(3000).await;
        };
        pick_from_menu(1).await;
        shot("6-chat");
        let in_chat = this
            .update(cx, |shell, cx| {
                shell.agents_screen().is_some_and(|agents| agents.read(cx).current_conversation().is_some())
            })
            .unwrap_or(false);
        eprintln!("shared-delete-drive: Show in Chat opens the chat {}", if in_chat { "PASS" } else { "FAIL" });
        // Edit (its third item): the chat's image editor, with v2.
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_agent(DEFAULT_AGENT_ID.to_string(), window, cx));
            }
        })
        .ok();
        wait(2000).await;
        to_bottom("agent-profile");
        this.update_in(cx, |_, window, _| window.refresh()).ok();
        wait(1000).await;
        pick_from_menu(3).await;
        shot("6b-edit");
        native_input::key(width, ESCAPE.0, ESCAPE.1, 0);
        wait(1000).await;
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_group(window, cx));
            }
        })
        .ok();
        wait(3000).await;
        to_bottom("group-settings");
        this.update_in(cx, |_, window, _| window.refresh()).ok();
        wait(1000).await;
        shot("7-group-shared");
        eprintln!("shared-delete-drive: done");
        quit(cx);
    })
    .detach();
}

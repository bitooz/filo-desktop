// `FILO_OPEN=shared-delete-drive`: Filo's copies of a chat's files deleted from its Shared list, like a person would.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, Window};
use serde_json::{Value, json};

use super::{PAGE_SCROLLS, click, native_input, on_screen, shot, window_width};
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

/// Seeds the main agent's chat (`seed`), opens its profile, scrolls to Shared, clicks Select, picks v1 and the sketch,
/// Delete and the dialog's Return, then checks only those copies went (the logo and brief where the agent has them
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
        click(&this, cx, "shared-select");
        wait(800).await;
        shot("2-select");
        // Newest first: v2, the sketch, v1, three to a row.
        let Some(row) = on_screen("shared-media-row-0") else {
            eprintln!("shared-delete-drive: FAIL the media grid isn't on screen");
            return quit(cx);
        };
        let Some(width) = window_width(&this, cx) else { return quit(cx) };
        let tile = |column: f32| {
            let x = f32::from(row.origin.x) + f32::from(row.size.width) * (column + 0.5) / 3.;
            (f64::from(x), f64::from(f32::from(row.center().y)))
        };
        for column in [1., 2.] {
            let (x, y) = tile(column);
            native_input::click(width, x, y);
            wait(400).await;
        }
        // v2, the agent's newest, is still its file's copy: picked, then let go.
        let (x, y) = tile(0.);
        native_input::click(width, x, y);
        wait(400).await;
        native_input::click(width, x, y);
        wait(600).await;
        shot("3-picked");
        click(&this, cx, "shared-delete");
        wait(1000).await;
        shot("4-confirm");
        native_input::key(width, "\r", 36, 0);
        wait(2000).await;
        shot("5-deleted");
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
        // The voice message, from the Voice tab.
        if let Some(tabs) = on_screen("shared-tabs") {
            let x = f32::from(tabs.origin.x) + f32::from(tabs.size.width) * 7. / 8.;
            native_input::click(width, f64::from(x), f64::from(f32::from(tabs.center().y)));
        }
        wait(800).await;
        click(&this, cx, "shared-select");
        wait(600).await;
        click(&this, cx, "shared-voice-0");
        wait(600).await;
        shot("5b-voice-picked");
        click(&this, cx, "shared-delete");
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
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.open_agent(DEFAULT_AGENT_ID.to_string(), window, cx));
            }
        })
        .ok();
        wait(3000).await;
        shot("6-chat");
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

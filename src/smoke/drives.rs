// `FILO_SMOKE=1 filo`: opens every tab and pushed page in turn with real ids
// from this Mac's data, logging each step, then quits. A panic in any
// screen shows up in the log. Pages that would write data on open are skipped
// unless the data makes them read-only (a feed post that already has details).

use std::time::Duration;

use gpui::{Context, Task, Window};
use serde_json::{Value, json};

use crate::app::AppShell;
use crate::backend::backend;
use crate::nav::{Route, Tab};
use crate::screens::note::NoteScreen;

use super::{DRIVE_BOUNDS, NOTE_SHARE_ITEMS, PAGE_SCROLLS};

// `FILO_OPEN=list-reorder-timing` (smoke/drives/list_reorder.rs).
mod list_reorder;
// `FILO_OPEN=shared-delete-drive` (smoke/drives/shared_delete.rs).
mod shared_delete;
// `FILO_OPEN=shared-show-drive` (smoke/drives/shared_show.rs).
mod shared_show;

async fn first_id(
    future: impl std::future::Future<Output = Result<Value, String>>,
    pick: impl Fn(&Value) -> Option<String>,
) -> Option<String> {
    future.await.ok().and_then(|value| pick(&value))
}

fn id_of(value: &Value) -> Option<String> {
    value.get("_id").and_then(Value::as_str).map(str::to_string)
}

/// `FILO_SMOKE=artifacts`: timed first/repeat opens in an isolated FILO_CONFIG_DIR.
/// Readiness is the iframe load followed by two animation frames, not just the database reply.
/// The web view draws nothing until its page is ready (no white or stale page shows first), then draws it.
async fn wait_ready(page: &gpui::Entity<crate::screens::artifact::ArtifactScreen>, cx: &mut gpui::AsyncWindowContext) {
    let waiting = std::time::Instant::now();
    while !page.update(cx, |page, _| page.preview_ready()).unwrap() {
        assert!(
            waiting.elapsed() < Duration::from_secs(10),
            "preview timed out: {}",
            page.update(cx, |page, _| page.smoke_state()).unwrap()
        );
        let (ready, drawn) = page.update(cx, |page, cx| page.smoke_drawn(cx)).unwrap();
        assert!(ready || !drawn, "a page still loading is drawn");
        cx.background_executor().timer(Duration::from_millis(2)).await;
    }
    assert!(page.update(cx, |page, cx| page.smoke_drawn(cx)).unwrap().1, "a ready page isn't drawn");
}

pub fn run_artifacts(window: &mut Window, cx: &mut Context<AppShell>) -> Task<()> {
    let backend = backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let list = backend.call_raw("canvasArtifacts:list", json!({})).await.unwrap();
        let id = std::env::var("FILO_ARTIFACT_ID").ok().or_else(|| id_of(&list[0])).expect("seed an artifact in the isolated test config");
        let others: Vec<String> = list.as_array().unwrap().iter().filter_map(id_of).filter(|other| *other != id).collect();
        cx.background_executor().timer(Duration::from_millis(500)).await;
        // A screen recorder can ask for time to start first.
        if let Ok(file) = std::env::var("FILO_ARTIFACT_WAIT_FILE") {
            let waiting = std::time::Instant::now();
            while !std::path::Path::new(&file).exists() && waiting.elapsed() < Duration::from_secs(10) {
                cx.background_executor().timer(Duration::from_millis(20)).await;
            }
        }
        let prepare_ms = std::env::var("FILO_ARTIFACT_PREPARE_MS").ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(0);
        if prepare_ms > 0 {
            let before = backend.call_raw("canvasArtifacts:get", json!({"id":id})).await.unwrap();
            let prepared = std::time::Instant::now();
            this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Artifacts, window, cx)).unwrap();
            cx.background_executor().timer(Duration::from_millis(prepare_ms)).await;
            let after = backend.call_raw("canvasArtifacts:get", json!({"id":id})).await.unwrap();
            assert_eq!(before["openedAt"], after["openedAt"], "preparation must not mark an artifact opened");
            eprintln!("artifact: list preparation window {:.3} ms; openedAt unchanged", prepared.elapsed().as_secs_f64()*1000.);
        }
        for pass in 0..6 {
            this.update_in(cx, |shell, window, cx| shell.select_tab(Tab::Artifacts, window, cx)).unwrap();
            let started = std::time::Instant::now();
            this.update_in(cx, |shell, window, cx| shell.push(Route::Artifact(id.clone()), window, cx)).unwrap();
            let page = this.update(cx, |shell, _| shell.top_page().unwrap().downcast::<crate::screens::artifact::ArtifactScreen>().unwrap()).unwrap();
            wait_ready(&page, cx).await;
            eprintln!("artifact: open {pass} {:.3} ms", started.elapsed().as_secs_f64() * 1000.);
            let drawn = cx.update(|window, _| {
                let (tx, rx) = futures::channel::oneshot::channel();
                window.on_next_frame(move |window, _| {
                    window.on_next_frame(move |_, _| { let _ = tx.send(()); });
                    window.refresh();
                });
                window.refresh();
                rx
            }).unwrap();
            drawn.await.unwrap();
            eprintln!("artifact: open {pass} through two display frames {:.3} ms", started.elapsed().as_secs_f64() * 1000.);
            if pass == 5 {
                let eval = |script: &str, cx: &mut gpui::AsyncWindowContext| page.update(cx, |page, cx| page.smoke_eval(script, cx)).unwrap();
                // The fixture's test script reports its actual iframe scroll and a load token.
                eval("window.probe=null;addEventListener('message',e=>{if(e.source===document.getElementById('a').contentWindow&&e.data?.artifactProbe)window.probe=e.data.artifactProbe});document.getElementById('a').contentWindow.postMessage({artifactProbe:'scroll',y:420},'*');true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let before: Value = serde_json::from_str(&eval("window.probe", cx).await.unwrap()).unwrap();
                eprintln!("artifact: before menu {before}");
                assert_eq!(before["y"], 420, "fixture must scroll: {before}");
                for action in ["open", "close", "source", "preview", "open", "escape", "open", "outside"] {
                    match action {
                        "open" => page.update(cx, |page, cx| page.smoke_menu(true, cx)).unwrap(),
                        "close" => page.update(cx, |page, cx| page.smoke_menu(false, cx)).unwrap(),
                        "source" => page.update(cx, |page, cx| page.smoke_source(true, cx)).unwrap(),
                        "preview" => page.update(cx, |page, cx| page.smoke_source(false, cx)).unwrap(),
                        "escape" => { eval("document.querySelector('#artifact-host-menu button').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));true", cx).await.unwrap(); },
                        "outside" => { eval("document.getElementById('artifact-menu-layer').dispatchEvent(new PointerEvent('pointerdown',{bubbles:true}));true", cx).await.unwrap(); },
                        _ => unreachable!(),
                    }
                    cx.background_executor().timer(Duration::from_millis(80)).await;
                    eval("document.getElementById('a').contentWindow.postMessage({artifactProbe:'read'},'*');true", cx).await.unwrap();
                    cx.background_executor().timer(Duration::from_millis(30)).await;
                    let state: Value = serde_json::from_str(&eval("window.probe", cx).await.unwrap()).unwrap();
                    assert_eq!(state, before, "scroll or iframe changed after {action}");
                    let menu = eval("!document.getElementById('artifact-menu-layer').hidden", cx).await.unwrap();
                    assert_eq!(menu, if action == "open" { "true" } else { "false" });
                    assert_eq!(page.update(cx, |page, cx| page.smoke_visible(cx)).unwrap(), action != "source", "native visibility after {action}");
                    eprintln!("artifact: {action} preserves iframe and scroll; menu visible={menu}");
                    if action == "open" {
                        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap();
                        if let Some(number) = native_input::window_number(1100.) {
                            crate::window_shot::save(number, format!("{shots}/artifact-menu-preserves-preview.png"));
                        }
                    }
                }
                // The menu choice that temporarily uses GPUI instead of the native preview.
                page.update(cx, |page, cx| page.smoke_menu(true, cx)).unwrap();
                eval("document.querySelector('[data-mode=source]').click();true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(80)).await;
                assert!(!page.update(cx, |page, cx| page.smoke_visible(cx)).unwrap());
                page.update(cx, |page, cx| page.smoke_source(false, cx)).unwrap();
                cx.background_executor().timer(Duration::from_millis(80)).await;
                eval("document.getElementById('a').contentWindow.postMessage({artifactProbe:'read'},'*');true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(30)).await;
                assert_eq!(serde_json::from_str::<Value>(&eval("window.probe", cx).await.unwrap()).unwrap(), before);
                eprintln!("artifact: host source choice preserves iframe and scroll");
                // The page's menu lists the versions, newest first, the shown one checked and the current one marked;
                // picking one switches the artifact in place, and Show current version brings the current page back.
                page.update(cx, |page, cx| page.smoke_menu(true, cx)).unwrap();
                cx.background_executor().timer(Duration::from_millis(30)).await;
                let rows = eval("[...document.querySelectorAll('#artifact-host-menu [data-version]')].map(b=>b.dataset.version+(b.getAttribute('aria-checked')==='true'?'*':'')+(b.querySelector('.chip')?'!':'')).join(',')", cx).await.unwrap();
                eprintln!("artifact: version rows {rows}");
                assert!(rows.contains("*!,") || rows.ends_with("*!\""), "the current version is first, shown and marked: {rows}");
                assert!(rows.contains(",1\""), "version 1 is last: {rows}");
                let when = eval("document.querySelector('#artifact-host-menu [data-version] .when').textContent", cx).await.unwrap();
                assert!(when.contains(':'), "each version says when it was made: {when}");
                assert_eq!(eval("document.querySelector('#artifact-host-menu [data-mode=versions]')", cx).await.unwrap(), "null", "no Versions page entry");
                assert_eq!(eval("document.querySelector('#artifact-host-menu .older').hidden", cx).await.unwrap(), "true");
                eval("document.querySelector(\"#artifact-host-menu [data-version='1']\").click();true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(80)).await;
                let (menu, _, preview, older, _) = page.update(cx, |page, _| page.smoke_view()).unwrap();
                assert!(!menu && preview && older == Some(1.), "the menu's version row shows version 1 in Preview");
                wait_ready(&page, cx).await;
                page.update(cx, |page, cx| page.smoke_menu(true, cx)).unwrap();
                cx.background_executor().timer(Duration::from_millis(30)).await;
                let rows = eval("[...document.querySelectorAll('#artifact-host-menu [data-version]')].map(b=>b.dataset.version+(b.getAttribute('aria-checked')==='true'?'*':'')).join(',')", cx).await.unwrap();
                assert!(rows.ends_with(",1*\""), "version 1 is the one checked on its page: {rows}");
                assert_eq!(eval("document.querySelector('#artifact-host-menu .older').hidden", cx).await.unwrap(), "false", "Show current version and Restore show for an older version");
                eval("document.querySelector('#artifact-host-menu [data-mode=current]').click();true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(80)).await;
                let (menu, _, preview, older, _) = page.update(cx, |page, _| page.smoke_view()).unwrap();
                assert!(!menu && preview && older.is_none(), "Show current version brings the current version back");
                wait_ready(&page, cx).await;
                eval("document.getElementById('a').contentWindow.postMessage({artifactProbe:'read'},'*');true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(30)).await;
                assert_eq!(serde_json::from_str::<Value>(&eval("window.probe", cx).await.unwrap()).unwrap(), before, "the current page keeps its scroll across the menu's version switch");
                eprintln!("artifact: the menu's version rows switch the artifact in place and back");
                for version in [Some(1.), None, Some(1.), None] {
                    let started = std::time::Instant::now();
                    page.update(cx, |page, cx| page.smoke_version(version, cx)).unwrap();
                    wait_ready(&page, cx).await;
                    eprintln!("artifact: version {version:?} {:.3} ms", started.elapsed().as_secs_f64()*1000.);
                    cx.background_executor().timer(Duration::from_millis(80)).await;
                }
                assert_eq!(serde_json::from_str::<Value>(&eval("window.probe", cx).await.unwrap()).unwrap(), before, "current preview must keep its scroll across versions");
                eprintln!("artifact: version switches preserve current preview and scroll");

                // Left on an older version in Source, then opened again from the list: the current version in
                // Preview, with its scroll.
                page.update(cx, |page, cx| page.smoke_version(Some(1.), cx)).unwrap();
                wait_ready(&page, cx).await;
                page.update(cx, |page, cx| page.smoke_source(true, cx)).unwrap();
                this.update_in(cx, |shell, window, cx| shell.select_tab(Tab::Artifacts, window, cx)).unwrap();
                cx.background_executor().timer(Duration::from_millis(100)).await;
                this.update_in(cx, |shell, window, cx| shell.push(Route::Artifact(id.clone()), window, cx)).unwrap();
                let reopened = this.update(cx, |shell, _| shell.top_page().unwrap().downcast::<crate::screens::artifact::ArtifactScreen>().unwrap()).unwrap();
                assert_eq!(reopened.entity_id(), page.entity_id(), "the cached page is reused");
                let (menu, _, preview, older, _) = page.update(cx, |page, _| page.smoke_view()).unwrap();
                assert!(!menu && preview && older.is_none(), "reopened on the current version in Preview");
                wait_ready(&page, cx).await;
                eval("document.getElementById('a').contentWindow.postMessage({artifactProbe:'read'},'*');true", cx).await.unwrap();
                cx.background_executor().timer(Duration::from_millis(30)).await;
                assert_eq!(serde_json::from_str::<Value>(&eval("window.probe", cx).await.unwrap()).unwrap(), before, "reopening keeps the scroll");
                eprintln!("artifact: reopening starts on the current version in Preview and keeps the scroll");

                // While the preview isn't ready, the app draws the menu (with the web view hidden under it).
                page.update(cx, |page, cx| page.release_web(cx)).unwrap();
                while page.update(cx, |page, _| page.smoke_view().4.is_none()).unwrap() {
                    cx.background_executor().timer(Duration::from_millis(1)).await;
                }
                page.update(cx, |page, cx| page.smoke_menu(true, cx)).unwrap();
                let (menu, in_host, ..) = page.update(cx, |page, _| page.smoke_view()).unwrap();
                assert!(menu && !in_host, "a preview still loading gets the app's menu");
                assert!(!page.update(cx, |page, cx| page.smoke_visible(cx)).unwrap(), "the web view hides under the app's menu");
                cx.background_executor().timer(Duration::from_millis(150)).await;
                if let Some(number) = native_input::window_number(1100.) {
                    let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap();
                    crate::window_shot::save(number, format!("{shots}/artifact-menu-fallback.png"));
                }
                page.update(cx, |page, cx| page.smoke_menu(false, cx)).unwrap();
                wait_ready(&page, cx).await;
                page.update(cx, |page, cx| page.smoke_menu(true, cx)).unwrap();
                let (menu, in_host, ..) = page.update(cx, |page, _| page.smoke_view()).unwrap();
                assert!(menu && in_host && page.update(cx, |page, cx| page.smoke_visible(cx)).unwrap(), "a ready preview draws its own menu");
                page.update(cx, |page, cx| page.smoke_menu(false, cx)).unwrap();
                eprintln!("artifact: menu falls back to the app's until the preview is ready");
            }
            cx.background_executor().timer(Duration::from_millis(200)).await;
        }
        if others.len() >= 2 {
            // Pointing at cards moves one preloaded page between them: its web view stays.
            this.update_in(cx, |shell, window, cx| shell.select_tab(Tab::Artifacts, window, cx)).unwrap();
            let mut web = None;
            for other in &others[..2] {
                this.update_in(cx, |shell, window, cx| shell.prepare_artifact(other.clone(), window, cx)).unwrap();
                let preload = this.update(cx, |shell, _| shell.artifact_preload()).unwrap().expect("a preloaded page");
                let started = std::time::Instant::now();
                let loaded = loop {
                    if let (_, _, _, _, Some(loaded)) = preload.update(cx, |page, _| page.smoke_view()).unwrap()
                        && preload.update(cx, |page, _| page.smoke_state()).unwrap().starts_with("data=true") { break loaded; }
                    cx.background_executor().timer(Duration::from_millis(1)).await;
                };
                eprintln!("artifact: hover {other} prepared in {:.3} ms", started.elapsed().as_secs_f64() * 1000.);
                assert!(web.is_none_or(|web| web == loaded), "hovering another card reuses the web view");
                web = Some(loaded);
                cx.background_executor().timer(Duration::from_millis(300)).await;
            }
            eprintln!("artifact: hovering cards reuses one web view");
            for (other, label) in [(&others[1], "hovered"), (&others[0], "not hovered")] {
                this.update_in(cx, |shell, window, cx| shell.select_tab(Tab::Artifacts, window, cx)).unwrap();
                let started = std::time::Instant::now();
                this.update_in(cx, |shell, window, cx| shell.push(Route::Artifact(other.clone()), window, cx)).unwrap();
                let page = this.update(cx, |shell, _| shell.top_page().unwrap().downcast::<crate::screens::artifact::ArtifactScreen>().unwrap()).unwrap();
                wait_ready(&page, cx).await;
                eprintln!("artifact: first open {label} {other} {:.3} ms", started.elapsed().as_secs_f64() * 1000.);
                cx.background_executor().timer(Duration::from_millis(300)).await;
            }
            // Another tab lets every artifact web view go.
            this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Notes, window, cx)).unwrap();
            assert!(this.update(cx, |shell, _| shell.artifact_preload().is_none()).unwrap());
            eprintln!("artifact: leaving Artifacts releases its pages");
        }
        for path in ["canvasArtifacts:get", "canvasArtifacts:listVersions", "canvasArtifacts:getVersion"] {
            for pass in 0..3 {
                let started = std::time::Instant::now();
                backend.call_raw(path, json!({"id":id,"version":1})).await.unwrap();
                eprintln!("artifact: data {path} pass {pass} {:.3} ms", started.elapsed().as_secs_f64()*1000.);
            }
        }
        eprintln!("artifact: done");
        cx.update(|_, cx| cx.quit()).ok();
    })
}

pub fn run(window: &mut Window, cx: &mut Context<AppShell>) -> Task<()> {
    let backend = backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let pause = Duration::from_millis(1500);
        let step = |label: &str| eprintln!("smoke: {label}");
        let query = |path: &'static str, args: Value| backend.call_raw(path, args);

        let note = first_id(query("notes:mine", json!({})), |v| v.as_array()?.first().and_then(id_of)).await;
        let routine =
            first_id(query("routines:myRoutines", json!({})), |v| v.as_array()?.first().and_then(id_of)).await;
        let artifact = first_id(query("canvasArtifacts:list", json!({ "limit": 100 })), |v| {
            // Only already-opened artifacts: opening a new one marks it opened.
            v.as_array()?.iter().find(|a| a.get("openedAt").is_some()).and_then(id_of)
        })
        .await;
        let post =
            first_id(query("feed:mine", json!({})), |v| v.get("posts")?.as_array()?.first().and_then(id_of)).await;

        let mut steps: Vec<(String, Option<Tab>, Option<Route>)> =
            Tab::ALL.iter().map(|tab| (format!("tab {}", tab.label()), Some(*tab), None)).collect();
        let mut push = |label: &str, route: Option<Route>| match route {
            Some(route) => steps.push((format!("page {label}"), None, Some(route))),
            None => eprintln!("smoke: skip {label} (no suitable data)"),
        };
        push("Scheduled", Some(Route::Scheduled));
        push("Memories", Some(Route::Memories));
        push("Appearance", Some(Route::Appearance));
        push("Name", Some(Route::Name));
        push("App icon", Some(Route::AppIcon));
        push("Desktop pet", Some(Route::DesktopPet));
        push("Feed settings", Some(Route::FeedSettings));
        push("Note", note.map(|id| Route::Note(Some(id))));
        push("Routine", routine.map(Route::Routine));
        push("Artifact", artifact.map(Route::Artifact));
        push("Feed post", post.map(Route::FeedPost));

        cx.background_executor().timer(pause).await;
        for (label, tab, route) in steps {
            step(&label);
            let ok = this
                .update_in(cx, |shell, window, cx| {
                    if let Some(tab) = tab {
                        shell.open_tab(tab, window, cx);
                    }
                    if let Some(route) = route {
                        shell.select_tab(Tab::Settings, window, cx);
                        shell.push(route, window, cx);
                    }
                })
                .is_ok();
            if !ok {
                break;
            }
            cx.background_executor().timer(pause).await;
        }
        // Back leaves a page and Forward brings the same one back.
        step("back and forward");
        let top = |shell: &AppShell| shell.top_page().map(|view| view.entity_id());
        let result = this
            .update_in(cx, |shell, window, cx| {
                shell.select_tab(Tab::Settings, window, cx);
                shell.push(Route::Memories, window, cx);
                let page = top(shell);
                shell.back(window, cx);
                let after_back = top(shell);
                shell.forward(window, cx);
                (page, after_back, top(shell))
            })
            .ok();
        match result {
            Some((Some(page), None, Some(again))) if page == again => {}
            other => eprintln!("smoke: FAILED back/forward: {other:?}"),
        }
        cx.background_executor().timer(pause).await;
        // Type into a new note like a keyboard would; autosave must create it.
        step("type into a new note");
        this.update_in(cx, |shell, window, cx| {
            shell.select_tab(Tab::Notes, window, cx);
            shell.push(Route::Note(None), window, cx);
        })
        .ok();
        cx.background_executor().timer(pause).await;
        let note = this
            .update(cx, |shell, _| shell.top_page().and_then(|view| view.downcast::<NoteScreen>().ok()))
            .ok()
            .flatten();
        match note {
            Some(note) => {
                note.update_in(cx, |note, window, cx| {
                    note.type_text("Smoke test note", "Typed by the desktop smoke test.", window, cx)
                })
                .ok();
                // The page loads in its own process; give it (and the 1.5 s autosave) time.
                let mut id = None;
                for _ in 0..30 {
                    cx.background_executor().timer(Duration::from_millis(500)).await;
                    id = note.update(cx, |note, _| note.saved_id()).ok().flatten();
                    if id.is_some() {
                        break;
                    }
                }
                match id {
                    Some(id) => {
                        eprintln!("smoke: note autosaved as {id}; deleting it");
                        backend.call_raw("notes:deleteMine", json!({ "id": id })).await.ok();
                    }
                    None => eprintln!("smoke: FAILED note was not autosaved"),
                }
            }
            None => eprintln!("smoke: FAILED note page did not open"),
        }
        eprintln!("smoke: done");
        cx.update(|_, cx| cx.quit()).ok();
    })
}

/// `FILO_SMOKE=app-icon filo`: the app icon. Picks each colour of Filo's mark and two avatars (one on a colour) in turn (as
/// Settings › App icon does), waits for the Dock to show it and for it to be kept for next launch, writes what
/// the Dock shows to /tmp/filo-dock-icon-<id>.png, then puts the pick back; then uploads a picture and removes it.
pub fn run_app_icon(window: &mut Window, cx: &mut Context<AppShell>) -> Task<()> {
    use crate::app_icon::{self, CHOICES, Icon};
    cx.spawn_in(window, async move |_, cx| {
        let tick = Duration::from_millis(100);
        let mut failed = false;
        let original = app_icon::chosen();
        let icons = CHOICES.iter().copied().map(Icon::Mark).chain([
            Icon::Avatar("owl-prop".into(), None),
            Icon::Avatar("owl-prop".into(), Some("purple")),
            Icon::Avatar("panda-waving".into(), None),
        ]);
        for choice in icons.filter(|icon| *icon != original).chain([original.clone()]) {
            let started = std::time::Instant::now();
            let expected = app_icon::render_icon(&choice).ok();
            cx.update(|_, cx| app_icon::choose(choice.clone(), cx)).ok();
            let mut kept = false;
            for _ in 0..100 {
                cx.background_executor().timer(tick).await;
                if expected.is_some() && std::fs::read(app_icon::cache_path()).ok() == expected {
                    kept = true;
                    break;
                }
            }
            if !kept {
                eprintln!("smoke: FAILED the {} icon was not kept", choice.id());
                failed = true;
                continue;
            }
            match cx.update(|_, _| app_icon::dock_icon_png()).ok().flatten() {
                Some(png) => {
                    let path = format!("/tmp/filo-dock-icon-{}.png", choice.id());
                    std::fs::write(&path, &png).ok();
                    eprintln!(
                        "smoke: {} on the Dock after {} ms → {path}",
                        choice.label(),
                        started.elapsed().as_millis()
                    );
                }
                None => {
                    eprintln!("smoke: FAILED couldn't read the Dock icon");
                    failed = true;
                }
            }
        }
        // A picture uploaded as the crop sheet keeps it: picked and on the Dock; removed, Filo's own is back.
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 30, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .ok();
        let before = app_icon::chosen();
        match cx.update(|_, cx| app_icon::keep_picture(&png, cx)) {
            Ok(Ok(())) => {
                let picture = app_icon::chosen();
                let name = picture.id().to_string();
                let kept = |icon: &Icon| {
                    app_icon::render_icon(icon)
                        .ok()
                        .filter(|png| std::fs::read(app_icon::cache_path()).ok().as_ref() == Some(png))
                        .is_some()
                };
                let mut shown = false;
                for _ in 0..100 {
                    cx.background_executor().timer(tick).await;
                    if kept(&picture) {
                        shown = true;
                        break;
                    }
                }
                if !matches!(picture, Icon::Picture(_)) || !app_icon::pictures().contains(&name) || !shown {
                    eprintln!("smoke: FAILED the uploaded picture wasn't picked and shown ({})", picture.id());
                    failed = true;
                } else {
                    let dock = cx.update(|_, _| app_icon::dock_icon_png()).ok().flatten();
                    if let Some(dock) = dock {
                        std::fs::write("/tmp/filo-dock-icon-picture.png", dock).ok();
                    }
                    eprintln!("smoke: {} ({name}) on the Dock → /tmp/filo-dock-icon-picture.png", picture.label());
                }
                cx.update(|_, cx| app_icon::remove_picture(&name, cx)).ok();
                let mut back = false;
                for _ in 0..100 {
                    cx.background_executor().timer(tick).await;
                    if kept(&Icon::default()) {
                        back = true;
                        break;
                    }
                }
                if app_icon::pictures().contains(&name)
                    || app_icon::picture_path(&name).exists()
                    || app_icon::chosen() != Icon::default()
                    || !back
                {
                    eprintln!("smoke: FAILED removing the picture didn't put Filo's own back");
                    failed = true;
                } else {
                    eprintln!("smoke: picture removed, Filo's own on the Dock");
                }
                if before != Icon::default() {
                    cx.update(|_, cx| app_icon::choose(before.clone(), cx)).ok();
                }
            }
            _ => {
                eprintln!("smoke: FAILED couldn't keep a picture");
                failed = true;
            }
        }
        eprintln!("smoke: {}", if failed { "FAILED" } else { "app icon done" });
        cx.update(|_, cx| cx.quit()).ok();
    })
}

/// `FILO_SMOKE=update filo` (from an app bundle with an update feed): waits for the launch check to find a newer build
/// and download it in the background, takes the picture `update-ready` (`FILO_DRIVE_SHOTS`), checks again (no second
/// banner), then does what `FILO_UPDATE_THEN` says: `restart` (default) clicks "Restart to update", `quit` quits through
/// Filo's menu, so the build goes in once Filo has quit. `banner-update` (automatic downloads off, `updates.json`) waits
/// for the build to be found instead (picture `update-available`), checks again (no second banner) and presses the
/// banner's Update button, which downloads, installs and restarts. `FILO_UPDATE_NEXT_QUIT=<ms>` has the restarted Filo
/// quit through its menu that long after it opens (`FILO_QUIT_DRIVE`), for a second round on the build it finds.
pub fn run_update(window: &mut Window, cx: &mut Context<AppShell>) -> Task<()> {
    use crate::updater;
    cx.spawn_in(window, async move |this, cx| {
        eprintln!("smoke: running {} from {:?}", updater::version_label(), updater::current_bundle());
        let Some(updates) = this.update(cx, |shell, _| shell.updates()).ok().flatten() else {
            eprintln!("smoke: FAILED this build has no update feed");
            cx.update(|_, cx| cx.quit()).ok();
            return;
        };
        let then = std::env::var("FILO_UPDATE_THEN").unwrap_or_default();
        let found_only = then == "banner-update";
        let started = std::time::Instant::now();
        let release = loop {
            let found = updates.update(cx, |updates, _| if found_only { updates.available() } else { updates.ready() });
            if let Ok(Some(release)) = found {
                break release;
            }
            if started.elapsed() > Duration::from_secs(180) {
                eprintln!("smoke: FAILED no build was {} in 3 min", if found_only { "found" } else { "downloaded" });
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            cx.background_executor().timer(Duration::from_millis(250)).await;
        };
        if found_only {
            eprintln!("smoke: {} found after {} ms, not downloaded", release.label(), started.elapsed().as_millis());
        } else {
            eprintln!("smoke: {} ready after {} ms, unasked", release.label(), started.elapsed().as_millis());
        }
        cx.background_executor().timer(Duration::from_millis(800)).await;
        crate::window_shot::save_step(if found_only { "update-available" } else { "update-ready" });
        if let Ok(check) = updates.update(cx, |updates, cx| updates.check(cx)) {
            check.await;
        }
        cx.background_executor().timer(Duration::from_millis(500)).await;
        if let Some(ms) = std::env::var("FILO_UPDATE_NEXT_QUIT").ok().and_then(|ms| ms.parse::<u64>().ok()) {
            // SAFETY: set on the main thread before the relaunch reads it; nothing else here reads the environment.
            unsafe { std::env::set_var("FILO_QUIT_DRIVE", format!("wait:{ms},menu")) };
        }
        match then.as_str() {
            "banner-update" => {
                eprintln!("smoke: pressing Update on the banner");
                crate::notifications::update_action(crate::notifications::UpdateAction::Update);
            }
            "quit" => {
                eprintln!("smoke: quitting with {} waiting", release.label());
                cx.update(|_, cx| crate::quit_guard::quit_deliberately(cx)).ok();
            }
            _ => {
                eprintln!("smoke: clicking Restart to update");
                updates.update_in(cx, |updates, window, cx| updates.click(window, cx)).ok();
            }
        }
    })
}

/// Resident memory of this process in MB, from `ps`.
fn rss_mb() -> f64 {
    let output = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    output
        .ok()
        .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.)
        .unwrap_or(0.)
}

/// `FILO_OPEN=soak-plan` with `FILO_SOAK_PLAN="feed,chat:<id>,coding-task:<id>,memories"`: visits those pages in
/// turn, over and over, waiting on each until its pictures have loaded (no load started or finished for 3 s, up
/// to 90 s), then staying FILO_SOAK_DWELL seconds (default 20). Logs one line per visit: how long the pictures took,
/// how many loads it started, and what the picture cache holds (pictures.rs).
fn run_soak_plan(window: &mut Window, cx: &mut Context<AppShell>) {
    let plan: Vec<String> = std::env::var("FILO_SOAK_PLAN")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|step| !step.is_empty())
        .map(str::to_string)
        .collect();
    let dwell = Duration::from_secs(std::env::var("FILO_SOAK_DWELL").ok().and_then(|v| v.parse().ok()).unwrap_or(20));
    cx.spawn_in(window, async move |this, cx| {
        let poll = Duration::from_millis(250);
        let stats =
            |cx: &mut gpui::AsyncWindowContext| cx.update(|_, cx| crate::pictures::stats(cx)).unwrap_or_default();
        eprintln!("soak-plan: visit,step,settle_ms,new_loads,loaded,loading,cache_mb,dropped_total,rss_mb");
        let mut pushed = false;
        for visit in 0usize.. {
            let Some(step) = plan.get(visit % plan.len().max(1)).cloned() else { break };
            let before = stats(cx);
            let started = std::time::Instant::now();
            let ok = this
                .update_in(cx, |shell, window, cx| {
                    if pushed {
                        shell.back(window, cx);
                    }
                    pushed = false;
                    match step.split_once(':') {
                        Some(("chat", id)) => shell.open_chat(id.to_string(), window, cx),
                        Some(("coding-task", id)) => {
                            shell.select_tab(Tab::Coding, window, cx);
                            shell.push(Route::CodingTask(id.to_string()), window, cx);
                            pushed = true;
                        }
                        _ if step == "memories" => {
                            shell.select_tab(Tab::Settings, window, cx);
                            shell.push(Route::Memories, window, cx);
                            pushed = true;
                        }
                        _ => shell.open_tab(Tab::Feed, window, cx),
                    }
                })
                .is_ok();
            if !ok {
                break;
            }
            // FILO_SOAK_SCROLL=<px>: a coding task opens at its end; scroll up that far once it has laid out.
            if let Some(up) = std::env::var("FILO_SOAK_SCROLL")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|_| step.starts_with("coding-task:"))
            {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                this.update(cx, |shell, cx| {
                    let thread = shell
                        .top_page()
                        .and_then(|view| view.downcast::<crate::screens::coding_thread::CodingThreadScreen>().ok());
                    if let Some(thread) = thread {
                        thread.update(cx, |thread, cx| thread.scroll_up(up, cx));
                    }
                })
                .ok();
            }
            // Settled: no load started or finished for 3 s. Pictures left loading on a page that's no longer drawn only
            // finish when it's drawn again, so they don't count.
            let mut last = (before.started, usize::MAX);
            let mut last_change = std::time::Instant::now();
            let mut settled = None;
            while started.elapsed() < Duration::from_secs(90) {
                cx.background_executor().timer(poll).await;
                let now = stats(cx);
                if (now.started, now.loading) != last {
                    last = (now.started, now.loading);
                    last_change = std::time::Instant::now();
                } else if last_change.elapsed() >= Duration::from_millis(3000) {
                    settled = Some(last_change.duration_since(started));
                    break;
                }
            }
            let now = stats(cx);
            let settle = settled.map_or("timeout".to_string(), |time| time.as_millis().to_string());
            eprintln!(
                "soak-plan: {},{step},{settle},{},{},{},{:.1},{},{:.0}",
                visit + 1,
                now.started - before.started,
                now.loaded,
                now.loading,
                now.bytes as f64 / 1048576.,
                now.dropped,
                rss_mb()
            );
            cx.background_executor().timer(dwell).await;
        }
    })
    .detach();
}

/// How long each `voiceSessions:detail` answer took the chat on screen to take in (conversation.rs `detail_arrived`).
static DETAIL_HANDLED: std::sync::Mutex<Vec<Duration>> = std::sync::Mutex::new(Vec::new());

pub fn detail_handled(took: Duration) {
    DETAIL_HANDLED.lock().unwrap().push(took);
}

/// The next frame after the one coming has been drawn: everything changed before this call is on screen.
async fn drawn(cx: &mut gpui::AsyncWindowContext) {
    for _ in 0..2 {
        let (tx, rx) = futures::channel::oneshot::channel();
        let asked = cx
            .update(|window, _| {
                window.on_next_frame(move |_, _| {
                    let _ = tx.send(());
                });
                window.refresh();
            })
            .is_ok();
        if !asked || rx.await.is_err() {
            return;
        }
    }
}

/// The live queries run since the last call, in short: how many, their time added up (and the CPU in it), and from
/// the first one's start to the last one's end; with the slowest few by path.
fn query_summary() -> String {
    summary_of(&crate::backend::query_times::take())
}

fn summary_of(runs: &[crate::backend::query_times::Run]) -> String {
    let Some(first) = runs.iter().map(|run| run.at).min() else { return "no queries".into() };
    let end = runs.iter().map(|run| run.at + run.took).max().unwrap_or(first);
    let ms = |took: Duration| took.as_secs_f64() * 1000.;
    let (took, cpu): (Duration, Duration) =
        (runs.iter().map(|run| run.took).sum(), runs.iter().map(|run| run.cpu).sum());
    let mut by_path: std::collections::HashMap<&str, (usize, Duration, Duration)> = std::collections::HashMap::new();
    for run in runs {
        let entry = by_path.entry(run.path.as_str()).or_default();
        entry.0 += 1;
        entry.1 += run.took;
        entry.2 += run.cpu;
    }
    let mut paths: Vec<_> = by_path.into_iter().collect();
    paths.sort_by(|a, b| b.1.2.cmp(&a.1.2));
    let top: Vec<String> =
        paths.iter().take(6).map(|(path, (count, _, cpu))| format!("{path}×{count} {:.1}", ms(*cpu))).collect();
    format!(
        "{} queries, {:.1} ms added up ({:.1} ms CPU), {:.1} ms first to last [CPU: {}]",
        runs.len(),
        ms(took),
        ms(cpu),
        ms(end.duration_since(first)),
        top.join(", ")
    )
}

/// `FILO_OPEN=chat-timing` with `FILO_TIMING_CHATS="<session id>,<session id>"`, on a copy of the user's data (a
/// FILO_CONFIG_DIR of its own; nothing of the copy's runs, `smoke::quiet_backend`): how long an agent's chat takes to
/// show, opened and switched to (`open`: from the click until its messages are in the list, `drawn` until a frame
/// with them has been drawn), and what a refresh while a turn runs costs: a change with no new message (what the
/// turn is doing, a turn's state) and one with a new reply appended to the first chat (how long until it's drawn).
/// Logs the live queries each time (backend.rs `query_times`) and how long the chat took in each answer.
fn run_chat_timing(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    let chats: Vec<String> = std::env::var("FILO_TIMING_CHATS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let quit = |cx: &mut gpui::AsyncWindowContext| {
            if std::env::var_os("FILO_DRIVE_STAY").is_none() {
                cx.update(|_, cx| cx.quit()).ok();
            }
        };
        if !crate::test_run::own_config() || chats.is_empty() {
            eprintln!("chat-timing: FAIL needs FILO_TIMING_CHATS and a FILO_CONFIG_DIR of its own (it adds messages to the chats)");
            return quit(cx);
        }
        let agents = backend.call_raw("agents:list", json!({})).await.unwrap_or_default();
        let agent_of = |session: &str| {
            agents.as_array()?.iter().find(|row| row.get("sessionId").and_then(Value::as_str) == Some(session)).and_then(id_of)
        };
        let Some(chats) = chats.iter().map(|chat| Some((agent_of(chat)?, chat.clone()))).collect::<Option<Vec<_>>>() else {
            eprintln!("chat-timing: FAIL a chat isn't an agent's");
            return quit(cx);
        };
        // FILO_TIMING_DUMP=<file>: what the chats' queries answer, saved for comparing two builds, then quit.
        if let Ok(file) = std::env::var("FILO_TIMING_DUMP") {
            let mut answers = serde_json::Map::new();
            let mut ask = vec![("agents:list".to_string(), json!({})), ("voiceSessions:unseen".to_string(), json!({}))];
            for (_, session) in &chats {
                for limit in [40, 600] {
                    ask.push(("voiceSessions:detail".into(), json!({ "id": session, "limit": limit })));
                }
                for path in ["voiceSessions:chatTurns", "coding:sessionsForChat", "coding:requestsForSession", "chatShared:list"] {
                    ask.push((path.into(), json!({ "sessionId": session })));
                }
            }
            for (path, args) in ask {
                let answer = backend.call_raw(&path, args.clone()).await;
                answers.insert(format!("{path} {args}"), answer.unwrap_or_else(|error| json!({ "error": error })));
            }
            std::fs::write(&file, serde_json::to_string_pretty(&Value::Object(answers)).unwrap_or_default()).ok();
            eprintln!("chat-timing: answers saved in {file}");
            return quit(cx);
        }
        // Whether the chat on screen is `session` with its messages in the list.
        let shown = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, session: &str, message: Option<&str>| {
            let conversation = shown_conversation(this, cx).1;
            conversation.is_some_and(|conversation| {
                conversation
                    .read_with(cx, |conversation, _| {
                        let labels = conversation.row_labels();
                        conversation.session_id() == Some(session)
                            && match message {
                                Some(id) => labels.iter().any(|label| label == id),
                                None => labels.iter().filter(|label| label.contains('-')).count() > 1,
                            }
                    })
                    .unwrap_or(false)
            })
        };
        wait(4000).await;
        crate::backend::query_times::start();
        // Opened for the first time, then switched back to.
        for visit in 0..3 {
            for (agent, session) in &chats {
                DETAIL_HANDLED.lock().unwrap().clear();
                crate::backend::query_times::take();
                let started = std::time::Instant::now();
                this.update_in(cx, |shell, window, cx| shell.open_agent(agent.clone(), window, cx)).ok();
                let mut open = None;
                while started.elapsed() < Duration::from_secs(20) {
                    if shown(&this, cx, session, None) {
                        open = Some(started.elapsed());
                        break;
                    }
                    wait(1).await;
                }
                drawn(cx).await;
                let drawn_at = started.elapsed();
                // The rest of its queries answer.
                wait(1500).await;
                let handled: Vec<String> = DETAIL_HANDLED.lock().unwrap().iter().map(|took| format!("{:.1}", took.as_secs_f64() * 1000.)).collect();
                let size = std::fs::metadata(crate::config::config_dir().join("local/agents/chats").join(session).join("messages.jsonl")).map_or(0, |meta| meta.len());
                eprintln!(
                    "chat-timing: {} {session} ({:.1} MB): open {} ms, drawn {:.0} ms; detail taken in [{}] ms; {}",
                    if visit == 0 { "first open" } else { "switch to" },
                    size as f64 / 1048576.,
                    open.map_or("timeout".into(), |open| format!("{:.0}", open.as_secs_f64() * 1000.)),
                    drawn_at.as_secs_f64() * 1000.,
                    handled.join(", "),
                    query_summary()
                );
            }
        }
        let (agent, session) = chats[0].clone();
        this.update_in(cx, |shell, window, cx| shell.open_agent(agent.clone(), window, cx)).ok();
        wait(2000).await;
        // A change that moves nothing in the chat (what the turn is doing): every live query runs again.
        let mut live = Vec::new();
        for round in 0..5 {
            crate::backend::query_times::take();
            backend.poke();
            wait(1500).await;
            live = crate::backend::query_times::take();
            eprintln!("chat-timing: refresh {} with nothing new: {}", round + 1, summary_of(&live));
        }
        // Each of those queries again, alone, a few times: what each costs without waiting on the others.
        let mut alone = Vec::new();
        let mut total = (Duration::ZERO, Duration::ZERO);
        // Not what the window asked once (marking the chat read): that changes the data.
        let live: Vec<_> = live.into_iter().filter(|run| !run.path.starts_with("call ")).collect();
        for run in &live {
            let mut times = Vec::new();
            for _ in 0..5 {
                times.push(backend.time_query(&run.path, run.args.clone()).await);
            }
            times.sort();
            let (took, cpu) = times[times.len() / 2];
            total = (total.0 + took, total.1 + cpu);
            let about = run.args.get("sessionId").or_else(|| run.args.get("id")).and_then(Value::as_str).map_or(String::new(), |id| format!(" {}", &id[..8.min(id.len())]));
            alone.push((took, format!("{}{about} {:.1}", run.path, took.as_secs_f64() * 1000.)));
        }
        alone.sort_by(|a, b| b.0.cmp(&a.0));
        eprintln!(
            "chat-timing: a refresh's {} queries one at a time (median of 5): {:.1} ms ({:.1} ms CPU) [{}]",
            live.len(),
            total.0.as_secs_f64() * 1000.,
            total.1.as_secs_f64() * 1000.,
            alone.iter().map(|(_, line)| line.as_str()).collect::<Vec<_>>().join(", ")
        );
        // A new reply in the chat: until it's drawn.
        let store = filo_local::Store::at(crate::config::config_dir().join("local/agents"));
        for round in 0..5 {
            DETAIL_HANDLED.lock().unwrap().clear();
            crate::backend::query_times::take();
            let message = filo_local::Message::new(filo_local::Role::Assistant, format!("Timing check {}: a new reply while the turn runs.", round + 1), None);
            if let Err(error) = store.append(&session, &message) {
                eprintln!("chat-timing: FAIL couldn't add a message: {error}");
                break;
            }
            let started = std::time::Instant::now();
            backend.poke();
            let mut seen = None;
            while started.elapsed() < Duration::from_secs(20) {
                if shown(&this, cx, &session, Some(&message.id)) {
                    seen = Some(started.elapsed());
                    break;
                }
                wait(1).await;
            }
            drawn(cx).await;
            let drawn_at = started.elapsed();
            wait(1500).await;
            let handled: Vec<String> = DETAIL_HANDLED.lock().unwrap().iter().map(|took| format!("{:.1}", took.as_secs_f64() * 1000.)).collect();
            eprintln!(
                "chat-timing: new reply {}: in the list {} ms, drawn {:.0} ms; detail taken in [{}] ms; {}",
                round + 1,
                seen.map_or("timeout".into(), |seen| format!("{:.0}", seen.as_secs_f64() * 1000.)),
                drawn_at.as_secs_f64() * 1000.,
                handled.join(", "),
                query_summary()
            );
        }
        eprintln!("chat-timing: done");
        quit(cx);
    })
    .detach();
}

/// CPU time this process has used, all threads (user and system).
fn process_cpu() -> Duration {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let time = |t: libc::timeval| Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64);
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// CPU time the calling thread has used (the main thread, from a drive).
fn thread_cpu() -> Duration {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// `FILO_OPEN=chat-scroll-timing` with `FILO_TIMING_CHATS=<session id>`, on a copy of the user's data (nothing of it
/// runs, `smoke::quiet_backend`): opens that agent's chat at its newest message, then turns the wheel over the feed
/// (`FILO_SCROLL_STEP` points a frame, default 60, a wheel event posted to the window) until it's at the
/// top of all the history it loads; with `FILO_TIMING_SCRUB=1` it drags the timeline at the chat's right instead, fast
/// up and down across the pages of history as they load. Logs each frame's draw (from the frame starting until it's
/// presented, and the main thread's CPU in that), the time between frames and the main thread's CPU in it, the
/// process's and the main thread's CPU over it, then 5 s with nothing happening; and whether each page that came in
/// kept the message on screen, any frame that moved the chat the wrong way for the input, and any with messages out
/// of order. `FILO_DRIVE_VIDEO=<file.mp4>` records the window meanwhile (window_shot.rs `Recording`, needs `ffmpeg`);
/// `FILO_TIMING_TRACE=1` logs every frame's place.
fn run_chat_scroll_timing(window: &mut Window, cx: &mut Context<AppShell>) {
    let session =
        std::env::var("FILO_TIMING_CHATS").unwrap_or_default().split(',').next().unwrap_or("").trim().to_string();
    let step: f32 = std::env::var("FILO_SCROLL_STEP").ok().and_then(|step| step.parse().ok()).unwrap_or(60.);
    let scrubbing = std::env::var_os("FILO_TIMING_SCRUB").is_some();
    let backend = backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let quit = |cx: &mut gpui::AsyncWindowContext| {
            if std::env::var_os("FILO_DRIVE_STAY").is_none() {
                cx.update(|_, cx| cx.quit()).ok();
            }
        };
        if !crate::test_run::own_config() || session.is_empty() {
            eprintln!("chat-scroll-timing: FAIL needs FILO_TIMING_CHATS and a FILO_CONFIG_DIR of its own");
            return quit(cx);
        }
        let agents = backend.call_raw("agents:list", json!({})).await.unwrap_or_default();
        let agent = agents
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row.get("sessionId").and_then(Value::as_str) == Some(session.as_str())))
            .and_then(id_of);
        let Some(agent) = agent else {
            eprintln!("chat-scroll-timing: FAIL {session} isn't an agent's chat");
            return quit(cx);
        };
        this.update_in(cx, |shell, window, cx| shell.open_agent(agent.clone(), window, cx)).ok();
        let opened = std::time::Instant::now();
        let conversation = loop {
            let shown = shown_conversation(&this, cx).1.filter(|conversation| {
                conversation
                    .read_with(cx, |conversation, _| {
                        conversation.session_id() == Some(session.as_str()) && conversation.history_loaded().0 > 0
                    })
                    .unwrap_or(false)
            });
            if let Some(conversation) = shown {
                break conversation;
            }
            if opened.elapsed() > Duration::from_secs(20) {
                eprintln!("chat-scroll-timing: FAIL the chat didn't open");
                return quit(cx);
            }
            wait(10).await;
        };
        // Its glances, pictures and coding rows come in.
        wait(4000).await;
        drawn(cx).await;
        // `FILO_TIMING_IDLE=<seconds>`: the chat left alone that long, as it sits while agents work.
        if let Some(seconds) = std::env::var("FILO_TIMING_IDLE").ok().and_then(|seconds| seconds.parse::<u64>().ok()) {
            eprintln!("chat-scroll-timing: idle from here");
            let (idle_from, cpu, main_cpu) = (std::time::Instant::now(), process_cpu(), thread_cpu());
            // Recorded, every frame presented (a turning spinner's steps are presented with nothing drawn).
            let width = cx.update(|window, _| f64::from(f32::from(window.viewport_size().width))).unwrap_or(0.);
            let recording = std::env::var("FILO_DRIVE_VIDEO").ok().and_then(|file| {
                crate::window_shot::Recording::start(native_input::window_number(width)?, std::path::Path::new(&file))
            });
            match &recording {
                Some(recording) => {
                    while idle_from.elapsed() < Duration::from_secs(seconds) {
                        let (tx, rx) = futures::channel::oneshot::channel();
                        cx.update(|window, _| {
                            window.on_next_frame(move |_, _| {
                                let _ = tx.send(());
                            })
                        })
                        .ok();
                        if rx.await.is_err() {
                            break;
                        }
                        recording.frame();
                    }
                }
                None => wait(seconds * 1000).await,
            }
            if let Some(recording) = recording {
                eprintln!("chat-scroll-timing: video saved: {}", recording.finish());
            }
            let idle = idle_from.elapsed().as_secs_f64();
            eprintln!("chat-scroll-timing: idle to here");
            eprintln!(
                "chat-scroll-timing: CPU idle {:.1}% (main thread {:.1}%) over {idle:.0} s",
                100. * (process_cpu() - cpu).as_secs_f64() / idle,
                100. * (thread_cpu() - main_cpu).as_secs_f64() / idle
            );
            eprintln!("chat-scroll-timing: done");
            return quit(cx);
        }
        let size = cx.update(|window, _| window.viewport_size()).unwrap_or_default();
        let (width, height) = (f64::from(f32::from(size.width)), f64::from(f32::from(size.height)));
        let read = |cx: &mut gpui::AsyncWindowContext| {
            conversation
                .read_with(cx, |conversation, _| {
                    (
                        conversation.scroll_position(),
                        conversation.history_loaded(),
                        conversation.top_message(),
                        conversation.messages_in_order(),
                        conversation.timeline_strip().map(|(strip, _)| strip),
                        conversation.timeline_strip().map(|(_, place)| place),
                    )
                })
                .ok()
        };
        // The drag: where along the timeline's strip the pointer goes, frame by frame (0 its first dash, 1 its last),
        // fast from the newest to the oldest and back across the pages as they load, then on past its top.
        let scrub: Option<Vec<f32>> = scrubbing.then(|| {
            let mut plan = Vec::new();
            let go = |plan: &mut Vec<f32>, from: f32, to: f32, frames: usize| {
                plan.extend((1..=frames).map(|frame| from + (to - from) * frame as f32 / frames as f32));
            };
            for _ in 0..6 {
                go(&mut plan, 1., 0., 18);
                go(&mut plan, 0., 0., 10);
                go(&mut plan, 0., 0.6, 12);
                go(&mut plan, 0.6, 0., 12);
                go(&mut plan, 0., 0., 10);
                go(&mut plan, 0., 1., 18);
            }
            go(&mut plan, 1., -0.15, 20);
            go(&mut plan, -0.15, -0.15, 20);
            plan
        });
        let strip_y = |strip: gpui::Bounds<gpui::Pixels>, along: f32| {
            // The first and last dashes are 14 points in from the strip's ends (timeline.rs `PAD`).
            let (top, bottom) = (f32::from(strip.top()) + 14., f32::from(strip.bottom()) - 14.);
            f64::from(top + (bottom - top) * along)
        };
        let (mut strip_x, mut pointer_y) = (0., 0.);
        if let Some(plan) = &scrub {
            let Some((_, _, _, _, Some(strip), _)) = read(cx) else {
                eprintln!("chat-scroll-timing: FAIL the timeline isn't showing");
                return quit(cx);
            };
            (strip_x, pointer_y) = (f64::from(f32::from(strip.center().x)), strip_y(strip, 1.));
            native_input::drag_step(width, native_input::Drag::Down, strip_x, pointer_y);
            eprintln!("chat-scroll-timing: dragging the timeline, {} moves", plan.len());
        } else {
            eprintln!("chat-scroll-timing: scrolling");
        }
        let recording = std::env::var("FILO_DRIVE_VIDEO").ok().and_then(|file| {
            let number = native_input::window_number(width)?;
            let recording = crate::window_shot::Recording::start(number, std::path::Path::new(&file));
            if recording.is_none() {
                eprintln!("chat-scroll-timing: couldn't record {file}");
            }
            recording
        });
        DETAIL_HANDLED.lock().unwrap().clear();
        let mut draws: Vec<Duration> = Vec::new();
        let mut draw_cpus: Vec<Duration> = Vec::new();
        let mut frame_cpus: Vec<Duration> = Vec::new();
        let mut gaps: Vec<Duration> = Vec::new();
        let (started, cpu, main_cpu) = (std::time::Instant::now(), process_cpu(), thread_cpu());
        let mut last_frame: Option<(std::time::Instant, Duration)> = None;
        let (mut still, mut stuck) = (0, 0);
        let (wheel_y, mut wheel_at) = ([0.4, 0.25, 0.6, 0.15], 0);
        let mut position = (usize::MAX, 0.);
        // What each frame showed: the loaded messages and the one at the top (how many loaded are older), to tell a
        // page that came in without moving it, and a move the wrong way.
        let mut before = read(cx).and_then(|(_, (loaded, _), top, ..)| Some((loaded, top?)));
        let (mut pages, mut kept, mut wrong_way, mut unordered) = (Vec::new(), 0, Vec::new(), 0);
        let mut along_before = 1.;
        // Where the input went: up the chat (older) is up the strip, or the wheel turned up.
        let (mut toward_before, mut toward_earlier) = (0f32, 0f32);
        // More than this many messages is a jump (a page is 40); up to it, the input's own move in that frame.
        const JUMP: i64 = 5;
        let mut frame_index = 0;
        while started.elapsed() < Duration::from_secs(240) {
            // Up the chat: older messages, through AppKit as a person's wheel or pointer.
            let toward = match &scrub {
                Some(plan) => {
                    let Some(&along) = plan.get(frame_index) else { break };
                    // Moving, to that place on the strip as it shows now; holding, the pointer stays where it is
                    // while the strip grows under it with the pages that come in, as a hand's does.
                    let strip = read(cx).and_then(|(.., strip, _)| strip);
                    if let Some(strip) = strip
                        && along != along_before
                    {
                        pointer_y = strip_y(strip, along);
                    }
                    native_input::drag_step(width, native_input::Drag::Move, strip_x, pointer_y);
                    pointer_y as f32
                }
                None => {
                    // Over the feed, right of the agents' list and above the composer; elsewhere over it while a web
                    // page shown in it (a glance's) is under the pointer, which takes the wheel itself.
                    if stuck >= 10 {
                        let (native, _) = native_input::native_at(width, width * 0.7, height * wheel_y[wheel_at]);
                        eprintln!("chat-scroll-timing: the wheel stopped at {position:?} over {native:?}; moving it");
                        wheel_at = (wheel_at + 1) % wheel_y.len();
                        stuck = 0;
                    }
                    if !native_input::scroll_in(width, width * 0.7, height * wheel_y[wheel_at], -f64::from(step)) {
                        eprintln!("chat-scroll-timing: FAIL couldn't post a wheel event");
                        break;
                    }
                    toward_before - 1.
                }
            };
            frame_index += 1;
            // The input notifies the chat, as a person's does: nothing else is drawn again (no `refresh`, which
            // would draw views kept as drawn afresh).
            let (tx, rx) = futures::channel::oneshot::channel();
            cx.update(|window, _| {
                window.on_next_frame(move |_, _| {
                    let _ = tx.send((std::time::Instant::now(), thread_cpu()));
                });
            })
            .ok();
            let Ok((frame, frame_cpu)) = rx.await else { break };
            // This runs once the frame that began at `frame` has been drawn and presented.
            draws.push(frame.elapsed());
            draw_cpus.push(thread_cpu().saturating_sub(frame_cpu));
            if let Some((last, last_cpu)) = last_frame.replace((frame, frame_cpu)) {
                gaps.push(frame - last);
                // All the main thread did from one frame to the next: the frame, the input, what arrived.
                frame_cpus.push(frame_cpu.saturating_sub(last_cpu));
            }
            if let Some(recording) = &recording {
                recording.frame();
            }
            let Some((now, (loaded, more), top, in_order, _, place)) = read(cx) else { break };
            if std::env::var_os("FILO_TIMING_TRACE").is_some() {
                eprintln!(
                    "chat-scroll-timing: frame {frame_index} input {toward:.1} place {place:?} loaded {loaded} top {now:?} {top:?}"
                );
            }
            unordered += usize::from(!in_order);
            if let (Some((loaded_before, (id_before, older_before))), Some((id, older))) = (&before, &top) {
                let added = loaded.saturating_sub(*loaded_before);
                // How far the top moved in messages, older ones that came in above it not counted (- older).
                let moved = *older as i64 - (*older_before + added) as i64;
                if added > 0 {
                    pages.push(format!("+{added}: {}", if id == id_before { "kept".into() } else { format!("moved {moved}") }));
                    kept += usize::from(moved.abs() <= JUMP);
                }
                // Up the strip (or the wheel up) never shows newer messages, down it never older ones. A posted event
                // can land a frame late, so the step before counts too.
                let up = toward < toward_before || toward_before < toward_earlier;
                let down = toward > toward_before || toward_before > toward_earlier;
                // Where the input turned back, either way is the input's.
                let turned = up && down;
                if !turned && ((up && moved > JUMP) || (down && moved < -JUMP) || (!up && !down && moved.abs() > JUMP)) {
                    wrong_way.push(format!("frame {frame_index}: {moved:+}"));
                }
            }
            before = top.map(|top| (loaded, top));
            (toward_earlier, toward_before) = (toward_before, toward);
            if let Some(plan) = &scrub {
                along_before = plan[frame_index - 1];
            }
            // Scrolling: at the top with nothing more to load, for half a second of frames.
            still = if now == position && now == (0, 0.) && !more { still + 1 } else { 0 };
            stuck = if now == position && now != (0, 0.) { stuck + 1 } else { 0 };
            position = now;
            if scrub.is_none() && still >= 30 {
                break;
            }
        }
        if scrub.is_some() {
            native_input::drag_step(width, native_input::Drag::Up, strip_x, pointer_y);
        }
        if let Some(recording) = recording {
            eprintln!("chat-scroll-timing: video saved: {}", recording.finish());
        }
        eprintln!(
            "chat-scroll-timing: pages that came in: {} of {} kept the message on screen, within {JUMP} [{}]",
            kept,
            pages.len(),
            pages.join(", ")
        );
        eprintln!(
            "chat-scroll-timing: jumps (the wrong way for the input, or with none): {} [{}]; frames with messages out of \
             order: {unordered}",
            wrong_way.len(),
            wrong_way.iter().take(12).cloned().collect::<Vec<_>>().join(", ")
        );
        let (took, cpu, main_cpu) = (started.elapsed(), process_cpu() - cpu, thread_cpu() - main_cpu);
        let (loaded, more) = conversation.read_with(cx, |conversation, _| conversation.history_loaded()).unwrap_or_default();
        let ms = |d: Duration| d.as_secs_f64() * 1000.;
        let stats = |times: &mut Vec<Duration>| {
            times.sort();
            let at = |q: f64| times.get(((times.len() as f64 - 1.) * q).round() as usize).copied().map_or(0., ms);
            format!(
                "median {:.1} ms, p90 {:.1}, p99 {:.1}, max {:.1}; over 16.7 ms: {}, over 33 ms: {}",
                at(0.5),
                at(0.9),
                at(0.99),
                at(1.),
                times.iter().filter(|t| ms(**t) > 16.7).count(),
                times.iter().filter(|t| ms(**t) > 33.4).count()
            )
        };
        eprintln!(
            "chat-scroll-timing: {} frames in {:.1} s ({:.0} fps), {} messages loaded{}, at the top: {}",
            draws.len(),
            took.as_secs_f64(),
            draws.len() as f64 / took.as_secs_f64(),
            loaded,
            if more { ", more to load" } else { "" },
            position == (0, 0.)
        );
        eprintln!("chat-scroll-timing: draw {}", stats(&mut draws));
        eprintln!("chat-scroll-timing: draw, main thread CPU {}", stats(&mut draw_cpus));
        eprintln!("chat-scroll-timing: between frames {}", stats(&mut gaps));
        eprintln!("chat-scroll-timing: main thread CPU from frame to frame {}", stats(&mut frame_cpus));
        eprintln!(
            "chat-scroll-timing: CPU while scrolling {:.0}% (main thread {:.0}%)",
            100. * cpu.as_secs_f64() / took.as_secs_f64(),
            100. * main_cpu.as_secs_f64() / took.as_secs_f64()
        );
        let handled: Vec<String> =
            DETAIL_HANDLED.lock().unwrap().iter().map(|took| format!("{:.0}", ms(*took))).collect();
        eprintln!("chat-scroll-timing: pages of history taken in (ms): [{}]", handled.join(", "));
        let (idle_from, cpu, main_cpu) = (std::time::Instant::now(), process_cpu(), thread_cpu());
        wait(5000).await;
        let idle = idle_from.elapsed().as_secs_f64();
        eprintln!(
            "chat-scroll-timing: CPU idle after {:.1}% (main thread {:.1}%)",
            100. * (process_cpu() - cpu).as_secs_f64() / idle,
            100. * (thread_cpu() - main_cpu).as_secs_f64() / idle
        );
        eprintln!("chat-scroll-timing: done");
        quit(cx);
    })
    .detach();
}

fn run_soak(home: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    let every = Duration::from_secs(std::env::var("FILO_SOAK_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(90));
    cx.spawn_in(window, async move |this, cx| {
        let settle = Duration::from_secs(4);
        // The agents' conversations, up to six.
        let agents = backend.call_raw("agents:list", json!({})).await.unwrap_or_default();
        let session = |row: &Value| row.get("sessionId").and_then(Value::as_str).map(str::to_string);
        let others: Vec<String> =
            agents.as_array().map(|rows| rows.iter().filter_map(session).take(6).collect()).unwrap_or_default();
        let home = home.or_else(|| others.first().cloned());
        let open_home = |shell: &mut AppShell, window: &mut Window, cx: &mut Context<AppShell>| match &home {
            Some(id) => shell.open_chat(id.clone(), window, cx),
            None => shell.open_tab(Tab::Agents, window, cx),
        };
        this.update_in(cx, open_home).ok();
        eprintln!("soak: start, chat {home:?}, {:.0} MB", rss_mb());
        for round in 0usize.. {
            cx.background_executor().timer(every).await;
            let label = match round % 5 {
                0 => "feed",
                1 => "artifacts",
                2 => "agents",
                3 => "memories",
                _ => "another chat",
            };
            let ok = this
                .update_in(cx, |shell, window, cx| match round % 5 {
                    0 => shell.open_tab(Tab::Feed, window, cx),
                    1 => shell.open_tab(Tab::Artifacts, window, cx),
                    2 => shell.open_tab(Tab::Agents, window, cx),
                    3 => {
                        shell.select_tab(Tab::Settings, window, cx);
                        shell.push(Route::Memories, window, cx);
                    }
                    _ => match others.get(1 + round / 5 % others.len().saturating_sub(1).max(1)) {
                        Some(id) => shell.open_chat(id.clone(), window, cx),
                        None => shell.open_tab(Tab::Feed, window, cx),
                    },
                })
                .is_ok();
            if !ok {
                break;
            }
            cx.background_executor().timer(settle).await;
            this.update_in(cx, |shell, window, cx| {
                if round % 5 == 3 {
                    shell.back(window, cx);
                }
                open_home(shell, window, cx);
            })
            .ok();
            eprintln!("soak: round {} ({label}), {:.0} MB", round + 1, rss_mb());
        }
    })
    .detach();
}

/// `FILO_OPEN=feed-drive`: the Feed's covers through what made them go dark (opening and closing "More" on every
/// post, scrolling to the end and back, a topic, leaving the tab past the picture cache's KEEP,
/// closing and reopening the tab). After each step it logs every listed cover's state in the picture cache and what
/// the cache holds; with `FILO_DRIVE_SHOTS=<folder>` a screenshot of each step goes there. Quits at the end.
/// `FILO_DRIVE_PAUSE=<ms>` is the wait after each step (700), `FILO_DRIVE_AWAY=<s>` how long it stays off the tab (35).
fn run_feed_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Feed, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let pause: u64 = std::env::var("FILO_DRIVE_PAUSE").ok().and_then(|v| v.parse().ok()).unwrap_or(700);
        // What the listed covers are doing, and the cache.
        let report = |step: &str, cx: &mut gpui::AsyncWindowContext| {
            let line = this
                .update(cx, |shell, cx| {
                    let feed = shell.feed_screen()?;
                    let covers = feed.read(cx).drive_covers();
                    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
                    for (_, url) in &covers {
                        *counts.entry(crate::pictures::state(url, cx)).or_default() += 1;
                    }
                    let stats = crate::pictures::stats(cx);
                    Some(format!(
                        "covers {} {:?} | cache loaded {} loading {} {:.0} MB, started {} dropped {}",
                        covers.len(),
                        counts,
                        stats.loaded,
                        stats.loading,
                        stats.bytes as f64 / 1048576.,
                        stats.started,
                        stats.dropped
                    ))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no feed".into());
            eprintln!("feed-drive: {step}: {line}, rss {:.0} MB", rss_mb());
        };
        let shot = |name: &str| {
            crate::window_shot::save_step(name);
        };
        let feed = |cx: &mut gpui::AsyncWindowContext,
                    f: &dyn Fn(
            &mut crate::screens::feed::FeedScreen,
            &mut Context<crate::screens::feed::FeedScreen>,
        )| {
            this.update(cx, |shell, cx| {
                if let Some(feed) = shell.feed_screen() {
                    feed.update(cx, |feed, cx| f(feed, cx));
                }
            })
            .ok();
        };
        // Loaded: no cover still loading for 3 s (up to 90 s).
        async fn settle(this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext) {
            let started = std::time::Instant::now();
            let mut quiet = 0;
            while started.elapsed() < Duration::from_secs(90) && quiet < 12 {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let loading = this
                    .update(cx, |shell, cx| {
                        let Some(feed) = shell.feed_screen() else { return 1 };
                        let covers = feed.read(cx).drive_covers();
                        if covers.is_empty() {
                            return 1;
                        }
                        // Covers off screen aren't drawn (the list), so only ones loading count, once one has loaded.
                        let states: Vec<&str> = covers.iter().map(|(_, url)| crate::pictures::state(url, cx)).collect();
                        if !states.contains(&"loaded") {
                            1
                        } else {
                            states.iter().filter(|state| **state == "loading").count()
                        }
                    })
                    .unwrap_or(1);
                quiet = if loading == 0 { quiet + 1 } else { 0 };
            }
        }
        settle(&this, cx).await;
        report("loaded", cx);
        shot("01-loaded");
        // Clicks on the first post's More, as the user did: open and close it five times, the next post's cover in view.
        let ids = this
            .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_post_ids()))
            .ok()
            .flatten()
            .unwrap_or_default();
        if let Some(first) = first_on_screen("more-", &ids) {
            for round in 1..=5 {
                for open in [true, false] {
                    let clicked = click(&this, cx, &format!("more-{first}"));
                    wait(pause).await;
                    let expanded = this
                        .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_expanded(&first)))
                        .ok()
                        .flatten();
                    report(&format!("clicked More ({clicked}) #{round}, open {expanded:?} (wanted {open})"), cx);
                    if round == 1 && open {
                        shot("02-clicked-more-open");
                    }
                }
            }
            shot("03-clicked-more-closed-after-5");
        }
        // The post being read keeps its place: scroll 80 points into a post with a summary, click its More (a real
        // click), and the list must still start 80 points into that post (GPUI alone put it back at its top).
        let with_summary = this
            .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_post_ids()))
            .ok()
            .flatten()
            .unwrap_or_default();
        for id in with_summary.iter().skip(2).take(6) {
            let Some(row) = this
                .update(cx, |shell, cx| shell.feed_screen().and_then(|feed| feed.read(cx).drive_row_of(id)))
                .ok()
                .flatten()
            else {
                continue;
            };
            let target = row;
            feed(cx, &move |feed, cx| feed.drive_scroll_into(target, 80., cx));
            wait(pause).await;
            if on_screen(&format!("more-{id}")).is_none() {
                continue;
            }
            let before = this
                .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_scroll_top()))
                .ok()
                .flatten();
            let clicked = click(&this, cx, &format!("more-{id}"));
            wait(pause).await;
            let after = this
                .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_scroll_top()))
                .ok()
                .flatten();
            report(
                &format!(
                    "anchor: clicked More ({clicked}) on row {row} read 80 pt in: list top {before:?} -> {after:?}"
                ),
                cx,
            );
            shot("03c-more-opened-while-reading-keeps-place");
            click(&this, cx, &format!("more-{id}"));
            wait(pause).await;
            let closed = this
                .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_scroll_top()))
                .ok()
                .flatten();
            report(&format!("anchor: closed More: list top {closed:?}"), cx);
            break;
        }
        feed(cx, &|feed, cx| feed.drive_scroll_row(0, cx));
        wait(pause).await;
        // Then every post's More at once (programmatic), the heaviest relayout.
        for round in 1..=5 {
            feed(cx, &|feed, cx| feed.drive_more(true, cx));
            wait(pause).await;
            report(&format!("more open #{round}"), cx);
            if round == 1 {
                shot("02b-all-more-open");
            }
            feed(cx, &|feed, cx| feed.drive_more(false, cx));
            wait(pause).await;
            report(&format!("more closed #{round}"), cx);
        }
        shot("03b-all-more-closed-after-5");
        for (index, row) in [2usize, 4, 8, 16, 24, 32, 99, 0].into_iter().enumerate() {
            feed(cx, &move |feed, cx| feed.drive_scroll_row(row, cx));
            wait(pause).await;
            report(&format!("scrolled to row {row}"), cx);
            if index == 1 {
                feed(cx, &|feed, cx| feed.drive_more(true, cx));
                wait(pause).await;
                report("more open mid-page", cx);
                shot("04-mid-page-more-open");
                feed(cx, &|feed, cx| feed.drive_more(false, cx));
            }
        }
        let topics = this
            .update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_topics()))
            .ok()
            .flatten()
            .unwrap_or_default();
        for topic in topics.iter().take(3) {
            let key = topic.clone();
            feed(cx, &move |feed, cx| feed.drive_topic(Some(key.clone()), cx));
            wait(pause).await;
            report(&format!("topic {topic}"), cx);
        }
        shot("05-topic");
        feed(cx, &|feed, cx| feed.drive_topic(None, cx));
        let away: u64 = std::env::var("FILO_DRIVE_AWAY").ok().and_then(|v| v.parse().ok()).unwrap_or(35);
        this.update_in(cx, |shell, window, cx| {
            shell.open_tab(Tab::Notes, window, cx);
            shell.close_tab_view(Tab::Feed);
        })
        .ok();
        wait(away * 1000).await;
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Feed, window, cx)).ok();
        wait(400).await;
        report("reopened", cx);
        shot("08-reopened");
        settle(&this, cx).await;
        report("reopened, settled", cx);
        shot("09-reopened-settled");
        eprintln!("feed-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// A drive: what `FILO_OPEN=<name>` (or `<name>:<id>`, its id the first argument) starts.
type Drive = fn(Option<String>, &mut Window, &mut Context<AppShell>);

/// The drives `open_from_env` starts by name. They click and type through the window like a person (in-app AppKit
/// events, `native_input`), so while one runs the elements it looks for record where they are (`driving`).
const DRIVES: [(&str, Drive); 41] = [
    ("shared-delete-drive", |_, window, cx| shared_delete::run(window, cx)),
    ("shared-show-drive", |_, window, cx| shared_show::run(window, cx)),
    ("list-reorder-timing", |_, window, cx| list_reorder::run(window, cx)),
    ("feed-drive", |_, window, cx| run_feed_drive(window, cx)),
    ("chat-timing", |_, window, cx| run_chat_timing(window, cx)),
    ("chat-scroll-timing", |_, window, cx| run_chat_scroll_timing(window, cx)),
    ("feed-glance-drive", |_, window, cx| run_feed_glance_drive(window, cx)),
    ("feed-chat", |_, window, cx| run_feed_chat(window, cx)),
    ("feed-refresh", |_, window, cx| run_feed_refresh(window, cx)),
    ("agents-drive", |_, window, cx| run_agents_drive(window, cx)),
    ("welcome-drive", |_, window, cx| run_welcome_drive(window, cx)),
    ("main-agent-drive", |_, window, cx| run_main_agent_drive(window, cx)),
    ("agents-view-drive", |_, window, cx| run_agents_view_drive(window, cx)),
    ("group-optional-drive", |_, window, cx| run_group_optional_drive(window, cx)),
    ("group-setup-scroll-drive", |_, window, cx| run_group_setup_scroll_drive(window, cx)),
    ("signed-in-models-drive", |_, window, cx| run_signed_in_models_drive(window, cx)),
    ("note-share-drive", |_, window, cx| run_note_share_drive(window, cx)),
    ("drafts-drive", |_, window, cx| run_drafts_drive(window, cx)),
    ("chip-mention-drive", |_, window, cx| run_chip_mention_drive(window, cx)),
    ("skill-editor-drive", |_, window, cx| run_skill_editor_drive(window, cx)),
    ("voice-keys-drive", |_, window, cx| run_voice_keys_drive(window, cx)),
    ("desktop-pet-drive", |_, window, cx| run_desktop_pet_drive(window, cx)),
    ("report-drive", |_, window, cx| run_report_drive(window, cx)),
    ("coding-result-drive", run_coding_result_drive),
    ("post-report-drive", run_post_report_drive),
    ("coding-result-share-drive", run_coding_result_share_drive),
    ("artifact-share-drive", run_artifact_share_drive),
    ("artifact-tooltip-drive", run_artifact_tooltip_drive),
    ("overlays-drive", run_overlays_drive),
    ("coding-pickers-drive", |_, window, cx| run_coding_pickers_drive(window, cx)),
    ("coding-update-drive", |_, window, cx| run_coding_update_drive(window, cx)),
    ("pages-bottom-drive", |_, window, cx| run_pages_bottom_drive(window, cx)),
    ("agent-model-drive", |_, window, cx| run_agent_model_drive(window, cx)),
    ("agent-save-keys-drive", |_, window, cx| run_agent_save_keys_drive(window, cx)),
    ("agent-back-drive", |_, window, cx| run_agent_back_drive(window, cx)),
    ("window-keys-drive", |_, window, cx| run_window_keys_drive(window, cx)),
    ("page-keys-drive", |_, window, cx| run_page_keys_drive(window, cx)),
    ("search-drive", |_, window, cx| run_search_drive(window, cx)),
    ("search-button-drive", |_, window, cx| run_search_button_drive(window, cx)),
    ("files-drive", run_files_drive),
    ("reasoning-drive", |_, window, cx| run_reasoning_drive(window, cx)),
];

/// The fixtures (opened from main.rs, not `open_from_env`) that are driven the same way.
const DRIVEN_FIXTURES: [&str; 4] = ["coding-cards", "chat-composer", "feed-fixture", "feed-settings-fixture"];

/// Whether `FILO_OPEN` names a drive or a driven fixture (before any `:<id>`).
pub(super) fn driving() -> bool {
    static DRIVING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DRIVING.get_or_init(|| {
        std::env::var("FILO_OPEN").is_ok_and(|open| {
            let name = open.split_once(':').map_or(open.as_str(), |(name, _)| name);
            DRIVES.iter().any(|(drive, _)| *drive == name) || DRIVEN_FIXTURES.contains(&name)
        })
    })
}

/// Input for Feed checks: AppKit events built and posted inside this app (`NSApp postEvent:`), so they go through
/// AppKit and GPUI's event handling exactly like real clicks and typing, to the window `width` points wide, without
/// moving the user's pointer or needing the Accessibility permission an outside event poster would.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2's macros
pub(crate) mod native_input {
    use objc::runtime::{BOOL, NO, Object, YES};
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }

    const LEFT_DOWN: u64 = 1;
    const LEFT_UP: u64 = 2;
    const KEY_DOWN: u64 = 10;
    const KEY_UP: u64 = 11;
    pub const CONTROL: u64 = 1 << 18;
    pub const COMMAND: u64 = 1 << 20;
    pub const SHIFT: u64 = 1 << 17;
    pub const FUNCTION: u64 = 1 << 23;

    /// The window `width` points wide, its number and content height.
    unsafe fn window(width: f64) -> Option<(*mut Object, isize, f64)> {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let windows: *mut Object = msg_send![app, windows];
            let count: usize = msg_send![windows, count];
            for index in 0..count {
                let window: *mut Object = msg_send![windows, objectAtIndex: index];
                let frame: Rect = msg_send![window, frame];
                if (frame.size.width - width).abs() < 1. {
                    let number: isize = msg_send![window, windowNumber];
                    let view: *mut Object = msg_send![window, contentView];
                    let bounds: Rect = msg_send![view, bounds];
                    return Some((window, number, bounds.size.height));
                }
            }
            None
        }
    }

    unsafe fn uptime() -> f64 {
        unsafe {
            let info: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
            msg_send![info, systemUptime]
        }
    }

    unsafe fn post(event: *mut Object) {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, postEvent: event atStart: NO];
        }
    }

    /// Whether this app is the active one and the window `width` points wide its key window: posted keys reach the
    /// window only then (another app brought forward takes them).
    pub fn has_keys(width: f64) -> (bool, bool) {
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let active: BOOL = msg_send![app, isActive];
            let key = window(width).is_some_and(|(window, _, _)| {
                let key: BOOL = msg_send![window, isKeyWindow];
                key == YES
            });
            (active == YES, key)
        }
    }

    /// The name of the app in front (another app taking the focus while keys are posted).
    pub fn frontmost() -> String {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            let app: *mut Object = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return "none".into();
            }
            let pid: i32 = msg_send![app, processIdentifier];
            let name: *mut Object = msg_send![app, localizedName];
            let name = if name.is_null() {
                "unnamed".into()
            } else {
                let text: *const std::ffi::c_char = msg_send![name, UTF8String];
                std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned()
            };
            let mine = if pid == std::process::id() as i32 { ", this app" } else { "" };
            format!("{name} (pid {pid}{mine})")
        }
    }

    /// The window `width` points wide: its number, for `window_shot::save`.
    pub fn window_number(width: f64) -> Option<isize> {
        unsafe { window(width).map(|(_, number, _)| number) }
    }

    /// A right click at (x, y), window points from the top left (opens a context menu).
    pub fn right_click(width: f64, x: f64, y: f64) -> bool {
        const RIGHT_DOWN: u64 = 3;
        const RIGHT_UP: u64 = 4;
        unsafe {
            let Some((_, number, height)) = window(width) else { return false };
            let location = Point { x, y: height - y };
            #[link(name = "ApplicationServices", kind = "framework")]
            unsafe extern "C" {
                fn CGEventCreateCopy(event: *const std::ffi::c_void) -> *mut std::ffi::c_void;
                fn CGEventSetIntegerValueField(event: *mut std::ffi::c_void, field: u32, value: i64);
                fn CFRelease(value: *const std::ffi::c_void);
            }
            // kCGMouseEventButtonNumber: an NSEvent made with a right-button type still says button 0, which GPUI reads
            // as the left button.
            const BUTTON_NUMBER: u32 = 3;
            for kind in [RIGHT_DOWN, RIGHT_UP] {
                let event: *mut Object = msg_send![class!(NSEvent), mouseEventWithType: kind location: location modifierFlags: 0u64 timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() eventNumber: 0isize clickCount: 1isize pressure: 1.0f32];
                if event.is_null() {
                    return false;
                }
                let cg: *const std::ffi::c_void = msg_send![event, CGEvent];
                let copy = CGEventCreateCopy(cg);
                if copy.is_null() {
                    return false;
                }
                CGEventSetIntegerValueField(copy, BUTTON_NUMBER, 1);
                let right: *mut Object = msg_send![class!(NSEvent), eventWithCGEvent: copy];
                CFRelease(copy);
                if right.is_null() {
                    return false;
                }
                post(right);
            }
            true
        }
    }

    /// One step of a drag with the left button at (x, y), window points from the top left: `Down`, any number of
    /// `Drag`s, `Up`. Give the app a frame between them: a text view starts following the pointer on the frame after
    /// the button goes down.
    pub fn drag_step(width: f64, step: Drag, x: f64, y: f64) -> bool {
        unsafe {
            let Some((_, number, height)) = window(width) else { return false };
            let kind = match step {
                Drag::Down => LEFT_DOWN,
                Drag::Move => 6,
                Drag::Up => LEFT_UP,
            };
            let location = Point { x, y: height - y };
            let event: *mut Object = msg_send![class!(NSEvent), mouseEventWithType: kind location: location modifierFlags: 0u64 timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() eventNumber: 0isize clickCount: 1isize pressure: 1.0f32];
            if event.is_null() {
                return false;
            }
            post(event);
            true
        }
    }

    pub enum Drag {
        Down,
        Move,
        Up,
    }

    /// The pointer moved to (x, y), window points from the top left, no button down (hovers what's there).
    pub fn hover(width: f64, x: f64, y: f64) -> bool {
        const MOUSE_MOVED: u64 = 5;
        unsafe {
            let Some((window, number, height)) = window(width) else { return false };
            // A pop-up window (the fixtures') hears the real pointer through a tracking area only, and AppKit drops a
            // posted move for a window that doesn't take them.
            let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
            let location = Point { x, y: height - y };
            let event: *mut Object = msg_send![class!(NSEvent), mouseEventWithType: MOUSE_MOVED location: location modifierFlags: 0u64 timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() eventNumber: 0isize clickCount: 0isize pressure: 0.0f32];
            if event.is_null() {
                return false;
            }
            post(event);
            true
        }
    }

    /// A left click at (x, y), window points from the top left.
    pub fn click(width: f64, x: f64, y: f64) -> bool {
        click_with(width, x, y, 0)
    }

    /// `click` with keys held (`flags`: `COMMAND`, …): a ⌘-click or a Shift-click.
    pub fn click_with(width: f64, x: f64, y: f64, flags: u64) -> bool {
        unsafe {
            let Some((_, number, height)) = window(width) else { return false };
            let location = Point { x, y: height - y };
            for kind in [LEFT_DOWN, LEFT_UP] {
                let event: *mut Object = msg_send![class!(NSEvent), mouseEventWithType: kind location: location modifierFlags: flags timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() eventNumber: 0isize clickCount: 1isize pressure: 1.0f32];
                if event.is_null() {
                    return false;
                }
                post(event);
            }
            true
        }
    }

    /// A double-click at (x, y), window points from the top left: two clicks, the second counted as such.
    pub fn double_click(width: f64, x: f64, y: f64) -> bool {
        unsafe {
            let Some((_, number, height)) = window(width) else { return false };
            let location = Point { x, y: height - y };
            for (kind, count) in [(LEFT_DOWN, 1isize), (LEFT_UP, 1), (LEFT_DOWN, 2), (LEFT_UP, 2)] {
                let event: *mut Object = msg_send![class!(NSEvent), mouseEventWithType: kind location: location modifierFlags: 0u64 timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() eventNumber: 0isize clickCount: count pressure: 1.0f32];
                if event.is_null() {
                    return false;
                }
                post(event);
            }
            true
        }
    }

    /// One key: `characters` as typed, `code` the virtual key code (36 Return, 51 Delete), with `flags`.
    pub fn key(width: f64, characters: &str, code: u16, flags: u64) -> bool {
        unsafe {
            let Some((window, number, _)) = window(width) else { return false };
            // Keys go to the key window: make it the app's key window (the app itself isn't brought forward).
            let _: () = msg_send![window, makeKeyWindow];
            let text = std::ffi::CString::new(characters).unwrap_or_default();
            let string: *mut Object = msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()];
            for kind in [KEY_DOWN, KEY_UP] {
                let event: *mut Object = msg_send![class!(NSEvent), keyEventWithType: kind location: Point { x: 0., y: 0. } modifierFlags: flags timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() characters: string charactersIgnoringModifiers: string isARepeat: NO keyCode: code];
                if event.is_null() {
                    return false;
                }
                post(event);
            }
            true
        }
    }

    /// The modifier keys changing to `flags` (only those down; 0 lets all go), as pressing or letting go of one does.
    pub fn flags(width: f64, flags: u64) -> bool {
        const FLAGS_CHANGED: u64 = 12;
        unsafe {
            let Some((window, number, _)) = window(width) else { return false };
            let _: () = msg_send![window, makeKeyWindow];
            let empty: *mut Object = msg_send![class!(NSString), string];
            // 63 is fn's key code; GPUI reads only the flags.
            let event: *mut Object = msg_send![class!(NSEvent), keyEventWithType: FLAGS_CHANGED location: Point { x: 0., y: 0. } modifierFlags: flags timestamp: uptime() windowNumber: number context: std::ptr::null_mut::<Object>() characters: empty charactersIgnoringModifiers: empty isARepeat: NO keyCode: 63u16];
            if event.is_null() {
                return false;
            }
            post(event);
            true
        }
    }

    /// GPUI's view in `window`: in its content view, which holds nothing else.
    unsafe fn gpui_view(window: *mut Object) -> Option<*mut Object> {
        unsafe {
            let class = objc::runtime::Class::get("GPUIView")?;
            let content: *mut Object = msg_send![window, contentView];
            let subviews: *mut Object = msg_send![content, subviews];
            let count: usize = msg_send![subviews, count];
            (0..count).map(|index| -> *mut Object { msg_send![subviews, objectAtIndex: index] }).find(|view| {
                let gpui: BOOL = msg_send![*view, isKindOfClass: class];
                gpui == YES
            })
        }
    }

    /// A blank web view put in GPUI's view at (x, y, w, h), window points from the top left, as a glance, an artifact
    /// or the note editor puts theirs. The window made key, as a person's click would.
    pub fn add_page(width: f64, (x, y, w, h): (f64, f64, f64, f64)) -> Option<usize> {
        unsafe {
            let (window, _, height) = window(width)?;
            let _: () = msg_send![window, makeKeyWindow];
            let view = gpui_view(window)?;
            let flipped: BOOL = msg_send![view, isFlipped];
            let frame = Rect {
                origin: Point { x, y: if flipped == YES { y } else { height - y - h } },
                size: Size { width: w, height: h },
            };
            let page: *mut Object = msg_send![class!(WKWebView), alloc];
            let page: *mut Object = msg_send![page, initWithFrame: frame];
            if page.is_null() {
                return None;
            }
            let _: () = msg_send![view, addSubview: page];
            Some(page as usize)
        }
    }

    /// The page hidden, as a glance's is when it's scrolled out of the chat.
    pub fn hide_page(page: usize) {
        unsafe {
            let _: () = msg_send![page as *mut Object, setHidden: YES];
        }
    }

    /// The page taken out of the window and freed, as a glance's or an artifact's is when its chat or page closes.
    pub fn remove_page(page: usize) {
        let page = page as *mut Object;
        unsafe {
            let _: () = msg_send![page, removeFromSuperview];
            let _: () = msg_send![page, release];
        }
    }

    /// What has the keyboard of the window `width` points wide: "page" (a web view or a view inside one), "window" (the
    /// window itself: no view does), "view" (GPUI's) or another class's name.
    pub fn keyboard(width: f64) -> String {
        unsafe {
            let Some((window, _, _)) = window(width) else { return "no window".into() };
            let responder: *mut Object = msg_send![window, firstResponder];
            let view = gpui_view(window).unwrap_or(std::ptr::null_mut());
            if responder.is_null() {
                return "nothing".into();
            }
            if responder == window {
                return "window".into();
            }
            if responder == view {
                return "view".into();
            }
            let mut ancestor = responder;
            while !ancestor.is_null() {
                let page: BOOL = msg_send![ancestor, isKindOfClass: class!(WKWebView)];
                if page == YES {
                    return "page".into();
                }
                ancestor = msg_send![ancestor, superview];
            }
            let class: *mut Object = msg_send![responder, className];
            let text: *const std::ffi::c_char = msg_send![class, UTF8String];
            std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned()
        }
    }

    /// At (x, y), window points from the top left of the window `width` points wide: the native view that shows there
    /// (a subview of GPUI's view: a web view, a glance's clip view, a video), its class and pointer; and what AppKit
    /// sends a click there to: "view" (GPUI's), or the class of the native view that takes it.
    pub fn native_at(width: f64, x: f64, y: f64) -> (Option<(String, usize)>, String) {
        unsafe fn class_name(object: *mut Object) -> String {
            unsafe {
                let class: *mut Object = msg_send![object, className];
                let text: *const std::ffi::c_char = msg_send![class, UTF8String];
                std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        }
        unsafe {
            let Some((window, _, height)) = window(width) else { return (None, "no window".into()) };
            let Some(gpui) = gpui_view(window) else { return (None, "no view".into()) };
            let base = Point { x, y: height - y };
            let null = std::ptr::null_mut::<Object>();
            let local: Point = msg_send![gpui, convertPoint: base fromView: null];
            let subviews: *mut Object = msg_send![gpui, subviews];
            let count: usize = msg_send![subviews, count];
            let mut native = None;
            for index in (0..count).rev() {
                let view: *mut Object = msg_send![subviews, objectAtIndex: index];
                let hidden: BOOL = msg_send![view, isHidden];
                let frame: Rect = msg_send![view, frame];
                let inside = local.x >= frame.origin.x
                    && local.x < frame.origin.x + frame.size.width
                    && local.y >= frame.origin.y
                    && local.y < frame.origin.y + frame.size.height;
                if hidden == NO && inside {
                    native = Some((class_name(view), view as usize));
                    break;
                }
            }
            let content: *mut Object = msg_send![window, contentView];
            let superview: *mut Object = msg_send![content, superview];
            let point: Point = msg_send![superview, convertPoint: base fromView: null];
            let hit: *mut Object = msg_send![content, hitTest: point];
            let taker = if hit.is_null() {
                "nothing".into()
            } else if hit == gpui {
                "view".into()
            } else {
                let mut view = hit;
                loop {
                    let parent: *mut Object = msg_send![view, superview];
                    if parent.is_null() {
                        break class_name(hit);
                    }
                    if parent == gpui {
                        break class_name(view);
                    }
                    view = parent;
                }
            };
            (native, taker)
        }
    }

    /// Gives the keyboard to `page`, or with none to the window's view (GPUI's), as it has when the window opens.
    pub fn give_keyboard(width: f64, page: Option<usize>) {
        unsafe {
            let Some((window, _, _)) = window(width) else { return };
            let view: *mut Object = match page {
                Some(page) => page as *mut Object,
                None => {
                    let Some(view) = gpui_view(window) else { return };
                    view
                }
            };
            let _: BOOL = msg_send![window, makeFirstResponder: view];
        }
    }

    /// Scrolls by `dy` points (down is positive) at (x, y), window points from the top left, in the window `width`
    /// points wide even when it isn't key or under the pointer (a pop-up fixture window): the wheel event names it.
    pub fn scroll_in(width: f64, x: f64, y: f64, dy: f64) -> bool {
        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn CGEventCreateScrollWheelEvent(
                source: *const std::ffi::c_void,
                units: u32,
                count: u32,
                wheel1: i32,
                ...
            ) -> *mut std::ffi::c_void;
            fn CGEventSetLocation(event: *mut std::ffi::c_void, location: Point);
            fn CGEventSetIntegerValueField(event: *mut std::ffi::c_void, field: u32, value: i64);
            fn CFRelease(value: *const std::ffi::c_void);
        }
        // kCGMouseEventWindowUnderMousePointer and …ThatCanHandleThisEvent.
        const UNDER_POINTER: u32 = 91;
        const CAN_HANDLE: u32 = 92;
        unsafe {
            let Some((window, number, height)) = window(width) else { return false };
            // Global points from the top left of the main screen, as CoreGraphics counts them.
            let frame: Rect = msg_send![window, frame];
            let screens: *mut Object = msg_send![class!(NSScreen), screens];
            let main: *mut Object = msg_send![screens, objectAtIndex: 0usize];
            let screen: Rect = msg_send![main, frame];
            let location = Point { x: frame.origin.x + x, y: screen.size.height - (frame.origin.y + height) + y };
            let cg = CGEventCreateScrollWheelEvent(std::ptr::null(), 0, 1, (-dy) as i32);
            if cg.is_null() {
                return false;
            }
            CGEventSetLocation(cg, location);
            CGEventSetIntegerValueField(cg, UNDER_POINTER, number as i64);
            CGEventSetIntegerValueField(cg, CAN_HANDLE, number as i64);
            let event: *mut Object = msg_send![class!(NSEvent), eventWithCGEvent: cg];
            CFRelease(cg);
            if event.is_null() {
                return false;
            }
            let _: () = msg_send![window, sendEvent: event];
            true
        }
    }

    /// Scrolls by `dy` points (down is positive) at the screen point (x, y), top-left origin: a pixel-precise wheel
    /// event made with CoreGraphics, posted through AppKit.
    pub fn scroll(width: f64, x: f64, y: f64, dy: f64) -> bool {
        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn CGEventCreateScrollWheelEvent(
                source: *const std::ffi::c_void,
                units: u32,
                count: u32,
                wheel1: i32,
                ...
            ) -> *mut std::ffi::c_void;
            fn CGEventSetLocation(event: *mut std::ffi::c_void, location: Point);
            fn CFRelease(value: *const std::ffi::c_void);
        }
        unsafe {
            if window(width).is_none() {
                return false;
            }
            // Scrolled in steps, as a trackpad does.
            let steps = (dy.abs() / 100.).ceil().max(1.) as i32;
            for _ in 0..steps {
                let cg = CGEventCreateScrollWheelEvent(std::ptr::null(), 0, 1, (-dy / steps as f64) as i32);
                if cg.is_null() {
                    return false;
                }
                CGEventSetLocation(cg, Point { x, y });
                let event: *mut Object = msg_send![class!(NSEvent), eventWithCGEvent: cg];
                CFRelease(cg);
                if event.is_null() {
                    return false;
                }
                post(event);
            }
            true
        }
    }
}

/// The window's width (to find its AppKit window) and origin on screen.
fn window_width(this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext) -> Option<f64> {
    this.update_in(cx, |_, window, _| f64::from(f32::from(window.bounds().size.width))).ok()
}

/// `id`'s bounds if the window's latest frame laid it out (rows scrolled away keep older ones).
pub(crate) fn on_screen(id: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    let bounds = DRIVE_BOUNDS.lock().unwrap();
    let latest = bounds.values().map(|(_, at)| *at).max()?;
    bounds.get(id).filter(|(_, at)| latest.duration_since(*at) < Duration::from_millis(250)).map(|(bounds, _)| *bounds)
}

/// A left click at the middle of `id` (in the latest frame).
fn click(this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, id: &str) -> bool {
    let Some(bounds) = on_screen(id) else {
        eprintln!("click: {id} isn't on screen");
        return false;
    };
    let Some(width) = window_width(this, cx) else { return false };
    let center = bounds.center();
    native_input::click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)))
}

/// Types `text` into whatever has the keyboard in the window, then (with `enter`) presses Return, so key bindings
/// (Enter sends) apply.
fn type_keys(this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, text: &str, enter: bool) {
    let Some(width) = window_width(this, cx) else { return };
    for character in text.chars() {
        native_input::key(width, &character.to_string(), 0, 0);
    }
    if enter {
        native_input::key(width, "\r", 36, 0);
    }
}

/// ⌘A then Delete: clears the field with the keyboard.
fn clear_field(this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext) {
    let Some(width) = window_width(this, cx) else { return };
    native_input::key(width, "a", 0, native_input::COMMAND);
    native_input::key(width, "\u{7f}", 51, 0);
}

/// `<FILO_DRIVE_SHOTS>/<name>.png` (or `FILO_FIXTURE_SHOTS`), when that's set.
fn shot(name: &str) {
    crate::window_shot::save_step(name);
}

fn feed_screen(
    this: &gpui::WeakEntity<AppShell>,
    cx: &mut gpui::AsyncWindowContext,
) -> Option<gpui::Entity<crate::screens::feed::FeedScreen>> {
    this.update(cx, |shell, _| shell.feed_screen()).ok().flatten()
}

/// A scroll of `dy` points (up is negative) over `position`, through the window's input handling like a trackpad.
fn scroll_wheel(
    this: &gpui::WeakEntity<AppShell>,
    cx: &mut gpui::AsyncWindowContext,
    position: gpui::Point<gpui::Pixels>,
    dy: f32,
) {
    let Some(width) = window_width(this, cx) else { return };
    let Ok(origin) = this.update_in(cx, |_, window, _| window.bounds().origin) else { return };
    native_input::scroll(
        width,
        f64::from(f32::from(origin.x + position.x)),
        f64::from(f32::from(origin.y + position.y)),
        f64::from(dy),
    );
}

/// The first of `ids` with a tracked element `{prefix}{id}` on screen.
fn first_on_screen(prefix: &str, ids: &[String]) -> Option<String> {
    ids.iter().find(|id| on_screen(&format!("{prefix}{id}")).is_some()).cloned()
}

/// `FILO_OPEN=main-agent-drive`: the main agent picked on the Agents tab. Right-clicks the first agent that isn't main
/// (in-app AppKit events, `native_input`), shows its menu (Make main agent), closes it, makes that agent main as the
/// menu item does, and logs which row has the Main badge before and after, with window screenshots in
/// `FILO_DRIVE_SHOTS`. Needs two agents or more.
fn run_main_agent_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-agents".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("main-agent-drive: shot {path}");
            }
        };
        wait(6000).await;
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        wait(3000).await;
        let screen =
            |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let rows = |cx: &mut gpui::AsyncWindowContext| {
            screen(cx).and_then(|agents| agents.read_with(cx, |agents, _| agents.drive_rows()).ok()).unwrap_or_default()
        };
        let main = |rows: &[(String, String, bool)]| {
            rows.iter().filter(|(_, _, main)| *main).map(|(_, name, _)| name.clone()).collect::<Vec<_>>()
        };
        let Some(width) = window_width(&this, cx) else { return eprintln!("main-agent-drive: no window") };
        let before = rows(cx);
        eprintln!(
            "main-agent-drive: agents {:?}, Main badge on {:?}",
            before.iter().map(|(_, name, _)| name).collect::<Vec<_>>(),
            main(&before)
        );
        snap(width, "main-01-before");
        let Some((id, name, _)) = before.iter().find(|(_, _, main)| !main).cloned() else {
            return eprintln!("main-agent-drive: FAIL no agent that isn't main");
        };
        let Some(bounds) = on_screen(&format!("agent-row-{id}")) else {
            return eprintln!("main-agent-drive: FAIL {name}'s row isn't on screen");
        };
        let center = bounds.center();
        let clicked = native_input::right_click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
        wait(800).await;
        eprintln!("main-agent-drive: right-clicked {name} ({clicked})");
        snap(width, "main-02-menu");
        native_input::key(width, "\u{1b}", 53, 0);
        wait(400).await;
        let Some(agents) = screen(cx) else { return };
        this.update_in(cx, |_, window, cx| {
            agents.update(cx, |agents, cx| agents.drive_make_main(id.clone(), window, cx))
        })
        .ok();
        wait(1500).await;
        let after = rows(cx);
        let moved = main(&after) == [name.clone()];
        eprintln!(
            "main-agent-drive: made {name} main: Main badge on {:?} ({})",
            main(&after),
            if moved { "PASS" } else { "FAIL" }
        );
        snap(width, "main-03-after");
        eprintln!("main-agent-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=agents-view-drive`: the ⌄ after the Agents list's title. Logs whether the tool marks are on and the
/// saved settings, clicks the ⌄ (in-app AppKit events, `native_input`), picks "Show tool logos" and logs both again,
/// with window screenshots in `FILO_DRIVE_SHOTS`. A second launch on the same config logs at start what the first one
/// left.
fn run_agents_view_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-agents".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("agents-view-drive: shot {path}");
            }
        };
        let settings =
            || std::fs::read_to_string(crate::config::config_dir().join("desktop-agents.json")).unwrap_or_default();
        wait(6000).await;
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        wait(3000).await;
        let screen =
            |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let marks = |cx: &mut gpui::AsyncWindowContext| {
            screen(cx).and_then(|agents| agents.read_with(cx, |agents, _| agents.tool_marks()).ok())
        };
        let Some(width) = window_width(&this, cx) else { return eprintln!("agents-view-drive: no window") };
        let before = marks(cx);
        eprintln!("agents-view-drive: at start marks {before:?}, settings {}", settings().trim());
        snap(width, "view-01-start");
        // The list drawn afresh, so its ⌄'s place is as new as any other's.
        if let Some(agents) = screen(cx) {
            agents.update(cx, |_, cx| cx.notify()).ok();
        }
        wait(200).await;
        let Some(bounds) = on_screen("agents-view-menu") else {
            return eprintln!("agents-view-drive: FAIL the ⌄ isn't on screen");
        };
        let center = bounds.center();
        let clicked = native_input::click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
        wait(800).await;
        eprintln!("agents-view-drive: clicked the ⌄ ({clicked})");
        snap(width, "view-02-menu");
        // The menu opens under the ⌄, its one item first.
        let (x, y) = (f32::from(bounds.left()) + 80., f32::from(bounds.bottom()) + 22.);
        let picked = native_input::click(width, f64::from(x), f64::from(y));
        eprintln!("agents-view-drive: clicked the item at {x},{y} ({picked})");
        wait(100).await;
        snap(width, "view-03-fading");
        wait(900).await;
        let after = marks(cx);
        let toggled = before.is_some() && after == before.map(|on| !on);
        eprintln!(
            "agents-view-drive: after the pick marks {after:?}, settings {} ({})",
            settings().trim(),
            if toggled { "PASS" } else { "FAIL" }
        );
        snap(width, "view-04-after");
        eprintln!("agents-view-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=group-optional-drive`: the optional group, end to end, in a fresh test config (`FILO_CONFIG_DIR` of its
/// own; it makes its agents on Claude Code's Haiku). With one agent, the group's page is its setup with New agent,
/// which makes the second; then All agents, picked members, the routing step (the Edit agent page's model picker, Haiku picked),
/// Turn on, a post naming no one ("Picking who answers", then whoever the routing model picked answers) and one to
/// @Developer (no routing), Group settings saved, Turn off (asked first; the setup again, filled in) and on again (the
/// marks). Last, with the group off, App Designer asks Developer (the MCP server's ask_agents from its chat): Developer
/// works on it in its own chat, and the answer comes back to App Designer's. Each state is logged and, with
/// `FILO_DRIVE_SHOTS`, pictured. The real turns ask Claude Code; `FILO_GROUP_DRIVE_QUICK` leaves them out.
fn run_group_optional_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::screens::agents::GROUP;
    let backend = crate::backend::backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let quick = std::env::var_os("FILO_GROUP_DRIVE_QUICK").is_some();
        let mut failed = false;
        let mut check = |ok: bool, what: &str| {
            failed |= !ok;
            eprintln!("group-optional-drive: {what} ({})", if ok { "PASS" } else { "FAIL" });
        };
        let screen = |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let state = |cx: &mut gpui::AsyncWindowContext| screen(cx).and_then(|agents| agents.read_with(cx, |agents, cx| agents.drive_group(cx)).ok()).unwrap_or_default();
        // The setup's buttons, as a click on them does.
        let act = |cx: &mut gpui::AsyncWindowContext, action: &str| {
            if let Some(agents) = screen(cx) {
                this.update_in(cx, |_, window, cx| agents.update(cx, |agents, cx| agents.drive_setup(action, window, cx))).ok();
            }
        };
        let agent = |name: &str| json!({ "name": name, "instructions": format!("You are {name}. Answer in one short sentence."), "engine": "claude", "model": "haiku" });
        // Fresh data has the Filo agent alone: it answers on Haiku too, to keep the real turns quick.
        let list = backend.call_raw("agents:list", json!({})).await.ok().and_then(|rows| rows.as_array().cloned()).unwrap_or_default();
        check(list.len() == 1, &format!("one agent to start with: {}", list.len()));
        for row in &list {
            backend.call_raw("agents:update", json!({ "id": row["_id"], "engine": "claude", "model": "haiku" })).await.ok();
        }
        this.update_in(cx, |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.open_agent(GROUP.to_string(), window, cx));
            }
        })
        .ok();
        wait(3000).await;

        // One agent: the setup, with New agent and no way on.
        let (on, setup, _) = state(cx);
        check(!on && setup.as_ref().is_some_and(|(settings, step, members, ..)| !settings && *step == 0 && members.len() == 1), &format!("first open with one agent: {setup:?}"));
        check(on_screen("group-setup-new-agent").is_some(), "New agent row");
        shot("01-one-agent");
        act(cx, "new agent");
        wait(1500).await;
        if let Some(agents) = screen(cx) {
            this.update_in(cx, |_, window, cx| agents.update(cx, |agents, cx| agents.drive_fill_form("Developer", "You are Developer. Answer in one short sentence.", window, cx))).ok();
        }
        wait(800).await;
        shot("02-new-agent-form");
        act(cx, "save agent");
        wait(3000).await;
        let made = backend.call_raw("agents:create", agent("App Designer")).await;
        let designer = made.as_ref().ok().and_then(|id| id.as_str().map(str::to_string)).unwrap_or_default();
        check(!designer.is_empty(), &format!("made App Designer: {made:?}"));
        wait(2000).await;
        let (_, setup, _) = state(cx);
        check(setup.as_ref().is_some_and(|(_, step, members, all, name, _)| *step == 0 && members.len() == 3 && *all && name == "Group"), &format!("back on the setup, the new agent in it: {setup:?}"));
        shot("03-first-open");

        // Picked members: untick one, Continue is off; tick it again.
        act(cx, "all");
        wait(1000).await;
        shot("04-members");
        let ids = backend.call_raw("agents:list", json!({})).await.ok().and_then(|rows| rows.as_array().cloned()).unwrap_or_default();
        let developer = ids.iter().find(|row| row["name"] == "Developer").and_then(|row| row["_id"].as_str()).unwrap_or_default().to_string();
        act(cx, &format!("member:{developer}"));
        act(cx, &format!("member:{designer}"));
        wait(800).await;
        let (_, setup, _) = state(cx);
        check(setup.as_ref().is_some_and(|(_, _, members, all, ..)| !all && members.len() == 1), &format!("one member left: {setup:?}"));
        shot("04b-one-member");
        act(cx, "next");
        wait(800).await;
        check(state(cx).1.is_some_and(|(_, step, ..)| step == 0), "Continue needs two members");
        act(cx, &format!("member:{developer}"));
        act(cx, &format!("member:{designer}"));
        act(cx, "all");
        wait(800).await;

        // The routing step: the model picker, the cheapest fast model picked.
        act(cx, "next");
        wait(3000).await;
        let (_, setup, signed_in) = state(cx);
        check(signed_in && setup.as_ref().is_some_and(|(_, step, _, _, _, route)| *step == 1 && route.is_some()), &format!("routing step, picked {:?}", setup.as_ref().and_then(|setup| setup.5.clone())));
        shot("05-routing");
        wait(500).await;
        act(cx, "commit");
        wait(4000).await;
        let (on, setup, _) = state(cx);
        check(on && setup.is_none(), "turned on: the group's chat");
        shot("07-group-on");
        let session = backend.call_raw("agents:group", json!({})).await.ok().and_then(|row| row["sessionId"].as_str().map(str::to_string)).unwrap_or_default();
        let turns = async |session: &str| backend.call_raw("voiceSessions:chatTurns", json!({ "sessionId": session })).await.ok().and_then(|turns| turns.as_array().cloned()).unwrap_or_default();

        if !quick {
            // A post naming no one: "Picking who answers", then whoever it picked answers.
            backend.call_raw("voiceSessions:sendMessage", json!({ "sessionId": session, "message": "Which of you would design a new settings screen?", "requestId": "drive-route" })).await.ok();
            wait(700).await;
            let picking = on_screen("routing-line").is_some();
            shot("08-picking-who-answers");
            check(picking, "Picking who answers shows");
            let mut answered = false;
            for _ in 0..120 {
                wait(1500).await;
                let rows = turns(&session).await;
                let row = rows.iter().find(|row| row["requestId"] == "drive-route");
                if row.is_some_and(|row| row["activity"] != "Picking who answers" && row["speaker"].is_string()) {
                    eprintln!("group-optional-drive: routing picked {}", row.map(|row| row["speaker"].clone()).unwrap_or_default());
                }
                if row.is_some_and(|row| row["state"] == "completed") {
                    answered = true;
                    break;
                }
            }
            check(answered, "the post was answered");
            wait(1500).await;
            shot("09-routed-answer");
            // @Developer skips routing.
            backend.call_raw("voiceSessions:sendMessage", json!({ "sessionId": session, "message": "@Developer how long would it take?", "requestId": "drive-mention" })).await.ok();
            wait(600).await;
            let rows = turns(&session).await;
            let row = rows.iter().find(|row| row["requestId"] == "drive-mention").cloned().unwrap_or_default();
            check(row["activity"] != "Picking who answers", &format!("@Developer skips routing: {row}"));
            for _ in 0..120 {
                wait(1500).await;
                if turns(&session).await.iter().any(|row| row["requestId"] == "drive-mention" && row["state"] == "completed") {
                    break;
                }
            }
            wait(1500).await;
            shot("10-mention-answer");
        }

        // Group settings, from the ⋯.
        click(&this, cx, "group-menu");
        wait(1000).await;
        shot("11-group-menu");
        if let Some(width) = window_width(&this, cx) {
            native_input::key(width, "\u{1b}", 53, 0);
        }
        wait(500).await;
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_group(window, cx));
            }
        })
        .ok();
        wait(1500).await;
        check(state(cx).1.is_some_and(|(settings, ..)| settings), "Group settings open");
        check(on_screen("agent-model").is_some() && on_screen("group-setup-next").is_none(), "Group settings is one page: the model chip, no Continue");
        shot("12-group-settings");
        act(cx, "commit");
        wait(2000).await;
        check(state(cx).0 && state(cx).1.is_none(), "Group settings saved");

        // Turn off, asked first; the setup again, filled in as it was.
        if let Some(agents) = screen(cx) {
            this.update_in(cx, |_, window, cx| agents.update(cx, |agents, cx| agents.drive_confirm_turn_off(window, cx))).ok();
        }
        wait(1000).await;
        shot("14-turn-off-asks");
        if let Some(agents) = screen(cx) {
            this.update_in(cx, |_, window, cx| {
                use gpui_component::WindowExt as _;
                window.close_dialog(cx);
                agents.update(cx, |agents, cx| agents.drive_turn_off(window, cx));
            })
            .ok();
        }
        wait(3000).await;
        let (on, setup, _) = state(cx);
        check(!on && setup.as_ref().is_some_and(|(settings, _, members, ..)| !settings && members.len() == 3), &format!("off: the setup again, filled in: {setup:?}"));
        check(backend.call_raw("agents:group", json!({})).await.is_ok_and(|row| row.is_null()), "off: no group chat");
        shot("15-off-setup");
        act(cx, "next");
        wait(1500).await;
        act(cx, "commit");
        wait(4000).await;
        let marks = backend.call_raw("voiceSessions:detail", json!({ "id": session })).await.ok().map(|detail| {
            detail["messages"].as_array().into_iter().flatten().filter_map(|row| row["mark"].as_str().map(str::to_string)).collect::<Vec<_>>()
        });
        check(state(cx).0, &format!("on again, posts kept, marks {marks:?}"));
        shot("16-on-again");

        // Off again, App Designer asks Developer from its chat: it works there, and the answer comes back.
        if !quick {
            backend.call_raw("agents:turnOffGroup", json!({})).await.ok();
            let chat = backend.call_raw("agents:openChat", json!({ "id": designer })).await.ok().and_then(|chat| chat.as_str().map(str::to_string)).unwrap_or_default();
            let root = crate::config::config_dir().join("local");
            let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "ask_agents", "arguments": { "message": "Does the group picture still read at 24 px? One line.", "ask": ["Developer"] } } });
            let program = std::env::current_exe().unwrap_or_default();
            let asked = std::process::Command::new(program)
                .args(["--local-mcp", "--chat", &chat, "--dir"])
                .arg(root.join("agents"))
                .arg("--db")
                .arg(root.join(filo_local::db::FILE))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write as _;
                    child.stdin.take().map(|mut stdin| writeln!(stdin, "{request}")).transpose()?;
                    child.wait_with_output()
                });
            eprintln!("group-optional-drive: ask_agents said {:?}", asked.map(|output| String::from_utf8_lossy(&output.stdout).into_owned()));
            this.update_in(cx, |shell, window, cx| shell.open_agent(designer.clone(), window, cx)).ok();
            let mut back = false;
            for _ in 0..200 {
                wait(1500).await;
                let messages = backend.call_raw("voiceSessions:detail", json!({ "id": chat })).await.ok().and_then(|detail| detail["messages"].as_array().cloned()).unwrap_or_default();
                if messages.iter().any(|row| row["groupLink"]["kind"] == "answered") && messages.last().is_some_and(|row| row["role"] == "assistant") {
                    back = true;
                    break;
                }
            }
            check(back, "the answer came back to App Designer's chat, and it passed it on");
            wait(2000).await;
            shot("17-asker-chat");
            this.update_in(cx, |shell, window, cx| shell.open_agent(developer.clone(), window, cx)).ok();
            wait(3000).await;
            shot("18-asked-chat");
        }
        eprintln!("group-optional-drive: done{}", if failed { " (FAILED)" } else { "" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=group-setup-scroll-drive`: the group's setup with 18 agents ticked one by one, in a fresh test config. It
/// opens at the top, the picture and name in view; the step scrolls as one area to the last member, the dots and
/// buttons staying put; the routing step, short, stays centred, with the Edit agent page's model picker (Haiku picked,
/// then Sonnet) and the composer's reasoning picker right after it (Low, then Medium, saved with the model); Turn off's
/// dialog has no gap under its title; and Group settings is one page from the top (the chips under the name, Sonnet and
/// Medium kept, the members below, only Cancel and Save) that scrolls as one to the last member.
fn run_group_setup_scroll_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::screens::agents::GROUP;
    let backend = crate::backend::backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let mut failed = false;
        let mut check = |ok: bool, what: &str| {
            failed |= !ok;
            eprintln!("group-setup-scroll-drive: {what} ({})", if ok { "PASS" } else { "FAIL" });
        };
        let screen = |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let act = |cx: &mut gpui::AsyncWindowContext, action: &str| {
            if let Some(agents) = screen(cx) {
                this.update_in(cx, |_, window, cx| agents.update(cx, |agents, cx| agents.drive_setup(action, window, cx))).ok();
            }
        };
        for index in 1..18 {
            let name = format!("Agent {index}");
            backend.call_raw("agents:create", json!({ "name": name, "instructions": "Answer in one short sentence.", "engine": "claude", "model": "haiku" })).await.ok();
        }
        let ids: Vec<String> = backend.call_raw("agents:list", json!({})).await.ok().and_then(|rows| rows.as_array().cloned()).unwrap_or_default().iter().filter_map(|row| row["_id"].as_str().map(str::to_string)).collect();
        check(ids.len() == 18, &format!("18 agents: {}", ids.len()));
        this.update_in(cx, |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.open_agent(GROUP.to_string(), window, cx));
            }
        })
        .ok();
        wait(3000).await;
        // All agents off: each agent ticked, the step taller than the window.
        act(cx, "all");
        wait(1500).await;
        let header = crate::theme::HEADER_HEIGHT;
        let footer_top = on_screen("group-setup-next").map(|bounds| f32::from(bounds.top())).unwrap_or(0.);
        let lowest = || ids.iter().filter_map(|id| on_screen(&format!("group-setup-member-{id}"))).map(|bounds| f32::from(bounds.bottom())).fold(0f32, f32::max);
        let all_top = on_screen("group-setup-all").map(|bounds| f32::from(bounds.top()));
        // The picture and name stand above All agents.
        check(all_top.is_some_and(|top| top > header + 60.), &format!("opens at the top, picture and name in view: All agents at {all_top:?}"));
        check(lowest() > footer_top, &format!("the step is taller than its area: last member at {}, buttons at {footer_top}", lowest()));
        shot("01-many-top");
        if let Some(width) = window_width(&this, cx) {
            for _ in 0..6 {
                native_input::scroll_in(width, width * 0.7, 400., 800.);
                wait(200).await;
            }
        }
        wait(1000).await;
        let footer_after = on_screen("group-setup-next").map(|bounds| f32::from(bounds.top())).unwrap_or(0.);
        check(lowest() <= footer_after && lowest() > footer_after - 120., &format!("scrolls to the last member: at {}, buttons at {footer_after}", lowest()));
        check((footer_after - footer_top).abs() < 1., "the dots and buttons stay put");
        shot("02-many-bottom");
        // The routing step: short, centred; the Edit agent page's model picker, Haiku picked.
        act(cx, "next");
        wait(3000).await;
        let state = |cx: &mut gpui::AsyncWindowContext| screen(cx).and_then(|agents| agents.read_with(cx, |agents, cx| agents.drive_group(cx)).ok()).unwrap_or_default();
        let picked = state(cx).1.and_then(|setup| setup.5);
        check(picked.as_deref().is_some_and(|route| route.starts_with("claude:") && route.contains("haiku")), &format!("Haiku picked: {picked:?}"));
        check(on_screen("agent-model").is_some(), "the model chip shows");
        shot("03-routing-centred");
        click(&this, cx, "agent-model");
        wait(1200).await;
        shot("03b-model-menu");
        if let Some(width) = window_width(&this, cx) {
            native_input::key(width, "\u{1b}", 53, 0);
        }
        wait(500).await;
        let sonnet = cx.update(|_, cx| crate::screens::chat::conversation::model_rows(cx).iter().find(|row| row.engine == filo_local::Engine::Claude && row.id.contains("sonnet")).map(|row| row.id.clone())).ok().flatten();
        if let Some(sonnet) = &sonnet {
            act(cx, &format!("route:claude:{sonnet}"));
        }
        wait(800).await;
        let picked = state(cx).1.and_then(|setup| setup.5);
        check(sonnet.is_some() && picked == sonnet.as_ref().map(|id| format!("claude:{id}")), &format!("Sonnet picked: {picked:?}"));
        shot("03c-sonnet-picked");
        // How hard it thinks: the composer's reasoning picker right after the model chip, Low to start with.
        let reasoning = |cx: &mut gpui::AsyncWindowContext| screen(cx).and_then(|agents| agents.update(cx, |agents, cx| agents.drive_route_reasoning(cx)).ok()).flatten();
        check(reasoning(cx) == Some("low"), &format!("the reasoning chip has Low: {:?}", reasoning(cx)));
        let (model_chip, reasoning_chip) = (on_screen("agent-model"), on_screen("route-reasoning"));
        check(
            model_chip.zip(reasoning_chip).is_some_and(|(model, chip)| chip.left() > model.right() && chip.left() - model.right() < gpui::px(16.) && (chip.center().y - model.center().y).abs() < gpui::px(2.)),
            &format!("the reasoning chip is right after the model chip: {model_chip:?} {reasoning_chip:?}"),
        );
        click(&this, cx, "route-reasoning");
        wait(1000).await;
        shot("03d-reasoning-menu");
        if let Some(width) = window_width(&this, cx) {
            native_input::key(width, "\u{1b}", 53, 0);
        }
        wait(500).await;
        act(cx, "reasoning:medium");
        wait(800).await;
        check(reasoning(cx) == Some("medium"), &format!("Medium picked: {:?}", reasoning(cx)));
        shot("03e-medium-picked");
        act(cx, "commit");
        wait(4000).await;
        if let Some(agents) = screen(cx) {
            this.update_in(cx, |_, window, cx| agents.update(cx, |agents, cx| agents.drive_confirm_turn_off(window, cx))).ok();
        }
        wait(1000).await;
        shot("04-turn-off-dialog");
        this.update_in(cx, |_, window, cx| {
            use gpui_component::WindowExt as _;
            window.close_dialog(cx);
        })
        .ok();
        wait(500).await;
        // Group settings: one page from the top, the model and reasoning chips under the name, the members below, only
        // Cancel and Save under it.
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_group(window, cx));
            }
        })
        .ok();
        wait(1500).await;
        let (model_chip, reasoning_chip, all_row) = (on_screen("agent-model"), on_screen("route-reasoning"), on_screen("group-setup-all"));
        check(
            model_chip.zip(all_row).is_some_and(|(model, all)| model.top() > gpui::px(header + 20.) && model.bottom() < all.top()),
            &format!("Group settings opens at the top, the model chip above the members: {model_chip:?} {all_row:?}"),
        );
        check(
            model_chip.zip(reasoning_chip).is_some_and(|(model, chip)| chip.left() > model.right() && (chip.center().y - model.center().y).abs() < gpui::px(2.)),
            "the reasoning chip is right after the model chip",
        );
        check(
            on_screen("group-setup-dot-0").is_none() && on_screen("group-setup-next").is_none() && on_screen("group-setup-cancel").is_some() && on_screen("group-setup-commit").is_some(),
            "the footer has only Cancel and Save",
        );
        shot("05-settings-top");
        let footer = on_screen("group-setup-commit").map(|bounds| f32::from(bounds.top())).unwrap_or(0.);
        check(lowest() > footer, &format!("the members go past the footer: last at {}, buttons at {footer}", lowest()));
        if let Some(width) = window_width(&this, cx) {
            for _ in 0..6 {
                native_input::scroll_in(width, width * 0.7, 400., 800.);
                wait(200).await;
            }
        }
        wait(1000).await;
        check(lowest() <= footer && lowest() > footer - 120., &format!("the page scrolls as one to the last member: at {}, buttons at {footer}", lowest()));
        check(model_chip.zip(on_screen("agent-model")).is_some_and(|(before, after)| after.top() < before.top()), "the model chip scrolled with the page");
        shot("05b-settings-bottom");
        let picked = state(cx).1.and_then(|setup| setup.5);
        check(sonnet.is_some() && picked == sonnet.as_ref().map(|id| format!("claude:{id}")), &format!("Group settings has Sonnet: {picked:?}"));
        check(reasoning(cx) == Some("medium"), &format!("Group settings has Medium: {:?}", reasoning(cx)));
        let saved = backend.call_raw("agents:groupSetup", json!({})).await.ok().map(|setup| setup["route"]["reasoning"].clone());
        check(saved == Some(json!("medium")), &format!("Medium is saved with the route model: {saved:?}"));
        shot("06-settings-model");
        eprintln!("group-setup-scroll-drive: done{}", if failed { " (FAILED)" } else { "" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=signed-in-models-drive`, with `FILO_TOOL_STATUS="claude=ready codex=signin muse=missing opencode=missing"`
/// in a fresh test config: the model pickers list Claude Code's models alone (the composer's reads the same rows; an
/// agent's own chat has none), the Edit agent page's and the group's routing step's pictured, and Filo, set to a Codex
/// model before, still names it on its chip.
fn run_signed_in_models_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::screens::agents::GROUP;
    use crate::screens::chat::conversation::{all_model_rows, model_rows};
    let backend = crate::backend::backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let mut failed = false;
        let mut check = |ok: bool, what: &str| {
            failed |= !ok;
            eprintln!("signed-in-models-drive: {what} ({})", if ok { "PASS" } else { "FAIL" });
        };
        let screen = |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let engines = |cx: &mut gpui::AsyncWindowContext| {
            let mut engines: Vec<filo_local::Engine> = cx.update(|_, cx| model_rows(cx).iter().map(|row| row.engine).collect()).unwrap_or_default();
            engines.dedup();
            engines
        };
        let codex = cx.update(|_, cx| all_model_rows(cx).iter().find(|row| row.engine == filo_local::Engine::Codex).cloned()).ok().flatten();
        check(codex.is_some(), &format!("Codex's models are known: {:?}", codex.as_ref().map(|row| &row.label)));
        let list = backend.call_raw("agents:list", json!({})).await.ok().and_then(|rows| rows.as_array().cloned()).unwrap_or_default();
        let filo = list.first().and_then(|row| row["_id"].as_str()).unwrap_or_default().to_string();
        if let Some(codex) = &codex {
            backend.call_raw("agents:update", json!({ "id": filo, "engine": "codex", "model": codex.id })).await.ok();
        }
        backend.call_raw("agents:create", json!({ "name": "Developer", "instructions": "Answer in one short sentence.", "engine": "claude", "model": "haiku" })).await.ok();
        this.update_in(cx, |shell, window, cx| shell.open_agent(filo.clone(), window, cx)).ok();
        // The first look at the tools answers in the background.
        for _ in 0..40 {
            wait(500).await;
            if !engines(cx).is_empty() {
                break;
            }
        }
        wait(1500).await;
        let listed = engines(cx);
        check(listed == [filo_local::Engine::Claude], &format!("the pickers list the tools signed in: {listed:?}"));
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_agent(filo.clone(), window, cx));
            }
        })
        .ok();
        wait(1500).await;
        shot("01-edit-agent-chip");
        click(&this, cx, "agent-model");
        wait(1200).await;
        shot("02-edit-agent-menu");
        if let Some(width) = window_width(&this, cx) {
            native_input::key(width, "\u{1b}", 53, 0);
        }
        wait(500).await;
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.open_agent(GROUP.to_string(), window, cx));
            }
        })
        .ok();
        wait(2000).await;
        if let Some(agents) = screen(cx) {
            this.update_in(cx, |_, window, cx| agents.update(cx, |agents, cx| agents.drive_setup("next", window, cx))).ok();
        }
        wait(2000).await;
        let route = screen(cx).and_then(|agents| agents.read_with(cx, |agents, cx| agents.drive_group(cx)).ok()).and_then(|(_, setup, _)| setup?.5);
        check(route.as_deref().is_some_and(|route| route.starts_with("claude:") && route.contains("haiku")), &format!("the routing step has Haiku: {route:?}"));
        click(&this, cx, "agent-model");
        wait(1200).await;
        shot("03-routing-menu");
        eprintln!("signed-in-models-drive: done{}", if failed { " (FAILED)" } else { "" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=desktop-pet-drive`: Settings › Desktop pet used like a person would (in-app AppKit clicks,
/// `native_input`): logs what's kept and what the desktop pet shows, clicks another character, then the Show on desktop
/// switch off and on again, checking each reaches the pet and desktop-companion.json. With `FILO_DRIVE_LEAVE_HIDDEN`
/// it ends with the pet hidden; a second launch logs whether both were kept.
fn run_desktop_pet_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::companion;
    cx.on_next_frame(window, |shell, window, cx| shell.push(Route::DesktopPet, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        // (kept character, shown, pet window open, the pose the desktop pet shows, what's in the file)
        let state = |cx: &mut gpui::AsyncWindowContext| {
            cx.update(|_, cx| {
                let settings = companion::settings(cx);
                let file = std::fs::read_to_string(crate::config::config_dir().join("desktop-companion.json"))
                    .unwrap_or_default();
                let file: Value = serde_json::from_str(&file).unwrap_or_default();
                (
                    settings.pet_character,
                    settings.pet,
                    cx.has_global::<crate::pet::PetWindow>(),
                    crate::pet::showing(cx),
                    file.get("petCharacter").cloned(),
                    file.get("pet").cloned(),
                )
            })
            .ok()
        };
        wait(5000).await;
        let before = state(cx);
        eprintln!("desktop-pet-drive: at launch {before:?}");
        let Some((kept, ..)) = before else { return };
        let mut failed = false;
        let target = if kept.as_deref() == Some("owl") { "cat" } else { "owl" };
        let clicked = click(&this, cx, &format!("desktop-pet-{target}-tile"));
        wait(1500).await;
        let after = state(cx);
        let ok = after.as_ref().is_some_and(|(kept, shown, open, pose, file, _)| {
            kept.as_deref() == Some(target)
                && (!*shown || (*open && pose.as_deref().is_some_and(|pose| pose.starts_with(&format!("{target}-")))))
                && file.as_ref().and_then(Value::as_str) == Some(target)
        });
        failed |= !ok;
        eprintln!("desktop-pet-drive: clicked {target} ({clicked}): {after:?} {}", if ok { "PASS" } else { "FAIL" });
        for round in 0..2 {
            let shown = state(cx).is_some_and(|(_, shown, ..)| shown);
            if round == 1 && !shown && std::env::var_os("FILO_DRIVE_LEAVE_HIDDEN").is_some() {
                break;
            }
            let clicked = click(&this, cx, "desktop-pet-show");
            wait(1200).await;
            let after = state(cx);
            let ok = after.as_ref().is_some_and(|(_, now, open, _, _, file)| {
                *now != shown && *open != shown && file.as_ref().and_then(Value::as_bool) == Some(!shown)
            });
            failed |= !ok;
            eprintln!(
                "desktop-pet-drive: switched {} ({clicked}): {after:?} {}",
                if shown { "off" } else { "on" },
                if ok { "PASS" } else { "FAIL" }
            );
        }
        eprintln!("desktop-pet-drive: {}", if failed { "FAILED" } else { "done" });
        cx.update(|_, cx| cx.quit()).ok();
    })
    .detach();
}

/// `FILO_OPEN=report-drive`: Report a problem about a failed reply (turn `FILO_DRIVE_TURN`, default "drive-turn"),
/// used like a person would (in-app AppKit clicks and keys, `native_input`): waits for the checks, types a note in
/// What happened?, clicks Send and logs the checks, the preview and how sending went. Point `FILO_REPORT_URL` at a
/// stand-in server to see what arrives.
/// `FILO_OPEN=welcome-drive` (on a config of its own, `FILO_CONFIG_DIR`, with `FILO_TOOL_STATUS` for the tools): the
/// welcome clicked through as a person would, logging where it is after each click. `FILO_WELCOME_DRIVE` picks the way:
/// `create` (Continue, Continue past Make it yours, the Builder starter, Create agent, then the first thing to try in its chat), `skip` (Skip
/// setup), `setup` (on to Set up a tool, then Codex), `also` (opening the line of tools that aren't on this Mac),
/// `ready-later` (on to Set up a tool, then waiting up to 30 s for a tool to be signed in) or `again` (Skip setup, then
/// Settings › Welcome tour). Quits at the end.
fn run_welcome_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let way = std::env::var("FILO_WELCOME_DRIVE").unwrap_or_else(|_| "create".into());
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let welcome = |cx: &mut gpui::AsyncWindowContext| {
            let tour = this.update(cx, |shell, _| shell.welcome()).ok().flatten()?;
            tour.read_with(cx, |tour, cx| tour.drive_state(cx)).ok()
        };
        let agents = |cx: &mut gpui::AsyncWindowContext| {
            this.update(cx, |shell, cx| {
                let screen = shell.agents_screen()?;
                let screen = screen.read(cx);
                Some(format!(
                    "tab {:?}, agents {:?}, chat {:?}",
                    shell.current_tab(),
                    screen.drive_rows(),
                    screen.drive_first_chat(cx)
                ))
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| "no Agents screen".into())
        };
        // Until the tools have been looked at.
        wait(1500).await;
        for _ in 0..40 {
            if welcome(cx).is_some_and(|state| !state.contains("Checking")) {
                break;
            }
            wait(500).await;
        }
        eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
        let step = |name: &str| shot(&format!("welcome-{way}-{name}"));
        // A click `x` of the way across `id` (cards in rows) and `y` of the way down.
        let click_across = |cx: &mut gpui::AsyncWindowContext, id: &str, x: f32, y: f32| {
            let (Some(bounds), Some(width)) = (on_screen(id), window_width(&this, cx)) else { return false };
            let at = |start: gpui::Pixels, length: gpui::Pixels, part: f32| f64::from(f32::from(start + length * part));
            native_input::click(
                width,
                at(bounds.origin.x, bounds.size.width, x),
                at(bounds.origin.y, bounds.size.height, y),
            )
        };
        step("1-welcome");
        let ends = !matches!(way.as_str(), "setup" | "ready-later" | "also");
        match way.as_str() {
            "skip" => eprintln!("welcome-drive: clicked Skip ({})", click(&this, cx, "welcome-skip")),
            "again" => {
                eprintln!("welcome-drive: clicked Skip ({})", click(&this, cx, "welcome-skip"));
                wait(2000).await;
                eprintln!(
                    "welcome-drive: the welcome is {}",
                    if welcome(cx).is_some() { "still open" } else { "closed" }
                );
                // What Settings › Welcome tour does.
                cx.update(crate::screens::welcome::show).ok();
                wait(1500).await;
                eprintln!("welcome-drive: again: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("2-again");
                eprintln!("welcome-drive: clicked Skip ({})", click(&this, cx, "welcome-skip"));
            }
            "also" => {
                eprintln!("welcome-drive: clicked the tools not on this Mac ({})", click(&this, cx, "welcome-also"));
                wait(800).await;
                eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("2-also-open");
            }
            "ready-later" => {
                eprintln!("welcome-drive: clicked the primary button ({})", click(&this, cx, "welcome-next"));
                wait(1200).await;
                let before = welcome(cx).unwrap_or_default();
                eprintln!("welcome-drive: {before}");
                step("2-waiting");
                for _ in 0..60 {
                    wait(500).await;
                    let now = welcome(cx).unwrap_or_default();
                    if now != before {
                        eprintln!("welcome-drive: then {now}");
                        wait(500).await;
                        step("3-signed-in");
                        break;
                    }
                }
            }
            "setup" => {
                eprintln!("welcome-drive: clicked the primary button ({})", click(&this, cx, "welcome-next"));
                wait(1200).await;
                eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("2-set-up-a-tool");
                // Codex: the second card of the first row.
                eprintln!("welcome-drive: clicked Codex ({})", click_across(cx, "welcome-options", 0.75, 0.25));
                wait(800).await;
                eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("3-codex");
                eprintln!("welcome-drive: clicked Back ({})", click(&this, cx, "welcome-back"));
            }
            _ => {
                eprintln!("welcome-drive: clicked Continue ({})", click(&this, cx, "welcome-next"));
                wait(1200).await;
                eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("2-make-it-yours");
                eprintln!("welcome-drive: clicked Continue ({})", click(&this, cx, "welcome-next"));
                wait(1200).await;
                eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("2-first-agent");
                eprintln!("welcome-drive: clicked Builder ({})", click_across(cx, "welcome-starters", 0.125, 0.5));
                wait(800).await;
                eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
                step("3-builder");
                eprintln!("welcome-drive: clicked Create agent ({})", click(&this, cx, "welcome-next"));
            }
        }
        wait(800).await;
        if ends {
            for _ in 0..40 {
                if welcome(cx).is_none() {
                    break;
                }
                wait(500).await;
            }
            eprintln!("welcome-drive: the welcome is {}", if welcome(cx).is_some() { "still open" } else { "closed" });
            // The list and the chat load.
            wait(3000).await;
            eprintln!("welcome-drive: {}", agents(cx));
            eprintln!("welcome-drive: empty state's New agent on screen: {}", on_screen("agents-new-first").is_some());
            step("4-after");
            for index in 0..3 {
                eprintln!(
                    "welcome-drive: thing to try {index} at {:?}",
                    on_screen(&format!("first-try-{index}")).map(|bounds| bounds.origin)
                );
            }
            if way != "skip" {
                eprintln!("welcome-drive: clicked the first thing to try ({})", click(&this, cx, "first-try-0"));
                wait(1000).await;
                eprintln!("welcome-drive: {}", agents(cx));
                step("5-tried");
            }
        } else {
            wait(800).await;
            eprintln!("welcome-drive: {}", welcome(cx).unwrap_or_else(|| "no welcome".into()));
        }
        eprintln!("welcome-drive: done");
        wait(1500).await;
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

fn run_report_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::screens::report::ReportScreen;
    let turn_id = std::env::var("FILO_DRIVE_TURN").unwrap_or_else(|_| "drive-turn".into());
    let origin = crate::report::Origin::Turn { turn_id, error: Some("Claude Code stopped with status 1.".into()) };
    cx.on_next_frame(window, move |shell, window, cx| shell.push(Route::Report(origin), window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let state = |cx: &mut gpui::AsyncWindowContext| {
            let page = this.update(cx, |shell, _| shell.top_page()).ok().flatten()?.downcast::<ReportScreen>().ok()?;
            page.read_with(cx, |page, cx| page.drive_state(cx)).ok()
        };
        wait(2000).await;
        let mut checking = true;
        for _ in 0..180 {
            checking = state(cx).is_none_or(|(checking, ..)| checking);
            if !checking {
                break;
            }
            wait(500).await;
        }
        let Some((_, checks, preview, sending)) = state(cx) else {
            return eprintln!("report-drive: FAIL no report page");
        };
        for check in &checks {
            eprintln!("report-drive: check {check}");
        }
        eprintln!("report-drive: preview: {preview}");
        eprintln!(
            "report-drive: checks {} ({}), sending {sending}",
            if checking { "still running" } else { "done" },
            checks.len()
        );
        // Send is below the checks: scroll down to it, as a person would.
        if let Some(width) = window_width(&this, cx) {
            native_input::hover(width, width / 2., 400.);
            native_input::scroll_in(width, width / 2., 400., 800.);
        }
        wait(800).await;
        eprintln!("report-drive: Send at {:?}", on_screen("report-send").map(|bounds| bounds.center()));
        eprintln!("report-drive: clicked the note ({})", click(&this, cx, "report-note"));
        wait(400).await;
        type_keys(&this, cx, "It stopped mid reply", false);
        wait(400).await;
        if let Some(width) = window_width(&this, cx) {
            native_input::hover(width, width / 2., 400.);
            native_input::scroll_in(width, width / 2., 400., 800.);
        }
        wait(800).await;
        eprintln!("report-drive: Send at {:?}", on_screen("report-send"));
        eprintln!("report-drive: clicked Send ({})", click(&this, cx, "report-send"));
        let mut sending = String::new();
        for _ in 0..60 {
            wait(500).await;
            sending = state(cx).map(|(.., sending)| sending).unwrap_or_default();
            if sending != "sending" && !sending.starts_with("idle") {
                break;
            }
        }
        eprintln!("report-drive: after Send: {sending}");
        eprintln!("report-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=feed-glance-drive`: the first glance post in the Feed (feed_glance.rs), used like a person would, with
/// in-app AppKit events: waits for its page, hovers it and clicks the last choice in it (through the web view), then
/// the Glances chip, Save, Open larger (closed with Esc) and Hide. Logs what each did, and with
/// `FILO_FIXTURE_SHOTS=<folder>` saves a screenshot after each there. Quits at the end unless `FILO_DRIVE_STAY`.
fn run_feed_glance_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Feed, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let page = |id: &str, cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |shell, window, cx| shell.feed_screen().and_then(|feed| feed.read(cx).drive_glance_page(id, window))).ok().flatten()
        };
        let read = |cx: &mut gpui::AsyncWindowContext, f: &dyn Fn(&crate::screens::feed::FeedScreen) -> String| {
            this.update(cx, |shell, cx| shell.feed_screen().map(|feed| f(feed.read(cx)))).ok().flatten().unwrap_or_else(|| "no feed".into())
        };
        wait(2500).await;
        let mut id = None;
        for _ in 0..40 {
            id = this.update(cx, |shell, cx| shell.feed_screen().and_then(|feed| feed.read(cx).drive_glance_posts().first().cloned())).ok().flatten();
            if id.is_some() {
                break;
            }
            wait(500).await;
        }
        let Some(id) = id else { return eprintln!("feed-glance-drive: FAIL no glance post in the Feed") };
        let mut placed = None;
        for _ in 0..40 {
            placed = page(&id, cx);
            if placed.as_ref().is_some_and(|(ready, bounds)| *ready && bounds.is_some()) {
                break;
            }
            wait(500).await;
        }
        eprintln!("feed-glance-drive: post {id}, page {placed:?}");
        wait(800).await;
        shot("glance-feed");
        let (Some((true, Some(bounds))), Some(width)) = (placed, window_width(&this, cx)) else {
            return eprintln!("feed-glance-drive: FAIL the glance's page never showed");
        };
        let (left, bottom) = (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y + bounds.size.height)));
        native_input::hover(width, left + 120., f64::from(f32::from(bounds.origin.y)) + 40.);
        wait(600).await;
        shot("glance-hover");
        // The choices sit at the bottom of the page: the last one's middle, a little in from the left.
        eprintln!("feed-glance-drive: clicked the last choice ({})", native_input::click(width, left + 60., bottom - 22.));
        let mut pick = String::new();
        for _ in 0..20 {
            wait(300).await;
            pick = read(cx, &|feed| format!("{:?}", feed.drive_pick(&id)));
            if !pick.starts_with("(None") && pick.ends_with("false)") {
                break;
            }
        }
        eprintln!("feed-glance-drive: pick {pick}, notice {}", read(cx, &|feed| format!("{:?}", feed.drive_notice())));
        wait(600).await;
        shot("glance-picked");
        eprintln!("feed-glance-drive: clicked Glances ({})", click(&this, cx, "topic-glances"));
        wait(900).await;
        eprintln!("feed-glance-drive: glances only {}, listed {}", read(cx, &|feed| feed.drive_glances_only().to_string()), read(cx, &|feed| format!("{:?}", feed.drive_post_ids())));
        shot("glance-filter");
        eprintln!("feed-glance-drive: clicked Save ({})", click(&this, cx, &format!("save-{id}")));
        wait(1200).await;
        eprintln!("feed-glance-drive: notice {}", read(cx, &|feed| format!("{:?}", feed.drive_notice())));
        shot("glance-saved");
        eprintln!("feed-glance-drive: clicked Open larger ({})", click(&this, cx, &format!("open-glance-{id}")));
        wait(1800).await;
        let dialog = this.update_in(cx, |_, window, cx| gpui_component::WindowExt::has_active_dialog(window, cx)).unwrap_or(false);
        eprintln!("feed-glance-drive: larger view open {dialog}, the Feed's page {:?}", page(&id, cx));
        shot("glance-larger");
        native_input::key(width, "\u{1b}", 53, 0);
        wait(900).await;
        let dialog = this.update_in(cx, |_, window, cx| gpui_component::WindowExt::has_active_dialog(window, cx)).unwrap_or(false);
        eprintln!("feed-glance-drive: after Esc, larger view open {dialog}");
        eprintln!("feed-glance-drive: clicked For you ({})", click(&this, cx, "topic-all"));
        wait(900).await;
        eprintln!("feed-glance-drive: clicked Hide ({})", click(&this, cx, &format!("hide-{id}")));
        wait(900).await;
        eprintln!("feed-glance-drive: page after Hide {:?}", page(&id, cx));
        shot("glance-hidden");
        // `FILO_DRIVE_WATCH_UPDATE`: an agent changes a glance meanwhile (update_feed_glance); the post shows the new one
        // where it was.
        if std::env::var_os("FILO_DRIVE_WATCH_UPDATE").is_some() {
            let ids = |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, cx| shell.feed_screen().map(|feed| feed.read(cx).drive_glance_ids())).ok().flatten().unwrap_or_default();
            let order = |cx: &mut gpui::AsyncWindowContext| read(cx, &|feed| format!("{:?}", feed.drive_post_ids()));
            let before = ids(cx);
            eprintln!("feed-glance-drive: watching {before:?}, order {}", order(cx));
            for _ in 0..120 {
                wait(500).await;
                let now = ids(cx);
                if let Some((post, glance)) = now.iter().find(|(post, glance)| before.iter().any(|(was, old)| was == post && old != glance)) {
                    let (post, glance) = (post.clone(), glance.clone());
                    let mut shown = None;
                    for _ in 0..20 {
                        shown = page(&post, cx);
                        if shown.as_ref().is_some_and(|(ready, bounds)| *ready && bounds.is_some()) {
                            break;
                        }
                        wait(500).await;
                    }
                    eprintln!("feed-glance-drive: updated in place: post {post} now shows glance {glance}, page {shown:?}, order {}", order(cx));
                    shot("glance-updated");
                    break;
                }
            }
        }
        eprintln!("feed-glance-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=agent-save-keys-drive`: an agent's settings page with its "Unsaved changes" bar showing, ⌘S and then
/// Esc pressed (in-app AppKit keys) with the keyboard in each place it can be: typing in the instructions, after a
/// model picked in the model menu, after the agents list's ⌄ menu opened and closed, with the focus dropped (`blur`), and after a
/// click on the page. ⌘S has to save (the bar gone, the change kept) and Esc revert (the bar gone, the change undone).
/// Logs each, saves `FILO_DRIVE_SHOTS` window shots and quits (exit 1 when a check fails). Run it on a config of its
/// own (`FILO_CONFIG_DIR`).
fn run_agent_save_keys_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").ok();
        let failed = std::cell::Cell::new(false);
        let check = |step: &str, ok: bool| {
            failed.set(failed.get() || !ok);
            eprintln!("agent-save-keys-drive: {step}: {}", if ok { "PASS" } else { "FAIL" });
        };
        let screen =
            |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let id = first_id(
            backend.call_raw("agents:create", json!({ "name": "Keys test", "instructions": "Hey" })),
            |value| value.as_str().map(str::to_string),
        )
        .await;
        let Some(id) = id else { return eprintln!("agent-save-keys-drive: FAIL couldn't make an agent") };
        wait(5000).await;
        let Some(agents) = screen(cx) else { return eprintln!("agent-save-keys-drive: FAIL no Agents tab") };
        agents.update_in(cx, |agents, window, cx| agents.edit_agent(id.clone(), window, cx)).ok();
        // The models come from the CLIs in the background on a fresh config: two to pick between.
        for _ in 0..60 {
            if cx.update(|_, cx| crate::screens::chat::conversation::model_rows(cx).len()).unwrap_or_default() >= 2 {
                break;
            }
            wait(1000).await;
        }
        wait(1000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("agent-save-keys-drive: no window") };
        let command_s = || native_input::key(width, "s", 1, native_input::COMMAND);
        let escape = || native_input::key(width, "\u{1b}", 53, 0);
        let state = |cx: &mut gpui::AsyncWindowContext| {
            screen(cx).and_then(|agents| cx.update(|_, cx| agents.read(cx).drive_form_state(cx)).ok().flatten())
        };
        let shoot = async |name: &str, cx: &mut gpui::AsyncWindowContext| {
            let Some(shots) = &shots else { return };
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(250).await;
            crate::window_shot::save_largest(format!("{shots}/{name}.png"));
        };
        let focused = |cx: &mut gpui::AsyncWindowContext| {
            agents.update_in(cx, |agents, window, cx| agents.drive_focus_place(window, cx)).unwrap_or_default()
        };
        // Esc goes back to what was last saved.
        let Some((_, text, model)) = state(cx) else { return eprintln!("agent-save-keys-drive: FAIL no form") };
        let mut saved = (text, model);
        let mut round = 0;
        for case in [
            "typing in the instructions",
            "after picking a model",
            "after the list's menu",
            "with the focus dropped",
            "after a click on the page",
        ] {
            for key in ["⌘S", "Esc"] {
                round += 1;
                let Some((_, before, model_before)) = state(cx) else {
                    return eprintln!("agent-save-keys-drive: FAIL the form closed");
                };
                let changed = format!("{before} {round}");
                match case {
                    "typing in the instructions" => {
                        agents.update_in(cx, |agents, window, cx| agents.drive_focus_instructions(window, cx)).ok();
                        wait(300).await;
                        agents
                            .update_in(cx, |agents, window, cx| {
                                agents.drive_set_instructions(before.trim_end(), window, cx)
                            })
                            .ok();
                        type_keys(&this, cx, &format!(" {round}"), false);
                    }
                    "after picking a model" => {
                        click(&this, cx, "agent-model");
                        wait(1200).await;
                        let rows =
                            cx.update(|_, cx| crate::screens::chat::conversation::model_rows(cx)).unwrap_or_default();
                        let other = (0..rows.len()).find(|&row| {
                            on_screen(&format!("agent-model-menu-row-{row}")).is_some()
                                && (rows[row].engine, Some(rows[row].id.clone())) != model_before
                        });
                        let Some(other) = other else {
                            return eprintln!("agent-save-keys-drive: FAIL no other model shown");
                        };
                        click(&this, cx, &format!("agent-model-menu-row-{other}"));
                    }
                    _ => {
                        agents
                            .update_in(cx, |agents, window, cx| agents.drive_set_instructions(&changed, window, cx))
                            .ok();
                        wait(300).await;
                        match case {
                            "after the list's menu" => {
                                click(&this, cx, "agents-view-menu");
                                wait(800).await;
                                click(&this, cx, "agents-view-menu");
                            }
                            "with the focus dropped" => {
                                this.update_in(cx, |_, window, _| window.blur()).ok();
                            }
                            _ => {
                                // Beside the page's column, under its header.
                                let Some(column) = on_screen("agent-profile-end") else {
                                    return eprintln!("agent-save-keys-drive: FAIL no page");
                                };
                                native_input::click(
                                    width,
                                    f64::from(f32::from(column.left())) - 12.,
                                    f64::from(f32::from(column.top())) + 140.,
                                );
                            }
                        }
                    }
                }
                wait(800).await;
                let shown = state(cx);
                let dirty = shown.as_ref().is_some_and(|(dirty, ..)| *dirty);
                eprintln!("agent-save-keys-drive: {case}: unsaved {dirty}, keyboard in [{}]", focused(cx));
                if !dirty {
                    check(&format!("{case}: the bar shows"), false);
                    continue;
                }
                if round == 1 {
                    shoot("save-bar-showing", cx).await;
                }
                let (_, edited, model_edited) = shown.unwrap();
                if key == "⌘S" {
                    command_s()
                } else {
                    escape()
                };
                wait(1200).await;
                let Some((dirty, after, model_after)) = state(cx) else {
                    return eprintln!("agent-save-keys-drive: FAIL the form closed");
                };
                let ok = !dirty
                    && if key == "⌘S" {
                        after == edited && model_after == model_edited
                    } else {
                        (after.clone(), model_after.clone()) == saved
                    };
                if ok && key == "⌘S" {
                    saved = (after.clone(), model_after.clone());
                }
                eprintln!(
                    "agent-save-keys-drive: {case}: after {key}: unsaved {dirty}, instructions {after:?}, model {:?}",
                    model_after.1
                );
                check(&format!("{case}: {key} {}", if key == "⌘S" { "saves" } else { "reverts" }), ok);
            }
        }
        // What was saved is what the backend has.
        let saved = backend.call_raw("agents:list", json!({})).await.ok().and_then(|rows| {
            rows.as_array()?
                .iter()
                .find(|row| row["_id"].as_str() == Some(id.as_str()))
                .map(|row| row["instructions"].clone())
        });
        let shown = state(cx).map(|(_, text, _)| text);
        eprintln!("agent-save-keys-drive: saved instructions {saved:?}, shown {shown:?}");
        check(
            "the saved instructions are the ones shown",
            saved.is_some_and(|saved| saved.as_str().map(str::trim) == shown.as_deref().map(str::trim)),
        );
        shoot("saved", cx).await;
        eprintln!("agent-save-keys-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        crate::window_shot::flush(&executor).await;
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            std::process::exit(if failed.get() { 1 } else { 0 });
        }
    })
    .detach();
}

/// `FILO_OPEN=agent-back-drive`: an agent's profile, opened over its chat, left with its chevron, ⌘[ and ⌘← (on the
/// page, and in the instructions: ⌘← there first moves the caret to its line's start, then goes back); each has to show
/// the chat again with its composer taking the keys. Logs each, saves `FILO_DRIVE_SHOTS`
/// window shots and quits (exit 1 when a check fails). Run it on a config of its own (`FILO_CONFIG_DIR`).
fn run_agent_back_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let failed = std::cell::Cell::new(false);
        let check = |step: &str, ok: bool| {
            failed.set(failed.get() || !ok);
            eprintln!("agent-back-drive: {step}: {}", if ok { "PASS" } else { "FAIL" });
        };
        let screen =
            |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        let id = first_id(
            backend.call_raw("agents:create", json!({ "name": "Back test", "instructions": "Hey" })),
            |value| value.as_str().map(str::to_string),
        )
        .await;
        let Some(id) = id else { return eprintln!("agent-back-drive: FAIL couldn't make an agent") };
        wait(4000).await;
        let Some(agents) = screen(cx) else { return eprintln!("agent-back-drive: FAIL no Agents tab") };
        let Some(width) = window_width(&this, cx) else { return eprintln!("agent-back-drive: no window") };
        let in_chat = |cx: &mut gpui::AsyncWindowContext| {
            agents.update_in(cx, |agents, window, cx| agents.drive_composer_focused(window, cx)).unwrap_or_default()
        };
        let form_open = |cx: &mut gpui::AsyncWindowContext| {
            cx.update(|_, cx| agents.read(cx).drive_form_state(cx).is_some()).unwrap_or_default()
        };
        let caret = |cx: &mut gpui::AsyncWindowContext| {
            cx.update(|_, cx| agents.read(cx).drive_instructions_caret(cx)).ok().flatten()
        };
        let command_bracket = || native_input::key(width, "[", 33, native_input::COMMAND);
        let command_left = || native_input::key(width, "\u{F702}", 123, native_input::COMMAND | native_input::FUNCTION);
        agents.update_in(cx, |agents, window, cx| agents.open_agent(id.clone(), window, cx)).ok();
        for _ in 0..30 {
            if in_chat(cx) {
                break;
            }
            wait(500).await;
        }
        check("its chat opens, the composer taking the keys", in_chat(cx));
        let open = async |cx: &mut gpui::AsyncWindowContext| {
            agents.update_in(cx, |agents, window, cx| agents.edit_agent(id.clone(), window, cx)).ok();
            wait(1000).await;
        };
        let back_in_chat = async |step: &str, cx: &mut gpui::AsyncWindowContext| {
            wait(1000).await;
            let (open, chat) = (form_open(cx), in_chat(cx));
            eprintln!("agent-back-drive: {step}: profile open {open}, composer has the keys {chat}");
            check(step, !open && chat);
        };
        open(cx).await;
        let place = |id: &str| {
            on_screen(id).map(|bounds| {
                format!(
                    "x {:.0}..{:.0} y {:.0}..{:.0}",
                    f32::from(bounds.left()),
                    f32::from(bounds.right()),
                    f32::from(bounds.top()),
                    f32::from(bounds.bottom())
                )
            })
        };
        eprintln!(
            "agent-back-drive: chevron {:?}, page column {:?}",
            place("agent-form-back"),
            place("agent-profile-end")
        );
        check("the profile shows its chevron", form_open(cx) && on_screen("agent-form-back").is_some());
        shot("profile");
        command_bracket();
        back_in_chat("⌘[ on the page goes back", cx).await;
        open(cx).await;
        command_left();
        back_in_chat("⌘← on the page goes back", cx).await;
        // In the instructions: the caret mid-line moves to the line's start first.
        open(cx).await;
        agents
            .update_in(cx, |agents, window, cx| {
                agents.drive_focus_instructions(window, cx);
                agents.drive_set_instructions("First line\nSecond line", window, cx);
            })
            .ok();
        wait(500).await;
        // ⌘→: the caret to the end of the second line.
        native_input::key(width, "\u{F703}", 124, native_input::COMMAND | native_input::FUNCTION);
        wait(500).await;
        eprintln!("agent-back-drive: caret in the instructions at {:?}", caret(cx));
        command_left();
        wait(800).await;
        let (open_after, moved) = (form_open(cx), caret(cx));
        eprintln!("agent-back-drive: after ⌘← mid-line: profile open {open_after}, caret {moved:?}");
        check(
            "⌘← mid-line in the instructions moves the caret to the line's start",
            open_after && moved.is_some_and(|(_, column)| column == 0),
        );
        command_left();
        back_in_chat("⌘← at the line's start in the instructions goes back", cx).await;
        open(cx).await;
        agents
            .update_in(cx, |agents, window, cx| {
                agents.drive_focus_instructions(window, cx);
                agents.drive_set_instructions("Hey there", window, cx);
            })
            .ok();
        wait(500).await;
        command_bracket();
        back_in_chat("⌘[ mid-line in the instructions goes back", cx).await;
        open(cx).await;
        // A frame laid out with the chevron in it, so the click lands on it.
        for _ in 0..10 {
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(300).await;
            if on_screen("agent-form-back").is_some() {
                break;
            }
        }
        click(&this, cx, "agent-form-back");
        back_in_chat("the chevron goes back", cx).await;
        shot("back-in-chat");
        eprintln!("agent-back-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        crate::window_shot::flush(&executor).await;
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            std::process::exit(if failed.get() { 1 } else { 0 });
        }
    })
    .detach();
}

/// `FILO_OPEN=agent-model-drive`: a new agent's form and its model menu (every tool's models, OpenCode's hundreds
/// among them), used like a person would with in-app AppKit events: opens it and logs where its list sits in the
/// window, scrolls the list to its end and checks the last model is shown, types `FILO_DRIVE_SEARCH` (default "muse")
/// and logs how many models match, then Down `FILO_DRIVE_DOWNS` times (default 2), Up and Return (or a click on the
/// first, with `FILO_DRIVE_CLICK`), and logs the form's model. `FILO_DRIVE_HOVER=<line>` first rests the pointer on that
/// line of the matches, which moves the highlight there. Run it on a config of its own
/// (`FILO_CONFIG_DIR`).
fn run_agent_model_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        for tool in filo_local::coding::TOOLS {
            backend.call_raw("coding:setPlugged", json!({ "tool": tool, "plugged": true })).await.ok();
        }
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        const LIST: &str = "agent-model-menu-list";
        let place = |id: &str| {
            on_screen(id).map(|bounds| {
                format!(
                    "x {:.0}..{:.0} y {:.0}..{:.0}",
                    f32::from(bounds.left()),
                    f32::from(bounds.right()),
                    f32::from(bounds.top()),
                    f32::from(bounds.bottom())
                )
            })
        };
        let screen =
            |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        wait(4000).await;
        let Some(agents) = screen(cx) else { return eprintln!("agent-model-drive: FAIL no Agents tab") };
        agents.update_in(cx, |agents, window, cx| agents.new_agent(window, cx)).ok();
        // The models come from the CLIs in the background on a fresh config.
        let mut rows = std::sync::Arc::new(Vec::new());
        for _ in 0..120 {
            rows = cx.update(|_, cx| crate::screens::chat::conversation::model_rows(cx)).unwrap_or_default();
            if rows.iter().any(|row| row.engine == filo_local::Engine::OpenCode) && on_screen("agent-model").is_some() {
                break;
            }
            wait(1000).await;
        }
        let count = |engine| rows.iter().filter(|row| row.engine == engine).count();
        use filo_local::Engine::{Claude, Codex, Muse, OpenCode};
        eprintln!(
            "agent-model-drive: models: Claude {}, Codex {}, Muse {}, OpenCode {} ({} in all)",
            count(Claude),
            count(Codex),
            count(Muse),
            count(OpenCode),
            rows.len()
        );
        let Some(width) = window_width(&this, cx) else { return eprintln!("agent-model-drive: no window") };
        let height = cx.update(|window, _| f32::from(window.viewport_size().height)).unwrap_or_default();
        eprintln!(
            "agent-model-drive: window {width:.0}x{height:.0}, model button {}",
            place("agent-model").unwrap_or_else(|| "-".into())
        );
        // A first click on a window that isn't key can go to making it key: again until the menu opens.
        for _ in 0..3 {
            eprintln!("agent-model-drive: clicked the model ({})", click(&this, cx, "agent-model"));
            wait(1000).await;
            if on_screen(LIST).is_some() {
                break;
            }
        }
        shot("agent-model-1-opened");
        let Some(list) = on_screen(LIST) else { return eprintln!("agent-model-drive: FAIL the menu didn't open") };
        let inside = f32::from(list.top()) >= 0. && f32::from(list.bottom()) <= height;
        eprintln!(
            "agent-model-drive: list {} ({})",
            place(LIST).unwrap_or_default(),
            if inside { "inside the window" } else { "OFF THE WINDOW" }
        );
        let last = rows.len() - 1;
        // A posted wheel lands off where it's aimed (`scroll_in` here): one that scrolls nothing, aimed near the
        // window's top left, tells by how much (gpui takes the pointer to where a wheel lands), then the list's middle.
        let center = list.center();
        native_input::scroll_in(width, 40., 200., 0.);
        wait(300).await;
        let landed = cx.update(|window, _| window.mouse_position()).unwrap_or_default();
        let (dx, dy) = (f64::from(f32::from(landed.x)) - 40., f64::from(f32::from(landed.y)) - 200.);
        let (x, y) = (f64::from(f32::from(center.x)) - dx, f64::from(f32::from(center.y)) - dy);
        eprintln!("agent-model-drive: a wheel lands {dx:.0},{dy:.0} off; aimed at {x:.0},{y:.0}");
        for _ in 0..80 {
            if on_screen(&format!("agent-model-menu-row-{last}")).is_some() {
                break;
            }
            native_input::scroll_in(width, x, y, 600.);
            wait(200).await;
        }
        let first = (0..rows.len()).find(|row| on_screen(&format!("agent-model-menu-row-{row}")).is_some());
        let landed = cx.update(|window, _| window.mouse_position()).unwrap_or_default();
        eprintln!(
            "agent-model-drive: scrolled over {landed:?}: first line shown {first:?}; last model (line {}) {}",
            last + 1,
            place(&format!("agent-model-menu-row-{last}"))
                .map_or("NOT SHOWN".to_string(), |at| format!("shown at {at}"))
        );
        eprintln!(
            "agent-model-drive: list after scrolling {} (window {height:.0} tall)",
            place(LIST).unwrap_or_default()
        );
        // A wheel that lands off the menu closes it: open it again for the search.
        if on_screen(LIST).is_none() {
            eprintln!("agent-model-drive: the menu closed; clicked the model ({})", click(&this, cx, "agent-model"));
            wait(1000).await;
        }
        let search = std::env::var("FILO_DRIVE_SEARCH").unwrap_or_else(|_| "muse".into());
        type_keys(&this, cx, &search, false);
        wait(1000).await;
        let shown = on_screen(LIST).map_or(0., |bounds| (f32::from(bounds.size.height) / 26.).round());
        let matches: Vec<String> = rows
            .iter()
            .filter(|row| {
                let text = format!("{} {} {}", row.label, row.id, row.engine.label()).to_lowercase();
                search.split_whitespace().all(|word| text.contains(&word.to_lowercase()))
            })
            .map(|row| format!("{} ({})", row.label, row.engine.label()))
            .collect();
        eprintln!(
            "agent-model-drive: searched {search:?}: list {} ({shown} lines shown; {} match: {})",
            place(LIST).unwrap_or_default(),
            matches.len(),
            matches.join(", ")
        );
        shot("agent-model-2-searched");
        // `FILO_DRIVE_CLICK`: the first that matches clicked rather than Return.
        if std::env::var_os("FILO_DRIVE_CLICK").is_some() {
            eprintln!("agent-model-drive: clicked the first match ({})", click(&this, cx, "agent-model-menu-row-0"));
        } else {
            // The highlight stops at the list's ends, so Up from the last match is on the one before it.
            let downs: usize = std::env::var("FILO_DRIVE_DOWNS").ok().and_then(|n| n.parse().ok()).unwrap_or(2);
            let start: usize = std::env::var("FILO_DRIVE_HOVER").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
            if let Some(line) =
                std::env::var_os("FILO_DRIVE_HOVER").and(on_screen(&format!("agent-model-menu-row-{start}")))
            {
                let center = line.center();
                let hovered = native_input::hover(width, f32::from(center.x).into(), f32::from(center.y).into());
                eprintln!("agent-model-drive: pointer rests on match {start} ({hovered})");
                wait(500).await;
            }
            for _ in 0..downs {
                native_input::key(width, "\u{f701}", 125, 0);
                wait(150).await;
            }
            native_input::key(width, "\u{f700}", 126, 0);
            wait(300).await;
            shot("agent-model-3-arrows");
            wait(500).await;
            let expected = matches.get((start + downs).min(matches.len().saturating_sub(1)).saturating_sub(1)).cloned();
            eprintln!(
                "agent-model-drive: pressed Down {downs} times, then Up; Return should pick {}",
                expected.unwrap_or_default()
            );
            type_keys(&this, cx, "", true);
        }
        wait(1500).await;
        let model =
            screen(cx).and_then(|agents| agents.read_with(cx, |agents, _| agents.drive_form_model()).ok().flatten());
        shot("agent-model-4-picked");
        eprintln!(
            "agent-model-drive: after Return: list {}, form's model {model:?}",
            place(LIST).unwrap_or_else(|| "closed".into())
        );
        eprintln!("agent-model-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=coding-update-drive`: the Coding page's "Update to vX" pills (every tool plugged in, the newest versions
/// looked up as the page opens), then the pill of each tool in `FILO_DRIVE_TOOLS` (default "pi") clicked, but only when
/// its CLI is a stand-in in the first folder of `FILO_LOGIN_PATH`: a real tool is never updated. The rows are logged,
/// with shots in `FILO_DRIVE_SHOTS`, before, while and after the updates.
fn run_coding_update_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Coding, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        for tool in filo_local::coding::TOOLS {
            backend.call_raw("coding:setPlugged", json!({ "tool": tool, "plugged": true })).await.ok();
        }
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let rows = async || {
            let overview = backend.call_raw("coding:overview", json!({})).await.unwrap_or_default();
            overview["tools"].as_array().cloned().unwrap_or_default()
        };
        let log = |when: &str, rows: &[Value]| {
            for row in rows {
                eprintln!(
                    "coding-update-drive: {when}: {} version {} latest {} updating {} error {}",
                    row["id"].as_str().unwrap_or("-"),
                    row["version"].as_str().unwrap_or("-"),
                    row["latestVersion"].as_str().unwrap_or("-"),
                    row["updating"] == true,
                    row["updateError"].as_str().unwrap_or("-"),
                );
            }
        };
        let wanted = std::env::var("FILO_DRIVE_TOOLS").unwrap_or_else(|_| "pi".into());
        let stand_ins = std::env::var("FILO_LOGIN_PATH").ok().and_then(|path| path.split(':').next().map(str::to_string));
        let tools: Vec<String> = wanted
            .split_whitespace()
            .filter(|tool| {
                let Some(engine) = filo_local::coding::engine_of(tool) else { return false };
                let stand_in =
                    stand_ins.as_ref().is_some_and(|dir| std::path::Path::new(dir).join(engine.program()).is_file());
                if !stand_in {
                    eprintln!("coding-update-drive: {tool} isn't a stand-in in FILO_LOGIN_PATH's first folder: not clicked");
                }
                stand_in
            })
            .map(str::to_string)
            .collect();
        // The look for newer versions goes out as the page opens; its pills come when it answers.
        for _ in 0..60 {
            if tools.iter().all(|tool| on_screen(&format!("coding-update-{tool}")).is_some()) {
                break;
            }
            wait(1000).await;
        }
        log("opened", &rows().await);
        shot("coding-update-pills");
        for tool in &tools {
            let pill = format!("coding-update-{tool}");
            if let (Some(width), Some(bounds)) = (window_width(&this, cx), on_screen(&pill))
                && f32::from(bounds.top()) > 600.
            {
                native_input::hover(width, width / 2., 300.);
                native_input::scroll_in(width, width / 2., 300., f64::from(f32::from(bounds.top())) - 300.);
                wait(800).await;
            }
            eprintln!("coding-update-drive: clicked {tool}'s pill ({})", click(&this, cx, &pill));
        }
        wait(700).await;
        log("clicked", &rows().await);
        shot("coding-updating");
        for _ in 0..120 {
            if !rows().await.iter().any(|row| row["updating"] == true) {
                break;
            }
            wait(500).await;
        }
        wait(1000).await;
        log("updated", &rows().await);
        shot("coding-updated");
        eprintln!("coding-update-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=coding-pickers-drive`: the Coding page's Model, Reasoning and Mode pickers (every tool plugged in), whose
/// places it logs for each tool (all on one line when their middles match). Then one tool's model picker
/// (`FILO_DRIVE_TOOL`, default "opencode"), used like a person would: a click, `FILO_DRIVE_SEARCH` typed (default
/// "free") and Return, the places logged again (Reasoning and Mode where they were), with shots in `FILO_DRIVE_SHOTS`.
fn run_coding_pickers_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Coding, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        // Every tool plugged in, so each shows its pickers (run it on a config of its own: `FILO_CONFIG_DIR`).
        for tool in filo_local::coding::TOOLS {
            let plugged = backend.call_raw("coding:setPlugged", json!({ "tool": tool, "plugged": true })).await;
            eprintln!("coding-pickers-drive: plugged {tool} ({})", if plugged.is_ok() { "ok" } else { "failed" });
        }
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let place = |id: &str| {
            on_screen(id).map(|bounds| {
                format!(
                    "x {:.0}..{:.0} y {:.0}..{:.0}",
                    f32::from(bounds.left()),
                    f32::from(bounds.right()),
                    f32::from(bounds.top()),
                    f32::from(bounds.bottom())
                )
            })
        };
        let log = |when: &str| {
            for tool in filo_local::coding::TOOLS {
                let places: Vec<String> = ["model", "reasoning", "mode"]
                    .iter()
                    .map(|picker| {
                        format!("{picker} {}", place(&format!("coding-{picker}-{tool}")).unwrap_or_else(|| "-".into()))
                    })
                    .collect();
                let tops: Vec<i32> = ["model", "reasoning", "mode"]
                    .iter()
                    .filter_map(|picker| {
                        on_screen(&format!("coding-{picker}-{tool}"))
                            .map(|bounds| f32::from(bounds.center().y).round() as i32)
                    })
                    .collect();
                let one_line = tops.windows(2).all(|pair| pair[0] == pair[1]);
                eprintln!(
                    "coding-pickers-drive: {when}: {tool}: {} ({})",
                    places.join(", "),
                    if one_line { "one line" } else { "WRAPS" }
                );
            }
        };
        wait(3000).await;
        let tool = std::env::var("FILO_DRIVE_TOOL").unwrap_or_else(|_| "opencode".into());
        let (picker, list) = (format!("coding-model-{tool}"), format!("coding-model-search-{tool}-list"));
        // The Muse Code, OpenCode and pi lists come in the background, from their own servers.
        for _ in 0..90 {
            if on_screen(&picker).is_some() && on_screen("coding-model-muse").is_some() {
                break;
            }
            wait(1000).await;
        }
        log("opened");
        shot("coding-opened");
        // Its row may be below the fold: scrolled up to the window's top third first, as a person would.
        if let (Some(width), Some(bounds)) = (window_width(&this, cx), on_screen(&picker)) {
            native_input::hover(width, width / 2., 300.);
            native_input::scroll_in(width, width / 2., 300., f64::from(f32::from(bounds.top())) - 200.);
        }
        wait(1000).await;
        log("scrolled");
        shot("coding-scrolled");
        eprintln!("coding-pickers-drive: clicked {tool}'s model ({})", click(&this, cx, &picker));
        wait(800).await;
        eprintln!("coding-pickers-drive: list {}", place(&list).unwrap_or_else(|| "not open".into()));
        shot("coding-model-list");
        let search = std::env::var("FILO_DRIVE_SEARCH").unwrap_or_else(|_| "free".into());
        type_keys(&this, cx, &search, false);
        wait(800).await;
        eprintln!("coding-pickers-drive: searched {search:?}: list {}", place(&list).unwrap_or_else(|| "none".into()));
        shot("coding-model-searched");
        type_keys(&this, cx, "", true);
        wait(1500).await;
        eprintln!("coding-pickers-drive: after Return: list {}", place(&list).unwrap_or_else(|| "closed".into()));
        log("picked");
        shot("coding-picked");
        let overview = backend.call_raw("coding:overview", json!({})).await.unwrap_or_default();
        let row = overview["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["id"] == tool.as_str())
            .cloned()
            .unwrap_or_default();
        let offered = row["models"].as_array().map_or(0, Vec::len);
        eprintln!("coding-pickers-drive: {tool} offers {offered} models; its model now {}", row["model"]);
        eprintln!("coding-pickers-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=pages-bottom-drive`: each scrolling page (Coding, Settings, Extensions, Notes, Artifacts, Feed, an
/// agent's profile) scrolled as far down as it goes, then checked: its column ends at the window's bottom edge, its
/// content a bottom padding above that, and on the Coding page Projects' last line ("Show N more") at least the page's
/// 48 pt padding above the edge. The Coding page again with every task and project shown, and in two other window
/// sizes. Run it on a config of its own (`FILO_CONFIG_DIR`); a short window (`FILO_WINDOW=1008x400`) makes every page
/// scroll.
fn run_pages_bottom_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let failed = std::rc::Rc::new(std::cell::Cell::new(false));
        let check = |cx: &mut gpui::AsyncWindowContext, page: &str, when: &str| {
            let handle = PAGE_SCROLLS.with(|scrolls| scrolls.borrow().get(page).cloned());
            let Some(handle) = handle else {
                failed.set(true);
                eprintln!("pages-bottom-drive: {page} {when}: no scroll seen FAIL");
                return None;
            };
            handle.set_offset(gpui::point(gpui::px(0.), -handle.max_offset().height));
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            Some((handle, page.to_string(), when.to_string()))
        };
        let report = |cx: &mut gpui::AsyncWindowContext, seen: Option<(gpui::ScrollHandle, String, String)>| {
            let Some((handle, page, when)) = seen else { return };
            let height = this.update_in(cx, |_, window, _| f32::from(window.viewport_size().height)).unwrap_or_default();
            let view = handle.bounds();
            let last = on_screen(&format!("{page}-end"));
            let Some(last) = last else {
                failed.set(true);
                return eprintln!("pages-bottom-drive: {page} {when}: its end isn't on screen FAIL");
            };
            // `<page>-end` is the page's column, its bottom padding included: at the bottom it ends where the scroll does
            // when there's more than a window of it.
            let gap = f32::from(view.bottom()) - f32::from(last.bottom());
            let gap_ok = if f32::from(handle.max_offset().height) > 0. { gap.abs() <= 0.5 } else { gap >= -0.5 };
            // Its content ends a bottom padding above its end, not past it.
            let padding = on_screen(&format!("{page}-content-end")).map(|content| f32::from(last.bottom()) - f32::from(content.bottom()));
            let padding_ok = padding.is_some_and(|padding| padding >= 24.);
            // The Coding page's last line, Projects' "Show N more", inside the window with the page's bottom padding under it.
            let more = (page == "coding").then(|| on_screen("coding-projects-more").or_else(|| on_screen("coding-projects-card"))).flatten();
            let more_gap = more.map(|more| height - f32::from(more.bottom()));
            let ok = more_gap.is_none_or(|more_gap| more_gap >= 48.);
            let ok = ok && f32::from(view.bottom()) <= height + 0.5 && gap_ok && padding_ok;
            failed.set(failed.get() || !ok);
            eprintln!(
                "pages-bottom-drive: {page} {when}: window {height:.0}, scroll {:.0}..{:.0} at {:.0} of {:.0}, column's end at {:.0} (gap {gap:.0}), content's end {} above it{} {}",
                f32::from(view.top()),
                f32::from(view.bottom()),
                -f32::from(handle.offset().y),
                f32::from(handle.max_offset().height),
                f32::from(last.bottom()),
                padding.map(|padding| format!("{padding:.0}")).unwrap_or_else(|| "?".into()),
                more_gap.map(|more_gap| format!(", last line {more_gap:.0} above the window's bottom")).unwrap_or_default(),
                if ok { "PASS" } else { "FAIL" }
            );
        };
        wait(2000).await;
        for (tab, page) in [(Tab::Coding, "coding"), (Tab::Settings, "settings"), (Tab::Connectors, "extensions"), (Tab::Notes, "notes"), (Tab::Artifacts, "artifacts")] {
            this.update_in(cx, |shell, window, cx| shell.open_tab(tab, window, cx)).ok();
            wait(5000).await;
            let seen = check(cx, page, "opened");
            wait(600).await;
            report(cx, seen);
        }
        // The Feed is a list: its last row scrolled as far up as it goes.
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Feed, window, cx)).ok();
        wait(5000).await;
        if let Some(feed) = feed_screen(&this, cx) {
            feed.update(cx, |feed, cx| feed.drive_scroll_into(usize::MAX, 100_000., cx)).ok();
        }
        wait(1000).await;
        let height = this.update_in(cx, |_, window, _| f32::from(window.viewport_size().height)).unwrap_or_default();
        match on_screen("feed-end") {
            Some(last) => {
                let gap = height - f32::from(last.bottom());
                let padding = on_screen("feed-content-end").map(|content| f32::from(last.bottom()) - f32::from(content.bottom()));
                let ok = gap >= -0.5 && padding.is_some_and(|padding| padding >= 24.);
                failed.set(failed.get() || !ok);
                eprintln!("pages-bottom-drive: feed opened: window {height:.0}, last row's end at {:.0} (gap {gap:.0}), content's end {padding:?} above it {}", f32::from(last.bottom()), if ok { "PASS" } else { "FAIL" });
            }
            None => {
                failed.set(true);
                eprintln!("pages-bottom-drive: feed opened: its end isn't on screen FAIL");
            }
        }
        // The main agent's profile (its edit page).
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        wait(3000).await;
        this.update_in(cx, |shell, window, cx| {
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.edit_agent(crate::local_backend::DEFAULT_AGENT_ID.to_string(), window, cx));
            }
        })
        .ok();
        wait(3000).await;
        let seen = check(cx, "agent-profile", "opened");
        wait(600).await;
        report(cx, seen);
        // The Coding page with every task and project shown, then in a smaller window.
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Coding, window, cx)).ok();
        wait(1500).await;
        let opened = this.update_in(cx, |shell, _, cx| shell.coding_screen().is_some_and(|coding| coding.update(cx, |coding, cx| coding.drive_show_all(cx)))).unwrap_or(false);
        wait(1500).await;
        let seen = check(cx, "coding", if opened { "all shown" } else { "all shown (no Show more)" });
        wait(600).await;
        report(cx, seen);
        let original = this.update_in(cx, |_, window, _| window.bounds().size).ok();
        for (width, height) in [(820., 560.), (1000., 700.)] {
            this.update_in(cx, |_, window, _| window.resize(gpui::size(gpui::px(width), gpui::px(height)))).ok();
            wait(1500).await;
            let seen = check(cx, "coding", &format!("resized to {width}x{height}"));
            wait(600).await;
            report(cx, seen);
        }
        if let Some(size) = original {
            this.update_in(cx, |_, window, _| window.resize(size)).ok();
        }
        eprintln!("pages-bottom-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=window-keys-drive`: ⌃⌘F twice and ⌘M with the main agent's composer focused, pressed like a person would
/// (in-app AppKit key events, `native_input`), then Window › Minimize from the menu bar. Logs whether the window went
/// full screen, came back and was minimized after each, brings it back and quits.
fn run_window_keys_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let failed = std::cell::Cell::new(false);
        let check = |step: &str, ok: bool| {
            failed.set(failed.get() || !ok);
            eprintln!("window-keys-drive: {step}: {}", if ok { "PASS" } else { "FAIL" });
        };
        let full_screen = |cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |_, window, _| window.is_fullscreen()).unwrap_or_default()
        };
        let minimized = |cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |_, window, _| is_minimized(window)).unwrap_or_default()
        };
        wait(6000).await;
        let clicked = click(&this, cx, "composer-input");
        wait(600).await;
        let typing = this
            .update_in(cx, |_, window, _| window.context_stack().iter().any(|context| context.contains("Input")))
            .unwrap_or_default();
        eprintln!("window-keys-drive: clicked the composer ({clicked}), a text field has the keyboard: {typing}");
        for (step, want) in [("ctrl-cmd-f into full screen", true), ("ctrl-cmd-f out of full screen", false)] {
            let Some(width) = window_width(&this, cx) else { return eprintln!("window-keys-drive: no window") };
            native_input::key(width, "f", 3, native_input::CONTROL | native_input::COMMAND);
            wait(2500).await;
            check(step, full_screen(cx) == want);
        }
        let Some(width) = window_width(&this, cx) else { return };
        native_input::key(width, "m", 46, native_input::COMMAND);
        wait(1500).await;
        check("cmd-m minimizes", minimized(cx));
        this.update_in(cx, |_, window, _| unminimize(window)).ok();
        wait(1500).await;
        check("back from the Dock", !minimized(cx));
        // Outside an update: the menu's action runs right away and needs the app.
        let picked = click_menu_item("Window", "Minimize");
        eprintln!("window-keys-drive: picked Window › Minimize ({picked})");
        wait(1500).await;
        check("Window › Minimize", minimized(cx));
        this.update_in(cx, |_, window, _| unminimize(window)).ok();
        wait(1000).await;
        eprintln!("window-keys-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=page-keys-drive`: typing and fn+⌃ in the main agent's message box after a web view over the window (a
/// glance, an artifact, the note editor) had the keyboard, pressed like a person would (in-app AppKit events,
/// `native_input`). A page is clicked, so it has the keyboard, then goes: taken out of the window, or hidden as a
/// glance scrolled away is; AppKit leaves the keyboard with the window itself, and keys must still reach the box. Then a
/// page that stays is clicked and the box clicked after it. Logs who has the keyboard and PASS/FAIL per check, and
/// exits 1 when one fails.
fn run_page_keys_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let failed = std::cell::Cell::new(false);
        let check = |step: &str, ok: bool| {
            failed.set(failed.get() || !ok);
            eprintln!("page-keys-drive: {}: {step}", if ok { "PASS" } else { "FAIL" });
        };
        wait(6000).await;
        let (Some(view), Some(width)) = (shown_conversation(&this, cx).1, window_width(&this, cx)) else {
            eprintln!("page-keys-drive: FAIL: no agent chat on screen");
            cx.update(|_, cx| cx.quit()).ok();
            std::process::exit(1);
        };
        let text = |cx: &mut gpui::AsyncWindowContext| {
            view.read_with(cx, |view, cx| view.composer().read(cx).value().to_string()).unwrap_or_default()
        };
        let hint = |cx: &mut gpui::AsyncWindowContext| {
            view.read_with(cx, |view, _| view.composer_hint()).ok().flatten().unwrap_or_default()
        };
        // Posted events are handled in order but not at once: wait (up to 3 s) for what they should do.
        let until =
            async |cx: &mut gpui::AsyncWindowContext, done: &dyn Fn(&mut gpui::AsyncWindowContext) -> bool| -> bool {
                for _ in 0..60 {
                    if done(cx) {
                        return true;
                    }
                    wait(50).await;
                }
                done(cx)
            };
        // Each case from an empty, focused box with the keyboard where the window opened with it.
        let reset = async |cx: &mut gpui::AsyncWindowContext| {
            native_input::give_keyboard(width, None);
            this.update_in(cx, |_, window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx))).ok();
            wait(200).await;
            clear_field(&this, cx);
            until(cx, &|cx| text(cx).is_empty()).await
        };
        // A page over the chat, clicked like a person would, so it has the keyboard.
        let clicked_page = async |cx: &mut gpui::AsyncWindowContext| {
            let page = native_input::add_page(width, (440., 150., 300., 160.))?;
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(300).await;
            native_input::click(width, 590., 230.);
            if !until(cx, &|_| native_input::keyboard(width) == "page").await {
                eprintln!(
                    "page-keys-drive: the click left the keyboard with {}; the page takes it as a click would",
                    native_input::keyboard(width)
                );
                native_input::give_keyboard(width, Some(page));
            }
            eprintln!("page-keys-drive: page clicked, keyboard: {}", native_input::keyboard(width));
            Some(page)
        };
        let chord = native_input::FUNCTION | native_input::CONTROL;

        check(
            &format!("the box is empty and focused to start (keyboard: {})", native_input::keyboard(width)),
            reset(cx).await,
        );
        type_keys(&this, cx, "a", false);
        check("typing reaches the message box", until(cx, &|cx| text(cx) == "a").await);

        for case in ["page taken out", "page hidden"] {
            reset(cx).await;
            let Some(page) = clicked_page(cx).await else {
                check(&format!("{case}: a page goes over the chat"), false);
                continue;
            };
            if case == "page hidden" {
                native_input::hide_page(page);
            } else {
                native_input::remove_page(page);
            }
            // As when a glance or an artifact goes: in a frame GPUI draws.
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(300).await;
            eprintln!("page-keys-drive: {case}, keyboard: {}", native_input::keyboard(width));
            type_keys(&this, cx, "b", false);
            let typed = until(cx, &|cx| text(cx) == "b").await;
            check(
                &format!(
                    "{case}: typing reaches the message box (text {:?}, keyboard {})",
                    text(cx),
                    native_input::keyboard(width)
                ),
                typed,
            );
            native_input::flags(width, chord);
            wait(700).await;
            native_input::flags(width, 0);
            let heard = until(cx, &|cx| hint(cx).starts_with("Clear the message box")).await;
            check(
                &format!("{case}: fn+⌃ held with text reaches the chat, which asks to clear it ({:?})", hint(cx)),
                typed && heard,
            );
            if case == "page hidden" {
                native_input::remove_page(page);
            }
        }

        reset(cx).await;
        if let Some(page) = clicked_page(cx).await {
            let clicked = click(&this, cx, "composer-input");
            wait(300).await;
            eprintln!(
                "page-keys-drive: clicked the box ({clicked}) with the page still there, keyboard: {}",
                native_input::keyboard(width)
            );
            type_keys(&this, cx, "c", false);
            let typed = until(cx, &|cx| text(cx) == "c").await;
            check(
                &format!(
                    "a click in the box takes the keyboard back from a page (text {:?}, keyboard {})",
                    text(cx),
                    native_input::keyboard(width)
                ),
                typed,
            );
            native_input::remove_page(page);
        } else {
            check("a page goes over the chat", false);
        }
        reset(cx).await;

        eprintln!("page-keys-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        cx.update(|_, cx| cx.quit()).ok();
        if failed.get() {
            std::process::exit(1);
        }
    })
    .detach();
}

/// `FILO_OPEN=search-drive`: ⌘P's search box (screens/search.rs), pressed and typed into like a person would (in-app
/// AppKit key events, `native_input`), in the light theme then the dark one: opened empty, then typed `FILO_SEARCH_NAME`
/// ("plan") with ↓ twice, esc, then `FILO_SEARCH_WORD` ("saffron") with ↓ to the last message found (far back in its
/// chat) and ↵, which opens its chat there, flashing it; and ⌘P, "tut", ↵ for a name. Logs the rows and checks each step, saves window screenshots to
/// `FILO_DRIVE_SHOTS` and quits (exit 1 when a check fails). Needs agents whose chats hold those words.
fn run_search_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-search".into());
        std::fs::create_dir_all(&shots).ok();
        let name = std::env::var("FILO_SEARCH_NAME").unwrap_or_else(|_| "plan".into());
        let word = std::env::var("FILO_SEARCH_WORD").unwrap_or_else(|_| "saffron".into());
        let failed = std::cell::Cell::new(false);
        let check = |step: &str, ok: bool| {
            failed.set(failed.get() || !ok);
            eprintln!("search-drive: {step}: {}", if ok { "PASS" } else { "FAIL" });
        };
        let panel = |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.search_panel()).ok().flatten();
        let state = |cx: &mut gpui::AsyncWindowContext| {
            let panel = panel(cx)?;
            cx.update(|_, cx| panel.read(cx).drive_state(cx)).ok()
        };
        let log = |step: &str, cx: &mut gpui::AsyncWindowContext| match state(cx) {
            Some((query, selected, rows)) => {
                eprintln!("search-drive: {step}: query {query:?}, row {} selected of {:?}", selected + 1, rows)
            }
            None => eprintln!("search-drive: {step}: closed"),
        };
        let open_chat = |cx: &mut gpui::AsyncWindowContext| {
            let agents = this.update(cx, |shell, _| shell.agents_screen()).ok().flatten()?;
            cx.update(|_, cx| agents.read(cx).drive_selected()).ok().flatten()
        };
        // After a frame drawn for sure.
        let shoot = async |name: &str, cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(250).await;
            crate::window_shot::save_largest(format!("{shots}/{name}.png"));
        };
        wait(5000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("search-drive: no window") };
        let command_p = || native_input::key(width, "p", 35, native_input::COMMAND);
        let down = || native_input::key(width, "\u{f701}", 125, 0);
        let escape = || native_input::key(width, "\u{1b}", 53, 0);
        let enter = || native_input::key(width, "\r", 36, 0);
        for mode in ["light", "dark"] {
            this.update_in(cx, |_, window, cx| {
                use crate::theme::ActiveTheme as _;
                let mut prefs = cx.theme().prefs.clone();
                prefs.theme_mode = mode.into();
                crate::theme::save_and_apply(prefs, window, cx);
            })
            .ok();
            wait(800).await;
            // Typing in the composer: ⌘P still opens it.
            click(&this, cx, "composer-input");
            wait(400).await;
            command_p();
            wait(900).await;
            log("⌘P", cx);
            check(
                &format!("{mode}: ⌘P opens it, empty, listing every chat"),
                state(cx).is_some_and(|(query, _, rows)| query.is_empty() && rows.len() >= 2),
            );
            shoot(&format!("search-empty-{mode}"), cx).await;
            type_keys(&this, cx, &name, false);
            wait(900).await;
            log(&format!("typed {name:?}"), cx);
            check(
                &format!("{mode}: {name:?} finds names and messages"),
                state(cx).is_some_and(|(_, _, rows)| {
                    rows.iter().any(|row| !row.contains(" › ")) && rows.iter().any(|row| row.contains(" › "))
                }),
            );
            shoot(&format!("search-typed-{mode}"), cx).await;
            down();
            down();
            wait(400).await;
            log("↓↓", cx);
            check(
                &format!("{mode}: ↓↓ moves to the third row"),
                state(cx).is_some_and(|(_, selected, _)| selected == 2),
            );
            shoot(&format!("search-moved-{mode}"), cx).await;
            escape();
            wait(500).await;
            check(&format!("{mode}: esc closes it"), panel(cx).is_none());
            // A message far back in its chat: ↵ opens the chat there, flashing it.
            command_p();
            wait(700).await;
            type_keys(&this, cx, &word, false);
            wait(900).await;
            log(&format!("typed {word:?}"), cx);
            let rows = state(cx).map(|(_, _, rows)| rows).unwrap_or_default();
            check(&format!("{mode}: {word:?} finds a message"), rows.first().is_some_and(|row| row.contains(" › ")));
            // The oldest one found: far back in its chat, which loads back to it.
            for _ in 1..rows.len() {
                down();
            }
            wait(300).await;
            enter();
            let mut flashed = None;
            for _ in 0..40 {
                wait(100).await;
                let agents = this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
                let conversation =
                    agents.and_then(|agents| cx.update(|_, cx| agents.read(cx).current_conversation()).ok().flatten());
                flashed = conversation.and_then(|conversation| {
                    cx.update(|_, cx| conversation.read(cx).drive_highlighted()).ok().flatten()
                });
                if flashed.is_some() {
                    break;
                }
            }
            wait(250).await;
            shoot(&format!("search-opened-message-{mode}"), cx).await;
            eprintln!("search-drive: ↵ opened {:?}, flashing {flashed:?}", open_chat(cx));
            check(
                &format!("{mode}: ↵ opens the chat at the message, flashing it"),
                panel(cx).is_none() && flashed.is_some(),
            );
            wait(1800).await;
            // A name: ⌘P, "tut", ↵.
            command_p();
            wait(700).await;
            type_keys(&this, cx, "tut", false);
            wait(700).await;
            log("typed \"tut\"", cx);
            enter();
            wait(800).await;
            let opened = open_chat(cx);
            eprintln!("search-drive: ↵ opened {opened:?}");
            check(&format!("{mode}: ↵ on a name opens its chat"), panel(cx).is_none() && opened.is_some());
            shoot(&format!("search-opened-chat-{mode}"), cx).await;
        }
        eprintln!("search-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        crate::window_shot::flush(&executor).await;
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            std::process::exit(if failed.get() { 1 } else { 0 });
        }
    })
    .detach();
}

/// `FILO_OPEN=search-button-drive`: the Agents header's magnifier, in the light theme then the dark one: the pointer
/// over it (its "Search ⌘P" tooltip), then a click, which opens ⌘P's search box, and esc. Logs and checks each step,
/// saves window screenshots to `FILO_DRIVE_SHOTS` and quits (exit 1 when a check fails). `FILO_SEARCH_MODES=dark` (or
/// any of `light,dark`, comma-separated) picks the themes.
fn run_search_button_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-search".into());
        std::fs::create_dir_all(&shots).ok();
        let failed = std::cell::Cell::new(false);
        let check = |step: &str, ok: bool| {
            failed.set(failed.get() || !ok);
            eprintln!("search-button-drive: {step}: {}", if ok { "PASS" } else { "FAIL" });
        };
        let open = |cx: &mut gpui::AsyncWindowContext| {
            this.update(cx, |shell, _| shell.search_panel().is_some()).unwrap_or_default()
        };
        wait(5000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("search-button-drive: no window") };
        let modes = std::env::var("FILO_SEARCH_MODES").unwrap_or_else(|_| "light,dark".into());
        for mode in modes.split(',').map(str::to_string) {
            let mode = mode.as_str();
            this.update_in(cx, |_, window, cx| {
                use crate::theme::ActiveTheme as _;
                let mut prefs = cx.theme().prefs.clone();
                prefs.theme_mode = mode.into();
                crate::theme::save_and_apply(prefs, window, cx);
            })
            .ok();
            wait(800).await;
            let Some(button) = on_screen("agents-search") else {
                return eprintln!("search-button-drive: FAIL no button");
            };
            eprintln!("search-button-drive: {mode}: button at {button:?}");
            let center = button.center();
            native_input::hover(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
            wait(1500).await;
            let pointer = cx.update(|window, _| window.mouse_position()).ok();
            eprintln!("search-button-drive: {mode}: pointer at {pointer:?}, keys {:?}", native_input::has_keys(width));
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(250).await;
            crate::window_shot::save_largest(format!("{shots}/agents-header-{mode}.png"));
            check(&format!("{mode}: closed before the click"), !open(cx));
            click(&this, cx, "agents-search");
            wait(900).await;
            check(&format!("{mode}: the magnifier opens the search box"), open(cx));
            this.update_in(cx, |_, window, _| window.refresh()).ok();
            wait(250).await;
            crate::window_shot::save_largest(format!("{shots}/agents-header-clicked-{mode}.png"));
            native_input::key(width, "\u{1b}", 53, 0);
            wait(600).await;
            check(&format!("{mode}: esc closes it"), !open(cx));
            native_input::hover(width, 600., 500.);
            wait(400).await;
        }
        eprintln!("search-button-drive: {}", if failed.get() { "FAIL" } else { "PASS" });
        crate::window_shot::flush(&executor).await;
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            std::process::exit(if failed.get() { 1 } else { 0 });
        }
    })
    .detach();
}

#[allow(unexpected_cfgs)] // objc 0.2's msg_send! checks a cfg this crate doesn't declare
fn ns_window(window: &Window) -> Option<*mut objc::runtime::Object> {
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    let view = handle.ns_view.as_ptr() as *mut objc::runtime::Object;
    // SAFETY: an AppKit message on the main thread.
    let ns_window: *mut objc::runtime::Object = unsafe { msg_send![view, window] };
    (!ns_window.is_null()).then_some(ns_window)
}

/// Whether the window sits in the Dock.
#[allow(unexpected_cfgs)]
fn is_minimized(window: &Window) -> bool {
    use objc::{msg_send, sel, sel_impl};
    let Some(ns_window) = ns_window(window) else { return false };
    // SAFETY: a read-only AppKit message on the main thread.
    let value: objc::runtime::BOOL = unsafe { msg_send![ns_window, isMiniaturized] };
    value == objc::runtime::YES
}

/// Brings the window back from the Dock.
#[allow(unexpected_cfgs)]
fn unminimize(window: &Window) {
    use objc::{msg_send, sel, sel_impl};
    if let Some(ns_window) = ns_window(window) {
        // SAFETY: an AppKit message on the main thread.
        let _: () = unsafe { msg_send![ns_window, deminiaturize: std::ptr::null_mut::<objc::runtime::Object>()] };
    }
}

/// Picks `item` in the menu bar's `menu`, as a click on it would.
#[allow(unexpected_cfgs)]
fn click_menu_item(menu: &str, item: &str) -> bool {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    let string = |text: &str| -> *mut Object {
        let text = std::ffi::CString::new(text).unwrap_or_default();
        unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
    };
    // SAFETY: AppKit messages on the main thread.
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let bar: *mut Object = msg_send![app, mainMenu];
        let entry: *mut Object = msg_send![bar, itemWithTitle: string(menu)];
        if entry.is_null() {
            return false;
        }
        let submenu: *mut Object = msg_send![entry, submenu];
        let index: isize = msg_send![submenu, indexOfItemWithTitle: string(item)];
        if index < 0 {
            return false;
        }
        let _: () = msg_send![submenu, performActionForItemAtIndex: index];
        true
    }
}

/// `FILO_OPEN=files-drive:result:<task id>@<round>` (a coding round's result page) or `files-drive:agent:<agent id>`
/// (that agent's chat): clicks each file row `FILO_FILES` names (`coding-file-<name>` on a result page,
/// `chat-file-<name>` in a chat, comma-separated) like a person would (in-app AppKit events, `native_input`), logging
/// what the viewer shows and saving a shot to `FILO_DRIVE_SHOTS`; then clicks the middle of the viewer (its page: it
/// stays open) and presses Esc (it closes), logging after each.
fn run_files_drive(spec: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let Some(spec) = spec else { return eprintln!("files-drive: give it result:<task>@<round> or agent:<id>") };
    match spec.split_once(':') {
        Some(("result", task)) => {
            let route = match task.split_once('@') {
                Some((task, round)) => Route::CodingResult(task.to_string(), round.parse().ok()),
                None => Route::CodingResult(task.to_string(), None),
            };
            cx.on_next_frame(window, move |shell, window, cx| shell.push(route, window, cx));
        }
        Some(("agent", id)) => {
            let id = id.to_string();
            cx.on_next_frame(window, move |shell, window, cx| {
                shell.open_tab(Tab::Agents, window, cx);
                if let Some(agents) = shell.agents_screen() {
                    agents.update(cx, |agents, cx| agents.open_agent(id.clone(), window, cx));
                }
            });
        }
        _ => return eprintln!("files-drive: give it result:<task>@<round> or agent:<id>"),
    }
    let files: Vec<String> = std::env::var("FILO_FILES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").ok();
        wait(4000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("files-drive: no window") };
        let height = cx.update(|window, _| f64::from(f32::from(window.viewport_size().height))).unwrap_or(760.);
        let report = |step: &str, cx: &mut gpui::AsyncWindowContext| {
            let state = cx.update(|window, cx| crate::media_viewer::smoke_state(window, cx)).unwrap_or_default();
            eprintln!("files-drive: {step}: {state}");
            if let (Some(shots), Some(number)) = (&shots, native_input::window_number(width)) {
                crate::window_shot::save(number, format!("{shots}/{step}.png"));
            }
        };
        report("opened", cx);
        for id in &files {
            let Some(row) = on_screen(id) else {
                eprintln!("files-drive: {id} isn't on the page");
                continue;
            };
            // Below the fold: scrolled to first.
            let top = f64::from(f32::from(row.origin.y));
            if top > height - 120. {
                native_input::hover(width, width / 2., 400.);
                native_input::scroll_in(width, width / 2., 400., top - height / 2.);
                wait(1200).await;
            }
            eprintln!("files-drive: clicked {id} ({})", click(&this, cx, id));
            wait(3000).await;
            report(&format!("{id}-open"), cx);
            native_input::click(width, width / 2., height / 2.);
            wait(800).await;
            report(&format!("{id}-clicked-inside"), cx);
            native_input::key(width, "\u{1b}", 53, 0);
            wait(1200).await;
            report(&format!("{id}-esc"), cx);
        }
        eprintln!("files-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=coding-result-drive:<task id>@<round>`: a coding task's result page and its round switcher in the top
/// bar, used like a person would: ←, then →, through the window's key handling (to what has the keyboard, as a key
/// press is), then a click on round 5, or on ‹ when the switcher counts rather than lists (in-app AppKit events,
/// `native_input`). Saves a window screenshot after each to `FILO_DRIVE_SHOTS`.
fn run_coding_result_drive(id: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let Some(id) = id else { return eprintln!("coding-result-drive: give it a task id") };
    let route = match id.split_once('@') {
        Some((task, round)) => Route::CodingResult(task.to_string(), round.parse().ok()),
        None => Route::CodingResult(id, None),
    };
    cx.on_next_frame(window, move |shell, window, cx| shell.push(route, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-coding-result".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("coding-result-drive: shot {path}");
            }
        };
        wait(4000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("coding-result-drive: no window") };
        snap(width, "result-1-opened");
        // A recording on the page: scrolled to, shown as its poster, then played in the viewer and closed with Esc.
        if let Some(video) = on_screen("coding-result-video") {
            native_input::hover(width, width / 2., 400.);
            native_input::scroll_in(width, width / 2., 400., f64::from(f32::from(video.origin.y)) - 120.);
            wait(1500).await;
            snap(width, "result-video-1-poster");
            eprintln!("coding-result-drive: clicked the recording ({})", click(&this, cx, "coding-result-video"));
            wait(3000).await;
            let state = cx.update(|window, cx| crate::media_viewer::smoke_state(window, cx)).unwrap_or_default();
            eprintln!("coding-result-drive: viewer: {state}");
            snap(width, "result-video-2-playing");
            native_input::key(width, "\u{1b}", 53, 0);
            wait(1000).await;
            let state = cx.update(|window, cx| crate::media_viewer::smoke_state(window, cx)).unwrap_or_default();
            eprintln!("coding-result-drive: after Esc: {state}");
        }
        for (step, key) in [(2, "left"), (3, "right")] {
            let pressed =
                cx.update(|window, cx| window.dispatch_keystroke(gpui::Keystroke::parse(key).expect("key"), cx));
            eprintln!("coding-result-drive: pressed {key} ({pressed:?})");
            wait(1500).await;
            snap(width, &format!("result-{step}-{key}"));
        }
        let target = if on_screen("coding-result-round-5").is_some() {
            "coding-result-round-5"
        } else {
            "coding-result-round-previous"
        };
        eprintln!("coding-result-drive: clicked {target} ({})", click(&this, cx, target));
        wait(1500).await;
        snap(width, "result-4-clicked");
        eprintln!("coding-result-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=post-report-drive:<post id>`: the group, then a click on that post's report row (conversation.rs
/// `report_footer`, in-app AppKit events) and the report's page it opens. Saves a window screenshot of each to
/// `FILO_DRIVE_SHOTS`.
fn run_post_report_drive(id: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let Some(id) = id else { return eprintln!("post-report-drive: give it a post id") };
    let backend = crate::backend::backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        if backend.call_raw("agents:openGroup", json!({})).await.is_err() {
            return eprintln!("post-report-drive: no group");
        }
        this.update_in(cx, |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents
                    .update(cx, |agents, cx| agents.open_agent(crate::screens::agents::GROUP.to_string(), window, cx));
            }
        })
        .ok();
        wait(5000).await;
        let row = format!("report-{id}");
        eprintln!("post-report-drive: report row shown: {}", on_screen(&row).is_some());
        crate::window_shot::save_step("report-1-post");
        eprintln!("post-report-drive: clicked the report row ({})", click(&this, cx, &row));
        wait(2500).await;
        crate::window_shot::save_step("report-2-page");
        crate::window_shot::flush(&executor).await;
        eprintln!("post-report-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=coding-result-share-drive:<task id>@<round>`: a round's result page shared and unshared like a person
/// would: a click on Share (in-app AppKit events, `native_input`), the link it put on the clipboard fetched with curl,
/// then a click on Unshare and the link fetched again. Logs each step and the link's HTTP status, saves the shared page's
/// HTML and window screenshots to `FILO_DRIVE_SHOTS`, and puts the clipboard back as it was. Shares with Filo's server,
/// or `FILO_RESULTS_URL`.
fn run_coding_result_share_drive(id: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let Some(id) = id else { return eprintln!("coding-result-share-drive: give it a task id") };
    let route = match id.split_once('@') {
        Some((task, round)) => Route::CodingResult(task.to_string(), round.parse().ok()),
        None => Route::CodingResult(id, None),
    };
    cx.on_next_frame(window, move |shell, window, cx| shell.push(route, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-coding-result-share".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("coding-result-share-drive: shot {path}");
            }
        };
        // The link's HTTP status, its page kept in `save` when given.
        let fetch = |link: &str, save: Option<String>| {
            let mut curl = std::process::Command::new("/usr/bin/curl");
            curl.args([
                "-sS",
                "--max-time",
                "20",
                "-w",
                "%{http_code}",
                "-o",
                save.as_deref().unwrap_or("/dev/null"),
                link,
            ]);
            curl.output().ok().map(|out| String::from_utf8_lossy(&out.stdout).to_string()).unwrap_or_default()
        };
        let mut passed = true;
        let mut fail = |what: String| {
            passed = false;
            eprintln!("coding-result-share-drive: FAIL {what}");
        };
        wait(4000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("coding-result-share-drive: no window") };
        snap(width, "share-1-opened");
        let clipboard = cx.update(|_, cx| cx.read_from_clipboard()).ok().flatten();
        eprintln!("coding-result-share-drive: clicked Share ({})", click(&this, cx, "coding-result-share"));
        // It asks first, saying what goes: Return shares.
        wait(1500).await;
        snap(width, "share-1b-asked");
        native_input::key(width, "\r", 36, 0);
        wait(300).await;
        snap(width, "share-2-sharing");
        let started = std::time::Instant::now();
        while on_screen("coding-result-unshare").is_none() && started.elapsed() < Duration::from_secs(180) {
            wait(500).await;
        }
        eprintln!("coding-result-share-drive: shared after {:.1} s", started.elapsed().as_secs_f64());
        wait(800).await;
        snap(width, "share-3-shared");
        let link =
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())).ok().flatten().unwrap_or_default();
        eprintln!("coding-result-share-drive: copied {link}");
        if !link.starts_with("https://filo.im/results/") {
            fail(format!("the clipboard has {link:?}, not a filo.im/results link"));
        }
        let shared = fetch(&link, Some(format!("{shots}/shared-page.html")));
        eprintln!("coding-result-share-drive: the link answers {shared} while shared");
        if shared != "200" {
            fail(format!("the shared link answered {shared}"));
        }
        if std::env::var_os("FILO_DRIVE_KEEP_SHARED").is_none() {
            eprintln!("coding-result-share-drive: clicked Unshare ({})", click(&this, cx, "coding-result-unshare"));
            let started = std::time::Instant::now();
            while on_screen("coding-result-share").is_none() && started.elapsed() < Duration::from_secs(60) {
                wait(500).await;
            }
            wait(800).await;
            snap(width, "share-4-unshared");
            let unshared = fetch(&link, None);
            eprintln!("coding-result-share-drive: the link answers {unshared} once unshared");
            if unshared != "404" {
                fail(format!("the unshared link answered {unshared}"));
            }
        }
        if let Some(item) = clipboard {
            cx.update(|_, cx| cx.write_to_clipboard(item)).ok();
        }
        eprintln!("coding-result-share-drive: {}", if passed { "PASS" } else { "FAIL" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=artifact-share-drive:<artifact id>`: an artifact's page shared, changed, shared again and unshared like a
/// person would: a click on Share (in-app AppKit events, `native_input`), the link it copied fetched with curl, and the
/// artifact itself in the frame that page names; then a new version (the page's Restore of version 1, through the
/// backend), a click on Update, the same link fetched for the new version; then Unshare, and both fetched again for
/// a 404. With `FILO_DRIVE_CHROME=<Chrome binary>` headless Chrome draws the link at each version. Logs each step,
/// saves the pages and screenshots to `FILO_DRIVE_SHOTS` and puts the clipboard back. Shares with Filo's server, or
/// `FILO_ARTIFACTS_URL`; `FILO_DRIVE_KEEP_SHARED` leaves it shared.
fn run_artifact_share_drive(id: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let Some(id) = id else { return eprintln!("artifact-share-drive: give it an artifact id") };
    let route = Route::Artifact(id.clone());
    cx.on_next_frame(window, move |shell, window, cx| shell.push(route, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-artifact-share".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("artifact-share-drive: shot {path}");
            }
        };
        // The link's HTTP status and its page, kept as `save` in the shots when given.
        let fetch = |link: &str, save: Option<&str>| {
            let file = save.map(|name| format!("{shots}/{name}"));
            let mut curl = std::process::Command::new("/usr/bin/curl");
            curl.args(["-sS", "--max-time", "20", "-w", "%{http_code}", "-o", file.as_deref().unwrap_or("/dev/null"), link]);
            let status = curl.output().ok().map(|out| String::from_utf8_lossy(&out.stdout).to_string()).unwrap_or_default();
            (status, file.and_then(|file| std::fs::read_to_string(file).ok()).unwrap_or_default())
        };
        // The artifact's own address, from the frame on the link's page.
        let frame_of = |page: &str| page.split("<iframe src=\"").nth(1).and_then(|rest| rest.split('"').next()).unwrap_or_default().to_string();
        let browse = |link: &str, name: &str| {
            let Ok(chrome) = std::env::var("FILO_DRIVE_CHROME") else { return };
            let profile = std::env::temp_dir().join(format!("filo-drive-chrome-{}", uuid::Uuid::new_v4()));
            let path = format!("{shots}/{name}.png");
            let out = std::process::Command::new(chrome)
                .args(["--headless=new", "--disable-gpu", "--hide-scrollbars", "--window-size=1100,800", "--virtual-time-budget=4000"])
                .arg(format!("--user-data-dir={}", profile.display()))
                .arg(format!("--screenshot={path}"))
                .arg(link)
                .output();
            let _ = std::fs::remove_dir_all(&profile);
            eprintln!("artifact-share-drive: Chrome drew {link} to {path} ({:?})", out.map(|out| out.status));
        };
        let mut passed = true;
        let mut fail = |what: String| {
            passed = false;
            eprintln!("artifact-share-drive: FAIL {what}");
        };
        let link_now = |cx: &mut gpui::AsyncWindowContext| cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())).ok().flatten().unwrap_or_default();
        wait(4000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("artifact-share-drive: no window") };
        snap(width, "artifact-share-1-opened");
        let clipboard = cx.update(|_, cx| cx.read_from_clipboard()).ok().flatten();

        eprintln!("artifact-share-drive: clicked Share ({})", click(&this, cx, "artifact-share-link"));
        // It asks first, saying what in the page looks private: Return shares.
        wait(1500).await;
        snap(width, "artifact-share-1b-asked");
        native_input::key(width, "\r", 36, 0);
        let started = std::time::Instant::now();
        while on_screen("artifact-unshare").is_none() && started.elapsed() < Duration::from_secs(60) {
            wait(300).await;
        }
        eprintln!("artifact-share-drive: shared after {:.1} s", started.elapsed().as_secs_f64());
        wait(800).await;
        snap(width, "artifact-share-2-shared");
        let link = link_now(cx);
        eprintln!("artifact-share-drive: copied {link}");
        if !link.starts_with("https://filo.im/artifacts/") {
            fail(format!("the clipboard has {link:?}, not a filo.im/artifacts link"));
        }
        let (status, page) = fetch(&link, Some("shared-v1.html"));
        let frame = frame_of(&page);
        let (frame_status, framed) = fetch(&frame, Some("shared-v1-frame.html"));
        eprintln!("artifact-share-drive: the link answers {status}, its frame {frame} answers {frame_status} ({} bytes)", framed.len());
        if status != "200" || frame_status != "200" || frame.starts_with("https://filo.im/") {
            fail(format!("shared: the link answered {status}, its frame ({frame}) {frame_status}"));
        }
        browse(&link, "browser-v1");

        // A new version: version 1 restored, as the page's menu does.
        let restored = match cx.update(|_, cx| backend(cx)) {
            Ok(backend) => backend.call_raw("canvasArtifacts:restoreVersion", json!({ "id": id, "version": 1 })).await,
            Err(error) => Err(error.to_string()),
        };
        eprintln!("artifact-share-drive: restored version 1 ({restored:?})");
        let started = std::time::Instant::now();
        while on_screen("artifact-update-link").is_none() && started.elapsed() < Duration::from_secs(20) {
            wait(300).await;
        }
        wait(800).await;
        snap(width, "artifact-share-3-changed");
        // A window that doesn't show (on another Space, covered) draws no frames (GPUI stops its display link), so its
        // buttons can't be clicked: then Update goes through the backend, as its button does.
        if click(&this, cx, "artifact-update-link") {
            eprintln!("artifact-share-drive: clicked Update");
            wait(1500).await;
            native_input::key(width, "\r", 36, 0);
        } else {
            eprintln!("artifact-share-drive: the window isn't drawing (on another Space, or covered): Update through the backend, as its button does");
            let shared = match cx.update(|_, cx| backend(cx)) {
                Ok(backend) => backend.call_raw("canvasArtifacts:share", json!({ "id": id })).await,
                Err(error) => Err(error.to_string()),
            };
            eprintln!("artifact-share-drive: canvasArtifacts:share answered {shared:?}");
            if let Some(url) = shared.ok().and_then(|value| value["url"].as_str().map(str::to_string)) {
                cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(url))).ok();
            }
        }
        let started = std::time::Instant::now();
        while on_screen("artifact-update-link").is_some() && started.elapsed() < Duration::from_secs(60) {
            wait(300).await;
        }
        eprintln!("artifact-share-drive: updated after {:.1} s", started.elapsed().as_secs_f64());
        wait(800).await;
        snap(width, "artifact-share-4-updated");
        let again = link_now(cx);
        let (status, page) = fetch(&again, Some("shared-v2.html"));
        let frame_after = frame_of(&page);
        let (frame_status, framed_after) = fetch(&frame_after, Some("shared-v2-frame.html"));
        eprintln!("artifact-share-drive: copied {again}; it answers {status}, its frame {frame_after} answers {frame_status}");
        if again != link || status != "200" || frame_status != "200" || frame_after == frame || framed_after == framed {
            fail(format!("updated: the link {again:?} (was {link:?}) answered {status}, its frame ({frame_after}) {frame_status}"));
        }
        browse(&again, "browser-v2");

        if std::env::var_os("FILO_DRIVE_KEEP_SHARED").is_none() {
            if click(&this, cx, "artifact-unshare") {
                eprintln!("artifact-share-drive: clicked Unshare");
            } else {
                eprintln!("artifact-share-drive: the window isn't drawing: Unshare through the backend, as its button does");
                let unshared = match cx.update(|_, cx| backend(cx)) {
                    Ok(backend) => backend.call_raw("canvasArtifacts:unshare", json!({ "id": id })).await,
                    Err(error) => Err(error.to_string()),
                };
                eprintln!("artifact-share-drive: canvasArtifacts:unshare answered {unshared:?}");
            }
            let started = std::time::Instant::now();
            while on_screen("artifact-share-link").is_none() && started.elapsed() < Duration::from_secs(60) {
                wait(300).await;
            }
            wait(800).await;
            snap(width, "artifact-share-5-unshared");
            let (status, _) = fetch(&link, None);
            let (frame_status, _) = fetch(&frame_after, None);
            eprintln!("artifact-share-drive: once unshared the link answers {status}, its frame {frame_status}");
            if status != "404" || frame_status != "404" {
                fail(format!("unshared: the link answered {status}, its frame {frame_status}"));
            }
        }
        if let Some(item) = clipboard {
            cx.update(|_, cx| cx.write_to_clipboard(item)).ok();
        }
        eprintln!("artifact-share-drive: {}", if passed { "PASS" } else { "FAIL" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=artifact-tooltip-drive:<artifact id>`: the pointer on the artifact page's header buttons, whose tooltips
/// fall over the preview's web view (a native view over everything GPUI draws): each shows whole, through a hole in the
/// web view, and leaves no hole once the pointer moves on. Logs the holes and saves `FILO_DRIVE_SHOTS` pictures.
fn run_artifact_tooltip_drive(id: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    let Some(id) = id else { return eprintln!("artifact-tooltip-drive: give it an artifact id") };
    let route = Route::Artifact(id);
    cx.on_next_frame(window, move |shell, window, cx| shell.push(route, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-artifact-tooltip".into());
        let _ = std::fs::create_dir_all(&shots);
        let mut passed = true;
        wait(4000).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("artifact-tooltip-drive: no window") };
        let holes = |cx: &mut gpui::AsyncWindowContext| {
            this.update(cx, |shell, cx| {
                let page = shell.top_page()?.downcast::<crate::screens::artifact::ArtifactScreen>().ok()?;
                page.read(cx).smoke_web_holes(cx)
            })
            .ok()
            .flatten()
        };
        for (button, name) in
            [("artifact-share", "claude"), ("artifact-claude-link", "link"), ("artifact-export", "download")]
        {
            let Some(bounds) = on_screen(button) else {
                eprintln!("artifact-tooltip-drive: {button} isn't on screen");
                continue;
            };
            let center = bounds.center();
            native_input::hover(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)) + 6.);
            wait(1500).await;
            let shown = holes(cx);
            eprintln!("artifact-tooltip-drive: on {button} the web view's holes are {shown:?}");
            if shown.is_none_or(|holes| holes == 0) {
                passed = false;
                eprintln!("artifact-tooltip-drive: FAIL {button}'s tooltip has no hole in the web view");
            }
            if let Some(number) = native_input::window_number(width) {
                crate::window_shot::save(number, format!("{shots}/artifact-tooltip-{name}.png"));
            }
        }
        // Away from the buttons, into the page: the tooltip goes, and its hole with it.
        native_input::hover(width, width / 2., 400.);
        wait(1000).await;
        let left = holes(cx);
        eprintln!("artifact-tooltip-drive: off the buttons the web view's holes are {left:?}");
        if left.is_some_and(|holes| holes > 0) {
            passed = false;
            eprintln!("artifact-tooltip-drive: FAIL a hole stayed after the tooltip went");
        }
        if let Some(number) = native_input::window_number(width) {
            crate::window_shot::save(number, format!("{shots}/artifact-tooltip-gone.png"));
        }
        eprintln!("artifact-tooltip-drive: {}", if passed { "PASS" } else { "FAIL" });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

thread_local! {
    /// What `overlays-drive` draws over the window (a menu, a tooltip): the shell draws it last (`overlay`).
    static OVERLAY: std::cell::RefCell<Option<std::rc::Rc<dyn Fn(&mut Window, &mut gpui::App) -> gpui::AnyElement>>> = Default::default();
}

/// What a drive draws over the window, after everything else in it (smoke.rs `overlay`).
pub fn overlay(window: &mut Window, cx: &mut gpui::App) -> Option<gpui::AnyElement> {
    OVERLAY.with(|overlay| overlay.borrow().clone()).map(|build| build(window, cx))
}

/// `FILO_OPEN=overlays-drive:<artifact id>,<video file>`: every kind of overlay over every kind of native view, which
/// AppKit draws over everything GPUI draws: an artifact's page, the note editor, a glance opened larger and a video in
/// the viewer, each with an in-app notification card, a toast, a tooltip, a menu and a dialog over it. Each must show
/// through a hole in the view (gpui's `Window::painted_over`, web_holes.rs), AppKit must send a click there to GPUI's
/// view, and a real click must reach it (the card, the menu's item, the dialog's button). A dialog's dim lies over the
/// page and a click on it closes the dialog; with the page holding the keyboard, Esc closes the dialog. With nothing
/// open, nothing is cut. Logs a line per check, saves `FILO_DRIVE_SHOTS` pictures and ends with PASS or FAIL.
fn run_overlays_drive(arg: Option<String>, window: &mut Window, cx: &mut Context<AppShell>) {
    use gpui::{AppContext as _, IntoElement as _, ParentElement as _};
    use gpui_component::WindowExt as _;
    use std::cell::Cell;
    use std::rc::Rc;
    let Some((artifact, video)) =
        arg.as_deref().and_then(|arg| arg.split_once(',')).map(|(a, v)| (a.to_string(), v.to_string()))
    else {
        return eprintln!("overlays-drive: give it <artifact id>,<video file>");
    };
    let _ = window;
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-overlays".into());
        let _ = std::fs::create_dir_all(&shots);
        let failed = Rc::new(Cell::new(0usize));
        let check = |what: String, ok: bool, detail: String| {
            if !ok {
                failed.set(failed.get() + 1);
            }
            eprintln!("overlays-drive: {} {what}: {detail}", if ok { "PASS" } else { "FAIL" });
        };
        wait(1500).await;
        let Some(width) = window_width(&this, cx) else { return eprintln!("overlays-drive: no window") };
        let shot = |name: String| {
            if let Some(number) = native_input::window_number(width) {
                crate::window_shot::save(number, format!("{shots}/{name}.png"));
            }
        };
        // Where the drive's menu and tooltip go: the middle of the window, over each view.
        let (probe_x, probe_y) = (width / 2., 400.);
        let holes = || {
            let (mut holes, tints) = crate::web_holes::window_holes();
            holes.extend(crate::screens::chat::conversation::glance::smoke_window_holes());
            (holes, tints)
        };
        let surfaces = ["artifact", "note-editor", "glance", "video"];
        for surface in surfaces {
            // Open it.
            let opened = match surface {
                "artifact" => this.update_in(cx, |shell, window, cx| shell.push(Route::Artifact(artifact.clone()), window, cx)).is_ok(),
                "note-editor" => this.update_in(cx, |shell, window, cx| shell.push(Route::Note(None), window, cx)).is_ok(),
                "glance" => this
                    .update_in(cx, |shell, window, cx| {
                        shell.back(window, cx);
                        shell.back(window, cx);
                        let data: crate::screens::chat::conversation::glance::GlanceData = serde_json::from_value(json!({
                            "title": "Overlay check",
                            "text": "A glance under overlays.",
                            "document": std::env::var("FILO_DRIVE_GLANCE").ok().and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_else(|| "<!doctype html><html><body style='height:900px;background:#2a6'><h1>Overlay check</h1></body></html>".into()),
                        }))
                        .expect("glance data");
                        let glance = crate::screens::chat::GlanceRef { glance_id: "overlay-glance".into(), title: "Overlay check".into(), as_of: None, icon: None, wide: false };
                        let from = crate::screens::chat::conversation::glance::FeedSheet {
                            copy: String::new(),
                            from: "Overlay check".into(),
                            head: String::new(),
                            ask: Rc::new(|_, _| {}),
                            choose: Rc::new(|_, _, _| {}),
                        };
                        crate::screens::chat::conversation::glance::open_feed_sheet(glance, Rc::new(data), from, window, cx);
                    })
                    .is_ok(),
                _ => cx
                    .update(|window, cx| {
                        window.close_all_dialogs(cx);
                        let item = crate::file_preview::Item { url: String::new(), name: "overlay.mp4".into(), content_type: "video/mp4".into(), file: Some(video.clone().into()) };
                        crate::media_viewer::open(vec![item], 0, None, window, cx);
                    })
                    .is_ok(),
            };
            wait(if surface == "artifact" { 4000 } else { 3000 }).await;
            let (native, _) = native_input::native_at(width, probe_x, probe_y);
            check(format!("{surface}: a native view shows"), opened && native.is_some(), format!("{native:?}"));
            let page = native.as_ref().map(|(_, view)| *view);
            let (before, _) = holes();
            check(format!("{surface}: nothing is cut with nothing open"), before.is_empty(), format!("{before:?}"));
            shot(format!("{surface}-0-alone"));
            // Each overlay: shown, then where it falls over the view is a hole that takes clicks.
            for overlay in ["card", "toast", "tooltip", "menu", "dialog"] {
                let clicked = Rc::new(Cell::new(false));
                let on = clicked.clone();
                cx.update(|window, cx| match overlay {
                    "card" => {
                        let notice = crate::unseen::ReplyNotice { session_id: "overlay-drive".into(), title: "Desktop App Developer".into(), banner: false };
                        let card = crate::in_app_notice::card(&notice).autohide(false).on_click(move |_, _, _| on.set(true));
                        window.push_notification(card, cx);
                    }
                    "toast" => crate::ui::toast(window, cx, "Link copied"),
                    "tooltip" => {
                        let tooltip = cx.new(|_| gpui_component::tooltip::Tooltip::new("Download as HTML"));
                        OVERLAY.with(|slot| {
                            *slot.borrow_mut() = Some(Rc::new(move |_, _| {
                                gpui::deferred(gpui::anchored().position(gpui::point(gpui::px(probe_x as f32), gpui::px(probe_y as f32))).child(tooltip.clone()))
                                    .with_priority(3)
                                    .into_any_element()
                            }))
                        });
                        window.refresh();
                    }
                    "menu" => {
                        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, _, _| {
                            let on = on.clone();
                            menu.item(gpui_component::menu::PopupMenuItem::new("Copy link").on_click(move |_, _, _| on.set(true)))
                        });
                        OVERLAY.with(|slot| {
                            *slot.borrow_mut() = Some(Rc::new(move |_, _| {
                                gpui::deferred(gpui::anchored().position(gpui::point(gpui::px(probe_x as f32), gpui::px(probe_y as f32))).child(menu.clone()))
                                    .with_priority(3)
                                    .into_any_element()
                            }))
                        });
                        window.refresh();
                    }
                    _ => crate::ui::confirm(window, cx, "Restore version 1?", "The page goes back to how it was then.", "Restore", "Cancel", false, move |_, _| on.set(true)),
                })
                .ok();
                wait(1200).await;
                let (cut, tints) = holes();
                let area = |bounds: &gpui::Bounds<gpui::Pixels>| f32::from(bounds.size.width) * f32::from(bounds.size.height);
                let hole = cut.iter().max_by(|a, b| area(a).total_cmp(&area(b))).copied();
                // A notification or toast shows at the window's top right: over this view only if it reaches there.
                let over = !matches!(overlay, "card" | "toast") || [20., 40., 60.].iter().any(|y| native_input::native_at(width, width - 226., *y).0.is_some());
                if !over {
                    eprintln!("overlays-drive: SKIP {surface}: the {overlay} doesn't fall over the view (nothing to cut): {cut:?}");
                    cx.update(|window, cx| window.clear_notifications(cx)).ok();
                    wait(800).await;
                    continue;
                }
                check(format!("{surface}: the {overlay} has a hole in the view"), hole.is_some(), format!("{cut:?}"));
                let name = format!("{surface}-{overlay}");
                shot(name.clone());
                let Some(hole) = hole else { continue };
                let f = |value: gpui::Pixels| f64::from(f32::from(value));
                let (x, y) = (f(hole.center().x), f(hole.center().y));
                let (native, taker) = native_input::native_at(width, x, y);
                // A video's view takes no clicks itself: they go on to GPUI's view under it.
                let reaches = taker == "view" || (surface == "video" && native.is_some());
                check(format!("{surface}: AppKit sends a click on the {overlay} to GPUI"), native.is_some() && reaches, format!("at ({x:.0}, {y:.0}) over {native:?}, taken by {taker}"));
                match overlay {
                    "card" | "menu" => {
                        native_input::click(width, x, y);
                        wait(600).await;
                        check(format!("{surface}: a click on the {overlay} reaches it"), clicked.get(), String::new());
                    }
                    "dialog" => {
                        if surface != "glance" {
                            check(format!("{surface}: the dialog's dim lies over the view"), tints > 0, format!("{tints} tints"));
                        }
                        // Its Restore button, at the dialog's bottom right.
                        let (bx, by) = (f(hole.origin.x + hole.size.width) - 4. - 67., f(hole.origin.y + hole.size.height) - 4. - 39.);
                        native_input::click(width, bx, by);
                        wait(800).await;
                        let (left, _) = holes();
                        check(format!("{surface}: a click on the dialog's button reaches it"), clicked.get() && left.is_empty(), format!("clicked {}, holes left {left:?}", clicked.get()));
                        // Again: the page under its dim is out of reach, a click there is GPUI's (the dim's).
                        cx.update(|window, cx| crate::ui::confirm(window, cx, "Restore version 1?", "The page goes back to how it was then.", "Restore", "Cancel", false, |_, _| {})).ok();
                        wait(1200).await;
                        let (native, taker) = native_input::native_at(width, probe_x, probe_y);
                        check(format!("{surface}: under the dialog, a click on the view goes to GPUI"), native.is_some() && (taker == "view" || surface == "video"), format!("over {native:?}, taken by {taker}"));
                        // Esc closes it, the keyboard with GPUI.
                        native_input::key(width, "\u{1b}", 53, 0);
                        wait(800).await;
                        let (left, _) = holes();
                        check(format!("{surface}: Esc closes the dialog"), left.is_empty(), format!("keyboard {}, holes left {left:?}", native_input::keyboard(width)));
                        if !left.is_empty() {
                            cx.update(|window, cx| window.close_dialog(cx)).ok();
                            wait(800).await;
                        }
                        // With the page holding the keyboard, a dialog opening takes it, and Esc closes the dialog.
                        if let (Some(page), true) = (page, matches!(surface, "artifact" | "note-editor")) {
                            native_input::give_keyboard(width, Some(page));
                            wait(300).await;
                            let held = native_input::keyboard(width);
                            cx.update(|window, cx| crate::ui::confirm(window, cx, "Restore version 1?", "The page goes back to how it was then.", "Restore", "Cancel", false, |_, _| {})).ok();
                            wait(1000).await;
                            let taken = native_input::keyboard(width);
                            native_input::key(width, "\u{1b}", 53, 0);
                            wait(800).await;
                            let open = cx.update(|window, cx| window.has_active_dialog(cx)).unwrap_or(true);
                            check(format!("{surface}: Esc closes a dialog opened while the page had the keyboard"), held == "page" && taken == "view" && !open, format!("keyboard {held} then {taken}, dialog open {open}"));
                        }
                    }
                    _ => {}
                }
                // Gone: no hole left.
                OVERLAY.with(|slot| slot.borrow_mut().take());
                cx.update(|window, cx| {
                    window.clear_notifications(cx);
                    if matches!(surface, "artifact" | "note-editor") {
                        window.close_all_dialogs(cx);
                    }
                    window.refresh();
                })
                .ok();
                wait(1000).await;
                let (left, _) = holes();
                check(format!("{surface}: no hole is left once the {overlay} goes"), left.is_empty(), format!("{left:?}"));
            }
        }
        cx.update(|window, cx| window.close_all_dialogs(cx)).ok();
        let failed = failed.get();
        eprintln!("overlays-drive: {}", if failed == 0 { "PASS".to_string() } else { format!("FAIL ({failed} checks)") });
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=agents-drive`: the Agents tab's list edge, dragged with mouse events through the window like a person
/// would (in-app AppKit events, `native_input`): wider, past both limits, on a narrow window, then back to how it was.
/// Logs the widths and `desktop-agents.json` after each step and saves window screenshots to `FILO_DRIVE_SHOTS`. With
/// `FILO_DRIVE_AGENT=<name>` it then opens that agent's chat and logs its work indicator every second for
/// `FILO_DRIVE_WATCH` seconds (default 60), with a screenshot each time it changes: send the agent a message meanwhile
/// (`filo send "…" --session $(filo agents open <id>)`) to watch a reply start and finish.
fn run_agents_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-agents".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("agents-drive: shot {path}");
            }
        };
        let saved = || {
            std::fs::read_to_string(crate::config::config_dir().join("desktop-agents.json"))
                .map_or("(no file)".to_string(), |text| text.trim().to_string())
        };
        // The tabs are built as the window starts: open it once that has settled.
        wait(6000).await;
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        wait(5000).await;
        // The tab as it is now, and its list on screen.
        let screen =
            |cx: &mut gpui::AsyncWindowContext| this.update(cx, |shell, _| shell.agents_screen()).ok().flatten();
        for _ in 0..60 {
            if on_screen("agents-list-edge").is_some() {
                break;
            }
            this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
            wait(500).await;
        }
        if screen(cx).is_none() {
            return eprintln!("agents-drive: no Agents tab");
        }
        let widths =
            |cx: &mut gpui::AsyncWindowContext| screen(cx)?.read_with(cx, |agents, _| agents.list_widths()).ok();
        let log = |cx: &mut gpui::AsyncWindowContext, label: &str| {
            let content = crate::ui::content_width();
            eprintln!(
                "agents-drive: {label}: list (drawn, chosen) {:?}, tab {content} wide, file {}",
                widths(cx),
                saved()
            );
        };
        let Some(width) = window_width(&this, cx) else { return eprintln!("agents-drive: no window") };
        let before = widths(cx).map(|(_, chosen)| chosen);
        log(cx, "start");
        snap(width, "agents-01-start");
        // Down on the edge, then the pointer moves `by` points in steps, a frame apart, and lets go.
        let drag = |by: f64, shot: Option<&'static str>| {
            let snap = &snap;
            let wait = &wait;
            async move {
                let Some(bounds) = on_screen("agents-list-edge") else {
                    return eprintln!("agents-drive: FAIL the edge isn't on screen");
                };
                let center = bounds.center();
                let (x, y) = (f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
                native_input::drag_step(width, native_input::Drag::Down, x, y);
                wait(120).await;
                for step in 1..=10 {
                    native_input::drag_step(width, native_input::Drag::Move, x + by * f64::from(step) / 10., y);
                    wait(40).await;
                }
                if let Some(name) = shot {
                    wait(200).await;
                    snap(width, name);
                }
                native_input::drag_step(width, native_input::Drag::Up, x + by, y);
                wait(500).await;
            }
        };
        drag(120., Some("agents-02-dragging-wider")).await;
        log(cx, "dragged 120 pt right");
        snap(width, "agents-03-wider");
        drag(-600., None).await;
        log(cx, "dragged far left (stops at the minimum)");
        snap(width, "agents-04-narrowest");
        drag(600., None).await;
        log(cx, "dragged far right (stops at the maximum)");
        snap(width, "agents-05-widest");
        // A narrow window: the list gives way so the chat keeps its room.
        let original = this.update_in(cx, |_, window, _| window.bounds().size).ok();
        this.update_in(cx, |_, window, _| window.resize(gpui::size(gpui::px(860.), gpui::px(720.)))).ok();
        wait(1200).await;
        log(cx, "window 860 pt wide");
        snap(860., "agents-06-narrow-window");
        if let Some(size) = original {
            this.update_in(cx, |_, window, _| window.resize(size)).ok();
            wait(1200).await;
        }
        log(cx, "window back to its size (the chosen width again)");
        // A double-click on the edge: the usual width.
        if let Some(bounds) = on_screen("agents-list-edge") {
            let center = bounds.center();
            native_input::double_click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)));
        }
        wait(800).await;
        log(cx, "double-clicked the edge (the usual width)");
        // Back to how it was before the check.
        if let Some(before) = before.filter(|before| (before - 280.).abs() > 0.5) {
            let _ = std::fs::write(
                crate::config::config_dir().join("desktop-agents.json"),
                format!("{}\n", json!({ "listWidth": before.round() })),
            );
        }
        if let Ok(name) = std::env::var("FILO_DRIVE_AGENT") {
            let Some(agents) = screen(cx) else { return eprintln!("agents-drive: no Agents tab") };
            let id = agents.read_with(cx, |agents, _| agents.agent_named(&name)).ok().flatten();
            let Some(id) = id else { return eprintln!("agents-drive: no agent named {name}") };
            this.update_in(cx, |_, window, cx| {
                agents.update(cx, |agents, cx| agents.open_agent(id.clone(), window, cx))
            })
            .ok();
            wait(3000).await;
            snap(width, "agents-10-agent-chat");
            let seconds: u64 =
                std::env::var("FILO_DRIVE_WATCH").ok().and_then(|value| value.parse().ok()).unwrap_or(60);
            let mut last: Option<(Option<String>, bool)> = None;
            let mut index = 11;
            for second in 0..seconds {
                let now = screen(cx)
                    .and_then(|agents| agents.read_with(cx, |agents, cx| agents.work_status(cx)).ok())
                    .unwrap_or_default();
                if last.as_ref() != Some(&now) {
                    eprintln!("agents-drive: {second}s: indicator {:?}, reply under way {}", now.0, now.1);
                    wait(300).await;
                    snap(width, &format!("agents-{index}-status-{}", if now.1 { "replying" } else { "idle" }));
                    index += 1;
                    last = Some(now);
                }
                wait(1000).await;
            }
        }
        eprintln!("agents-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// What a conversation's composer holds for the note-share check: its chat, what's typed, the notes waiting above it
/// (key, whole body) and whether a message is on its way.
#[derive(Clone, Debug, PartialEq)]
struct ComposerState {
    session: Option<String>,
    text: String,
    notes: Vec<(String, String)>,
    sending: bool,
}

type ConversationView = gpui::Entity<crate::screens::chat::conversation::Conversation>;

fn composer_state(conversation: &ConversationView, cx: &mut gpui::AsyncWindowContext) -> Option<ComposerState> {
    conversation
        .read_with(cx, |conversation, cx| ComposerState {
            session: conversation.session_id().map(str::to_string),
            text: conversation.composer().read(cx).value().to_string(),
            notes: conversation.shared_notes().iter().map(|note| (note.key.clone(), note.body.clone())).collect(),
            sending: conversation.delivering(),
        })
        .ok()
}

/// The tab on screen and the conversation it shows (Agents).
fn shown_conversation(
    this: &gpui::WeakEntity<AppShell>,
    cx: &mut gpui::AsyncWindowContext,
) -> (Option<Tab>, Option<ConversationView>) {
    this.update(cx, |shell, cx| {
        let tab = shell.current_tab();
        let conversation = match tab {
            Tab::Agents => shell.agents_screen().and_then(|agents| agents.read(cx).current_conversation()),
            _ => None,
        };
        (Some(tab), conversation)
    })
    .unwrap_or((None, None))
}

/// A right click at the middle of `id` (in the latest frame).
fn right_click(this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, id: &str) -> bool {
    let Some(bounds) = on_screen(id) else {
        eprintln!("right click: {id} isn't on screen");
        return false;
    };
    let Some(width) = window_width(this, cx) else { return false };
    let center = bounds.center();
    native_input::right_click(width, f64::from(f32::from(center.x)), f64::from(f32::from(center.y)))
}

/// `FILO_OPEN=drafts-drive`: agents left with something typed go on top of the Agents list (chat/drafts.rs), with
/// fixture data (scripts/drafts-check.sh runs it offline in a sandbox). Types to the agent Pip with keys: it stays put
/// while open; leaving it for Juno puts it on top; back with Pip, its text is there and it stays on top; cleared with the
/// keyboard, it drops back. Logs PASS/FAIL per check, saves screenshots to `FILO_DRIVE_SHOTS` and exits
/// 1 when any check fails. Nothing can be sent: it runs offline.
fn run_drafts_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-drafts".into());
        let _ = std::fs::create_dir_all(&shots);
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        let (pip, juno) = (env("FILO_DRIVE_AGENT_ID"), env("FILO_DRIVE_AGENT_2"));
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: String| {
            eprintln!("drafts: {} {what}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                failures.push(what);
            }
        };
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("drafts: shot {path}");
            }
        };
        // The two agents' order, under the group and the main agent.
        let agent_list = |this: &gpui::WeakEntity<AppShell>,
                          cx: &mut gpui::AsyncWindowContext|
         -> (Vec<String>, Vec<(String, String)>) {
            let (mut order, drafts) = this
                .update(cx, |shell, cx| {
                    shell.agents_screen().map(|agents| (agents.read(cx).list_order(), agents.read(cx).drafts()))
                })
                .ok()
                .flatten()
                .unwrap_or_default();
            order.retain(|key| *key == pip || *key == juno);
            (order, drafts)
        };
        let open_agent = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, id: &str| {
            this.update_in(cx, |shell, window, cx| {
                if let Some(agents) = shell.agents_screen() {
                    agents.update(cx, |agents, cx| agents.open_agent(id.to_string(), window, cx));
                }
            })
            .ok();
        };
        let type_draft = async |this: &gpui::WeakEntity<AppShell>,
                                cx: &mut gpui::AsyncWindowContext,
                                text: &str|
               -> Option<ConversationView> {
            let (_, conversation) = shown_conversation(this, cx);
            let conversation = conversation?;
            this.update_in(cx, |_, window, cx| {
                conversation.update(cx, |conversation, cx| conversation.focus_composer(window, cx))
            })
            .ok();
            wait(300).await;
            type_keys(this, cx, text, false);
            wait(400).await;
            Some(conversation)
        };
        let shown_text = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| -> String {
            shown_conversation(this, cx)
                .1
                .and_then(|view| composer_state(&view, cx))
                .map(|state| state.text)
                .unwrap_or_default()
        };

        // The tabs build at launch: wait for the agents to load.
        wait(3000).await;
        for _ in 0..60 {
            let targets = cx.update(|_, cx| crate::nav::share_targets(cx)).unwrap_or_default();
            if targets.len() >= 2 {
                break;
            }
            this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
            wait(500).await;
        }
        let Some(width) = window_width(&this, cx) else { return eprintln!("drafts: no window") };

        // Agents: Pip, below Juno, typed into and left goes on top.
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        open_agent(&this, cx, &pip);
        wait(1200).await;
        type_draft(&this, cx, "Draft for Pip").await;
        let (order, drafts) = agent_list(&this, cx);
        check(
            order == [juno.clone(), pip.clone()] && drafts.is_empty(),
            format!("typing to Pip leaves the list as it is (order {order:?}, drafts {drafts:?})"),
        );
        open_agent(&this, cx, &juno);
        wait(1200).await;
        let (order, drafts) = agent_list(&this, cx);
        check(
            order == [pip.clone(), juno.clone()] && drafts == [(pip.clone(), "Draft for Pip".to_string())],
            format!("left with a draft, Pip is on top (order {order:?}, drafts {drafts:?})"),
        );
        snap(width, "agents-draft-on-top");
        open_agent(&this, cx, &pip);
        wait(1200).await;
        let (text, (order, _)) = (shown_text(&this, cx), agent_list(&this, cx));
        check(
            text == "Draft for Pip" && order == [pip.clone(), juno.clone()],
            format!("back with Pip the draft is kept ({text:?}) and Pip stays on top (order {order:?})"),
        );
        clear_field(&this, cx);
        wait(500).await;
        let (order, drafts) = agent_list(&this, cx);
        check(
            order == [juno.clone(), pip.clone()] && drafts.is_empty(),
            format!("cleared, Pip drops back (order {order:?}, drafts {drafts:?})"),
        );

        if failures.is_empty() {
            eprintln!("drafts: all checks passed");
        } else {
            eprintln!("drafts: {} failed: {failures:?}", failures.len());
        }
        let code = i32::from(!failures.is_empty());
        crate::window_shot::flush(cx.background_executor()).await;
        cx.update(|_, cx| cx.quit()).ok();
        std::process::exit(code);
    })
    .detach();
}

/// `FILO_OPEN=skill-editor-drive`: the skill editor (screens/skill_editor.rs, the Notes page's editor), by real clicks
/// and keys on the window. New skill: types a name, Return, when to use it, Return, and 30 lines of
/// instructions (a Markdown list, typed as source); the body grows a line at a time and the page follows. The draft is
/// kept; leaving the page and opening New skill again (drafts read back from the file) brings back all three fields.
/// Save creates the skill, drops the draft and shows it to read, its Markdown drawn; Edit shows the
/// source again, a line typed at the end is kept as the skill's draft and Save replaces the skill with exactly that. The
/// test skill is removed and the owner's own New skill draft put back at the end. Logs PASS/FAIL per check, saves
/// screenshots to `FILO_DRIVE_SHOTS` and exits 1 when any check fails.
fn run_skill_editor_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::screens::skill_editor;
    const NAME: &str = "filo-editor-check";
    const DESCRIPTION: &str = "When checking the desktop skill editor by hand";
    let backend = backend(cx);
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-skill-editor".into());
        let _ = std::fs::create_dir_all(&shots);
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: String| {
            eprintln!("skill-editor: {} {what}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                failures.push(what);
            }
        };
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("skill-editor: shot {path}");
            }
        };
        let skills = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            this.update(cx, |shell, cx| shell.connectors_screen().and_then(|abilities| abilities.read(cx).skills())).ok().flatten()
        };
        let editor = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            skills(this, cx).and_then(|skills| cx.update(|_, cx| skills.read(cx).editor()).ok().flatten())
        };
        let values = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            editor(this, cx).and_then(|editor| cx.update(|_, cx| editor.read(cx).values().clone()).ok())
        };
        let draft = |cx: &mut gpui::AsyncWindowContext, name: Option<&str>| cx.update(|_, cx| skill_editor::draft(cx, name)).ok().flatten();
        let server_body = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            skills(this, cx).and_then(|skills| cx.update(|_, cx| skills.read(cx).skill(NAME)).ok().flatten()).map(|skill| skill.body)
        };
        // Clicks `id` at `dx` points from its left edge and `dy` from its top (window points).
        let click_at = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, id: &str, dx: f32, dy: f32| {
            let (Some(bounds), Some(width)) = (on_screen(id), window_width(this, cx)) else {
                eprintln!("skill-editor: {id} isn't on screen");
                return false;
            };
            eprintln!("skill-editor: click {id} {bounds:?} +({dx}, {dy})");
            native_input::click(width, f64::from(f32::from(bounds.origin.x) + dx), f64::from(f32::from(bounds.origin.y) + dy))
        };

        wait(3000).await;
        // The owner's own New skill draft, put back at the end; the test skill gone from an earlier run.
        let owner_draft = draft(cx, None);
        cx.update(|_, cx| skill_editor::put(cx, None, None)).ok();
        cx.update(|_, cx| skill_editor::put(cx, Some(NAME), None)).ok();
        let _ = backend.call_raw("skills:remove", json!({ "name": NAME })).await;

        this.update_in(cx, |shell, window, cx| shell.open_skills(window, cx)).ok();
        for _ in 0..40 {
            wait(500).await;
            if on_screen("skills-new").is_some() {
                break;
            }
        }
        let Some(width) = window_width(&this, cx) else { return eprintln!("skill-editor: no window") };
        // Opens the page by the New skill button and waits for it to draw.
        let open_new = async |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            // Clicked again until the page is there: the first launch after a build is slow to start the web view.
            for tick in 0..120 {
                if tick % 8 == 0 && editor(this, cx).is_none() {
                    click(this, cx, "skills-new");
                }
                wait(250).await;
                let painted = editor(this, cx).and_then(|editor| cx.update(|_, cx| editor.read(cx).painted()).ok()).unwrap_or(false);
                if painted && on_screen("skill-page").is_some() {
                    break;
                }
            }
            wait(500).await;
        };
        let page_state = async |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| -> Value {
            let Some(editor) = editor(this, cx) else { return Value::Null };
            let Some(receiver) = cx.update(|_, cx| editor.read(cx).page_state(cx)).ok().flatten() else { return Value::Null };
            // The page's value is a JSON string holding the state.
            let text = receiver.await.unwrap_or_default();
            serde_json::from_str::<String>(&text).ok().and_then(|inner| serde_json::from_str(&inner).ok()).unwrap_or(Value::Null)
        };
        let number = |state: &Value, key: &str| state.get(key).and_then(Value::as_f64).unwrap_or(0.);

        open_new(&this, cx).await;
        let state = page_state(&this, cx).await;
        check(state.get("title").and_then(Value::as_str) == Some(""), format!("New skill opens the Notes editor, empty ({state})"));
        snap(width, "1-new-skill-empty");

        // Name, Return, when to use it, Return, then the instructions: all typed.
        let Some(page) = on_screen("skill-page") else { return eprintln!("skill-editor: no page") };
        // The page's column (web/note.html #note): 760 wide in the middle, 24 in and 28 down.
        let left = ((f32::from(page.size.width) - 760.).max(0.) / 2.) + 24.;
        click_at(&this, cx, "skill-page", left + 40., 40.);
        wait(300).await;
        type_keys(&this, cx, NAME, true);
        wait(200).await;
        type_keys(&this, cx, DESCRIPTION, true);
        wait(200).await;
        let mut heights = Vec::new();
        for line in 1..=30 {
            type_keys(&this, cx, &format!("- Step {line}: do **the** `thing`"), line < 30);
            wait(120).await;
            // From line 15 on, past the body's least height (web/note.html: 40% of the page).
            if line == 15 || line == 30 {
                heights.push(number(&page_state(&this, cx).await, "bodyHeight"));
            }
        }
        wait(800).await;
        let typed = values(&this, cx).unwrap_or_default();
        let lines = typed.body.lines().filter(|line| !line.trim().is_empty()).count();
        check(typed.name == NAME && typed.description == DESCRIPTION, format!("name and when to use it typed, Return moving on ({:?}, {:?})", typed.name, typed.description));
        check(lines == 30, format!("30 lines typed into the instructions ({lines} there)"));
        let line = (heights.get(1).copied().unwrap_or(0.) - heights.first().copied().unwrap_or(0.)) / 15.;
        check(line > 15. && line < 40., format!("the instructions grow a line at a time as they're typed ({heights:?} px at 15 and 30 lines, {line:.1} px a line)"));
        let state = page_state(&this, cx).await;
        let (scroll, viewport, height) = (number(&state, "scrollY"), number(&state, "viewport"), number(&state, "pageHeight"));
        check(height <= viewport || scroll > 0., format!("the page follows the typing (at {scroll} of {height} px, {viewport} px shown)"));
        snap(width, "2-typed");

        let kept = draft(cx, None);
        check(kept.as_ref() == Some(&typed), format!("kept as the New skill draft ({:?})", kept.map(|draft| draft.name)));

        // Closed by the breadcrumb, the drafts read again from the file, and New skill again: all back.
        click_at(&this, cx, "skill-crumbs", 14., 10.);
        wait(800).await;
        check(editor(&this, cx).is_none(), "the breadcrumb goes back to the list".into());
        cx.update(|_, cx| skill_editor::forget_loaded(cx)).ok();
        open_new(&this, cx).await;
        let state = page_state(&this, cx).await;
        let shown_body = state.get("body").and_then(Value::as_str).unwrap_or_default().to_string();
        check(
            state.get("title").and_then(Value::as_str) == Some(NAME) && state.get("description").and_then(Value::as_str) == Some(DESCRIPTION) && shown_body == typed.body,
            format!("reopened, the draft is back in the page ({:?}, {:?}, {} lines)", state.get("title"), state.get("description"), shown_body.lines().count()),
        );
        snap(width, "3-reopened-draft");

        // Save creates it and drops the draft.
        click(&this, cx, "skill-save");
        for _ in 0..40 {
            wait(250).await;
            if server_body(&this, cx).is_some() {
                break;
            }
        }
        let saved = server_body(&this, cx);
        check(saved.as_deref() == Some(typed.body.trim()), format!("Save creates the skill ({} lines on the server)", saved.unwrap_or_default().lines().count()));
        let (new_draft, named) = (draft(cx, None), editor(&this, cx).and_then(|editor| cx.update(|_, cx| editor.read(cx).name().map(str::to_string)).ok().flatten()));
        check(new_draft.is_none() && named.as_deref() == Some(NAME), format!("the draft is gone and the page is the saved skill's ({named:?})"));
        wait(500).await;
        let editing = editor(&this, cx).and_then(|editor| cx.update(|_, cx| editor.read(cx).editing()).ok()).unwrap_or(true);
        let state = page_state(&this, cx).await;
        let drawn = |tag: &str| state.pointer(&format!("/rendered/{tag}")).and_then(Value::as_u64).unwrap_or(0);
        check(
            !editing && state.get("mode").and_then(Value::as_str) == Some("read") && (drawn("li"), drawn("strong"), drawn("code")) == (30, 30, 30),
            format!("saved, the skill shows to read, its Markdown drawn ({:?})", state.get("rendered")),
        );
        snap(width, "4-saved");

        // Edit, a line typed at the end: kept as the skill's draft, then Save replaces the skill with just that added.
        let saved_body = server_body(&this, cx).unwrap_or_default();
        click(&this, cx, "skill-edit");
        wait(500).await;
        let state = page_state(&this, cx).await;
        check(state.get("mode").and_then(Value::as_str) == Some("edit") && state.get("body").and_then(Value::as_str) == Some(saved_body.as_str()), "Edit shows the skill's source".into());
        click_at(&this, cx, "skill-page", left + 40., 200.);
        wait(300).await;
        native_input::key(width, "\u{F701}", 125, native_input::COMMAND);
        wait(200).await;
        type_keys(&this, cx, "", true);
        type_keys(&this, cx, "Step 31: check it saved", false);
        wait(1000).await;
        let changed = draft(cx, Some(NAME));
        check(changed.as_ref().is_some_and(|draft| draft.body.ends_with("Step 31: check it saved")), format!("a change is kept as the skill's draft ({:?})", changed.map(|draft| draft.body.lines().last().map(str::to_string))));
        click(&this, cx, "skill-save");
        for _ in 0..40 {
            wait(250).await;
            if server_body(&this, cx).is_some_and(|body| body.contains("Step 31")) {
                break;
            }
        }
        let replaced = server_body(&this, cx).unwrap_or_default();
        check(replaced == format!("{saved_body}\nStep 31: check it saved"), format!("Save replaces the skill, only the typed line added ({} lines)", replaced.lines().count()));
        wait(500).await;
        let state = page_state(&this, cx).await;
        check(state.get("mode").and_then(Value::as_str) == Some("read"), "saved, back to reading".into());
        check(draft(cx, Some(NAME)).is_none(), "saved, the skill's draft is gone".into());
        snap(width, "5-replaced");

        let removed = backend.call_raw("skills:remove", json!({ "name": NAME })).await;
        check(removed.is_ok(), format!("the test skill is removed ({removed:?})"));
        cx.update(|_, cx| skill_editor::put(cx, None, owner_draft)).ok();

        if failures.is_empty() {
            eprintln!("skill-editor: all checks passed");
        } else {
            eprintln!("skill-editor: {} failed: {failures:?}", failures.len());
        }
        let code = i32::from(!failures.is_empty());
        crate::window_shot::flush(cx.background_executor()).await;
        cx.update(|_, cx| cx.quit()).ok();
        std::process::exit(code);
    })
    .detach();
}

/// `FILO_OPEN=note-share-drive`: the Notes tab's share menu, by real clicks and keys on the window, with
/// fixture data (scripts/note-share-check.sh runs it offline in a sandbox). Types a draft to the agent; a left click on
/// a note opens it and builds no menu; a right click opens the menu and no page; the keyboard picks the agent. Its
/// conversation shows with its draft kept and the note's whole text in a chip, nothing sent; the chip's × takes the note
/// off and leaves the draft; no note changed. Logs PASS/FAIL per check, saves screenshots to `FILO_DRIVE_SHOTS` and exits 1 when any check fails.
fn run_note_share_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-note-share".into());
        let _ = std::fs::create_dir_all(&shots);
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        let (note_1, agent) = (env("FILO_DRIVE_NOTE_1"), env("FILO_DRIVE_AGENT_ID"));
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: String| {
            eprintln!("note-share: {} {what}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                failures.push(what);
            }
        };
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("note-share: shot {path}");
            }
        };
        let keys = |width: f64, keys: &[(&str, u16)]| {
            for (characters, code) in keys {
                native_input::key(width, characters, *code, 0);
            }
        };
        const DOWN: (&str, u16) = ("\u{F701}", 125);
        const RIGHT: (&str, u16) = ("\u{F703}", 124);
        const ENTER: (&str, u16) = ("\r", 36);
        const ESCAPE: (&str, u16) = ("\u{1b}", 53);

        // The tabs build at launch: wait for the agents to load.
        wait(3000).await;
        let mut targets = Vec::new();
        for _ in 0..60 {
            targets = cx.update(|_, cx| crate::nav::share_targets(cx)).unwrap_or_default();
            if !targets.is_empty() {
                break;
            }
            this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
            wait(500).await;
        }
        eprintln!("note-share: targets {targets:?}");
        check(!targets.is_empty(), "the fixture's agent loaded".into());
        let Some(width) = window_width(&this, cx) else { return eprintln!("note-share: no window") };

        // Drafts typed with keys into each conversation a note goes to.
        let mut drafts: Vec<(&str, ConversationView, String)> = Vec::new();
        let type_draft = async |this: &gpui::WeakEntity<AppShell>,
                                cx: &mut gpui::AsyncWindowContext,
                                text: &str|
               -> Option<ConversationView> {
            let (_, conversation) = shown_conversation(this, cx);
            let conversation = conversation?;
            this.update_in(cx, |_, window, cx| {
                conversation.update(cx, |conversation, cx| conversation.focus_composer(window, cx))
            })
            .ok();
            wait(300).await;
            type_keys(this, cx, text, false);
            wait(400).await;
            Some(conversation)
        };
        this.update_in(cx, |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.open_agent(agent.clone(), window, cx));
            }
        })
        .ok();
        wait(1200).await;
        if let Some(view) = type_draft(&this, cx, "Draft for Pip").await {
            drafts.push(("agent", view, "Draft for Pip".into()));
        }
        for (name, view, text) in &drafts {
            let state = composer_state(view, cx);
            check(
                state.as_ref().is_some_and(|state| state.text == *text && state.notes.is_empty()),
                format!("{name}: typed draft {text:?} (got {state:?})"),
            );
        }
        check(drafts.len() == 1, format!("{} conversations got a draft", drafts.len()));

        let open_notes =
            async |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext, row: &str| -> bool {
                this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Notes, window, cx)).ok();
                for _ in 0..40 {
                    if on_screen(row).is_some() {
                        return true;
                    }
                    wait(250).await;
                }
                false
            };
        let row_1 = format!("note-row-{note_1}");
        check(open_notes(&this, cx, &row_1).await, "the Notes tab lists the fixture notes".into());
        // What the notes are now, to check none changed at the end.
        let notes_backend = cx.update(|_, cx| backend(cx)).ok();
        let notes_before = match &notes_backend {
            Some(backend) => backend.call_raw("notes:mine", json!({})).await.ok(),
            None => None,
        };
        let bodies: std::collections::HashMap<String, String> = std::env::var("FILO_DRIVE_BODIES")
            .ok()
            .and_then(|path| serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok())
            .unwrap_or_default();

        // A left click opens the note and builds no menu.
        NOTE_SHARE_ITEMS.lock().unwrap().clear();
        click(&this, cx, &row_1);
        wait(800).await;
        let page = this
            .update(cx, |shell, _| shell.top_page().map(|page| page.downcast::<NoteScreen>().is_ok()))
            .ok()
            .flatten();
        let built = NOTE_SHARE_ITEMS.lock().unwrap().clone();
        check(
            page == Some(true) && built.is_empty(),
            format!("left click opens the note (page {page:?}) and no share menu (items {built:?})"),
        );
        snap(width, "note-share-01-left-click-opens-note");
        this.update_in(cx, |shell, window, cx| shell.back(window, cx)).ok();
        wait(600).await;

        // Each destination: a right click (no page, the menu), then the keyboard picks it.
        let mut expected_menu = vec!["Share to agent".to_string()];
        expected_menu.extend(targets.iter().map(|target| target.name.clone()));
        let agent_index = targets.iter().position(|target| target.id == agent).unwrap_or(0);
        // Down selects the first item; Right opens its submenu on its first item.
        let mut to_agent = vec![DOWN, RIGHT];
        to_agent.extend(std::iter::repeat_n(DOWN, agent_index));
        to_agent.push(ENTER);
        let routes: Vec<(&str, &str, Vec<(&str, u16)>, &str)> = vec![("agent", note_1.as_str(), to_agent, "agent")];
        let mut shared: Vec<(&str, &str)> = Vec::new();
        for (step, (label, note, route, target)) in routes.iter().enumerate() {
            let row = &row_1;
            check(open_notes(&this, cx, row).await, format!("{label}: the note's row is on screen"));
            NOTE_SHARE_ITEMS.lock().unwrap().clear();
            right_click(&this, cx, row);
            wait(800).await;
            let page = this.update(cx, |shell, _| shell.top_page().is_some()).unwrap_or(true);
            let built = NOTE_SHARE_ITEMS.lock().unwrap().clone();
            let menu_ok = !built.is_empty()
                && built.len().is_multiple_of(expected_menu.len())
                && built.chunks(expected_menu.len()).all(|chunk| chunk == expected_menu.as_slice());
            check(
                !page && menu_ok,
                format!(
                    "{label}: right click opens the menu {expected_menu:?} (built {built:?}) and no page (page {page})"
                ),
            );
            if step == 0 {
                snap(width, "note-share-02-right-click-menu");
            }
            keys(width, &route[..route.len() - 1]);
            wait(500).await;
            snap(width, &format!("note-share-0{}-submenu-{}", step + 2, target.replace(' ', "-")));
            keys(width, &[ENTER]);
            wait(1200).await;
            let (tab, view) = shown_conversation(&this, cx);
            let expected_tab = Tab::Agents;
            let draft = drafts.iter().find(|(name, _, _)| name == target);
            let same_view = draft.is_some_and(|(_, draft_view, _)| view.as_ref() == Some(draft_view));
            let state = view.as_ref().and_then(|view| composer_state(view, cx));
            check(
                tab == Some(expected_tab) && same_view,
                format!("{label}: shows {target}'s conversation (tab {tab:?}, same view {same_view})"),
            );
            let text = draft.map(|(_, _, text)| text.clone()).unwrap_or_default();
            let body = bodies.get(*note).cloned().unwrap_or_default();
            let ok = state.as_ref().is_some_and(|state| {
                state.text == text && !state.sending && state.notes == vec![(note.to_string(), body.clone())]
            });
            check(
                ok && !body.is_empty(),
                format!(
                    "{label}: draft kept, one chip with the note's whole text ({} chars), nothing sent (got {state:?})",
                    body.chars().count()
                ),
            );
            snap(width, &format!("note-share-1{step}-shared-to-{}", target.replace(' ', "-")));
            shared.push((target, note));
        }

        // Nothing went out anywhere: every conversation keeps its draft and its chip.
        for (name, view, text) in &drafts {
            let state = composer_state(view, cx);
            let note = shared.iter().find(|(target, _)| target == name).map(|(_, note)| note.to_string());
            let ok = state.as_ref().is_some_and(|state| {
                state.text == *text
                    && !state.sending
                    && state.notes.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>()
                        == note.iter().cloned().collect::<Vec<_>>()
            });
            check(ok, format!("{name}: still holds its draft and chip, nothing sent (got {state:?})"));
        }

        // The chip's × takes the note off; the draft stays.
        let (_, view) = shown_conversation(&this, cx);
        if let Some((target, note)) = shared.last().copied() {
            let remove = format!("remove-shared-note-{note}");
            let on = on_screen(&remove).is_some();
            check(on, format!("{target}: the chip's × is on screen"));
            snap(width, "note-share-20-chip-before-remove");
            click(&this, cx, &remove);
            wait(800).await;
            let state = view.as_ref().and_then(|view| composer_state(view, cx));
            let text =
                drafts.iter().find(|(name, _, _)| *name == target).map(|(_, _, text)| text.clone()).unwrap_or_default();
            check(
                state.as_ref().is_some_and(|state| state.notes.is_empty() && state.text == text && !state.sending),
                format!("{target}: × removed the chip, draft kept, nothing sent (got {state:?})"),
            );
            snap(width, "note-share-21-chip-removed");
        }

        // The notes themselves didn't change: nothing waiting to be saved.
        let notes_after = match &notes_backend {
            Some(backend) => backend.call_raw("notes:mine", json!({})).await.ok(),
            None => None,
        };
        check(notes_before.is_some() && notes_after == notes_before, "no note changed".into());
        keys(width, &[ESCAPE]);

        if failures.is_empty() {
            eprintln!("note-share: all checks passed");
        } else {
            eprintln!("note-share: {} failed: {failures:?}", failures.len());
        }
        let code = i32::from(!failures.is_empty());
        crate::window_shot::flush(cx.background_executor()).await;
        cx.update(|_, cx| cx.quit()).ok();
        std::process::exit(code);
    })
    .detach();
}

/// `FILO_OPEN=feed-chat`: "Ask Filo" beside the posts, by clicks and keys. Scrolls down a little, clicks a post's Ask
/// Filo (the Feed keeps its place, nothing is sent), types a question, clicks another post's Ask Filo and back (each
/// keeps its own draft), closes and reopens the panel. With `FILO_CHAT_SEND="<question>"` it then replaces the draft
/// with that question, presses Enter (a real send) and waits for Filo's reply in the panel.
fn run_feed_chat(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Feed, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        wait(6000).await;
        let Some(feed) = feed_screen(&this, cx) else { return eprintln!("feed-chat: no feed") };
        scroll_wheel(&this, cx, gpui::point(gpui::px(500.), gpui::px(500.)), 1400.);
        wait(1500).await;
        if feed.read_with(cx, |feed, _| feed.drive_scroll_top().0).unwrap_or_default() == 0 {
            eprintln!("feed-chat: the wheel event didn't move the list; scrolling it programmatically instead");
            feed.update(cx, |feed, cx| feed.drive_scroll_row(4, cx)).ok();
            wait(1500).await;
        }
        let ids = feed.read_with(cx, |feed, _| feed.drive_post_ids()).unwrap_or_default();
        let before = feed.read_with(cx, |feed, _| feed.drive_scroll_top()).unwrap_or_default();
        let Some(first) = first_on_screen("ask-", &ids) else { return eprintln!("feed-chat: no Ask Filo on screen") };
        let clicked = click(&this, cx, &format!("ask-{first}"));
        wait(1500).await;
        let after = feed.read_with(cx, |feed, _| feed.drive_scroll_top()).unwrap_or_default();
        let Some(chat) = feed.read_with(cx, |feed, _| feed.drive_chat()).ok().flatten() else {
            return eprintln!("feed-chat: clicked Ask Filo ({clicked}) but the panel didn't open");
        };
        let session = chat.read_with(cx, |chat, _| chat.drive_session()).ok().flatten();
        eprintln!("feed-chat: clicked Ask Filo on post {first}: panel open, chat started: {session:?} (nothing sent); feed top {before:?} -> {after:?}");
        shot("chat-01-opened-nothing-sent");
        click(&this, cx, "composer-input");
        type_keys(&this, cx, "What is the main finding", false);
        wait(800).await;
        let draft = chat.read_with(cx, |chat, cx| chat.drive_draft(cx)).unwrap_or_default();
        eprintln!("feed-chat: typed into the panel: {draft:?}");
        shot("chat-02-typed");
        let others: Vec<String> = ids.iter().filter(|id| **id != first).cloned().collect();
        if let Some(other) = first_on_screen("ask-", &others) {
            click(&this, cx, &format!("ask-{other}"));
            wait(800).await;
            let (shown, draft) = chat.read_with(cx, |chat, cx| (chat.drive_posts(), chat.drive_draft(cx))).unwrap_or_default();
            eprintln!("feed-chat: clicked Add to chat on post {other}: panel about {shown:?}, draft kept {draft:?}");
            shot("chat-03-other-post");
            click(&this, cx, &format!("ask-{first}"));
            wait(800).await;
            let open = feed.read_with(cx, |feed, _| feed.drive_chat().is_some()).unwrap_or(true);
            eprintln!("feed-chat: clicked Ask Filo on post {first} again: panel open {open}");
            if !open {
                click(&this, cx, &format!("ask-{first}"));
                wait(800).await;
            }
        }
        click(&this, cx, "feed-chat-close");
        wait(800).await;
        let open = feed.read_with(cx, |feed, _| feed.drive_chat().is_some()).unwrap_or(true);
        eprintln!("feed-chat: clicked Close: panel open {open}");
        shot("chat-04-closed");
        click(&this, cx, &format!("ask-{first}"));
        wait(1000).await;
        let draft = chat.read_with(cx, |chat, cx| chat.drive_draft(cx)).unwrap_or_default();
        let top = feed.read_with(cx, |feed, _| feed.drive_scroll_top()).unwrap_or_default();
        eprintln!("feed-chat: reopened on post {first}: draft kept {draft:?}; feed top {top:?}");
        shot("chat-05-reopened-draft-kept");
        if let Ok(question) = std::env::var("FILO_CHAT_SEND") {
            click(&this, cx, "composer-input");
            clear_field(&this, cx);
            type_keys(&this, cx, &question, true);
            wait(500).await;
            let sending = chat.read_with(cx, |chat, cx| chat.drive_sending(cx)).unwrap_or(false);
            eprintln!("feed-chat: pressed Enter: sending {sending}");
            shot("chat-06-sending");
            let started = std::time::Instant::now();
            let mut reply = None;
            while started.elapsed() < Duration::from_secs(150) {
                wait(1000).await;
                let conversation = chat.read_with(cx, |chat, _| chat.drive_conversation()).ok().flatten();
                if let Some(conversation) = conversation {
                    let (latest, replying) =
                        conversation.read_with(cx, |view, _| (view.latest_reply(), view.replying())).unwrap_or((None, true));
                    if let (Some(latest), false) = (latest, replying) {
                        reply = Some(latest);
                        break;
                    }
                }
            }
            let session = chat.read_with(cx, |chat, _| chat.drive_session()).ok().flatten();
            match reply {
                Some((id, _, text)) => eprintln!("feed-chat: sent in chat {session:?}; reply in {id}: {:?}", text.chars().take(400).collect::<String>()),
                None => eprintln!("feed-chat: sent in chat {session:?}; no reply within 150 s"),
            }
            wait(1500).await;
            shot("chat-07-reply");
            let top = feed.read_with(cx, |feed, _| feed.drive_scroll_top()).unwrap_or_default();
            eprintln!("feed-chat: feed top after the reply {top:?}");
        }
        eprintln!("feed-chat: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=feed-refresh`: a topic's Refresh, by clicks, against a stand-in backend (no research runs): clicks the
/// first topic, then Refresh (asking: the button spins), the topic reported running (a second click asks nothing),
/// finished with two posts, and failed; then a Refresh the backend refuses (the daily limit) and one that errors.
fn run_feed_refresh(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Feed, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        wait(6000).await;
        let Some(feed) = feed_screen(&this, cx) else { return eprintln!("feed-refresh: no feed") };
        let topics = feed.read_with(cx, |feed, _| feed.drive_topics()).unwrap_or_default();
        let Some(key) = topics.first().cloned() else { return eprintln!("feed-refresh: no topics") };
        feed.update(cx, |feed, _| feed.drive_mock_refresh(Some(Ok("started".into())))).ok();
        click(&this, cx, "topic-0");
        wait(1500).await;
        shot("refresh-01-topic-selected");
        click(&this, cx, "feed-topic-refresh");
        wait(250).await;
        shot("refresh-02-clicked-asking");
        wait(1000).await;
        let topic = key.clone();
        feed.update(cx, |feed, cx| feed.drive_topic_state(&topic, true, None, None, cx)).ok();
        wait(600).await;
        let asks = feed.read_with(cx, |feed, _| feed.drive_refresh_asks()).unwrap_or_default();
        click(&this, cx, "feed-topic-refresh");
        click(&this, cx, "feed-topic-refresh");
        wait(600).await;
        let again = feed.read_with(cx, |feed, _| feed.drive_refresh_asks()).unwrap_or_default();
        eprintln!("feed-refresh: Refresh asks before two more clicks while running: {asks}, after: {again}");
        shot("refresh-03-running");
        let topic = key.clone();
        feed.update(cx, |feed, cx| feed.drive_topic_state(&topic, false, Some(2.), None, cx)).ok();
        wait(800).await;
        shot("refresh-04-finished");
        let topic = key.clone();
        feed.update(cx, |feed, cx| {
            feed.drive_topic_state(&topic, false, Some(0.), Some("Search is not answering right now.".into()), cx)
        })
        .ok();
        wait(800).await;
        shot("refresh-05-run-failed");
        feed.update(cx, |feed, _| {
            feed.drive_forget_pending();
            feed.drive_mock_refresh(Some(Ok("limited".into())));
        })
        .ok();
        click(&this, cx, "feed-topic-refresh");
        wait(1200).await;
        shot("refresh-06-limited");
        feed.update(cx, |feed, _| {
            feed.drive_forget_pending();
            feed.drive_mock_refresh(Some(Err("Couldn't reach Filo. Check your connection.".into())));
        })
        .ok();
        click(&this, cx, "feed-topic-refresh");
        wait(1200).await;
        shot("refresh-07-error");
        // The keyboard: the "For you" chip and a post's More, focused as Tab would, then Space and Return (real key
        // events through AppKit).
        feed.update_in(cx, |feed, window, cx| feed.drive_focus("topic-all", window, cx)).ok();
        wait(400).await;
        shot("refresh-08-chip-focused");
        if let Some(width) = window_width(&this, cx) {
            native_input::key(width, " ", 49, 0);
        }
        wait(800).await;
        let selected = feed.read_with(cx, |feed, _| feed.drive_selected_topic()).ok().flatten();
        eprintln!("feed-refresh: Space on the focused For you chip: topic now {selected:?} (None is For you)");
        // The first post of For you (read after the topic changed: the one on screen now).
        let first_post = feed.read_with(cx, |feed, _| feed.drive_post_ids().first().cloned()).ok().flatten();
        if let Some(post) = first_post {
            let key = format!("more-{post}");
            feed.update_in(cx, |feed, window, cx| feed.drive_focus(&key, window, cx)).ok();
            wait(400).await;
            if let Some(width) = window_width(&this, cx) {
                native_input::key(width, "\r", 36, 0);
            }
            wait(800).await;
            let open = feed.read_with(cx, |feed, _| feed.drive_expanded(&post)).unwrap_or(false);
            eprintln!("feed-refresh: Return on the focused More: open {open}");
            shot("refresh-09-more-opened-by-keyboard");
            if let Some(width) = window_width(&this, cx) {
                native_input::key(width, " ", 49, 0);
            }
            wait(800).await;
            let open = feed.read_with(cx, |feed, _| feed.drive_expanded(&post)).unwrap_or(false);
            eprintln!("feed-refresh: then Space on the focused More: open {open}");
            shot("refresh-10-more-toggled-by-space");
        }
        let asks = feed.read_with(cx, |feed, _| feed.drive_refresh_asks()).unwrap_or_default();
        eprintln!("feed-refresh: Refresh asks in all: {asks}");
        eprintln!("feed-refresh: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

/// `FILO_OPEN=voice-keys-drive`: the voice message shortcuts in an agent's chat and in a chat, with fixture data
/// (scripts/voice-keys-check.sh runs it offline in a sandbox). With the message box focused, fn+⌃ and Esc go in as
/// real key events posted inside the app: held with text, fn+⌃ asks to clear the box; a tap leaves nothing recording
/// or kept; held on an empty box it reaches the recorder (a bare binary without microphone access says to start Filo as an app; one that
/// can record does, and Esc while holding throws the recording away); the other conversation hears none of it. The
/// open shortcut held (the commands hotkeys.rs sends) records in the chat on screen, the agent's too, staying on its
/// tab; with none on screen it opens the Chats tab as before. Each step starts from an empty box with no recording.
/// Posted keys reach the window only while it's the key window: when another app comes forward mid-step (the user
/// at work on the Mac), the step is logged as interrupted, with the app that took the keys, and run again (three
/// tries). Nothing can be sent (offline). Logs PASS/FAIL per check and exits 1 when any fails.
fn run_voice_keys_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    use crate::companion::{self, Command};
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        let (agent, chat_a) = (env("FILO_DRIVE_AGENT_ID"), env("FILO_DRIVE_CHAT_A"));
        let begun = std::time::Instant::now();
        let mut failures: Vec<String> = Vec::new();
        let width = std::cell::Cell::new(0.);
        let mut check = |ok: bool, what: String| {
            let (active, key) = native_input::has_keys(width.get());
            eprintln!(
                "voice-keys: {} {what} [{:.1}s, app active {active}, window key {key}]",
                if ok { "PASS" } else { "FAIL" },
                begun.elapsed().as_secs_f32()
            );
            if !ok {
                failures.push(what);
            }
        };
        let open_agent = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |shell, window, cx| {
                shell.open_tab(Tab::Agents, window, cx);
                if let Some(agents) = shell.agents_screen() {
                    agents.update(cx, |agents, cx| agents.open_agent(agent.clone(), window, cx));
                }
            })
            .ok();
        };
        let open_chat = |this: &gpui::WeakEntity<AppShell>, cx: &mut gpui::AsyncWindowContext| {
            this.update_in(cx, |shell, window, cx| shell.open_chat(chat_a.clone(), window, cx)).ok();
        };

        // The window builds its tabs as it starts: wait for the chat and the agent to show.
        wait(3000).await;
        let (mut chat, mut pip) = (None, None);
        for _ in 0..40 {
            open_chat(&this, cx);
            wait(400).await;
            chat = shown_conversation(&this, cx).1;
            open_agent(&this, cx);
            wait(400).await;
            pip = shown_conversation(&this, cx).1;
            if chat.is_some() && pip.is_some() {
                break;
            }
        }
        let Some(found) = window_width(&this, cx) else { return eprintln!("voice-keys: no window") };
        width.set(found);
        let width = found;
        // Who has the keys, logged whenever that changes; `lost` counts the times the window stops being key.
        let lost = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let taker = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        cx.spawn({
            let (lost, taker) = (lost.clone(), taker.clone());
            async move |cx| {
                let mut last: Option<((bool, bool), String)> = None;
                loop {
                    let now = (native_input::has_keys(width), native_input::frontmost());
                    if last.as_ref() != Some(&now) {
                        let ((active, key), front) = &now;
                        eprintln!("voice-keys: focus [{:.2}s] app active {active}, window key {key}, in front: {front}", begun.elapsed().as_secs_f32());
                        if !key && last.as_ref().is_none_or(|((_, was_key), _)| *was_key) {
                            lost.set(lost.get() + 1);
                            *taker.borrow_mut() = front.clone();
                        }
                        last = Some(now);
                    }
                    cx.background_executor().timer(Duration::from_millis(20)).await;
                }
            }
        })
        .detach();
        let (Some(chat), Some(pip)) = (chat.clone(), pip.clone()) else {
            check(false, format!("the fixture's chat and agent show (chat {}, agent {})", chat.is_some(), pip.is_some()));
            cx.update(|_, cx| cx.quit()).ok();
            std::process::exit(1);
        };
        check(chat.entity_id() != pip.entity_id(), "the chat and the agent's chat are different conversations".into());
        let hint = |view: &ConversationView, cx: &mut gpui::AsyncWindowContext| view.read_with(cx, |view, _| view.composer_hint()).ok().flatten();
        let recording = |view: &ConversationView, cx: &mut gpui::AsyncWindowContext| view.read_with(cx, |view, _| view.recording()).unwrap_or(false);
        let text = |view: &ConversationView, cx: &mut gpui::AsyncWindowContext| {
            view.read_with(cx, |view, cx| view.composer().read(cx).value().to_string()).unwrap_or_default()
        };
        let reaches_recorder = |hint: Option<&str>| hint.is_some_and(|h| h.contains("runs as an app") || h.contains("Microphone"));
        // Posted events are handled in order but not at once: wait (up to 3 s) for what they should do.
        let until = async |cx: &mut gpui::AsyncWindowContext, done: &dyn Fn(&mut gpui::AsyncWindowContext) -> bool| -> bool {
            for _ in 0..60 {
                if done(cx) {
                    return true;
                }
                wait(50).await;
            }
            done(cx)
        };
        let chord = native_input::FUNCTION | native_input::CONTROL;

        for (name, view, other) in [("agent", &pip, &chat), ("chat", &chat, &pip)] {
            if name == "agent" {
                open_agent(&this, cx);
            } else {
                open_chat(&this, cx);
            }
            wait(800).await;
            check(shown_conversation(&this, cx).1.is_some_and(|shown| shown.entity_id() == view.entity_id()), format!("{name}: on screen"));
            let other_before = (hint(other, cx), text(other, cx));
            for step in ["held with text", "tap", "held, then Esc"] {
                for attempt in 1..=3 {
                    // From an empty box with no recording and no note (typing clears the note), the box focused.
                    view.update(cx, |view, cx| view.cancel_voice(cx)).ok();
                    this.update_in(cx, |_, window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx))).ok();
                    wait(200).await;
                    clear_field(&this, cx);
                    type_keys(&this, cx, "x", false);
                    until(cx, &|cx| text(view, cx) == "x").await;
                    clear_field(&this, cx);
                    let ready = until(cx, &|cx| text(view, cx).is_empty() && hint(view, cx).is_none()).await;
                    let lost_before = lost.get();
                    let (ok, what) = match step {
                        "held with text" => {
                            type_keys(&this, cx, "hi", false);
                            let typed = until(cx, &|cx| text(view, cx) == "hi").await;
                            native_input::flags(width, chord);
                            wait(700).await;
                            native_input::flags(width, 0);
                            let asked = until(cx, &|cx| hint(view, cx).is_some_and(|h| h.starts_with("Clear the message box"))).await;
                            (typed && asked, format!("{name}: fn+⌃ held with text asks to clear it (typed {typed}, {:?})", hint(view, cx)))
                        }
                        "tap" => {
                            native_input::flags(width, chord);
                            wait(100).await;
                            native_input::flags(width, 0);
                            // Nothing left, however long it's waited for. It reaches the recorder at once (a build that
                            // can't record says so), and one that can drops a press that short.
                            wait(1000).await;
                            let kept = view.read_with(cx, |view, cx| view.has_unsent_work(cx)).unwrap_or(true);
                            let note = hint(view, cx);
                            (
                                (note.is_none() || reaches_recorder(note.as_deref())) && !recording(view, cx) && !kept,
                                format!("{name}: a fn+⌃ tap leaves nothing recording or kept ({note:?}, kept {kept})"),
                            )
                        }
                        _ => {
                            native_input::flags(width, chord);
                            let reached = until(cx, &|cx| recording(view, cx) || reaches_recorder(hint(view, cx).as_deref())).await;
                            if recording(view, cx) {
                                native_input::key(width, "\u{1b}", 53, chord);
                                let stopped = until(cx, &|cx| !recording(view, cx)).await;
                                native_input::flags(width, 0);
                                wait(500).await;
                                let kept = view.read_with(cx, |view, cx| view.has_unsent_work(cx)).unwrap_or(true);
                                (stopped && !kept, format!("{name}: fn+⌃ held records, Esc while holding throws it away (stopped {stopped}, kept {kept})"))
                            } else {
                                native_input::flags(width, 0);
                                eprintln!("voice-keys: {name}: Esc while recording not checked: this build can't record");
                                (reached, format!("{name}: fn+⌃ held reaches the recorder ({:?})", hint(view, cx)))
                            }
                        }
                    };
                    let what = if ready { what } else { format!("{what}; the box didn't empty first") };
                    if lost.get() != lost_before {
                        let interrupted = format!("{name}: {step}: interrupted, {} took the keys mid-step ({what})", taker.borrow());
                        if attempt < 3 {
                            eprintln!("voice-keys: RETRY {interrupted}");
                            continue;
                        }
                        check(false, format!("{interrupted}, three times"));
                        break;
                    }
                    check(ok && ready, what);
                    break;
                }
            }
            let other_after = (hint(other, cx), text(other, cx));
            check(
                other_after == other_before && !recording(other, cx),
                format!("{name}: the other conversation heard none of it ({other_before:?} → {other_after:?})"),
            );
        }

        // The open shortcut held, as hotkeys.rs sends it, with the agent's message box focused.
        open_agent(&this, cx);
        wait(800).await;
        pip.update(cx, |view, cx| view.cancel_voice(cx)).ok();
        this.update_in(cx, |_, window, cx| pip.update(cx, |view, cx| view.focus_composer(window, cx))).ok();
        wait(300).await;
        check(hint(&pip, cx).is_none() && text(&pip, cx).is_empty(), format!("open shortcut: agent's box empty, no note ({:?})", hint(&pip, cx)));
        let chat_before = (hint(&chat, cx), recording(&chat, cx));
        cx.update(|_, cx| companion::run(Command::TalkStart, cx)).ok();
        let started = until(cx, &|cx| recording(&pip, cx) || reaches_recorder(hint(&pip, cx).as_deref())).await;
        let (tab, _) = shown_conversation(&this, cx);
        let was_recording = recording(&pip, cx);
        cx.update(|_, cx| companion::run(Command::TalkCancel, cx)).ok();
        cx.update(|_, cx| companion::run(Command::TalkEnd, cx)).ok();
        let stopped = until(cx, &|cx| !recording(&pip, cx)).await;
        check(
            tab == Some(Tab::Agents) && started && (hint(&chat, cx), recording(&chat, cx)) == chat_before,
            format!("open shortcut held on the agent's chat: stays on {tab:?}, reaches its recorder {started} (recording {was_recording}), not the chat's"),
        );
        check(stopped, "open shortcut: cancelled, nothing left recording".into());

        // With no chat on screen it opens the Chats tab, as before.
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Notes, window, cx)).ok();
        wait(500).await;
        cx.update(|_, cx| companion::run(Command::TalkStart, cx)).ok();
        let opened = until(cx, &|cx| shown_conversation(&this, cx).1.is_some_and(|shown| shown.entity_id() == chat.entity_id())).await;
        cx.update(|_, cx| companion::run(Command::TalkCancel, cx)).ok();
        cx.update(|_, cx| companion::run(Command::TalkEnd, cx)).ok();
        let stopped = until(cx, &|cx| !recording(&chat, cx)).await;
        check(opened && stopped, format!("open shortcut held on the Notes tab opens the Chats tab's chat ({:?})", shown_conversation(&this, cx).0));

        let code = i32::from(!failures.is_empty());
        eprintln!("voice-keys: done, {} failed", failures.len());
        crate::window_shot::flush(cx.background_executor()).await;
        cx.update(|_, cx| cx.quit()).ok();
        std::process::exit(code);
    })
    .detach();
}

/// `FILO_OPEN=chip-mention-drive`: a picture attached in an agent's composer keeps its chip and its picture when an
/// "@" is typed after it and a name picked from the list with Return (the group chat's "@Filo") (picking one used to set the whole text again,
/// leaving the picture's name and size as words). With fixture data (scripts/chip-mention-check.sh runs it offline in
/// a sandbox; FILO_DRIVE_PICTURE is the picture). Logs PASS/FAIL per check, saves screenshots to `FILO_DRIVE_SHOTS`
/// and exits 1 when any check fails. Nothing can be sent: it runs offline.
fn run_chip_mention_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    cx.spawn_in(window, async move |this, cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-chip-mention".into());
        let _ = std::fs::create_dir_all(&shots);
        let env = |name: &str| std::env::var(name).unwrap_or_default();
        let (agent, picture) = (env("FILO_DRIVE_AGENT_ID"), env("FILO_DRIVE_PICTURE"));
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: String| {
            eprintln!("chip-mention: {} {what}", if ok { "PASS" } else { "FAIL" });
            if !ok {
                failures.push(what);
            }
        };
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("chip-mention: shot {path}");
            }
        };
        // (text, chips: (with a picture), files attached)
        let composer =
            |conversation: &ConversationView, cx: &mut gpui::AsyncWindowContext| -> (String, Vec<bool>, usize) {
                conversation
                    .read_with(cx, |conversation, cx| {
                        let input = conversation.composer().read(cx);
                        (
                            input.value().to_string(),
                            input.atoms().iter().map(|atom| atom.image.is_some()).collect(),
                            conversation.attachment_count(),
                        )
                    })
                    .unwrap_or_default()
            };

        wait(3000).await;
        for _ in 0..60 {
            let targets = cx.update(|_, cx| crate::nav::share_targets(cx)).unwrap_or_default();
            if targets.len() >= 2 {
                break;
            }
            this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
            wait(500).await;
        }
        let Some(width) = window_width(&this, cx) else { return eprintln!("chip-mention: no window") };
        this.update_in(cx, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx)).ok();
        let mut shown = None;
        for _ in 0..20 {
            this.update_in(cx, |shell, window, cx| {
                if let Some(agents) = shell.agents_screen() {
                    agents.update(cx, |agents, cx| agents.open_agent(agent.clone(), window, cx));
                }
            })
            .ok();
            wait(500).await;
            shown = shown_conversation(&this, cx).1;
            if shown.is_some() {
                break;
            }
        }
        let Some(conversation) = shown else {
            eprintln!("chip-mention: FAIL no conversation");
            cx.update(|_, cx| cx.quit()).ok();
            std::process::exit(1);
        };
        wait(1000).await;
        this.update_in(cx, |_, window, cx| {
            conversation.update(cx, |conversation, cx| {
                conversation.attach_draft(vec!["Look at this ".into(), picture.clone()], window, cx)
            })
        })
        .ok();
        wait(1500).await;
        let (text, chips, files) = composer(&conversation, cx);
        check(
            chips == [true] && files == 1,
            format!(
                "the attached picture shows as a chip with its picture (chips {chips:?}, files {files}, text {text:?})"
            ),
        );
        snap(width, "1-picture-attached");

        type_keys(&this, cx, "@Fi", false);
        wait(800).await;
        snap(width, "2-mention-list-open");
        type_keys(&this, cx, "", true);
        wait(800).await;
        let (text, chips, files) = composer(&conversation, cx);
        check(text.contains("@Filo"), format!("Return picks the name from the list ({text:?})"));
        check(
            chips == [true] && files == 1,
            format!("after the pick the chip and its picture are still there (chips {chips:?}, files {files})"),
        );
        snap(width, "3-after-mention-picked");

        if failures.is_empty() {
            eprintln!("chip-mention: all checks passed");
        } else {
            eprintln!("chip-mention: {} failed: {failures:?}", failures.len());
        }
        let code = i32::from(!failures.is_empty());
        crate::window_shot::flush(cx.background_executor()).await;
        cx.update(|_, cx| cx.quit()).ok();
        std::process::exit(code);
    })
    .detach();
}

/// `FILO_OPEN=feed|notes|artifacts|connectors|you|chat:<id>|note:<id>|view:<type>,<url>|preview:<type>,<url>|edit:<url>|…`: starts on that
/// tab or page, for checking a screen by eye; with `FILO_SCROLL=<points>` it then scrolls the page that far, and with
/// `FILO_SHOT_AFTER=<seconds>:<file.png>` saves a picture of the window and quits (without `FILO_OPEN`, neither does
/// anything). `coding-result:<task id>@<round>` opens that round. The drives (`DRIVES`) start the same way.
pub fn open_from_env(window: &mut Window, cx: &mut Context<AppShell>) {
    let Ok(target) = std::env::var("FILO_OPEN") else { return };
    // `FILO_SHOT_AFTER=<seconds>:<file.png>`: a picture of the window that long after it opens, then quit (checking a
    // page by eye from a script; with FILO_SCROLL, after it has scrolled). Several, `;` between, picture it as it
    // changes: `2:loading.png;9:loaded.png`, quitting after the last.
    let shots: Vec<(f64, String)> = std::env::var("FILO_SHOT_AFTER")
        .unwrap_or_default()
        .split(';')
        .filter_map(|shot| {
            let (seconds, path) = shot.split_once(':')?;
            Some((seconds.parse::<f64>().ok()?, path.to_string()))
        })
        .collect();
    if !shots.is_empty() {
        cx.spawn_in(window, async move |this, cx| {
            let mut at = 0.;
            for (seconds, path) in shots {
                cx.background_executor().timer(Duration::from_secs_f64((seconds - at).max(0.))).await;
                at = seconds.max(at);
                if let Some(number) = window_width(&this, cx).and_then(native_input::window_number) {
                    crate::window_shot::save(number, &path).await;
                    eprintln!("smoke: shot {path}");
                }
            }
            cx.update(|_, cx| cx.quit()).ok();
        })
        .detach();
    }
    // `FILO_CLICK_AFTER=<seconds>:<x>,<y>`: a click at (x, y), window points from its top left, that long after it
    // opens, as a person's (in-app AppKit events): a page's own controls, like an artifact's video.
    if let Some((seconds, x, y)) = std::env::var("FILO_CLICK_AFTER").ok().and_then(|value| {
        let (seconds, at) = value.split_once(':')?;
        let (x, y) = at.split_once(',')?;
        Some((seconds.parse::<f64>().ok()?, x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))
    }) {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f64(seconds)).await;
            if let Some(width) = window_width(&this, cx) {
                eprintln!("smoke: clicked at {x},{y} ({})", native_input::click(width, x, y));
            }
        })
        .detach();
    }
    let (kind, id) = target
        .split_once(':')
        .map(|(kind, id)| (kind.to_string(), Some(id.to_string())))
        .unwrap_or((target.clone(), None));
    // `FILO_SEND=<text>` with `agent:<id>` or `chat:<id>`: sends it in that conversation through the backend, as the
    // composer does (a whole turn of the user's Claude Code or Codex, without typing).
    if let (Ok(text), Some(id), true) = (std::env::var("FILO_SEND"), id.clone(), kind == "agent" || kind == "chat") {
        let backend = crate::backend::backend(cx);
        let agent = kind == "agent";
        cx.background_executor()
            .spawn(async move {
                let session = if agent {
                    backend
                        .call_raw("agents:openChat", json!({ "id": id }))
                        .await
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                } else {
                    Some(id)
                };
                let Some(session) = session else { return eprintln!("smoke: no conversation to send in") };
                let args =
                    json!({ "sessionId": session, "requestId": uuid::Uuid::new_v4().to_string(), "message": text });
                let sent = backend.call_raw("voiceSessions:sendMessage", args).await;
                eprintln!("smoke: sent in {session}: {sent:?}");
            })
            .detach();
    }
    if let Some((_, drive)) = DRIVES.iter().find(|(name, _)| *name == kind) {
        return drive(id, window, cx);
    }
    // `group`: the agents' group chat in the Agents tab, made first if need be. With `FILO_SEND=<text>`, the text is
    // sent there through the backend, as the composer does (the members' turns run on the user's Claude Code or Codex).
    if kind == "group" {
        let backend = crate::backend::backend(cx);
        let text = std::env::var("FILO_SEND").ok();
        cx.spawn_in(window, async move |this, cx| {
            let session = backend.call_raw("agents:openGroup", json!({})).await;
            let Some(session) = session.ok().and_then(|value| value.as_str().map(str::to_string)) else {
                return eprintln!("smoke: no group");
            };
            this.update_in(cx, |shell, window, cx| {
                shell.open_tab(Tab::Agents, window, cx);
                if let Some(agents) = shell.agents_screen() {
                    agents.update(cx, |agents, cx| {
                        agents.open_agent(crate::screens::agents::GROUP.to_string(), window, cx)
                    });
                }
            })
            .ok();
            if let Some(text) = text {
                let args =
                    json!({ "sessionId": session, "requestId": uuid::Uuid::new_v4().to_string(), "message": text });
                let sent = backend.call_raw("voiceSessions:sendMessage", args).await;
                eprintln!("smoke: sent in the group {session}: {sent:?}");
            }
        })
        .detach();
        return;
    }
    // `agent:<agent id>`: that agent's chat in the Agents tab.
    if let ("agent", Some(id)) = (kind.as_str(), id.clone()) {
        cx.on_next_frame(window, move |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| agents.open_agent(id.clone(), window, cx));
            }
        });
        return;
    }
    // `view:<content type>,<url or path>|<content type>,<url or path>…`: those pictures and videos in the app's viewer,
    // from the first. A path is a file on this Mac.
    if let ("view", Some(spec)) = (kind.as_str(), id.clone()) {
        let items: Vec<crate::file_preview::Item> = spec
            .split('|')
            .filter_map(|part| part.split_once(','))
            .map(|(content_type, url)| {
                let name = url.rsplit('/').next().unwrap_or("file").to_string();
                let file = url.starts_with('/').then(|| std::path::PathBuf::from(url));
                crate::file_preview::Item { url: url.to_string(), name, content_type: content_type.to_string(), file }
            })
            .collect();
        cx.on_next_frame(window, move |_, window, cx| crate::media_viewer::open(items.clone(), 0, None, window, cx));
        // `FILO_VIEW_STEPS=<step>,<step>…`: keys and clicks one after another once it's open (`space`, `m`, `f`, `left`,
        // `right`, `esc`, `click` and `double-click` on the middle of the window, `scrub` halfway along the scrubber,
        // `wait`), logging what the viewer shows after each, with a shot of the window in `FILO_DRIVE_SHOTS`.
        if let Ok(steps) = std::env::var("FILO_VIEW_STEPS") {
            cx.spawn_in(window, async move |this, cx| {
                let executor = cx.background_executor().clone();
                let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
                wait(4000).await;
                let Some(width) = window_width(&this, cx) else { return eprintln!("view: no window") };
                let shots = std::env::var("FILO_DRIVE_SHOTS").ok();
                let report = |step: &str, number: usize, cx: &mut gpui::AsyncWindowContext| {
                    let state =
                        cx.update(|window, cx| crate::media_viewer::smoke_state(window, cx)).unwrap_or_default();
                    eprintln!("view: after {step}: {state}");
                    if let (Some(shots), Some(window)) = (&shots, native_input::window_number(width)) {
                        crate::window_shot::save(window, format!("{shots}/view-{number}-{step}.png"));
                    }
                };
                report("open", 0, cx);
                let height = cx.update(|window, _| f64::from(f32::from(window.viewport_size().height))).unwrap_or(760.);
                for (number, step) in steps.split(',').map(str::trim).enumerate() {
                    let (middle, scrubber) =
                        ((width / 2., height / 2.), (width / 2., height - crate::media_viewer::BAR as f64 / 2.));
                    let done = match step {
                        "space" => native_input::key(width, " ", 49, 0),
                        "m" => native_input::key(width, "m", 46, 0),
                        "f" => native_input::key(width, "f", 3, 0),
                        "left" => native_input::key(width, "\u{F702}", 123, native_input::FUNCTION),
                        "right" => native_input::key(width, "\u{F703}", 124, native_input::FUNCTION),
                        "esc" => native_input::key(width, "\u{1b}", 53, 0),
                        "click" => native_input::click(width, middle.0, middle.1),
                        "double-click" => native_input::double_click(width, middle.0, middle.1),
                        "scrub" => native_input::click(width, scrubber.0, scrubber.1),
                        _ => true,
                    };
                    wait(if step == "f" || step == "esc" || step == "double-click" { 2500 } else { 1500 }).await;
                    report(&format!("{step}{}", if done { "" } else { " (not sent)" }), number + 1, cx);
                }
                eprintln!("view: done");
                if std::env::var_os("FILO_DRIVE_STAY").is_none() {
                    cx.update(|_, cx| cx.quit()).ok();
                }
            })
            .detach();
        }
        // `FILO_VIEW_ZOOM=<n>`: ⌘= pressed that many times once the picture is in, then `FILO_SCROLL` pans it.
        let presses = std::env::var("FILO_VIEW_ZOOM").ok().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        let pan = std::env::var("FILO_SCROLL").ok().and_then(|dy| dy.parse::<f64>().ok());
        if presses > 0 || pan.is_some() {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(4)).await;
                let Some(width) = window_width(&this, cx) else { return };
                for _ in 0..presses {
                    native_input::key(width, "=", 24, native_input::COMMAND);
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                }
                if let Some(dy) = pan {
                    native_input::hover(width, width / 2., 400.);
                    native_input::scroll_in(width, width / 2., 400., dy);
                }
            })
            .detach();
        }
        return;
    }
    // `preview:<content type>,<url>`: that file in Quick Look.
    if let ("preview", Some(spec)) = (kind.as_str(), id.clone()) {
        let (content_type, url) = spec.split_once(',').unwrap_or(("application/pdf", spec.as_str()));
        let name = url.rsplit('/').next().unwrap_or("file").to_string();
        let item = crate::file_preview::Item {
            url: url.to_string(),
            name,
            content_type: content_type.to_string(),
            file: None,
        };
        cx.on_next_frame(window, move |_, _, cx| crate::file_preview::quick_look(item.clone(), cx));
        return;
    }
    // `soak` or `soak:<chat id>`: light use for a memory soak, without the keyboard. Every FILO_SOAK_EVERY seconds
    // (default 90) it visits another tab or page (Feed, Artifacts, Agents, Settings › Memories and Back, another recent chat)
    // and comes back to the chat, logging resident memory; sample the footprint from outside with `footprint`.
    if kind == "soak" {
        run_soak(id, window, cx);
        return;
    }
    if kind == "soak-plan" {
        run_soak_plan(window, cx);
        return;
    }
    let tab = match kind.as_str() {
        "feed" => Some(Tab::Feed),
        "notes" => Some(Tab::Notes),
        "artifacts" => Some(Tab::Artifacts),
        "connectors" => Some(Tab::Connectors),
        "agents" => Some(Tab::Agents),
        "coding" => Some(Tab::Coding),
        "settings" | "you" => Some(Tab::Settings),
        _ => None,
    };
    let route = match (kind.as_str(), id.clone()) {
        ("note", id) => Some(Route::Note(id)),
        ("artifact", Some(id)) => Some(Route::Artifact(id)),
        ("post", Some(id)) => Some(Route::FeedPost(id)),
        ("scheduled", _) => Some(Route::Scheduled),
        ("feed-settings", _) => Some(Route::FeedSettings),
        ("appearance", _) => Some(Route::Appearance),
        ("memories", _) => Some(Route::Memories),
        ("report", _) => Some(Route::Report(crate::report::Origin::Settings)),
        ("coding-task", Some(id)) => Some(Route::CodingTask(id)),
        // `coding-result:<task id>@<round>` opens that round.
        ("coding-result", Some(id)) => Some(match id.split_once('@') {
            Some((task, round)) => Route::CodingResult(task.to_string(), round.parse().ok()),
            None => Route::CodingResult(id, None),
        }),
        ("app-icon", _) => Some(Route::AppIcon),
        ("desktop-pet", _) => Some(Route::DesktopPet),
        _ => None,
    };
    // `agent-new`: the new-agent form; `agent-picker`: with its picture picker open.
    if kind == "agent-new" || kind == "agent-picker" {
        let picker = kind == "agent-picker";
        cx.on_next_frame(window, move |shell, window, cx| {
            shell.open_tab(Tab::Agents, window, cx);
            if let Some(agents) = shell.agents_screen() {
                agents.update(cx, |agents, cx| {
                    agents.new_agent(window, cx);
                    if picker {
                        agents.open_picker(window, cx);
                    }
                });
            }
        });
        return;
    }
    // `agent-shared:<agent id>`: that agent's profile (its edit page), with what its conversation shared under the form.
    if let ("agent-shared", Some(id)) = (kind.as_str(), id.clone()) {
        cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
        cx.spawn_in(window, async move |shell, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            shell
                .update_in(cx, |shell, window, cx| {
                    if let Some(agents) = shell.agents_screen() {
                        agents.update(cx, |agents, cx| agents.edit_agent(id, window, cx));
                    }
                })
                .ok();
        })
        .detach();
        return;
    }
    // `edit:<url>`: that picture open to draw on over the agent's open conversation.
    if let ("edit", Some(url)) = (kind.as_str(), id.clone()) {
        cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
        cx.spawn_in(window, async move |shell, cx| {
            // Waits for the chat to load (up to 2 min).
            for _ in 0..120 {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let opened = shell
                    .update_in(cx, |shell, window, cx| {
                        let Some(conversation) =
                            shell.agents_screen().and_then(|agents| agents.read(cx).current_conversation())
                        else {
                            return false;
                        };
                        let name = url.rsplit('/').next().unwrap_or("Picture").to_string();
                        let item = crate::file_preview::Item {
                            url: url.clone(),
                            name,
                            content_type: "image/png".into(),
                            file: None,
                        };
                        conversation.update(cx, |conversation, cx| {
                            conversation.edit_picture(
                                crate::screens::chat::image_editor::Source::Chat(item),
                                window,
                                cx,
                            );
                            if let Some(editor) = conversation.image_editor() {
                                editor.update(cx, |editor, _| editor.show_demo())
                            }
                        });
                        true
                    })
                    .unwrap_or(true);
                if opened {
                    break;
                }
            }
        })
        .detach();
        return;
    }
    // `mention:<text>`: the text typed into the agent's open conversation's composer (an "@" opens the mention list).
    if let ("mention", Some(text)) = (kind.as_str(), id.clone()) {
        cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
        cx.spawn_in(window, async move |shell, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            shell
                .update_in(cx, |shell, window, cx| {
                    let conversation = shell.agents_screen().and_then(|agents| agents.read(cx).current_conversation());
                    if let Some(conversation) = conversation {
                        conversation.update(cx, |conversation, cx| conversation.type_draft(text, window, cx));
                    }
                })
                .ok();
        })
        .detach();
        return;
    }
    // `attach:<text>|<path>|<text>|<path>…`: typed into the agent's open conversation's composer, each file attached where it falls, as a
    // chip (uploads them, sends nothing).
    if let ("attach", Some(spec)) = (kind.as_str(), id.clone()) {
        let parts: Vec<String> = spec.split('|').map(str::to_string).collect();
        cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
        cx.spawn_in(window, async move |shell, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            shell
                .update_in(cx, |shell, window, cx| {
                    let conversation = shell.agents_screen().and_then(|agents| agents.read(cx).current_conversation());
                    if let Some(conversation) = conversation {
                        conversation.update(cx, |conversation, cx| conversation.attach_draft(parts, window, cx));
                    }
                })
                .ok();
        })
        .detach();
        return;
    }
    if let ("chat", Some(id)) = (kind.as_str(), id.clone()) {
        cx.on_next_frame(window, move |shell, window, cx| shell.open_chat(id, window, cx));
    } else if kind == "skills" {
        cx.on_next_frame(window, |shell, window, cx| shell.open_skills(window, cx));
    } else if let Some(tab) = tab {
        cx.on_next_frame(window, move |shell, window, cx| shell.open_tab(tab, window, cx));
    } else if let Some(route) = route {
        cx.on_next_frame(window, move |shell, window, cx| shell.push(route, window, cx));
    } else if !matches!(kind.as_str(), "welcome" | "welcome-demo") {
        // The fixtures open from main.rs, and the welcome's own names are read by welcome.rs.
        eprintln!("smoke: FILO_OPEN={target} is no page, conversation or drive this build knows; nothing opened");
    }
    // `FILO_SCROLL=<points>`: once the page has loaded (5 s), the wheel turned that far over its middle, to see what
    // scrolled content looks like (under a pushed page's header, say).
    if let Some(dy) = std::env::var("FILO_SCROLL").ok().and_then(|dy| dy.parse::<f32>().ok()) {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let Some(width) = window_width(&this, cx) else { return };
            // Named by the window: a test copy started from a terminal isn't key or under the pointer.
            native_input::hover(width, width / 2., 400.);
            native_input::scroll_in(width, width / 2., 400., f64::from(dy));
        })
        .detach();
    }
}

/// `FILO_OPEN=reasoning-drive` (on a config of its own, `FILO_CONFIG_DIR`): the composer's reasoning picker in an agent's
/// chat on each engine. Makes an agent on Claude Code, on Codex (`FILO_DRIVE_CODEX_MODEL`, else gpt-5.5) and on
/// OpenCode (a model of its that has levels, and one that has none, once it has said), each with Max picked; opens
/// each one's chat, logs the levels its model takes and whether the picker shows, and clicks it (in-app AppKit events,
/// `native_input`), with a shot of it open in `FILO_DRIVE_SHOTS`.
fn run_reasoning_drive(window: &mut Window, cx: &mut Context<AppShell>) {
    let backend = backend(cx);
    cx.on_next_frame(window, |shell, window, cx| shell.open_tab(Tab::Agents, window, cx));
    cx.spawn_in(window, async move |this, cx| {
        use filo_local::Engine::{Claude, Codex, OpenCode};
        for tool in filo_local::coding::TOOLS {
            backend.call_raw("coding:setPlugged", json!({ "tool": tool, "plugged": true })).await.ok();
        }
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let shots = std::env::var("FILO_DRIVE_SHOTS").unwrap_or_else(|_| "/tmp/filo-reasoning".into());
        let _ = std::fs::create_dir_all(&shots);
        let snap = |width: f64, name: &str| {
            if let Some(number) = native_input::window_number(width) {
                let path = format!("{shots}/{name}.png");
                crate::window_shot::save(number, &path);
                eprintln!("reasoning-drive: shot {path}");
            }
        };
        // OpenCode's models and their levels come from it in the background on a fresh config.
        let (mut with, mut without) = (None, None);
        for _ in 0..90 {
            let rows = cx.update(|_, cx| crate::screens::chat::conversation::model_rows(cx)).unwrap_or_default();
            let opencode: Vec<(String, bool)> = rows.iter().filter(|row| row.engine == OpenCode).map(|row| (row.id.clone(), !backend.reasoning_levels(OpenCode, Some(&row.id)).is_empty())).collect();
            with = opencode.iter().find(|(_, levels)| *levels).map(|(id, _)| id.clone());
            without = opencode.iter().find(|(_, levels)| !*levels).map(|(id, _)| id.clone());
            if with.is_some() {
                break;
            }
            wait(1000).await;
        }
        eprintln!("reasoning-drive: OpenCode models with levels: {with:?}, without: {without:?}");
        let codex = std::env::var("FILO_DRIVE_CODEX_MODEL").unwrap_or_else(|_| "gpt-5.5".into());
        let mut cases = vec![("claude", Claude, None), ("codex", Codex, Some(codex))];
        cases.extend(with.map(|model| ("opencode", OpenCode, Some(model))));
        cases.extend(without.map(|model| ("opencode-none", OpenCode, Some(model))));
        let Some(width) = window_width(&this, cx) else { return eprintln!("reasoning-drive: no window") };
        for (name, engine, model) in cases {
            let agent = json!({ "name": format!("Check {name}"), "instructions": "Checks the reasoning picker.", "engine": engine, "model": model });
            let Ok(Value::String(id)) = backend.call_raw("agents:create", agent).await else { return eprintln!("reasoning-drive: FAIL couldn't make the {name} agent") };
            let Ok(Value::String(session)) = backend.call_raw("agents:openChat", json!({ "id": id })).await else { return eprintln!("reasoning-drive: FAIL no chat for {name}") };
            backend.call_raw("voiceSessions:setReasoning", json!({ "sessionId": session, "reasoning": "max" })).await.ok();
            this.update_in(cx, |shell, window, cx| shell.open_agent(id.clone(), window, cx)).ok();
            wait(3000).await;
            let levels = backend.session_reasoning_levels(&session);
            let shown = on_screen("composer-reasoning").is_some();
            eprintln!("reasoning-drive: {name} ({} {model:?}, Max picked): levels {levels:?}, picker {}", engine.label(), if shown { "shown" } else { "not shown" });
            if shown {
                click(&this, cx, "composer-reasoning");
                wait(1000).await;
            }
            snap(width, &format!("reasoning-{name}"));
            native_input::key(width, "\u{1b}", 53, 0);
            wait(500).await;
        }
        eprintln!("reasoning-drive: done");
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|_, cx| cx.quit()).ok();
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_drive_has_a_name_of_its_own() {
        let mut names: Vec<&str> = DRIVES.iter().map(|(name, _)| *name).chain(DRIVEN_FIXTURES).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "{names:?}");
    }
}

// Filo for Mac, built with GPUI, with no account or hosted backend. The pages call functions by name ("notes:mine"),
// answered on this Mac (backend.rs, local_backend.rs); conversations are answered by the user's own coding tool
// (Claude Code, Codex, Muse Code or OpenCode) through Filo's MCP server (crates/filo-local).

mod app;
mod app_icon;
mod assets;
mod avatar_colors;
mod avatars;
mod backend;
// Filo's own browser, which agents work in the background (Window › Browser).
mod browser;
mod chrome;
mod command;
mod commit_links;
mod companion;
mod computer_use;
mod config;
mod escape;
mod file_preview;
mod format;
mod hotkeys;
// The background helper that fires reminders and routines while the window is closed (`filo --scheduler`).
mod in_app_notice;
mod launch_agent;
mod live_transcription;
mod local_backend;
mod media;
mod media_viewer;
mod menu_bar;
mod model;
mod nav;
mod notifications;
mod pages;
mod pasteboard;
mod pet;
mod pet_pick;
mod pictures;
mod presence;
mod quit_guard;
mod recorder;
mod report;
mod screens;
mod screenshot;
// Telling the user when the Mac slept while tasks kept it awake.
mod slept;
mod smoke;
// Anonymous install and daily-open counts, on by default (docs/telemetry.md).
mod telemetry;
// Smoke runs and fixtures never capture the screen or ask macOS for a permission.
mod test_run;
mod theme;
mod ui;
mod unseen;
mod updater;
mod video_player;
mod voice_route;
mod web_holes;
// Test windows' screenshots, made in the app.
#[cfg(feature = "dev")]
mod window_shot;

use gpui::{App, AppContext as _, Application, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;

fn main() {
    // `filo --disclaim <program> <args…>`: becomes the program, which then answers for its own macOS permissions
    // (crates/filo-local disclaim.rs). It's how every Filo process below starts the coding tools.
    if std::env::args_os().nth(1).is_some_and(|arg| arg == filo_local::disclaim::ARG) {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
        std::process::exit(filo_local::disclaim::exec(&args));
    }
    if let Ok(program) = std::env::current_exe() {
        filo_local::disclaim::launch_with(program);
    }

    // `filo --local-mcp --chat <id> --turn <id> --dir <root>`: Filo's MCP server for a conversation's turn, on
    // stdin/stdout. A coding tool starts it (local_backend.rs, crates/filo-local); no window, no backend.
    if std::env::args().nth(1).as_deref() == Some("--local-mcp") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(filo_local::mcp::main_from_args(&args));
    }

    // `filo --feed-mcp [--db <filo.db>] [--author <name>]`: the Feed's tools on their own, for a coding tool
    // session outside Filo (`claude mcp add filo-feed -- <this binary> --feed-mcp`).
    if std::env::args().nth(1).as_deref() == Some("--feed-mcp") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        let db = config::config_dir().join("local").join(filo_local::db::FILE);
        std::process::exit(filo_local::feed_mcp::main_from_args(&args, db));
    }

    // `filo --scheduler [install|uninstall|status]`: the background helper launchd keeps running, which fires
    // reminders, routines and check-ins while the window is closed (launch_agent.rs); no window, no Tokio.
    if std::env::args().nth(1).as_deref() == Some("--scheduler") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(launch_agent::main(&args));
    }

    // `filo --lid-backstop --root <dir>`: what the lid backstop's launchd agent runs every minute, once this binary passes
    // its signature check: sets sleep back on if Filo left it off with nothing running (local_backend/lid.rs).
    if std::env::args().nth(1).as_deref() == Some("--lid-backstop") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(local_backend::lid_backstop_main(&args));
    }

    // `filo --uninstall-lid`: before deleting Filo, sets sleep back on and removes what "Keep running with the lid closed"
    // installed (its backstop agent and, with the administrator prompt, /etc/sudoers.d/filo; local_backend/lid.rs).
    if std::env::args().nth(1).as_deref() == Some("--uninstall-lid") {
        std::process::exit(local_backend::lid_uninstall_main());
    }

    // `filo --install-update --pid <app> --app <new .app> --staging <dir> --bundle <.app>`: what the app starts as it
    // quits with an update downloaded: puts it in place once the app is gone and chat replies have finished (updater.rs).
    if std::env::args().nth(1).as_deref() == Some(updater::INSTALL_ARG) {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(updater::install_main(&args));
    }

    // `filo --worker turn|round … --root <dir>`: one conversation turn or coding round in a process of its own, which
    // carries on when the app quits (local_backend/workers.rs); no window, no Tokio.
    if std::env::args().nth(1).as_deref() == Some("--worker") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(local_backend::worker_main(&args));
    }

    // The local backend's work runs on Tokio; GPUI keeps the UI thread.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");

    // Anything but the modes here and above, or what macOS itself may pass an app, stops before the data folder is
    // opened: a mistyped flag (`--chek`, `--help`) used to open the whole app on the user's data.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(arg) =
        args.first().filter(|arg| !DEV_MODES.contains(&arg.as_str()) && *arg != "--app-icon" && !from_macos(arg))
    {
        eprintln!("filo: unknown option {arg}");
        eprintln!(
            "The app opens with none. Its other modes: --app-icon, --check, --record, --update, --avatar-colors (a build from the checkout), and those launchd and the coding tools start (README.md)."
        );
        std::process::exit(2);
    }
    // A build without the `dev` feature (the packaged app) has none of the developer's modes below: it says so rather
    // than open the app.
    #[cfg(not(feature = "dev"))]
    if let Some(mode) = args.first().filter(|arg| DEV_MODES.contains(&arg.as_str())) {
        eprintln!(
            "filo {mode}: not in this build (a packaged app has no developer modes; use a build from the checkout)"
        );
        std::process::exit(2);
    }

    // `filo --avatar-colors <dir>`: every animal pose in every colour but blue, as `<dir>/<color>/<id>.png`
    // (scripts/avatar-colors.sh makes them the WebP pictures the app bundles). No data is opened.
    #[cfg(feature = "dev")]
    if args.first().map(String::as_str) == Some("--avatar-colors") {
        let Some(dir) = args.get(1).map(std::path::PathBuf::from) else {
            eprintln!("usage: filo --avatar-colors <dir>");
            std::process::exit(2);
        };
        let mut written = 0;
        for (color, _, rgb) in avatar_colors::COLORS {
            if rgb.is_none() {
                continue;
            }
            std::fs::create_dir_all(dir.join(color)).expect("can't create the output folder");
            for animal in avatars::ANIMALS.iter() {
                for (pose, _) in animal.poses {
                    let id = animal.avatar_id(pose);
                    let png = avatar_colors::colored_png(&id, color).unwrap_or_else(|| panic!("no picture for {id}"));
                    std::fs::write(dir.join(color).join(format!("{id}.png")), png).expect("can't write the picture");
                    written += 1;
                }
            }
        }
        println!("{written} pictures in {}", dir.display());
        std::process::exit(0);
    }

    // `filo --update [dir]`: this build's channel and number, and what its feed offers; with a dir, downloads the
    // update into it, checks it and unpacks it the way the app does before swapping, without installing it. No data
    // is opened.
    #[cfg(feature = "dev")]
    if args.first().map(String::as_str) == Some("--update") {
        let http = reqwest::Client::builder().user_agent("filo-desktop").build().expect("http client");
        std::process::exit(runtime.block_on(update_check(http, args.get(1).cloned())));
    }

    // `filo --app-icon out.png [icon]`: draws the Dock icon for one of the icons to pick (`filo`, `sage`…), an avatar
    // id or `custom:<id>` (default: the avatar saved on the profile) into a PNG, without opening a window. One of the
    // icons to pick (packaging draws the bundle's default one) opens no data.
    if args.first().map(String::as_str) == Some("--app-icon")
        && let Some(code) = app_icon_of_choice(&args[1..])
    {
        std::process::exit(code);
    }

    // The modes left read the agents and chats: they run on a data folder of their own (FILO_CONFIG_DIR), never the
    // user's ~/.config/filo. Opening it migrates filo.db to this build's schema and writes its backup and skills, and a
    // build of another commit could leave the user's Filo unable to read its data.
    if let Some(mode) = args.first().filter(|arg| !from_macos(arg) && !test_run::own_config()) {
        eprintln!(
            "filo {mode}: runs on a data folder of its own, never ~/.config/filo. Set one: FILO_CONFIG_DIR=\"$(mktemp -d)\" filo {mode} … (or a copy of ~/.config/filo)"
        );
        std::process::exit(2);
    }

    let backend = backend::Backend::new(runtime.handle().clone()).expect("Couldn't start the backend");

    // `filo --check [path [json-args]]`: runs one
    // query through the same backend code the window uses, without opening a window.
    #[cfg(feature = "dev")]
    if args.first().map(String::as_str) == Some("--check") {
        std::process::exit(runtime.block_on(check(backend, args[1..].to_vec())));
    }

    // `filo --record [seconds] [sessionId]`: records from the microphone (only if access
    // is already granted, so no prompt appears), streaming it to Filo's relay like a
    // voice message, and, with a session, sends it there the way a voice message goes
    // without live words (createUpload, kept, voiceRecordings:send). `filo --record
    // clip.wav` (16 kHz mono PCM16) streams the file to the relay in real time instead,
    // and `filo --record clip.m4a [sessionId]` sends a recording through it at once.
    #[cfg(feature = "dev")]
    if args.first().map(String::as_str) == Some("--record") {
        std::process::exit(runtime.block_on(record_check(backend, args[1..].to_vec())));
    }

    // `filo --app-icon out.png [avatar]`: an avatar's icon, or the main agent's.
    if args.first().map(String::as_str) == Some("--app-icon") {
        std::process::exit(runtime.block_on(app_icon_check(backend, args[1..].to_vec())));
    }

    // Data a newer Filo wrote (its database's schema) isn't opened: this Filo stops and says why, rather than run on
    // without the user's agents.
    if let Some(error) = backend.newer_data() {
        stop_for_newer_data(&error);
    }

    // The app runs the schedule while it's open; the helper that runs it when it's closed is installed once something
    // is scheduled (launch_agent.rs), and kept up to date from here.
    // A drive timing a copy of the user's chats (`FILO_OPEN=chat-timing`) runs none of the copy's work.
    if !smoke::quiet_backend() {
        backend.start_schedule();
        // The Coding page's projects are looked for, and its tasks run, while the app is open.
        backend.start_coding();
        // The agents' group chat answers while the app is open; turns left waiting when it closed run now.
        backend.start_group();
        // Artifacts made before Filo saved their pages' web pictures get them, once.
        backend.save_artifact_pictures();
    }
    launch_agent::refresh_in_background();

    // Smoke runs and fixtures: their windows' frames can be read back for screenshots (window_shot.rs).
    #[cfg(feature = "dev")]
    window_shot::init();
    let http = std::sync::Arc::new(media::ReqwestHttp::new(runtime.handle().clone()));
    Application::new().with_assets(assets::Assets).with_http_client(http).run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.text_system().add_fonts(assets::fonts()).expect("fonts");
        cx.set_global(backend::BackendGlobal(backend));
        // Quitting ends the work running in the app; turns and coding tasks in workers carry on with "Keep coding after
        // Filo quits" on, and stop with it off (local_backend/workers.rs).
        // A downloaded update goes in once Filo has quit (updater.rs).
        cx.on_app_quit(|cx| {
            updater::install_on_quit();
            backend::backend(cx).quit();
            browser::quit();
            async {}
        })
        .detach();
        theme::apply(config::load_prefs(), None, cx);
        app_icon::restore();
        app_icon::show(cx);

        chrome::init(cx);
        // The browser and the socket the agents' browser tools reach it through (browser.rs).
        browser::init(cx);
        // Feed and Notes, on or off (pages.rs): the sidebar, ⌘1 – ⌘7 and the View menu follow.
        pages::init(cx);
        // Esc closes what's open on top (escape.rs).
        escape::init(cx);
        // Commit hashes in messages open on GitHub (commit_links.rs).
        commit_links::init(cx);
        // Menus over a web view show through a hole in it (web_holes.rs).
        web_holes::init(cx);
        // ⌘Q twice to quit (quit_guard.rs).
        quit_guard::init(cx);
        screens::chat::bind_keys(cx);
        screens::agents::bind_keys(cx);
        media_viewer::bind_keys(cx);
        // The test windows on made-up data (`dev` builds).
        #[cfg(feature = "dev")]
        {
            // `FILO_OPEN=coding-cards`: the chat's coding cards and working panel on made-up data, no backend.
            if screens::chat::fixture::wanted() {
                screens::chat::fixture::open(cx);
                return;
            }
            // `FILO_OPEN=chat-send`: sending text and pictures, with slow and failing uploads and deliveries, on a made-up chat, no backend.
            if screens::chat::conversation::send_fixture::wanted() {
                screens::chat::conversation::send_fixture::open(cx);
                return;
            }
            // `FILO_OPEN=chat-overlap`: agent messages coming one after another while the agent works, no backend.
            if screens::chat::conversation::overlap_fixture::wanted() {
                screens::chat::conversation::overlap_fixture::open(cx);
                return;
            }
            // `FILO_OPEN=chat-composer`: the composer going from one row to stacked and back, growing and scrolling, no backend.
            if screens::chat::conversation::composer_fixture::wanted() {
                screens::chat::conversation::composer_fixture::open(cx);
                return;
            }
            // `FILO_OPEN=glances:<file>`: a glance card in a made-up chat, its page's size, clip and scrolling checked, no backend.
            if screens::chat::conversation::glance_fixture::wanted() {
                screens::chat::conversation::glance_fixture::open(cx);
                return;
            }
            // `FILO_OPEN=chat-scroll`: where a chat's feed sits as it opens, switches and loads, on made-up chats, no backend.
            if screens::chat::conversation::scroll_fixture::wanted() {
                screens::chat::conversation::scroll_fixture::open(cx);
                return;
            }
            // `FILO_OPEN=feed-fixture`: the Feed on made-up posts, no backend (screens/feed_fixture.rs).
            if screens::feed_fixture::wanted() {
                screens::feed_fixture::open(cx);
                return;
            }
            // `FILO_OPEN=markdown-compare`: a chat reply in today's markdown style and a candidate one, no backend.
            if screens::markdown_compare::wanted() {
                screens::markdown_compare::open(cx);
                return;
            }
            // `FILO_OPEN=browser-check`: Filo's browser end to end against a test site, with another app in front
            // (browser/check.rs).
            if browser::check::wanted() {
                browser::check::open(cx);
                return;
            }
            // `FILO_OPEN=menus`: one menu or popover open at a time, checked with real clicks, no backend.
            if screens::menus_fixture::wanted() {
                screens::menus_fixture::open(cx);
                return;
            }
        }
        // Counts this install and the days it's opened, unless turned off (telemetry.rs).
        let backend = backend::backend(cx);
        telemetry::start(&backend.runtime(), backend.http());
        // The menu bar item, the pet and the shortcuts that work in any app.
        companion::init(cx);
        // The main window closed (the pet's doesn't count): quit.
        cx.on_window_closed(|cx| {
            let main = cx.try_global::<companion::MainWindow>().map(|main| main.0.window_id());
            if cx.windows().iter().all(|window| Some(window.window_id()) != main) {
                quit_guard::quit_deliberately(cx);
            }
        })
        .detach();

        // `FILO_WINDOW=760x860` opens at that size (checking a screen by eye; `dev` builds).
        let (width, height) = test_run::window_size((1180., 800.));
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        // Debug builds, `FILO_WINDOW_FLOAT=1` (scripted checks while the Mac is in use): above other windows, without
        // taking the focus. GPUI stops drawing a window that's covered.
        let float = cfg!(feature = "dev") && std::env::var_os("FILO_WINDOW_FLOAT").is_some();
        let main_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(720.), px(520.))),
                    // No visible title bar: content runs to the top edge, the traffic lights sit
                    // in the sidebar, and the top band still drags the window.
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Filo".into()),
                        appears_transparent: true,
                        // Centred in the 84 px sidebar card (app::SIDEBAR_WIDTH minus 6 px margins).
                        traffic_light_position: Some(gpui::point(px(18.), px(20.))),
                    }),
                    kind: if float { gpui::WindowKind::PopUp } else { gpui::WindowKind::Normal },
                    focus: !float,
                    ..Default::default()
                },
                |window, cx| {
                    // With Filo in the menu bar or on the desktop, closing the window hides Filo instead of quitting.
                    window.on_window_should_close(cx, |_, cx| {
                        let settings = companion::settings(cx);
                        if settings.menu_bar || settings.pet {
                            cx.hide();
                            return false;
                        }
                        true
                    });
                    let shell = cx.new(|cx| app::AppShell::new(window, cx));
                    cx.new(|cx| Root::new(shell, window, cx))
                },
            )
            .expect("open window");
        cx.set_global(companion::MainWindow(main_window.into()));
        if !float {
            cx.activate(true);
        }
    });
    drop(runtime);
}

#[cfg(feature = "dev")]
async fn check(backend: backend::Backend, args: Vec<String>) -> i32 {
    eprintln!("backend: this Mac (voice messages: {})", live_transcription::origin());
    let path = args.first().cloned().unwrap_or("users:current".into());
    let call_args = args.get(1).and_then(|raw| serde_json::from_str(raw).ok()).unwrap_or(serde_json::json!({}));
    match backend.call_raw(&path, call_args).await {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
            0
        }
        Err(error) => {
            eprintln!("{path}: {error}");
            1
        }
    }
}

/// The developer's modes; a packaged build refuses them.
const DEV_MODES: [&str; 4] = ["--check", "--record", "--avatar-colors", "--update"];

/// What macOS may put on an app's command line: an old launch's process serial number, or a default set for this run
/// (`-NSDocumentRevisionsDebugMode YES`).
fn from_macos(arg: &str) -> bool {
    arg.starts_with("-psn_") || arg.starts_with("-NS") || arg.starts_with("-Apple")
}

/// `filo --app-icon out.png <icon>` for one of the icons to pick from (a colour of Filo's mark, an avatar, an uploaded
/// picture; `picture` is the last one uploaded): packaging draws the bundle's default icon this way, opening no data.
/// None when it's an avatar's icon, which needs the backend.
fn app_icon_of_choice(args: &[String]) -> Option<i32> {
    let Some(out) = args.first() else {
        eprintln!(
            "usage: filo --app-icon out.png [icon (filo, sage…, an avatar id, picture, an uploaded picture's file) | custom:<id>]"
        );
        return Some(2);
    };
    let icon = args.get(1).and_then(|id| match id.as_str() {
        "picture" => app_icon::pictures().pop().map(app_icon::Icon::Picture),
        id => app_icon::Icon::by_id(id),
    })?;
    Some(match app_icon::render_icon(&icon).and_then(|png| Ok(std::fs::write(out, png)?)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("app icon: {error:#}");
            1
        }
    })
}

async fn app_icon_check(backend: backend::Backend, args: Vec<String>) -> i32 {
    let Some(out) = args.first() else { return 2 };
    // An avatar id or `custom:<id>`; none, the main agent's picture.
    let look = async {
        let yours: Vec<avatars::CustomAvatar> = serde_json::from_value(
            backend.call_raw("customAvatars:list", serde_json::json!({})).await.map_err(anyhow::Error::msg)?,
        )?;
        let wanted = match args.get(1) {
            Some(wanted) => wanted.clone(),
            None => {
                let agents: Vec<screens::agents::AgentRow> = serde_json::from_value(
                    backend.call_raw("agents:list", serde_json::json!({})).await.map_err(anyhow::Error::msg)?,
                )?;
                agents
                    .into_iter()
                    .find(|agent| agent.main)
                    .map(|agent| agent.picture)
                    .ok_or_else(|| anyhow::anyhow!("no main agent"))?
            }
        };
        avatars::Look::of_picture(&wanted, &yours)
            .ok_or_else(|| anyhow::anyhow!("no avatar {wanted} (an animal or one of yours)"))
    };
    let result = async {
        let look = look.await?;
        eprintln!("avatar: {} ({})", look.label(), look.reference());
        let png = app_icon::render(&app_icon::picture(look)?)?;
        std::fs::write(out, &png)?;
        anyhow::Ok(png.len())
    };
    match result.await {
        Ok(bytes) => {
            println!("{out} · {bytes} bytes");
            0
        }
        Err(error) => {
            eprintln!("app icon: {error:#}");
            1
        }
    }
}

#[cfg(feature = "dev")]
async fn update_check(http: reqwest::Client, dir: Option<String>) -> i32 {
    println!(
        "this build: {} · channel {} · build {}",
        updater::version_label(),
        updater::CHANNEL.unwrap_or("none"),
        updater::build_number()
    );
    println!(
        "bundle: {}",
        updater::current_bundle().map(|path| path.display().to_string()).unwrap_or("none (bare binary)".into())
    );
    let Some(feed) = updater::feed() else {
        println!(
            "no update feed (local build; set FILO_UPDATE_FEED=https://github.com/bitooz/filo-desktop/releases/latest/download to try one)"
        );
        return 0;
    };
    println!("feed: {feed}");
    let release = match updater::fetch_release(http.clone(), feed.clone()).await {
        Ok(release) => release,
        Err(error) => {
            eprintln!("check failed: {error:#}");
            return 1;
        }
    };
    let newer = release.build > updater::build_number();
    println!("latest: {} · {} · {} bytes · {}", release.label(), release.commit, release.size, release.published_at);
    println!("{}", if newer { "an update is available" } else { "up to date" });
    let Some(dir) = dir else { return 0 };
    let staging = std::path::PathBuf::from(dir);
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let result = async {
        let zip = updater::download(http, feed, release, staging.clone(), progress).await?;
        println!("downloaded {} (size and SHA-256 match)", zip.display());
        let current = updater::current_bundle();
        let app = updater::unpack(&zip, &staging, current.as_deref())?;
        anyhow::Ok(app)
    };
    match result.await {
        Ok(app) => {
            println!("unpacked and verified {}", app.display());
            0
        }
        Err(error) => {
            eprintln!("update failed: {error:#}");
            1
        }
    }
}

#[cfg(feature = "dev")]
async fn record_check(backend: backend::Backend, args: Vec<String>) -> i32 {
    eprintln!("relay: {}", live_transcription::origin());
    // An existing .m4a instead of the microphone: what the relay hears in it, and with a session, sent there.
    if let Some(file) = args.first().filter(|arg| arg.ends_with(".m4a")) {
        let relay = live_transcription::Relay::new(&live_transcription::origin());
        let started = std::time::Instant::now();
        let transcript = live_transcription::transcribe_file(&relay, std::path::Path::new(file)).await;
        println!("transcript after {} ms: {transcript:?}", started.elapsed().as_millis());
        let bytes = std::fs::read(file).unwrap_or_default();
        return upload_check(backend, bytes, 1500., args.get(1).cloned()).await;
    }
    if let Some(file) = args.first().filter(|arg| arg.ends_with(".wav")) {
        // The WAV's samples, in 100 ms chunks at the pace a microphone delivers them.
        let wav = std::fs::read(file).unwrap_or_default();
        let pcm = live_transcription::wav_data(&wav).unwrap_or_default().to_vec();
        let duration_ms = pcm.len() as f64 / 32.;
        let live = live_transcription::LiveTranscription::start(&backend);
        let sink = live.sink();
        for chunk in pcm.chunks(3200) {
            sink.push(chunk.to_vec());
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let ended = std::time::Instant::now();
        let heard = live.heard().words;
        let transcript = live.finish(duration_ms).await;
        println!("words heard while streaming: {heard:?}");
        println!(
            "{duration_ms:.0} ms of audio · live transcript after {} ms: {transcript:?}",
            ended.elapsed().as_millis()
        );
        return if transcript.is_ok() { 0 } else { 1 };
    }
    let permission = recorder::permission();
    eprintln!("microphone: {permission:?}");
    if permission != recorder::Permission::Granted {
        return 2;
    }
    let seconds: f64 = args.first().and_then(|value| value.parse().ok()).unwrap_or(2.);
    let recorder = match recorder::Recorder::start() {
        Ok(recorder) => recorder,
        Err(error) => {
            eprintln!("start failed: {error}");
            return 1;
        }
    };
    let live = Some(live_transcription::LiveTranscription::start(&backend));
    let streamed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mic = {
        let (sink, streamed) = (live.as_ref().map(|live| live.sink()), streamed.clone());
        recorder::MicStream::start(live_transcription::SAMPLE_RATE, move |pcm| {
            streamed.fetch_add(pcm.len(), std::sync::atomic::Ordering::Relaxed);
            if let Some(sink) = &sink {
                sink.push(pcm);
            }
        })
    };
    if let Err(error) = &mic {
        eprintln!("microphone stream failed: {error}");
    }
    let mut peak = 0f32;
    let steps = (seconds * 10.) as usize;
    for _ in 0..steps {
        std::thread::sleep(std::time::Duration::from_millis(100));
        peak = peak.max(recorder.level());
    }
    let (path, duration_ms) = recorder.finish();
    drop(mic);
    let bytes = std::fs::read(&path).unwrap_or_default();
    println!("{} · {duration_ms:.0} ms · {} bytes · peak level {peak:.2}", path.display(), bytes.len());
    println!("microphone stream: {} ms of 16 kHz audio", streamed.load(std::sync::atomic::Ordering::Relaxed) / 32);
    if let Some(live) = live {
        let ended = std::time::Instant::now();
        let transcript = live.finish(duration_ms).await;
        println!("live transcript after {} ms: {transcript:?}", ended.elapsed().as_millis());
    }
    recorder::remove(&path);
    upload_check(backend, bytes, duration_ms, args.get(1).cloned()).await
}

#[cfg(feature = "dev")]
async fn upload_check(backend: backend::Backend, bytes: Vec<u8>, duration_ms: f64, session_id: Option<String>) -> i32 {
    let Some(session_id) = session_id else { return 0 };
    let ticket = backend
        .call_raw(
            "voiceRecordings:createUpload",
            serde_json::json!({ "sessionId": session_id, "durationMs": duration_ms.round() }),
        )
        .await;
    let token = match ticket.as_ref().ok().and_then(|ticket| ticket.get("token")?.as_str().map(str::to_string)) {
        Some(token) => token,
        None => {
            eprintln!("createUpload failed: {ticket:?}");
            return 1;
        }
    };
    let upload_id = ticket
        .as_ref()
        .ok()
        .and_then(|ticket| ticket.get("uploadId")?.as_str().map(str::to_string))
        .unwrap_or_default();
    let file = std::env::temp_dir().join(format!("filo-record-check-{}.m4a", uuid::Uuid::new_v4()));
    if std::fs::write(&file, &bytes).is_err() {
        return 1;
    }
    let kept = backend.upload_voice(token, file.clone()).await;
    let _ = std::fs::remove_file(&file);
    if let Err(error) = kept {
        eprintln!("upload failed: {error}");
        return 1;
    }
    let sent = backend.call_raw("voiceRecordings:send", serde_json::json!({ "uploadId": upload_id })).await;
    println!("send: {sent:?}");
    if sent.is_ok() { 0 } else { 1 }
}

/// Quits on data from a newer Filo, with an alert that says so (in a test run, only the message on stderr).
fn stop_for_newer_data(error: &str) -> ! {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    eprintln!("filo: {error}");
    if !test_run::active() {
        let text = "Your agents, chats and notes were last opened by a newer version of Filo, which this one can't read. Open the newer Filo, or update this one.";
        // SAFETY: AppKit messages on the main thread, before the app's own run loop starts.
        unsafe {
            let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![&app, activateIgnoringOtherApps: true];
            let alert: Retained<AnyObject> = msg_send![class!(NSAlert), new];
            let _: () = msg_send![&alert, setMessageText: &*NSString::from_str("This Filo is older than your data")];
            let _: () = msg_send![&alert, setInformativeText: &*NSString::from_str(text)];
            let _: isize = msg_send![&alert, runModal];
        }
    }
    std::process::exit(1);
}

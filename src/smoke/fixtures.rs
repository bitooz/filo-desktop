// What the scripted fixtures' test windows share (`FILO_OPEN=chat-composer`, `chat-send`, `chat-scroll`, `menus`,
// `glances:<file>`): the window, how its check ends, and its screenshots (`FILO_FIXTURE_SHOTS`).

use gpui::{
    App, AppContext as _, AsyncWindowContext, Bounds, Entity, Render, Window, WindowBounds, WindowOptions, px, size,
};

/// Opens a fixture's window, `title`, `width`×`height`, whose content is `host`; `drive` runs its checks, then the
/// process ends, printing how they went as `name`, with 1 when any failed. Above other windows without taking the focus
/// (the check runs while the Mac is in use), unless `focus` (a web view is only made in the active window).
pub fn open<V: Render + 'static>(
    cx: &mut App,
    name: &'static str,
    title: &str,
    (width, height): (f32, f32),
    focus: bool,
    host: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    drive: impl AsyncFnOnce(Entity<V>, &mut AsyncWindowContext) -> usize + 'static,
) {
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    if focus {
        cx.activate(true);
    }
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(title.to_string().into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            kind: if focus { gpui::WindowKind::Normal } else { gpui::WindowKind::PopUp },
            focus,
            ..Default::default()
        },
        |window, cx| {
            let host = host(window, cx);
            let driven = host.clone();
            window
                .spawn(cx, async move |cx| {
                    let failures = drive(driven, cx).await;
                    println!(
                        "{name}: {}",
                        if failures == 0 { "all checks passed".to_string() } else { format!("{failures} failed") }
                    );
                    crate::window_shot::flush(cx.background_executor()).await;
                    std::process::exit(if failures == 0 { 0 } else { 1 });
                })
                .detach();
            cx.new(|cx| gpui_component::Root::new(host, window, cx))
        },
    )
    .expect("open window");
}

/// A screenshot of the fixture's window (found by its `width`) as `<shots>/<name>.png`, with `FILO_FIXTURE_SHOTS` set.
pub fn shot(shots: Option<&str>, width: f64, name: &str) {
    let Some(dir) = shots else { return };
    if let Some(number) = crate::smoke::native_input::window_number(width) {
        crate::window_shot::save(number, format!("{dir}/{name}.png"));
    }
}

// Feed and Notes, the optional pages (crates/filo-local `pages`): switched on or off in Settings › Pages and on the
// welcome, one section both draw (`section`). Read once at launch and kept here; a switch shows at once and is saved
// in the background.
//
// Off, the page is gone from the sidebar, ⌘1 – ⌘7 and the View menu (nav.rs `sidebar`, chrome.rs `pages_changed`), the
// shell opens neither its tab nor its pages (app.rs), and with Notes off a Feed post has no Save to Notes. Agents lose
// its tools and their prompt's lines from their next turn (crates/filo-local mcp.rs). Nothing of it is deleted.

use gpui::{AnyElement, App, Global, IntoElement as _, SharedString, Window};
use gpui_component::switch::Switch;

pub use filo_local::pages::{Page, Pages};

use crate::nav::Tab;
use crate::ui;

struct PagesGlobal(Pages);

impl Global for PagesGlobal {}

/// Reads them as saved, at launch.
pub fn init(cx: &mut App) {
    let pages = crate::backend::backend(cx).pages();
    cx.set_global(PagesGlobal(pages));
    crate::chrome::pages_changed(cx);
}

/// A new install's first welcome: Feed and Notes start off unless they have something in them already.
pub fn start_off(cx: &mut App) {
    let pages = crate::backend::backend(cx).start_pages_off();
    show(pages, cx);
}

/// Which pages are on now (both until `init`).
pub fn get(cx: &App) -> Pages {
    cx.try_global::<PagesGlobal>().map(|pages| pages.0).unwrap_or_default()
}

fn show(pages: Pages, cx: &mut App) {
    cx.set_global(PagesGlobal(pages));
    crate::chrome::pages_changed(cx);
    cx.refresh_windows();
}

/// Turns `page` on or off: the app follows at once; if it can't be saved, it goes back as it was and says why.
pub fn set(page: Page, on: bool, window: &mut Window, cx: &mut App) {
    let before = get(cx);
    if before.on(page) == on {
        return;
    }
    show(
        Pages {
            feed: if page == Page::Feed { on } else { before.feed },
            notes: if page == Page::Notes { on } else { before.notes },
        },
        cx,
    );
    crate::nav::pages_changed(window, cx);
    let saving = crate::backend::backend(cx).set_page(page, on);
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = saving.await {
                cx.update(|window, cx| {
                    show(before, cx);
                    crate::nav::pages_changed(window, cx);
                    ui::toast_error(
                        window,
                        cx,
                        format!("Couldn't turn {} {}", page.label(), if on { "on" } else { "off" }),
                        error,
                    );
                })
                .ok();
            }
        })
        .detach();
}

/// "Pages": a switch for Feed and one for Notes, each with its sidebar icon. Settings and the welcome draw it;
/// `id` keeps their rows apart.
pub fn section(id: &str, cx: &App) -> AnyElement {
    let pages = get(cx);
    let rows = [(Page::Feed, Tab::Feed), (Page::Notes, Tab::Notes)]
        .into_iter()
        .map(|(page, tab)| {
            let row = format!("{id}-{}", tab.label().to_lowercase());
            let switch = Switch::new(SharedString::from(format!("{row}-switch")))
                .checked(pages.on(page))
                .on_click(move |checked: &bool, window, cx| set(page, *checked, window, cx));
            ui::ListRow::new(SharedString::from(row.clone()), page.label())
                .icon(tab.icon().unwrap_or_default())
                .trailing(crate::smoke::tracked(format!("{row}-switch"), switch))
                .into_any_element()
        })
        .collect();
    ui::section(Some("Pages"), None, rows, cx).into_any_element()
}

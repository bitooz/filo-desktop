// Settings › Browser: the engine Filo's browser runs its pages in (browser/engine.rs). WebKit, built into macOS, or
// Chromium, marked Beta, downloaded the first time it's picked. Picking one shows each step under the rows (looking for
// the engine, downloading with its progress, checking it, bringing the logins over, opening the tabs again), then what
// it brought over, or why it couldn't switch. With Chromium installed and WebKit picked, Remove Chromium deletes it.

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px, relative,
};

use crate::browser::engine::{self, Busy, Kind};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, ListRow};

/// About how much Chromium downloads, before the feed says exactly.
const DOWNLOAD_MB: u64 = 140;

pub struct BrowserSettingsScreen {
    _engines: Option<Subscription>,
}

impl BrowserSettingsScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _engines = engine::engines(cx).map(|engines| cx.observe(&engines, |_, _, cx| cx.notify()));
        BrowserSettingsScreen { _engines }
    }

    /// An engine to pick: its name (Chromium's with Beta), what it is, and a tick on the one in use.
    fn choice(kind: Kind, picked: bool, busy: bool, subtitle: String, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let id = format!("browser-engine-{}", kind.label().to_lowercase());
        let group = SharedString::from(format!("{id}-row"));
        let row = div()
            .min_h(px(52.))
            .px(px(space::SM + 2.))
            .py(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .rounded(px(radius::SM))
            .when(!picked && !busy, |this| this.group_hover(group.clone(), |style| style.bg(colors.text.opacity(0.06))))
            .child(
                div()
                    .size(px(28.))
                    .flex_none()
                    .rounded(px(8.))
                    .bg(colors.surface_strong)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        ui::icon(if kind == Kind::WebKit { "compass" } else { "globe" })
                            .size(px(16.))
                            .text_color(colors.text),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(div().callout().medium().text_color(colors.text).child(kind.label()))
                            .when(kind == Kind::Chromium, |this| {
                                this.child(ui::chip("Beta", ui::Tone::Accent, true, cx))
                            }),
                    )
                    .child(div().footnote().text_color(colors.text_secondary).child(subtitle)),
            )
            .child(
                div()
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(picked, |this| this.child(ui::icon("check").size(px(18.)).text_color(colors.accent))),
            );
        let row = div().id(SharedString::from(id.clone())).group(group).p(px(space::XS)).child(row);
        let row =
            if picked || busy { row } else { row.cursor_pointer().on_click(move |_, _, cx| engine::choose(kind, cx)) };
        crate::smoke::tracked(id, row)
    }

    /// What's going on: a spinner, the step, and the download's progress.
    fn progress(busy: &Busy, received: u64, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let mb = |bytes: u64| (bytes as f64 / 1_000_000.).round() as u64;
        let (words, done) = match busy {
            Busy::Looking => ("Looking for the newest Chromium…".to_string(), None),
            Busy::Downloading { total, .. } => (
                format!("Downloading Chromium… {} of {} MB", mb(received.min(*total)), mb(*total)),
                Some(received as f32 / (*total).max(1) as f32),
            ),
            Busy::Checking => ("Checking the download…".to_string(), None),
            Busy::CopyingLogins => ("Bringing your logins over…".to_string(), None),
            Busy::Restarting => ("Opening your tabs again…".to_string(), None),
        };
        div()
            .px(px(space::MD))
            .py(px(space::SM + 2.))
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(ui::spinner(14., cx))
                    .child(div().footnote().text_color(colors.text_secondary).child(words)),
            )
            .when_some(done, |this, done| {
                this.child(
                    div().h(px(4.)).w_full().rounded(px(radius::FULL)).bg(colors.surface_strong).child(
                        div().h_full().w(relative(done.clamp(0., 1.))).rounded(px(radius::FULL)).bg(colors.accent),
                    ),
                )
            })
            .into_any_element()
    }
}

impl Render for BrowserSettingsScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let current = engine::current();
        let installed = engine::installed();
        let state = engine::engines(cx).map(|engines| {
            let engines = engines.read(cx);
            (
                engines.busy.clone(),
                engines.received(),
                engines.error.clone(),
                engines.note.clone(),
                engines.updated.clone(),
            )
        });
        let (busy, received, error, note, updated) = state.unwrap_or_default();
        let chromium_subtitle = match (&installed, &updated) {
            (_, Some(updated)) => format!("Updated to Chromium {updated}, which starts with your next new tab"),
            (Some(installed), None) => format!("Chromium {} · updates on its own", installed.chromium),
            (None, None) if engine::app_path().is_some() => "A build of this Mac's".to_string(),
            (None, None) => format!("Downloads once, about {DOWNLOAD_MB} MB · sites made for Chrome work as there"),
        };
        let rows = vec![
            Self::choice(
                Kind::WebKit,
                current == Kind::WebKit,
                busy.is_some(),
                "Built into macOS, like Safari · lighter on memory".into(),
                cx,
            ),
            Self::choice(Kind::Chromium, current == Kind::Chromium, busy.is_some(), chromium_subtitle, cx),
        ]
        .into_iter()
        .chain(busy.as_ref().map(|busy| Self::progress(busy, received, cx)))
        .collect();
        let footer = SharedString::from(
            "Switching opens your tabs again in the other engine and brings your logins over. A few sites keep \
             sign-ins that can't move: sign in to those again.",
        );
        let remove = (installed.is_some() && current == Kind::WebKit && busy.is_none()).then(|| {
            ListRow::new("browser-engine-remove", "Remove Chromium")
                .icon("trash-2")
                .subtitle("Frees about 330 MB; it downloads again if you pick it")
                .on_click(|_, window, cx| {
                    if let Err(error) = engine::remove(cx) {
                        ui::toast_error(window, cx, "Couldn’t remove Chromium", error);
                    }
                })
                .into_any_element()
        });
        ui::sub_page(
            ui::sub_page_header("browser-settings", "Browser", vec![], cx),
            div().id("browser-settings-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                ui::page_column()
                    .pt(px(space::SM))
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .children(error.map(|error| ui::banner(error, ui::Tone::Danger, cx)))
                    .children(note.filter(|_| busy.is_none()).map(|note| ui::banner(note, ui::Tone::Success, cx)))
                    .child(ui::section(Some("Engine"), Some(footer), rows, cx))
                    .children(remove.map(|remove| ui::section(None, None, vec![remove], cx)))
                    .child(
                        div()
                            .px(px(space::XXS))
                            .footnote()
                            .text_color(colors.text_tertiary)
                            .child("Agents' tabs run in the background in either engine; open the browser (⇧⌘B) to watch them or take over."),
                    ),
            ),
            cx,
        )
    }
}

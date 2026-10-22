// Glances (design/filo-desktop-glances.html): a small visual Filo shows above its reply, drawn from HTML it wrote. The
// message carries only the card's head (`Message::glance`); the page comes from `glances:get` once the card is first
// laid out, and shows in a native web view: a host page with no network that frames the glance in a sandboxed iframe
// (scripts, but no origin, so nothing in it can reach the app or the host's IPC), with the app's theme and font put in.
//
// A native view draws above everything GPUI paints, so each one sits inside a clip view of its own, cut to what of the
// card shows between the chat's top and the composer, and a sweep at the end of each frame (`sweep`) parks every one
// that wasn't laid out in it: scrolled away, another page, or under a dialog. Parked is see-through and out of reach,
// never hidden, so WebKit keeps the page drawn at full priority and it shows at once when it's back (`native::park`);
// a new page's card shows its placeholder until the page has drawn (`PageMessage::Painted`). Scroll-wheel events pass through the
// card's view to GPUI, so the chat scrolls over a glance as over anything else. Open larger shows the same page larger
// the way the media viewer shows a picture (media_viewer.rs): over the whole window on its dark backdrop, the page on a
// card of the theme's surface in the middle, and a bar of controls along the bottom: the views (the visual, the table,
// the text), Copy, Ask about this and Save to Artifacts. Esc, the close button or a click on the backdrop closes it.
//
// The Feed shows glances too (src/screens/feed_glance.rs): the same pages and the same larger view, opened from a post
// (`SheetFrom::Feed`), and a page there can send a pick back (`PageMessage::Choice`, from the head the Feed puts in).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::OnceLock;
use std::time::Instant;

use base64::Engine as _;
use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, ClipboardItem, Context, Element, ElementId, Entity,
    GlobalElementId, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Style, Styled as _, Task as GpuiTask, WeakEntity, Window,
    div, px, relative,
};
use gpui_component::WindowExt as _;
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::types::{GlanceRef, Message};
use super::Conversation;
use crate::backend;
use crate::format;
use crate::theme::{ActiveTheme as _, Palette, Typography as _, radius, space};
use crate::ui::{self, IconButton};

/// A glance as `glances:get` returns it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlanceData {
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub visual_label: Option<String>,
    pub text: String,
    #[serde(default)]
    pub table: Option<GlanceTable>,
    /// The whole page: theme roles, helpers, tooltips and the fragment (crates/filo-local glance_document.rs).
    pub document: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GlanceTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Where a glance's page stands for this chat.
enum Fetch {
    Loading,
    Ready(Rc<GlanceData>),
    Failed,
}

/// Most glance pages kept alive in one chat: one web view each, so the ones longest out of sight go first.
const MAX_PAGES: usize = 4;
/// A card's page height before its page says how tall it is.
pub(crate) const DEFAULT_HEIGHT: f32 = 180.;
const MIN_HEIGHT: f32 = 24.;
const MAX_HEIGHT: f32 = 640.;

/// What a glance's page says: how tall it is, or Esc pressed in it (the page has the keyboard after a click in it, so
/// GPUI doesn't hear the key: it closes what's on top here, escape.rs).
pub(crate) enum PageMessage {
    /// How tall it is, and its content's natural width: the narrowest at which nothing is cut off (`EMBED_JS`).
    Size {
        height: f32,
        width: Option<f32>,
    },
    /// A frame has been drawn since it first said how tall it is: it may show (until then the card shows its
    /// placeholder).
    Painted,
    Escape,
    /// One of its choices clicked (`data-choice`): only a page in the Feed sends these.
    Choice(String),
}

/// Esc from a page: closes the window's top layer.
pub(crate) fn page_escape(window: gpui::AnyWindowHandle, cx: &mut gpui::AsyncApp) {
    cx.update_window(window, |_, window, cx| {
        crate::escape::dismiss(window, cx);
    })
    .ok();
}

thread_local! {
    /// Each glance's page height, as its page last measured it: the card keeps it when the chat comes back.
    static HEIGHTS: RefCell<HashMap<String, f32>> = RefCell::new(HashMap::new());
    /// Each glance's content at its natural width, as its page last measured it: the card is sized from it.
    static WIDTHS: RefCell<HashMap<String, f32>> = RefCell::new(HashMap::new());
}

pub(crate) fn known_height(glance_id: &str) -> Option<f32> {
    HEIGHTS.with(|heights| heights.borrow().get(glance_id).copied())
}

/// Notes a glance page's height as it measured itself (the Feed's cards keep it too); whether it changed.
pub(crate) fn note_height(glance_id: &str, height: f32) -> bool {
    let height = height.clamp(MIN_HEIGHT, MAX_HEIGHT);
    HEIGHTS.with(|heights| {
        let previous = heights.borrow_mut().insert(glance_id.to_string(), height);
        previous.is_none_or(|previous| (previous - height).abs() > 0.5)
    })
}

fn known_width(glance_id: &str) -> Option<f32> {
    WIDTHS.with(|widths| widths.borrow().get(glance_id).copied())
}

/// The card's padding and border either side of its page.
const CARD_CHROME: f32 = 2. * 14. + 2.;
/// The widest a glance's natural width counts (anything wider scrolls sideways in the widest card anyway).
const MAX_NATURAL: f32 = 4_000.;

/// Where a glance's card goes in a chat whose list is `list` wide with `gutter` left at its right (timeline.rs
/// `gutter`): its left edge and width, in points from the list's left. It starts exactly where the reply bubbles do and
/// grows only to the right: at least a bubble's width, at most up to the timeline's rail less the gap the bubbles keep
/// from it (`CLEAR` from the chat's edge); between those as wide as its content needs (`natural`, the page's), or the
/// most for a `wide` one.
pub(super) fn card_place(list: f32, gutter: f32, natural: Option<f32>, wide: bool) -> (f32, f32) {
    use crate::theme::{MAX_CONTENT_WIDTH, PAGE_PADDING};
    let shell = list.min(MAX_CONTENT_WIDTH);
    let column = (shell - 2. * PAGE_PADDING).max(1.);
    let left = (list - shell) / 2. + PAGE_PADDING;
    let bubble = (column * 0.86).floor();
    let most = (list + gutter - super::timeline::CLEAR - left).max(bubble);
    let wanted = if wide { most } else { natural.map_or(bubble, |natural| (natural + CARD_CHROME).ceil()) };
    (left, wanted.clamp(bubble, most))
}

/// A chat's glances: their pages, what each card shows, and the head action the pointer is on.
#[derive(Default)]
pub(super) struct Glances {
    fetched: HashMap<String, Fetch>,
    as_text: HashSet<String>,
    pages: HashMap<String, InlinePage>,
    /// The card and the label of the head action under the pointer (GPUI's tooltips open under the pointer, where
    /// the page would cover them, so the card shows its own beside the actions).
    tip: Option<(String, SharedString)>,
    /// `note_glance_clip`.
    clip_bottom: Option<Pixels>,
    /// The chat's list as last laid out, its width (`note_glance_clip`): the cards are placed across it.
    list_width: f32,
}

struct InlinePage {
    page: Rc<NativePage>,
    used: Instant,
    _ipc: GpuiTask<()>,
}

/// What a glance card shows, for the row's measure: a change measures the row again.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Look {
    loading: bool,
    failed: bool,
    as_text: bool,
    height: u32,
    /// The content's natural width (0 until the page says): a change places the card again.
    width: u32,
}

impl Conversation {
    /// The card's look for `glance`, fetching its page the first time (the fixture's glances are given).
    pub(super) fn glance_look(&mut self, glance: &GlanceRef, cx: &mut Context<Self>) -> Look {
        let id = glance.glance_id.clone();
        if !self.glances.fetched.contains_key(&id) && !self.fixture() {
            self.glances.fetched.insert(id.clone(), Fetch::Loading);
            let glance_id = id.clone();
            backend::call(
                cx,
                "glances:get",
                json!({ "glanceId": id }),
                move |this: &mut Self, result: Result<Option<GlanceData>, String>, cx| {
                    let fetch = match result {
                        Ok(Some(data)) => Fetch::Ready(Rc::new(data)),
                        _ => Fetch::Failed,
                    };
                    this.glances.fetched.insert(glance_id, fetch);
                    this.rebuild(cx);
                    cx.notify();
                },
            );
        }
        let fetch = self.glances.fetched.get(&id);
        Look {
            loading: matches!(fetch, None | Some(Fetch::Loading)),
            failed: matches!(fetch, Some(Fetch::Failed)),
            as_text: self.glances.as_text.contains(&id),
            height: known_height(&id).unwrap_or(DEFAULT_HEIGHT).round() as u32,
            width: known_width(&id).unwrap_or(0.).round() as u32,
        }
    }

    /// The fixture's glance (`FILO_OPEN=glances:<file>`), as `glances:get` would give it.
    #[cfg(feature = "dev")]
    pub fn set_fixture_glance(&mut self, glance_id: String, data: GlanceData, cx: &mut Context<Self>) {
        self.glances.fetched.insert(glance_id, Fetch::Ready(Rc::new(data)));
        self.rebuild(cx);
        cx.notify();
    }

    /// Out of sight: the web views go (they come back when the chat shows again; the heights stay).
    pub(super) fn drop_glance_pages(&mut self) {
        self.glances.pages.clear();
        self.glances.tip = None;
    }

    /// A card's page as last placed (the fixture's checks): whether it has said how tall it is, the height it said, and
    /// what of it shows, in window points (None when hidden).
    #[cfg(feature = "dev")]
    pub(super) fn glance_probe(
        &self,
        glance_id: &str,
        window: &Window,
    ) -> Option<(bool, Option<f32>, Option<Bounds<Pixels>>)> {
        let page = &self.glances.pages.get(glance_id)?.page;
        Some((page.ready.get(), known_height(glance_id), page.on_screen(window)))
    }

    /// The glance's content at its natural width, as its page last said (the fixture's checks).
    #[cfg(feature = "dev")]
    pub(super) fn glance_natural_width(&self, glance_id: &str) -> Option<f32> {
        known_width(glance_id)
    }

    #[cfg(feature = "dev")]
    pub(super) fn glance_holes(&self, glance_id: &str) -> usize {
        self.glances.pages.get(glance_id).map_or(0, |page| page.page.holes())
    }

    #[cfg(feature = "dev")]
    pub(super) fn toggle_glance_text_for(&mut self, glance_id: &str, cx: &mut Context<Self>) {
        self.toggle_glance_text(glance_id, cx);
    }

    #[cfg(feature = "dev")]
    pub(super) fn open_glance_for(
        &mut self,
        message_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<GlanceSheet>> {
        let message = self.rows.iter().find_map(|row| match row {
            super::ViewRow::Message { message, .. } if message.id == message_id => Some(message.clone()),
            _ => None,
        })?;
        self.open_glance(&message, window, cx)
    }

    /// Where the chat's list shows, and the composer's height over its bottom (the fixture's checks).
    #[cfg(feature = "dev")]
    pub(super) fn glance_column(&self) -> (Bounds<Pixels>, f32) {
        (self.list_state.viewport_bounds(), self.composer_height)
    }

    fn glance_data(&self, glance_id: &str) -> Option<Rc<GlanceData>> {
        match self.glances.fetched.get(glance_id) {
            Some(Fetch::Ready(data)) => Some(data.clone()),
            _ => None,
        }
    }

    /// The page for a card, made when the card is first drawn while the window is in front (making a web view brings
    /// the app forward), and made again when the theme changed.
    fn glance_page(
        &mut self,
        glance_id: &str,
        data: &GlanceData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Rc<NativePage>> {
        let theme = ThemeCss::new(cx);
        if let Some(inline) = self.glances.pages.get_mut(glance_id)
            && inline.page.theme == theme.key
        {
            inline.used = Instant::now();
            return Some(inline.page.clone());
        }
        self.glances.pages.remove(glance_id);
        if !window.is_window_active() {
            return None;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PageMessage>();
        let page = NativePage::open(window, &data.document, &theme, false, tx)?;
        let id = glance_id.to_string();
        let weak = Rc::downgrade(&page);
        let handle = window.window_handle();
        let ipc = cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                let (height, width) = match message {
                    PageMessage::Size { height, width } => {
                        (height.clamp(MIN_HEIGHT, MAX_HEIGHT), width.map(|width| width.clamp(0., MAX_NATURAL)))
                    }
                    PageMessage::Painted => {
                        let first = weak.upgrade().is_some_and(|page| !page.ready.replace(true));
                        if first && this.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                        continue;
                    }
                    PageMessage::Escape => {
                        page_escape(handle, cx);
                        continue;
                    }
                    // A chat's glances offer no choices.
                    PageMessage::Choice(_) => continue,
                };
                let mut changed = HEIGHTS.with(|heights| {
                    let previous = heights.borrow_mut().insert(id.clone(), height);
                    previous.is_none_or(|previous| (previous - height).abs() > 0.5)
                });
                if let Some(width) = width {
                    changed |= WIDTHS.with(|widths| {
                        let previous = widths.borrow_mut().insert(id.clone(), width);
                        previous.is_none_or(|previous| (previous - width).abs() > 0.5)
                    });
                }
                if !changed {
                    continue;
                }
                let updated = this.update(cx, |this, cx| {
                    this.rebuild(cx);
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        // One web view per card on screen: the one longest out of sight goes when there are too many.
        if self.glances.pages.len() >= MAX_PAGES {
            let oldest = self.glances.pages.iter().min_by_key(|(_, inline)| inline.used).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest.filter(|oldest| self.glances.pages[oldest].used.elapsed().as_millis() > 250) {
                self.glances.pages.remove(&oldest);
            }
        }
        self.glances
            .pages
            .insert(glance_id.to_string(), InlinePage { page: page.clone(), used: Instant::now(), _ipc: ipc });
        Some(page)
    }

    fn toggle_glance_text(&mut self, glance_id: &str, cx: &mut Context<Self>) {
        if !self.glances.as_text.remove(glance_id) {
            self.glances.as_text.insert(glance_id.to_string());
        }
        self.glances.tip = None;
        self.rebuild(cx);
        cx.notify();
    }

    fn set_glance_tip(&mut self, glance_id: &str, label: Option<&'static str>, cx: &mut Context<Self>) {
        let tip = label.map(|label| (glance_id.to_string(), SharedString::from(label)));
        let same = match (&tip, &self.glances.tip) {
            (Some(tip), Some(current)) => tip == current,
            (None, Some((card, _))) => card != glance_id,
            (None, None) => true,
            _ => false,
        };
        if !same && (tip.is_some() || self.glances.tip.as_ref().is_some_and(|(card, _)| card == glance_id)) {
            self.glances.tip = tip;
            cx.notify();
        }
    }

    /// Opens the glance larger, over the chat.
    fn open_glance(
        &mut self,
        message: &Message,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<GlanceSheet>> {
        let glance = message.glance.clone()?;
        let data = self.glance_data(&glance.glance_id)?;
        let conversation = cx.entity().downgrade();
        let from = SheetFrom::Chat { message: message.clone(), conversation };
        let sheet = cx.new(|cx| GlanceSheet::new(glance, data, from, self.fixture(), window, cx));
        present_sheet(sheet.clone(), window, cx);
        Some(sheet)
    }

    /// Asks Filo about the glance: a reply to its message, in the composer.
    fn ask_about_glance(&mut self, message: &Message, window: &mut Window, cx: &mut Context<Self>) {
        self.reply(message, window, cx);
    }

    /// The glance's card in the chat: its head with the actions, then the page, its text, or a placeholder.
    pub(super) fn render_glance(
        &mut self,
        message: &Message,
        look: &Look,
        joins_prev: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = *cx.colors();
        let Some(glance) = message.glance.clone() else { return div().into_any_element() };
        let id = glance.glance_id.clone();
        let data = self.glance_data(&id);
        let group = SharedString::from(format!("glance-{}", message.id));
        let icon = icon_for(glance.icon.as_deref().or(data.as_ref().and_then(|data| data.icon.as_deref())));
        let text = glance_text(message, data.as_deref());
        let show_text = look.as_text || look.failed;
        // The row isn't in the chat's column: the card is as wide as its content, from a bubble's width to the rail.
        let gutter = super::timeline::gutter(self.width);
        let measured = self.glances.list_width;
        let list = if measured > 0. {
            measured
        } else if self.width > 0. {
            self.width - gutter
        } else {
            ui::content_width()
        };
        let (left, width) = card_place(list, gutter, (look.width > 0).then_some(look.width as f32), glance.wide);

        let head_button =
            |name: &'static str, icon: &'static str, label: &'static str, selected: bool, cx: &mut Context<Self>| {
                let card = id.clone();
                div()
                    .id(SharedString::from(format!("glance-{name}-{}", message.id)))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.set_glance_tip(&card, hovered.then_some(label), cx)
                    }))
                    .child(
                        IconButton::new(SharedString::from(format!("glance-{name}-button-{}", message.id)), icon)
                            .small()
                            .selected(selected),
                    )
            };
        let tip = self.glances.tip.as_ref().filter(|(card, _)| card == &id).map(|(_, label)| label.clone());
        let actions = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .opacity(if tip.is_some() { 1. } else { 0. })
            .group_hover(group.clone(), |style| style.opacity(1.))
            .children(tip.map(|label| div().mr(px(space::XS)).caption().text_color(colors.text_secondary).child(label)))
            .when(!look.failed, |this| {
                let card = id.clone();
                this.child(
                    head_button(
                        "text",
                        if look.as_text { icon } else { "list" },
                        if look.as_text { "Show the visual" } else { "Show as text" },
                        look.as_text,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_glance_text(&card, cx))),
                )
            })
            .child({
                let copy = message.content.clone();
                head_button("copy", "copy", "Copy", false, cx).on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                    ui::toast(window, cx, "Copied as text");
                })
            })
            .when(data.is_some(), |this| {
                let message = message.clone();
                this.child(head_button("open", "arrow-up-right", "Open larger", false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.open_glance(&message, window, cx);
                    },
                )))
            });
        let head = div()
            .h(px(28.))
            .mb(px(space::SM))
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(ui::icon(icon).size(px(16.)).flex_none().text_color(colors.text_tertiary))
            .map(|this| {
                if look.loading {
                    this.child(div().footnote().text_color(colors.text_tertiary).child("Making a chart…"))
                } else {
                    this.child(
                        div()
                            .min_w_0()
                            .subhead()
                            .semibold()
                            .text_color(colors.text)
                            .truncate()
                            .child(glance.title.clone()),
                    )
                    .children(
                        glance
                            .as_of
                            .clone()
                            .map(|as_of| div().flex_none().caption().text_color(colors.text_tertiary).child(as_of)),
                    )
                }
            })
            .child(div().flex_1())
            .when(!look.loading, |this| this.child(actions));

        let body: AnyElement = if look.loading {
            skeleton_bars(&colors).into_any_element()
        } else if show_text {
            text_block(&text, &colors).into_any_element()
        } else {
            let data = data.clone().expect("a glance that isn't loading or failed has its page");
            let page = self.glance_page(&id, &data, window, cx);
            let ready = page.as_ref().is_some_and(|page| page.ready.get());
            let bottom = self.glances.clip_bottom;
            let sideways = page.clone();
            div()
                .relative()
                .w(px(width - CARD_CHROME))
                .h(px(look.height as f32))
                // The wheel reaches GPUI over the page (`hit_test`): a mostly sideways one scrolls content wider than
                // the card inside it, anything else scrolls the chat.
                .on_scroll_wheel(move |event: &gpui::ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                    if let Some(page) = sideways.as_ref().filter(|_| dx.abs() > dy.abs()) {
                        page.scroll_sideways(-dx);
                        cx.stop_propagation();
                    }
                })
                .when(!ready, |this| this.child(skeleton_bars(&colors)))
                .children(page.map(|page| {
                    div().absolute().inset_0().child(PageElement { page, clip_bottom: bottom, show: true })
                }))
                .into_any_element()
        };

        div()
            .id(SharedString::from(format!("row-{}", message.id)))
            .w_full()
            .pt(px(if joins_prev { space::XS } else { space::MD }))
            .pl(px(left))
            .flex()
            .flex_col()
            .items_start()
            .child(
                div()
                    .group(group)
                    .w(px(width))
                    .min_w_0()
                    .pt(px(10.))
                    .px(px(14.))
                    .pb(px(14.))
                    .rounded(px(radius::LG))
                    .bg(colors.surface)
                    .border_1()
                    .border_color(colors.border.opacity(0.7))
                    .child(head)
                    .child(body),
            )
            .into_any_element()
    }

    /// The lowest a glance's page may show in the chat (the composer floats over the list's bottom), as the list was
    /// last laid out. Read before the list lays out its rows: it holds its state while it does.
    pub(super) fn note_glance_clip(&mut self) {
        let viewport = self.list_state.viewport_bounds();
        self.glances.clip_bottom =
            (viewport.size.height > px(0.)).then(|| viewport.bottom() - px(self.composer_height));
        self.glances.list_width = f32::from(viewport.size.width);
    }
}

/// Opens `sheet` once its page has said how tall it is at the viewer's width (or a moment at most): its card is then
/// that size from the first frame, and nothing moves as the page comes in.
fn present_sheet(sheet: Entity<GlanceSheet>, window: &mut Window, cx: &mut App) {
    match sheet.update(cx, |sheet, _| sheet.measured.take()) {
        Some(measured) => {
            window
                .spawn(cx, async move |cx| {
                    let timer = cx.background_executor().timer(MEASURE_WAIT);
                    futures::future::select(measured, Box::pin(timer)).await;
                    cx.update(|window, cx| show_sheet(sheet, window, cx)).ok();
                })
                .detach();
        }
        None => show_sheet(sheet, window, cx),
    }
}

/// Opens a Feed post's glance larger (src/screens/feed_glance.rs), as a chat's: the same viewer, with the post's Copy,
/// Ask and choices.
pub(crate) fn open_feed_sheet(
    glance: GlanceRef,
    data: Rc<GlanceData>,
    from: FeedSheet,
    window: &mut Window,
    cx: &mut App,
) {
    let sheet = cx.new(|cx| GlanceSheet::new(glance, data, SheetFrom::Feed(from), false, window, cx));
    present_sheet(sheet, window, cx);
}

/// What the card's text view and Copy say: the glance's text, or the message's after its title line.
fn glance_text(message: &Message, data: Option<&GlanceData>) -> String {
    match data {
        Some(data) => data.text.clone(),
        None => message.content.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_default(),
    }
}

/// The head's icon: the one Filo picked, if the app has it.
pub(crate) fn icon_for(icon: Option<&str>) -> &'static str {
    const ICONS: [&str; 8] = [
        "calendar-clock",
        "layout-grid",
        "trending-up",
        "chart-line",
        "grid-3x3",
        "chart-pie",
        "chart-column-stacked",
        "chart-gantt",
    ];
    icon.and_then(|icon| ICONS.iter().find(|known| **known == icon).copied()).unwrap_or("trending-up")
}

pub(crate) fn skeleton_bars(colors: &Palette) -> gpui::Div {
    div().w_full().flex().flex_col().gap(px(10.)).py(px(space::XS)).children(
        [0.38, 0.92, 0.76, 0.84, 0.6]
            .map(|width| div().w(relative(width)).h(px(10.)).rounded(px(5.)).bg(colors.surface_strong)),
    )
}

pub(crate) fn text_block(text: &str, colors: &Palette) -> gpui::Div {
    div()
        .w_full()
        .px(px(2.))
        .py(px(space::XS))
        .flex()
        .flex_col()
        .gap(px(2.))
        .subhead()
        .text_color(colors.text)
        .children(text.lines().filter(|line| !line.trim().is_empty()).map(|line| div().child(line.trim().to_string())))
}

// --- Open larger ---

/// How long the larger view waits for its page to say how tall it is before opening anyway.
const MEASURE_WAIT: std::time::Duration = std::time::Duration::from_millis(400);
/// How long the dialog takes to open (gpui-component's dialog.rs animation): the page shows after it.
const OPENING: std::time::Duration = std::time::Duration::from_millis(260);

thread_local! {
    /// The larger page's height, by glance and width, as it last measured itself: a later opening is sized at once.
    static LARGE_HEIGHTS: RefCell<HashMap<(String, u32), f32>> = RefCell::new(HashMap::new());
}

/// The viewer's card for a window: its width, and the room inside it for the page (width, most height).
fn sheet_size(window: &Window) -> (f32, (f32, f32)) {
    use crate::media_viewer::{BAR, EDGE, TOP};
    let viewport = window.viewport_size();
    let room = ((f32::from(viewport.width) - 2. * EDGE).max(1.), (f32::from(viewport.height) - TOP - BAR).max(1.));
    let width = room.0.min(SHEET_WIDTH);
    (width, ((width - 2. * SHEET_PAD).max(1.), (room.1 - 2. * SHEET_PAD).max(1.)))
}

/// Shows `sheet` over the window, as the media viewer shows a picture: the whole window, the sheet drawing its own
/// backdrop. Its page shows once the dialog has opened (`OPENING`).
fn show_sheet(sheet: Entity<GlanceSheet>, window: &mut Window, cx: &mut App) {
    sheet.update(cx, |sheet, cx| {
        sheet.opened = Some(Instant::now());
        sheet._opening = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(OPENING).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        }));
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .w(window.viewport_size().width)
            .margin_top(px(-crate::media_viewer::DIALOG_SLIDE))
            .close_button(false)
            .p_0()
            .border_0()
            .rounded(px(0.))
            .bg(gpui::transparent_black())
            .child(sheet.clone())
    });
}

/// The card the page sits on, at most this wide: the page's own width opened larger (about 680 px, glances_mcp.rs)
/// and the card's padding either side.
const SHEET_WIDTH: f32 = 680. + 2. * SHEET_PAD;
const SHEET_PAD: f32 = space::XXL;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Visual,
    Table,
    Text,
}

/// Where the label of the control under the pointer shows: beside the close button, or in the bar. The page draws over
/// GPUI's tooltips, which open by the pointer, so the viewer shows its own where nothing covers them.
#[derive(Clone, PartialEq)]
enum Tip {
    Close,
    Bar(SharedString),
}

/// Where the larger view was opened from: a glance's message in a chat, or a post in the Feed.
enum SheetFrom {
    Chat { message: Message, conversation: WeakEntity<Conversation> },
    Feed(FeedSheet),
}

/// What the larger view does for a Feed post's glance (src/screens/feed_glance.rs).
pub(crate) struct FeedSheet {
    /// What Copy puts on the clipboard.
    pub copy: String,
    /// The line under the title: who posted it, and when.
    pub from: String,
    /// Put in the page's head with the theme: the choices' runtime.
    pub head: String,
    pub ask: Rc<dyn Fn(&mut Window, &mut App)>,
    /// A choice picked in the page.
    pub choose: Rc<dyn Fn(String, &mut Window, &mut App)>,
}

pub struct GlanceSheet {
    glance: GlanceRef,
    data: Rc<GlanceData>,
    from: SheetFrom,
    /// The glance fixture's sheet: Save to Artifacts only says so.
    #[cfg(feature = "dev")]
    fixture: bool,
    tab: Tab,
    page: Option<Rc<NativePage>>,
    height: f32,
    saving: bool,
    tip: Option<Tip>,
    /// Copied as text: the Copy button shows a tick until the timer ends.
    copied: Option<GpuiTask<()>>,
    focus: gpui::FocusHandle,
    focused: bool,
    /// Told when the page first says how tall it is (`open_glance` waits on it); none once it has, or with no page.
    measured: Option<futures::channel::oneshot::Receiver<()>>,
    measured_tx: Option<futures::channel::oneshot::Sender<()>>,
    /// When the dialog opened: the page shows once it has (`OPENING`).
    opened: Option<Instant>,
    _opening: Option<GpuiTask<()>>,
    _ipc: Option<GpuiTask<()>>,
}

impl GlanceSheet {
    #[allow(clippy::too_many_arguments)]
    fn new(
        glance: GlanceRef,
        data: Rc<GlanceData>,
        from: SheetFrom,
        fixture: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        #[cfg(not(feature = "dev"))]
        let _ = fixture;
        let (_, inner) = sheet_size(window);
        let key = (glance.glance_id.clone(), inner.0.round() as u32);
        let known = LARGE_HEIGHTS.with(|heights| heights.borrow().get(&key).copied());
        // Until it says: as tall as it last was here, else as in the chat.
        let height = known.or_else(|| known_height(&glance.glance_id)).unwrap_or(DEFAULT_HEIGHT);
        let mut sheet = GlanceSheet {
            glance,
            data,
            from,
            #[cfg(feature = "dev")]
            fixture,
            tab: Tab::Visual,
            page: None,
            height,
            saving: false,
            tip: None,
            copied: None,
            focus: cx.focus_handle(),
            focused: false,
            measured: None,
            measured_tx: None,
            opened: None,
            _opening: None,
            _ipc: None,
        };
        // Made now, at the card's width, so it has measured itself by the time the viewer opens.
        if let Some(page) = sheet.page(window, cx) {
            page.preset(inner.0, height.min(inner.1));
            if known.is_none() {
                let (tx, rx) = futures::channel::oneshot::channel();
                sheet.measured = Some(rx);
                sheet.measured_tx = Some(tx);
            }
        }
        sheet
    }

    fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Rc<NativePage>> {
        let theme = ThemeCss::new(cx);
        if self.page.as_ref().is_some_and(|page| page.theme == theme.key) {
            return self.page.clone();
        }
        self.page = None;
        if !window.is_window_active() {
            return None;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PageMessage>();
        let head = match &self.from {
            SheetFrom::Feed(feed) => feed.head.as_str(),
            SheetFrom::Chat { .. } => "",
        };
        let page = NativePage::open_with(window, &self.data.document, &theme, true, head, tx)?;
        let weak = Rc::downgrade(&page);
        let handle = window.window_handle();
        self._ipc = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(message) = rx.next().await {
                let height = match message {
                    PageMessage::Size { height, .. } => height,
                    PageMessage::Painted => {
                        if let Some(page) = weak.upgrade()
                            && !page.ready.replace(true)
                            && this.update(cx, |_, cx| cx.notify()).is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    PageMessage::Escape => {
                        page_escape(handle, cx);
                        continue;
                    }
                    PageMessage::Choice(choice) => {
                        let picked = this.update_in(cx, |this, window, cx| {
                            if let SheetFrom::Feed(feed) = &this.from {
                                let choose = feed.choose.clone();
                                choose(choice, window, cx);
                            }
                        });
                        if picked.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                if this
                    .update_in(cx, |this, window, cx| {
                        this.height = height.clamp(MIN_HEIGHT, 2_000.);
                        let (_, inner) = sheet_size(window);
                        LARGE_HEIGHTS.with(|heights| {
                            heights
                                .borrow_mut()
                                .insert((this.glance.glance_id.clone(), inner.0.round() as u32), this.height)
                        });
                        if let Some(measured) = this.measured_tx.take() {
                            let _ = measured.send(());
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        self.page = Some(page.clone());
        Some(page)
    }

    /// The page's height the card is sized for, once the viewer has opened (the fixture's checks).
    #[cfg(feature = "dev")]
    pub fn opened_height(&self) -> Option<f32> {
        self.opened.map(|_| self.height)
    }

    /// The larger page: whether it has said how tall it is, and what of it shows (the fixture's checks).
    #[cfg(feature = "dev")]
    pub fn probe(&self, window: &Window) -> Option<(bool, Option<Bounds<Pixels>>)> {
        self.page.as_ref().map(|page| (page.ready.get(), page.on_screen(window)))
    }

    #[cfg(feature = "dev")]
    pub fn show_tab(&mut self, table: bool, cx: &mut Context<Self>) {
        self.tab = if table { Tab::Table } else { Tab::Visual };
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = None;
        window.close_dialog(cx);
    }

    fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
        match &self.from {
            SheetFrom::Chat { message, conversation } => {
                let message = message.clone();
                conversation.update(cx, |conversation, cx| conversation.ask_about_glance(&message, window, cx)).ok();
            }
            SheetFrom::Feed(feed) => {
                let ask = feed.ask.clone();
                ask(window, cx);
            }
        }
    }

    /// The glance in words on the clipboard; the button shows a tick and the bar says so for a moment.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = match &self.from {
            SheetFrom::Chat { message, .. } => message.content.clone(),
            SheetFrom::Feed(feed) => feed.copy.clone(),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::media_viewer::COPIED_FOR).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn set_tip(&mut self, tip: Option<Tip>, hovered: bool, cx: &mut Context<Self>) {
        let tip = if hovered {
            tip
        } else if self.tip == tip {
            None
        } else {
            return;
        };
        if self.tip != tip {
            self.tip = tip;
            cx.notify();
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let title = self.data.title.clone();
        #[cfg(feature = "dev")]
        if self.fixture {
            ui::toast(window, cx, format!("Saved “{title}” to Artifacts"));
            return;
        }
        self.saving = true;
        cx.notify();
        let args = json!({ "glanceId": self.glance.glance_id });
        backend::call_in(
            window,
            cx,
            "glances:saveToArtifacts",
            args,
            move |this: &mut Self, result: Result<Value, String>, window, cx| {
                this.saving = false;
                cx.notify();
                match result {
                    Ok(saved) if saved.get("created").and_then(Value::as_bool) == Some(false) => {
                        ui::toast(window, cx, format!("“{title}” is in Artifacts already"))
                    }
                    Ok(_) => ui::toast(window, cx, format!("Saved “{title}” to Artifacts")),
                    Err(error) => ui::toast_error(window, cx, "Couldn’t save to Artifacts", error),
                }
            },
        );
    }

    fn render_table(&self, colors: &Palette) -> gpui::Stateful<gpui::Div> {
        let Some(table) = &self.data.table else { return div().id("glance-table") };
        let columns = table.columns.len().max(1);
        let cell = |text: &str, head: bool, first: bool| {
            div()
                .flex_1()
                .min_w_0()
                .px(px(space::MD))
                .py(px(if head { 6. } else { 9. }))
                .truncate()
                .map(|this| {
                    if head {
                        this.caption().medium().text_color(colors.text_tertiary)
                    } else {
                        this.subhead().text_color(colors.text)
                    }
                })
                .when(first && !head, |this| this.text_color(colors.text_secondary))
                .child(text.to_string())
        };
        div()
            .id("glance-table")
            .w_full()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .children(table.columns.iter().enumerate().map(|(index, column)| cell(column, true, index == 0))),
            )
            .children(table.rows.iter().map(|row| {
                div().flex().border_t_1().border_color(colors.border.opacity(0.45)).children(
                    (0..columns).map(|index| cell(row.get(index).map(String::as_str).unwrap_or(""), false, index == 0)),
                )
            }))
    }

    /// The bar along the bottom, as the media viewer's: the glance's icon, title and where it's from, then the views
    /// and the actions, with the label of the one under the pointer.
    fn render_bar(&self, icon: &'static str, cx: &mut Context<Self>) -> AnyElement {
        use crate::media_viewer::BAR;
        let colors = *cx.colors();
        let data = &self.data;
        let from = match (&self.from, data.source.as_deref()) {
            (SheetFrom::Chat { message, .. }, Some(source)) => {
                format!("From Filo’s reply at {} · {source}", format::clock(message.creation_time))
            }
            (SheetFrom::Chat { message, .. }, None) => {
                format!("From Filo’s reply at {}", format::clock(message.creation_time))
            }
            (SheetFrom::Feed(feed), Some(source)) => format!("{} · {source}", feed.from),
            (SheetFrom::Feed(feed), None) => feed.from.clone(),
        };
        let button = |id: &'static str, icon: &'static str, label: SharedString, cx: &mut Context<Self>| {
            let tip = Tip::Bar(label);
            div()
                .id(SharedString::from(format!("{id}-hover")))
                .rounded(px(radius::FULL))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.set_tip(Some(tip.clone()), *hovered, cx)))
                .child(IconButton::new(id, icon).color(colors.on_media))
        };
        let mut views =
            vec![(Tab::Visual, icon, SharedString::from(data.visual_label.clone().unwrap_or_else(|| "Chart".into())))];
        if data.table.as_ref().is_some_and(|table| !table.rows.is_empty()) {
            views.push((Tab::Table, "table", "Table".into()));
        }
        views.push((Tab::Text, "list", "As text".into()));
        let tab = self.tab;
        let label = match (&self.tip, self.copied.is_some()) {
            (_, true) => Some(SharedString::from("Copied as text")),
            (Some(Tip::Bar(label)), _) => Some(label.clone()),
            _ => None,
        };
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(space::XS))
            // Clicks on the buttons aren't ones on the backdrop.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(
                label.map(|label| {
                    div().mr(px(space::SM)).footnote().text_color(colors.on_media.opacity(0.8)).child(label)
                }),
            )
            .children(views.into_iter().map(|(view, icon, label)| {
                let id = match view {
                    Tab::Visual => "glance-view-visual",
                    Tab::Table => "glance-view-table",
                    Tab::Text => "glance-view-text",
                };
                button(id, icon, label, cx).when(view == tab, |this| this.bg(colors.on_media.opacity(0.16))).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.tab = view;
                        cx.notify();
                    }),
                )
            }))
            .child(div().w(px(space::MD)))
            .child(
                button("glance-copy", if self.copied.is_some() { "check" } else { "copy" }, "Copy as text".into(), cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy(cx))),
            )
            .child(
                button("glance-ask", "message-circle", "Ask about this".into(), cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.ask(window, cx))),
            )
            .child({
                let saving = self.saving;
                div()
                    .id("glance-save-hover")
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        this.set_tip(Some(Tip::Bar("Save to Artifacts".into())), *hovered, cx)
                    }))
                    .child(
                        IconButton::new("glance-save", "tab-artifacts")
                            .color(colors.on_media)
                            .spinning(saving)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx))),
                    )
            });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(BAR))
            .px(px(space::XL))
            .flex()
            .items_center()
            .gap(px(space::LG))
            .bg(gpui::linear_gradient(
                180.,
                gpui::linear_color_stop(colors.media.opacity(0.), 0.),
                gpui::linear_color_stop(colors.media.opacity(0.75), 0.6),
            ))
            .child(ui::icon_sized(icon, 18., colors.on_media.opacity(0.8)).flex_none())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    // One line each, ending in "…": a clamped line truncates at its laid-out width, where `truncate`
                    // (no wrapping) keeps its first measure and cut the line off bare.
                    .child(
                        div()
                            .callout()
                            .semibold()
                            .text_color(colors.on_media)
                            .line_clamp(1)
                            .text_ellipsis()
                            .child(data.title.clone()),
                    )
                    .child(
                        div()
                            .footnote()
                            .text_color(colors.on_media.opacity(0.6))
                            .line_clamp(1)
                            .text_ellipsis()
                            .child(from),
                    ),
            )
            .child(buttons)
            .into_any_element()
    }
}

impl Render for GlanceSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::media_viewer::{BAR, EDGE, TOP};
        // Esc reaches the dialog around it from here.
        if !self.focused {
            self.focused = true;
            self.focus.focus(window);
        }
        let colors = *cx.colors();
        let data = self.data.clone();
        let icon = icon_for(self.glance.icon.as_deref().or(data.icon.as_deref()));
        // The room between the top band and the bar, as a picture's; the card is as wide as the page wants, and as tall,
        // up to the room (the page scrolls inside beyond that).
        let viewport = window.viewport_size();
        let area = (f32::from(viewport.width), f32::from(viewport.height));
        let room = ((area.0 - 2. * EDGE).max(1.), (area.1 - TOP - BAR).max(1.));
        let (width, inner) = sheet_size(window);
        let body: AnyElement = match self.tab {
            Tab::Visual => {
                let page = self.page(window, cx);
                let ready = page.as_ref().is_some_and(|page| page.ready.get());
                // Held back while the dialog opens, then faded in; a spinner, at the card's final size, only if it's
                // still coming then.
                let opened = self.opened.is_some_and(|opened| opened.elapsed() >= OPENING);
                // A size of its own, not one from the card: the page's box has to be as wide as the card's inside.
                div()
                    .relative()
                    .flex_none()
                    .w(px(inner.0))
                    .h(px(self.height.min(inner.1)))
                    .when(opened && !ready, |this| {
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(ui::spinner(28., cx)),
                        )
                    })
                    .children(page.map(|page| {
                        div().absolute().inset_0().child(PageElement { page, clip_bottom: None, show: opened })
                    }))
                    .into_any_element()
            }
            Tab::Table => self.render_table(&colors).into_any_element(),
            Tab::Text => div()
                .id("glance-text")
                .w_full()
                .min_h_0()
                .overflow_y_scroll()
                .child(text_block(&data.text, &colors))
                .into_any_element(),
        };
        let card = div()
            .id("glance-sheet-card")
            .w(px(width))
            .max_h(px(room.1))
            .p(px(SHEET_PAD))
            .flex()
            .flex_col()
            .rounded(px(radius::XL))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            // A click on the card isn't one on the backdrop, which closes the viewer.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(body);
        let close_tip = self.tip == Some(Tip::Close);
        div()
            .id("glance-sheet")
            .track_focus(&self.focus)
            .relative()
            .w(px(area.0))
            .h(px(area.1))
            .overflow_hidden()
            // Opaque, unlike the media viewer's: the chat's text read through it behind the glance's own.
            .bg(colors.media)
            // A click on the backdrop closes it, as the media viewer does.
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .absolute()
                    .left(px(EDGE))
                    .top(px(TOP))
                    .w(px(room.0))
                    .h(px(room.1))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(card),
            )
            .child(self.render_bar(icon, cx))
            .child(
                div()
                    .absolute()
                    .top(px(space::MD))
                    .right(px(space::MD))
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(close_tip, |this| {
                        this.child(div().footnote().text_color(colors.on_media.opacity(0.8)).child("Close (Esc)"))
                    })
                    .child(
                        div()
                            .id("glance-sheet-close-hover")
                            .rounded(px(radius::FULL))
                            .bg(colors.media.opacity(0.5))
                            .on_hover(
                                cx.listener(|this, hovered: &bool, _, cx| this.set_tip(Some(Tip::Close), *hovered, cx)),
                            )
                            .child(
                                IconButton::new("glance-sheet-close", "x")
                                    .color(colors.on_media)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx))),
                            ),
                    ),
            )
    }
}

// --- The page ---

/// The app's theme and font as CSS for a glance or artifact page, and a key that changes with them.
pub(crate) struct ThemeCss {
    pub(crate) key: String,
    css: String,
}

impl ThemeCss {
    pub(crate) fn new(cx: &App) -> Self {
        Self::build(cx, true)
    }

    /// Native artifact previews serve the bundled fonts directly, avoiding base64 in each HTML load.
    pub(crate) fn artifact(cx: &App) -> Self {
        Self::build(cx, false)
    }

    fn build(cx: &App, inline_fonts: bool) -> Self {
        let colors = cx.colors();
        let dark = colors.background.l < 0.5;
        let hex = |color: gpui::Hsla| {
            let rgba = gpui::Rgba::from(color);
            let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
            format!("#{:02x}{:02x}{:02x}", channel(rgba.r), channel(rgba.g), channel(rgba.b))
        };
        let roles = [
            ("background", colors.background),
            ("surface", colors.surface),
            ("surfaceStrong", colors.surface_strong),
            ("raised", colors.raised),
            ("sunken", colors.sunken),
            ("border", colors.border),
            ("text", colors.text),
            ("textSecondary", colors.text_secondary),
            ("textTertiary", colors.text_tertiary),
            ("textDisabled", colors.text_disabled),
            ("accent", colors.accent),
            ("onAccent", colors.on_accent),
            ("accentSoft", colors.accent_soft),
            ("onAccentSoft", colors.on_accent_soft),
            ("success", colors.success),
            ("successSoft", colors.success_soft),
            ("onSuccessSoft", colors.on_success_soft),
            ("danger", colors.danger),
            ("warning", colors.warning),
            ("info", colors.info),
        ];
        // Series colours: the mockup's, for light and dark.
        let series = if dark {
            ["#3987e5", "#d95926", "#199e70", "#c98500"]
        } else {
            ["#2a78d6", "#eb6834", "#1baf7a", "#eda100"]
        };
        let scale = crate::theme::text_scale();
        let sizes = [
            ("title2", 18.),
            ("title3", 16.),
            ("headline", 14.),
            ("body", 14.),
            ("callout", 14.),
            ("subhead", 13.),
            ("footnote", 12.),
            ("caption", 11.),
        ];
        let mut vars: Vec<String> = roles.iter().map(|(role, color)| format!("--{role}:{}", hex(*color))).collect();
        vars.extend(series.iter().enumerate().map(|(index, color)| format!("--c{}:{color}", index + 1)));
        vars.extend(sizes.iter().map(|(name, size)| format!("--{name}:{:.1}px", size * scale)));
        let family = cx.theme().prefs.font_family.clone();
        let font = if family == crate::config::DEFAULT_FONT {
            "Atkinson".to_string()
        } else {
            format!("\"{}\"", family.replace(['"', '<', '>', '\\'], ""))
        };
        let key = format!(
            ":root{{{};color-scheme:{}}}html,body{{font-family:{font},system-ui,-apple-system,sans-serif}}",
            vars.join(";"),
            if dark { "dark" } else { "light" }
        );
        let css = if family == crate::config::DEFAULT_FONT {
            let faces = if inline_fonts {
                font_faces().to_string()
            } else {
                ["400", "500", "600"].iter().map(|weight| format!("@font-face{{font-family:Atkinson;font-weight:{weight};src:url(filo-font://atkinson/{weight}.ttf) format('truetype')}}")).collect()
            };
            format!("{faces}{key}")
        } else {
            key.clone()
        };
        ThemeCss { key, css }
    }
}

/// The app's own font for the pages (they can't load one from a file): Atkinson Hyperlegible Next, regular and semibold.
fn font_faces() -> &'static str {
    static FACES: OnceLock<String> = OnceLock::new();
    FACES.get_or_init(|| {
        [("400", "fonts/AtkinsonHyperlegibleNext_400Regular.ttf"), ("500", "fonts/AtkinsonHyperlegibleNext_500Medium.ttf"), ("600", "fonts/AtkinsonHyperlegibleNext_600SemiBold.ttf")]
            .iter()
            .filter_map(|(weight, path)| {
                let file = crate::assets::Embedded::get(path)?;
                let data = base64::engine::general_purpose::STANDARD.encode(file.data.as_ref());
                Some(format!("@font-face{{font-family:Atkinson;font-weight:{weight};src:url(data:font/ttf;base64,{data}) format('truetype')}}"))
            })
            .collect()
    })
}

/// Reports the glance's height to the host as it lays out, and its content's natural width (the card is sized from
/// it): the narrowest it can be with nothing cut off, its min-content width or more where something still sticks out,
/// so text and chips wrap while a table or a fixed width doesn't; measured on a copy out of sight, so the page itself
/// never changes. It scrolls sideways when
/// the card says (its content wider than the card: the page never takes the wheel itself, so the chat scrolls over
/// it), and keeps the page's own menu shut.
const EMBED_JS: &str = r#"(()=>{let last='';
const natural=()=>{const g=document.getElementById('filo-glance');if(!g)return 0;const box=document.createElement('div');box.style.cssText='position:absolute;left:0;top:0;width:0;height:0;overflow:hidden;visibility:hidden;pointer-events:none';const c=g.cloneNode(true);c.style.width='min-content';box.appendChild(c);document.body.appendChild(box);const w=Math.max(c.getBoundingClientRect().width,c.scrollWidth);box.remove();return Math.ceil(w)};
const measure=()=>{let h=0;for(const el of document.body.children){if(el.tagName==='SCRIPT'||el.classList.contains('ctip'))continue;const r=el.getBoundingClientRect();h=Math.max(h,r.bottom+window.scrollY+parseFloat(getComputedStyle(el).marginBottom||'0'))}
h=Math.ceil(h);const w=natural();if(h+','+w!==last){last=h+','+w;parent.postMessage({filo:'height',h,w},'*')}};
const watch=()=>{measure();const ro=new ResizeObserver(measure);ro.observe(document.body);for(const el of document.body.children)ro.observe(el)};
if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',watch);else watch();
addEventListener('load',measure);if(document.fonts)document.fonts.ready.then(measure);
addEventListener('message',e=>{if(e.source!==parent||!e.data||e.data.filo!=='scroll')return;const s=document.scrollingElement||document.documentElement;s.scrollLeft+=Number(e.data.dx)||0});
document.addEventListener('contextmenu',e=>e.preventDefault());
addEventListener('keydown',e=>{if(e.key==='Escape'&&!e.defaultPrevented&&!e.metaKey&&!e.ctrlKey&&!e.altKey&&!e.shiftKey)parent.postMessage({filo:'escape'},'*')})})();"#;

/// `inject` put in at the page's `<!--filo:head-->` marker (or before `</head>`, or at the top).
fn put_in_head(document: &str, inject: &str) -> String {
    match document.find("<!--filo:head-->").or_else(|| document.find("</head>")) {
        Some(at) => format!("{}{inject}{}", &document[..at], &document[at..]),
        None => format!("{inject}{document}"),
    }
}

/// An artifact page with the app's theme and font put in at its `<!--filo:head-->` marker. Pages without the marker
/// (React builds, old specs drawn as static HTML) carry their own look and are left as they are.
pub(crate) fn themed_page(document: &str, theme: &ThemeCss) -> String {
    if document.contains("<!--filo:head-->") {
        put_in_head(document, &format!("<style>{}</style>", theme.css))
    } else {
        document.to_string()
    }
}

/// The glance page with the app's theme and font put in at its `<!--filo:head-->` marker, and `extra` after them.
fn themed_document(document: &str, theme: &ThemeCss, large: bool, extra: &str) -> String {
    let mut page = put_in_head(document, &format!("<style>{}</style><script>{EMBED_JS}</script>{extra}", theme.css));
    if large {
        page = page.replacen("<html lang=\"en\"", "<html lang=\"en\" data-size=\"large\"", 1);
    }
    page
}

/// The host: a page with no network that frames the glance in a sandbox (scripts, no origin) and passes its height on.
fn host_page(document: &str, theme: &ThemeCss, large: bool, extra: &str) -> String {
    let page = themed_document(document, theme, large, extra);
    // As a script string: JSON, with every "<" escaped, so nothing in it can close the host's script.
    let source = serde_json::to_string(&page).unwrap_or_else(|_| "\"\"".into()).replace('<', "\\u003c");
    let scheme = if theme.key.contains("color-scheme:dark") { "dark" } else { "light" };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; media-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'">
<style>:root{{color-scheme:{scheme}}}html,body{{margin:0;height:100%;overflow:hidden;background:transparent}}iframe{{display:block;border:0;width:100%;height:100%;background:transparent}}</style></head>
<body><iframe id="g" sandbox="allow-scripts" title="Glance"></iframe><script>(()=>{{const f=document.getElementById('g');let drawn=false;
const esc=()=>window.ipc.postMessage(JSON.stringify({{escape:true}}));
addEventListener('keydown',e=>{{if(e.key==='Escape'&&!e.defaultPrevented&&!e.metaKey&&!e.ctrlKey&&!e.altKey&&!e.shiftKey)esc()}});
addEventListener('message',e=>{{if(e.source!==f.contentWindow||!e.data)return;if(e.data.filo==='escape')return esc();if(e.data.filo==='choice')return window.ipc.postMessage(JSON.stringify({{choice:String(e.data.value??'').slice(0,200)}}));if(e.data.filo!=='height')return;const h=Math.max(0,Math.min(4000,Number(e.data.h)||0)),w=Math.max(0,Math.min(4000,Number(e.data.w)||0));window.ipc.postMessage(JSON.stringify({{height:h,width:w}}));if(!drawn){{drawn=true;requestAnimationFrame(()=>requestAnimationFrame(()=>window.ipc.postMessage(JSON.stringify({{painted:true}}))))}}}});
window.__filoScroll=dx=>{{if(f.contentWindow)f.contentWindow.postMessage({{filo:'scroll',dx}},'*')}};
f.srcdoc={source};document.addEventListener('contextmenu',e=>e.preventDefault())}})();</script></body></html>"#
    )
}

thread_local! {
    /// Every page alive, so the sweep at the end of a frame can hide the ones that frame didn't lay out.
    static PAGES: RefCell<Vec<Weak<NativePage>>> = const { RefCell::new(Vec::new()) };
}

/// One glance page in a web view, in a clip view of its own over the window.
pub struct NativePage {
    /// The page's theme (`ThemeCss::key`): a new theme makes a new page.
    pub(crate) theme: String,
    /// The page has drawn since it said how tall it is (`PageMessage::Painted`): until then its view stays parked and
    /// the card shows its placeholder.
    pub(crate) ready: Cell<bool>,
    /// Laid out this frame (`PageElement`); the sweep hides the page otherwise.
    placed: Cell<bool>,
    /// In a chat (under dialogs) rather than in the larger view (in one).
    inline: bool,
    window: u64,
    native: native::NativePage,
}

impl NativePage {
    fn open(
        window: &Window,
        document: &str,
        theme: &ThemeCss,
        large: bool,
        heights: futures::channel::mpsc::UnboundedSender<PageMessage>,
    ) -> Option<Rc<Self>> {
        Self::open_with(window, document, theme, large, "", heights)
    }

    /// A page with `extra` put in its head after the theme (the Feed's choices runtime).
    pub(crate) fn open_with(
        window: &Window,
        document: &str,
        theme: &ThemeCss,
        large: bool,
        extra: &str,
        heights: futures::channel::mpsc::UnboundedSender<PageMessage>,
    ) -> Option<Rc<Self>> {
        {
            let native = native::NativePage::open(window, host_page(document, theme, large, extra), !large, heights)?;
            let page = Rc::new(NativePage {
                theme: theme.key.clone(),
                ready: Cell::new(false),
                placed: Cell::new(false),
                inline: !large,
                window: window.window_handle().window_id().as_u64(),
                native,
            });
            PAGES.with(|pages| pages.borrow_mut().push(Rc::downgrade(&page)));
            Some(page)
        }
    }

    /// Lays the page out `width` by `height` points before it's placed anywhere, so it measures itself at that size.
    fn preset(&self, width: f32, height: f32) {
        self.native.preset(width, height);
    }

    /// Scrolls its content sideways by `dx` points (right is positive), when it's wider than the page.
    pub(crate) fn scroll_sideways(&self, dx: f32) {
        self.native.scroll_sideways(dx);
    }

    /// How many holes are cut in it for things floating over it (the fixture's checks).
    #[cfg(feature = "dev")]
    fn holes(&self) -> usize {
        self.native.holes()
    }

    /// Where its clip view is and whether the page in it shows, as the system has it; None while hidden.
    #[cfg(feature = "dev")]
    pub(crate) fn on_screen(&self, window: &Window) -> Option<Bounds<Pixels>> {
        self.native.on_screen(window)
    }
}

/// Places a page over its box each frame it's laid out, cut to the clip it's drawn in (the chat's list, above the
/// composer).
pub(crate) struct PageElement {
    pub(crate) page: Rc<NativePage>,
    pub(crate) clip_bottom: Option<Pixels>,
    /// The page may show once it's ready (the larger view holds it back while it opens).
    pub(crate) show: bool,
}

impl IntoElement for PageElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PageElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<Bounds<Pixels>>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style { size: gpui::Size::full(), ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let mut clip = window.content_mask().bounds;
        if let Some(bottom) = self.clip_bottom {
            clip.size.height = (bottom - clip.origin.y).min(clip.size.height).max(px(0.));
        }
        let visible = bounds.intersect(&clip);
        self.page.placed.set(true);
        let shows = visible.size.width > px(0.) && visible.size.height > px(0.);
        {
            let ready = self.page.ready.get() && self.show;
            if shows {
                self.page.native.place(bounds, visible, ready, window);
            } else {
                self.page.native.hide();
            }
        }
        shows.then_some(visible)
    }

    /// What GPUI paints after the page, over it (a pill or card over the chat, a menu, tooltip, notification, anything:
    /// gpui's `Window::painted_over`), shows through holes cut in the page once the frame is drawn, and takes its clicks
    /// there.
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        visible: &mut Option<Bounds<Pixels>>,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(visible) = *visible else { return };
        let index = window.scene_index();
        let page = self.page.clone();
        window.on_next_frame(move |window, _| {
            let (tints, painted): (Vec<_>, Vec<_>) =
                window.painted_over(index, visible).into_iter().partition(|painted| painted.laid());
            let holes: Vec<Hole> = painted
                .into_iter()
                .map(|painted| Hole { bounds: painted.bounds, radius: Some(f32::from(painted.radius)) })
                .collect();
            page.native.set_holes(&holes, &tints);
        });
    }
}

/// The holes cut in the glance pages that show, window points (the test windows' checks).
#[cfg(feature = "dev")]
pub(crate) fn smoke_window_holes() -> Vec<Bounds<Pixels>> {
    native::window_holes()
}

/// Where something is drawn over a page, window points, and its corners' radius (None: a pill's, half its height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hole {
    bounds: Bounds<Pixels>,
    radius: Option<f32>,
}

/// The last thing a window lays out: hides the glance pages that frame didn't lay out (scrolled away, another page
/// open), and the chat's while a dialog is over it.
pub struct Sweep;

pub fn sweep() -> Sweep {
    Sweep
}

impl IntoElement for Sweep {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Sweep {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style { position: gpui::Position::Absolute, ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let window_id = window.window_handle().window_id().as_u64();
        let pages: Vec<Rc<NativePage>> = PAGES.with(|pages| {
            let mut pages = pages.borrow_mut();
            pages.retain(|page| page.strong_count() > 0);
            pages.iter().filter_map(Weak::upgrade).filter(|page| page.window == window_id).collect()
        });
        let dialog = window.has_active_dialog(cx);
        for page in pages {
            if !page.placed.replace(false) || (page.inline && dialog) {
                page.native.hide();
            }
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        // A click GPUI hears (not one in a page) takes the keyboard back from a page that had it.
        window.on_mouse_event(|_: &gpui::MouseDownEvent, phase, window, _| {
            if phase == gpui::DispatchPhase::Capture {
                native::reclaim_keys(window);
            }
        });
    }
}

mod native {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::OnceLock;

    use gpui::{Bounds, Pixels, Window};
    use gpui_component::wry;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2::sel;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::{AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};

    /// NSEventTypeScrollWheel.
    /// kCGEventScrollWheel.
    const SCROLL_WHEEL: u32 = 22;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventGetType(event: *const c_void) -> u32;
        fn CGPathCreateMutable() -> *mut c_void;
        fn CGPathAddRect(path: *mut c_void, transform: *const c_void, rect: NSRect);
        fn CGPathAddRoundedRect(
            path: *mut c_void,
            transform: *const c_void,
            rect: NSRect,
            corner_width: f64,
            corner_height: f64,
        );
        fn CGPathRelease(path: *mut c_void);
    }

    pub struct NativePage {
        /// The clip view: cut to what of the card shows, drawing nothing of its own.
        container: Retained<AnyObject>,
        web: wry::WebView,
        frame: Cell<Option<(f64, f64, f64, f64)>>,
        inner: Cell<Option<(f64, f64, f64, f64)>>,
        /// The clip view shows (opaque); otherwise it's parked (`park`).
        shown: Cell<bool>,
        /// The larger view's page has faded in.
        faded: Cell<bool>,
        /// What of the card the clip view shows, window points (`set_holes` cuts in it).
        visible: Cell<Option<Bounds<Pixels>>>,
        /// The holes cut last, in the clip view's points, and its size (none: no mask).
        holes: std::cell::RefCell<Vec<[f64; 4]>>,
        /// In a chat rather than in the larger view.
        inline: bool,
    }

    /// How long the larger view's page takes to fade in.
    const FADE_SECONDS: f64 = 0.18;

    /// Parks a clip view or shows it. A parked one is see-through and lets every click and the wheel through
    /// (`sheet_hit_test`), but isn't hidden: WebKit takes a hidden web view (`isHiddenOrHasHiddenAncestor`) for one out
    /// of sight and runs its page and GPU work at background priority, where a busy Mac starves them: a page made hidden
    /// took seconds to load, and WebKit could kill its GPU process as unresponsive, leaving the card's page drawn blank.
    fn park(view: &AnyObject, parked: bool) {
        // SAFETY: AppKit messages on the main thread; `view` is a live clip view.
        unsafe {
            let _: () = msg_send![view, setAlphaValue: if parked { 0.0f64 } else { 1.0f64 }];
        }
    }

    /// Whether a clip view is parked (`park`).
    fn parked(view: &AnyObject) -> bool {
        // SAFETY: as in `park`.
        let alpha: f64 = unsafe { msg_send![view, alphaValue] };
        alpha <= 0.
    }

    /// Fades `view` in from clear.
    fn fade_in(view: &AnyObject) {
        // SAFETY: AppKit messages on the main thread; `view` is a live clip view.
        unsafe {
            let _: () = msg_send![view, setAlphaValue: 0.0f64];
            let Some(context) = AnyClass::get(c"NSAnimationContext") else {
                let _: () = msg_send![view, setAlphaValue: 1.0f64];
                return;
            };
            let _: () = msg_send![context, beginGrouping];
            let current: *mut AnyObject = msg_send![context, currentContext];
            let _: () = msg_send![current, setDuration: FADE_SECONDS];
            let animator: *mut AnyObject = msg_send![view, animator];
            let _: () = msg_send![animator, setAlphaValue: 1.0f64];
            let _: () = msg_send![context, endGrouping];
        }
    }

    /// Room around a pill's hole for its shadow.
    const HOLE_MARGIN: f64 = 4.;

    thread_local! {
        /// The holes cut in each chat clip view (`set_holes`), by the view, in its points: a click there is GPUI's.
        static HIT_HOLES: std::cell::RefCell<std::collections::HashMap<usize, Vec<NSRect>>> = Default::default();
        /// The same holes in window points, by the clip view (the test windows' checks).
        #[cfg(feature = "dev")]
        static WINDOW_HOLES: std::cell::RefCell<std::collections::HashMap<usize, Vec<Bounds<Pixels>>>> = Default::default();
    }

    /// The holes cut in the glance pages that show, window points (the test windows' checks).
    #[cfg(feature = "dev")]
    pub fn window_holes() -> Vec<Bounds<Pixels>> {
        WINDOW_HOLES.with(|holes| {
            holes
                .borrow()
                .iter()
                .filter(|(container, _)| {
                    // SAFETY: on the main thread; a clip view with holes is kept by its page.
                    unsafe {
                        let view = **container as *const AnyObject;
                        let hidden: Bool = msg_send![view, isHiddenOrHasHiddenAncestor];
                        let window: *mut AnyObject = msg_send![view, window];
                        !hidden.as_bool() && !parked(&*view) && !window.is_null()
                    }
                })
                .flat_map(|(_, holes)| holes.clone())
                .collect()
        })
    }

    /// A `CGPathRef`, as AppKit's `setPath:` takes it.
    #[repr(transparent)]
    struct Path(*mut c_void);

    // SAFETY: a pointer to an opaque CGPath, as Core Animation declares the argument.
    unsafe impl objc2::Encode for Path {
        const ENCODING: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGPath", &[]));
    }

    /// A clip view as a parent for wry: its "window handle" is the view.
    struct Parent(NonNull<c_void>);

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // SAFETY: the clip view outlives this borrow (it's held by the page being made).
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AppKit(AppKitWindowHandle::new(self.0))) })
        }
    }

    extern "C-unwind" fn is_flipped(_: &AnyObject, _: Sel) -> Bool {
        Bool::YES
    }

    /// The chat's clip views let the scroll wheel through to GPUI's view under them, so the chat scrolls over a glance.
    extern "C-unwind" fn hit_test(this: &AnyObject, _: Sel, point: NSPoint) -> *mut AnyObject {
        // SAFETY: AppKit messages on the main thread, where AppKit hit-tests.
        unsafe {
            if let Some(app) = AnyClass::get(c"NSApplication") {
                let app: *mut AnyObject = msg_send![app, sharedApplication];
                let event: *mut AnyObject = msg_send![app, currentEvent];
                if !event.is_null() {
                    // Its type through CoreGraphics: `type` is a Rust keyword, and `r#type` isn't AppKit's selector.
                    let cg: *const c_void = msg_send![event, CGEvent];
                    if !cg.is_null() && CGEventGetType(cg) == SCROLL_WHEEL {
                        return std::ptr::null_mut();
                    }
                }
            }
            sheet_hit_test(this, sel!(hitTest:), point)
        }
    }

    /// What floats over a page shows through a hole in it (`set_holes`), and the click there is its own.
    extern "C-unwind" fn sheet_hit_test(this: &AnyObject, _: Sel, point: NSPoint) -> *mut AnyObject {
        if parked(this) {
            return std::ptr::null_mut();
        }
        // SAFETY: AppKit messages on the main thread, where AppKit hit-tests.
        unsafe {
            let key = this as *const AnyObject as usize;
            if HIT_HOLES.with(|hit| hit.borrow().get(&key).is_some_and(|holes| !holes.is_empty())) {
                let superview: *mut AnyObject = msg_send![this, superview];
                let local: NSPoint = msg_send![this, convertPoint: point, fromView: superview];
                let inside = |rect: &NSRect| {
                    local.x >= rect.origin.x
                        && local.x < rect.origin.x + rect.size.width
                        && local.y >= rect.origin.y
                        && local.y < rect.origin.y + rect.size.height
                };
                if HIT_HOLES.with(|hit| hit.borrow().get(&key).is_some_and(|holes| holes.iter().any(inside))) {
                    return std::ptr::null_mut();
                }
            }
            let Some(view) = AnyClass::get(c"NSView") else { return std::ptr::null_mut() };
            msg_send![super(this, view), hitTest: point]
        }
    }

    /// The clip view's class: flipped (y down, like GPUI), clicks through its holes, and for the chat's cards, see-through
    /// to the scroll wheel.
    fn clip_class(inline: bool) -> Option<&'static AnyClass> {
        static INLINE: OnceLock<Option<usize>> = OnceLock::new();
        static LARGE: OnceLock<Option<usize>> = OnceLock::new();
        let cell = if inline { &INLINE } else { &LARGE };
        let class = cell.get_or_init(|| {
            let name = if inline { c"FiloGlanceClip" } else { c"FiloGlanceSheetClip" };
            if let Some(class) = AnyClass::get(name) {
                return Some(class as *const AnyClass as usize);
            }
            let mut builder = ClassBuilder::new(name, AnyClass::get(c"NSView")?)?;
            // SAFETY: the methods' signatures match AppKit's `isFlipped` and `hitTest:`.
            unsafe {
                builder.add_method(sel!(isFlipped), is_flipped as extern "C-unwind" fn(_, _) -> _);
                if inline {
                    builder.add_method(sel!(hitTest:), hit_test as extern "C-unwind" fn(_, _, _) -> _);
                } else {
                    builder.add_method(sel!(hitTest:), sheet_hit_test as extern "C-unwind" fn(_, _, _) -> _);
                }
            }
            Some(builder.register() as *const AnyClass as usize)
        });
        // SAFETY: a registered class lives as long as the process.
        class.map(|class| unsafe { &*(class as *const AnyClass) })
    }

    /// Gives the keyboard back to GPUI's view when a glance's web view has it: one takes it when it's made, shown or
    /// clicked, and GPUI doesn't take it back on a click of its own, so the composer would stop hearing keys.
    pub fn reclaim_keys(window: &Window) {
        let Some(view) = content_view(window) else { return };
        let classes = [clip_class(true), clip_class(false)];
        // SAFETY: AppKit messages on the main thread; `view` is GPUI's view, alive with its window.
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            if ns_window.is_null() {
                return;
            }
            let responder: *mut AnyObject = msg_send![ns_window, firstResponder];
            let Some(ns_view) = AnyClass::get(c"NSView") else { return };
            if responder.is_null() || responder == view || !msg_send![responder, isKindOfClass: ns_view] {
                return;
            }
            let mut ancestor = responder;
            while !ancestor.is_null() && ancestor != view {
                if classes.iter().flatten().any(|class| msg_send![ancestor, isMemberOfClass: *class]) {
                    let _: Bool = msg_send![ns_window, makeFirstResponder: view];
                    return;
                }
                ancestor = msg_send![ancestor, superview];
            }
        }
    }

    fn content_view(window: &Window) -> Option<*mut AnyObject> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
        Some(handle.ns_view.as_ptr() as *mut AnyObject)
    }

    impl NativePage {
        pub fn open(
            window: &Window,
            html: String,
            inline: bool,
            heights: futures::channel::mpsc::UnboundedSender<super::PageMessage>,
        ) -> Option<Self> {
            let parent = content_view(window)?;
            let class = clip_class(inline)?;
            // SAFETY: AppKit messages on the main thread, where GPUI renders; `parent` is GPUI's view, alive with its
            // window.
            let container: Retained<AnyObject> = unsafe {
                let container: Option<Retained<AnyObject>> = msg_send![class, new];
                let container = container?;
                let _: () = msg_send![&*container, setWantsLayer: true];
                // The page is placed inside it by hand each frame: no resizing of its own when the clip changes.
                let _: () = msg_send![&*container, setAutoresizesSubviews: false];
                // Clips the page to it, as a view and as a layer: since macOS 14 a view doesn't clip what's in it by
                // default.
                let clips: Bool = msg_send![&*container, respondsToSelector: sel!(setClipsToBounds:)];
                if clips.as_bool() {
                    let _: () = msg_send![&*container, setClipsToBounds: true];
                }
                let layer: *mut AnyObject = msg_send![&*container, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setMasksToBounds: true];
                }
                let _: () = msg_send![parent, addSubview: &*container];
                container
            };
            park(&container, true);
            let handle = Parent(NonNull::new(Retained::as_ptr(&container) as *mut c_void)?);
            let built = wry::WebViewBuilder::new()
                .with_html(html)
                .with_transparent(true)
                // Never suspended while out of sight (a window behind others, a minimized one): back in view, it shows
                // at once rather than blank until WebKit wakes it.
                .with_background_throttling(wry::BackgroundThrottlingPolicy::Disabled)
                .with_focused(false)
                .with_incognito(true)
                .with_accept_first_mouse(true)
                // The host and its frame only: nothing navigates anywhere.
                .with_navigation_handler(|url| url == "about:blank" || url == "about:srcdoc")
                .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
                .with_ipc_handler(move |request: wry::http::Request<String>| {
                    if *request.uri() != "about:blank" {
                        return;
                    }
                    let Ok(message) = serde_json::from_str::<serde_json::Value>(request.body()) else { return };
                    if message.get("painted").and_then(serde_json::Value::as_bool) == Some(true) {
                        let _ = heights.unbounded_send(super::PageMessage::Painted);
                    } else if message.get("escape").and_then(serde_json::Value::as_bool) == Some(true) {
                        let _ = heights.unbounded_send(super::PageMessage::Escape);
                    } else if let Some(choice) = message
                        .get("choice")
                        .and_then(serde_json::Value::as_str)
                        .filter(|choice| !choice.trim().is_empty())
                    {
                        let _ = heights.unbounded_send(super::PageMessage::Choice(choice.to_string()));
                    } else if let Some(height) = message.get("height").and_then(serde_json::Value::as_f64) {
                        let width = message.get("width").and_then(serde_json::Value::as_f64).map(|width| width as f32);
                        let _ = heights.unbounded_send(super::PageMessage::Size { height: height as f32, width });
                    }
                })
                .build_as_child(&handle);
            reclaim_keys(window);
            match built {
                Ok(web) => Some(NativePage {
                    container,
                    web,
                    frame: Cell::new(None),
                    inner: Cell::new(None),
                    shown: Cell::new(false),
                    faded: Cell::new(false),
                    visible: Cell::new(None),
                    holes: std::cell::RefCell::new(Vec::new()),
                    inline,
                }),
                Err(_) => {
                    // SAFETY: as above.
                    unsafe {
                        let _: () = msg_send![&*container, removeFromSuperview];
                    }
                    None
                }
            }
        }

        /// The clip view onto `visible` and the page onto `bounds` inside it (window points from the top left).
        pub fn place(&self, bounds: Bounds<Pixels>, visible: Bounds<Pixels>, ready: bool, window: &Window) {
            let Some(parent) = content_view(window) else { return };
            self.visible.set(Some(visible));
            let f = |value: Pixels| f64::from(f32::from(value));
            let (x, y, width, height) =
                (f(visible.origin.x), f(visible.origin.y), f(visible.size.width), f(visible.size.height));
            // SAFETY: as in `open`.
            unsafe {
                let flipped: bool = msg_send![parent, isFlipped];
                let parent_bounds: NSRect = msg_send![parent, bounds];
                let y = if flipped { y } else { parent_bounds.size.height - y - height };
                let frame = (x, y, width, height);
                if self.frame.get() != Some(frame) {
                    self.frame.set(Some(frame));
                    let _: () = msg_send![&*self.container, setFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))];
                }
            }
            let inner = (
                f(bounds.origin.x - visible.origin.x),
                f(bounds.origin.y - visible.origin.y),
                f(bounds.size.width),
                f(bounds.size.height),
            );
            if self.inner.get() != Some(inner) {
                self.inner.set(Some(inner));
                let _ = self.web.set_bounds(wry::Rect {
                    position: wry::dpi::LogicalPosition::new(inner.0, inner.1).into(),
                    size: wry::dpi::LogicalSize::new(inner.2, inner.3).into(),
                });
            }
            // Parked until its page has drawn; then it shows wherever it's placed. The larger view's page fades in the
            // first time; the chat's shows as it is, with its card.
            if ready && !self.shown.replace(true) {
                if !self.inline && !self.faded.replace(true) {
                    fade_in(&self.container);
                } else {
                    park(&self.container, false);
                }
                reclaim_keys(window);
            }
        }

        /// Where the clip view really is while its page shows (window points from the top left), as AppKit has it:
        /// None while it's parked or hidden (the fixture's checks).
        #[cfg(feature = "dev")]
        pub fn on_screen(&self, window: &Window) -> Option<Bounds<Pixels>> {
            let parent = content_view(window)?;
            // SAFETY: as in `open`.
            unsafe {
                let hidden: bool = msg_send![&*self.container, isHiddenOrHasHiddenAncestor];
                if hidden || parked(&self.container) {
                    return None;
                }
                let frame: NSRect = msg_send![&*self.container, frame];
                let flipped: bool = msg_send![parent, isFlipped];
                let parent_bounds: NSRect = msg_send![parent, bounds];
                let y = if flipped {
                    frame.origin.y
                } else {
                    parent_bounds.size.height - frame.origin.y - frame.size.height
                };
                let bounds = Bounds::new(
                    gpui::point(gpui::px(frame.origin.x as f32), gpui::px(y as f32)),
                    gpui::size(gpui::px(frame.size.width as f32), gpui::px(frame.size.height as f32)),
                );
                Some(bounds)
            }
        }

        /// Cuts holes in the clip view where `floating` (window points) crosses it, each its shape, so what GPUI floats
        /// there shows through and takes the clicks there (`hit_test`); none clears them.
        /// Cuts `floating` out of the page and lays `tints` over it (a dialog's dim, a fade: web_holes.rs); a click in a
        /// hole or a dim is GPUI's, one in a fade the page's.
        pub fn set_holes(&self, floating: &[super::Hole], tints: &[gpui::PaintedOver]) {
            let Some(visible) = self.visible.get() else { return };
            let f = |value: Pixels| f64::from(f32::from(value));
            crate::web_holes::lay_tints(&self.container, visible, tints);
            // A little wider than each, for its shadow; in the clip view's points, y down.
            let shaped: Vec<(NSRect, f64)> = floating
                .iter()
                .filter(|hole| hole.bounds.intersects(&visible))
                .map(|hole| {
                    let bounds = hole.bounds;
                    let rect = NSRect::new(
                        NSPoint::new(
                            f(bounds.origin.x - visible.origin.x) - HOLE_MARGIN,
                            f(bounds.origin.y - visible.origin.y) - HOLE_MARGIN,
                        ),
                        NSSize::new(f(bounds.size.width) + 2. * HOLE_MARGIN, f(bounds.size.height) + 2. * HOLE_MARGIN),
                    );
                    let half = rect.size.width.min(rect.size.height) / 2.;
                    let radius = hole.radius.map_or(half, |radius| (f64::from(radius) + HOLE_MARGIN).min(half));
                    (rect, radius)
                })
                .collect();
            let holes: Vec<NSRect> = shaped.iter().map(|(rect, _)| *rect).collect();
            #[cfg(feature = "dev")]
            WINDOW_HOLES.with(|window_holes| {
                let key = &*self.container as *const AnyObject as usize;
                let shown = floating
                    .iter()
                    .map(|hole| hole.bounds.intersect(&visible))
                    .filter(|bounds| bounds.size.width > gpui::px(0.) && bounds.size.height > gpui::px(0.))
                    .collect();
                window_holes.borrow_mut().insert(key, shown);
            });
            HIT_HOLES.with(|hit| {
                let mut hit = hit.borrow_mut();
                let key = &*self.container as *const AnyObject as usize;
                let tinted = tints.iter().filter(|tint| tint.tint.is_some()).map(|tint| {
                    let bounds = tint.bounds;
                    NSRect::new(
                        NSPoint::new(f(bounds.origin.x - visible.origin.x), f(bounds.origin.y - visible.origin.y)),
                        NSSize::new(f(bounds.size.width), f(bounds.size.height)),
                    )
                });
                let taken: Vec<NSRect> = holes.iter().copied().chain(tinted).collect();
                if taken.is_empty() {
                    hit.remove(&key);
                } else {
                    hit.insert(key, taken);
                }
            });
            let size = NSSize::new(f(visible.size.width), f(visible.size.height));
            let key: Vec<[f64; 4]> = holes
                .iter()
                .map(|r| [r.origin.x, r.origin.y, r.size.width, r.size.height])
                .chain((!holes.is_empty()).then_some([0., 0., size.width, size.height]))
                .collect();
            if *self.holes.borrow() == key {
                return;
            }
            *self.holes.borrow_mut() = key;
            // SAFETY: AppKit and Core Animation on the main thread; the path is released once the mask holds it.
            unsafe {
                let layer: *mut AnyObject = msg_send![&*self.container, layer];
                if layer.is_null() {
                    return;
                }
                if holes.is_empty() {
                    let _: () = msg_send![layer, setMask: std::ptr::null_mut::<AnyObject>()];
                    return;
                }
                let Some(shape_class) = AnyClass::get(c"CAShapeLayer") else { return };
                let shape: *mut AnyObject = msg_send![shape_class, layer];
                let path = CGPathCreateMutable();
                CGPathAddRect(path, std::ptr::null(), NSRect::new(NSPoint::new(0., 0.), size));
                let flipped: bool = msg_send![layer, contentsAreFlipped];
                for (hole, radius) in &shaped {
                    // The mask's y runs up unless the layer is drawn flipped.
                    let y = if flipped { hole.origin.y } else { size.height - hole.origin.y - hole.size.height };
                    let rect = NSRect::new(NSPoint::new(hole.origin.x, y), hole.size);
                    let radius = radius.min(rect.size.width.min(rect.size.height) / 2.);
                    CGPathAddRoundedRect(path, std::ptr::null(), rect, radius, radius);
                }
                let rule = objc2_foundation::NSString::from_str("even-odd");
                let _: () = msg_send![shape, setFrame: NSRect::new(NSPoint::new(0., 0.), size)];
                let _: () = msg_send![shape, setPath: Path(path)];
                let _: () = msg_send![shape, setFillRule: &*rule];
                let _: () = msg_send![layer, setMask: shape];
                CGPathRelease(path);
            }
        }

        #[cfg(feature = "dev")]
        pub fn holes(&self) -> usize {
            self.holes.borrow().len().saturating_sub(1)
        }

        pub fn preset(&self, width: f32, height: f32) {
            let _ = self.web.set_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0., 0.).into(),
                size: wry::dpi::LogicalSize::new(f64::from(width), f64::from(height)).into(),
            });
        }

        pub fn scroll_sideways(&self, dx: f32) {
            let _ = self.web.evaluate_script(&format!("window.__filoScroll&&window.__filoScroll({dx})"));
        }

        pub fn hide(&self) {
            self.visible.set(None);
            if self.shown.replace(false) {
                park(&self.container, true);
            }
        }
    }

    impl Drop for NativePage {
        fn drop(&mut self) {
            HIT_HOLES.with(|hit| hit.borrow_mut().remove(&(&*self.container as *const AnyObject as usize)));
            #[cfg(feature = "dev")]
            WINDOW_HOLES.with(|holes| holes.borrow_mut().remove(&(&*self.container as *const AnyObject as usize)));
            crate::web_holes::lay_tints(&self.container, Bounds::default(), &[]);
            // SAFETY: as in `hide`. The web view goes with its clip view.
            unsafe {
                let _: () = msg_send![&*self.container, removeFromSuperview];
            }
        }
    }
}

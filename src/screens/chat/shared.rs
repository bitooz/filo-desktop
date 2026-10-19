// What a chat shared (local_backend/shared.rs), like a messenger's shared-media screens: pictures and videos, files,
// links and voice, newest first under month headings. Pictures and videos open in the app's viewer (media_viewer.rs),
// files in Quick Look, voice plays in place, and links open in the browser. An agent's profile (the Agents tab's edit
// page) shows its chat's under the form; the page around it scrolls it.
// The newest page is live; older ones load as the list needs them, one at a time.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, ImageSource, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task,
    Window, div, img, prelude::FluentBuilder as _, px,
};
use serde::Deserialize;
use serde_json::json;

use crate::backend;
use crate::file_preview;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};
use crate::{format, media};

/// A tab keeps loading older pages until it has this many items or the history ends.
const FILL: usize = 30;
const GRID_GAP: f32 = 2.;
const ROW_TILE: f32 = 40.;

/// One thing a message shared.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SharedItem {
    kind: String,
    message_id: String,
    at: f64,
    from_user: bool,
    url: String,
    name: Option<String>,
    title: Option<String>,
    content_type: Option<String>,
    size: Option<f64>,
    /// A voice message's length.
    duration_ms: Option<f64>,
}

impl SharedItem {
    fn key(&self) -> (String, String) {
        (self.message_id.clone(), self.url.clone())
    }

    /// This Mac's copy when the item is one (the local backend's are `file://` addresses), not something to download.
    fn local_file(&self) -> Option<std::path::PathBuf> {
        self.url.strip_prefix("file://").map(std::path::PathBuf::from)
    }

    /// Where its tile's picture loads from: its file on this Mac, else its URL. None for a video: its tile shows its
    /// poster.
    fn picture(&self) -> Option<gpui::Resource> {
        let video = self.content_type.as_deref().is_some_and(|kind| kind.starts_with("video/"));
        if video {
            return None;
        }
        Some(match self.local_file() {
            Some(file) => gpui::Resource::Path(file.as_path().into()),
            None => gpui::Resource::Uri(self.url.clone().into()),
        })
    }

    fn preview_item(&self) -> file_preview::Item {
        let video = self.content_type.as_deref().is_some_and(|kind| kind.starts_with("video/"));
        let fallback = if self.kind == "media" { if video { "Video" } else { "Picture" } } else { "File" };
        file_preview::Item {
            url: self.url.clone(),
            name: self.name.clone().unwrap_or_else(|| fallback.to_string()),
            content_type: self.content_type.clone().unwrap_or_else(|| {
                if self.kind == "media" { "image/png".to_string() } else { "application/octet-stream".to_string() }
            }),
            file: self.local_file(),
        }
    }
}

#[derive(Deserialize)]
struct SharedPage {
    items: Vec<SharedItem>,
    cursor: String,
    done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Media,
    Files,
    Links,
    Voice,
}

impl Tab {
    fn kind(self) -> &'static str {
        match self {
            Tab::Media => "media",
            Tab::Files => "file",
            Tab::Links => "link",
            Tab::Voice => "voice",
        }
    }

    fn empty(self) -> (&'static str, &'static str) {
        match self {
            Tab::Media => ("image", "No media yet."),
            Tab::Files => ("file-text", "No files yet."),
            Tab::Links => ("link", "No links yet."),
            Tab::Voice => ("mic", "No voice messages yet."),
        }
    }
}

pub struct SharedPanel {
    /// The chat whose items these are.
    session_id: String,
    /// Who "not you" is in the Voice tab.
    agent_name: String,
    tab: Tab,
    /// Newest first; a message's items keep its order.
    items: Vec<SharedItem>,
    /// What the live page held last time, to tell a deleted message from one that only moved off the page.
    live_keys: HashSet<(String, String)>,
    /// Where the next older page starts, once the first page is in.
    cursor: Option<String>,
    loaded: bool,
    done: bool,
    fetching: bool,
    error: Option<String>,
    /// An older page failed; loading stops until "Try again".
    more_error: bool,
    /// The column it's laid out in: tiles need a height and one-line text a width before layout.
    width: f32,
    /// Which grid rows were near the screen: the rest leave their pictures out (the page around scrolls this).
    on_screen: crate::pictures::OnScreen,
    _live: Task<()>,
}

impl SharedPanel {
    pub fn new(session_id: String, agent_name: String, width: f32, cx: &mut Context<Self>) -> Self {
        let _live = backend::subscribe(
            cx,
            "chatShared:list",
            json!({ "sessionId": session_id, "cursor": null }),
            |this: &mut Self, result: Result<SharedPage, String>, cx| this.on_live(result, cx),
        );
        SharedPanel {
            session_id,
            agent_name,
            tab: Tab::Media,
            items: Vec::new(),
            live_keys: HashSet::new(),
            cursor: None,
            loaded: false,
            done: false,
            fetching: false,
            error: None,
            more_error: false,
            width,
            on_screen: Default::default(),
            _live,
        }
    }

    pub fn set_agent_name(&mut self, name: String, cx: &mut Context<Self>) {
        if self.agent_name != name {
            self.agent_name = name;
            cx.notify();
        }
    }

    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.width != width {
            self.width = width;
            cx.notify();
        }
    }

    /// The newest page changed. Older pages continue from the cursor of the first answer, so an item that moved off
    /// the live page stays; one gone from the span the page still covers was deleted.
    fn on_live(&mut self, result: Result<SharedPage, String>, cx: &mut Context<Self>) {
        match result {
            Ok(page) => {
                if !self.loaded {
                    self.loaded = true;
                    self.cursor = Some(page.cursor);
                    self.done = page.done;
                }
                let keys: HashSet<(String, String)> = page.items.iter().map(SharedItem::key).collect();
                let oldest = page.items.iter().map(|item| item.at).reduce(f64::min);
                let previous = std::mem::take(&mut self.live_keys);
                self.items.retain(|item| {
                    let key = item.key();
                    if keys.contains(&key) {
                        return false;
                    }
                    !(previous.contains(&key) && oldest.is_some_and(|oldest| item.at >= oldest))
                });
                self.items.extend(page.items);
                self.items.sort_by(|a, b| b.at.total_cmp(&a.at));
                self.live_keys = keys;
                self.error = None;
            }
            Err(error) if !self.loaded => self.error = Some(error),
            Err(_) => {}
        }
        self.fill(cx);
        cx.notify();
    }

    fn count(&self, tab: Tab) -> usize {
        self.items.iter().filter(|item| item.kind == tab.kind()).count()
    }

    /// Loads the next older page while the open tab is short.
    fn fill(&mut self, cx: &mut Context<Self>) {
        if self.count(self.tab) < FILL {
            self.load_older(cx);
        }
    }

    /// The next older page, when there is one and none is loading (the page scrolled near this list's end).
    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        if !self.loaded || self.done || self.fetching || self.more_error {
            return;
        }
        let Some(cursor) = self.cursor.clone() else { return };
        self.fetching = true;
        let args = json!({ "sessionId": self.session_id, "cursor": cursor });
        backend::call(cx, "chatShared:list", args, move |this: &mut Self, result: Result<SharedPage, String>, cx| {
            this.fetching = false;
            match result {
                Ok(page) => {
                    let known: HashSet<(String, String)> = this.items.iter().map(SharedItem::key).collect();
                    this.items.extend(page.items.into_iter().filter(|item| !known.contains(&item.key())));
                    this.items.sort_by(|a, b| b.at.total_cmp(&a.at));
                    this.cursor = Some(page.cursor);
                    this.done = page.done;
                    this.fill(cx);
                }
                Err(error) => {
                    eprintln!("chatShared:list failed: {error}");
                    this.more_error = true;
                }
            }
            cx.notify();
        });
    }

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.fill(cx);
            cx.notify();
        }
    }
}

/// "September 2026"
fn month_label(ms: f64) -> String {
    format::local(ms).format("%B %Y").to_string()
}

/// The host without "www.", or the url itself when it has none.
fn domain(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.trim_start_matches("www.").to_string()))
        .unwrap_or_else(|| url.to_string())
}

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

fn open_url(url: String) -> OnClick {
    Box::new(move |_, _, cx| cx.open_url(&url))
}

fn preview(item: &SharedItem) -> OnClick {
    let item = item.preview_item();
    Box::new(move |_, window, cx| file_preview::open(item.clone(), window, cx))
}

/// The items split into runs of one month each.
fn by_month<'a>(items: &[&'a SharedItem]) -> Vec<(String, Vec<&'a SharedItem>)> {
    let mut groups: Vec<(String, Vec<&SharedItem>)> = Vec::new();
    for item in items {
        let month = month_label(item.at);
        match groups.last_mut() {
            Some((last, group)) if *last == month => group.push(item),
            _ => groups.push((month, vec![item])),
        }
    }
    groups
}

fn month_heading(label: String, cx: &App) -> AnyElement {
    div()
        .pt(px(space::MD))
        .pb(px(space::SM))
        .caption()
        .semibold()
        .text_color(cx.colors().text_secondary)
        .child(label)
        .into_any_element()
}

/// Three square tiles a row across the column; a video shows its frame and length. A tile opens the app's viewer on
/// `gallery` (the tab's pictures and videos) at its place, so the arrows step through the rest. A row far off screen
/// leaves its pictures out (`on_screen`).
fn media_grid(
    items: &[&SharedItem],
    first: usize,
    gallery: &Rc<Vec<file_preview::Item>>,
    on_screen: &crate::pictures::OnScreen,
    width: f32,
    cx: &mut App,
) -> AnyElement {
    let colors = *cx.colors();
    // A video shows its poster and length, made from its file.
    let posters: Vec<Option<crate::video_player::Poster>> = items
        .iter()
        .map(|item| {
            let video = item.content_type.as_deref().is_some_and(|kind| kind.starts_with("video/"));
            video.then(|| crate::video_player::poster(&item.preview_item(), cx)).flatten()
        })
        .collect();
    let tile = ((width - 2. * GRID_GAP) / 3.).floor();
    div()
        .flex()
        .flex_col()
        .gap(px(GRID_GAP))
        .children(items.chunks(3).enumerate().map(|(row, chunk)| {
            let (key_message, key_url) = chunk[0].key();
            let key = SharedString::from(format!("{key_message}:{key_url}"));
            // The first rows show at once; the rest once they've been drawn near the screen.
            let shows = on_screen.shows(&key, first + row * 3 < 12);
            div()
                .relative()
                .flex()
                .gap(px(GRID_GAP))
                .children(chunk.iter().enumerate().map(|(column, item)| {
                    let video = item.content_type.as_deref().is_some_and(|kind| kind.starts_with("video/"));
                    let poster = posters[row * 3 + column].as_ref();
                    let picture = item
                        .picture()
                        .or_else(|| poster.map(|poster| gpui::Resource::Path(poster.picture.as_path().into())));
                    let duration = poster.and_then(|poster| poster.duration_ms);
                    div()
                        .id(SharedString::from(format!("shared-media-{}", first + row * 3 + column)))
                        .relative()
                        .flex_1()
                        .h(px(tile))
                        .overflow_hidden()
                        .bg(colors.surface_strong)
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.85))
                        .map(|this| match picture {
                            // Off screen: the tile's colour only.
                            _ if !shows => this,
                            Some(picture) => this.child(
                                img(crate::pictures::source(ImageSource::Resource(picture)))
                                    .size_full()
                                    .object_fit(ObjectFit::Cover),
                            ),
                            None => this.flex().items_center().justify_center().child(ui::icon_sized(
                                "video",
                                22.,
                                colors.text_secondary,
                            )),
                        })
                        .when(video, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .bottom(px(space::XS))
                                    .right(px(space::XS))
                                    .px(px(space::XS + 2.))
                                    .rounded(px(radius::XS))
                                    .bg(colors.media.opacity(0.62))
                                    .flex()
                                    .items_center()
                                    .gap(px(space::XXS))
                                    .micro()
                                    .text_color(colors.on_media)
                                    .child(ui::icon_sized("play", 9., colors.on_media))
                                    .when_some(duration, |this, duration| this.child(format::duration_clock(duration))),
                            )
                        })
                        .on_click({
                            let (gallery, at) = (gallery.clone(), first + row * 3 + column);
                            move |_, window, cx| crate::media_viewer::open(gallery.to_vec(), at, None, window, cx)
                        })
                }))
                // The last row keeps its tiles the size of the others.
                .children((chunk.len()..3).map(|_| div().flex_1()))
                .child(on_screen.probe(key))
        }))
        .into_any_element()
}

/// A list row: a 40 px tile, a title and a line under it, and what goes at the end.
fn row(
    id: String,
    tile: AnyElement,
    title: AnyElement,
    subtitle: AnyElement,
    trailing: Option<String>,
    on_click: OnClick,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .gap(px(space::MD))
        .px(px(space::SM))
        .py(px(space::SM))
        .rounded(px(radius::MD))
        .cursor_pointer()
        .hover(|style| style.bg(colors.selected_fill.opacity(0.5)))
        .child(tile)
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(title).child(subtitle))
        .when_some(trailing, |this, trailing| {
            this.child(div().flex_none().caption().text_color(colors.text_tertiary).child(trailing))
        })
        .on_click(on_click)
        .into_any_element()
}

fn tile(round: bool, cx: &App) -> gpui::Div {
    let colors = *cx.colors();
    div()
        .size(px(ROW_TILE))
        .flex_none()
        .rounded(px(if round { radius::FULL } else { radius::SM }))
        .overflow_hidden()
        .bg(colors.accent_soft)
        .text_color(colors.on_accent_soft)
        .flex()
        .items_center()
        .justify_center()
}

impl SharedPanel {
    /// The text width of a row: the column less the row's padding, the tile and the gap.
    fn text_width(&self) -> f32 {
        (self.width - 2. * space::SM - ROW_TILE - space::MD).max(0.)
    }

    /// One line, cut with "…" at the row's text width (a flexible width isn't known in time in a scrolling page).
    fn line(&self, text: impl Into<SharedString>) -> gpui::Div {
        div().w(px(self.text_width())).truncate().child(text.into())
    }

    fn file_row(&self, index: usize, item: &SharedItem, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let picture = tile(false, cx).map(|this| this.child(ui::icon_sized("file-text", 20., colors.on_accent_soft)));
        let name = item.name.clone().unwrap_or_else(|| domain(&item.url));
        let details = match item.size {
            Some(size) => format!("{} · {}", format::file_size(size), format::short_date(item.at)),
            None => format::short_date(item.at),
        };
        row(
            format!("shared-file-{index}"),
            picture.into_any_element(),
            self.line(name).callout().medium().into_any_element(),
            self.line(details).footnote().text_color(colors.text_secondary).into_any_element(),
            None,
            preview(item),
            cx,
        )
    }

    fn link_row(&self, index: usize, item: &SharedItem, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let host = domain(&item.url);
        let letter =
            host.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or("·".into());
        let picture = tile(false, cx).map(|this| this.title3().child(letter));
        row(
            format!("shared-link-{index}"),
            picture.into_any_element(),
            self.line(item.title.clone().unwrap_or(host)).callout().semibold().into_any_element(),
            self.line(item.url.clone()).footnote().text_color(colors.link).into_any_element(),
            None,
            open_url(item.url.clone()),
            cx,
        )
    }

    /// A recording is titled by when it was sent; an audio file by its name.
    fn voice_row(&self, index: usize, item: &SharedItem, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let audio_id = format!("shared-voice-{}-{}", item.message_id, item.url);
        let icon = match (media::is_playing(&audio_id, cx), &item.name) {
            (true, _) => "pause",
            (false, Some(_)) => "music",
            (false, None) => "mic",
        };
        let picture = tile(true, cx).child(ui::icon_sized(icon, 18., colors.on_accent_soft));
        let sender = if item.from_user { "You".to_string() } else { self.agent_name.clone() };
        let (title, subtitle) = match &item.name {
            Some(name) => (name.clone(), format!("{sender} · {}", format::short_date(item.at))),
            None => (format::short_date_time(item.at), sender),
        };
        let duration = item.duration_ms.map(format::duration_clock);
        let width = self.text_width() - if duration.is_some() { 40. } else { 0. };
        row(
            format!("shared-voice-{index}"),
            picture.into_any_element(),
            div().w(px(width)).truncate().callout().medium().child(title).into_any_element(),
            div()
                .w(px(width))
                .truncate()
                .footnote()
                .text_color(colors.text_secondary)
                .child(subtitle)
                .into_any_element(),
            duration,
            {
                let url = item.url.clone();
                Box::new(move |_, _, cx| media::toggle(audio_id.clone(), url.clone(), None, cx))
            },
            cx,
        )
    }

    fn render_list(&self, items: &[&SharedItem], cx: &mut App) -> AnyElement {
        let mut body = div().flex().flex_col();
        let mut index = 0;
        let gallery = Rc::new(if self.tab == Tab::Media {
            items.iter().map(|item| item.preview_item()).collect()
        } else {
            Vec::new()
        });
        for (month, group) in by_month(items) {
            body = body.child(month_heading(month, cx));
            body = match self.tab {
                Tab::Media => body.child(media_grid(&group, index, &gallery, &self.on_screen, self.width, cx)),
                _ => body.child(div().flex().flex_col().children(group.iter().enumerate().map(|(offset, item)| {
                    match self.tab {
                        Tab::Files => self.file_row(index + offset, item, cx),
                        Tab::Links => self.link_row(index + offset, item, cx),
                        _ => self.voice_row(index + offset, item, cx),
                    }
                }))),
            };
            index += group.len();
        }
        body.into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.more_error {
            let colors = *cx.colors();
            return Some(
                div()
                    .p(px(space::LG))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(space::SM))
                    .child(div().footnote().text_color(colors.text_secondary).child("Couldn’t load older items."))
                    .child(Button::secondary("shared-retry", "Try again").small().on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.more_error = false;
                            this.load_older(cx);
                            cx.notify();
                        },
                    )))
                    .into_any_element(),
            );
        }
        (!self.done)
            .then(|| div().p(px(space::LG)).flex().justify_center().child(ui::spinner(18., cx)).into_any_element())
    }
}

impl Render for SharedPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = ui::segmented(
            "shared-tabs",
            vec![
                (Tab::Media, "Media".into(), None),
                (Tab::Files, "Files".into(), None),
                (Tab::Links, "Links".into(), None),
                (Tab::Voice, "Voice".into(), None),
            ],
            &self.tab,
            {
                let this = cx.entity().downgrade();
                move |tab, _, cx| {
                    this.update(cx, |this, cx| this.select(tab, cx)).ok();
                }
            },
            cx,
        );
        let colors = *cx.colors();
        let items: Vec<&SharedItem> = self.items.iter().filter(|item| item.kind == self.tab.kind()).collect();
        // A short message in the list's place: an error, the tab's empty state.
        let notice = |icon: &str, text: String| {
            div()
                .py(px(space::XXL))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::SM))
                .text_center()
                .child(ui::icon_sized(icon, 24., colors.text_tertiary))
                .child(div().callout().text_color(colors.text_secondary).child(text))
                .into_any_element()
        };
        let body = if let Some(error) = &self.error {
            notice("triangle-alert", error.clone())
        } else if !self.loaded || (items.is_empty() && !self.done && !self.more_error) {
            div().py(px(space::XXL)).flex().justify_center().child(ui::spinner(20., cx)).into_any_element()
        } else if items.is_empty() && self.done {
            let (icon, text) = self.tab.empty();
            notice(icon, text.to_string())
        } else {
            div()
                .flex()
                .flex_col()
                .child(self.render_list(&items, cx))
                .children(self.render_footer(cx))
                .into_any_element()
        };
        div().w(px(self.width)).flex().flex_col().gap(px(space::XS)).child(tabs).child(body)
    }
}

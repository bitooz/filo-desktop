// What a chat shared (local_backend/shared.rs), like a messenger's shared-media screens: pictures and videos, files,
// links and voice, newest first under month headings. Pictures and videos open in the app's viewer (media_viewer.rs),
// files in Quick Look, voice plays in place, and links open in the browser. An agent's profile (the Agents tab's edit
// page) shows its chat's under the form, Group settings the group's; the page around it scrolls it.
// The newest page is live; older ones load as the list needs them, one at a time.
// A right-click on a picture, video, file or recording opens its menu (`item_menu`): Show in Chat, Show in Finder, Edit
// (a picture, in the chat's image editor), Select, and Delete for the files Filo keeps (`deletable`: its copies and voice
// messages' recordings, never a file shown where it is on this Mac). ⌘-click and Shift-click pick several, the Mac way;
// the menu's Select starts picking (`selecting`): a plain click then picks or lets go of a file too, instead of opening
// it, until the last one is let go or Esc. Picked tiles show a mark and their size. The menu and the Delete key act on
// all picked, and Esc lets them go. Deleting frees the room they take, and their messages show them as no longer kept.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, EventEmitter, FocusHandle, ImageSource, InteractiveElement as _, IntoElement,
    KeyDownEvent, ObjectFit, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Task, WeakEntity, Window, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
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
/// A picked tile's mark.
const CHECK: f32 = 20.;

/// What the Shared list asks of the page around it: its menu's Show in Chat (the message's id, and how many messages
/// from the newest it is: 0 when that isn't known) and Edit (a picture).
pub enum SharedEvent {
    ShowInChat(String, usize),
    Edit(file_preview::Item),
}

/// One shared item, as the list knows it (`SharedItem::key`): its message and its address.
type Key = (String, String);

/// How a click picks: ⌘ adds or takes away one, Shift the run from the last one picked.
#[derive(Clone, Copy)]
enum Pick {
    Toggle,
    Range,
}

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
    /// Who sent a group post (the group's list; a chat's says "You" or the agent).
    sender: Option<String>,
    /// It's Filo's copy, which the user can delete (local_backend/shared.rs `deleteFiles`).
    #[serde(default)]
    deletable: bool,
}

impl SharedItem {
    fn key(&self) -> (String, String) {
        (self.message_id.clone(), self.url.clone())
    }

    fn is_video(&self) -> bool {
        self.content_type.as_deref().is_some_and(|kind| kind.starts_with("video/"))
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
    /// Itself, for what a click on a file and its menu do.
    weak: WeakEntity<Self>,
    /// The files picked (⌘-click, Shift-click, the menu's Select), and the last one picked, where a Shift-click's run
    /// starts.
    picked: HashSet<Key>,
    anchor: Option<Key>,
    /// Picking since the menu's Select: a plain click picks or lets go of a file, until none is picked.
    selecting: bool,
    /// Takes the Delete key while files are picked.
    focus: FocusHandle,
    /// The picked files are being deleted.
    deleting: bool,
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
            weak: cx.entity().downgrade(),
            picked: HashSet::new(),
            anchor: None,
            selecting: false,
            focus: cx.focus_handle(),
            deleting: false,
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
                self.forget_gone_picks();
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

    /// Lets go of the files picked (Esc, a plain click), and ends picking with plain clicks.
    fn clear_picks(&mut self, cx: &mut Context<Self>) {
        if !self.picked.is_empty() || self.selecting {
            self.picked.clear();
            self.anchor = None;
            self.selecting = false;
            cx.notify();
        }
    }

    /// Picks `key` (⌘-click: or lets it go), or (Shift-click) every item of its tab from the last one picked to it.
    fn pick(&mut self, key: Key, how: Pick, cx: &mut Context<Self>) {
        let run = match (how, &self.anchor) {
            (Pick::Range, Some(anchor)) => {
                let tab: Vec<Key> =
                    self.items.iter().filter(|item| item.kind == self.tab.kind()).map(SharedItem::key).collect();
                let at = |key: &Key| tab.iter().position(|other| other == key);
                at(anchor).zip(at(&key)).map(|(from, to)| tab[from.min(to)..=from.max(to)].to_vec())
            }
            _ => None,
        };
        match run {
            Some(run) => self.picked.extend(run),
            None if !self.picked.remove(&key) => _ = self.picked.insert(key.clone()),
            None => {}
        }
        self.anchor = Some(key);
        // The last one let go ends picking: a plain click opens a file again.
        self.selecting &= !self.picked.is_empty();
        cx.notify();
    }

    /// The menu's Select: picks `key`, and plain clicks pick too from now on (`selecting`).
    fn start_selecting(&mut self, key: Key, cx: &mut Context<Self>) {
        self.selecting = true;
        if !self.picked.contains(&key) {
            self.pick(key, Pick::Toggle, cx);
        }
    }

    /// How many files are picked (`FILO_OPEN=shared-delete-drive`).
    #[cfg(feature = "dev")]
    pub fn drive_picked(&self) -> usize {
        self.picked.len()
    }

    /// The menu's Show in Chat for `message` (`FILO_OPEN=shared-show-drive`).
    #[cfg(feature = "dev")]
    pub fn drive_show_in_chat(&mut self, message: String, cx: &mut Context<Self>) {
        self.show_in_chat(message, cx);
    }

    /// The menu's Show in Chat: how far back `message` is (`chatShared:depth`), so its chat loads back to it, then the
    /// page around shows it there.
    fn show_in_chat(&mut self, message: String, cx: &mut Context<Self>) {
        let args = json!({ "sessionId": self.session_id, "messageId": message });
        backend::call(cx, "chatShared:depth", args, move |_: &mut Self, result: Result<Option<usize>, String>, cx| {
            let depth = result.unwrap_or_else(|error| {
                eprintln!("chatShared:depth failed: {error}");
                None
            });
            cx.emit(SharedEvent::ShowInChat(message.clone(), depth.unwrap_or(0)));
        });
    }

    /// What `key`'s menu acts on: every file picked when it's one of them, else itself.
    fn targets(&self, key: &Key) -> Vec<SharedItem> {
        if self.picked.contains(key) {
            self.items.iter().filter(|item| self.picked.contains(&item.key())).cloned().collect()
        } else {
            self.items.iter().filter(|item| item.key() == *key).cloned().collect()
        }
    }

    /// Picks no longer listed (deleted, or their message gone) aren't picked.
    fn forget_gone_picks(&mut self) {
        let listed: HashSet<(String, String)> = self.items.iter().map(SharedItem::key).collect();
        self.picked.retain(|key| listed.contains(key));
        self.selecting &= !self.picked.is_empty();
    }

    /// The Delete key: the picked files Filo keeps.
    fn delete_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<SharedItem> =
            self.items.iter().filter(|item| self.picked.contains(&item.key()) && item.deletable).cloned().collect();
        self.confirm_delete(items, window, cx);
    }

    /// Asks, then deletes Filo's copies of `items` (each `deletable`); their messages then show them as no longer kept.
    fn confirm_delete(&mut self, items: Vec<SharedItem>, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<SharedItem> = items.into_iter().filter(|item| item.deletable).collect();
        if items.is_empty() || self.deleting {
            return;
        }
        let size: f64 = items.iter().filter_map(|item| item.size).sum();
        let files: Vec<serde_json::Value> =
            items.iter().map(|item| json!({ "messageId": item.message_id, "url": item.url })).collect();
        let title = match files.len() {
            1 => "Delete this file?".to_string(),
            count => format!("Delete {count} files?"),
        };
        let message = format!(
            "Filo's {} deleted, which frees {}. The messages they came with show them as no longer kept.",
            if files.len() == 1 { "copy is" } else { "copies are" },
            format::file_size(size)
        );
        let this = cx.entity().downgrade();
        ui::confirm(window, cx, title, message, "Delete", "Cancel", true, move |window, cx| {
            this.update(cx, |this, cx| this.delete(files.clone(), window, cx)).ok();
        });
    }

    /// Deletes them; why it couldn't, in a toast.
    fn delete(&mut self, files: Vec<serde_json::Value>, window: &mut Window, cx: &mut Context<Self>) {
        let gone: HashSet<Key> = files
            .iter()
            .filter_map(|file| Some((file["messageId"].as_str()?.to_string(), file["url"].as_str()?.to_string())))
            .collect();
        self.deleting = true;
        let handle = window.window_handle();
        let args = json!({ "sessionId": self.session_id, "files": files });
        backend::call(
            cx,
            "chatShared:deleteFiles",
            args,
            move |this: &mut Self, result: Result<serde_json::Value, String>, cx| {
                this.deleting = false;
                match result {
                    Ok(_) => {
                        // Gone from the list now; the live page agrees once it's read again. Any picked that weren't
                        // Filo's to delete are let go too.
                        this.items.retain(|item| !gone.contains(&item.key()));
                        this.picked.clear();
                        this.anchor = None;
                        this.selecting = false;
                        this.fill(cx);
                    }
                    Err(error) => {
                        eprintln!("chatShared:deleteFiles failed: {error}");
                        handle.update(cx, |_, window, cx| ui::toast_error(window, cx, "Couldn’t delete", error)).ok();
                    }
                }
                cx.notify();
            },
        );
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

/// The files picked, and the panel a click picks them in and a right-click opens a file's menu for.
struct Picks<'a> {
    picked: &'a HashSet<Key>,
    panel: WeakEntity<SharedPanel>,
}

impl Picks<'_> {
    /// What a click on `item` does: with ⌘ or Shift, picks it (the panel takes the keyboard, for Delete), as a plain
    /// one does while picking since the menu's Select; else a plain one lets go of those picked and `opens` it.
    fn on_click(&self, item: &SharedItem, opens: OnClick) -> OnClick {
        let (panel, key) = (self.panel.clone(), item.key());
        Box::new(move |event, window, cx| {
            let modifiers = event.modifiers();
            let picks = panel
                .update(cx, |panel, cx| {
                    let how = if modifiers.platform || (panel.selecting && !modifiers.shift) {
                        Some(Pick::Toggle)
                    } else if modifiers.shift {
                        Some(Pick::Range)
                    } else {
                        None
                    };
                    match how {
                        Some(how) => {
                            window.focus(&panel.focus);
                            panel.pick(key.clone(), how, cx);
                        }
                        None => panel.clear_picks(cx),
                    }
                    how.is_some()
                })
                .unwrap_or(false);
            if !picks {
                opens(event, window, cx);
            }
        })
    }

    fn picked(&self, item: &SharedItem) -> bool {
        self.picked.contains(&item.key())
    }

    /// `element` with `item`'s right-click menu (`item_menu`), under an id of its own: gpui-component names every
    /// context menu "context-menu", so items side by side would share one.
    fn with_menu(
        &self,
        id: String,
        item: &SharedItem,
        element: impl gpui::ParentElement + gpui::Styled + IntoElement + 'static,
    ) -> AnyElement {
        let (panel, key) = (self.panel.clone(), item.key());
        div()
            .id(SharedString::from(format!("{id}-menu")))
            .size_full()
            .child(element.context_menu(move |menu, window, cx| item_menu(menu, &panel, &key, window, cx)))
            .into_any_element()
    }
}

/// A file's right-click menu, built as it opens: for it, or for every file picked when it's one of them. Show in Chat
/// (one file: its message), Show in Finder, Edit (one picture), Select (one not picked yet: from then a plain click
/// picks too), and Delete (those Filo keeps; none for a file shown where it is).
fn item_menu(
    menu: PopupMenu,
    panel: &WeakEntity<SharedPanel>,
    key: &Key,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(shared) = panel.upgrade() else { return menu };
    let (targets, picked, focus) = {
        let shared = shared.read(cx);
        (shared.targets(key), shared.picked.contains(key), shared.focus.clone())
    };
    // Closed, the menu gives the list the keyboard back, as Finder's does, so Delete acts on the files picked.
    cx.subscribe_in(&cx.entity(), window, move |_, _, _: &gpui::DismissEvent, window, _| window.focus(&focus)).detach();
    let single = (targets.len() == 1).then(|| targets[0].clone());
    let mut menu = menu;
    if let Some(item) = &single {
        let (panel, message) = (panel.clone(), item.message_id.clone());
        menu = menu.item(PopupMenuItem::new("Show in Chat").on_click(move |_, _, cx| {
            panel.update(cx, |panel, cx| panel.show_in_chat(message.clone(), cx)).ok();
        }));
    }
    let files: Vec<std::path::PathBuf> = targets.iter().filter_map(SharedItem::local_file).collect();
    if !files.is_empty() {
        menu = menu.item(PopupMenuItem::new("Show in Finder").on_click(move |_, _, _| show_in_finder(&files)));
    }
    if let Some(item) = single.filter(|item| item.kind == "media" && !item.is_video()) {
        let (panel, picture) = (panel.clone(), item.preview_item());
        menu = menu.item(PopupMenuItem::new("Edit").on_click(move |_, _, cx| {
            panel.update(cx, |_, cx| cx.emit(SharedEvent::Edit(picture.clone()))).ok();
        }));
    }
    if !picked {
        let (panel, key) = (panel.clone(), key.clone());
        menu = menu.item(PopupMenuItem::new("Select").on_click(move |_, window, cx| {
            panel
                .update(cx, |panel, cx| {
                    window.focus(&panel.focus);
                    panel.start_selecting(key.clone(), cx);
                })
                .ok();
        }));
    }
    let deletable: Vec<SharedItem> = targets.into_iter().filter(|item| item.deletable).collect();
    if !deletable.is_empty() {
        let label = match deletable.len() {
            1 => "Delete".to_string(),
            count => format!("Delete {count} Files"),
        };
        let panel = panel.clone();
        menu = menu.separator().item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
            panel.update(cx, |panel, cx| panel.confirm_delete(deletable.clone(), window, cx)).ok();
        }));
    }
    menu
}

/// Finder, with `files` selected in one window, as its own Show in Finder does.
fn show_in_finder(files: &[std::path::PathBuf]) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSArray, NSString, NSURL};
    let urls: Vec<objc2::rc::Retained<NSURL>> =
        files.iter().filter_map(|file| Some(NSURL::fileURLWithPath(&NSString::from_str(file.to_str()?)))).collect();
    let urls = NSArray::from_retained_slice(&urls);
    // SAFETY: an AppKit message on the main thread (a menu item's click), with an array of file URLs.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let _: () = msg_send![workspace, activateFileViewerSelectingURLs: &*urls];
    }
}

/// A picked tile's round mark, filled and ticked.
fn check_mark(cx: &App) -> gpui::Div {
    let colors = *cx.colors();
    div()
        .size(px(CHECK))
        .flex_none()
        .rounded(px(radius::FULL))
        .border_2()
        .border_color(colors.accent)
        .bg(colors.accent)
        .flex()
        .items_center()
        .justify_center()
        .child(ui::icon_sized("check", 12., colors.on_accent))
}

/// Three square tiles a row across the column; a video shows its frame and length. A tile opens the app's viewer on
/// `gallery` (the tab's pictures and videos) at its place, so the arrows step through the rest. A row far off screen
/// leaves its pictures out (`on_screen`). ⌘- or Shift-click picks a tile instead (`picks`): a picked one shows a mark,
/// its outline and its size. Each has its right-click menu.
fn media_grid(
    items: &[&SharedItem],
    first: usize,
    gallery: &Rc<Vec<file_preview::Item>>,
    on_screen: &crate::pictures::OnScreen,
    width: f32,
    picks: &Picks,
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
            let tiles = div()
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
                    let picked = picks.picked(item);
                    let at = first + row * 3 + column;
                    let square = div()
                        .id(SharedString::from(format!("shared-media-{at}")))
                        .relative()
                        .size_full()
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
                        .when(picked, |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .p(px(space::XS))
                                    .flex()
                                    .flex_col()
                                    .justify_between()
                                    .items_end()
                                    .border_2()
                                    .border_color(colors.accent)
                                    .child(check_mark(cx))
                                    .when_some(item.size, |this, size| {
                                        this.child(
                                            div()
                                                .px(px(space::XS + 2.))
                                                .rounded(px(radius::XS))
                                                .bg(colors.media.opacity(0.62))
                                                .micro()
                                                .text_color(colors.on_media)
                                                .child(format::file_size(size)),
                                        )
                                    }),
                            )
                        })
                        .on_click(picks.on_click(item, {
                            let gallery = gallery.clone();
                            Box::new(move |_, window, cx| {
                                crate::media_viewer::open(gallery.to_vec(), at, None, window, cx)
                            })
                        }));
                    div().flex_1().min_w_0().overflow_hidden().h(px(tile)).child(picks.with_menu(
                        format!("shared-media-{at}"),
                        item,
                        square,
                    ))
                }))
                // The last row keeps its tiles the size of the others.
                .children((chunk.len()..3).map(|_| div().flex_1()))
                .child(on_screen.probe(key));
            // A drive clicks a tile by its row's place.
            crate::smoke::tracked_fill(format!("shared-media-row-{}", first + row * 3), tiles)
        }))
        .into_any_element()
}

/// A list row: a 40 px tile, a title and a line under it, and what goes at the end. A file's (`menu`: the panel's
/// picks and the item) is drawn picked when it is, and has its right-click menu; a link's has neither.
#[allow(clippy::too_many_arguments)]
fn row(
    id: String,
    tile: AnyElement,
    title: AnyElement,
    subtitle: AnyElement,
    trailing: Option<String>,
    on_click: OnClick,
    menu: Option<(&Picks, &SharedItem)>,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    let picked = menu.is_some_and(|(picks, item)| picks.picked(item));
    let row = div()
        .id(SharedString::from(id.clone()))
        .flex()
        .items_center()
        .gap(px(space::MD))
        .px(px(space::SM))
        .py(px(space::SM))
        .rounded(px(radius::MD))
        .cursor_pointer()
        .map(|this| match picked {
            true => this.bg(colors.selected_fill),
            false => this.hover(|style| style.bg(colors.selected_fill.opacity(0.5))),
        })
        .child(tile)
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(title).child(subtitle))
        .when_some(trailing, |this, trailing| {
            this.child(div().flex_none().caption().text_color(colors.text_tertiary).child(trailing))
        })
        .on_click(on_click);
    match menu {
        Some((picks, item)) => picks.with_menu(id, item, row),
        None => row.into_any_element(),
    }
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

    fn picks(&self) -> Picks<'_> {
        Picks { picked: &self.picked, panel: self.weak.clone() }
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
        let picks = self.picks();
        row(
            format!("shared-file-{index}"),
            picture.into_any_element(),
            self.line(name).callout().medium().into_any_element(),
            self.line(details).footnote().text_color(colors.text_secondary).into_any_element(),
            None,
            picks.on_click(item, preview(item)),
            Some((&picks, item)),
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
            None,
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
        let sender = match &item.sender {
            _ if item.from_user => "You".to_string(),
            Some(sender) => sender.clone(),
            None => self.agent_name.clone(),
        };
        // Its size once it's picked.
        let size = item.size.filter(|_| self.picked.contains(&item.key())).map(format::file_size);
        let (title, subtitle) = match &item.name {
            Some(name) => (name.clone(), format!("{sender} · {}", size.unwrap_or_else(|| format::short_date(item.at)))),
            None => {
                (format::short_date_time(item.at), size.map_or(sender.clone(), |size| format!("{sender} · {size}")))
            }
        };
        let duration = item.duration_ms.map(format::duration_clock);
        let width = self.text_width() - if duration.is_some() { 40. } else { 0. };
        let picks = self.picks();
        let on_click = picks.on_click(item, {
            let url = item.url.clone();
            Box::new(move |_, _, cx| media::toggle(audio_id.clone(), url.clone(), None, cx))
        });
        let row = row(
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
            on_click,
            Some((&picks, item)),
            cx,
        );
        crate::smoke::tracked_fill(format!("shared-voice-{index}"), row)
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
                Tab::Media => {
                    body.child(media_grid(&group, index, &gallery, &self.on_screen, self.width, &self.picks(), cx))
                }
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

impl EventEmitter<SharedEvent> for SharedPanel {}

impl Render for SharedPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Esc lets go of the files picked, before the page around it takes Esc; over a file's open menu, it closes the
        // menu, the files stay picked and the list has the keyboard again (for Delete).
        if !self.picked.is_empty() {
            crate::escape::layer(window, cx, |this, window, cx| {
                if gpui_component::one_menu::close_in(window, cx) {
                    window.focus(&this.focus);
                } else {
                    this.clear_picks(cx);
                }
            });
        }
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
        // A drive picks a tab by its place on the bar.
        let tabs = crate::smoke::tracked_fill("shared-tabs", tabs);
        div()
            .id("shared-panel")
            .w(px(self.width))
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .track_focus(&self.focus)
            // A click in it gives it the keyboard, for Delete.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, window, _| window.focus(&this.focus)),
            )
            // The Delete key (⌫, or ⌦) deletes the files picked, Filo's own.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "backspace" | "delete") && !this.picked.is_empty() {
                    cx.stop_propagation();
                    this.delete_picked(window, cx);
                }
            }))
            .child(tabs)
            .child(body)
    }
}

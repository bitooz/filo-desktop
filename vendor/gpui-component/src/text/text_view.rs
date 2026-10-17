use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, DismissEvent, Element, ElementId, Entity,
    EntityId, Focusable, FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement,
    IntoElement, KeyBinding, LayoutId, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, RenderOnce, SharedString, Size, StyleRefinement, Styled, Subscription, Timer, Window, anchored,
    canvas, deferred, div, px,
};
use smol::stream::StreamExt;

use crate::highlighter::HighlightTheme;
use crate::menu::{PopupMenu, PopupMenuItem};
use crate::notification::Notification;
use crate::one_menu::{self, MenuKey};
use crate::WindowExt as _;
use crate::scroll::ScrollableElement;
use crate::text::node::CodeBlock;
use crate::{ActiveTheme, StyledExt, v_flex};
use crate::{
    global_state::GlobalState,
    input::{self},
    text::{
        TextViewStyle,
        node::{self, NodeContext},
    },
};

const CONTEXT: &'static str = "TextView";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(vec![
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", input::Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", input::Copy, Some(CONTEXT)),
    ]);
}

#[derive(IntoElement, Clone)]
struct TextViewElement {
    list_state: Option<ListState>,
    state: Entity<TextViewState>,
}

impl RenderOnce for TextViewElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| {
            v_flex()
                .size_full()
                .map(|this| match &mut state.parsed_result {
                    Some(Ok(content)) => this.child(content.root_node.render_root(
                        self.list_state.clone(),
                        &content.node_cx,
                        window,
                        cx,
                    )),
                    Some(Err(err)) => this.child(
                        v_flex()
                            .gap_1()
                            .child("Failed to parse content")
                            .child(err.to_string()),
                    ),
                    None => this,
                })
        })
    }
}

/// Type for code block actions generator function.
pub(crate) type CodeBlockActionsFn =
    dyn Fn(&CodeBlock, &mut Window, &mut App) -> AnyElement + Send + Sync;

/// Filo: what shows under a finished selection (`TextView::selection_actions`).
type SelectionActionsFn = dyn Fn(SelectedText, &mut Window, &mut App) -> AnyElement;

/// Filo: a text view's selection, handed to its selection actions. Read the text when the action runs: it's what's
/// highlighted, even after the press that began the action cleared the selection's ends.
#[derive(Clone)]
pub struct SelectedText {
    state: Entity<TextViewState>,
}

impl SelectedText {
    /// The selected text, trimmed, as ⌘C copies it.
    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).selection_text().unwrap_or_default().trim().to_string()
    }

    /// Unselects it.
    pub fn clear(&self, cx: &mut App) {
        let parent = self.state.update(cx, |state, _| {
            state.clear_selection();
            state.parent_entity
        });
        if let Some(parent) = parent {
            cx.notify(parent);
        }
    }
}

/// A text view that can render Markdown or HTML.
///
/// ## Goals
///
/// - Provide a rich text rendering component for such as Markdown or HTML,
/// used to display rich text in GPUI application (e.g., Help messages, Release notes)
/// - Support Markdown GFM and HTML (Simple HTML like Safari Reader Mode) for showing most common used markups.
/// - Support Heading, Paragraph, Bold, Italic, StrikeThrough, Code, Link, Image, Blockquote, List, Table, HorizontalRule, CodeBlock ...
///
/// ## Not Goals
///
/// - Customization of the complex style (some simple styles will be supported)
/// - As a Markdown editor or viewer (If you want to like this, you must fork your version).
/// - As a HTML viewer, we not support CSS, we only support basic HTML tags for used to as a content reader.
///
/// See also [`MarkdownElement`], [`HtmlElement`]
#[derive(Clone)]
pub struct TextView {
    id: ElementId,
    init_state: Option<InitState>,
    raw: SharedString,
    state: Entity<TextViewState>,
    style: StyleRefinement,
    selectable: bool,
    scrollable: bool,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
    selection_actions: Option<std::rc::Rc<SelectionActionsFn>>,
}

#[derive(PartialEq)]
pub(crate) struct ParsedContent {
    pub(crate) root_node: node::Node,
    pub(crate) node_cx: node::NodeContext,
}

/// The type of the text view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextViewType {
    /// Markdown view
    Markdown,
    /// HTML view
    Html,
}

enum Update {
    Text(SharedString),
    Style(Box<TextViewStyle>),
}

struct UpdateFuture {
    type_: TextViewType,
    highlight_theme: Arc<HighlightTheme>,
    current_style: TextViewStyle,
    current_text: SharedString,
    timer: Timer,
    rx: Pin<Box<smol::channel::Receiver<Update>>>,
    tx_result: smol::channel::Sender<Result<ParsedContent, SharedString>>,
    delay: Duration,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

impl UpdateFuture {
    #[allow(clippy::too_many_arguments)]
    fn new(
        type_: TextViewType,
        style: TextViewStyle,
        text: SharedString,
        highlight_theme: Arc<HighlightTheme>,
        rx: smol::channel::Receiver<Update>,
        tx_result: smol::channel::Sender<Result<ParsedContent, SharedString>>,
        delay: Duration,
        code_block_actions: Option<Arc<CodeBlockActionsFn>>,
    ) -> Self {
        Self {
            type_,
            highlight_theme,
            current_style: style,
            current_text: text,
            timer: Timer::never(),
            rx: Box::pin(rx),
            tx_result,
            delay,
            code_block_actions,
        }
    }
}

impl Future for UpdateFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.rx.poll_next(cx) {
                Poll::Ready(Some(update)) => {
                    let changed = match update {
                        Update::Text(text) if self.current_text != text => {
                            self.current_text = text;
                            true
                        }
                        Update::Style(style) if self.current_style != *style => {
                            self.current_style = *style;
                            true
                        }
                        _ => false,
                    };
                    if changed {
                        let delay = self.delay;
                        self.timer.set_after(delay);
                    }
                    continue;
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => {}
            }

            match self.timer.poll_next(cx) {
                Poll::Ready(Some(_)) => {
                    let res = parse_content(
                        self.type_,
                        &self.current_text,
                        self.current_style.clone(),
                        &self.highlight_theme,
                        &self.code_block_actions.clone(),
                    );
                    _ = self.tx_result.try_send(res);
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Clone)]
enum InitState {
    Initializing {
        type_: TextViewType,
        text: SharedString,
        style: Box<TextViewStyle>,
        highlight_theme: Arc<HighlightTheme>,
    },
    Initialized {
        tx: smol::channel::Sender<Update>,
    },
}

pub(crate) struct TextViewState {
    parent_entity: Option<EntityId>,
    tx: Option<smol::channel::Sender<Update>>,
    parsed_result: Option<Result<ParsedContent, SharedString>>,
    focus_handle: Option<FocusHandle>,
    /// The bounds of the text view
    bounds: Bounds<Pixels>,
    /// The local (in TextView) position of the selection.
    selection_positions: (Option<Point<Pixels>>, Option<Point<Pixels>>),
    /// Is current in selection.
    is_selecting: bool,
    is_selectable: bool,
    /// Filo: the selection actions were rendered last frame.
    actions_shown: bool,
    /// Filo: where the selection actions were drawn last frame. It hides what's under them from the mouse, so a press
    /// on them doesn't also click a row or start a selection there, and isn't a press outside that clears the selection.
    actions_hitbox: Option<Hitbox>,
    /// Filo: the menu a right-click on a link opened (`open_link_menu`), until it's dismissed.
    link_menu: Option<LinkMenu>,
    list_state: ListState,
}

/// Filo: a link's right-click menu, where the pointer was (in the window).
struct LinkMenu {
    position: Point<Pixels>,
    menu: Entity<PopupMenu>,
    _dismissed: Subscription,
}

/// Filo: opens the menu for a right-click on the link to `url` at `position`: only "Copy link", which puts the whole
/// address (not the link's words) on the clipboard and says "Copied", as the app's toast (`ui::toast`) does for a
/// message's Copy button.
pub(crate) fn open_link_menu(state: &Entity<TextViewState>, url: SharedString, position: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let menu = PopupMenu::build(window, cx, move |menu, _, _| {
        menu.item(PopupMenuItem::new("Copy link").on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
            window.push_notification(Notification::new().message("Copied"), cx);
        }))
    });
    // The one-open-menu rule (one_menu.rs): the menu open before closes; this one closes when another opens.
    let key = MenuKey::of(state.entity_id());
    let _dismissed = window.subscribe(&menu, cx, {
        let state = state.downgrade();
        move |_, _: &DismissEvent, window, cx| {
            state.update(cx, |state, _| state.link_menu = None).ok();
            one_menu::closed(key, cx);
            window.refresh();
        }
    });
    state.update(cx, |state, _| state.link_menu = Some(LinkMenu { position, menu, _dismissed }));
    let weak = state.downgrade();
    one_menu::opened(key, window, cx, move |_, cx| {
        weak.update(cx, |state, _| state.link_menu = None).ok();
    });
    window.refresh();
}

impl TextViewState {
    fn new(cx: &mut Context<TextViewState>) -> Self {
        let focus_handle = cx.focus_handle();
        Self {
            parent_entity: None,
            tx: None,
            parsed_result: None,
            focus_handle: Some(focus_handle),
            bounds: Bounds::default(),
            selection_positions: (None, None),
            is_selecting: false,
            is_selectable: false,
            actions_shown: false,
            actions_hitbox: None,
            link_menu: None,
            list_state: ListState::new(0, gpui::ListAlignment::Top, px(1000.)),
        }
    }
}

impl TextViewState {
    /// Save bounds and unselect if bounds changed.
    fn update_bounds(&mut self, bounds: Bounds<Pixels>) {
        if self.bounds.size != bounds.size {
            self.clear_selection();
        }
        self.bounds = bounds;
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection_positions = (None, None);
        self.is_selecting = false;
    }

    /// Filo: selects from `start` to `end` (window points), as a double-click on a word does.
    pub(crate) fn select_between(&mut self, start: Point<Pixels>, end: Point<Pixels>) {
        self.selection_positions = (Some(start - self.bounds.origin), Some(end - self.bounds.origin));
        self.is_selecting = false;
    }

    fn start_selection(&mut self, pos: Point<Pixels>) {
        let pos = pos - self.bounds.origin;
        self.selection_positions = (Some(pos), Some(pos));
        self.is_selecting = true;
    }

    fn update_selection(&mut self, pos: Point<Pixels>) {
        let pos = pos - self.bounds.origin;
        if let (Some(start), Some(_)) = self.selection_positions {
            self.selection_positions = (Some(start), Some(pos))
        }
    }

    fn end_selection(&mut self) {
        self.is_selecting = false;
        // Filo: a press and release a few points apart is a click, not a selection of a letter or two.
        if let (Some(start), Some(end)) = self.selection_positions {
            if (start.x - end.x).abs().max((start.y - end.y).abs()) <= px(3.) {
                self.selection_positions = (None, None);
            }
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        if let (Some(start), Some(end)) = self.selection_positions {
            start != end
        } else {
            false
        }
    }

    pub(crate) fn is_selectable(&self) -> bool {
        self.is_selectable
    }

    /// Return the bounds of the selection in window coordinates.
    #[allow(unused)]
    pub(crate) fn selection_bounds(&self) -> Bounds<Pixels> {
        selection_bounds(
            self.selection_positions.0,
            self.selection_positions.1,
            self.bounds,
        )
    }

    /// Filo: the selection's two ends in window coordinates, where the drag began and where it is (or ended).
    pub(crate) fn selection_points(&self) -> Option<(Point<Pixels>, Point<Pixels>)> {
        match self.selection_positions {
            (Some(start), Some(end)) if start != end => Some((start + self.bounds.origin, end + self.bounds.origin)),
            _ => None,
        }
    }

    fn selection_text(&self) -> Option<String> {
        Some(
            self.parsed_result
                .as_ref()?
                .as_ref()
                .ok()?
                .root_node
                .selected_text(),
        )
    }
}

#[derive(IntoElement, Clone)]
pub enum Text {
    String(SharedString),
    TextView(Box<TextView>),
}

impl From<SharedString> for Text {
    fn from(s: SharedString) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self::String(SharedString::from(s.to_string()))
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Self::String(s.into())
    }
}

impl From<TextView> for Text {
    fn from(e: TextView) -> Self {
        Self::TextView(Box::new(e))
    }
}

impl Text {
    /// Set the style for [`TextView`].
    ///
    /// Do nothing if this is `String`.
    pub fn style(self, style: TextViewStyle) -> Self {
        match self {
            Self::String(s) => Self::String(s),
            Self::TextView(e) => Self::TextView(Box::new(e.style(style))),
        }
    }

    /// Get the str
    pub fn as_str(&self) -> &str {
        match self {
            Self::String(s) => s.as_str(),
            Self::TextView(view) => view.raw.as_str(),
        }
    }
}

impl RenderOnce for Text {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        match self {
            Self::String(s) => s.into_any_element(),
            Self::TextView(e) => e.into_any_element(),
        }
    }
}

impl Styled for TextView {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl TextView {
    fn create_init_state(
        type_: TextViewType,
        text: &SharedString,
        highlight_theme: &Arc<HighlightTheme>,
        state: &Entity<TextViewState>,
        cx: &mut App,
    ) -> InitState {
        let state = state.read(cx);
        if let Some(tx) = &state.tx {
            InitState::Initialized { tx: tx.clone() }
        } else {
            InitState::Initializing {
                type_,
                text: text.clone(),
                style: Default::default(),
                highlight_theme: highlight_theme.clone(),
            }
        }
    }

    /// Create a new markdown text view.
    pub fn markdown(
        id: impl Into<ElementId>,
        markdown: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let markdown = markdown.into();
        let highlight_theme = cx.theme().highlight_theme.clone();
        let state =
            window.use_keyed_state(SharedString::from(format!("{}/state", id)), cx, |_, cx| {
                TextViewState::new(cx)
            });
        let init_state = Self::create_init_state(
            TextViewType::Markdown,
            &markdown,
            &highlight_theme,
            &state,
            cx,
        );
        if let Some(tx) = &state.read(cx).tx {
            let _ = tx.try_send(Update::Text(markdown.clone()));
        }
        Self {
            id,
            init_state: Some(init_state),
            raw: markdown.clone(),
            style: StyleRefinement::default(),
            state,
            selectable: false,
            scrollable: false,
            code_block_actions: None,
            selection_actions: None,
        }
    }

    /// Create a new html text view.
    pub fn html(
        id: impl Into<ElementId>,
        html: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let html = html.into();
        let highlight_theme = cx.theme().highlight_theme.clone();
        let state =
            window.use_keyed_state(SharedString::from(format!("{}/state", id)), cx, |_, cx| {
                TextViewState::new(cx)
            });
        let init_state =
            Self::create_init_state(TextViewType::Html, &html, &highlight_theme, &state, cx);
        if let Some(tx) = &state.read(cx).tx {
            let _ = tx.try_send(Update::Text(html.clone()));
        }
        Self {
            id,
            init_state: Some(init_state),
            style: StyleRefinement::default(),
            state,
            raw: html,
            selectable: false,
            scrollable: false,
            code_block_actions: None,
            selection_actions: None,
        }
    }

    /// Set the source text of the text view.
    pub fn text(mut self, raw: impl Into<SharedString>) -> Self {
        let raw: SharedString = raw.into();
        if let Some(init_state) = &mut self.init_state {
            match init_state {
                InitState::Initializing { text, .. } => *text = raw.clone(),
                InitState::Initialized { tx } => {
                    let _ = tx.try_send(Update::Text(raw.clone()));
                }
            }
        }
        self.raw = raw;
        self
    }

    /// Set [`TextViewStyle`].
    pub fn style(mut self, style: TextViewStyle) -> Self {
        if let Some(init_state) = &mut self.init_state {
            match init_state {
                InitState::Initializing { style: s, .. } => **s = style,
                InitState::Initialized { tx } => {
                    let _ = tx.try_send(Update::Style(Box::new(style)));
                }
            }
        }
        self
    }

    /// Set the text view to be selectable, default is false.
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Set the text view to be scrollable, default is false.
    ///
    /// ## If true for `scrollable`
    ///
    /// The `scrollable` mode used for large content,
    /// will show scrollbar, but requires the parent to have a fixed height,
    /// and use [`gpui::list`] to render the content in a virtualized way.
    ///
    /// ## If false to fit content
    ///
    /// The TextView will expand to fit all content, no scrollbar.
    /// This mode is suitable for small content, such as a few lines of text, a label, etc.
    pub fn scrollable(mut self, scrollable: bool) -> Self {
        self.scrollable = scrollable;
        self
    }

    fn on_action_copy(state: &Entity<TextViewState>, cx: &mut App) {
        let Some(selected_text) = state.read(cx).selection_text() else {
            return;
        };

        cx.write_to_clipboard(ClipboardItem::new_string(selected_text.trim().to_string()));
    }

    /// Filo: shows `f`'s element under the selection's end once a selection is made (by dragging or a double-click),
    /// until it's cleared: the chat's Quote button. It sits above the text and takes its clicks; clicking it leaves the
    /// selection to the action, which reads it with [`SelectedText::text`] and can [`SelectedText::clear`] it.
    pub fn selection_actions<F, E>(mut self, f: F) -> Self
    where
        F: Fn(SelectedText, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.selection_actions = Some(std::rc::Rc::new(move |selection, window, cx| {
            f(selection, window, cx).into_any_element()
        }));
        self
    }

    /// Set custom block actions for code blocks.
    ///
    /// The closure receives the [`CodeBlock`],
    /// and returns an element to display.
    pub fn code_block_actions<F, E>(mut self, f: F) -> Self
    where
        F: Fn(&CodeBlock, &mut Window, &mut App) -> E + Send + Sync + 'static,
        E: IntoElement,
    {
        self.code_block_actions = Some(Arc::new(move |code_block, window, cx| {
            f(&code_block, window, cx).into_any_element()
        }));
        self
    }
}

impl TextView {
    /// Filo: the selection actions, a little below the selection's end (the later one in reading order), drawn above
    /// everything after them.
    fn render_selection_actions(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let actions = self.selection_actions.clone()?;
        let shown = self.wants_selection_actions(cx);
        self.state.update(cx, |state, _| {
            state.actions_shown = shown;
            // Set again when they're drawn: a hitbox is only good for the frame it was made in.
            state.actions_hitbox = None;
        });
        let (Some(a), Some(b)) = self.state.read(cx).selection_positions else { return None };
        if !shown {
            return None;
        }
        let line_height = window.line_height();
        let end = if (a.y - b.y).abs() < line_height * 0.5 { if a.x >= b.x { a } else { b } } else if a.y > b.y { a } else { b };
        let element = actions(SelectedText { state: self.state.clone() }, window, cx);
        // Inserted before the actions' own hitboxes, so it hides only what's under them, wherever they're drawn (under
        // the selection, or over it when snapped up from the window's bottom). Scrolling still reaches the chat.
        let state = self.state.clone();
        let blocker = canvas(
            move |bounds, window, cx| {
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::BlockMouseExceptScroll);
                state.update(cx, |state, _| state.actions_hitbox = Some(hitbox));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let element = div().relative().child(blocker).child(element);
        Some(
            div()
                .absolute()
                .left(end.x)
                .top(end.y + line_height * 0.6)
                .child(deferred(anchored().snap_to_window_with_margin(px(8.)).child(element)).with_priority(2))
                .into_any_element(),
        )
    }
}

impl TextView {
    /// Filo: the link menu (`open_link_menu`) at the pointer, over everything, as `ContextMenu` draws one.
    fn render_link_menu(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let (position, menu) = self.state.read(cx).link_menu.as_ref().map(|link| (link.position, link.menu.clone()))?;
        if !menu.focus_handle(cx).contains_focused(window, cx) {
            menu.focus_handle(cx).focus(window);
        }
        let screen = window.bounds().size;
        Some(
            deferred(
                anchored().child(
                    div()
                        .w(screen.width)
                        .h(screen.height)
                        .occlude()
                        // A right-click elsewhere closes this menu and opens the one there (one_menu.rs).
                        .on_mouse_down(MouseButton::Right, |event, window, cx| one_menu::pass_through(event.position, window, cx))
                        .child(anchored().position(position).snap_to_window_with_margin(px(8.)).child(menu)),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Filo: whether the selection actions show: a finished selection with words in it.
    fn wants_selection_actions(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        self.selectable
            && !state.is_selecting
            && state.has_selection()
            && state.selection_text().is_some_and(|text| !text.trim().is_empty())
    }
}

impl IntoElement for TextView {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextView {
    type RequestLayoutState = AnyElement;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        if let Some(InitState::Initializing {
            type_,
            text,
            style,
            highlight_theme,
        }) = self.init_state.take()
        {
            let style = *style;
            let highlight_theme = highlight_theme.clone();
            let code_block_actions = self.code_block_actions.clone();
            let (tx, rx) = smol::channel::unbounded::<Update>();
            let (tx_result, rx_result) =
                smol::channel::unbounded::<Result<ParsedContent, SharedString>>();
            let parsed_result = parse_content(
                type_,
                &text,
                style.clone(),
                &highlight_theme,
                &code_block_actions,
            );

            self.state.update(cx, {
                let tx = tx.clone();
                |state, _| {
                    state.parsed_result = Some(parsed_result);
                    state.tx = Some(tx);
                }
            });

            cx.spawn({
                let state = self.state.downgrade();
                async move |cx| {
                    while let Ok(parsed_result) = rx_result.recv().await {
                        if let Some(state) = state.upgrade() {
                            _ = state.update(cx, |state, cx| {
                                state.parsed_result = Some(parsed_result);
                                if let Some(parent_entity) = state.parent_entity {
                                    let app = &mut **cx;
                                    app.notify(parent_entity);
                                }
                                state.clear_selection();
                            });
                        } else {
                            // state released, stopping processing
                            break;
                        }
                    }
                }
            })
            .detach();

            cx.background_spawn(UpdateFuture::new(
                type_,
                style,
                text,
                highlight_theme,
                rx,
                tx_result,
                Duration::from_millis(200),
                code_block_actions,
            ))
            .detach();

            self.init_state = Some(InitState::Initialized { tx });
        }

        let selection_actions = self.render_selection_actions(window, cx);
        let link_menu = self.render_link_menu(window, cx);
        let list_state = &self.state.read(cx).list_state;

        let focus_handle = self
            .state
            .read(cx)
            .focus_handle
            .as_ref()
            .expect("focus_handle should init by TextViewState::new");

        let mut el = div()
            .key_context(CONTEXT)
            .track_focus(focus_handle)
            .size_full()
            .relative()
            .on_action({
                let state = self.state.clone();
                move |_: &input::Copy, _, cx| {
                    Self::on_action_copy(&state, cx);
                }
            })
            .child(TextViewElement {
                list_state: if self.scrollable {
                    Some(list_state.clone())
                } else {
                    None
                },
                state: self.state.clone(),
            })
            .children(selection_actions)
            .children(link_menu)
            .refine_style(&self.style)
            .vertical_scrollbar(list_state)
            .into_any_element();
        let layout_id = el.request_layout(window, cx);
        (layout_id, el)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        request_layout.prepaint(window, cx);
        // Filo: presses count only where the view shows (not under a popover, a menu or its own selection actions).
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let entity_id = window.current_view();
        let is_selectable = self.selectable;

        self.state.update(cx, |state, _| {
            state.parent_entity = Some(entity_id);
            state.update_bounds(bounds);
            state.is_selectable = is_selectable;
        });

        GlobalState::global_mut(cx)
            .text_view_state_stack
            .push(self.state.clone());
        request_layout.paint(window, cx);
        GlobalState::global_mut(cx).text_view_state_stack.pop();

        // Filo: the text shows what's selected only once painted (a double-click selects in the press, after this
        // frame's layout), so the selection actions come or go on the next frame.
        if self.selection_actions.is_some() && self.wants_selection_actions(cx) != self.state.read(cx).actions_shown {
            window.request_animation_frame();
        }

        if self.selectable {
            let is_selecting = self.state.read(cx).is_selecting;
            let has_selection = self.state.read(cx).has_selection();

            window.on_mouse_event({
                let state = self.state.clone();
                let hitbox = hitbox.clone();
                // Filo: only the left button selects: a right-click keeps the selection. Shift extends it.
                move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.bubble() || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                        return;
                    }

                    state.update(cx, |state, _| {
                        if event.modifiers.shift && state.selection_positions.0.is_some() {
                            state.is_selecting = true;
                            state.update_selection(event.position);
                        } else {
                            state.start_selection(event.position);
                        }
                    });
                    cx.notify(entity_id);
                }
            });

            if is_selecting {
                // move to update end position.
                // Filo: only while the left button is held. A drag whose release never reached this view kept
                // selecting as the pointer moved with no button down; the first such move ends it where it was.
                window.on_mouse_event({
                    let state = self.state.clone();
                    move |event: &MouseMoveEvent, phase, _, cx| {
                        if !phase.capture() || !state.read(cx).is_selecting {
                            return;
                        }

                        state.update(cx, |state, _| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                state.update_selection(event.position);
                            } else {
                                state.end_selection();
                            }
                        });
                        cx.notify(entity_id);
                    }
                });
            }

            // up to end selection
            // Filo: in the capture phase, so nothing drawn over the text can stop it on the way, and whether or not a
            // frame has drawn since the press: a release that came before the next frame had no listener to end it.
            // The selection ends where the button came up, for a drag whose moves all came before that frame too.
            window.on_mouse_event({
                let state = self.state.clone();
                move |event: &MouseUpEvent, phase, _, cx| {
                    if !phase.capture() || event.button != MouseButton::Left || !state.read(cx).is_selecting {
                        return;
                    }

                    state.update(cx, |state, _| {
                        state.update_selection(event.position);
                        state.end_selection();
                    });
                    cx.notify(entity_id);
                }
            });

            if has_selection {
                // down outside to clear selection
                window.on_mouse_event({
                    let state = self.state.clone();
                    let hitbox = hitbox.clone();
                    move |_: &MouseDownEvent, phase, window, cx| {
                        // Filo: outside is wherever the view doesn't show, except its selection actions.
                        if !phase.capture() || hitbox.is_hovered(window) {
                            return;
                        }
                        if state.read(cx).actions_hitbox.as_ref().is_some_and(|actions| actions.is_hovered(window)) {
                            return;
                        }

                        state.update(cx, |state, _| {
                            state.clear_selection();
                        });
                        cx.notify(entity_id);
                    }
                });
            }
        }
    }
}

fn parse_content(
    type_: TextViewType,
    text: &str,
    style: TextViewStyle,
    highlight_theme: &HighlightTheme,
    code_block_actions: &Option<Arc<CodeBlockActionsFn>>,
) -> Result<ParsedContent, SharedString> {
    let mut node_cx = NodeContext {
        style: style.clone(),
        code_block_actions: code_block_actions.clone(),
        ..NodeContext::default()
    };

    let res = match type_ {
        TextViewType::Markdown => {
            super::format::markdown::parse(text, &style, &mut node_cx, highlight_theme)
        }
        TextViewType::Html => super::format::html::parse(text, &mut node_cx),
    };
    res.map(move |root_node| ParsedContent { root_node, node_cx })
}

fn selection_bounds(
    start: Option<Point<Pixels>>,
    end: Option<Point<Pixels>>,
    bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    if let (Some(start), Some(end)) = (start, end) {
        let start = start + bounds.origin;
        let end = end + bounds.origin;

        let origin = Point {
            x: start.x.min(end.x),
            y: start.y.min(end.y),
        };
        let size = Size {
            width: (start.x - end.x).abs(),
            height: (start.y - end.y).abs(),
        };

        return Bounds { origin, size };
    }

    Bounds::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, point, px, size};

    #[test]
    fn test_text_view_state_selection_bounds() {
        assert_eq!(
            selection_bounds(None, None, Default::default()),
            Bounds::default()
        );
        assert_eq!(
            selection_bounds(None, Some(point(px(10.), px(20.))), Default::default()),
            Bounds::default()
        );
        assert_eq!(
            selection_bounds(Some(point(px(10.), px(20.))), None, Default::default()),
            Bounds::default()
        );

        // 10,10 start
        //   |------|
        //   |      |
        //   |------|
        //         50,50
        assert_eq!(
            selection_bounds(
                Some(point(px(10.), px(10.))),
                Some(point(px(50.), px(50.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        // 10,10
        //   |------|
        //   |      |
        //   |------|
        //         50,50 start
        assert_eq!(
            selection_bounds(
                Some(point(px(50.), px(50.))),
                Some(point(px(10.), px(10.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        //        50,10 start
        //   |------|
        //   |      |
        //   |------|
        // 10,50
        assert_eq!(
            selection_bounds(
                Some(point(px(50.), px(10.))),
                Some(point(px(10.), px(50.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        //        50,10
        //   |------|
        //   |      |
        //   |------|
        // 10,50 start
        assert_eq!(
            selection_bounds(
                Some(point(px(10.), px(50.))),
                Some(point(px(50.), px(10.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
    }
}

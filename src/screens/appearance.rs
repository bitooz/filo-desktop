// Appearance: a live preview, mode, contrast and theme family, stored on this Mac (config::Prefs). There's no system
// contrast setting to follow, so contrast "Auto" means standard. Typography: the interface and monospace fonts and
// their sizes.

use gpui::{
    AnyElement, App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px, relative,
};

use std::collections::HashSet;
use std::rc::Rc;

use gpui::Corner;
use gpui_component::Sizable as _;
use gpui_component::button::Button as MenuButton;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

use crate::config::{self, Prefs};
use crate::theme::{self, ActiveTheme as _, Family, Palette, Typography as _, radius, space};
use crate::ui;

/// (font name, label). The first entries are always offered: Atkinson ships with
/// the app and `.SystemUIFont` is macOS's SF Pro; the rest only if installed.
const INTERFACE_FONTS: [(&str, &str); 9] = [
    (config::DEFAULT_FONT, "Atkinson Hyperlegible"),
    (".SystemUIFont", "SF Pro"),
    ("Helvetica Neue", "Helvetica Neue"),
    ("Avenir Next", "Avenir Next"),
    ("Inter", "Inter"),
    ("Roboto", "Roboto"),
    ("Open Sans", "Open Sans"),
    ("Lato", "Lato"),
    ("Georgia", "Georgia"),
];
const ALWAYS_INTERFACE: usize = 2;
const MONO_FONTS: [(&str, &str); 7] = [
    (config::DEFAULT_MONO, "Menlo"),
    ("SF Mono", "SF Mono"),
    ("Monaco", "Monaco"),
    ("JetBrains Mono", "JetBrains Mono"),
    ("Fira Code", "Fira Code"),
    ("Cascadia Code", "Cascadia Code"),
    ("Courier New", "Courier New"),
];
const ALWAYS_MONO: usize = 1;

pub struct AppearanceScreen {
    installed: HashSet<String>,
}

impl AppearanceScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        AppearanceScreen { installed: cx.text_system().all_font_names().into_iter().collect() }
    }

    fn options(&self, fonts: &[(&str, &str)], always: usize) -> Vec<(String, String)> {
        fonts
            .iter()
            .enumerate()
            .filter(|(index, (name, _))| *index < always || self.installed.contains(*name))
            .map(|(_, (name, label))| (name.to_string(), label.to_string()))
            .collect()
    }
}

type Pick = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// A select-style button with a checked list (fonts, sizes).
fn dropdown(
    id: &str,
    current: String,
    width: f32,
    options: Vec<(String, String)>,
    selected: String,
    pick: Pick,
) -> impl IntoElement {
    MenuButton::new(SharedString::from(id.to_string()))
        .outline()
        .small()
        .label(current)
        .dropdown_caret(true)
        .w(px(width))
        .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
            options.iter().fold(menu, |menu, (value, label)| {
                let (pick, value) = (pick.clone(), value.clone());
                menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(value == selected)
                        .on_click(move |_, window, cx| pick(value.clone(), window, cx)),
                )
            })
        })
}

fn sizes(range: std::ops::RangeInclusive<u32>) -> Vec<(String, String)> {
    range.map(|size| (size.to_string(), format!("{size} px"))).collect()
}

/// One font setting: name and description, the font and size pickers, a preview below.
fn font_row(title: &str, description: &str, pickers: [AnyElement; 2], preview: AnyElement, cx: &App) -> Div {
    let c = *cx.colors();
    let [font, size] = pickers;
    div()
        .p(px(space::MD + 2.))
        .flex()
        .flex_col()
        .gap(px(space::MD))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().callout().medium().child(title.to_string()))
                        .child(div().footnote().text_color(c.text_secondary).child(description.to_string())),
                )
                .child(font)
                .child(size),
        )
        .child(preview)
}

fn update_prefs(window: &mut Window, cx: &mut App, change: impl FnOnce(&mut Prefs)) {
    let mut prefs = cx.theme().prefs.clone();
    change(&mut prefs);
    theme::save_and_apply(prefs, window, cx);
}

/// A titled group without the card (Section `plain`).
fn plain_section(title: &str, content: impl IntoElement, cx: &App) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM))
        .child(
            div()
                .px(px(space::SM))
                .subhead()
                .semibold()
                .text_color(cx.colors().text_secondary)
                .child(title.to_string()),
        )
        .child(content)
}

/// The active theme on a slice of the app: reminders, a chat exchange, and the two button kinds.
fn preview(cx: &App) -> Div {
    let c = *cx.colors();
    let ring = |done: bool| {
        div()
            .mt(px(1.))
            .size(px(26.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(13.))
            .border_2()
            .border_color(if done { c.accent } else { c.text_secondary })
            .when(done, |this| this.bg(c.accent).child(ui::icon_sized("check", 14., c.on_accent)))
    };
    let reminder = |done: bool, title: &str, meta: &str, meta_color| {
        div()
            .flex()
            .items_start()
            .gap(px(space::MD))
            .pb(px(space::MD))
            .border_b_1()
            .border_color(c.border)
            .child(ring(done))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .child(div().body().bold().when(done, |this| this.opacity(0.55)).child(title.to_string()))
                    .child(div().caption().text_color(meta_color).child(meta.to_string())),
            )
    };
    let button = |label: &str, bg, fg, border: bool| {
        div()
            .flex_1()
            .h(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(radius::FULL))
            .bg(bg)
            .when(border, |this| this.border_1().border_color(c.border))
            .footnote()
            .semibold()
            .text_color(fg)
            .child(label.to_string())
    };
    // Pixel widths for the bubbles: a percentage width doesn't resolve for wrapped text in this column, so the
    // bubbles were laid out a letter wide and their lines ran over everything below.
    let inner = ui::column_width() - 2. * space::LG - 2.;
    div()
        .flex()
        .flex_col()
        .gap(px(space::MD))
        .p(px(space::LG))
        .rounded(px(radius::XL))
        .bg(c.background)
        .border_1()
        .border_color(c.border)
        .child(reminder(true, "Take your meds", "Done · 9:00", c.text_secondary))
        .child(reminder(false, "Call the dentist", "Overdue · 2:30 PM", c.danger))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .child(
                    div().flex().justify_end().child(
                        div()
                            .max_w(px((inner * 0.8).round()))
                            .px(px(14.))
                            .py(px(10.))
                            .rounded(px(20.))
                            .bg(c.user_bubble)
                            .callout()
                            .text_color(c.on_user_bubble)
                            .child("Nudge me again at 5?"),
                    ),
                )
                .child(
                    div().flex().child(
                        div()
                            .max_w(px((inner * 0.86).round()))
                            .px(px(14.))
                            .py(px(9.))
                            .rounded(px(20.))
                            .bg(c.assistant_bubble)
                            .border_1()
                            .border_color(c.border)
                            .callout()
                            .text_color(c.on_assistant_bubble)
                            .child("Done. I’ll remind you at 5:00."),
                    ),
                ),
        )
        .child(
            div()
                .flex()
                .gap(px(space::SM))
                .child(button("Save", c.action, c.on_action, false))
                .child(button("Snooze", c.surface, c.text, true)),
        )
}

/// A miniature of one scheme: a card with its done check, the user's bubble, and the action button.
/// One half of the swatch. GPUI clips to rectangles, so each half rounds its own
/// outer corners to sit inside the frame's radius.
fn scheme_swatch(p: Palette, left: bool) -> Div {
    let corner = px(radius::SM - 1.);
    div()
        .map(|this| if left { this.rounded_l(corner) } else { this.rounded_r(corner) })
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(3.))
        .px(px(5.))
        .bg(p.background)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .p(px(3.))
                .rounded(px(3.))
                .bg(p.surface)
                .border_1()
                .border_color(p.border)
                .child(div().size(px(6.)).flex_none().rounded(px(radius::FULL)).bg(p.accent))
                .child(div().flex_1().h(px(3.)).rounded(px(1.)).bg(p.text)),
        )
        .child(div().flex().justify_end().child(div().w(relative(0.72)).h(px(7.)).rounded(px(3.)).bg(p.user_bubble)))
        .child(
            div().flex().justify_end().child(div().w(relative(0.46)).h(px(7.)).rounded(px(radius::FULL)).bg(p.action)),
        )
}

fn family_row(family: &'static Family, selected: bool, high: bool, cx: &App) -> AnyElement {
    let c = *cx.colors();
    let name = family.name.clone();
    div()
        .id(SharedString::from(format!("theme-{}", family.name)))
        .relative()
        .min_h(px(84.))
        .p(px(space::MD))
        .flex()
        .items_center()
        .gap(px(space::MD))
        .rounded(px(radius::LG))
        .bg(c.surface)
        .border_1()
        .border_color(c.border)
        .cursor_pointer()
        .hover(|style| style.bg(c.surface_strong))
        .on_click(move |_, window, cx| {
            let name = name.clone();
            update_prefs(window, cx, |prefs| prefs.theme_family = name);
        })
        .child(
            div()
                .w(px(88.))
                .h(px(60.))
                .flex_none()
                .flex()
                .overflow_hidden()
                .rounded(px(radius::SM))
                .border_1()
                .border_color(c.border)
                .child(scheme_swatch(family.palette(false, high), true))
                .child(scheme_swatch(family.palette(true, high), false)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .child(div().headline().truncate().child(family.label.clone()))
                .child(div().subhead().text_color(c.text_secondary).child(family.description.clone())),
        )
        .child(
            div()
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::FULL))
                .border_2()
                .map(|this| {
                    if selected { this.bg(c.accent).border_color(c.accent) } else { this.border_color(c.border_strong) }
                })
                .when(selected, |this| this.child(ui::icon_sized("check", 14., c.on_accent))),
        )
        // An overlay ring, so selecting never shifts the row's layout.
        .when(selected, |this| {
            this.child(
                div()
                    .absolute()
                    .top(px(-1.))
                    .left(px(-1.))
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .rounded(px(radius::LG))
                    .border_2()
                    .border_color(c.accent),
            )
        })
        .into_any_element()
}

impl Render for AppearanceScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let prefs = theme.prefs.clone();
        let high = theme.high_contrast();
        let selected_family = theme.family().name.clone();

        let mode = ui::segmented(
            "appearance-mode",
            vec![
                ("system".to_string(), "Auto".into(), Some("sun-moon")),
                ("light".to_string(), "Light".into(), Some("sun")),
                ("dark".to_string(), "Dark".into(), Some("moon")),
            ],
            &prefs.theme_mode,
            |value, window, cx| update_prefs(window, cx, |prefs| prefs.theme_mode = value),
            cx,
        );
        let contrast = ui::segmented(
            "appearance-contrast",
            vec![
                ("system".to_string(), "Auto".into(), Some("smartphone")),
                ("standard".to_string(), "Standard".into(), Some("circle")),
                ("high".to_string(), "High".into(), Some("contrast")),
            ],
            &prefs.theme_contrast,
            |value, window, cx| update_prefs(window, cx, |prefs| prefs.theme_contrast = value),
            cx,
        );
        let c = *cx.colors();
        // Text in this column needs a pixel width to wrap (see feed_post).
        let preview_width = ui::column_width() - 2. * (space::MD + 2.) - 2. * space::MD - 2.;
        let interface_options = self.options(&INTERFACE_FONTS, ALWAYS_INTERFACE);
        let mono_options = self.options(&MONO_FONTS, ALWAYS_MONO);
        let label_of = |options: &[(String, String)], name: &str| {
            options.iter().find(|(value, _)| value == name).map(|(_, label)| label.clone()).unwrap_or(name.to_string())
        };
        let interface = font_row(
            "Interface font",
            "Everything outside code blocks.",
            [
                dropdown(
                    "appearance-font",
                    label_of(&interface_options, &prefs.font_family),
                    190.,
                    interface_options.clone(),
                    prefs.font_family.clone(),
                    Rc::new(|value, window, cx| update_prefs(window, cx, |prefs| prefs.font_family = value)),
                )
                .into_any_element(),
                dropdown(
                    "appearance-font-size",
                    format!("{} px", prefs.font_size.round() as u32),
                    90.,
                    sizes(12..=20),
                    (prefs.font_size.round() as u32).to_string(),
                    Rc::new(|value, window, cx| {
                        let size = value.parse().unwrap_or(config::DEFAULT_FONT_SIZE);
                        update_prefs(window, cx, |prefs| prefs.font_size = size)
                    }),
                )
                .into_any_element(),
            ],
            div()
                .p(px(space::MD))
                .rounded(px(radius::SM))
                .border_1()
                .border_color(c.border)
                .child(
                    div()
                        .w(px(preview_width))
                        .body()
                        .child("Remind me to call Mom at 6, then save the grocery list to Notes. If I snooze it twice, ask me again at 7."),
                )
                .into_any_element(),
            cx,
        );
        let mono_size = prefs.mono_size;
        let mono = font_row(
            "Monospace font",
            "Code blocks and artifact source.",
            [
                dropdown(
                    "appearance-mono",
                    label_of(&mono_options, &prefs.mono_family),
                    190.,
                    mono_options.clone(),
                    prefs.mono_family.clone(),
                    Rc::new(|value, window, cx| update_prefs(window, cx, |prefs| prefs.mono_family = value)),
                )
                .into_any_element(),
                dropdown(
                    "appearance-mono-size",
                    format!("{} px", mono_size.round() as u32),
                    90.,
                    sizes(10..=18),
                    (mono_size.round() as u32).to_string(),
                    Rc::new(|value, window, cx| {
                        let size = value.parse().unwrap_or(config::DEFAULT_MONO_SIZE);
                        update_prefs(window, cx, |prefs| prefs.mono_size = size)
                    }),
                )
                .into_any_element(),
            ],
            div()
                .p(px(space::MD))
                .rounded(px(radius::SM))
                .bg(c.sunken)
                .font_family(SharedString::from(prefs.mono_family.clone()))
                .text_size(px(mono_size))
                .line_height(px((mono_size * 1.55).round()))
                .children(
                    [
                        ("export function remind(task: Task) {", c.text),
                        ("  return filo.schedule(task, { at: \"18:00\" });", c.text),
                        ("} // 0O 1lI", c.text_tertiary),
                    ]
                    .map(|(line, color)| div().text_color(color).child(line)),
                )
                .into_any_element(),
            cx,
        );
        let is_default = prefs.font_family == config::DEFAULT_FONT
            && prefs.font_size == config::DEFAULT_FONT_SIZE
            && prefs.mono_family == config::DEFAULT_MONO
            && prefs.mono_size == config::DEFAULT_MONO_SIZE;
        let typography = div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(radius::MD))
                    .bg(c.surface)
                    .child(interface)
                    .child(div().mx(px(space::MD + 2.)).h(px(1.)).bg(c.border.opacity(0.6)))
                    .child(mono),
            )
            .when(!is_default, |this| {
                this.child(div().flex().justify_end().child(
                    ui::Button::ghost("appearance-font-reset", "Reset to defaults").small().on_click(
                        |_, window, cx| {
                            update_prefs(window, cx, |prefs| {
                                prefs.font_family = config::DEFAULT_FONT.into();
                                prefs.font_size = config::DEFAULT_FONT_SIZE;
                                prefs.mono_family = config::DEFAULT_MONO.into();
                                prefs.mono_size = config::DEFAULT_MONO_SIZE;
                            })
                        },
                    ),
                ))
            });
        let families = div().flex().flex_col().gap(px(space::SM)).children(
            theme::families().iter().map(|family| family_row(family, family.name == selected_family, high, cx)),
        );

        ui::sub_page(
            ui::sub_page_header("appearance", "Appearance", vec![], cx),
            div().id("appearance-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                ui::page_column()
                    .pt(px(space::XXL))
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .child(preview(cx))
                    .child(plain_section("Mode", mode, cx))
                    .child(plain_section("Contrast", contrast, cx))
                    .child(plain_section("Typography", typography, cx))
                    .child(plain_section("Theme", families, cx)),
            ),
            cx,
        )
    }
}

// The app's design tokens (src/theme): the five theme families in light and
// dark at standard and high contrast, resolved by the phone app's theme engine
// into assets/themes.json (scripts/dump-themes.ts, run in the phone app's code;
// here the file is kept as it was made), plus spacing, radii and the Atkinson
// Hyperlegible Next type scale.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use gpui::{App, FontWeight, Global, Hsla, Rgba, SharedString, Styled, Window, WindowAppearance, px};
use serde::Deserialize;

use crate::config::{self, Prefs};

use std::sync::atomic::{AtomicU32, Ordering};

/// The body size the type scale below is written for; the chosen size scales it.
const BASE_BODY_SIZE: f32 = 14.;

/// `Prefs::font_size / BASE_BODY_SIZE`, read by `Typography` (which has no `cx`).
static TEXT_SCALE: AtomicU32 = AtomicU32::new(0);

pub fn text_scale() -> f32 {
    match TEXT_SCALE.load(Ordering::Relaxed) {
        0 => config::DEFAULT_FONT_SIZE / BASE_BODY_SIZE,
        bits => f32::from_bits(bits),
    }
}

pub mod space {
    pub const XXS: f32 = 2.;
    pub const XS: f32 = 4.;
    pub const SM: f32 = 8.;
    pub const MD: f32 = 12.;
    pub const LG: f32 = 16.;
    pub const XL: f32 = 20.;
    pub const XXL: f32 = 24.;
    pub const XXXL: f32 = 32.;
    pub const HUGE: f32 = 48.;
}

pub mod radius {
    pub const XS: f32 = 6.;
    pub const SM: f32 = 10.;
    pub const MD: f32 = 14.;
    pub const LG: f32 = 18.;
    pub const XL: f32 = 24.;
    pub const FULL: f32 = 999.;
}

pub const MAX_CONTENT_WIDTH: f32 = 760.;
pub const PAGE_PADDING: f32 = 20.;
/// The bar on top of a page: the chat's, the Agents and Extensions lists', what's open beside them, and pushed pages'
/// (`ui::sub_page_header`).
pub const HEADER_HEIGHT: f32 = 44.;

fn parse_color(value: &str) -> Hsla {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        let parse = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap_or(0) as f32 / 255.;
        let alpha = if hex.len() >= 8 { parse(6..8) } else { 1. };
        return Rgba { r: parse(0..2), g: parse(2..4), b: parse(4..6), a: alpha }.into();
    }
    if let Some(inner) = value.strip_prefix("rgba(").and_then(|rest| rest.strip_suffix(')')) {
        let parts: Vec<f32> = inner.split(',').map(|part| part.trim().parse().unwrap_or(0.)).collect();
        if parts.len() == 4 {
            return Rgba { r: parts[0] / 255., g: parts[1] / 255., b: parts[2] / 255., a: parts[3] }.into();
        }
    }
    gpui::black()
}

macro_rules! palette {
    ($($field:ident),* $(,)?) => {
        /// The palette roles the app draws with, as assets/themes.json has them.
        #[derive(Clone, Copy, Debug)]
        pub struct Palette { $(pub $field: Hsla),* }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PaletteHex { $($field: String),* }

        impl From<&PaletteHex> for Palette {
            fn from(hex: &PaletteHex) -> Self {
                Palette { $($field: parse_color(&hex.$field)),* }
            }
        }
    };
}

palette!(
    background,
    chat_background,
    surface,
    surface_strong,
    raised,
    sunken,
    border,
    border_strong,
    text,
    text_secondary,
    text_tertiary,
    text_disabled,
    accent,
    on_accent,
    action,
    on_action,
    accent_soft,
    on_accent_soft,
    link,
    danger,
    on_danger,
    danger_soft,
    on_danger_soft,
    success,
    on_success,
    success_soft,
    on_success_soft,
    warning,
    on_warning,
    warning_soft,
    on_warning_soft,
    info,
    on_info,
    info_soft,
    on_info_soft,
    user_bubble,
    on_user_bubble,
    user_bubble_border,
    assistant_bubble,
    on_assistant_bubble,
    selected_fill,
    scrim,
    media,
    on_media,
);

#[derive(Deserialize)]
struct SchemesHex {
    light: PaletteHex,
    dark: PaletteHex,
}

#[derive(Deserialize)]
struct FamilyHex {
    label: String,
    description: Option<String>,
    standard: SchemesHex,
    high: SchemesHex,
}

pub struct Family {
    pub name: String,
    pub label: String,
    pub description: String,
    /// [standard, high] × [light, dark]
    palettes: [[Palette; 2]; 2],
}

impl Family {
    pub fn palette(&self, dark: bool, high: bool) -> Palette {
        self.palettes[high as usize][dark as usize]
    }
}

/// The built-in families, in the app's order.
pub fn families() -> &'static [Family] {
    static FAMILIES: OnceLock<Vec<Family>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let raw: BTreeMap<String, FamilyHex> =
            serde_json::from_str(include_str!("../assets/themes.json")).expect("themes.json");
        let order = ["default", "green", "ocean", "ember", "iris"];
        order
            .iter()
            .filter_map(|name| {
                let family = raw.get(*name)?;
                Some(Family {
                    name: name.to_string(),
                    label: family.label.clone(),
                    description: family.description.clone().unwrap_or_default(),
                    palettes: [
                        [(&family.standard.light).into(), (&family.standard.dark).into()],
                        [(&family.high.light).into(), (&family.high.dark).into()],
                    ],
                })
            })
            .collect()
    })
}

pub struct Theme {
    pub prefs: Prefs,
    pub colors: Palette,
}

impl Global for Theme {}

impl Theme {
    pub fn family(&self) -> &'static Family {
        families().iter().find(|family| family.name == self.prefs.theme_family).unwrap_or(&families()[0])
    }

    pub fn high_contrast(&self) -> bool {
        self.prefs.theme_contrast == "high"
    }

    pub fn font_family(&self) -> SharedString {
        SharedString::from(self.prefs.font_family.clone())
    }

    pub fn mono_family(&self) -> SharedString {
        SharedString::from(self.prefs.mono_family.clone())
    }

    pub fn mono_size(&self) -> f32 {
        self.prefs.mono_size
    }
}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
    fn colors(&self) -> &Palette {
        &self.theme().colors
    }
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

fn system_dark(appearance: WindowAppearance) -> bool {
    matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Resolves the palette from the saved preferences and the system appearance,
/// and keeps gpui-component's widgets (inputs, dialogs, toasts) in step.
pub fn apply(prefs: Prefs, window: Option<&mut Window>, cx: &mut App) {
    let appearance = window.as_ref().map(|window| window.appearance()).unwrap_or_else(|| cx.window_appearance());
    let dark = match prefs.theme_mode.as_str() {
        "light" => false,
        "dark" => true,
        _ => system_dark(appearance),
    };
    let family = families().iter().find(|family| family.name == prefs.theme_family).unwrap_or(&families()[0]);
    let colors = family.palette(dark, prefs.theme_contrast == "high");
    TEXT_SCALE.store((prefs.font_size.clamp(10., 28.) / BASE_BODY_SIZE).to_bits(), Ordering::Relaxed);
    let (font, font_size) = (prefs.font_family.clone(), prefs.font_size);
    let (mono, mono_size) = (prefs.mono_family.clone(), prefs.mono_size);
    cx.set_global(Theme { prefs, colors });

    use gpui_component::theme::{Theme as UiTheme, ThemeMode};
    UiTheme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let ui = UiTheme::global_mut(cx);
    ui.font_family = font.into();
    ui.font_size = px(font_size);
    ui.mono_font_family = mono.into();
    ui.mono_font_size = px(mono_size);
    ui.radius = px(radius::SM);
    ui.radius_lg = px(radius::LG);
    ui.shadow = false;
    let c = &mut ui.colors;
    c.background = colors.background;
    c.foreground = colors.text;
    c.border = colors.border;
    c.input = colors.border;
    c.ring = colors.accent;
    c.caret = colors.accent;
    // Translucent: text views paint the selection over the glyphs.
    c.selection = colors.accent.opacity(0.3);
    c.primary = colors.action;
    c.primary_hover = colors.action.opacity(0.9);
    c.primary_active = colors.action.opacity(0.8);
    c.primary_foreground = colors.on_action;
    c.secondary = colors.surface;
    c.secondary_hover = colors.surface_strong;
    // Also markdown's code (inline chips and blocks, outlined with `border`): vendor/gpui-component/FILO.md.
    c.secondary_active = colors.surface_strong;
    c.secondary_foreground = colors.text;
    c.muted = colors.surface;
    c.muted_foreground = colors.text_secondary;
    c.accent = colors.selected_fill;
    c.accent_foreground = colors.text;
    c.popover = colors.raised;
    c.popover_foreground = colors.text;
    c.link = colors.link;
    c.link_hover = colors.link;
    c.link_active = colors.link;
    c.danger = colors.danger;
    c.danger_hover = colors.danger.opacity(0.9);
    c.danger_active = colors.danger.opacity(0.8);
    c.danger_foreground = colors.on_danger;
    c.success = colors.success;
    c.success_foreground = colors.on_success;
    c.warning = colors.warning;
    c.warning_foreground = colors.on_warning;
    c.info = colors.info;
    c.info_foreground = colors.on_info;
    c.list = colors.background;
    c.list_hover = colors.selected_fill;
    c.list_active = colors.selected_fill;
    c.switch = colors.border_strong;
    c.switch_thumb = colors.raised;
    c.overlay = colors.scrim;
    c.scrollbar = gpui::transparent_black();
    c.scrollbar_thumb = colors.text.opacity(0.25);
    c.scrollbar_thumb_hover = colors.text.opacity(0.4);
    c.skeleton = colors.surface_strong;
    c.window_border = colors.border;
}

pub fn save_and_apply(prefs: Prefs, window: &mut Window, cx: &mut App) {
    config::save_prefs(&prefs);
    apply(prefs, Some(window), cx);
    window.refresh();
}

/// Re-resolves "system" mode when macOS switches between light and dark.
pub fn sync_appearance(window: &mut Window, cx: &mut App) {
    let prefs = cx.global::<Theme>().prefs.clone();
    if prefs.theme_mode == "system" {
        apply(prefs, Some(window), cx);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeVariant {
    LargeTitle,
    Title1,
    Title2,
    Title3,
    Headline,
    Body,
    Callout,
    Subhead,
    Footnote,
    Caption,
    Micro,
}

impl TypeVariant {
    /// (size, line height, default weight). The phone's scale (src/theme/tokens.ts,
    /// body 17) shrunk for a desktop window: body 14, like other Mac apps.
    fn metrics(self) -> (f32, f32, FontWeight) {
        match self {
            TypeVariant::LargeTitle => (24., 30., FontWeight::BOLD),
            TypeVariant::Title1 => (21., 28., FontWeight::BOLD),
            TypeVariant::Title2 => (18., 24., FontWeight::SEMIBOLD),
            TypeVariant::Title3 => (16., 22., FontWeight::SEMIBOLD),
            TypeVariant::Headline => (14., 20., FontWeight::SEMIBOLD),
            TypeVariant::Body => (14., 21., FontWeight::NORMAL),
            TypeVariant::Callout => (14., 20., FontWeight::NORMAL),
            TypeVariant::Subhead => (13., 18., FontWeight::NORMAL),
            TypeVariant::Footnote => (12., 17., FontWeight::NORMAL),
            // Regular, unlike the phone's medium: small metadata (times, counts)
            // shouldn't look bold. Emphasis is set where it's wanted.
            TypeVariant::Caption => (11., 15., FontWeight::NORMAL),
            TypeVariant::Micro => (11., 14., FontWeight::NORMAL),
        }
    }

    /// The type's size at the chosen text scale, as `Typography::typo` sets it (for measuring text before it's drawn).
    pub fn size(self) -> f32 {
        (self.metrics().0 * text_scale() * 2.).round() / 2.
    }
}

/// `typography(variant)` from src/theme/tokens.ts, for any styled element.
pub trait Typography: Styled + Sized {
    fn typo(self, variant: TypeVariant) -> Self {
        let (_, line_height, weight) = variant.metrics();
        self.text_size(px(variant.size())).line_height(px((line_height * text_scale()).round())).font_weight(weight)
    }

    fn large_title(self) -> Self {
        self.typo(TypeVariant::LargeTitle)
    }
    fn title1(self) -> Self {
        self.typo(TypeVariant::Title1)
    }
    fn title2(self) -> Self {
        self.typo(TypeVariant::Title2)
    }
    fn title3(self) -> Self {
        self.typo(TypeVariant::Title3)
    }
    fn headline(self) -> Self {
        self.typo(TypeVariant::Headline)
    }
    fn body(self) -> Self {
        self.typo(TypeVariant::Body)
    }
    fn callout(self) -> Self {
        self.typo(TypeVariant::Callout)
    }
    fn subhead(self) -> Self {
        self.typo(TypeVariant::Subhead)
    }
    fn footnote(self) -> Self {
        self.typo(TypeVariant::Footnote)
    }
    fn caption(self) -> Self {
        self.typo(TypeVariant::Caption)
    }
    fn micro(self) -> Self {
        self.typo(TypeVariant::Micro)
    }
    fn semibold(self) -> Self {
        self.font_weight(FontWeight::SEMIBOLD)
    }
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }
    fn bold(self) -> Self {
        self.font_weight(FontWeight::BOLD)
    }
}

impl<T: Styled> Typography for T {}

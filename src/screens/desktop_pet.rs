// Desktop pet (Settings › Desktop pet): the pet on a card as it rests on the desktop, breathing on its shadow, with
// its name and a Show on desktop switch (off hides it and keeps the pick); then the characters to pick from, App
// icon's tiles with their names under them, the current one ringed: the animals, then the custom pets the main agent
// made. A click sets the desktop pet at once and keeps it (companion.rs's `pet_character`); unpicked, the pet is the
// main agent's picture (companion.rs's `pet_look`).

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, Window, div, img,
    prelude::FluentBuilder as _, px,
};
use gpui_component::switch::Switch;
use serde_json::json;

use crate::avatars::{self, ANIMALS, CustomAvatar, Look};
use crate::backend;
use crate::companion::{self, SettingsGlobal};
use crate::screens::app_icon::{self, MAX_WIDTH, RING, RING_GAP, TILE};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

/// The pet on the card, as a share of its size on the desktop.
const PREVIEW: f32 = 0.8;
/// A hidden pet shows faded.
const HIDDEN_OPACITY: f32 = 0.35;

pub struct DesktopPetScreen {
    custom: Vec<CustomAvatar>,
    _custom_sub: Task<()>,
    _settings: Subscription,
    /// The pet's look changes with the main agent's picture.
    _companion: Option<Subscription>,
}

impl DesktopPetScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _custom_sub = backend::subscribe(
            cx,
            "customAvatars:list",
            json!({}),
            |this: &mut Self, result: Result<Vec<CustomAvatar>, String>, cx| {
                // A failed load leaves the animals.
                this.custom = result.unwrap_or_default();
                cx.notify();
            },
        );
        // The pet can be shown or hidden from the menu bar or its shortcut while this is open.
        let _settings = cx.observe_global::<SettingsGlobal>(|_, cx| cx.notify());
        let _companion = companion::companion(cx).map(|companion| cx.observe(&companion, |_, _, cx| cx.notify()));
        DesktopPetScreen { custom: Vec::new(), _custom_sub, _settings, _companion }
    }

    /// The pet as it is on the desktop, its name, what it does, and the switch.
    fn hero(look: &Look, shown: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        div()
            .flex()
            .items_center()
            .gap(px(space::LG))
            .py(px(space::SM))
            .pl(px(space::SM))
            .pr(px(space::LG + 2.))
            .rounded(px(radius::LG))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex_none()
                    .when(!shown, |this| this.opacity(HIDDEN_OPACITY))
                    .child(crate::pet::resting(look, PREVIEW)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .child(div().title3().semibold().truncate().child(companion::pet_name(look)))
                    .child(div().footnote().text_color(colors.text_secondary).child(if shown {
                        "Sits on your desktop and shows what your agents are doing"
                    } else {
                        "Hidden from your desktop"
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .footnote()
                    .text_color(colors.text_secondary)
                    .child("Show on desktop")
                    .child(crate::smoke::tracked(
                        "desktop-pet-show",
                        Switch::new("desktop-pet-show").checked(shown).on_click(cx.listener(
                            |_, checked: &bool, _, cx| {
                                let on = *checked;
                                companion::update_settings(cx, |settings| settings.pet = on);
                            },
                        )),
                    )),
            )
            .into_any_element()
    }

    /// A character: its tile in the ring that marks the current pet, its name under it.
    fn cell(key: String, name: String, selected: bool, tile: AnyElement, character: String, cx: &App) -> AnyElement {
        let colors = *cx.colors();
        let ring = app_icon::ring(
            key.clone(),
            selected,
            TILE * crate::app_icon::CORNER_SHARE,
            tile,
            move |_, cx| {
                let character = character.clone();
                companion::update_settings(cx, move |settings| settings.pet_character = Some(character));
            },
            cx,
        );
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::XS))
            .child(crate::smoke::tracked(format!("{key}-tile"), ring))
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .caption()
                    .text_color(if selected { colors.text } else { colors.text_secondary })
                    .child(name),
            )
            .into_any_element()
    }

    /// Every character: the animals at rest, then the custom pets.
    fn characters(&self, picked: &str, cx: &App) -> Vec<AnyElement> {
        let animals = ANIMALS.iter().map(|animal| {
            let id = animal.first_avatar();
            let tile = app_icon::avatar_ground(None, TILE).child(app_icon::avatar_figure(
                avatars::animal_image(&id),
                &id,
                TILE,
            ));
            Self::cell(
                format!("desktop-pet-{}", animal.id),
                animal.label.to_string(),
                picked == animal.id,
                tile.into_any_element(),
                animal.id.to_string(),
                cx,
            )
        });
        let custom = self.custom.iter().map(|avatar| {
            let character = avatars::custom_ref(&avatar.id);
            let picture = img(avatar.poses.idle.image())
                .size(px(TILE * crate::app_icon::AVATAR_SHARE))
                .flex_none()
                .object_fit(gpui::ObjectFit::Contain);
            let tile = app_icon::avatar_ground(None, TILE).child(picture);
            Self::cell(
                format!("desktop-pet-{character}"),
                avatar.name.clone(),
                picked == character,
                tile.into_any_element(),
                character,
                cx,
            )
        });
        animals.chain(custom).collect()
    }
}

impl Render for DesktopPetScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let settings = companion::settings(cx);
        let width = ui::column_width().min(MAX_WIDTH);
        let cell_width = TILE + 2. * (RING_GAP + RING);
        let per_row = (((width + space::XS) / (cell_width + space::XS)).floor() as usize).clamp(4, app_icon::PER_ROW);
        let look = companion::companion(cx).and_then(|companion| companion.read(cx).pet_look().cloned());
        let content: Vec<AnyElement> = match look {
            // Until the agents come, nothing to show.
            None => Vec::new(),
            Some(look) => {
                let picked = companion::pet_character(&look);
                let characters = self.characters(&picked, cx);
                vec![
                    Self::hero(&look, settings.pet, cx),
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(space::SM + 2.))
                        .child(
                            div()
                                .px(px(space::XXS))
                                .footnote()
                                .semibold()
                                .text_color(colors.text_secondary)
                                .child("Characters"),
                        )
                        .child(app_icon::grid(characters, per_row))
                        .child(
                            div()
                                .pt(px(space::XS))
                                .px(px(space::XXS))
                                .footnote()
                                .text_color(colors.text_tertiary)
                                .child("Want a different pet? Ask your main agent to make one, and it appears here."),
                        )
                        .into_any_element(),
                ]
            }
        };
        ui::sub_page(
            ui::sub_page_header("desktop-pet", "Desktop pet", vec![], cx),
            div().id("desktop-pet-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                ui::page_column()
                    .max_w(px(MAX_WIDTH + 2. * crate::theme::PAGE_PADDING))
                    .pt(px(space::SM))
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .children(content),
            ),
            cx,
        )
    }
}

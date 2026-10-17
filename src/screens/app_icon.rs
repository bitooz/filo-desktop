// App icon (Settings › App icon): the current icon big on a card with its name
// (Use default when it isn't Filo's own), then every icon to pick from as it looks in the Dock, one size, eight to a
// row: Filo's mark in its eight colours (app_icon::CHOICES), the avatars on their ground (the animals, then the pets
// the agents drew), and the pictures the user
// uploaded, then + to upload another (cropped to the icon's rounded square; × on a picture removes it). Names show on
// hover. While a character is the icon, its Pose and Background rows show under the avatars: its poses as small
// tiles, and the light ground or an avatar colour's as round swatches. A click sets the Dock icon at once and keeps it
// (app_icon.rs).

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DragMoveEvent, Entity, ImageSource, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, div, img, prelude::FluentBuilder as _, px, svg,
};
use gpui_component::WindowExt as _;
use gpui_component::slider::{Slider, SliderEvent, SliderState};

use crate::app_icon::{self, CHOICES, Choice, Icon};
use crate::avatars::{self, ANIMALS};
use crate::screens::agent_picture::{self, Crop, CropDrag};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};

/// Every icon's side in the grid, eight to a row when there's room; the current one's in the card above.
pub const TILE: f32 = 56.;
pub const PER_ROW: usize = 8;
const HERO: f32 = 84.;
/// The widest the page's column gets, so the grid's columns stay close.
pub const MAX_WIDTH: f32 = 600.;
/// The ring round the current icon: a gap, then the accent.
pub const RING_GAP: f32 = 2.;
pub const RING: f32 = 2.;
/// A character's Pose tiles and Background swatches, and the room their row names take.
const POSE: f32 = 34.;
const SWATCH: f32 = 22.;
const ROW_NAME: f32 = 84.;
/// The crop's rounded square in the upload sheet.
const CROP: f32 = 200.;
/// The kept picture's side: the icon's square is 824 px of its 1024.
const KEPT: u32 = 1024;

/// The edge round a tile: light on a dark ground, so a dark tile shows on the dark theme's page, dark on a light one.
fn edge(ground: u32) -> gpui::Hsla {
    let channel = |shift: u32| ((ground >> shift) & 0xff) as f32 / 255.;
    let luminance = 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0);
    if luminance < 0.3 { gpui::white().opacity(0.16) } else { gpui::black().opacity(0.08) }
}

/// Filo's mark on the rounded square in the choice's colours, `size` px wide.
fn mark(choice: Choice, size: f32) -> gpui::Div {
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size * app_icon::CORNER_SHARE))
        .bg(gpui::rgb(choice.ground))
        .border_1()
        .border_color(edge(choice.ground))
        .child(
            svg()
                .path("icons/filo-mark.svg")
                .size(px(size * app_icon::MARK_SHARE))
                .flex_none()
                .text_color(gpui::rgb(choice.mark)),
        )
}

/// An icon as drawn in the app: the rounded square `size` px wide (the Dock's PNG adds Apple's margin around it), with
/// the mark, an avatar on the light ground, or the uploaded picture filling it.
pub fn icon(icon: &Icon, size: f32) -> AnyElement {
    let corner = px(size * app_icon::CORNER_SHARE);
    match icon {
        Icon::Mark(choice) => mark(*choice, size).into_any_element(),
        Icon::Avatar(id, color) => {
            avatar_ground(*color, size).child(avatar_figure(avatars::avatar_source(id), id, size)).into_any_element()
        }
        Icon::Picture(name) => {
            let path = app_icon::picture_path(name);
            if path.is_file() {
                img(path).size(px(size)).flex_none().rounded(corner).into_any_element()
            } else {
                mark(CHOICES[0], size).into_any_element()
            }
        }
    }
}

/// The rounded square an avatar's icon stands on, `size` px wide: the light ground, or an avatar colour's.
pub fn avatar_ground(color: Option<&str>, size: f32) -> gpui::Div {
    let shade = |[r, g, b]: [f32; 3]| gpui::Hsla::from(gpui::Rgba { r: r / 255., g: g / 255., b: b / 255., a: 1. });
    let (top, bottom) = app_icon::ground(color);
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(px(size * app_icon::CORNER_SHARE))
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(shade(top), 0.),
            gpui::linear_color_stop(shade(bottom), 1.),
        ))
        .border_1()
        .border_color(gpui::black().opacity(0.08))
}

/// Avatar `id`'s picture (`source`, which may be recoloured) placed on its ground `size` px wide as the Dock's icon
/// has it: the figure (not the picture, which has room round it) fitted in its box and centred, a little low.
pub fn avatar_figure(source: ImageSource, id: &str, size: f32) -> gpui::Img {
    let (share, dx, dy) = app_icon::avatar_figure(id);
    let figure = size * app_icon::AVATAR_SHARE / share;
    avatars::picture(source, figure)
        .relative()
        .left(px(-dx * figure))
        .top(px(size * app_icon::AVATAR_DROP_SHARE - dy * figure))
}

pub type Pick = Rc<dyn Fn(Icon, &mut Window, &mut App)>;
/// After a picture is uploaded (and picked) or removed: the icon may have changed.
pub type Changed = Rc<dyn Fn(&mut Window, &mut App)>;

/// The current icon big, its name ("· Default" for Filo's own), where it shows, and Use default when it isn't.
fn hero(id: &str, picked: &Icon, pick: &Pick, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let default = *picked == Icon::default();
    let pick = pick.clone();
    div()
        .flex()
        .items_center()
        .gap(px(space::LG + 2.))
        .p(px(space::LG + 2.))
        .rounded(px(radius::LG))
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
        .child(icon(picked, HERO))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(space::XXS))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(space::XS + 1.))
                        .child(div().title3().semibold().truncate().child(picked.label()))
                        .when(default, |this| {
                            this.child(div().callout().text_color(colors.text_secondary).child("· Default"))
                        }),
                )
                .child(div().footnote().text_color(colors.text_secondary).child("Shown in your Dock and app switcher")),
        )
        .when(!default, |this| {
            this.child(
                div()
                    .id(SharedString::from(format!("{id}-use-default")))
                    .flex_none()
                    .footnote()
                    .text_color(colors.text_secondary)
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text))
                    .on_click(move |_: &ClickEvent, window, cx| pick(Icon::default(), window, cx))
                    .child("Use default"),
            )
        })
        .into_any_element()
}

/// `content` (rounded by `corner`) in the ring that marks the current pick, clicked with `on_click`.
pub fn ring(
    key: String,
    selected: bool,
    corner: f32,
    content: AnyElement,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let colors = *cx.colors();
    div()
        .id(SharedString::from(key))
        .flex_none()
        .p(px(RING_GAP))
        .rounded(px(corner + RING_GAP + RING))
        .border(px(RING))
        .border_color(if selected { colors.accent } else { gpui::transparent_black() })
        .cursor_pointer()
        .when(!selected, |this| this.hover(|style| style.border_color(colors.text.opacity(0.18))))
        .child(content)
        .on_click(move |_: &ClickEvent, window, cx| on_click(window, cx))
}

/// `content` in its ring, named on hover.
fn ringed(
    key: String,
    name: String,
    selected: bool,
    corner: f32,
    content: AnyElement,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    ring(key, selected, corner, content, on_click, cx)
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name.clone()).build(window, cx))
}

/// One cell of the grid: `content` (a tile `TILE` px wide) in the ring that marks the current icon, named on hover;
/// `remove` adds a × on hover at its corner.
fn cell(
    key: String,
    name: String,
    selected: bool,
    content: AnyElement,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    remove: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    let group = SharedString::from(format!("{key}-cell"));
    let ring = ringed(key.clone(), name, selected, TILE * app_icon::CORNER_SHARE, content, on_click, cx);
    let remove = remove.map(|remove| {
        div()
            .id(SharedString::from(format!("{key}-remove")))
            .absolute()
            .top(px(-3.))
            .right(px(-3.))
            .size(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(radius::FULL))
            .bg(colors.surface_strong)
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .invisible()
            .group_hover(group.clone(), |style| style.visible())
            .hover(|style| style.bg(colors.selected_fill))
            .child(ui::icon_sized("x", 11., colors.text_secondary))
            .on_click(move |_: &ClickEvent, window, cx| remove(window, cx))
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Remove").build(window, cx))
    });
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .justify_center()
        .child(div().relative().group(group).child(ring).children(remove))
        .into_any_element()
}

/// Cells `per_row` to a row, the last row filled out so its cells line up with the ones above.
pub fn grid(cells: Vec<AnyElement>, per_row: usize) -> gpui::Div {
    let count = cells.len();
    let mut cells = cells.into_iter();
    let rows = (0..count.div_ceil(per_row)).map(|_| {
        let row: Vec<AnyElement> = cells.by_ref().take(per_row).collect();
        let short = per_row - row.len();
        div().flex().gap(px(space::XS)).children(row).children((0..short).map(|_| div().flex_1()))
    });
    div().flex().flex_col().gap(px(space::SM)).children(rows)
}

/// A named row under the avatars (Pose, Background): the name, then its items.
fn detail_row(name: &str, items: Vec<AnyElement>, cx: &App) -> AnyElement {
    let colors = *cx.colors();
    div()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .child(
            div()
                .w(px(ROW_NAME))
                .flex_none()
                .pl(px(space::XXS))
                .footnote()
                .text_color(colors.text_secondary)
                .child(name.to_string()),
        )
        .child(div().flex_1().min_w_0().flex().flex_wrap().items_center().gap(px(space::XS)).children(items))
        .into_any_element()
}

/// The current character's poses as small tiles and its grounds as round swatches (the light one first), each on its
/// ground, the current ones ringed.
fn character_rows(
    id: &str,
    avatar: &str,
    color: Option<&'static str>,
    choose: &dyn Fn(Icon) -> Box<dyn Fn(&mut Window, &mut App)>,
    cx: &App,
) -> Vec<AnyElement> {
    // An animal's poses, or a pet's (avatars.rs `CHARACTER_POSES`), as avatar ids.
    let poses: Vec<(String, &str)> = match (avatars::animal_of(avatar), avatars::character_pose(avatar)) {
        (Some(animal), _) => animal.poses.iter().map(|(pose, label)| (animal.avatar_id(pose), *label)).collect(),
        (None, Some((pet, _, _))) => avatars::CHARACTER_POSES
            .iter()
            .map(|(pose, label)| (avatars::character_pose_id(&pet.id, pose), *label))
            .collect(),
        (None, None) => return Vec::new(),
    };
    let poses = poses
        .into_iter()
        .map(|(pose, label)| {
            let this = Icon::Avatar(pose.clone(), color);
            let on_click = choose(this.clone());
            ringed(
                format!("{id}-pose-{pose}"),
                label.to_string(),
                pose == avatar,
                POSE * app_icon::CORNER_SHARE,
                icon(&this, POSE),
                on_click,
                cx,
            )
            .into_any_element()
        })
        .collect();
    let shade = |[r, g, b]: [f32; 3]| gpui::Hsla::from(gpui::Rgba { r: r / 255., g: g / 255., b: b / 255., a: 1. });
    let grounds = std::iter::once(None)
        .chain(crate::avatar_colors::COLORS.iter().map(|(color, _, _)| Some(*color)))
        .map(|ground| {
            let (top, bottom) = app_icon::ground(ground);
            let swatch = div()
                .size(px(SWATCH))
                .rounded(px(radius::FULL))
                .bg(gpui::linear_gradient(
                    180.,
                    gpui::linear_color_stop(shade(top), 0.),
                    gpui::linear_color_stop(shade(bottom), 1.),
                ))
                .border_1()
                .border_color(gpui::black().opacity(0.1))
                .into_any_element();
            let on_click = choose(Icon::Avatar(avatar.to_string(), ground));
            let key = format!("{id}-ground-{}", ground.unwrap_or("light"));
            ringed(key, app_icon::ground_label(ground), ground == color, SWATCH / 2., swatch, on_click, cx)
                .into_any_element()
        })
        .collect();
    vec![detail_row("Pose", poses, cx), detail_row("Background", grounds, cx)]
}

fn group(title: &str, note: Option<&str>, cells: Vec<AnyElement>, per_row: usize, cx: &App) -> AnyElement {
    group_with(title, note, cells, per_row, Vec::new(), cx)
}

/// A group with `rows` under its grid.
fn group_with(
    title: &str,
    note: Option<&str>,
    cells: Vec<AnyElement>,
    per_row: usize,
    rows: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(px(space::SM + 2.))
        .child(
            div()
                .px(px(space::XXS))
                .flex()
                .justify_between()
                .footnote()
                .text_color(colors.text_secondary)
                .child(div().semibold().child(title.to_string()))
                .children(note.map(|note| div().text_color(colors.text_tertiary).child(note.to_string()))),
        )
        .child(grid(cells, per_row))
        .when(!rows.is_empty(), |this| {
            this.child(div().pt(px(space::XS)).flex().flex_col().gap(px(space::SM)).children(rows))
        })
        .into_any_element()
}

/// The page under its title, in a column `width` px wide: the current icon, then everything to pick from (Filo's
/// colours, the avatars, the user's pictures and +).
pub fn picker(id: &str, picked: &Icon, width: f32, pick: Pick, changed: Changed, cx: &App) -> Vec<AnyElement> {
    let colors = *cx.colors();
    // A cell is the tile in its ring; fewer to a row in a narrow window.
    let cell_width = TILE + 2. * (RING_GAP + RING);
    let per_row = (((width + space::XS) / (cell_width + space::XS)).floor() as usize).clamp(4, PER_ROW);
    let (avatar, color) = match picked {
        Icon::Avatar(current, color) => (Some(current.as_str()), *color),
        _ => (None, None),
    };
    let animal = avatar.and_then(avatars::animal_of);
    let choose = |icon: Icon| -> Box<dyn Fn(&mut Window, &mut App)> {
        let pick = pick.clone();
        Box::new(move |window: &mut Window, cx: &mut App| pick(icon.clone(), window, cx))
    };

    let colours = CHOICES
        .iter()
        .map(|choice| {
            let this = Icon::Mark(*choice);
            let name =
                if choice.is_default() { format!("{} · Default", choice.label) } else { choice.label.to_string() };
            cell(
                format!("{id}-{}", choice.id),
                name,
                *picked == this,
                icon(&this, TILE),
                choose(this.clone()),
                None,
                cx,
            )
        })
        .collect();
    let characters = ANIMALS
        .iter()
        .map(|item| {
            // The picked animal in its pose; another keeps that pose when it has it (as the agent picture sheet), else
            // waves; on the current character's ground.
            let this = Icon::Avatar(
                match avatar {
                    Some(current) => avatars::avatar_for_animal(item, current),
                    None => item.avatar_id("waving"),
                },
                color,
            );
            let selected = animal.is_some_and(|animal| animal.id == item.id);
            let shown = if selected { picked.clone() } else { this.clone() };
            cell(
                format!("{id}-animal-{}", item.id),
                item.label.to_string(),
                selected,
                icon(&shown, TILE),
                choose(this),
                None,
                cx,
            )
        })
        .collect::<Vec<_>>();
    // Then the pets the agents drew (the desktop-pet skill): the picked one in its pose, the others resting.
    let pet = avatar.and_then(avatars::character_pose).map(|(pet, _, _)| pet.id);
    let mut characters = characters;
    characters.extend(avatars::characters().into_iter().map(|item| {
        let this = Icon::Avatar(avatars::custom_ref(&item.id), color);
        let selected = pet.as_ref() == Some(&item.id);
        let shown = if selected { picked.clone() } else { this.clone() };
        cell(format!("{id}-pet-{}", item.id), item.name, selected, icon(&shown, TILE), choose(this), None, cx)
    }));
    let mut yours: Vec<AnyElement> = app_icon::pictures()
        .into_iter()
        .map(|name| {
            let this = Icon::Picture(name.clone());
            let changed = changed.clone();
            let remove: Box<dyn Fn(&mut Window, &mut App)> = Box::new({
                let name = name.clone();
                move |window, cx| {
                    app_icon::remove_picture(&name, cx);
                    changed(window, cx);
                }
            });
            cell(
                format!("{id}-picture-{name}"),
                this.label(),
                *picked == this,
                icon(&this, TILE),
                choose(this.clone()),
                Some(remove),
                cx,
            )
        })
        .collect();
    let add = div()
        .size(px(TILE))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(TILE * app_icon::CORNER_SHARE))
        .border_1()
        .border_dashed()
        .border_color(colors.text_secondary.opacity(0.7))
        .child(ui::icon_sized("plus", 22., colors.text_secondary))
        .into_any_element();
    yours.push(cell(
        format!("{id}-upload"),
        "Upload a picture".into(),
        false,
        add,
        move |window, cx| upload_picture(changed.clone(), window, cx),
        None,
        cx,
    ));

    vec![
        hero(id, picked, &pick, cx),
        group("Filo", None, colours, per_row, cx),
        group_with(
            "Characters",
            None,
            characters,
            per_row,
            avatar.map(|avatar| character_rows(id, avatar, color, &choose, cx)).unwrap_or_default(),
            cx,
        ),
        group("Yours", Some("PNG, JPEG or HEIC"), yours, per_row, cx),
    ]
}

/// Upload a picture: the file picker, then the picture in a sheet to crop to the icon's square; Use picture keeps it
/// and sets it on the Dock.
pub fn upload_picture(uploaded: Changed, window: &mut Window, cx: &mut App) {
    let chosen = agent_picture::choose_file_saying("Choose a picture for Filo’s app icon: PNG, JPEG or HEIC");
    window
        .spawn(cx, async move |cx| {
            let Ok(Some(path)) = chosen.await else { return };
            let read = cx.background_executor().spawn(async move { Crop::load_in(&path, CROP) }).await;
            cx.update(|window, cx| match read {
                Ok(crop) => {
                    let sheet = cx.new(|cx| CropSheet::new(crop, uploaded, cx));
                    window.open_dialog(cx, move |dialog, _, _| {
                        dialog
                            .w(px(380.))
                            .overlay_closable(true)
                            .title(div().headline().child("Crop your picture"))
                            .child(sheet.clone())
                    });
                }
                Err(error) => ui::toast_error(window, cx, "Couldn’t open the picture", error.to_string()),
            })
            .ok();
        })
        .detach();
}

/// The upload sheet: the picture under the icon's rounded square (drag to move it, the slider zooms), Cancel and Use
/// picture.
pub struct CropSheet {
    crop: Crop,
    zoom: Entity<SliderState>,
    keeping: bool,
    uploaded: Changed,
    _zoom: Subscription,
}

impl CropSheet {
    fn new(crop: Crop, uploaded: Changed, cx: &mut Context<Self>) -> Self {
        let zoom = cx.new(|_| SliderState::new().min(1.).max(agent_picture::MAX_ZOOM).step(0.01).default_value(1.));
        let _zoom = cx.subscribe(&zoom, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(value) = event;
            this.crop.set_zoom(value.start());
            cx.notify();
        });
        CropSheet { crop, zoom, keeping: false, uploaded, _zoom }
    }

    fn keep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.keeping {
            return;
        }
        self.keeping = true;
        cx.notify();
        let crop = self.crop.clone();
        cx.spawn_in(window, async move |this, cx| {
            let png = cx
                .background_executor()
                .spawn(async move {
                    let mut png = Vec::new();
                    image::DynamicImage::ImageRgba8(crop.square(KEPT))
                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
                    anyhow::Ok(png)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.keeping = false;
                match png.and_then(|png| app_icon::keep_picture(&png, cx)) {
                    Ok(()) => {
                        window.close_dialog(cx);
                        (this.uploaded)(window, cx);
                    }
                    Err(error) => ui::toast_error(window, cx, "Couldn’t keep the picture", error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The picture at its zoom and place, the square's corners masked in the sheet's colour; dragging moves it.
    fn frame(&self, mask: gpui::Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let (left, top) = self.crop.offset();
        let (width, height) = self.crop.shown_size();
        let corner = CROP * app_icon::CORNER_SHARE;
        // A ring round the rounded square, as wide as its corners are cut.
        let ring = corner;
        let mut cover = div()
            .absolute()
            .left(px(-ring))
            .top(px(-ring))
            .size(px(CROP + 2. * ring))
            .rounded(px(corner + ring))
            .border_color(mask);
        {
            let widths = &mut gpui::Styled::style(&mut cover).border_widths;
            let width = Some(px(ring).into());
            (widths.top, widths.right, widths.bottom, widths.left) = (width, width, width, width);
        }
        let (start, drag) = (cx.entity().downgrade(), cx.entity().downgrade());
        div()
            .id("app-icon-crop")
            .relative()
            .size(px(CROP))
            .flex_none()
            .overflow_hidden()
            .cursor(gpui::CursorStyle::OpenHand)
            .child(
                img(ImageSource::Render(self.crop.shown.clone()))
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height)),
            )
            .child(cover)
            .child(div().absolute().inset_0().rounded(px(corner)).border_1().border_color(gpui::black().opacity(0.08)))
            .on_mouse_down(MouseButton::Left, move |event: &gpui::MouseDownEvent, _, cx| {
                let at = (f32::from(event.position.x), f32::from(event.position.y));
                start.update(cx, |this: &mut CropSheet, _| this.crop.start_drag(at)).ok();
            })
            .on_drag(CropDrag, |drag, _, _, cx| cx.new(|_| drag.clone()))
            .on_drag_move(move |event: &DragMoveEvent<CropDrag>, window, cx| {
                let at = (f32::from(event.event.position.x), f32::from(event.event.position.y));
                Self::dragged(&drag, at, cx);
                window.refresh();
            })
    }

    fn dragged(sheet: &WeakEntity<CropSheet>, at: (f32, f32), cx: &mut App) {
        sheet
            .update(cx, |this, cx| {
                this.crop.drag_to(at);
                cx.notify();
            })
            .ok();
    }
}

impl Render for CropSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let mask = gpui_component::ActiveTheme::theme(&**cx).background;
        let keep = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::LG))
            .pt(px(space::SM))
            .child(self.frame(mask, cx))
            .child(
                div()
                    .w(px(240.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::SM))
                            .child(ui::icon_sized("minus", 13., colors.text_secondary))
                            .child(div().flex_1().child(Slider::new(&self.zoom)))
                            .child(ui::icon_sized("plus", 13., colors.text_secondary)),
                    )
                    .child(
                        div()
                            .footnote()
                            .text_center()
                            .text_color(colors.text_secondary)
                            .child("Drag the picture to move it; the slider zooms."),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .gap(px(space::SM))
                    .child(
                        Button::ghost("app-icon-crop-cancel", "Cancel")
                            .on_click(|_: &ClickEvent, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::primary("app-icon-crop-use", "Use picture").loading(self.keeping).on_click(
                        move |_: &ClickEvent, window, cx| {
                            keep.update(cx, |this, cx| this.keep(window, cx)).ok();
                        },
                    )),
            )
    }
}

/// Settings › App icon.
pub struct AppIconScreen;

impl AppIconScreen {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        AppIconScreen
    }
}

impl Render for AppIconScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = app_icon::chosen();
        let view = cx.entity().downgrade();
        let pick: Pick = Rc::new({
            let view = view.clone();
            move |icon, _, cx| {
                app_icon::choose(icon, cx);
                view.update(cx, |_, cx| cx.notify()).ok();
            }
        });
        let changed: Changed = Rc::new(move |_, cx| {
            view.update(cx, |_, cx| cx.notify()).ok();
        });
        ui::sub_page(
            ui::sub_page_header("app-icon", "App icon", vec![], cx),
            div().id("app-icon-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                ui::page_column()
                    .max_w(px(MAX_WIDTH + 2. * crate::theme::PAGE_PADDING))
                    .pt(px(space::SM))
                    .pb(px(48.))
                    .flex()
                    .flex_col()
                    .gap(px(space::XXL))
                    .children(picker("app-icon", &picked, ui::column_width().min(MAX_WIDTH), pick, changed, cx)),
            ),
            cx,
        )
    }
}

// A picture opened over the whole window to draw on: one from the chat goes back as a new message with a prompt
// typed under it, one attached in the composer is replaced by the marked-up copy. Strokes are kept in the picture's
// own pixels, so resizing the window doesn't move them; finishing burns them into a copy (JPEG, or PNG when the
// picture has transparency) that uploads like any attached file. The working copy is at most 2048 px on its long
// side; one over the 2000 the model takes is resized once more when it's attached (attachments.rs).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{
    AppContext as _, Bounds, ClickEvent, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, PathBuilder,
    PathStyle, Pixels, Render, RenderImage, SharedString, StatefulInteractiveElement as _, StrokeOptions, Styled as _,
    Subscription, Task, Window, canvas, div, img, prelude::FluentBuilder as _, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use image::{DynamicImage, ImageDecoder as _, RgbaImage};
use lyon::tessellation::{LineCap, LineJoin};

use super::attachments::{self, PendingAttachment};
use crate::backend::backend;
use crate::file_preview;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};

const MAX_SIDE: u32 = 2048;
const MAX_PROMPT_CHARS: usize = 4000;
/// Pen colours: fixed, like a marker's, so a drawing looks the same in every theme.
const COLORS: [(u32, &str); 6] = [
    (0xff3b30, "Red"),
    (0xffcc00, "Yellow"),
    (0x34c759, "Green"),
    (0x0a84ff, "Blue"),
    (0xffffff, "White"),
    (0x111111, "Black"),
];
/// Pen widths on screen, in points.
const WIDTHS: [(f32, &str); 3] = [(3., "Thin pen"), (6., "Medium pen"), (12., "Thick pen")];

gpui::actions!(image_editor, [CloseEditor, UndoStroke, SendPicture]);

pub enum Source {
    /// A picture in the conversation: sent back as a new message.
    Chat(file_preview::Item),
    /// A picture attached in the composer: replaced by the marked-up copy.
    Composer { local_id: u64, path: PathBuf },
}

pub enum ImageEditorEvent {
    Close,
    /// The uploaded picture and the prompt to send with it.
    Send {
        attachment: PendingAttachment,
        prompt: String,
    },
    /// The composer's attachment `local_id` becomes the file at `path`.
    Replace {
        local_id: u64,
        path: PathBuf,
    },
}

impl EventEmitter<ImageEditorEvent> for ImageEditor {}

struct Picture {
    /// The file it was read from, sent as it is when nothing was drawn.
    file: PathBuf,
    pixels: Arc<RgbaImage>,
    render: Arc<RenderImage>,
    alpha: bool,
}

#[derive(Clone)]
struct Stroke {
    color: usize,
    /// In the picture's pixels.
    width: f32,
    points: Vec<(f32, f32)>,
}

pub struct ImageEditor {
    source: Source,
    picture: Option<Result<Picture, String>>,
    strokes: Vec<Stroke>,
    /// The last stroke is still being drawn.
    drawing: bool,
    color: usize,
    width: usize,
    /// Where the picture can go: the area between the tools and the prompt, measured each frame.
    stage: Option<Bounds<Pixels>>,
    prompt: Entity<InputState>,
    focus: FocusHandle,
    saving: bool,
    error: Option<String>,
    /// Debug view (`FILO_OPEN=edit:<url>`): a scribble drawn as the picture opens.
    #[cfg(feature = "dev")]
    demo: bool,
    _prompt_sub: Subscription,
    _load: Task<()>,
}

impl ImageEditor {
    pub fn new(source: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| {
            InputState::new(window, cx).multi_line(true).auto_grow(1, 6).placeholder("Tell Filo what to do with it")
        });
        let prompt_sub = cx.subscribe_in(&prompt, window, |this, input, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                ui::cap_length(input, MAX_PROMPT_CHARS, window, cx);
                this.error = None;
                cx.notify();
            }
        });
        let file: LocalBoxFuture<'static, anyhow::Result<PathBuf>> = match &source {
            Source::Chat(item) => file_preview::download(item).boxed_local(),
            Source::Composer { path, .. } => futures::future::ready(Ok(path.clone())).boxed_local(),
        };
        let load = cx.spawn(async move |this, cx| {
            let loaded = match file.await {
                Ok(file) => cx.background_executor().spawn(async move { load(file) }).await,
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.picture = Some(loaded.map_err(|error| {
                    eprintln!("image editor: {error:#}");
                    "This picture couldn’t be opened.".to_string()
                }));
                #[cfg(feature = "dev")]
                if this.demo {
                    this.draw_demo();
                }
                cx.notify();
            })
            .ok();
        });
        let focus = cx.focus_handle();
        ImageEditor {
            source,
            picture: None,
            strokes: Vec::new(),
            drawing: false,
            color: 0,
            width: 1,
            stage: None,
            prompt,
            focus,
            saving: false,
            error: None,
            #[cfg(feature = "dev")]
            demo: false,
            _prompt_sub: prompt_sub,
            _load: load,
        }
    }

    /// Where the keys go first: the prompt for a chat's picture, else the picture (Esc, ⌘Z).
    pub fn focus_first(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.source {
            Source::Chat(_) => self.prompt.update(cx, |input, cx| input.focus(window, cx)),
            Source::Composer { .. } => window.focus(&self.focus),
        }
    }

    /// Debug view: a scribble across the picture once it's open.
    #[cfg(feature = "dev")]
    pub fn show_demo(&mut self) {
        self.demo = true;
        self.draw_demo();
    }

    #[cfg(feature = "dev")]
    fn draw_demo(&mut self) {
        let Some(picture) = self.loaded() else { return };
        let (width, height) = (picture.pixels.width() as f32, picture.pixels.height() as f32);
        let points = (0..=40).map(|step| {
            let t = step as f32 / 40.;
            (width * (0.2 + 0.6 * t), height * (0.5 + 0.2 * (t * 12.).sin()))
        });
        self.strokes = vec![
            Stroke { color: 0, width: width / 120., points: points.collect() },
            Stroke { color: 2, width: width / 60., points: vec![(width * 0.15, height * 0.2)] },
        ];
    }

    /// Frees the picture's texture; the conversation calls this as it closes the editor.
    pub fn release(&self, window: &mut Window) {
        if let Some(Ok(picture)) = &self.picture {
            window.drop_image(picture.render.clone()).ok();
        }
    }

    /// The conversation couldn't take the picture; the editor stays open with why.
    pub fn failed(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.saving = false;
        self.error = Some(message.into());
        cx.notify();
    }

    fn loaded(&self) -> Option<&Picture> {
        self.picture.as_ref()?.as_ref().ok()
    }

    /// The picture's place on screen: as large as fits the stage, at most twice its size, centred.
    fn picture_rect(&self) -> Option<Bounds<Pixels>> {
        let picture = self.loaded()?;
        let stage = self.stage?;
        let (width, height) = (picture.pixels.width() as f32, picture.pixels.height() as f32);
        let room_w = f32::from(stage.size.width) - 2. * space::LG;
        let room_h = f32::from(stage.size.height) - 2. * space::LG;
        if room_w <= 0. || room_h <= 0. {
            return None;
        }
        let scale = (room_w / width).min(room_h / height).min(2.);
        let size = gpui::size(px(width * scale), px(height * scale));
        let origin =
            stage.origin + gpui::point((stage.size.width - size.width) / 2., (stage.size.height - size.height) / 2.);
        Some(Bounds { origin, size })
    }

    /// A point on screen in the picture's pixels, and screen points per picture pixel.
    fn to_picture(&self, position: gpui::Point<Pixels>) -> Option<((f32, f32), f32)> {
        let rect = self.picture_rect()?;
        let scale = f32::from(rect.size.width) / self.loaded()?.pixels.width() as f32;
        let offset = position - rect.origin;
        Some(((f32::from(offset.x) / scale, f32::from(offset.y) / scale), scale))
    }

    fn begin_stroke(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Off the prompt, so ⌘Z undoes strokes rather than typing.
        window.focus(&self.focus);
        let Some((point, scale)) = self.to_picture(event.position) else { return };
        self.strokes.push(Stroke { color: self.color, width: WIDTHS[self.width].0 / scale, points: vec![point] });
        self.drawing = true;
        cx.notify();
    }

    fn extend_stroke(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.drawing {
            return;
        }
        // Let go outside the window: the stroke ends where it left.
        if event.pressed_button != Some(MouseButton::Left) {
            self.drawing = false;
            return;
        }
        let Some((point, _)) = self.to_picture(event.position) else { return };
        let Some(stroke) = self.strokes.last_mut() else { return };
        let (x, y) = *stroke.points.last().expect("a stroke starts with a point");
        if (point.0 - x).hypot(point.1 - y) >= 0.5 {
            stroke.points.push(point);
            cx.notify();
        }
    }

    fn end_stroke(&mut self, cx: &mut Context<Self>) {
        if self.drawing {
            self.drawing = false;
            cx.notify();
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        self.drawing = false;
        self.strokes.pop();
        cx.notify();
    }

    /// Done: a picture from the chat is uploaded and handed over with the prompt; one from the composer is saved
    /// and handed back.
    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(picture) = self.loaded() else { return };
        let (file, pixels, alpha) = (picture.file.clone(), picture.pixels.clone(), picture.alpha);
        let strokes = self.strokes.clone();
        let stem = file.file_stem().and_then(|stem| stem.to_str()).unwrap_or("Picture").to_string();
        let saved = cx
            .background_executor()
            .spawn(async move { if strokes.is_empty() { Ok(file) } else { save(&pixels, &strokes, alpha, &stem) } });
        self.saving = true;
        self.error = None;
        cx.notify();
        match &self.source {
            Source::Composer { local_id, .. } => {
                let (local_id, unchanged) = (*local_id, self.strokes.is_empty());
                cx.spawn(async move |this, cx| {
                    let saved = saved.await;
                    this.update(cx, |this, cx| match saved {
                        _ if unchanged => cx.emit(ImageEditorEvent::Close),
                        Ok(path) => cx.emit(ImageEditorEvent::Replace { local_id, path }),
                        Err(error) => this.failed(format!("The picture couldn’t be saved: {error}"), cx),
                    })
                    .ok();
                })
                .detach();
            }
            Source::Chat(_) => {
                let prompt = self.prompt.read(cx).value().trim().to_string();
                let backend = backend(cx);
                cx.spawn(async move |this, cx| {
                    let uploaded = async {
                        let path = saved.await?;
                        let prepared = cx.background_executor().spawn({
                            let path = path.clone();
                            let name =
                                path.file_name().and_then(|name| name.to_str()).unwrap_or("image.png").to_string();
                            async move { attachments::prepare(&path, &name) }
                        });
                        let (name, content_type, bytes) = prepared.await?;
                        let size = bytes.len();
                        let key = attachments::upload(backend, name.clone(), content_type.clone(), bytes)
                            .await
                            .map_err(|_| anyhow!(attachments::UPLOAD_FAILED))?;
                        anyhow::Ok(PendingAttachment {
                            local_id: 0,
                            name,
                            content_type,
                            size,
                            preview: Some(path),
                            storage_id: Some(key),
                            local_path: None,
                        })
                    }
                    .await;
                    this.update(cx, |this, cx| match uploaded {
                        Ok(attachment) => cx.emit(ImageEditorEvent::Send { attachment, prompt }),
                        Err(error) => this.failed(error.to_string(), cx),
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn render_tools(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let divider = || div().w(px(1.)).h(px(20.)).mx(px(space::XS)).bg(colors.border);
        let swatches = COLORS.iter().enumerate().map(|(index, (hex, label))| {
            let selected = index == self.color;
            let label = SharedString::from(*label);
            div()
                .id(SharedString::from(format!("pen-color-{index}")))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::FULL))
                .border_2()
                .border_color(if selected { colors.text } else { gpui::transparent_black() })
                .cursor_pointer()
                .child(
                    div()
                        .size(px(18.))
                        .rounded(px(radius::FULL))
                        .bg(Hsla::from(gpui::rgb(*hex)))
                        .border_1()
                        .border_color(colors.border),
                )
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.color = index;
                    cx.notify();
                }))
        });
        let widths = WIDTHS.iter().enumerate().map(|(index, (width, label))| {
            let selected = index == self.width;
            let label = SharedString::from(*label);
            div()
                .id(SharedString::from(format!("pen-width-{index}")))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(radius::FULL))
                .when(selected, |this| this.bg(colors.selected_fill))
                .hover(|style| style.bg(colors.selected_fill))
                .cursor_pointer()
                .child(div().size(px(width + 2.)).rounded(px(radius::FULL)).bg(colors.text))
                .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.width = index;
                    cx.notify();
                }))
        });
        let empty = self.strokes.is_empty();
        div()
            .flex()
            .items_center()
            .gap(px(space::XXS))
            .px(px(space::SM))
            .py(px(space::XXS))
            .rounded(px(radius::FULL))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.border)
            .children(swatches)
            .child(divider())
            .children(widths)
            .child(divider())
            .child(
                IconButton::new("editor-undo", "rotate-ccw")
                    .small()
                    .tooltip("Undo (⌘Z)")
                    .disabled(empty)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(cx))),
            )
            .child(
                IconButton::new("editor-clear", "trash-2").small().tooltip("Clear drawing").disabled(empty).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.strokes.clear();
                        this.drawing = false;
                        cx.notify();
                    }),
                ),
            )
    }

    fn render_stage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let view = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    if this.stage != Some(bounds) {
                        this.stage = Some(bounds);
                        cx.notify();
                    }
                })
                .ok();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let content = match (&self.picture, self.picture_rect(), self.stage) {
            (Some(Ok(picture)), Some(rect), Some(stage)) => {
                let strokes = self.strokes.clone();
                let picture_width = picture.pixels.width() as f32;
                let ink = canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let scale = f32::from(bounds.size.width) / picture_width;
                        let at = |(x, y): (f32, f32)| bounds.origin + gpui::point(px(x * scale), px(y * scale));
                        for stroke in &strokes {
                            let color = Hsla::from(gpui::rgb(COLORS[stroke.color].0));
                            let width = stroke.width * scale;
                            // A dot: lyon draws nothing for a line that goes nowhere.
                            if stroke.points.len() == 1 {
                                let center = at(stroke.points[0]);
                                let half = px(width / 2.);
                                let dot = Bounds {
                                    origin: center - gpui::point(half, half),
                                    size: gpui::size(half * 2., half * 2.),
                                };
                                window.paint_quad(gpui::fill(dot, color).corner_radii(half));
                                continue;
                            }
                            let options = StrokeOptions::default()
                                .with_line_width(width)
                                .with_line_cap(LineCap::Round)
                                .with_line_join(LineJoin::Round);
                            let mut path = PathBuilder::default().with_style(PathStyle::Stroke(options));
                            path.move_to(at(stroke.points[0]));
                            for point in &stroke.points[1..] {
                                path.line_to(at(*point));
                            }
                            if let Ok(path) = path.build() {
                                window.paint_path(path, color);
                            }
                        }
                    },
                )
                // Pinned to the corner: an absolute element without an offset sits after the picture.
                .absolute()
                .top_0()
                .left_0()
                .size_full();
                div()
                    .absolute()
                    .left(rect.origin.x - stage.origin.x)
                    .top(rect.origin.y - stage.origin.y)
                    .w(rect.size.width)
                    .h(rect.size.height)
                    .overflow_hidden()
                    .rounded(px(radius::XS))
                    .shadow_lg()
                    .bg(colors.surface_strong)
                    .cursor(CursorStyle::Crosshair)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| this.begin_stroke(event, window, cx)),
                    )
                    .child(img(picture.render.clone()).size_full())
                    .child(ink)
                    .into_any_element()
            }
            (Some(Err(error)), _, _) => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(space::MD))
                .child(div().callout().text_color(colors.text_secondary).child(error.clone()))
                .when_some(self.quick_look_item(), |this, item| {
                    this.child(Button::secondary("editor-quick-look-fallback", "Open in Quick Look").small().on_click(
                        move |_, _, cx| {
                            file_preview::quick_look(item.clone(), cx);
                        },
                    ))
                })
                .into_any_element(),
            _ => {
                div().size_full().flex().items_center().justify_center().child(ui::spinner(24., cx)).into_any_element()
            }
        };
        div().flex_1().min_h_0().w_full().relative().child(measure).child(content)
    }

    fn quick_look_item(&self) -> Option<file_preview::Item> {
        match &self.source {
            Source::Chat(item) => Some(item.clone()),
            Source::Composer { .. } => None,
        }
    }

    fn render_bottom(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let ready = self.loaded().is_some();
        let bar = match &self.source {
            Source::Chat(_) => {
                let action = if self.saving {
                    div()
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(ui::spinner(16., cx))
                        .into_any_element()
                } else {
                    IconButton::new("editor-send", "arrow-up")
                        .primary()
                        .small()
                        .tooltip("Send")
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.finish(cx)))
                        .into_any_element()
                };
                div()
                    .w_full()
                    .flex()
                    .items_end()
                    .gap(px(space::XS))
                    .p(px(space::XS))
                    .rounded(px(20.))
                    .bg(colors.surface)
                    .border_1()
                    .border_color(colors.border)
                    .shadow_lg()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h(px(28.))
                            .flex()
                            .items_center()
                            .callout()
                            .child(Input::new(&self.prompt).appearance(false).px(px(space::SM)).py(px(4.))),
                    )
                    .child(action)
                    .into_any_element()
            }
            Source::Composer { .. } => div()
                .w_full()
                .flex()
                .justify_end()
                .gap(px(space::SM))
                .child(
                    Button::ghost("editor-cancel", "Cancel")
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ImageEditorEvent::Close))),
                )
                .child(
                    Button::primary("editor-done", "Done")
                        .loading(self.saving)
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.finish(cx))),
                )
                .into_any_element(),
        };
        div()
            .w_full()
            .max_w(px(crate::theme::MAX_CONTENT_WIDTH))
            .px(px(crate::theme::PAGE_PADDING))
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().px(px(space::SM)).footnote().text_color(colors.danger).child(error))
            })
            .child(bar)
    }
}

impl Render for ImageEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let quick_look = self.quick_look_item().filter(|_| self.loaded().is_some());
        div()
            .id("image-editor")
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&self.focus)
            .key_context("ImageEditor")
            .on_action(cx.listener(|_, _: &CloseEditor, _, cx| cx.emit(ImageEditorEvent::Close)))
            .on_action(cx.listener(|this, _: &UndoStroke, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &SendPicture, _, cx| this.finish(cx)))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| this.extend_stroke(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_stroke(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_stroke(cx)))
            .flex()
            .flex_col()
            .items_center()
            .pb(px(space::LG))
            .bg(colors.chat_background)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(space::SM))
                    // Over the whole window: the close button clears the traffic lights, and the right side matches so
                    // the tools stay in the middle.
                    .px(px(crate::app::SIDEBAR_WIDTH))
                    .pt(px(space::MD))
                    .child(
                        IconButton::new("editor-close", "x")
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(ImageEditorEvent::Close))),
                    )
                    .child(self.render_tools(cx))
                    .child(match quick_look {
                        Some(item) => IconButton::new("editor-quick-look", "eye")
                            .tooltip("Open in Quick Look")
                            .on_click(move |_, _, cx| file_preview::quick_look(item.clone(), cx))
                            .into_any_element(),
                        // Keeps the tools centred.
                        None => div().size(px(32.)).into_any_element(),
                    }),
            )
            .child(self.render_stage(cx))
            .child(self.render_bottom(cx))
    }
}

/// Reads a picture upright (EXIF orientation applied), at most `MAX_SIDE` on its long side, with the texture GPUI
/// draws it from (BGRA).
fn load(file: PathBuf) -> anyhow::Result<Picture> {
    let mut decoder = image::ImageReader::open(&file)?.with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let alpha = image.color().has_alpha();
    let long_side = image.width().max(image.height());
    if long_side > MAX_SIDE {
        let scale = MAX_SIDE as f32 / long_side as f32;
        let (width, height) =
            ((image.width() as f32 * scale).round() as u32, (image.height() as f32 * scale).round() as u32);
        image = image.resize_exact(width.max(1), height.max(1), image::imageops::FilterType::Lanczos3);
    }
    let pixels = image.to_rgba8();
    let mut bgra = pixels.clone();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let render = Arc::new(RenderImage::new(vec![image::Frame::new(bgra)]));
    Ok(Picture { file, pixels: Arc::new(pixels), render, alpha })
}

/// The picture with the strokes burnt in, written to a new temporary file named like the original.
fn save(pixels: &RgbaImage, strokes: &[Stroke], alpha: bool, stem: &str) -> anyhow::Result<PathBuf> {
    let mut image = pixels.clone();
    for stroke in strokes {
        burn(&mut image, stroke);
    }
    let folder = std::env::temp_dir().join("filo-desktop-files").join("edited").join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&folder)?;
    let stem = Path::new(stem).file_name().and_then(|name| name.to_str()).unwrap_or("Picture");
    if alpha {
        let path = folder.join(format!("{stem}.png"));
        image.save_with_format(&path, image::ImageFormat::Png)?;
        return Ok(path);
    }
    let path = folder.join(format!("{stem}.jpg"));
    let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 90);
    DynamicImage::ImageRgba8(image).to_rgb8().write_with_encoder(encoder)?;
    Ok(path)
}

/// Paints one stroke: round ends and joins, edges smoothed. Coverage is gathered for the whole stroke first, so
/// where its segments overlap it isn't laid on twice.
fn burn(image: &mut RgbaImage, stroke: &Stroke) {
    let radius = stroke.width / 2.;
    let (width, height) = (image.width() as i64, image.height() as i64);
    let reach = radius + 1.;
    let (min_x, max_x, min_y, max_y) =
        stroke.points.iter().fold((f32::MAX, f32::MIN, f32::MAX, f32::MIN), |(a, b, c, d), &(x, y)| {
            (a.min(x), b.max(x), c.min(y), d.max(y))
        });
    let x0 = ((min_x - reach).floor() as i64).clamp(0, width);
    let x1 = ((max_x + reach).ceil() as i64).clamp(0, width);
    let y0 = ((min_y - reach).floor() as i64).clamp(0, height);
    let y1 = ((max_y + reach).ceil() as i64).clamp(0, height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let row = (x1 - x0) as usize;
    let mut coverage = vec![0f32; row * (y1 - y0) as usize];
    let first = stroke.points[0];
    let segments = stroke
        .points
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .chain((stroke.points.len() == 1).then_some((first, first)));
    for (a, b) in segments {
        let sx0 = ((a.0.min(b.0) - reach).floor() as i64).clamp(x0, x1);
        let sx1 = ((a.0.max(b.0) + reach).ceil() as i64).clamp(x0, x1);
        let sy0 = ((a.1.min(b.1) - reach).floor() as i64).clamp(y0, y1);
        let sy1 = ((a.1.max(b.1) + reach).ceil() as i64).clamp(y0, y1);
        for y in sy0..sy1 {
            for x in sx0..sx1 {
                let distance = distance_to_segment((x as f32 + 0.5, y as f32 + 0.5), a, b);
                let amount = (radius + 0.5 - distance).clamp(0., 1.);
                let cell = &mut coverage[(y - y0) as usize * row + (x - x0) as usize];
                *cell = cell.max(amount);
            }
        }
    }
    let rgb = COLORS[stroke.color].0;
    let ink = [(rgb >> 16) as f32, ((rgb >> 8) & 0xff) as f32, (rgb & 0xff) as f32, 255.];
    for (index, amount) in coverage.into_iter().enumerate() {
        if amount <= 0. {
            continue;
        }
        let (x, y) = (x0 + (index % row) as i64, y0 + (index / row) as i64);
        let pixel = image.get_pixel_mut(x as u32, y as u32);
        for (channel, ink) in pixel.0.iter_mut().zip(ink) {
            *channel = (ink * amount + *channel as f32 * (1. - amount)).round() as u8;
        }
    }
}

fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length == 0. { 0. } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0., 1.) };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burn_paints_the_line_and_leaves_the_rest() {
        let mut image = RgbaImage::from_pixel(40, 20, image::Rgba([255, 255, 255, 255]));
        burn(&mut image, &Stroke { color: 0, width: 4., points: vec![(5., 10.), (35., 10.)] });
        assert_eq!(image.get_pixel(20, 10).0, [0xff, 0x3b, 0x30, 255]);
        assert_eq!(image.get_pixel(20, 2).0, [255, 255, 255, 255]);
        // Round ends reach past the last point by the radius, no further.
        assert_ne!(image.get_pixel(36, 10).0, [255, 255, 255, 255]);
        assert_eq!(image.get_pixel(39, 10).0, [255, 255, 255, 255]);
    }

    #[test]
    fn a_tap_leaves_a_dot_and_strokes_off_the_picture_are_ignored() {
        let mut image = RgbaImage::from_pixel(10, 10, image::Rgba([0, 0, 0, 255]));
        burn(&mut image, &Stroke { color: 4, width: 4., points: vec![(5., 5.)] });
        assert_eq!(image.get_pixel(5, 5).0, [255, 255, 255, 255]);
        burn(&mut image, &Stroke { color: 4, width: 4., points: vec![(-50., -50.), (-20., -30.)] });
        assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 255]);
    }
}

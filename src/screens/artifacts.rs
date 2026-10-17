// Artifacts tab: Filo's finished work as a grid of cards, newest first, two to four columns depending on the window
// width. While Claude Code is signed in to claude.ai, the header's sliders
// open the tab's settings (Share to claude.ai, artifact_settings.rs).

use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde::Deserialize;
use serde_json::json;

use super::artifact_settings::ShareSettings;
use crate::backend;
use crate::format;
use crate::nav::{self, Route};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

const MAX_GRID_WIDTH: f32 = 1040.;
const MIN_CARD_WIDTH: f32 = 220.;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCard {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub created_at: Option<f64>,
    pub opened_at: Option<f64>,
}

pub struct ArtifactsScreen {
    artifacts: Option<Result<Vec<ArtifactCard>, String>>,
    /// Share to claude.ai's settings: the header offers them only with a claude.ai account.
    share: Option<ShareSettings>,
    _sub: Task<()>,
    _share_sub: Task<()>,
    _hover: Option<Task<()>>,
}

impl ArtifactsScreen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut answers = backend::backend(cx).subscribe_raw("canvasArtifacts:list", json!({ "limit": 100 }));
        let _sub = cx.spawn_in(window, async move |this, cx| {
            let mut first = true;
            while let Some(answer) = answers.recv().await {
                let result = answer.and_then(|value| {
                    serde_json::from_value::<Vec<ArtifactCard>>(value).map_err(|error| error.to_string())
                });
                if this
                    .update_in(cx, |this, window, cx| {
                        this.artifacts = Some(result.map(|mut cards| {
                            cards.retain(|card| card.created_at.is_some_and(f64::is_finite));
                            cards.sort_by(|a, b| b.created_at.unwrap_or(0.).total_cmp(&a.created_at.unwrap_or(0.)));
                            cards
                        }));
                        if first
                            && let Some(Ok(cards)) = &this.artifacts
                            && let Some(card) = cards.first()
                        {
                            nav::prepare_artifact(card.id.clone(), window, cx);
                            first = false;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let _share_sub = backend::subscribe(
            cx,
            "artifactShare:settings",
            json!({}),
            |this: &mut Self, result: Result<ShareSettings, String>, cx| {
                this.share = result.ok();
                cx.notify();
            },
        );
        ArtifactsScreen { artifacts: None, share: None, _sub, _share_sub, _hover: None }
    }

    pub(crate) fn first_id(&self) -> Option<String> {
        match &self.artifacts {
            Some(Ok(cards)) => cards.first().map(|card| card.id.clone()),
            _ => None,
        }
    }

    fn hover(&mut self, id: String, hovering: bool, window: &mut Window, cx: &mut Context<Self>) {
        self._hover = None;
        if hovering {
            self._hover = Some(cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
                cx.update(|window, cx| nav::prepare_artifact(id, window, cx)).ok();
            }));
        }
    }
}

/// An artifact's card: its summary (or the page icon), its title and when it was made.
fn card(artifact: &ArtifactCard, cx: &Context<ArtifactsScreen>) -> AnyElement {
    let colors = *cx.colors();
    let show_text = !artifact.summary.is_empty();
    let id = artifact.id.clone();
    let hover_id = id.clone();
    let is_new = artifact.opened_at.is_none();
    div()
        .id(SharedString::from(format!("artifact-card-{}", artifact.id)))
        .flex()
        .flex_col()
        .min_w_0()
        .cursor_pointer()
        .group("artifact-card")
        .on_hover(
            cx.listener(move |this, hovering: &bool, window, cx| this.hover(hover_id.clone(), *hovering, window, cx)),
        )
        .on_click(move |_: &ClickEvent, window, cx| nav::push(Route::Artifact(id.clone()), window, cx))
        .child(
            div()
                .relative()
                .h(px(148.))
                .p(px(space::MD))
                .rounded(px(radius::LG))
                .bg(colors.surface)
                .border_1()
                .border_color(colors.border)
                .overflow_hidden()
                .group_hover("artifact-card", |style| style.border_color(colors.border_strong))
                .map(|this| {
                    if show_text {
                        this.child(
                            div()
                                .text_color(colors.text_secondary)
                                .micro()
                                .font_weight(gpui::FontWeight::NORMAL)
                                .line_clamp(7)
                                .child(artifact.summary.clone()),
                        )
                    } else {
                        this.child(div().size_full().flex().items_center().justify_center().child(ui::icon_sized(
                            "file-text",
                            26.,
                            colors.text_secondary,
                        )))
                    }
                }),
        )
        .child(
            div()
                .mt(px(space::SM + 2.))
                .flex()
                .items_center()
                .gap(px(6.))
                .when(is_new, |this| {
                    this.child(div().size(px(7.)).flex_none().rounded(px(radius::FULL)).bg(colors.accent))
                })
                .child(div().flex_1().min_w_0().callout().semibold().truncate().child(artifact.title.clone())),
        )
        .child(
            div()
                .mt(px(space::XXS))
                .footnote()
                .text_color(colors.text_secondary)
                .child(format::day_label(artifact.created_at.unwrap_or(0.)))
                .when(is_new, |this| this.child(" · New")),
        )
        .into_any_element()
}

/// Three stacked sheets.
fn empty_illustration(cx: &App) -> AnyElement {
    let colors = *cx.colors();
    let sheet = |left: f32, top: f32| {
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(158.))
            .h(px(170.))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
    };
    let line = |width: f32, height: f32, color: gpui::Hsla| div().w(px(width)).h(px(height)).rounded(px(3.)).bg(color);
    div()
        .relative()
        .w(px(216.))
        .h(px(190.))
        .mb(px(18.))
        .child(sheet(10., 4.))
        .child(sheet(48., 4.))
        .child(
            sheet(29., 15.)
                .bg(colors.surface_strong)
                .border_color(colors.border_strong)
                .p(px(18.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .size(px(38.))
                        .rounded(px(11.))
                        .bg(colors.selected_fill)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(ui::icon_sized("file-text", 19., colors.text)),
                )
                .child(
                    div()
                        .mt(px(27.))
                        .flex()
                        .flex_col()
                        .gap(px(9.))
                        .child(line(88., 7., colors.text.opacity(0.85)))
                        .child(line(120., 5., colors.border_strong))
                        .child(line(94., 5., colors.border_strong)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .child(div().size(px(6.)).rounded(px(3.)).bg(colors.text_secondary))
                        .child(line(42., 4., colors.border_strong)),
                ),
        )
        .into_any_element()
}

impl Render for ArtifactsScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let available = (ui::content_width() - crate::theme::PAGE_PADDING * 2.).min(MAX_GRID_WIDTH);
        let columns = (((available + space::LG) / (MIN_CARD_WIDTH + space::LG)).floor() as u16).clamp(2, 4);
        let body: AnyElement = match &self.artifacts {
            // Cards while they load: the preview box and its title and details.
            None => div()
                .grid()
                .grid_cols(columns)
                .gap(px(space::LG))
                .children((0..columns as usize * 2).map(|index| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(space::SM))
                        .child(ui::skeleton(gpui::relative(1.), 148., radius::LG))
                        .child(ui::skeleton(gpui::relative([0.7, 0.55, 0.8, 0.6][index % 4]), 11., 5.).mt(px(2.)))
                        .child(ui::skeleton(px(72.), 9., 4.5).secondary())
                }))
                .into_any_element(),
            Some(Err(error)) => ui::banner(error.clone(), ui::Tone::Danger, cx).into_any_element(),
            Some(Ok(cards)) if cards.is_empty() => div()
                .pt(px(space::HUGE))
                .flex()
                .flex_col()
                .items_center()
                .child(empty_illustration(cx))
                .child(
                    div()
                        .max_w(px(520.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(space::SM))
                        .text_center()
                        .child(div().title1().child("A home for finished work."))
                        .child(div().max_w(px(440.)).callout().text_color(colors.text_secondary).child(
                            "Ask Filo to turn an idea into a document, plan, website, diagram, or code. The result will appear here when it is ready.",
                        )),
                )
                .into_any_element(),
            Some(Ok(cards)) => div()
                .grid()
                .grid_cols(columns)
                .gap(px(space::LG))
                .children(cards.iter().map(|artifact| card(artifact, cx)))
                .into_any_element(),
        };
        let settings: Vec<AnyElement> = self
            .share
            .as_ref()
            .filter(|share| share.account.is_some())
            .map(|_| {
                ui::IconButton::new("artifacts-settings", "sliders-horizontal")
                    .tooltip("Artifact settings")
                    .on_click(|_: &ClickEvent, window, cx| nav::push(Route::ArtifactSettings, window, cx))
                    .into_any_element()
            })
            .into_iter()
            .collect();
        div()
            .id("artifacts-scroll")
            .size_full()
            .overflow_y_scroll()
            .when_some(crate::smoke::page_scroll("artifacts"), |this, handle| this.track_scroll(&handle))
            .child(crate::smoke::tracked(
                "artifacts-end",
                ui::page_column()
                    .w(px(ui::content_width().min(MAX_GRID_WIDTH + crate::theme::PAGE_PADDING * 2.)))
                    .pb(px(48.))
                    .child(div().mb(px(space::LG)).child(ui::page_header("Artifacts", settings)))
                    .child(body)
                    .children(crate::smoke::page_end("artifacts")),
            ))
    }
}

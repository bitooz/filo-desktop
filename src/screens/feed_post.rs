// One post read for the user, /feed/[id] (src/features/feed/feed-post-screen.tsx): photos, who shared it and their
// note, the summary and key points, the way out to the page, and Ask (the agent Feed settings picks, in its own
// conversation).

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::json;

use super::feed_common::{self as common, FeedPost, FeedSettings, Notice, PostHost};
use crate::backend;
use crate::format;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};

pub struct FeedPostScreen {
    id: String,
    post: Option<Result<Option<FeedPost>, String>>,
    asking: Option<String>,
    /// Who answers Ask (`feed:settingsMine`).
    settings: Option<FeedSettings>,
    opened_at: f64,
    notice: Notice,
    _sub: Task<()>,
    _settings: Task<()>,
}

impl PostHost for FeedPostScreen {
    fn notice(&mut self) -> &mut Notice {
        &mut self.notice
    }
    fn set_asking(&mut self, post_id: Option<String>) {
        self.asking = post_id;
    }
}

impl FeedPostScreen {
    pub fn new(id: String, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _sub = backend::subscribe(
            cx,
            "feed:postMine",
            json!({ "postId": id }),
            |this: &mut Self, result: Result<Option<FeedPost>, String>, cx| {
                this.post = Some(result);
                cx.notify();
            },
        );
        let _settings = backend::subscribe(
            cx,
            "feed:settingsMine",
            json!({}),
            |this: &mut Self, result: Result<Option<FeedSettings>, String>, cx| {
                if let Ok(settings) = result {
                    this.settings = settings;
                    cx.notify();
                }
            },
        );
        FeedPostScreen {
            id,
            post: None,
            asking: None,
            settings: None,
            opened_at: format::now_ms(),
            notice: Notice::default(),
            _sub,
            _settings,
        }
    }

    fn set_liked(&mut self, liked: bool, cx: &mut Context<Self>) {
        backend::call(
            cx,
            "feed:setLikedMine",
            json!({ "postId": self.id, "liked": liked }),
            |this: &mut Self, result: Result<serde_json::Value, String>, cx| {
                if result.is_err() {
                    common::notify(this, "Couldn't save that. Try again.", cx);
                }
            },
        );
    }

    fn set_saved(&mut self, saved: bool, cx: &mut Context<Self>) {
        backend::call(
            cx,
            "feed:setSavedMine",
            json!({ "postId": self.id, "saved": saved }),
            move |this: &mut Self, result: Result<serde_json::Value, String>, cx| match result {
                Ok(_) => common::notify(this, if saved { "Saved to Notes." } else { "Removed from Notes." }, cx),
                Err(_) => common::notify(this, "Couldn't save that. Try again.", cx),
            },
        );
    }

    /// The post's photo, when it has one.
    fn render_cover(&self, image: Option<&str>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = *cx.colors();
        let image = image.filter(|image| !image.is_empty())?;
        Some(
            common::aspect(
                div().relative().w_full().rounded(px(radius::LG)).bg(colors.surface_strong).child(common::cover_image(
                    image,
                    radius::LG,
                    colors.background,
                )),
                1.91,
            )
            .into_any_element(),
        )
    }
}

impl Render for FeedPostScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.colors();
        let detail = match &self.post {
            None => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(ui::sub_page_header("feed-post", "", vec![], cx))
                    // The post while it loads: cover, title and text.
                    .child(
                        ui::page_column()
                            .pt(px(space::LG))
                            .flex()
                            .flex_col()
                            .gap(px(space::LG))
                            .child(ui::skeleton(
                                px(ui::column_width()),
                                (ui::column_width() / 1.91).round(),
                                radius::LG,
                            ))
                            .child(ui::skeleton_document()),
                    )
                    .into_any_element();
            }
            Some(Err(error)) => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(ui::sub_page_header("feed-post", "", vec![], cx))
                    .child(ui::page_column().pt(px(space::XL)).child(ui::banner(error.clone(), ui::Tone::Danger, cx)))
                    .into_any_element();
            }
            Some(Ok(None)) => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(ui::sub_page_header("feed-post", "", vec![], cx))
                    .child(ui::empty_state("newspaper", "This post is gone", None, None, cx))
                    .into_any_element();
            }
            Some(Ok(Some(detail))) => detail.clone(),
        };
        let post = &detail;
        let now = self.opened_at;

        let header_url = post.url.clone();
        // A post that's only an agent's words has no link to open or copy.
        let has_link = !post.url.is_empty();
        let header = ui::sub_page_header(
            "feed-post",
            post.source_name.clone(),
            has_link
                .then(|| {
                    IconButton::new("feed-post-open", "external-link")
                        .tooltip("Open in browser")
                        .on_click(move |_, _, cx| common::open_post(&header_url, cx))
                        .into_any_element()
                })
                .into_iter()
                .collect(),
            cx,
        );

        let lead = self.render_cover(post.image_url.as_deref(), cx);

        // Text here gets pixel widths: wrapped text in a flexible width is measured one line
        // tall, so what follows it was drawn over its last lines.
        let width = ui::column_width();
        let summary = match &post.summary {
            Some(summary) => div()
                .flex()
                .flex_col()
                .gap(px(space::MD))
                .child(div().w(px(width)).body().child(summary.clone()))
                .children(post.points.iter().map(|point| {
                    div()
                        .flex()
                        .items_start()
                        .gap(px(space::MD))
                        .child(common::bullet(6., 9., colors.text))
                        .child(div().w(px(width - 6. - space::MD)).callout().child(point.clone()))
                }))
                .into_any_element(),
            // Without a summary, the post is what its author wrote (the note above).
            None => div().into_any_element(),
        };

        let (open_url, ask_id, like, save) = (post.url.clone(), post.id.clone(), !post.liked, !post.saved);
        let share_url = post.url.clone();
        // One row on desktop: the two actions, then like / save / share.
        let actions = div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .pt(px(space::SM))
            .when(has_link, |this| {
                this.child(
                    Button::primary("feed-post-browser", "Open in browser")
                        .icon("external-link")
                        .on_click(move |_, _, cx| common::open_post(&open_url, cx)),
                )
            })
            .child(
                Button::secondary("feed-post-ask", FeedSettings::ask_label(self.settings.as_ref()))
                    .icon("message-circle")
                    .loading(self.asking.as_deref() == Some(post.id.as_str()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| common::ask_about_post(this, &ask_id, window, cx)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_end()
                    .gap(px(space::XS))
                    .child(
                        IconButton::new("feed-post-like", if post.liked { "heart-filled" } else { "heart" })
                            .tooltip(if post.liked { "Unlike" } else { "Like" })
                            .when(post.liked, |this| this.color(colors.accent))
                            .on_click(cx.listener(move |this, _, _, cx| this.set_liked(like, cx))),
                    )
                    // Saved to Notes: not while Notes is off (pages.rs).
                    .when(crate::pages::get(cx).notes, |this| {
                        this.child(
                            IconButton::new("feed-post-save", if post.saved { "bookmark-filled" } else { "bookmark" })
                                .tooltip(if post.saved { "Remove from Notes" } else { "Save to Notes" })
                                .when(post.saved, |this| this.color(colors.accent))
                                .on_click(cx.listener(move |this, _, _, cx| this.set_saved(save, cx))),
                        )
                    })
                    .when(has_link, |this| {
                        this.child(
                            IconButton::new("feed-post-share", "link")
                                .tooltip("Copy link")
                                .on_click(cx.listener(move |this, _, _, cx| common::share_post(this, &share_url, cx))),
                        )
                    }),
            );

        let content = ui::page_column()
            .pt(px(space::LG))
            .pb(px(space::XXXL))
            .flex()
            .flex_col()
            .gap(px(space::LG))
            .children(lead)
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    // A pixel width: percentage widths don't resolve for text in this column.
                    .child(div().w(px(ui::column_width())).title2().child(post.title.clone()))
                    .child(div().footnote().text_color(colors.text_secondary).child(format!(
                        "{} · {}",
                        post.source_name,
                        format::age(post.created_at, now)
                    ))),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_start()
                    .gap(px(space::SM))
                    .child(div().pt(px(1.)).child(common::author_avatar(post, 20., cx)))
                    .child(
                        div()
                            .w(px(width - 20. - space::SM))
                            .callout()
                            .text_color(colors.text_secondary)
                            .child(div().semibold().text_color(colors.text).child(format!(
                                "Shared by {}",
                                if post.author.trim().is_empty() { "an agent" } else { post.author.trim() }
                            )))
                            .when(!post.note.trim().is_empty(), |this| this.child(post.note.clone())),
                    ),
            )
            .child(summary)
            .when_some(post.event_starts_at.filter(|_| post.kind == "event"), |this, starts_at| {
                this.child(
                    common::event_row("feed-post-event", &post.id, starts_at, post.reminded, true, now, cx)
                        .p(px(space::MD))
                        .rounded(px(radius::LG))
                        .bg(colors.surface),
                )
            })
            .child(actions);

        ui::sub_page(header, div().id("feed-post-scroll").flex_1().min_h_0().overflow_y_scroll().child(content), cx)
            .relative()
            .children(common::render_notice(&self.notice, cx))
            .into_any_element()
    }
}

// Glance posts in the Feed (the "Feed with glances" mock): a glance an agent posted with `publish_feed_glance`
// (crates/filo-local feed_mcp.rs), shown live in the Feed as a chat shows one above a reply (chat/glance.rs): who posted
// it, its title and note, the glance's own page (hover, switch, sort and click inside it), then Ask about this, Save,
// Open larger (the chat's larger view) and Hide.
//
// The page is the chat's: the same document from `glances:get`, in the same native web view, placed over the post's
// box each frame and hidden by the window's sweep when the post scrolls away. In the Feed it also gets the choices'
// runtime (`choices_head`): a click on anything with `data-choice` marks it picked and sends the pick, which goes back
// to the agent that posted the glance as a message from the user (`feed:chooseInGlance`). An agent changing the glance
// gives the post a new one (`update_feed_glance`), so the post fetches and draws that, in the same place.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use futures::StreamExt as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde::Deserialize;
use serde_json::json;

use super::FeedScreen;
use super::common::{self, FeedPost};
use crate::backend;
use crate::format;
use crate::screens::chat::GlanceRef;
use crate::screens::chat::conversation::glance::{
    self, FeedSheet, GlanceData, NativePage, PageElement, PageMessage, ThemeCss,
};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button};

/// Most glance pages the Feed keeps alive (one web view each): the one longest out of sight goes first.
const MAX_PAGES: usize = 4;
/// The card's padding: its page is the column less this either side, and its border.
const CARD_PAD: f32 = 14.;

/// Where a glance's page stands.
enum Fetch {
    Loading,
    Ready(Rc<GlanceData>),
    Failed,
}

struct Page {
    page: Rc<NativePage>,
    used: Instant,
    _ipc: Task<()>,
}

/// The Feed's glances: their pages, and the picks being sent.
#[derive(Default)]
pub(super) struct FeedGlances {
    /// By glance id.
    fetched: HashMap<String, Fetch>,
    pages: HashMap<String, Page>,
    /// Picks made here, by post, until the Feed has them; and the posts whose pick is on its way.
    picked: HashMap<String, String>,
    sending: HashSet<String>,
}

#[derive(Deserialize)]
struct Chosen {
    #[serde(default)]
    author: String,
}

/// Put in a Feed glance's head: clicks on `data-choice` send the pick (chat/glance.rs host page), the one picked is
/// marked (`filo-picked`, `aria-pressed`), and the last pick is marked when the page opens.
fn choices_head(picked: Option<&str>) -> String {
    // As a script value: JSON, with every "<" escaped, so nothing in it can close the script.
    let picked = serde_json::to_string(&picked).unwrap_or_else(|_| "null".into()).replace('<', "\\u003c");
    format!(
        "<style>[data-choice]{{cursor:pointer}}</style><script>(()=>{{const picked={picked};\
const mark=v=>{{for(const el of document.querySelectorAll('[data-choice]')){{const on=el.dataset.choice===v;el.classList.toggle('filo-picked',on);el.setAttribute('aria-pressed',on?'true':'false')}}}};\
const ready=()=>{{if(picked)mark(picked)}};if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',ready);else ready();\
document.addEventListener('click',e=>{{const el=e.target.closest&&e.target.closest('[data-choice]');if(!el)return;e.preventDefault();\
const v=String(el.dataset.choice||'').trim().slice(0,200);if(!v)return;mark(v);parent.postMessage({{filo:'choice',value:v}},'*')}})}})();</script>"
    )
}

/// The glance in words, as Copy and Ask give it: its title (and when it's from), then its text.
fn glance_words(glance: &GlanceRef, data: &GlanceData) -> String {
    match &glance.as_of {
        Some(as_of) => format!("{} ({as_of})\n{}", glance.title, data.text),
        None => format!("{}\n{}", glance.title, data.text),
    }
}

impl FeedScreen {
    /// The Feed's glance pages that no post shows any more (changed in place, or gone) go.
    pub(super) fn forget_old_glances(&mut self) {
        let Some(Ok(feed)) = &self.feed else { return };
        let current: HashSet<&str> =
            feed.posts.iter().filter_map(|post| post.glance.as_ref().map(|glance| glance.glance_id.as_str())).collect();
        self.glances.pages.retain(|id, _| current.contains(id.as_str()));
        self.glances.fetched.retain(|id, _| current.contains(id.as_str()));
        let picks: HashMap<&str, Option<&str>> =
            feed.posts.iter().map(|post| (post.id.as_str(), post.glance_choice.as_deref())).collect();
        // A pick the Feed has now needn't be held here.
        self.glances
            .picked
            .retain(|post, choice| picks.get(post.as_str()).is_some_and(|stored| *stored != Some(choice.as_str())));
    }

    /// What a glance post shows, for its row's measure: loading, failed, its page's height and the pick line.
    pub(super) fn glance_sig(&self, post: &FeedPost) -> (bool, bool, u32, Option<String>) {
        let Some(glance) = &post.glance else { return (false, false, 0, None) };
        let fetch = self.glances.fetched.get(&glance.glance_id);
        (
            matches!(fetch, None | Some(Fetch::Loading)),
            matches!(fetch, Some(Fetch::Failed)),
            glance::known_height(&glance.glance_id).unwrap_or(glance::DEFAULT_HEIGHT).round() as u32,
            self.glance_pick(post).map(str::to_string),
        )
    }

    /// The pick a glance post shows: one made here a moment ago, else the Feed's.
    fn glance_pick<'a>(&'a self, post: &'a FeedPost) -> Option<&'a str> {
        self.glances.picked.get(&post.id).map(String::as_str).or(post.glance_choice.as_deref())
    }

    fn glance_data(&self, glance_id: &str) -> Option<Rc<GlanceData>> {
        match self.glances.fetched.get(glance_id) {
            Some(Fetch::Ready(data)) => Some(data.clone()),
            _ => None,
        }
    }

    /// Fetches a glance's page the first time its post is drawn.
    fn fetch_glance(&mut self, glance_id: &str, cx: &mut Context<Self>) {
        if self.glances.fetched.contains_key(glance_id) {
            return;
        }
        self.glances.fetched.insert(glance_id.to_string(), Fetch::Loading);
        let id = glance_id.to_string();
        backend::call(
            cx,
            "glances:get",
            json!({ "glanceId": glance_id }),
            move |this: &mut Self, result: Result<Option<GlanceData>, String>, cx| {
                let fetch = match result {
                    Ok(Some(data)) => Fetch::Ready(Rc::new(data)),
                    _ => Fetch::Failed,
                };
                this.glances.fetched.insert(id, fetch);
                cx.notify();
            },
        );
    }

    /// The page for a post's glance, made when it's first drawn while the window is in front (making a web view brings
    /// the app forward), and again when the theme changed. Its height comes back as it measures itself; a pick in it is
    /// sent (`choose`).
    fn glance_page(
        &mut self,
        post_id: &str,
        glance_id: &str,
        data: &GlanceData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Rc<NativePage>> {
        let theme = ThemeCss::new(cx);
        if let Some(page) = self.glances.pages.get_mut(glance_id)
            && page.page.theme == theme.key
        {
            page.used = Instant::now();
            return Some(page.page.clone());
        }
        self.glances.pages.remove(glance_id);
        if !window.is_window_active() {
            return None;
        }
        let picked = self.post(post_id).and_then(|post| self.glance_pick(post)).map(str::to_string);
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PageMessage>();
        let page = NativePage::open_with(window, &data.document, &theme, false, &choices_head(picked.as_deref()), tx)?;
        let (id, post) = (glance_id.to_string(), post_id.to_string());
        let weak = Rc::downgrade(&page);
        let handle = window.window_handle();
        let ipc = cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                let updated = match message {
                    PageMessage::Size { height, .. } => {
                        if !glance::note_height(&id, height) {
                            continue;
                        }
                        this.update(cx, |_, cx| cx.notify())
                    }
                    PageMessage::Painted => {
                        if weak.upgrade().is_none_or(|page| page.ready.replace(true)) {
                            continue;
                        }
                        this.update(cx, |_, cx| cx.notify())
                    }
                    PageMessage::Escape => {
                        glance::page_escape(handle, cx);
                        continue;
                    }
                    PageMessage::Choice(choice) => this.update(cx, |this, cx| this.choose(post.clone(), choice, cx)),
                };
                if updated.is_err() {
                    break;
                }
            }
        });
        // One web view per glance on screen: the one longest out of sight goes when there are too many.
        if self.glances.pages.len() >= MAX_PAGES {
            let oldest = self.glances.pages.iter().min_by_key(|(_, page)| page.used).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest.filter(|oldest| self.glances.pages[oldest].used.elapsed().as_millis() > 250) {
                self.glances.pages.remove(&oldest);
            }
        }
        self.glances.pages.insert(glance_id.to_string(), Page { page: page.clone(), used: Instant::now(), _ipc: ipc });
        Some(page)
    }

    /// A pick in a glance's choices goes to the agent that posted it, as a message from the user that starts its turn.
    /// The same pick again (a second click) sends nothing.
    fn choose(&mut self, post_id: String, choice: String, cx: &mut Context<Self>) {
        let Some(post) = self.post(&post_id) else { return };
        if self.glances.sending.contains(&post_id) || self.glance_pick(post) == Some(choice.as_str()) {
            return;
        }
        let author =
            if post.author.trim().is_empty() { "the agent".to_string() } else { post.author.trim().to_string() };
        self.glances.picked.insert(post_id.clone(), choice.clone());
        cx.notify();
        // The feed fixture has no backend: the pick is only shown.
        #[cfg(feature = "dev")]
        if self.feed_pinned {
            common::notify(self, format!("Sent to {author}. It'll reply in its chat."), cx);
            return;
        }
        self.glances.sending.insert(post_id.clone());
        backend::call(
            cx,
            "feed:chooseInGlance",
            json!({ "postId": post_id, "choice": choice }),
            move |this: &mut Self, result: Result<Chosen, String>, cx| {
                this.glances.sending.remove(&post_id);
                match result {
                    Ok(chosen) => {
                        let author = if chosen.author.trim().is_empty() { author.clone() } else { chosen.author };
                        common::notify(this, format!("Sent to {author}. It'll reply in its chat."), cx);
                    }
                    Err(error) => {
                        this.glances.picked.remove(&post_id);
                        common::notify(this, common::error_text(&error, "Couldn't send your pick. Try again."), cx);
                    }
                }
                cx.notify();
            },
        );
    }

    /// Ask about this: the Feed's Ask panel on the post, with what the glance shows in words for the agent.
    fn ask_about_glance(&mut self, post: &FeedPost, window: &mut Window, cx: &mut Context<Self>) {
        let mut post = post.clone();
        if let Some(glance) = post.glance.clone()
            && let Some(data) = self.glance_data(&glance.glance_id)
        {
            post.summary = Some(glance_words(&glance, &data));
        }
        self.ask(post, window, cx);
    }

    /// Open larger: the chat's larger view, with this post's Copy, Ask and choices.
    fn open_glance_larger(&mut self, post: &FeedPost, now: f64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(glance) = post.glance.clone() else { return };
        let Some(data) = self.glance_data(&glance.glance_id) else { return };
        let weak = cx.entity().downgrade();
        let (ask_view, choose_view, ask_post, post_id) = (weak.clone(), weak, post.clone(), post.id.clone());
        let author = if post.author.trim().is_empty() { "an agent" } else { post.author.trim() };
        let from = FeedSheet {
            copy: glance_words(&glance, &data),
            from: format!("Posted to your Feed by {author} · {}", format::age(post.created_at, now)),
            head: choices_head(self.glance_pick(post)),
            ask: Rc::new(move |window: &mut Window, cx: &mut App| {
                ask_view.update(cx, |this, cx| this.ask_about_glance(&ask_post, window, cx)).ok();
            }),
            choose: Rc::new(move |choice: String, _: &mut Window, cx: &mut App| {
                choose_view.update(cx, |this, cx| this.choose(post_id.clone(), choice, cx)).ok();
            }),
        };
        glance::open_feed_sheet(glance, data, from, window, cx);
    }

    /// A glance post: who posted it, its title and note, the live glance, the pick sent from it, then Ask about this,
    /// Save, Open larger and Hide.
    pub(super) fn render_glance_post(
        &mut self,
        post: &FeedPost,
        now: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Hidden this session: the undo row every post has.
        if self.hidden_now.contains_key(&post.id) {
            return self.render_post(post, now, window, cx);
        }
        let Some(glance) = post.glance.clone() else { return self.render_post(post, now, window, cx) };
        let colors = *cx.colors();
        let id = post.id.clone();
        self.fetch_glance(&glance.glance_id, cx);
        let (loading, failed, height, _) = self.glance_sig(post);
        let data = self.glance_data(&glance.glance_id);
        let saved = self.save_overrides.get(&post.id).copied().unwrap_or(post.saved);
        let (asking, adding) = self.ask_state(&post.id, cx);
        let page_width = (self.column_width() - 2. * CARD_PAD - 2.).max(1.);

        let author =
            if post.author.trim().is_empty() { "An agent".to_string() } else { post.author.trim().to_string() };
        let byline = div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .child(common::author_avatar(post, 20., cx))
            .child(div().min_w_0().footnote().semibold().text_color(colors.text).truncate().child(author.clone()))
            .child(
                div()
                    .flex_none()
                    .caption()
                    .text_color(colors.text_tertiary)
                    .child(format!("· {}", format::age(post.created_at, now))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(space::XXS))
                    .px(px(space::SM))
                    .py(px(1.))
                    .rounded(px(radius::FULL))
                    .bg(colors.surface_strong)
                    .caption()
                    .text_color(colors.text_secondary)
                    .child(ui::icon_sized(
                        glance::icon_for(
                            glance.icon.as_deref().or(data.as_ref().and_then(|data| data.icon.as_deref())),
                        ),
                        12.,
                        colors.text_secondary,
                    ))
                    .child("Glance"),
            );

        let title = div()
            .flex()
            .items_baseline()
            .gap(px(space::SM))
            .child(div().min_w_0().headline().child(post.title.clone()))
            .children(
                glance
                    .as_of
                    .clone()
                    .map(|as_of| div().flex_none().caption().text_color(colors.text_tertiary).child(as_of)),
            );

        let body: AnyElement =
            if loading {
                glance::skeleton_bars(&colors).into_any_element()
            } else if failed || data.is_none() {
                div()
                    .py(px(space::XS))
                    .subhead()
                    .text_color(colors.text_secondary)
                    .child("This glance couldn't be loaded.")
                    .into_any_element()
            } else {
                let data = data.clone().expect("checked above");
                let page = self.glance_page(&post.id, &glance.glance_id, &data, window, cx);
                let ready = page.as_ref().is_some_and(|page| page.ready.get());
                let sideways = page.clone();
                div()
                    .id(SharedString::from(format!("glance-page-{id}")))
                    .relative()
                    .w(px(page_width))
                    .h(px(height as f32))
                    // The wheel reaches GPUI over the page: a mostly sideways one scrolls content wider than the card
                    // inside it, anything else scrolls the Feed.
                    .on_scroll_wheel(move |event: &gpui::ScrollWheelEvent, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                        if let Some(page) = sideways.as_ref().filter(|_| dx.abs() > dy.abs()) {
                            page.scroll_sideways(-dx);
                            cx.stop_propagation();
                        }
                    })
                    .when(!ready, |this| this.child(glance::skeleton_bars(&colors)))
                    .children(page.map(|page| {
                        div().absolute().inset_0().child(PageElement { page, clip_bottom: None, show: true })
                    }))
                    .into_any_element()
            };

        let pick = self.glance_pick(post).map(|choice| {
            let sending = self.glances.sending.contains(&post.id);
            div()
                .flex()
                .items_center()
                .gap(px(space::XS))
                .footnote()
                .text_color(if sending { colors.text_secondary } else { colors.success })
                .child(if sending {
                    ui::spinner(12., cx).into_any_element()
                } else {
                    ui::icon_sized("check", 14., colors.success).into_any_element()
                })
                .child(if sending {
                    format!("Sending \u{201c}{choice}\u{201d} to {author}\u{2026}")
                } else {
                    format!("You picked \u{201c}{choice}\u{201d}. Sent to {author}.")
                })
        });

        let actions = {
            let (ask_post, save_id, open_post, hide_id, hide_topic) =
                (post.clone(), id.clone(), post.clone(), id.clone(), post.topic_label.clone());
            div()
                .flex()
                .items_center()
                .gap(px(space::XXS))
                .ml(px(-space::SM))
                .child(crate::smoke::tracked(
                    format!("ask-{id}"),
                    // Filled while the chat beside the posts is about this post (a click closes it); while it's about
                    // others, a click adds this one.
                    (if asking { Button::primary } else { Button::ghost })(
                        SharedString::from(format!("ask-{id}")),
                        if adding { "Add to chat" } else { "Ask about this" },
                    )
                    .small()
                    .icon(if adding { "plus" } else { "message-circle" })
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.ask_about_glance(&ask_post, window, cx)
                        }),
                    ),
                ))
                // Saved to Notes: not while Notes is off (pages.rs).
                .when(crate::pages::get(cx).notes, |this| {
                    this.child(crate::smoke::tracked(
                        format!("save-{id}"),
                        Button::ghost(SharedString::from(format!("save-{id}")), if saved { "Saved" } else { "Save" })
                            .small()
                            .icon(if saved { "bookmark-filled" } else { "bookmark" })
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| this.save(save_id.clone(), !saved, cx)),
                            ),
                    ))
                })
                .child(crate::smoke::tracked(
                    format!("open-glance-{id}"),
                    Button::ghost(SharedString::from(format!("open-glance-{id}")), "Open larger")
                        .small()
                        .icon("arrow-up-right")
                        .disabled(data.is_none())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_glance_larger(&open_post, format::now_ms(), window, cx)
                        })),
                ))
                .child(div().flex_1())
                .child(crate::smoke::tracked(
                    format!("hide-{id}"),
                    Button::ghost(SharedString::from(format!("hide-{id}")), "Hide").small().icon("ban").on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.hide(hide_id.clone(), &hide_topic, "not_interested", cx)
                        }),
                    ),
                ))
        };

        div()
            .id(SharedString::from(format!("glance-post-{id}")))
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .pt(px(space::MD))
            .px(px(CARD_PAD))
            .pb(px(space::SM))
            .rounded(px(radius::LG))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.border.opacity(0.7))
            .child(byline)
            .child(
                div().flex().flex_col().gap(px(space::XXS)).child(title).when(!post.note.trim().is_empty(), |this| {
                    this.child(div().subhead().text_color(colors.text_secondary).child(post.note.clone()))
                }),
            )
            .child(
                div()
                    .mt(px(space::XS))
                    .pt(px(space::MD))
                    .border_t_1()
                    .border_color(colors.border.opacity(0.6))
                    .child(body),
            )
            .children(pick)
            .child(actions)
            .into_any_element()
    }
}

// --- Scripted checks ---

impl FeedScreen {
    /// A glance post's page as last placed: whether it has said how tall it is, and what of it shows, in window points.
    #[cfg(feature = "dev")]
    pub fn drive_glance_page(
        &self,
        post_id: &str,
        window: &Window,
    ) -> Option<(bool, Option<gpui::Bounds<gpui::Pixels>>)> {
        let glance = self.post(post_id)?.glance.as_ref()?;
        let page = &self.glances.pages.get(&glance.glance_id)?.page;
        Some((page.ready.get(), page.on_screen(window)))
    }

    /// Whether only glances are listed.
    #[cfg(feature = "dev")]
    pub fn drive_glances_only(&self) -> bool {
        self.glances_only
    }

    /// The listed glance posts' ids, in order.
    #[cfg(feature = "dev")]
    pub fn drive_glance_posts(&self) -> Vec<String> {
        let Some(Ok(feed)) = &self.feed else { return Vec::new() };
        self.listed_posts(&feed.posts)
            .into_iter()
            .filter(|post| post.glance.is_some())
            .map(|post| post.id.clone())
            .collect()
    }

    /// The listed glance posts with the glance each shows now.
    #[cfg(feature = "dev")]
    pub fn drive_glance_ids(&self) -> Vec<(String, String)> {
        let Some(Ok(feed)) = &self.feed else { return Vec::new() };
        self.listed_posts(&feed.posts)
            .into_iter()
            .filter_map(|post| Some((post.id.clone(), post.glance.as_ref()?.glance_id.clone())))
            .collect()
    }

    /// The pick a glance post shows, and whether it's still on its way.
    #[cfg(feature = "dev")]
    pub fn drive_pick(&self, post_id: &str) -> (Option<String>, bool) {
        (
            self.post(post_id).and_then(|post| self.glance_pick(post)).map(str::to_string),
            self.glances.sending.contains(post_id),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_runtime_marks_the_last_pick_and_cannot_be_closed_early() {
        let head = choices_head(Some("Ship </script> tonight"));
        assert!(head.contains("const picked=\"Ship \\u003c/script> tonight\";"), "{head}");
        assert_eq!(head.matches("</script>").count(), 1);
        assert!(choices_head(None).contains("const picked=null;"));
        assert!(head.contains("parent.postMessage({filo:'choice',value:v},'*')"));
    }
}

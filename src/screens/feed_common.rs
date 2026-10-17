// Feed rules and pieces shared by the Feed tab, the post page and the feed settings page: the reply shapes of the
// `feed:*` functions (local_backend/feed.rs), the labels and filters, the short notice at the bottom of the page and
// the post actions.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Context, Div, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled, Task, Window, div, prelude::FluentBuilder as _, px,
};
use serde::Deserialize;
use serde_json::json;

use crate::backend;
use crate::format;
use crate::nav;
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui;

// ---------------------------------------------------------------------------
// Reply shapes (local_backend/feed.rs)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedPost {
    #[serde(rename = "_id")]
    pub id: String,
    pub topic_key: String,
    pub topic_label: String,
    /// article | video | thread | event | glance
    pub kind: String,
    pub title: String,
    pub note: String,
    pub why: String,
    pub url: String,
    pub source_name: String,
    pub image_url: Option<String>,
    pub event_starts_at: Option<f64>,
    pub summary: Option<String>,
    #[serde(default)]
    pub points: Vec<String>,
    pub liked: bool,
    pub saved: bool,
    /// less | not_interested
    pub hidden: Option<String>,
    pub reminded: bool,
    pub created_at: f64,
    /// Who shared it (an agent's name, Filo's, or a CLI's), and the agent's picture when it's one of the user's agents.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub author_picture: Option<String>,
    #[serde(default)]
    pub author_background: Option<String>,
    /// A glance post's card head (kind `glance`, feed_glance.rs); its page comes from `glances:get`.
    #[serde(default)]
    pub glance: Option<crate::screens::chat::GlanceRef>,
    /// What the user last picked in a glance post's choices.
    #[serde(default)]
    pub glance_choice: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSettings {
    pub pick_minute: f64,
    #[serde(default)]
    pub next_run_at: f64,
    pub running: bool,
    pub last_run_at: Option<f64>,
    pub last_run_error: Option<String>,
    #[serde(default)]
    pub failed_runs: f64,
    /// Minutes between regular checks; missing for a Feed on the once-a-day pick.
    pub interval_minutes: Option<f64>,
    /// Who answers Ask on posts: the agent Feed settings picked (`ask_agent_picked`), else the main agent.
    #[serde(default)]
    pub ask_agent_id: Option<String>,
    #[serde(default)]
    pub ask_agent_name: Option<String>,
    #[serde(default)]
    pub ask_agent_picked: bool,
}

impl FeedSettings {
    /// "Ask Chef": the name on a post's Ask button; "Ask" until it's known.
    pub fn ask_label(settings: Option<&FeedSettings>) -> String {
        settings
            .and_then(|settings| settings.ask_agent_name.as_deref())
            .map_or_else(|| "Ask".to_string(), |name| format!("Ask {name}"))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FeedMine {
    pub posts: Vec<FeedPost>,
    pub settings: Option<FeedSettings>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedTopic {
    pub topic_key: String,
    pub label: String,
    /// Its own Refresh is running.
    #[serde(default)]
    pub refreshing: bool,
    pub last_refreshed_at: Option<f64>,
    pub last_refresh_post_count: Option<f64>,
    pub last_refresh_error: Option<String>,
    /// The agent that watches it on its routine (`routine_id`); none when nobody does (its routine was deleted).
    #[serde(default)]
    pub routine_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    /// How often: every `interval_minutes`, or each day at `pick_minute`; `schedule_label` for another routine schedule.
    #[serde(default)]
    pub interval_minutes: Option<f64>,
    #[serde(default)]
    pub pick_minute: Option<f64>,
    #[serde(default)]
    pub schedule_label: Option<String>,
    #[serde(default)]
    pub paused: bool,
}

impl FeedTopic {
    /// "every hour", "daily at 8:00 AM", "paused": how often its agent looks.
    pub fn how_often(&self) -> String {
        if self.paused {
            return "paused".into();
        }
        match (self.interval_minutes, self.pick_minute, &self.schedule_label) {
            (Some(minutes), _, _) => interval_label(minutes),
            (None, Some(minute), _) => format!("daily at {}", pick_time_label(minute)),
            (None, None, Some(label)) => label.clone(),
            _ => "not scheduled".into(),
        }
    }

    /// "Chef · every hour", or that nobody watches it.
    pub fn watch_line(&self) -> String {
        match (&self.routine_id, &self.agent_name) {
            (Some(_), Some(name)) => format!("{name} · {}", self.how_often()),
            _ => "Nobody watches this topic. Pick an agent.".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskReply {
    /// The agent whose conversation it is.
    agent_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemindReply {
    due_at: f64,
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

pub const MAX_TOPIC_LENGTH: usize = 80;
/// "8:00 AM" for a local minute of the day.
pub fn pick_time_label(pick_minute: f64) -> String {
    format::minute_of_day(pick_minute)
}

fn weekday_date(ms: f64) -> String {
    format::local(ms).format("%a, %b %-d").to_string()
}

/// "Tonight at 9:00 PM", "Tomorrow at 10:00 AM", "Sat, Oct 3 at 7:30 PM".
pub fn event_label(starts_at: f64, now: f64) -> String {
    use chrono::Timelike as _;
    let start = format::local(starts_at);
    let current = format::local(now);
    let clock = format::clock(starts_at);
    if format::same_day(start, current) {
        return format!("{} at {clock}", if start.hour() >= 17 { "Tonight" } else { "Today" });
    }
    if format::same_day(start, current + chrono::Duration::days(1)) {
        return format!("Tomorrow at {clock}");
    }
    format!("{} at {clock}", weekday_date(starts_at))
}

/// The confirmation after "Remind me": "Filo will remind you at 8:30 PM."
pub fn reminder_notice(due_at: f64, now: f64) -> String {
    let due = format::local(due_at);
    let current = format::local(now);
    let clock = format::clock(due_at);
    if format::same_day(due, current) {
        return format!("Filo will remind you at {clock}.");
    }
    if format::same_day(due, current + chrono::Duration::days(1)) {
        return format!("Filo will remind you tomorrow at {clock}.");
    }
    format!("Filo will remind you on {} at {clock}.", weekday_date(due_at))
}

/// What to tell the user after asking for a check.
pub fn refresh_message(status: &str) -> String {
    match status {
        "started" => "Your agents are looking for new posts. They show up in a few minutes.",
        "fresh" => "Your Feed is up to date.",
        "running" => "Your agents are already looking.",
        "no_topics" => "Add a topic first so your agents know what to look for.",
        "no_agent" => "Nobody watches your topics. Pick an agent for them in Feed settings.",
        _ => "Couldn't check for new posts. Try again.",
    }
    .to_string()
}

/// What a check the user started ended with, once `feed:mine` says it's over: the posts it brought (`found`, those the
/// Feed didn't have when it started) or that it failed.
pub fn check_finished_message(settings: &FeedSettings, found: usize) -> String {
    if settings.failed_runs > 0. && settings.last_run_error.as_deref().is_some_and(|error| !error.is_empty()) {
        return "Couldn't finish looking for new posts. Your agents try again on their schedule.".into();
    }
    match found {
        0 => "Checked: no new posts this time.".into(),
        1 => "Found 1 new post.".into(),
        n => format!("Found {n} new posts."),
    }
}

/// "every 30 min", "every hour", "every 90 min", "every 2 hours" (routine_display.rs, as routines say it).
pub(crate) use super::routine_display::interval_words as interval_label;

/// When the Feed looks for new posts.
pub fn schedule_line(settings: &FeedSettings) -> String {
    match settings.interval_minutes {
        Some(minutes) => format!("Checks for new posts {}.", interval_label(minutes)),
        None => format!("New posts once a day, at {}.", pick_time_label(settings.pick_minute)),
    }
}

/// The Feed's quiet freshness line: "Checking for new posts…", "Couldn't update · trying again at 3:30 PM" or
/// "Updated 5 min ago". None before the first check.
pub fn freshness_line(settings: &FeedSettings, now: f64) -> Option<String> {
    if settings.running {
        return Some("Your agents are looking for new posts…".into());
    }
    if settings.failed_runs > 0. && settings.last_run_error.as_deref().is_some_and(|error| !error.is_empty()) {
        let retry = settings.next_run_at;
        let when = if format::same_day(format::local(retry), format::local(now)) {
            format!("at {}", format::clock(retry))
        } else {
            weekday_date(retry)
        };
        return Some(format!("Couldn't update · trying again {when}"));
    }
    let at = settings.last_run_at?;
    Some(format!("Updated {}", format::ago(at, now)))
}

/// Posts that came since the list was last shown: they wait behind "N new posts" so the list doesn't move under the
/// reader. `shown` is what the list showed; a list that showed nothing takes them straight away.
pub fn arrived_posts(posts: &[FeedPost], shown: &HashSet<String>) -> HashSet<String> {
    if !posts.iter().any(|post| shown.contains(&post.id)) {
        return HashSet::new();
    }
    posts.iter().filter(|post| !shown.contains(&post.id) && post.hidden.is_none()).map(|post| post.id.clone()).collect()
}

/// "1 new post", "4 new posts".
pub fn new_posts_label(count: usize) -> String {
    if count == 1 { "1 new post".into() } else { format!("{count} new posts") }
}

/// What to tell the user after asking for a topic's Refresh (`feed:refreshTopicMine`).
pub fn topic_refresh_message(status: &str, label: &str) -> String {
    match status {
        "started" => format!("Looking for new posts on {label}."),
        "running" => format!("Already looking into {label}."),
        "not_found" => "That topic isn't in your Feed any more.".into(),
        "no_agent" => format!("Nobody watches {label}. Pick an agent for it in Feed settings."),
        _ => "Couldn't refresh that topic. Try again.".into(),
    }
}

/// The line under a selected topic: what its Refresh is doing or last did.
pub fn topic_refresh_status(topic: &FeedTopic, pending: bool, now: f64) -> (String, bool) {
    if pending || topic.refreshing {
        return (format!("{} is looking for new posts…", topic.agent_name.as_deref().unwrap_or("Your agent")), false);
    }
    if let Some(error) = topic.last_refresh_error.as_deref().filter(|error| !error.is_empty()) {
        return (error_text(error, "The last refresh didn't work."), true);
    }
    match topic.last_refreshed_at {
        Some(at) => {
            let found = match topic.last_refresh_post_count.map(|count| count as i64) {
                Some(0) => " · nothing new".to_string(),
                Some(1) => " · 1 new post".to_string(),
                Some(count) => format!(" · {count} new posts"),
                None => String::new(),
            };
            (format!("Refreshed {}{found}", format::ago(at, now)), false)
        }
        None => ("Refresh to look for new posts on just this topic.".into(), false),
    }
}

/// The posts to show for a topic filter. Hidden posts drop out, except the
/// ones hidden in this session, which stay as an undo row.
pub fn visible_posts<'a>(
    posts: &'a [FeedPost],
    topic_key: Option<&str>,
    keep_hidden: &HashSet<String>,
) -> Vec<&'a FeedPost> {
    posts
        .iter()
        .filter(|post| {
            (post.hidden.is_none() || keep_hidden.contains(&post.id))
                && topic_key.is_none_or(|key| post.topic_key == key)
        })
        .collect()
}

/// The line under the last post. A Feed that checks regularly is caught up; one on the once-a-day pick names it.
pub fn feed_end_text(count: usize, settings: Option<&FeedSettings>, topic_label: Option<&str>) -> String {
    if let Some(minutes) = settings.and_then(|settings| settings.interval_minutes) {
        return match topic_label {
            Some(topic) if count > 0 => format!("That's everything on {topic} for now."),
            Some(topic) => format!("Nothing on {topic} yet. Your agents keep looking."),
            None => {
                format!("You're all caught up. Your agents look for new posts as often as {}.", interval_label(minutes))
            }
        };
    }
    let when = match settings {
        Some(settings) => format!("tomorrow at {}", pick_time_label(settings.pick_minute)),
        None => "tomorrow morning".into(),
    };
    if let Some(topic) = topic_label {
        return if count > 0 {
            format!("That's everything on {topic} for now.")
        } else {
            format!("Nothing on {topic} yet. Your agents look again {when}.")
        };
    }
    format!("That's today's {count}. New picks {when}.")
}

pub fn hide_confirmation(reason: &str, topic_label: &str) -> String {
    if reason == "less" {
        format!("Got it. Less about {topic_label}.")
    } else {
        "Got it. You won't see this one again.".into()
    }
}

/// The newest picture for each topic, for its bubble.
pub fn topic_covers(posts: &[FeedPost]) -> HashMap<String, String> {
    let mut covers = HashMap::new();
    for post in posts {
        if let Some(url) = &post.image_url
            && post.hidden.is_none()
            && !covers.contains_key(&post.topic_key)
        {
            covers.insert(post.topic_key.clone(), url.clone());
        }
    }
    covers
}

/// Optimistic toggles (like, save) win until the backend agrees.
pub fn settle_overrides(overrides: &mut HashMap<String, bool>, server_value: impl Fn(&str) -> Option<bool>) {
    overrides.retain(|id, value| server_value(id).is_some_and(|server| server != *value));
}

/// The backend's message when it's short enough to show, otherwise the fallback.
pub fn error_text(error: &str, fallback: &str) -> String {
    if !error.is_empty() && error.len() < 120 { error.to_string() } else { fallback.to_string() }
}

// ---------------------------------------------------------------------------
// Notice: a short confirmation at the bottom that clears itself.
// ---------------------------------------------------------------------------

const NOTICE_MS: u64 = 2_600;

#[derive(Default)]
pub struct Notice {
    current: Option<(u64, SharedString)>,
    seq: u64,
    _timer: Option<Task<()>>,
}

impl Notice {
    /// What it says now (scripted checks).
    #[cfg(feature = "dev")]
    pub fn text(&self) -> Option<String> {
        self.current.as_ref().map(|(_, text)| text.to_string())
    }
}

pub trait PostHost: Sized + 'static {
    fn notice(&mut self) -> &mut Notice;
    fn set_asking(&mut self, _post_id: Option<String>) {}
}

pub fn notify<V: PostHost>(view: &mut V, text: impl Into<SharedString>, cx: &mut Context<V>) {
    let notice = view.notice();
    notice.seq += 1;
    let id = notice.seq;
    notice.current = Some((id, text.into()));
    notice._timer = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(NOTICE_MS)).await;
        this.update(cx, |view, cx| {
            let notice = view.notice();
            if notice.current.as_ref().is_some_and(|(current, _)| *current == id) {
                notice.current = None;
                cx.notify();
            }
        })
        .ok();
    }));
    cx.notify();
}

/// The notice pill, absolutely placed at the bottom of a `relative()` parent.
pub fn render_notice(notice: &Notice, cx: &App) -> Option<AnyElement> {
    let (id, text) = notice.current.clone()?;
    let colors = *cx.colors();
    Some(
        div()
            .absolute()
            .bottom(px(space::XL))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .px(px(space::XL))
            .child(
                div()
                    .max_w(px(520.))
                    .px(px(space::LG))
                    .py(px(space::SM + space::XXS))
                    .rounded(px(radius::LG))
                    .bg(colors.media)
                    .text_color(colors.on_media)
                    .footnote()
                    .semibold()
                    .text_center()
                    .child(text)
                    .with_animation(
                        SharedString::from(format!("feed-notice-{id}")),
                        Animation::new(Duration::from_millis(220)),
                        |this, delta| this.opacity(delta),
                    ),
            )
            .into_any_element(),
    )
}

// ---------------------------------------------------------------------------
// Post actions
// ---------------------------------------------------------------------------

/// The original page in the browser.
pub fn open_post(url: &str, cx: &mut App) {
    cx.open_url(url);
}

/// Sharing a post copies its link.
pub fn share_post<V: PostHost>(view: &mut V, url: &str, cx: &mut Context<V>) {
    cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
    notify(view, "Link copied.", cx);
}

/// Asks about the post in the conversation of the agent that answers Ask, and opens it (the post page; the Feed opens
/// the conversation beside the posts instead).
pub fn ask_about_post<V: PostHost>(view: &mut V, post_id: &str, window: &mut Window, cx: &mut Context<V>) {
    view.set_asking(Some(post_id.to_string()));
    cx.notify();
    backend::call_in(
        window,
        cx,
        "feed:askAboutPost",
        json!({ "postId": post_id }),
        |this: &mut V, result: Result<AskReply, String>, window, cx| {
            this.set_asking(None);
            match result {
                Ok(reply) => nav::open_agent(reply.agent_id, window, cx),
                Err(error) => notify(this, error_text(&error, "Couldn't start that chat. Try again."), cx),
            }
            cx.notify();
        },
    );
}

pub fn remind_post<V: PostHost>(post_id: &str, cx: &mut Context<V>) {
    backend::call(
        cx,
        "feed:remindMine",
        json!({ "postId": post_id }),
        |this: &mut V, result: Result<RemindReply, String>, cx| match result {
            Ok(reply) => notify(this, reminder_notice(reply.due_at, format::now_ms()), cx),
            Err(error) => notify(this, error_text(&error, "Couldn't set that reminder."), cx),
        },
    );
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// A small round bullet aligned with the first line of a point.
pub fn bullet(size: f32, top: f32, color: gpui::Hsla) -> Div {
    div().mt(px(top)).size(px(size)).flex_none().rounded(px(radius::FULL)).bg(color)
}

/// Who shared a post, as a small picture: the agent's own on its colour, else the name's initial.
pub fn author_avatar(post: &FeedPost, size: f32, cx: &App) -> AnyElement {
    match &post.author_picture {
        Some(picture) => crate::screens::agents::agent_picture(
            crate::screens::agents::picture_source(picture),
            post.author_background.as_deref(),
            size,
            cx,
        )
        .into_any_element(),
        None => ui::letter_avatar(
            if post.author.is_empty() { "Filo" } else { &post.author },
            size,
            cx.colors().surface_strong,
            cx.colors().text_secondary,
        )
        .into_any_element(),
    }
}

/// A post's cover, `width` points across (a pixel width: GPUI lays a percentage-wide box whose only children are
/// absolutely placed out at zero width when the column above it changes height, so the cover vanished after "More").
/// Loading, it's a quiet placeholder of the same size; a picture that failed is `None`, for the caller's title card.
pub fn post_cover(id: &str, url: &str, width: f32, ratio: f32, video: bool, cx: &App) -> Option<gpui::Stateful<Div>> {
    let colors = *cx.colors();
    let state = crate::pictures::state(url, cx);
    if state == "failed" {
        return None;
    }
    let height = (width / ratio).max(80.);
    Some(
        div()
            .id(SharedString::from(format!("media-{id}")))
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .rounded(px(radius::LG))
            .bg(colors.surface)
            .when(state != "loaded", |this| {
                this.child(div().absolute().inset_0().flex().items_center().justify_center().child(ui::icon_sized(
                    "image",
                    28.,
                    colors.text_tertiary,
                )))
            })
            .child(cover_image(url, radius::LG, colors.background))
            .when(video, |this| {
                this.child(
                    div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().child(
                        div()
                            .size(px(56.))
                            .pl(px(space::XXS))
                            .rounded(px(radius::FULL))
                            .bg(colors.media.opacity(0.62))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::icon_sized("play", 22., colors.on_media)),
                    ),
                )
            }),
    )
}

/// A cover image that fills its (relative, unclipped) parent, rounded on `behind`.
pub fn cover_image(url: &str, rounded: f32, behind: gpui::Hsla) -> Div {
    ui::rounded_cover(SharedString::from(url.to_string()), rounded, behind)
}

/// Full width with the height for `ratio` (width / height), taken from the page
/// column width; GPUI's aspect ratio overflowed the column with `w_full`.
pub fn aspect<E: Styled>(element: E, ratio: f32) -> E {
    element.w_full().h(px((ui::column_width() / ratio).max(80.)))
}

/// The event line: calendar icon, when it starts, and "Remind me" or the
/// "Reminder set" chip.
pub fn event_row<V: PostHost>(
    id: &str,
    post_id: &str,
    starts_at: f64,
    reminded: bool,
    large: bool,
    now: f64,
    cx: &mut Context<V>,
) -> Div {
    let colors = *cx.colors();
    let post_id = post_id.to_string();
    div()
        .flex()
        .items_center()
        .gap(px(space::SM))
        .child(ui::icon_sized("calendar-clock", if large { 18. } else { 16. }, colors.text))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .map(|this| if large { this.callout() } else { this.footnote() })
                .semibold()
                .child(event_label(starts_at, now)),
        )
        .child(if reminded {
            ui::chip("Reminder set", ui::Tone::Success, false, cx).into_any_element()
        } else {
            ui::Button::secondary(SharedString::from(format!("{id}-remind")), "Remind me")
                .small()
                .on_click(cx.listener(move |_, _, _, cx| remind_post(&post_id, cx)))
                .into_any_element()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every address untrusted markdown would draw, from the tree the text view renders (gpui-component
    /// text::safety), must be one `is_safe_link` allows; and nothing may draw a picture.
    fn assert_safe(markdown: &str) -> Vec<String> {
        use gpui_component::text::safety::{is_safe_link, untrusted_markdown_targets};
        let targets = untrusted_markdown_targets(markdown);
        for target in &targets {
            assert!(!target.starts_with("image:"), "a picture would be fetched: {target} in {markdown:?}");
            let url = target.split_once(':').map(|(_, url)| url).unwrap_or_default();
            assert!(
                target.starts_with("definition:") && url.is_empty() || is_safe_link(url),
                "unsafe {target} from {markdown:?}"
            );
        }
        targets
    }

    #[test]
    fn untrusted_markdown_keeps_only_web_and_mail_links_on_every_path() {
        let attacks = [
            // The reviewer's bypass of the old text sanitizer: nested brackets in the label.
            "[a [b] c](file:///Applications/Calculator.app)",
            "[x](file:///Applications/Calculator.app 'single-quoted title')",
            "[x](javascript:alert((1)))",
            "[multi\nline\nlabel](file:///etc/passwd)",
            "[x](&#106;avascript:alert(1))",
            "[x](<javascript:alert(1)>)",
            "[x]( JAVASCRIPT:alert(1) )",
            "[x](vbscript:msgbox)",
            "[x](data:text/html;base64,PHNjcmlwdD4=)",
            "[x](//evil.example/path)",
            "[x](/settings)",
            "[x](filo://mcp/1/callback)",
            "[x](https:/one-slash)",
            "[x](http:///no-host)",
            "<file:///Applications/Calculator.app>",
            "<javascript:alert(1)>",
            "[ref]\n\n[ref]: file:///Applications/Calculator.app",
            "[ref][r]\n\n[r]: javascript:alert(1) \"t\"",
            "![pic](file:///Users/x/photo.png)",
            "![tracker](https://tracker.example/p.png)",
            "[![pic](https://img.example/a.png)](file:///Applications/Calculator.app)",
            "<a href=\"file:///Applications/Calculator.app\">open</a>",
            "<img src=\"https://tracker.example/p.png\">",
            "<div>\n<a href=\"javascript:alert(1)\">x</a>\n</div>",
            "    indented [x](file:///etc/passwd)",
            "   ```\n   [x](file:///etc/passwd)\n   ```",
            "> quote [x](file:///etc) and ![p](https://a.example/p.png)",
            "- item [x](file:///etc)\n  - nested [y](javascript:1)",
            "| a | b |\n|---|---|\n| [x](file:///etc) | ![p](https://a.example/p.png) |",
            "# Heading [x](file:///etc)",
            "**bold [x](file:///etc)** _it [y](javascript:1)_ ~~del [z](data:x)~~",
            "Footnote[^1]\n\n[^1]: see [x](file:///etc)",
            "www.example.com and hi@filo.im and https://filo.im",
        ];
        for attack in attacks {
            assert_safe(attack);
        }
        // Safe links survive; the label of an unsafe one stays readable.
        let kept = assert_safe("[docs](https://example.com) [mail](mailto:hi@filo.im) <https://filo.im>");
        assert!(
            kept.contains(&"link:https://example.com".to_string())
                && kept.contains(&"link:mailto:hi@filo.im".to_string()),
            "{kept:?}"
        );
        let text =
            gpui_component::text::safety::untrusted_markdown_text("[a [b] c](file:///Applications/Calculator.app) ok");
        assert!(text.contains("a [b] c") && text.contains("ok"), "{text}");
        // A picture becomes a link to it (nothing is fetched), named by its alt text.
        assert_eq!(assert_safe("![chart](https://img.example/c.png)"), vec!["link:https://img.example/c.png"]);
    }

    #[test]
    fn untrusted_markdown_shows_raw_html_as_text_and_keeps_formatting() {
        use gpui_component::text::safety::untrusted_markdown_text;
        let text = untrusted_markdown_text(
            "Hi <b>x</b> <img src=x onerror=alert(1)>\n\n# Weekly\n\n**Bold** `a < b`\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\n<script>keep()</script>\n```\n",
        );
        assert!(
            text.contains("<b>x</b>") && text.contains("<img src=x onerror=alert(1)>"),
            "raw HTML shows as text: {text}"
        );
        assert!(
            text.contains("Weekly") && text.contains("Bold") && text.contains("a < b") && text.contains('1'),
            "{text}"
        );
    }

    #[test]
    fn only_web_and_mail_addresses_open() {
        use gpui_component::text::safety::is_safe_link;
        for url in
            ["https://example.com", "http://a.b/c?d#e", "mailto:hi@filo.im", "HTTPS://Example.com", " https://x.y "]
        {
            assert!(is_safe_link(url), "{url}");
        }
        for url in [
            "",
            "file:///Applications/Calculator.app",
            "javascript:alert(1)",
            "data:text/html,x",
            "/settings",
            "//evil.example",
            "filo://mcp",
            "https://",
            "http:///etc",
            "https://a b",
            "https://a\u{0}b",
            "mailto:",
            "https:\\\\evil",
            "ftp://x.y",
        ] {
            assert!(!is_safe_link(url), "{url:?}");
        }
    }

    #[test]
    fn topic_refresh_status_reads_the_topics_state() {
        let mut topic = FeedTopic {
            topic_key: "k".into(),
            label: "Agents".into(),
            refreshing: false,
            last_refreshed_at: None,
            last_refresh_post_count: None,
            last_refresh_error: None,
            ..FeedTopic::default()
        };
        let now = 1_000_000_000.;
        assert_eq!(topic.watch_line(), "Nobody watches this topic. Pick an agent.");
        let watched = FeedTopic {
            routine_id: Some("r".into()),
            agent_name: Some("Chef".into()),
            interval_minutes: Some(120.),
            ..topic.clone()
        };
        assert_eq!(watched.watch_line(), "Chef · every 2 hours");
        assert_eq!(
            FeedTopic { interval_minutes: None, pick_minute: Some(480.), ..watched.clone() }.how_often(),
            "daily at 8:00 AM"
        );
        assert_eq!(FeedTopic { paused: true, ..watched }.how_often(), "paused");
        assert!(topic_refresh_status(&topic, false, now).0.starts_with("Refresh to look"));
        assert_eq!(topic_refresh_status(&topic, true, now).0, "Your agent is looking for new posts…");
        topic.last_refreshed_at = Some(now - 5. * 60_000.);
        topic.last_refresh_post_count = Some(2.);
        assert_eq!(topic_refresh_status(&topic, false, now).0, "Refreshed 5 min ago · 2 new posts");
        topic.last_refreshed_at = Some(now - 10_000.);
        topic.last_refresh_post_count = Some(0.);
        assert_eq!(topic_refresh_status(&topic, false, now).0, "Refreshed just now · nothing new");
        topic.last_refresh_error = Some("Search is not answering right now.".into());
        assert_eq!(topic_refresh_status(&topic, false, now), ("Search is not answering right now.".into(), true));
        topic.refreshing = true;
        assert!(!topic_refresh_status(&topic, false, now).1, "running wins over the last error");
    }

    fn settings(interval: Option<f64>) -> FeedSettings {
        serde_json::from_value(json!({
            "pickMinute": 480, "nextRunAt": 0, "running": false, "intervalMinutes": interval,
        }))
        .unwrap()
    }

    fn post(id: &str) -> FeedPost {
        serde_json::from_value(json!({
            "_id": id, "topicKey": "t", "topicLabel": "T", "kind": "article", "title": id, "note": "n", "why": "w",
            "url": "https://x.example", "sourceName": "X", "liked": false, "saved": false, "reminded": false, "createdAt": 0,
        }))
        .unwrap()
    }

    #[test]
    fn check_intervals_read_and_print() {
        let labels: Vec<String> = [30., 60., 90., 120., 45.].into_iter().map(interval_label).collect();
        assert_eq!(labels, ["every 30 min", "every hour", "every 90 min", "every 2 hours", "every 45 min"]);
    }

    #[test]
    fn the_schedule_says_how_often() {
        let every = settings(Some(30.));
        assert_eq!(schedule_line(&every), "Checks for new posts every 30 min.");
        assert_eq!(schedule_line(&settings(None)), "New posts once a day, at 8:00 AM.");
    }

    #[test]
    fn freshness_says_running_failing_or_how_long_ago() {
        let now = 1_800_000_000_000.;
        let mut feed = settings(Some(30.));
        assert_eq!(freshness_line(&feed, now), None);
        feed.last_run_at = Some(now - 5. * 60_000.);
        assert_eq!(freshness_line(&feed, now).as_deref(), Some("Updated 5 min ago"));
        feed.last_run_at = Some(now - 20_000.);
        assert_eq!(freshness_line(&feed, now).as_deref(), Some("Updated just now"));
        feed.last_run_at = Some(now - 3. * 3_600_000.);
        assert_eq!(freshness_line(&feed, now).as_deref(), Some("Updated 3 hr ago"));
        feed.failed_runs = 1.;
        feed.last_run_error = Some("Search failed".into());
        feed.next_run_at = now + 30. * 60_000.;
        let failing = freshness_line(&feed, now).unwrap();
        assert!(failing.starts_with("Couldn't update · trying again "), "{failing}");
        feed.running = true;
        assert_eq!(freshness_line(&feed, now).as_deref(), Some("Your agents are looking for new posts…"));
        assert_eq!(refresh_message("fresh"), "Your Feed is up to date.");
        feed.running = false;
        assert_eq!(
            check_finished_message(&feed, 2),
            "Couldn't finish looking for new posts. Your agents try again on their schedule."
        );
        feed.failed_runs = 0.;
        assert_eq!(check_finished_message(&feed, 0), "Checked: no new posts this time.");
        assert_eq!(check_finished_message(&feed, 1), "Found 1 new post.");
        assert_eq!(check_finished_message(&feed, 3), "Found 3 new posts.");
    }

    #[test]
    fn new_posts_wait_behind_the_banner() {
        let shown: HashSet<String> = ["a", "b"].into_iter().map(String::from).collect();
        let posts = [post("c"), post("d"), post("a"), post("b")];
        let mut arrived: Vec<String> = arrived_posts(&posts, &shown).into_iter().collect();
        arrived.sort();
        assert_eq!(arrived, ["c", "d"]);
        // A list that showed nothing (or only posts since pruned) takes new ones straight away.
        assert!(arrived_posts(&posts, &HashSet::new()).is_empty());
        assert!(arrived_posts(&[post("c")], &shown).is_empty());
        let mut hidden = post("e");
        hidden.hidden = Some("less".into());
        assert!(arrived_posts(&[post("a"), hidden], &shown).is_empty());
        assert_eq!(new_posts_label(1), "1 new post");
        assert_eq!(new_posts_label(4), "4 new posts");
    }

    #[test]
    fn the_end_line_follows_the_schedule() {
        let every = settings(Some(30.));
        assert_eq!(
            feed_end_text(6, Some(&every), None),
            "You're all caught up. Your agents look for new posts as often as every 30 min."
        );
        assert_eq!(
            feed_end_text(0, Some(&every), Some("Pottery")),
            "Nothing on Pottery yet. Your agents keep looking."
        );
        assert!(
            feed_end_text(6, Some(&settings(None)), None).starts_with("That's today's 6. New picks tomorrow at 8:00")
        );
    }
}
